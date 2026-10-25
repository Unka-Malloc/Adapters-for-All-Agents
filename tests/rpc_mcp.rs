//! RPC-lane MCP registration: admitted on the child argv/environment before
//! the first protocol frame, and refused (never dropped) when the lane has
//! no registration surface.

use a4aa::{
    Adapter, AgentEvent, AgentSpec, CommandSpec, EnvPolicy, EnvValue, LaneSpec, RpcCallSpec,
    RpcLaneSpec, RpcMcpConfig, RpcTransport, SessionRequest, TurnRequest,
};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use tokio::sync::mpsc;

fn config() -> RpcMcpConfig {
    RpcMcpConfig {
        flag: "-c".into(),
        root: "mcp_servers".into(),
        env_vars_key: "env_vars".into(),
        tools_approval: Some(a4aa::RpcMcpToolApproval {
            key: "default_tools_approval_mode".into(),
            value: "approve".into(),
        }),
        tools_approval_servers: vec!["probe".into()],
    }
}

fn rpc(mcp_servers: Vec<Value>, mcp_config: Option<RpcMcpConfig>) -> RpcLaneSpec {
    RpcLaneSpec {
        transport: RpcTransport::Stdio,
        max_frame_bytes: None,
        usage_path: None,
        failure_path: None,
        failure_value: None,
        failure_message_path: None,
        mcp_servers,
        mcp_config,
        initialize: Some(RpcCallSpec {
            method: "initialize".into(),
            params: json!({"clientInfo": {"name": "a4aa-test", "version": "0"}}),
        }),
        initialized: Some("initialized".into()),
        session_open: Some(RpcCallSpec {
            method: "session/new".into(),
            params: json!({"cwd": "{cwd}"}),
        }),
        session_resume: Some(RpcCallSpec {
            method: "session/resume".into(),
            params: json!({"sessionId": "{sessionId}"}),
        }),
        prompt: RpcCallSpec {
            method: "session/prompt".into(),
            params: json!({"text": "{prompt}"}),
        },
        completion: None,
        cancel: None,
        session_close: None,
        text_event_method: None,
        text_path: None,
        result_text_path: Some("text".into()),
        pre_turn: vec![],
        session_scope: Default::default(),
        kill_on_cancel: false,
        envelope: None,
        passthrough: vec![],
    }
}

fn script(directory: &std::path::Path) -> CommandSpec {
    let path = directory.join("agent.py");
    std::fs::write(
        &path,
        r#"
import json, os, sys
root = os.environ["MCP_TEST_DIR"]
with open(os.path.join(root, "argv.json"), "w") as handle:
    json.dump(sys.argv[1:], handle)
with open(os.path.join(root, "env.txt"), "w") as handle:
    handle.write(os.environ.get("MCP_TOKEN", ""))
for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    message = json.loads(line)
    method = message.get("method")
    if "id" not in message:
        continue
    if method == "session/prompt":
        result = {"text": "ok"}
    elif method in ("session/new", "session/resume"):
        result = {"sessionId": "s-1"}
    else:
        result = {}
    print(json.dumps({"jsonrpc": "2.0", "id": message["id"], "result": result}), flush=True)
"#,
    )
    .unwrap();
    let mut set = BTreeMap::new();
    set.insert(
        "MCP_TEST_DIR".to_owned(),
        EnvValue::Literal(directory.display().to_string()),
    );
    CommandSpec {
        program: "python3".into(),
        args: vec!["-u".into(), path.display().to_string()],
        cwd: None,
        env: EnvPolicy::Sanitized {
            inherit: vec![],
            set,
        },
    }
}

fn server() -> Value {
    json!({
        "name": "probe",
        "command": "/usr/bin/true",
        "args": ["--probe"],
        "env": [{"name": "MCP_TOKEN", "value": "secret-value"}]
    })
}

fn spec(directory: &std::path::Path, lane: RpcLaneSpec) -> AgentSpec {
    AgentSpec {
        id: "mcp-rpc".into(),
        client_name: None,
        lane: LaneSpec::Rpc(Box::new(lane)),
        command: script(directory),
        max_output_bytes: None,
    }
}

fn request() -> SessionRequest {
    SessionRequest::default()
}

async fn open(
    spec: AgentSpec,
    request: SessionRequest,
) -> (a4aa::AgentSession, mpsc::UnboundedReceiver<AgentEvent>) {
    let (tx, rx) = mpsc::unbounded_channel();
    let session = Adapter::new(spec)
        .open_session(request, tx)
        .await
        .expect("open_session");
    (session, rx)
}

