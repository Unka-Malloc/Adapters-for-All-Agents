//! MCP server declarations carried by a lane spec, and their translation
//! into protocol-native registration surfaces.
//!
//! Hosts and profiles already describe MCP servers in the ACP `session/new`
//! shape (`{name, command, args, env}`). A lane that can register MCP
//! servers declares *how* that list reaches its wire; a lane that cannot
//! must refuse the list rather than silently drop it. Dropping the servers
//! is what turns a verifier turn into a plausible-looking answer with no way
//! to submit a verdict.

use crate::error::AdapterError;
use crate::model::RpcMcpConfig;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

/// One stdio MCP server in the ACP declaration shape.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct McpServer {
    /// Dotted-path-safe name; unique within one lane spec.
    pub name: String,
    /// Executable launched by the agent to speak MCP over stdio.
    pub command: String,
    /// Arguments passed to the executable.
    pub args: Vec<String>,
    /// Environment entries the server needs, in declaration order.
    pub env: Vec<(String, String)>,
}

/// The argv additions and child-environment entries one
/// [`RpcMcpConfig`] needs to install a server list.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct McpInstall {
    /// Arguments appended to the agent's argv.
    pub args: Vec<String>,
    /// Environment entries added before the child is spawned. Values are
    /// passed by name to the agent's own config (`env_vars`), so secrets do
    /// not ride the command line.
    pub env: BTreeMap<String, String>,
}

impl McpServer {
    /// Parse one ACP-shaped server declaration. Unsupported or malformed
    /// declarations are refused; an unrecognized field is never ignored.
    pub fn from_value(value: &Value) -> Result<Self, AdapterError> {
        let object = value.as_object().ok_or_else(|| {
            invalid("an MCP server entry must be an object in the ACP `{name, command, args, env}` shape")
        })?;
        for key in object.keys() {
            if !matches!(key.as_str(), "name" | "command" | "args" | "env") {
                return Err(invalid(format!("unsupported MCP server field `{key}`")));
            }
        }
        let name = object
            .get("name")
            .and_then(Value::as_str)
            .filter(|name| valid_name(name))
            .ok_or_else(|| {
                invalid(
                    "MCP server `name` must be non-empty and use only ASCII letters, digits, `_` or `-`",
                )
            })?
            .to_owned();
        let command = object
            .get("command")
            .and_then(Value::as_str)
            .filter(|command| !command.trim().is_empty())
            .ok_or_else(|| invalid(format!("MCP server `{name}` has no `command`")))?
            .to_owned();
        let args = match object.get("args") {
            None => Vec::new(),
            Some(Value::Array(items)) => items
                .iter()
                .map(|item| {
                    item.as_str().map(str::to_owned).ok_or_else(|| {
                        invalid(format!("MCP server `{name}` has a non-string `args` entry"))
                    })
                })
                .collect::<Result<Vec<_>, _>>()?,
            Some(_) => {
                return Err(invalid(format!(
                    "MCP server `{name}` `args` must be an array of strings"
                )))
            }
        };
        let env = parse_env(&name, object.get("env"))?;
        Ok(Self {
            name,
            command,
            args,
            env,
        })
    }

    /// Parse and validate a lane's whole server list. Duplicate names are
    /// refused because the registration key (and therefore the last writer)
    /// would be ambiguous.
    pub fn parse_all(values: &[Value]) -> Result<Vec<Self>, AdapterError> {
        let mut servers = Vec::with_capacity(values.len());
        let mut names = BTreeSet::new();
        for value in values {
            let server = Self::from_value(value)?;
            if !names.insert(server.name.clone()) {
                return Err(invalid(format!(
                    "MCP server name `{}` is declared more than once",
                    server.name
                )));
            }
            servers.push(server);
        }
        Ok(servers)
    }
}

