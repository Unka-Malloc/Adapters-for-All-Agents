//! Provider route and model resolution against the supplier's declared
//! surface, plus the endpoint probe a caller may run before trusting a
//! configured model reference.
//!
//! Resolution never hardcodes a supplier endpoint or catalog: every fact
//! is parsed from the installed provider configuration — dsh profile
//! patches (`~/.dsh/profiles/*/cordis.patch.yml`), pi-ai's bundled
//! provider catalogs, dsh's own `dsh-llm-*` plugin packages, and the
//! credential records dsh itself resolves (`~/.dsh/.credentials.yaml`).
//! A route the declarations do not name is unknown; an endpoint is the
//! one the declaration publishes, nothing more.

use std::collections::BTreeSet;
use std::fmt;
use std::path::{Path, PathBuf};

/// What the installed declarations say one provider route is. Resolution
/// merges every source that names the provider; the caller decides what
/// is admissible.
///
/// `Debug` is written by hand so the credential prints as its variant
/// only; no formatting surface of this type carries the secret.
#[derive(Default)]
pub struct ResolvedRoute {
    /// Provider id as declared.
    pub provider: String,
    /// The endpoint the declaration publishes, when any does.
    pub base_url: Option<String>,
    /// dsh profile lanes carrying this route (empty for builtin plugins).
    pub lanes: BTreeSet<String>,
    /// Credential env name the patch declares (`apiKeyEnv`).
    pub api_key_env: Option<String>,
    /// Closed catalog: patch `models:` lists or pi-ai bundled ids. When
    /// non-empty it names every servable id.
    pub catalog: Vec<String>,
    /// Advisory default ids a builtin plugin ships (settings may extend).
    pub default_models: Vec<String>,
    /// The credential the launch would use, resolved from the same
    /// records dsh reads.
    pub credential: Option<Credential>,
}

/// How the resolved credential is presented at the endpoint. Mirrors the
/// transport each provider variant declares. `Debug` names the variant
/// and redacts the value.
pub enum Credential {
    /// `Authorization: Bearer` plus `x-api-key` (covers OpenAI-shaped and
    /// Anthropic-shaped APIs in one probe).
    ApiKey(String),
    /// `x-dsh-auth-token` session grant (dsh account-provider variant).
    SessionToken(String),
}

impl fmt::Debug for Credential {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ApiKey(_) => f.write_str("ApiKey([redacted])"),
            Self::SessionToken(_) => f.write_str("SessionToken([redacted])"),
        }
    }
}

impl fmt::Debug for ResolvedRoute {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ResolvedRoute")
            .field("provider", &self.provider)
            .field("base_url", &self.base_url)
            .field("lanes", &self.lanes)
            .field("api_key_env", &self.api_key_env)
            .field("catalog", &self.catalog)
            .field("default_models", &self.default_models)
            .field("credential", &self.credential)
            .finish()
    }
}