#[tokio::test]
async fn mcp_servers_are_installed_on_the_child_before_open() {
    let directory = tempfile::tempdir().unwrap();
    let (session, _events) = open(
        spec(directory.path(), rpc(vec![server()], Some(config()))),
        request(),
    )
    .await;

    let argv: Vec<String> =
        serde_json::from_str(&std::fs::read_to_string(directory.path().join("argv.json")).unwrap())
            .unwrap();
    assert_eq!(
        argv,
        vec![
            "-c".to_owned(),
            "mcp_servers.probe.command=\"/usr/bin/true\"".to_owned(),
            "-c".to_owned(),
            "mcp_servers.probe.args=[\"--probe\"]".to_owned(),
            "-c".to_owned(),
            "mcp_servers.probe.env_vars=[\"MCP_TOKEN\"]".to_owned(),
            "-c".to_owned(),
            "mcp_servers.probe.default_tools_approval_mode=\"approve\"".to_owned(),
        ]
    );
    // The value rides the child environment; only its name is on argv.
    assert_eq!(
        std::fs::read_to_string(directory.path().join("env.txt")).unwrap(),
        "secret-value"
    );

    let outcome = session
        .prompt(TurnRequest {
            text: "hi".into(),
            model: None,
            effort: None,
        })
        .await
        .expect("prompt");
    assert_eq!(outcome.text, "ok");
    session.close().await.expect("close");
}

#[tokio::test]
async fn mcp_registration_is_present_on_the_resume_path_too() {
    let directory = tempfile::tempdir().unwrap();
    let mut request = request();
    request.resume = Some("r-1".into());
    let (session, _events) = open(
        spec(directory.path(), rpc(vec![server()], Some(config()))),
        request,
    )
    .await;

    let argv: Vec<String> =
        serde_json::from_str(&std::fs::read_to_string(directory.path().join("argv.json")).unwrap())
            .unwrap();
    assert!(
        argv.iter()
            .any(|arg| arg == "mcp_servers.probe.command=\"/usr/bin/true\""),
        "resume spawn keeps the MCP registration: {argv:?}"
    );
    assert_eq!(session.session_id(), "r-1");
    session.close().await.expect("close");
}

#[tokio::test]
async fn servers_without_a_registration_surface_fail_closed() {
    let directory = tempfile::tempdir().unwrap();
    let (tx, _rx) = mpsc::unbounded_channel();
    let error = match Adapter::new(spec(directory.path(), rpc(vec![server()], None)))
        .open_session(request(), tx)
        .await
    {
        Ok(_) => panic!("a non-empty server list without a surface must fail closed"),
        Err(error) => error,
    };
    assert_eq!(error.code, "agent_mcp_server_invalid");
    assert!(
        error.to_string().contains("no MCP registration surface"),
        "{error}"
    );
    assert!(
        !directory.path().join("argv.json").exists(),
        "the agent must not be spawned when admission refuses the MCP list"
    );
}

#[tokio::test]
async fn a_lane_without_mcp_servers_spawns_unchanged() {
    let directory = tempfile::tempdir().unwrap();
    let (session, _events) = open(spec(directory.path(), rpc(vec![], None)), request()).await;
    let argv: Vec<String> =
        serde_json::from_str(&std::fs::read_to_string(directory.path().join("argv.json")).unwrap())
            .unwrap();
    assert!(
        argv.is_empty(),
        "no MCP declaration means no extra argv: {argv:?}"
    );
    session.close().await.expect("close");
}

#[tokio::test]
async fn environment_entries_command_envs_would_panic_on_are_refused_before_spawn() {
    for (label, env) in [
        (
            "array name with `=`",
            json!([{"name": "BAD=NAME", "value": "x"}]),
        ),
        (
            "array name with NUL",
            json!([{"name": "BAD\u{0}NAME", "value": "x"}]),
        ),
        (
            "array value with NUL",
            json!([{"name": "TOKEN", "value": "bad\u{0}value"}]),
        ),
        ("object name with `=`", json!({"BAD=NAME": "x"})),
        ("object name with NUL", json!({"BAD\u{0}NAME": "x"})),
        ("object value with NUL", json!({"TOKEN": "bad\u{0}value"})),
        ("object empty name", json!({"": "x"})),
    ] {
        let directory = tempfile::tempdir().unwrap();
        let mut broken = server();
        broken["env"] = env;
        let (tx, _rx) = mpsc::unbounded_channel();
        let error = match Adapter::new(spec(directory.path(), rpc(vec![broken], Some(config()))))
            .open_session(request(), tx)
            .await
        {
            Ok(_) => panic!("{label} must be refused, not panic in envs()"),
            Err(error) => error,
        };
        assert_eq!(error.code, "agent_mcp_server_invalid", "{label}: {error}");
        assert!(
            !directory.path().join("argv.json").exists(),
            "{label}: admission must refuse before the agent is spawned"
        );
    }
}
