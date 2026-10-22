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
//! Tests pass fixture documents. Do not point this code at the live
//! credentials file.

use serde_json::Value;

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
        }
    }
}

impl std::error::Error for CommandCodeMountError {}

/// The mounted route authenticates with the `COMMAND_CODE_API_KEY`
/// reference, so its endpoint must be a public host: a loopback, private
/// or link-local target would receive a credential it has no claim to.
fn base_url_has_private_host(base_url: &str) -> bool {
    let authority = base_url["https://".len()..]
        .split(['/', '?', '#'])
        .next()
        .unwrap_or_default()
        .rsplit('@')
        .next()
        .unwrap_or_default();
    let host = if let Some(rest) = authority.strip_prefix('[') {
        rest.split(']').next().unwrap_or_default()
    } else {
        authority.split(':').next().unwrap_or_default()
    }
    .to_ascii_lowercase();
    if host.is_empty()
        || host == "localhost"
        || host.ends_with(".localhost")
        || host.ends_with(".local")
        || host.ends_with(".internal")
    {
        return true;
    }
    match host.parse::<std::net::IpAddr>() {
        Ok(std::net::IpAddr::V4(address)) => {
            address.is_private()
                || address.is_loopback()
                || address.is_link_local()
                || address.is_unspecified()
                || address.is_broadcast()
        }
        Ok(std::net::IpAddr::V6(address)) => {
            address.is_loopback()
                || address.is_unspecified()
                || (address.segments()[0] & 0xffc0) == 0xfe80
                || (address.segments()[0] & 0xfe00) == 0xfc00
        }
        Err(_) => false,
    }
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

/// Render the ACP-profile patch that mounts Command Code through pi-ai.
///
/// The patch authenticates with `apiKeyEnv: COMMAND_CODE_API_KEY` — the
/// reference name only. DSH resolves the secret from its credential store
/// at request time.
pub fn command_code_patch(
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

    const CREDENTIALS: &str = include_str!("../tests/fixtures/dsh-credentials-fixture.yaml");
    const CREDENTIALS_WITHOUT: &str =
        include_str!("../tests/fixtures/dsh-credentials-without-command-code-fixture.yaml");
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
    }

    #[test]
    fn mount_requires_the_named_ref_and_at_least_one_model() {
        let models = [CommandCodeModel {
            id: "gpt-5.4".into(),
            name: "GPT-5.4".into(),
        }];
        assert_eq!(
            command_code_patch(CREDENTIALS_WITHOUT, COMMAND_CODE_BASE_URL, &models),
            Err(CommandCodeMountError::MissingCredentialRef)
        );
        assert_eq!(
            command_code_patch(CREDENTIALS, COMMAND_CODE_BASE_URL, &[]),
            Err(CommandCodeMountError::EmptyModels)
        );
        assert_eq!(
            command_code_patch(CREDENTIALS, "http://gateway.internal/v1", &models),
            Err(CommandCodeMountError::InsecureBaseUrl)
        );
        for url in [
            "https://localhost:8443/v1",
            "https://10.0.0.9/v1",
            "https://192.168.1.10/v1",
            "https://169.254.169.254/latest",
            "https://[fd00::1]/v1",
        ] {
            assert_eq!(
                command_code_patch(CREDENTIALS, url, &models),
                Err(CommandCodeMountError::PrivateBaseUrl),
                "{url} must not receive the credential reference"
            );
        }
        let patch = command_code_patch(CREDENTIALS, COMMAND_CODE_BASE_URL, &models).expect("mount");
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
