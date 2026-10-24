//! Command Code as a DSH ACP provider route.
//!
//! DSH ACP advertises the live LLM catalog through `session/new`
//! `configOptions`. Command Code is not a shipped pi-ai catalog route: it
//! is a hand-declared `@deepseek-ai/dsh-llm-pi-ai` provider that authenticates
//! with the `COMMAND_CODE_API_KEY` credential *reference* stored under
//! `~/.dsh/.credentials.yaml`. This module emits the ACP-profile
//! `cordis.patch.yml` fragment that mounts that route, and reads only
//! credential *names* from a credentials document — never the secret values.
//!
//! Tests pass fixture documents; the production reader is
//! [`mount_command_code`], which still returns only reference names.
//!
//! Provisioning is an explicit host step: [`mount_command_code`] reads the
//! DSH credentials document, checks that the `COMMAND_CODE_API_KEY`
//! *reference* is registered, and writes the ACP profile patch atomically.
//! The `a4aa-dsh mount` binary is that step's production entry point; DSH
//! then advertises the route on the next `session/new`.

use serde_json::Value;
use std::path::{Path, PathBuf};

/// Credential reference DSH resolves per request for the Command Code route.
pub const COMMAND_CODE_API_KEY_ENV: &str = "COMMAND_CODE_API_KEY";

/// Provider route name selected with `GenerateOptions.provider`.
pub const COMMAND_CODE_PROVIDER: &str = "command-code";

/// Built-in pi-ai adapter that owns hand-declared provider routes.
pub const PI_AI_PLUGIN: &str = "@deepseek-ai/dsh-llm-pi-ai";

/// Cordis patch entry id for the pi-ai adapter on the ACP profile.
pub const PI_AI_PATCH_ID: &str = "llm-pi-ai";

/// OpenAI-compatible Command Code gateway.
pub const COMMAND_CODE_BASE_URL: &str = "https://api.commandcode.ai/provider/v1";

/// Home-relative ACP profile patch the host writes (`UserHome` scope starts at `~`).
pub const ACP_PROFILE_PATCH: &str = ".dsh/profiles/acp/cordis.patch.yml";

/// Credentials document, relative to the DSH home (`$DSH_HOME/.credentials.yaml`).
pub const DSH_CREDENTIALS: &str = ".credentials.yaml";

/// Profile patch, relative to the DSH home.
pub const DSH_PROFILE_PATCH: &str = "profiles/acp/cordis.patch.yml";

/// What [`mount_command_code`] may do to an existing profile patch.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MountPolicy {
    /// Refuse to overwrite a patch that differs from the rendered one.
    CreateOnly,
    /// Replace an existing patch with the rendered one.
    Replace,
}

/// Result of a successful mount.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MountedCommandCode {
    /// Absolute patch path that DSH reads on its next boot.
    pub patch_path: PathBuf,
    /// Whether the file changed (false when the rendered patch was already
    /// installed byte-for-byte).
    pub changed: bool,
}

/// One model a hand-declared Command Code route can serve.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommandCodeModel {
    pub id: String,
    pub name: String,
}

/// Failure while mounting Command Code onto the ACP profile patch.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CommandCodeMountError {
    /// The credentials document names no `COMMAND_CODE_API_KEY` reference.
    MissingCredentialRef,
    /// A hand-declared pi-ai route is unserviceable without models.
    EmptyModels,
    /// The gateway endpoint must be an `https://` URL.
    InsecureBaseUrl,
    /// The route carries the `COMMAND_CODE_API_KEY` reference, so its
    /// endpoint must be a public host — loopback, private and link-local
    /// targets would receive a credential they have no claim to.
    PrivateBaseUrl,
    /// The DSH credentials document could not be read.
    CredentialsUnreadable,
    /// A different patch is already installed; the caller must opt in to
    /// replacing it instead of silently discarding other entries.
    PatchExists,
    /// The profile patch could not be written.
    PatchWriteFailed,
}

impl std::fmt::Display for CommandCodeMountError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingCredentialRef => write!(
                formatter,
                "credentials document has no {COMMAND_CODE_API_KEY_ENV} reference"
            ),
            Self::EmptyModels => write!(
                formatter,
                "command-code route requires a non-empty models list"
            ),
            Self::InsecureBaseUrl => write!(formatter, "command-code base URL must use https"),
            Self::PrivateBaseUrl => {
                write!(formatter, "command-code base URL must name a public host")
            }
            Self::CredentialsUnreadable => write!(
                formatter,
                "DSH credentials document could not be read; nothing was mounted"
            ),
            Self::PatchExists => write!(
                formatter,
                "a different DSH profile patch already exists; pass --replace to overwrite it"
            ),
            Self::PatchWriteFailed => {
                write!(formatter, "DSH profile patch could not be written")
            }
        }
    }
}

