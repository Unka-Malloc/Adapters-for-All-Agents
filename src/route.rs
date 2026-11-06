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
use std::path::PathBuf;

/// What the installed declarations say one provider route is. Resolution
/// merges every source that names the provider; the caller decides what
/// is admissible.
#[derive(Debug, Default)]
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
/// transport each provider variant declares.
#[derive(Debug)]
pub enum Credential {
    /// `Authorization: Bearer` plus `x-api-key` (covers OpenAI-shaped and
    /// Anthropic-shaped APIs in one probe).
    ApiKey(String),
    /// `x-dsh-auth-token` session grant (dsh account-provider variant).
    SessionToken(String),
}

fn home() -> PathBuf {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

fn dsh_plugins() -> PathBuf {
    home().join(".local/lib/node_modules/@deepseek-ai/dsh/node_modules/@deepseek-ai")
}

/// Scan `~/.dsh/profiles/*/cordis.patch.yml` for the `providers:` map of
/// the `llm-pi-ai` plugin. The files are machine-written by dsh: provider
/// keys sit at six-space indent under `providers:`, with `apiKeyEnv`,
/// `baseURL`, and a `models:` list of `- id:` entries inside each block.
fn patch_routes(route: &mut ResolvedRoute) {
    let profiles_dir = home().join(".dsh/profiles");
    let Ok(entries) = std::fs::read_dir(&profiles_dir) else {
        return;
    };
    for dir in entries.flatten() {
        let lane = dir.file_name().to_string_lossy().to_string();
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
fn builtin_route(provider: &str, route: &mut ResolvedRoute) -> Option<String> {
    let plugins = dsh_plugins();
    let entries = std::fs::read_dir(&plugins).ok()?;
    for dir in entries.flatten() {
        let name = dir.file_name().to_string_lossy().to_string();
        if !name.starts_with("dsh-llm-") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(dir.path().join("lib/index.js")) else {
            continue;
        };
        let needle = format!("PROVIDER = \"{provider}\"");
        if !text.contains(&needle) {
            continue;
        }
        let mut base = text
            .lines()
            .find(|l| l.contains("PUBLIC_BASE_URL"))
            .and_then(|l| l.split('"').nth(1))
            .unwrap_or_default()
            .to_string();
        route.default_models = text
            .lines()
            .take(80)
            .filter_map(|l| l.trim().strip_prefix("id: \""))
            .filter_map(|rest| rest.split('"').next().map(str::to_string))
            .collect();
        if base.is_empty() {
            // Variant plugins (api-key, account) register the route but
            // share the adapter in the sibling package — the endpoint
            // literal lives there.
            if let Ok(siblings) = std::fs::read_dir(&plugins) {
                for sib in siblings.flatten() {
                    let sib_name = sib.file_name().to_string_lossy().to_string();
                    if !sib_name.starts_with("dsh-llm-") || sib_name == name {
                        continue;
                    }
                    if let Ok(t) = std::fs::read_to_string(sib.path().join("lib/index.js")) {
                        if let Some(url) = t
                            .lines()
                            .find(|l| l.contains("PUBLIC_BASE_URL"))
                            .and_then(|l| l.split('"').nth(1))
                        {
                            base = url.to_string();
                            break;
                        }
                    }
                }
            }
        }
        if route.base_url.is_none() && !base.is_empty() {
            route.base_url = Some(base);
        }
        return Some(name);
    }
    None
}

/// Account-variant plugins authenticate with `x-dsh-auth-token` minted by
/// the sibling `dsh-*-account-platform` service, which stores its session
/// grant as record `<platform>/default` in the credentials file. Resolve
/// the same token the launch would use.
fn account_token(vendor: &str) -> Option<String> {
    let pkg = dsh_plugins().join(format!("dsh-{vendor}-account-platform/lib/index.js"));
    let text = std::fs::read_to_string(pkg).ok()?;
    let platform = text
        .split("credentialKey(\"")
        .nth(1)
        .and_then(|rest| rest.split('"').next())?;
    let record = format!("{platform}/default:");
    let creds = std::fs::read_to_string(home().join(".dsh/.credentials.yaml")).ok()?;
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
fn bundled_catalog(provider: &str, route: &mut ResolvedRoute) {
    let path = home().join(format!(
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
fn credential(env_name: &str) -> Option<String> {
    let text = std::fs::read_to_string(home().join(".dsh/.credentials.yaml")).ok()?;
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
    let mut route = ResolvedRoute {
        provider: provider.to_string(),
        ..Default::default()
    };
    patch_routes(&mut route);
    if let Some(lane) = lane {
        route.lanes.retain(|l| l == lane);
    }
    let has_patch =
        !route.lanes.is_empty() || route.base_url.is_some() || !route.catalog.is_empty();
    let patched_base = route.base_url.clone();
    let patched_catalog = route.catalog.clone();
    bundled_catalog(provider, &mut route);
    let builtin_pkg = if route.lanes.is_empty() {
        builtin_route(provider, &mut route)
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
    let _ = has_patch;
    if let Some(env) = &route.api_key_env {
        route.credential = credential(env).map(Credential::ApiKey);
    } else if let Some(pkg) = &builtin_pkg {
        let variant = std::fs::read_to_string(dsh_plugins().join(format!("{pkg}/lib/index.js")))
            .unwrap_or_default();
        if variant.contains("x-dsh-auth-token") {
            route.credential = account_token(provider.strip_suffix("-account").unwrap_or(provider))
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
}

/// Probe the supplier's declared endpoint: a live HTTP response proves
/// the route exists, and a credential rejection means the configured
/// credential is dead. Transport failures are reported, not judged —
/// whether they refuse a write is the caller's policy.
pub fn probe_route(route: &ResolvedRoute) -> Probe {
    let Some(base) = &route.base_url else {
        return Probe::NoEndpoint;
    };
    let url = format!("{}/models", base.trim_end_matches('/'));
    let agent = ureq::AgentBuilder::new()
        .timeout(std::time::Duration::from_secs(15))
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