fn home() -> PathBuf {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

fn dsh_plugins(home: &Path) -> PathBuf {
    home.join(".local/lib/node_modules/@deepseek-ai/dsh/node_modules/@deepseek-ai")
}

/// Scan `~/.dsh/profiles/*/cordis.patch.yml` for the `providers:` map of
/// the `llm-pi-ai` plugin. The files are machine-written by dsh: provider
/// keys sit at six-space indent under `providers:`, with `apiKeyEnv`,
/// `baseURL`, and a `models:` list of `- id:` entries inside each block.
/// With a bound `lane`, every other profile contributes nothing — its
/// endpoint and credential env never mix into the bound route.
fn patch_routes(home: &Path, lane_filter: Option<&str>, route: &mut ResolvedRoute) {
    let profiles_dir = home.join(".dsh/profiles");
    let Ok(entries) = std::fs::read_dir(&profiles_dir) else {
        return;
    };
    for dir in entries.flatten() {
        let lane = dir.file_name().to_string_lossy().to_string();
        if lane_filter.is_some_and(|bound| bound != lane) {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(dir.path().join("cordis.patch.yml")) else {
            continue;
        };
        let mut in_providers = false;
        let mut in_route = false;
        let mut in_models = false;
        for line in text.lines() {
            if line.starts_with("    providers:") {
                in_providers = true;
                continue;
            }
            if in_providers {
                // A key at four-space indent (or a new list item) ends the
                // providers map.
                if !line.starts_with("      ") {
                    in_providers = false;
                    in_route = false;
                    continue;
                }
                if let Some(rest) = line.strip_prefix("      ") {
                    if let Some(id) = rest.strip_suffix(':') {
                        if !id.contains(' ') {
                            in_route = id == route.provider;
                            if in_route {
                                route.lanes.insert(lane.clone());
                            }
                            in_models = false;
                            continue;
                        }
                    }
                }
                if !in_route {
                    continue;
                }
                let trimmed = line.trim();
                if let Some(v) = trimmed.strip_prefix("apiKeyEnv:") {
                    route.api_key_env = Some(v.trim().to_string());
                } else if let Some(v) = trimmed.strip_prefix("baseURL:") {
                    route.base_url = Some(v.trim().to_string());
                } else if trimmed == "models:" {
                    in_models = true;
                } else if in_models {
                    if let Some(v) = trimmed.strip_prefix("- id:") {
                        route.catalog.push(v.trim().to_string());
                    }
                }
            }
        }
    }
}

/// The routes dsh's own llm plugins register (`dsh-llm-*` packages in the
/// app bundle): each declares `const PROVIDER = "id"`, ships its endpoint
/// as `PUBLIC_BASE_URL`, and spells its default catalog as `id: "model"`
/// literals. Returns the matched package name — the variant decides how
/// the credential is presented.
fn builtin_route(plugins: &Path, provider: &str, route: &mut ResolvedRoute) -> Option<String> {
    let entries = std::fs::read_dir(plugins).ok()?;
    let needle = format!("PROVIDER = \"{provider}\"");
    for dir in entries.flatten() {
        let name = dir.file_name().to_string_lossy().to_string();
        if !name.starts_with("dsh-llm-") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(dir.path().join("lib/index.js")) else {
            continue;
        };
        if !text.contains(&needle) {
            continue;
        }
        route.default_models = text
            .lines()
            .take(80)
            .filter_map(|l| l.trim().strip_prefix("id: \""))
            .filter_map(|rest| rest.split('"').next().map(str::to_string))
            .collect();
        // Variant plugins (api-key, account) register the route but share
        // the adapter package they import — the endpoint literal lives
        // there. Only a package the matched one imports is bound to this
        // provider; an unrelated sibling's endpoint is never borrowed.
        let base = public_base_url(&text).or_else(|| {
            imported_llm_packages(&text)
                .filter(|sibling| *sibling != name)
                .find_map(|sibling| {
                    std::fs::read_to_string(plugins.join(sibling).join("lib/index.js"))
                        .ok()
                        .and_then(|t| public_base_url(&t))
                })
        });
        if route.base_url.is_none() {
            route.base_url = base;
        }
        return Some(name);
    }
    None
}

fn public_base_url(text: &str) -> Option<String> {
    text.lines()
        .find(|l| l.contains("PUBLIC_BASE_URL"))
        .and_then(|l| l.split('"').nth(1))
        .filter(|url| !url.is_empty())
        .map(str::to_string)
}

/// `dsh-llm-*` packages a plugin imports (`from "@deepseek-ai/<name>"`).
fn imported_llm_packages(text: &str) -> impl Iterator<Item = &str> {
    text.split("from \"@deepseek-ai/")
        .skip(1)
        .filter_map(|rest| rest.split('"').next())
        .filter(|name| name.starts_with("dsh-llm-") && !name.contains('/'))
}

/// Account-variant plugins authenticate with `x-dsh-auth-token` minted by
/// the sibling `dsh-*-account-platform` service, which stores its session
/// grant as record `<platform>/default` in the credentials file. Resolve
/// the same token the launch would use.
fn account_token(home: &Path, vendor: &str) -> Option<String> {
    let pkg = dsh_plugins(home).join(format!("dsh-{vendor}-account-platform/lib/index.js"));
    let text = std::fs::read_to_string(pkg).ok()?;
    let platform = text
        .split("credentialKey(\"")
        .nth(1)
        .and_then(|rest| rest.split('"').next())?;
    let record = format!("{platform}/default:");
    let creds = std::fs::read_to_string(home.join(".dsh/.credentials.yaml")).ok()?;
    let mut in_record = false;
    for line in creds.lines() {
        if line.trim_start().starts_with(&record) {
            in_record = true;
            continue;
        }
        if in_record {
            if let Some(v) = line.trim().strip_prefix("token:") {
                let v = v.trim().trim_matches('"').trim_matches('\'');
                if !v.is_empty() {
                    return Some(v.to_string());
                }
            }
            // A non-indented line ends this record's block.
            if !line.starts_with(' ') && !line.starts_with('\t') && !line.trim().is_empty() {
                break;
            }
        }
    }
    None
}

/// Models from pi-ai's bundled catalog for the provider id, plus the
/// endpoint the catalog declares (`baseUrl` on each model entry).
fn bundled_catalog(home: &Path, provider: &str, route: &mut ResolvedRoute) {
    let path = home.join(format!(
        ".dsh/profiles/node_modules/@earendil-works/pi-ai/dist/providers/data/{provider}.json"
    ));
    let Ok(text) = std::fs::read_to_string(path) else {
        return;
    };
    let Ok(doc) = serde_json::from_str::<serde_json::Value>(&text) else {
        return;
    };
    // Catalogs group entries by protocol id; every leaf is a model.
    if let Some(groups) = doc.as_object() {
        for group in groups.values() {
            if let Some(entries) = group.as_object() {
                for (id, entry) in entries {
                    route.catalog.push(id.clone());
                    if route.base_url.is_none() {
                        route.base_url = entry["baseUrl"].as_str().map(str::to_string);
                    }
                }
            }
        }
    }
}

/// Resolve a credential by its env name from `~/.dsh/.credentials.yaml`
/// `refs:` — the same source dsh's credential service reads.
fn credential(home: &Path, env_name: &str) -> Option<String> {
    let text = std::fs::read_to_string(home.join(".dsh/.credentials.yaml")).ok()?;
    let mut in_refs = false;
    for line in text.lines() {
        if line.starts_with("refs:") {
            in_refs = true;
            continue;
        }
        if in_refs {
            if !line.starts_with("  ") {
                break;
            }
            if let Some(v) = line.trim().strip_prefix(&format!("{env_name}:")) {
                let v = v.trim().trim_matches('"').trim_matches('\'');
                if !v.is_empty() {
                    return Some(v.to_string());
                }
            }
        }
    }
    None
}

/// Resolve one provider id against every installed declaration. `lane`
/// filters patch-declared routes to the dsh profile the caller binds
/// (`--profile <name>`); a builtin plugin route ignores lanes because it
/// registers wherever dsh runs. Returns `None` when no declaration names
/// the provider anywhere.
pub fn resolve_dsh_route(provider: &str, lane: Option<&str>) -> Option<ResolvedRoute> {
    resolve_dsh_route_in(&home(), provider, lane)
}

fn resolve_dsh_route_in(home: &Path, provider: &str, lane: Option<&str>) -> Option<ResolvedRoute> {
    let mut route = ResolvedRoute {
        provider: provider.to_string(),
        ..Default::default()
    };
    patch_routes(home, lane, &mut route);
    let patched_base = route.base_url.clone();
    let patched_catalog = route.catalog.clone();
    bundled_catalog(home, provider, &mut route);
    let plugins = dsh_plugins(home);
    let builtin_pkg = if route.lanes.is_empty() {
        builtin_route(&plugins, provider, &mut route)
    } else {
        None
    };
    if route.lanes.is_empty()
        && route.catalog.is_empty()
        && builtin_pkg.is_none()
        && route.base_url.is_none()
    {
        return None;
    }
    // Patch-declared fields win over bundled/builtin-derived ones.
    if !patched_catalog.is_empty() {
        route.catalog = patched_catalog;
    }
    if patched_base.is_some() {
        route.base_url = patched_base;
    }
    if let Some(env) = &route.api_key_env {
        route.credential = credential(home, env).map(Credential::ApiKey);
    } else if let Some(pkg) = &builtin_pkg {
        let variant = std::fs::read_to_string(plugins.join(format!("{pkg}/lib/index.js")))
            .unwrap_or_default();
        if variant.contains("x-dsh-auth-token") {
            route.credential =
                account_token(home, provider.strip_suffix("-account").unwrap_or(provider))
                    .map(Credential::SessionToken);
        }
    }
    Some(route)
}

/// The outcome of probing the endpoint a route's declarations publish.
#[derive(Debug)]
pub enum Probe {
    /// The endpoint answered (any HTTP status — a missing path is still
    /// a live route).
    Reachable,
    /// The endpoint rejected the resolved credential with 401/403.
    CredentialRejected,
    /// Transport failure — the endpoint could not be reached at all.
    Unreachable(String),
    /// The declarations publish no endpoint to probe.
    NoEndpoint,
    /// The endpoint is neither `https` nor plain `http` on a loopback
    /// host, so the resolved credential was not sent and nothing was
    /// probed.
    CredentialWithheld,
}

/// Probe the supplier's declared endpoint: a live HTTP response proves
/// the route exists, and a credential rejection means the configured
/// credential is dead. Transport failures are reported, not judged —
/// whether they refuse a write is the caller's policy. A credential is
/// attached only over `https` or to an explicit loopback `http` endpoint,
/// and redirects are not followed, so it never reaches another origin.
pub fn probe_route(route: &ResolvedRoute) -> Probe {
    let Some(base) = &route.base_url else {
        return Probe::NoEndpoint;
    };
    if route.credential.is_some() && !credential_endpoint_admitted(base) {
        return Probe::CredentialWithheld;
    }
    let url = format!("{}/models", base.trim_end_matches('/'));
    let agent = ureq::AgentBuilder::new()
        .timeout(std::time::Duration::from_secs(15))
        .redirects(0)
        .build();
    let mut req = agent.get(&url);
    match &route.credential {
        Some(Credential::ApiKey(key)) => {
            req = req
                .set("Authorization", &format!("Bearer {key}"))
                .set("x-api-key", key);
        }
        Some(Credential::SessionToken(token)) => {
            req = req.set("x-dsh-auth-token", token);
        }
        None => {}
    }
    match req.call() {
        Ok(_) => Probe::Reachable,
        Err(ureq::Error::Status(401 | 403, _)) if route.credential.is_some() => {
            Probe::CredentialRejected
        }
        Err(ureq::Error::Status(_, _)) => Probe::Reachable,
        Err(e) => Probe::Unreachable(e.to_string()),
    }
}

/// Whether a credential may be presented at `url`: any `https` endpoint,
/// or plain `http` only when the host is loopback (`localhost`,
/// `127.0.0.0/8`, `::1`).
fn credential_endpoint_admitted(url: &str) -> bool {
    let Some((scheme, rest)) = url.split_once("://") else {
        return false;
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    if authority.contains('@') {
        return false;
    }
    match scheme.to_ascii_lowercase().as_str() {
        "https" => !authority.is_empty(),
        "http" => {
            let host = match authority.strip_prefix('[') {
                Some(bracketed) => bracketed.split(']').next().unwrap_or_default(),
                None => authority.split(':').next().unwrap_or_default(),
            };
            host.eq_ignore_ascii_case("localhost")
                || host
                    .parse::<std::net::IpAddr>()
                    .is_ok_and(|ip| ip.is_loopback())
        }
        _ => false,
    }
}

/// Codex model ids from the supplier-fetched catalog codex keeps in
/// `~/.codex/models_cache.json`. `None` when the cache is absent or
/// unreadable — the caller decides whether that warns.
pub fn codex_catalog() -> Option<Vec<String>> {
    let text = std::fs::read_to_string(home().join(".codex/models_cache.json")).ok()?;
    let doc = serde_json::from_str::<serde_json::Value>(&text).ok()?;
    Some(
        doc["models"]
            .as_array()?
            .iter()
            .filter_map(|m| m["slug"].as_str().or_else(|| m["id"].as_str()))
            .map(str::to_string)
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    /// A synthetic credential value, assembled at runtime so no
    /// credential-shaped literal sits in the source.
    fn fixture_value() -> String {
        ["route", "fixture", "value"].join("-")
    }

    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    fn plugin(home: &Path, name: &str, text: &str) {
        write(&dsh_plugins(home).join(name).join("lib/index.js"), text);
    }

    #[test]
    fn debug_output_never_carries_the_credential() {
        let value = fixture_value();
        for credential in [
            Credential::ApiKey(value.clone()),
            Credential::SessionToken(value.clone()),
        ] {
            let route = ResolvedRoute {
                provider: "alpha".into(),
                credential: Some(credential),
                ..Default::default()
            };
            for rendered in [format!("{route:?}"), format!("{route:#?}")] {
                assert!(!rendered.contains(&value), "{rendered}");
                assert!(rendered.contains("[redacted]"), "{rendered}");
            }
        }
        assert_eq!(
            format!("{:?}", Credential::ApiKey(value.clone())),
            "ApiKey([redacted])"
        );
        assert_eq!(
            format!("{:?}", Credential::SessionToken(value)),
            "SessionToken([redacted])"
        );
    }

    #[test]
    fn a_variant_borrows_only_the_endpoint_of_the_package_it_imports() {
        let home = tempfile::tempdir().unwrap();
        plugin(
            home.path(),
            "dsh-llm-zeta",
            "const PUBLIC_BASE_URL = \"https://zeta.invalid/v1\";\n",
        );
        plugin(
            home.path(),
            "dsh-llm-alpha",
            "const PUBLIC_BASE_URL = \"https://alpha.invalid/v1\";\n",
        );
        plugin(
            home.path(),
            "dsh-llm-alpha-key",
            "import { register } from \"@deepseek-ai/dsh-llm-alpha\";\nconst PROVIDER = \"alpha-key\";\n",
        );
        plugin(
            home.path(),
            "dsh-llm-orphan",
            "import { util } from \"@deepseek-ai/dsh-util\";\nconst PROVIDER = \"orphan\";\n",
        );
        let bound = resolve_dsh_route_in(home.path(), "alpha-key", None).unwrap();
        assert_eq!(bound.base_url.as_deref(), Some("https://alpha.invalid/v1"));
        // A variant importing no adapter package publishes no endpoint —
        // the unrelated siblings' endpoints are never borrowed.
        let orphan = resolve_dsh_route_in(home.path(), "orphan", None).unwrap();
        assert_eq!(orphan.base_url, None);
    }

    #[test]
    fn a_bound_lane_takes_endpoint_and_credential_from_its_own_profile_only() {
        let home = tempfile::tempdir().unwrap();
        let patch = |base: &str, env: &str| {
            format!(
                "plugins:\n  llm-pi-ai:\n    providers:\n      alpha:\n        apiKeyEnv: {env}\n        baseURL: {base}\n"
            )
        };
        write(
            &home.path().join(".dsh/profiles/one/cordis.patch.yml"),
            &patch("https://one.invalid", "ONE_REF"),
        );
        write(
            &home.path().join(".dsh/profiles/two/cordis.patch.yml"),
            &patch("https://two.invalid", "TWO_REF"),
        );
        write(
            &home.path().join(".dsh/.credentials.yaml"),
            &format!("refs:\n  ONE_REF: {}\n", fixture_value()),
        );
        let route = resolve_dsh_route_in(home.path(), "alpha", Some("one")).unwrap();
        assert_eq!(route.base_url.as_deref(), Some("https://one.invalid"));
        assert_eq!(route.api_key_env.as_deref(), Some("ONE_REF"));
        assert!(matches!(&route.credential, Some(Credential::ApiKey(v)) if *v == fixture_value()));
        let route = resolve_dsh_route_in(home.path(), "alpha", Some("two")).unwrap();
        assert_eq!(route.base_url.as_deref(), Some("https://two.invalid"));
        assert!(route.credential.is_none());
    }

    #[test]
    fn credentials_go_only_to_https_or_loopback_http() {
        for admitted in [
            "https://api.invalid/v1",
            "HTTPS://api.invalid",
            "http://127.0.0.1:8080/v1",
            "http://127.1.2.3",
            "http://localhost:9000",
            "http://[::1]:7000/v1",
        ] {
            assert!(credential_endpoint_admitted(admitted), "{admitted}");
        }
        for refused in [
            "http://api.invalid/v1",
            "http://127.0.0.1.invalid/v1",
            "http://user@127.0.0.1/v1",
            "ftp://api.invalid",
            "api.invalid/v1",
            "https://",
        ] {
            assert!(!credential_endpoint_admitted(refused), "{refused}");
        }
    }

    #[test]
    fn a_plaintext_remote_endpoint_never_receives_the_credential() {
        let route = ResolvedRoute {
            base_url: Some("http://api.invalid/v1".into()),
            credential: Some(Credential::ApiKey(fixture_value())),
            ..Default::default()
        };
        assert!(matches!(probe_route(&route), Probe::CredentialWithheld));
    }

    #[test]
    fn a_loopback_endpoint_receives_the_credential() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = Vec::new();
            let mut buffer = [0u8; 1024];
            while !request.windows(4).any(|w| w == b"\r\n\r\n") {
                let read = stream.read(&mut buffer).unwrap();
                if read == 0 {
                    break;
                }
                request.extend_from_slice(&buffer[..read]);
            }
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                .unwrap();
            String::from_utf8_lossy(&request).into_owned()
        });
        let route = ResolvedRoute {
            base_url: Some(format!("http://127.0.0.1:{port}")),
            credential: Some(Credential::SessionToken(fixture_value())),
            ..Default::default()
        };
        assert!(matches!(probe_route(&route), Probe::Reachable));
        let request = server.join().unwrap().to_ascii_lowercase();
        assert!(request.starts_with("get /models "), "{request}");
        assert!(
            request.contains(&format!("x-dsh-auth-token: {}", fixture_value())),
            "{request}"
        );
    }
}