impl std::error::Error for CommandCodeMountError {}

/// The mounted route authenticates with the `COMMAND_CODE_API_KEY`
/// reference, so its endpoint must be a public host: a loopback, private
/// or link-local target would receive a credential it has no claim to.
/// Userinfo is refused outright — an embedded `userinfo@` authority would
/// smuggle a second credential into the same URL.
fn base_url_has_private_host(base_url: &str) -> bool {
    let authority = base_url["https://".len()..]
        // WHATWG HTTPS parsing treats a backslash as a path separator, so
        // a loopback authority followed by `\@public-host` really targets
        // the loopback address — split on it here too or the guard and the
        // downstream parser disagree.
        .split(['/', '\\', '?', '#'])
        .next()
        .unwrap_or_default();
    if authority.contains('@') {
        return true;
    }
    let host = if let Some(rest) = authority.strip_prefix('[') {
        rest.split(']').next().unwrap_or_default()
    } else {
        authority.split(':').next().unwrap_or_default()
    }
    .to_ascii_lowercase();
    // A trailing-dot FQDN names the same host as its bare form.
    let host = host.trim_end_matches('.');
    if host.is_empty()
        || host == "localhost"
        || host.ends_with(".localhost")
        || host.ends_with(".local")
        || host.ends_with(".internal")
    {
        return true;
    }
    match host.parse::<std::net::IpAddr>() {
        Ok(std::net::IpAddr::V4(address)) => is_private_v4(&address),
        Ok(std::net::IpAddr::V6(address)) => {
            // An IPv4-mapped/compatible IPv6 literal names the same private
            // destination as the IPv4 form.
            if let Some(v4) = address.to_ipv4() {
                return is_private_v4(&v4);
            }
            address.is_loopback()
                || address.is_unspecified()
                || (address.segments()[0] & 0xffc0) == 0xfe80
                || (address.segments()[0] & 0xfe00) == 0xfc00
        }
        // Non-canonical IP spellings — 2130706433, 0x7f000001, 127.1,
        // 0177.0.0.1 — fail IpAddr parsing but system resolvers still
        // read them as loopback or private destinations, so reject any
        // host spelled purely in numeric/dotted/hex form.
        Err(_) => {
            // A ':' or '%' only reaches here from bracketed IPv6 content
            // that failed to parse — malformed or zone-scoped literals are
            // never public hosts. Otherwise reject purely numeric/
            // dotted/hex spellings (2130706433, 0x7f000001, 127.1).
            host.contains([':', '%'])
                || (host.bytes().any(|b| b.is_ascii_digit())
                    && host
                        .bytes()
                        .all(|b| b.is_ascii_hexdigit() || matches!(b, b'.' | b'x' | b'X')))
        }
    }
}

fn is_private_v4(address: &std::net::Ipv4Addr) -> bool {
    address.is_private()
        || address.is_loopback()
        || address.is_link_local()
        || address.is_unspecified()
        || address.is_broadcast()
}

/// Names present under `refs:` in a DSH credentials YAML document.
///
/// Values are skipped — this function never returns secret material.
/// The live `~/.dsh/.credentials.yaml` is not read here; callers pass a
/// document (tests pass fixtures).
pub fn credential_ref_names(document: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut in_refs = false;
    for raw in document.lines() {
        let trimmed = raw.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let indent = raw.len() - raw.trim_start().len();
        if indent == 0 {
            in_refs = trimmed.starts_with("refs:");
            continue;
        }
        if !in_refs {
            continue;
        }
        if let Some(name) = ref_key(trimmed) {
            names.push(name.to_owned());
        }
    }
    names
}

/// Whether a credentials document names the Command Code API-key reference.
pub fn has_command_code_key(document: &str) -> bool {
    credential_ref_names(document)
        .iter()
        .any(|name| name == COMMAND_CODE_API_KEY_ENV)
}

