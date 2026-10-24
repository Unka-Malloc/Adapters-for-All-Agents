//! CLI-lane MCP admission: the lane has no native registration surface, so a
//! host-supplied server list must be refused before any process exists rather
//! than silently dropped.
#![cfg(unix)]

use a4aa::{
    Adapter, AgentSpec, CliLaneSpec, CommandSpec, EnvPolicy, EnvValue, LaneSpec, PromptChannel,
    SessionRequest, StreamMode,
};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use tokio::sync::mpsc;

fn marker_command(directory: &std::path::Path, marker: &std::path::Path) -> CommandSpec {
    use std::os::unix::fs::PermissionsExt;

    let script = directory.join("cli-agent.sh");
    std::fs::write(
        &script,
        "#!/bin/sh\n: > \"$CLI_MCP_MARKER\"\nprintf 'session-1\\n'\n",
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
    CommandSpec {
        program: script.display().to_string(),
        args: vec![],
        cwd: None,
        env: EnvPolicy::Sanitized {
            inherit: vec![],
            set: BTreeMap::from([(
                "CLI_MCP_MARKER".to_owned(),
                EnvValue::Literal(marker.display().to_string()),
            )]),
        },
    }
}

fn spec(directory: &std::path::Path, servers: Vec<Value>) -> AgentSpec {
    AgentSpec {
        id: "cli-mcp".into(),
        client_name: None,
        lane: LaneSpec::Cli(Box::new(CliLaneSpec {
            args: vec!["{prompt}".into()],
            prompt_channel: PromptChannel::ArgOnly,
            stream: StreamMode::Stdio,
            open_args: vec!["--create".into()],
            mcp_servers: servers,
            ..Default::default()
        })),
        command: marker_command(directory, &directory.join("spawned")),
        max_output_bytes: None,
    }
}

#[tokio::test]
async fn cli_mcp_servers_fail_closed_before_spawn() {
    let directory = tempfile::tempdir().unwrap();
    let (tx, _rx) = mpsc::unbounded_channel();
    let error = match Adapter::new(spec(
        directory.path(),
        vec![json!({"name": "host-tools", "command": "/bin/true"})],
    ))
    .open_session(SessionRequest::default(), tx)
    .await
    {
        Ok(_) => panic!("a cli lane cannot register MCP servers and must refuse the list"),
        Err(error) => error,
    };
    assert_eq!(error.code, "agent_mcp_server_invalid", "{error}");
    assert!(
        error.to_string().contains("no MCP registration surface"),
        "{error}"
    );
    assert!(
        !directory.path().join("spawned").exists(),
        "refusal must happen before the agent process is spawned"
    );
}

#[tokio::test]
async fn cli_without_mcp_servers_opens_normally() {
    let directory = tempfile::tempdir().unwrap();
    let (tx, _rx) = mpsc::unbounded_channel();
    let session = Adapter::new(spec(directory.path(), vec![]))
        .open_session(SessionRequest::default(), tx)
        .await
        .expect("open without MCP servers");
    assert_eq!(session.session_id(), "session-1");
    assert!(directory.path().join("spawned").exists());
    session.close().await.expect("close");
}