/// Translate a server list into the argv and environment additions declared
/// by `config`.
///
/// The config-override shape installs each server under
/// `<root>.<name>.{command,args,env_vars}`. Environment values travel in the
/// child's environment and only their *names* appear on the command line;
/// two servers asking for the same name with different values are refused
/// instead of one silently winning.
pub fn install(config: &RpcMcpConfig, servers: &[McpServer]) -> Result<McpInstall, AdapterError> {
    if config.flag.trim().is_empty() || config.root.trim().is_empty() {
        return Err(invalid(
            "MCP registration needs a non-empty argv flag and config root",
        ));
    }
    if !config.tools_approval_servers.is_empty() && config.tools_approval.is_none() {
        return Err(invalid(
            "MCP tools approval names servers but the lane declares no approval key",
        ));
    }
    for name in &config.tools_approval_servers {
        if name.is_empty()
            || !name
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')
        {
            return Err(invalid(
                "MCP tools approval server names must be non-empty ASCII letters, digits, `_` or `-`",
            ));
        }
    }
    let mut installed = McpInstall::default();
    for server in servers {
        let base = format!("{}.{}", config.root, server.name);
        push_config(
            &mut installed.args,
            config,
            &format!("{base}.command"),
            toml_string(&server.command),
        );
        if !server.args.is_empty() {
            push_config(
                &mut installed.args,
                config,
                &format!("{base}.args"),
                toml_array(server.args.iter().map(String::as_str)),
            );
        }
        if !server.env.is_empty() {
            if config.env_vars_key.trim().is_empty() {
                return Err(invalid(format!(
                    "MCP server `{}` carries environment values but this lane cannot forward them",
                    server.name
                )));
            }
            for (name, value) in &server.env {
                match installed.env.get(name) {
                    Some(existing) if existing != value => {
                        return Err(invalid(format!(
                        "MCP environment variable `{name}` has conflicting values across servers"
                    )))
                    }
                    Some(_) => {}
                    None => {
                        installed.env.insert(name.clone(), value.clone());
                    }
                }
            }
            push_config(
                &mut installed.args,
                config,
                &format!("{base}.{}", config.env_vars_key),
                toml_array(server.env.iter().map(|(name, _)| name.as_str())),
            );
        }
        if let Some(approval) = &config.tools_approval {
            if approval.key.trim().is_empty() || approval.value.trim().is_empty() {
                return Err(invalid(
                    "MCP tools approval needs a non-empty config key and value",
                ));
            }
            // Approval is per server, never card-wide. A server the host did
            // not name keeps the agent's prompting default; an empty list
            // therefore installs nothing.
            if config
                .tools_approval_servers
                .iter()
                .any(|name| name == &server.name)
            {
                push_config(
                    &mut installed.args,
                    config,
                    &format!("{base}.{}", approval.key),
                    toml_string(&approval.value),
                );
            }
        }
    }
    Ok(installed)
}

fn push_config(args: &mut Vec<String>, config: &RpcMcpConfig, key: &str, value: String) {
    args.push(config.flag.clone());
    args.push(format!("{key}={value}"));
}

fn parse_env(name: &str, value: Option<&Value>) -> Result<Vec<(String, String)>, AdapterError> {
    let mut env = Vec::new();
    match value {
        None => {}
        // ACP shape: [{"name": ..., "value": ...}]
        Some(Value::Array(items)) => {
            for item in items {
                let object = item.as_object().ok_or_else(|| {
                    invalid(format!(
                        "MCP server `{name}` `env` entries must be `{{name, value}}` objects"
                    ))
                })?;
                let key = object.get("name").and_then(Value::as_str).ok_or_else(|| {
                    invalid(format!("MCP server `{name}` has an env entry with no name"))
                })?;
                let value = object.get("value").and_then(Value::as_str).ok_or_else(|| {
                    invalid(format!(
                        "MCP server `{name}` env `{key}` must have a string value"
                    ))
                })?;
                push_env(&mut env, name, key, value)?;
            }
        }
        // Profile shape: {"KEY": "value"}
        Some(Value::Object(map)) => {
            for (key, item) in map {
                let value = item.as_str().ok_or_else(|| {
                    invalid(format!(
                        "MCP server `{name}` env `{key}` must have a string value"
                    ))
                })?;
                push_env(&mut env, name, key, value)?;
            }
        }
        Some(_) => {
            return Err(invalid(format!(
                "MCP server `{name}` `env` must be an array or object"
            )))
        }
    }
    let mut seen = BTreeSet::new();
    for (key, _) in &env {
        if !seen.insert(key.clone()) {
            return Err(invalid(format!(
                "MCP server `{name}` declares env `{key}` more than once"
            )));
        }
    }
    Ok(env)
}

/// Append one environment entry after validating it against `Command`'s own
/// contract. `Command::envs` panics on a name containing `=` or NUL and on a
/// value containing NUL; a malformed host declaration must become the
/// adapter's typed refusal before any spawn, not an abort mid-open.
fn push_env(
    env: &mut Vec<(String, String)>,
    server: &str,
    key: &str,
    value: &str,
) -> Result<(), AdapterError> {
    if key.trim().is_empty() {
        return Err(invalid(format!(
            "MCP server `{server}` has an env entry with no name"
        )));
    }
    if key.contains('=') || key.contains('\0') {
        return Err(invalid(format!(
            "MCP server `{server}` env name {key:?} cannot be passed to a child process"
        )));
    }
    if value.contains('\0') {
        return Err(invalid(format!(
            "MCP server `{server}` env {key:?} has a NUL in its value"
        )));
    }
    env.push((key.to_owned(), value.to_owned()));
    Ok(())
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')
}