/// Render the ACP-profile patch that mounts Command Code through pi-ai on
/// the canonical gateway.
///
/// The patch authenticates with `apiKeyEnv: COMMAND_CODE_API_KEY` — the
/// reference name only. DSH resolves the secret from its credential store
/// at request time.
pub fn command_code_patch(
    credentials_yaml: &str,
    models: &[CommandCodeModel],
) -> Result<String, CommandCodeMountError> {
    command_code_patch_endpoint(credentials_yaml, COMMAND_CODE_BASE_URL, models)
}

/// Render the patch for an operator-declared custom gateway.
///
/// A custom endpoint is an explicit trusted-user setting: the caller owns
/// `base_url` and must not forward values from untrusted workspace or
/// third-party configuration — the public-host checks below validate the
/// literal spelling only and cannot see which address a hostname resolves
/// to when DSH later connects.
pub fn command_code_patch_endpoint(
    credentials_yaml: &str,
    base_url: &str,
    models: &[CommandCodeModel],
) -> Result<String, CommandCodeMountError> {
    if !has_command_code_key(credentials_yaml) {
        return Err(CommandCodeMountError::MissingCredentialRef);
    }
    if !base_url.starts_with("https://") {
        return Err(CommandCodeMountError::InsecureBaseUrl);
    }
    if base_url_has_private_host(base_url) {
        return Err(CommandCodeMountError::PrivateBaseUrl);
    }
    if models.is_empty() {
        return Err(CommandCodeMountError::EmptyModels);
    }
    let mut body = format!(
        "\
- id: {PI_AI_PATCH_ID}
  name: '{PI_AI_PLUGIN}'
  config:
    providers:
      {COMMAND_CODE_PROVIDER}:
        displayName: Command Code
        apiKeyEnv: {COMMAND_CODE_API_KEY_ENV}
        api: openai-completions
        baseURL: {base_url}
        compat:
          supportsStore: false
          supportsDeveloperRole: false
          supportsStrictMode: false
          maxTokensField: max_completion_tokens
        models:
",
        base_url = yaml_plain(base_url)
    );
    for model in models {
        body.push_str(&format!(
            "        - id: {}\n          name: {}\n",
            yaml_plain(&model.id),
            yaml_plain(&model.name)
        ));
    }
    Ok(body)
}

/// Mount Command Code onto a DSH home's ACP profile.
///
/// This is the production provisioning path behind the `a4aa-dsh mount`
/// binary: it reads `<dsh_home>/.credentials.yaml` (only the `refs:` names;
/// the credential values never leave the file), requires the
/// `COMMAND_CODE_API_KEY` reference, renders the `@deepseek-ai/dsh-llm-pi-ai`
/// route patch, and installs it at `<dsh_home>/profiles/acp/cordis.patch.yml`.
/// DSH reads that patch on the next boot, so the following `session/new`
/// advertises the Command Code provider and its models.
///
/// `CreateOnly` refuses to overwrite a different existing patch — a host
/// may keep other profile entries there — while `Replace` is the explicit
/// opt-in used by `a4aa-dsh mount --replace`.
pub fn mount_command_code(
    dsh_home: &Path,
    base_url: &str,
    models: &[CommandCodeModel],
    policy: MountPolicy,
) -> Result<MountedCommandCode, CommandCodeMountError> {
    let credentials = std::fs::read_to_string(dsh_home.join(DSH_CREDENTIALS))
        .map_err(|_| CommandCodeMountError::CredentialsUnreadable)?;
    let patch = command_code_patch_endpoint(&credentials, base_url, models)?;
    let patch_path = dsh_home.join(DSH_PROFILE_PATCH);
    match std::fs::read_to_string(&patch_path) {
        Ok(existing) if existing == patch => {
            return Ok(MountedCommandCode {
                patch_path,
                changed: false,
            })
        }
        Ok(_) => {
            if policy == MountPolicy::CreateOnly {
                return Err(CommandCodeMountError::PatchExists);
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => {
            // An unreadable file is treated as a different patch: never
            // clobber state the mount could not inspect.
            if policy == MountPolicy::CreateOnly {
                return Err(CommandCodeMountError::PatchExists);
            }
        }
    }
    let parent = patch_path
        .parent()
        .ok_or(CommandCodeMountError::PatchWriteFailed)?;
    std::fs::create_dir_all(parent).map_err(|_| CommandCodeMountError::PatchWriteFailed)?;
    let staged = parent.join(format!(".cordis.patch.yml.tmp-{}", std::process::id()));
    std::fs::write(&staged, patch.as_bytes())
        .map_err(|_| CommandCodeMountError::PatchWriteFailed)?;
    if std::fs::rename(&staged, &patch_path).is_err() {
        let _ = std::fs::remove_file(&staged);
        return Err(CommandCodeMountError::PatchWriteFailed);
    }
    Ok(MountedCommandCode {
        patch_path,
        changed: true,
    })
}

/// A provider together with the models `session/new` advertised for it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AdvertisedProvider {
    pub id: String,
    pub name: String,
    pub models: Vec<AdvertisedModel>,
}

/// One selectable model inside an advertised provider group.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AdvertisedModel {
    pub id: String,
    pub name: String,
    /// Opaque ACP `configOptions` value (`JSON.stringify([provider, model])`
    /// on DSH).
    pub value: String,
}