fn invalid(detail: impl Into<String>) -> AdapterError {
    AdapterError::permanent("agent_mcp_server_invalid", "session/open").with_detail(detail.into())
}

/// Encode one TOML basic string. JSON strings and TOML basic strings share
/// the common escapes; control characters use TOML's `\u`/`\U` form.
fn toml_string(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for ch in text.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            ch if ch.is_control() => {
                let code = ch as u32;
                if code <= 0xFFFF {
                    out.push_str(&format!("\\u{code:04X}"));
                } else {
                    out.push_str(&format!("\\U{code:08X}"));
                }
            }
            ch => out.push(ch),
        }
    }
    out.push('"');
    out
}

fn toml_array<'a>(items: impl Iterator<Item = &'a str>) -> String {
    let encoded: Vec<String> = items.map(toml_string).collect();
    format!("[{}]", encoded.join(","))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn config() -> RpcMcpConfig {
        RpcMcpConfig {
            flag: "-c".into(),
            root: "mcp_servers".into(),
            env_vars_key: "env_vars".into(),
            tools_approval: None,
            tools_approval_servers: Vec::new(),
        }
    }

    #[test]
    fn tools_approval_is_installed_per_server() {
        let servers = McpServer::parse_all(&[json!({
            "name": "host-tools",
            "command": "/opt/host-tools/bin/server",
            "args": ["mcp"]
        })])
        .unwrap();
        let mut config = config();
        config.tools_approval = Some(crate::model::RpcMcpToolApproval {
            key: "default_tools_approval_mode".into(),
            value: "approve".into(),
        });
        config.tools_approval_servers = vec!["host-tools".into()];
        let installed = install(&config, &servers).unwrap();
        assert_eq!(
            installed.args.last().map(String::as_str),
            Some("mcp_servers.host-tools.default_tools_approval_mode=\"approve\"")
        );
    }

    #[test]
    fn acp_shaped_servers_become_config_overrides_with_env_names() {
        let servers = McpServer::parse_all(&[json!({
            "name": "host-tools",
            "command": "/opt/host-tools/bin/server",
            "args": ["mcp"],
            "env": [{"name": "HOST_TOOLS_TOKEN", "value": "synthetic-token"}]
        })])
        .unwrap();
        let installed = install(&config(), &servers).unwrap();
        assert_eq!(
            installed.args,
            vec![
                "-c".to_owned(),
                "mcp_servers.host-tools.command=\"/opt/host-tools/bin/server\"".to_owned(),
                "-c".to_owned(),
                "mcp_servers.host-tools.args=[\"mcp\"]".to_owned(),
                "-c".to_owned(),
                "mcp_servers.host-tools.env_vars=[\"HOST_TOOLS_TOKEN\"]".to_owned(),
            ]
        );
        assert_eq!(
            installed.env.get("HOST_TOOLS_TOKEN"),
            Some(&"synthetic-token".to_owned())
        );
    }

    #[test]
    fn profile_env_object_shape_is_accepted() {
        let servers = McpServer::parse_all(&[json!({
            "name": "files",
            "command": "/usr/local/bin/files-mcp",
            "env": {"ROOT": "/tmp"}
        })])
        .unwrap();
        assert_eq!(servers[0].env, vec![("ROOT".to_owned(), "/tmp".to_owned())]);
    }

    #[test]
    fn unknown_fields_are_refused_not_dropped() {
        let error = McpServer::from_value(&json!({
            "name": "remote",
            "url": "https://example.test/mcp"
        }))
        .unwrap_err();
        assert_eq!(error.code, "agent_mcp_server_invalid");
        assert!(
            error.to_string().contains("unsupported MCP server field"),
            "{error}"
        );
    }

    #[test]
    fn duplicate_names_and_conflicting_env_are_refused() {
        let duplicate = McpServer::parse_all(&[
            json!({"name": "same", "command": "/bin/one"}),
            json!({"name": "same", "command": "/bin/two"}),
        ])
        .unwrap_err();
        assert!(
            duplicate.to_string().contains("more than once"),
            "{duplicate}"
        );

        let servers = McpServer::parse_all(&[
            json!({"name": "one", "command": "/bin/one", "env": [{"name": "TOKEN", "value": "a"}]}),
            json!({"name": "two", "command": "/bin/two", "env": [{"name": "TOKEN", "value": "b"}]}),
        ])
        .unwrap();
        let conflict = install(&config(), &servers).unwrap_err();
        assert!(
            conflict.to_string().contains("conflicting values"),
            "{conflict}"
        );
    }

    #[test]
    fn escaped_values_never_break_out_of_the_toml_string() {
        let servers = McpServer::parse_all(&[json!({
            "name": "quoted",
            "command": "/bin/agent",
            "args": ["a\"b", "line\nbreak", "slash\\"]
        })])
        .unwrap();
        let installed = install(&config(), &servers).unwrap();
        assert_eq!(
            installed.args[3],
            "mcp_servers.quoted.args=[\"a\\\"b\",\"line\\nbreak\",\"slash\\\\\"]"
        );
    }

    #[test]
    fn env_without_a_forwarding_key_is_refused() {
        let servers = McpServer::parse_all(&[json!({
            "name": "needs-env",
            "command": "/bin/agent",
            "env": [{"name": "TOKEN", "value": "x"}]
        })])
        .unwrap();
        let mut config = config();
        config.env_vars_key.clear();
        let error = install(&config, &servers).unwrap_err();
        assert!(error.to_string().contains("cannot forward"), "{error}");
    }

    #[test]
    fn approval_is_installed_only_for_a_named_server() {
        let servers = McpServer::parse_all(&[
            json!({"name": "trusted", "command": "/bin/trusted"}),
            json!({"name": "other", "command": "/bin/other"}),
        ])
        .unwrap();
        let mut config = config();
        config.tools_approval = Some(crate::model::RpcMcpToolApproval {
            key: "default_tools_approval_mode".into(),
            value: "approve".into(),
        });
        config.tools_approval_servers = vec!["trusted".into()];
        let installed = install(&config, &servers).unwrap();
        let approval: Vec<&String> = installed
            .args
            .iter()
            .filter(|arg| arg.ends_with("default_tools_approval_mode=\"approve\""))
            .collect();
        assert_eq!(approval.len(), 1, "{:?}", installed.args);
        assert!(
            approval[0].starts_with("mcp_servers.trusted."),
            "the named server keeps prompting off, every other server keeps the default: {:?}",
            installed.args
        );
    }

    #[test]
    fn approval_without_named_servers_pre_approves_nothing() {
        let servers = McpServer::parse_all(&[json!({
            "name": "untrusted",
            "command": "/bin/agent"
        })])
        .unwrap();
        let mut config = config();
        config.tools_approval = Some(crate::model::RpcMcpToolApproval {
            key: "default_tools_approval_mode".into(),
            value: "approve".into(),
        });
        let installed = install(&config, &servers).unwrap();
        assert!(
            installed
                .args
                .iter()
                .all(|arg| !arg.contains("tools_approval_mode")),
            "card-wide approval must no longer exist: {:?}",
            installed.args
        );
    }

    #[test]
    fn approval_names_without_a_key_are_refused_not_silently_dropped() {
        let servers = McpServer::parse_all(&[json!({
            "name": "host-tools",
            "command": "/bin/agent"
        })])
        .unwrap();
        let mut config = config();
        config.tools_approval_servers = vec!["host-tools".into()];
        let error = install(&config, &servers).expect_err("a named approval needs its key");
        assert_eq!(error.code, "agent_mcp_server_invalid", "{error}");
        config.tools_approval = Some(crate::model::RpcMcpToolApproval {
            key: "default_tools_approval_mode".into(),
            value: "approve".into(),
        });
        config.tools_approval_servers = vec!["bad name".into()];
        let error = install(&config, &servers).expect_err("approval names are validated");
        assert_eq!(error.code, "agent_mcp_server_invalid", "{error}");
    }

    #[test]
    fn env_entries_that_command_envs_would_panic_on_are_refused() {
        for invalid in [
            json!({"name": "empty-name", "command": "/bin/agent", "env": {"": "x"}}),
            json!({"name": "blank-name", "command": "/bin/agent", "env": {"  ": "x"}}),
            json!({"name": "equals", "command": "/bin/agent", "env": [{"name": "BAD=NAME", "value": "x"}]}),
            json!({"name": "nul-name", "command": "/bin/agent", "env": [{"name": "BAD\u{0}NAME", "value": "x"}]}),
            json!({"name": "nul-value", "command": "/bin/agent", "env": [{"name": "TOKEN", "value": "bad\u{0}value"}]}),
            // The profile object shape goes through the same validation.
            json!({"name": "object-equals", "command": "/bin/agent", "env": {"BAD=NAME": "x"}}),
            json!({"name": "object-nul-name", "command": "/bin/agent", "env": {"BAD\u{0}NAME": "x"}}),
            json!({"name": "object-nul-value", "command": "/bin/agent", "env": {"TOKEN": "bad\u{0}value"}}),
        ] {
            let error = McpServer::from_value(&invalid)
                .expect_err("malformed environment entries must be refused");
            assert_eq!(error.code, "agent_mcp_server_invalid", "{error}");
        }
    }
}