/// Read registered providers and models from an ACP `configOptions` array
/// as DSH ACP returns them on `session/new`.
///
/// DSH groups model choices by provider. Ungrouped options fall back to a
/// single anonymous group. DSH model values are `["provider","model"]`
/// JSON; other encodings keep the whole value as the model id.
pub fn providers_from_config_options(config_options: &Value) -> Vec<AdvertisedProvider> {
    let Some(options) = config_options.as_array() else {
        return Vec::new();
    };
    let Some(model_option) = options.iter().find(|option| is_model_option(option)) else {
        return Vec::new();
    };
    let Some(entries) = model_option.get("options").and_then(Value::as_array) else {
        return Vec::new();
    };
    if entries.iter().any(is_group) {
        entries.iter().filter_map(parse_group).collect()
    } else {
        let models = entries
            .iter()
            .filter_map(parse_model_entry)
            .collect::<Vec<_>>();
        if models.is_empty() {
            Vec::new()
        } else {
            vec![AdvertisedProvider {
                id: String::new(),
                name: String::new(),
                models,
            }]
        }
    }
}

fn is_model_option(option: &Value) -> bool {
    option.get("category").and_then(Value::as_str) == Some("model")
        || option.get("id").and_then(Value::as_str) == Some("model")
}

fn is_group(entry: &Value) -> bool {
    entry.get("group").is_some() || entry.get("options").and_then(Value::as_array).is_some()
}

fn parse_group(entry: &Value) -> Option<AdvertisedProvider> {
    let nested = entry.get("options").and_then(Value::as_array)?;
    let models = nested
        .iter()
        .filter_map(parse_model_entry)
        .collect::<Vec<_>>();
    if models.is_empty() {
        return None;
    }
    let id = entry
        .get("group")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    let name = entry
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or(id.as_str())
        .to_owned();
    Some(AdvertisedProvider { id, name, models })
}

fn parse_model_entry(entry: &Value) -> Option<AdvertisedModel> {
    let value = entry.get("value").and_then(Value::as_str)?.to_owned();
    let name = entry
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or(value.as_str())
        .to_owned();
    let id = decode_model_id(&value);
    Some(AdvertisedModel { id, name, value })
}

fn decode_model_id(value: &str) -> String {
    match serde_json::from_str::<Value>(value) {
        Ok(Value::Array(parts)) if parts.len() >= 2 => {
            parts[1].as_str().unwrap_or(value).to_owned()
        }
        _ => value.to_owned(),
    }
}

fn ref_key(trimmed: &str) -> Option<&str> {
    let (name, _rest) = trimmed.split_once(':')?;
    let name = name.trim();
    if name.is_empty() {
        return None;
    }
    Some(name)
}

fn yaml_plain(value: &str) -> String {
    if value.is_empty()
        || value
            .bytes()
            .any(|byte| matches!(byte, b':' | b'#' | b'\'' | b'"' | b'\n' | b'[' | b']'))
    {
        format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
    } else {
        value.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CREDENTIALS: &str = include_str!("../tests/fixtures/dsh-command-code-mount.yaml");
    const CREDENTIALS_REORDERED: &str =
        include_str!("../tests/fixtures/dsh-command-code-mount-alt.yaml");
    const CREDENTIALS_WITHOUT: &str =
        include_str!("../tests/fixtures/dsh-command-code-absent.yaml");
    const CREDENTIALS_COMMENTED: &str =
        include_str!("../tests/fixtures/dsh-command-code-absent-alt.yaml");
    const SESSION_NEW: &str = include_str!("../tests/fixtures/session_new_config_options.json");

    #[test]
    fn fixture_credentials_name_the_command_code_ref_without_exposing_values() {
        let names = credential_ref_names(CREDENTIALS);
        assert!(names.iter().any(|name| name == COMMAND_CODE_API_KEY_ENV));
        assert!(names.iter().any(|name| name == "DEEPSEEK_API_KEY"));
        // The parser returns names only — a secret-shaped token must not leak.
        let joined = names.join(" ");
        assert!(!joined.contains("fixture-"));
        assert!(has_command_code_key(CREDENTIALS));
        assert!(!has_command_code_key(CREDENTIALS_WITHOUT));
        // Ref order is irrelevant: names are read positionally, not by line.
        assert_eq!(
            credential_ref_names(CREDENTIALS_REORDERED),
            vec![
                "COMMAND_CODE_API_KEY".to_owned(),
                "DEEPSEEK_API_KEY".to_owned()
            ]
        );
        // A comment inside the refs block is skipped, not read as a name.
        assert!(!has_command_code_key(CREDENTIALS_COMMENTED));
        assert_eq!(
            credential_ref_names(CREDENTIALS_COMMENTED),
            vec!["DEEPSEEK_API_KEY".to_owned()]
        );
    }

    fn mount_fixture() -> (tempfile::TempDir, Vec<CommandCodeModel>) {
        let home = tempfile::tempdir().expect("temp home");
        std::fs::create_dir_all(home.path().join(".dsh")).unwrap();
        std::fs::write(home.path().join(DSH_CREDENTIALS), CREDENTIALS).unwrap();
        (
            home,
            vec![CommandCodeModel {
                id: "gpt-5.4".into(),
                name: "GPT-5.4".into(),
            }],
        )
    }

    #[test]
    fn production_mount_reads_the_credential_ref_and_installs_the_patch() {
        let (home, models) = mount_fixture();
        let mounted = mount_command_code(
            home.path(),
            COMMAND_CODE_BASE_URL,
            &models,
            MountPolicy::CreateOnly,
        )
        .expect("mount");
        assert!(mounted.changed);
        assert_eq!(mounted.patch_path, home.path().join(DSH_PROFILE_PATCH));
        let installed = std::fs::read_to_string(&mounted.patch_path).expect("patch");
        assert!(installed.contains(PI_AI_PLUGIN));
        assert!(installed.contains("apiKeyEnv: COMMAND_CODE_API_KEY"));
        assert!(installed.contains("id: gpt-5.4"));
        // Credential values from the fixture never cross into the patch.
        assert!(!installed.contains("fixture"));
        // A second mount is idempotent rather than a rewrite.
        let again = mount_command_code(
            home.path(),
            COMMAND_CODE_BASE_URL,
            &models,
            MountPolicy::CreateOnly,
        )
        .expect("idempotent mount");
        assert!(!again.changed);
    }

    #[test]
    fn create_only_refuses_to_clobber_a_different_patch() {
        let (home, models) = mount_fixture();
        let path = home.path().join(DSH_PROFILE_PATCH);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "- id: other-entry\n").unwrap();
        let refused = mount_command_code(
            home.path(),
            COMMAND_CODE_BASE_URL,
            &models,
            MountPolicy::CreateOnly,
        )
        .expect_err("a different patch needs explicit replacement");
        assert_eq!(refused, CommandCodeMountError::PatchExists);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "- id: other-entry\n"
        );
        mount_command_code(
            home.path(),
            COMMAND_CODE_BASE_URL,
            &models,
            MountPolicy::Replace,
        )
        .expect("explicit replacement");
        assert!(std::fs::read_to_string(&path)
            .unwrap()
            .contains(PI_AI_PLUGIN));
    }

    #[test]
    fn mount_refuses_a_missing_credentials_file_or_ref_before_writing() {
        let home = tempfile::tempdir().unwrap();
        let models = [CommandCodeModel {
            id: "gpt-5.4".into(),
            name: "GPT-5.4".into(),
        }];
        assert_eq!(
            mount_command_code(
                home.path(),
                COMMAND_CODE_BASE_URL,
                &models,
                MountPolicy::Replace
            ),
            Err(CommandCodeMountError::CredentialsUnreadable)
        );
        assert!(!home.path().join(DSH_PROFILE_PATCH).exists());

        std::fs::create_dir_all(home.path().join(".dsh")).unwrap();
        std::fs::write(home.path().join(DSH_CREDENTIALS), CREDENTIALS_WITHOUT).unwrap();
        assert_eq!(
            mount_command_code(
                home.path(),
                COMMAND_CODE_BASE_URL,
                &models,
                MountPolicy::Replace
            ),
            Err(CommandCodeMountError::MissingCredentialRef)
        );
        assert!(!home.path().join(DSH_PROFILE_PATCH).exists());
    }

    #[test]
    fn mount_requires_the_named_ref_and_at_least_one_model() {
        let models = [CommandCodeModel {
            id: "gpt-5.4".into(),
            name: "GPT-5.4".into(),
        }];
        assert_eq!(
            command_code_patch(CREDENTIALS_WITHOUT, &models),
            Err(CommandCodeMountError::MissingCredentialRef)
        );
        assert_eq!(
            command_code_patch(CREDENTIALS, &[]),
            Err(CommandCodeMountError::EmptyModels)
        );
        assert_eq!(
            command_code_patch_endpoint(CREDENTIALS, "http://gateway.internal/v1", &models),
            Err(CommandCodeMountError::InsecureBaseUrl)
        );
        // Build the literal spellings at runtime: the guard's job is to
        // reject every private spelling, not to keep a table of them.
        let v4 = |first: u8, second: u8, third: u8, fourth: u8| {
            format!("https://{first}.{second}.{third}.{fourth}/v1")
        };
        let v4_host = |first: u8, second: u8, third: u8, fourth: u8| {
            format!("{first}.{second}.{third}.{fourth}")
        };
        let v6 = |left: &str, right: &str| format!("https://[{left}::{right}]/v1");
        for url in [
            "https://localhost:8443/v1".to_owned(),
            v4(10, 0, 0, 9),
            v4(192, 168, 1, 10),
            v4(169, 254, 169, 254),
            v6("fd00", "1"),
            v6("::ffff", &v4_host(127, 0, 0, 1)),
            v6("::ffff", &v4_host(10, 0, 0, 9)),
            v6("::ffff", &v4_host(169, 254, 169, 254)),
            // Non-canonical spellings resolvers still read as loopback.
            format!("https://{}/v1", 2_130_706_433u32),
            format!("https://0x{:x}/v1", 2_130_706_433u32),
            format!("https://{}.{}/v1", 127, 1),
            "https://localhost./v1".to_owned(),
            format!("https://[{}::{}%25eth0]/v1", "fe80", 1),
            format!(
                "https://{}\\@public.example/v1",
                v4(127, 0, 0, 1)
                    .trim_start_matches("https://")
                    .trim_end_matches("/v1")
            ),
            format!(
                "https://{}\\@public.example/v1",
                v4(10, 0, 0, 9)
                    .trim_start_matches("https://")
                    .trim_end_matches("/v1")
            ),
            // Userinfo would smuggle a second credential into the route.
            format!("https://{}:{}@{}/v1", "user", "pass", "api.commandcode.ai"),
            format!(
                "https://{}@{}/v1",
                "user",
                v4(10, 0, 0, 9)
                    .trim_start_matches("https://")
                    .trim_end_matches("/v1")
            ),
        ] {
            assert_eq!(
                command_code_patch_endpoint(CREDENTIALS, &url, &models),
                Err(CommandCodeMountError::PrivateBaseUrl),
                "{url} must not receive the credential reference"
            );
        }
        let patch = command_code_patch(CREDENTIALS, &models).expect("mount");
        assert!(patch.contains(PI_AI_PLUGIN));
        assert!(patch.contains("apiKeyEnv: COMMAND_CODE_API_KEY"));
        assert!(patch.contains("command-code:"));
        assert!(patch.contains(COMMAND_CODE_BASE_URL));
        assert!(patch.contains("id: gpt-5.4"));
        assert!(!patch.contains("fixture-command-code-key"));
    }

    #[test]
    fn session_new_config_options_list_registered_providers_and_models() {
        let payload: Value = serde_json::from_str(SESSION_NEW).expect("fixture json");
        let providers = providers_from_config_options(&payload["configOptions"]);
        assert_eq!(
            providers
                .iter()
                .map(|provider| provider.id.as_str())
                .collect::<Vec<_>>(),
            vec!["deepseek-official", "command-code"]
        );
        let command_code = providers
            .iter()
            .find(|provider| provider.id == COMMAND_CODE_PROVIDER)
            .expect("command-code group");
        assert_eq!(
            command_code
                .models
                .iter()
                .map(|model| model.id.as_str())
                .collect::<Vec<_>>(),
            vec!["gpt-5.4", "gpt-5.4-mini"]
        );
        assert!(command_code.models[0].value.contains("command-code"));
    }
}
