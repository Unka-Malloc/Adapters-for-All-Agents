//! End-to-end lane tests against `a4aa fixture-agent` — a deterministic ACP
//! agent shipped in this crate's binary. No network, no real agents.

use a4aa::rpc::JsonRpcPeer;
use a4aa::{
    Adapter, AgentEvent, AgentSpec, CliLaneSpec, CommandSpec, EnvPolicy, LaneSpec,
    PermissionDecision, PermissionHandler, PermissionPrompt, PromptChannel, RpcCallSpec,
    RpcLaneSpec, SessionRequest, StreamMode, TurnRequest,
};
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;

const FIXTURE: &str = env!("CARGO_BIN_EXE_a4aa");

fn fixture_command() -> CommandSpec {
    CommandSpec {
        program: FIXTURE.into(),
        args: vec!["fixture-agent".into()],
        cwd: None,
        env: EnvPolicy::default(),
    }
}

fn acp_spec() -> AgentSpec {
    AgentSpec {
        id: "fixture-acp".into(),
        client_name: Some("a4aa-test".into()),
        lane: LaneSpec::Acp {
            mcp_servers: vec![],
            prefer_load: false,
        },
        command: fixture_command(),
        max_output_bytes: None,
    }
}

fn rpc_spec() -> AgentSpec {
    AgentSpec {
        id: "fixture-rpc".into(),
        client_name: None,
        lane: LaneSpec::Rpc(Box::new(RpcLaneSpec {
            session_open: Some(RpcCallSpec {
                method: "session/new".into(),
                params: json!({"cwd": "{cwd}", "mcpServers": []}),
            }),
            prompt: RpcCallSpec {
                method: "session/prompt".into(),
                params: json!({
                    "sessionId": "{sessionId}",
                    "prompt": [{"type": "text", "text": "{prompt}"}],
                }),
            },
            cancel: Some(RpcCallSpec {
                method: "session/cancel".into(),
                params: json!({"sessionId": "{sessionId}"}),
            }),
            session_close: None,
            text_event_method: Some("session/update".into()),
            result_text_path: Some("stopReason".into()),
            passthrough: vec![],
        })),
        command: fixture_command(),
        max_output_bytes: None,
    }
}

fn turn(text: &str) -> TurnRequest {
    TurnRequest {
        text: text.to_owned(),
        model: None,
        effort: None,
    }
}

fn collect_text(rx: &mut mpsc::UnboundedReceiver<AgentEvent>) -> String {
    let mut text = String::new();
    while let Ok(event) = rx.try_recv() {
        if let AgentEvent::MessageChunk { text: chunk } = event {
            text.push_str(&chunk);
        }
    }
    text
}

// ------------------------------- ACP --------------------------------

#[tokio::test]
async fn acp_open_prompt_stream_close() {
    let adapter = Adapter::new(acp_spec());
    let (tx, mut rx) = mpsc::unbounded_channel();
    let session = adapter
        .open_session(SessionRequest::default(), tx)
        .await
        .expect("open");

    assert_eq!(session.session_id(), "fixture-session-1");
    let outcome = session.prompt(turn("hello")).await.expect("prompt");
    assert_eq!(outcome.stop_reason, "end_turn");
    // Give the forwarder a beat to flush trailing notifications.
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert_eq!(collect_text(&mut rx), "echo: hello");

    session.close().await.expect("close");
}

#[tokio::test]
async fn acp_resume_uses_load_or_resume() {
    let adapter = Adapter::new(acp_spec());
    let (tx, _rx) = mpsc::unbounded_channel();
    let request = SessionRequest {
        resume: Some("earlier-session".into()),
        ..SessionRequest::default()
    };
    let session = adapter
        .open_session(request, tx)
        .await
        .expect("resume open");
    assert_eq!(session.session_id(), "earlier-session");
    session.close().await.expect("close");
}

#[tokio::test]
async fn acp_permission_grant_flows_through_handler() {
    struct Allow;
    #[async_trait::async_trait]
    impl PermissionHandler for Allow {
        async fn decide(&self, prompt: PermissionPrompt) -> PermissionDecision {
            assert_eq!(prompt.tool_call_id, "fixture-call-1");
            assert!(prompt.options.iter().any(|(id, _, _)| id == "allow"));
            PermissionDecision::Selected("allow".into())
        }
    }
    let adapter = Adapter::new(acp_spec()).permission(Arc::new(Allow));
    let (tx, mut rx) = mpsc::unbounded_channel();
    let session = adapter
        .open_session(SessionRequest::default(), tx)
        .await
        .expect("open");
    let outcome = session
        .prompt(turn("needs permission"))
        .await
        .expect("prompt");
    assert_eq!(outcome.stop_reason, "end_turn");
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert_eq!(collect_text(&mut rx), "granted: needs permission");
    session.close().await.expect("close");
}

#[tokio::test]
async fn acp_permission_defaults_to_cancelled() {
    // No handler: the lane answers `cancelled`, the fixture ends in refusal.
    let adapter = Adapter::new(acp_spec());
    let (tx, _rx) = mpsc::unbounded_channel();
    let session = adapter
        .open_session(SessionRequest::default(), tx)
        .await
        .expect("open");
    let outcome = session
        .prompt(turn("needs permission"))
        .await
        .expect("prompt");
    assert_eq!(outcome.stop_reason, "refusal");
    session.close().await.expect("close");
}

#[tokio::test]
async fn acp_cancel_terminates_deferred_turn() {
    let adapter = Adapter::new(acp_spec());
    let (tx, mut rx) = mpsc::unbounded_channel();
    let session = adapter
        .open_session(SessionRequest::default(), tx)
        .await
        .expect("open");

    let outcome = {
        let prompt = session.prompt(turn("wait-for-cancel"));
        tokio::pin!(prompt);
        // Poll the prompt until the request is in flight and chunks stream.
        tokio::select! {
            outcome = &mut prompt => panic!("prompt finished before cancel: {outcome:?}"),
            () = async {
                let deadline = std::time::Instant::now() + Duration::from_secs(5);
                while std::time::Instant::now() < deadline {
                    if matches!(rx.try_recv(), Ok(AgentEvent::MessageChunk { .. })) {
                        return;
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
                panic!("no stream chunks arrived");
            } => {}
        }
        session.cancel().await.expect("cancel");
        tokio::time::timeout(Duration::from_secs(5), &mut prompt)
            .await
            .expect("prompt timed out after cancel")
            .expect("prompt result")
    };
    assert_eq!(outcome.stop_reason, "cancelled");
    session.close().await.expect("close");
}

#[tokio::test]
async fn acp_open_failure_is_reported() {
    let mut spec = acp_spec();
    spec.command.program = "a4aa-no-such-binary-9f2c".into();
    let adapter = Adapter::new(spec);
    let (tx, _rx) = mpsc::unbounded_channel();
    let result = adapter.open_session(SessionRequest::default(), tx).await;
    assert!(result.is_err());
}

// ------------------------------- RPC --------------------------------

#[tokio::test]
async fn rpc_open_prompt_close() {
    let adapter = Adapter::new(rpc_spec());
    let (tx, mut rx) = mpsc::unbounded_channel();
    let session = adapter
        .open_session(SessionRequest::default(), tx)
        .await
        .expect("open");
    assert_eq!(session.session_id(), "fixture-session-1");

    let outcome = session.prompt(turn("hello")).await.expect("prompt");
    assert_eq!(outcome.stop_reason, "end_turn");
    assert_eq!(outcome.text, "end_turn"); // result_text_path extracted
    tokio::time::sleep(Duration::from_millis(20)).await;
    // session/update notifications arrive as Raw events (params shape isn't
    // the lane's flat text shape) — they must flow, not drop.
    let mut raw_seen = false;
    while let Ok(event) = rx.try_recv() {
        raw_seen |= matches!(event, AgentEvent::Raw { .. });
    }
    assert!(raw_seen, "no session/update events arrived");
    session.close().await.expect("close");
}

#[tokio::test]
async fn rpc_cancel_reaches_agent() {
    let adapter = Adapter::new(rpc_spec());
    let (tx, _rx) = mpsc::unbounded_channel();
    let session = adapter
        .open_session(SessionRequest::default(), tx)
        .await
        .expect("open");

    let outcome = {
        let prompt = session.prompt(turn("wait-for-cancel"));
        tokio::pin!(prompt);
        // Drive the request onto the wire before cancelling.
        tokio::select! {
            outcome = &mut prompt => panic!("prompt finished before cancel: {outcome:?}"),
            () = tokio::time::sleep(Duration::from_millis(300)) => {}
        }
        session.cancel().await.expect("cancel");
        tokio::time::timeout(Duration::from_secs(5), &mut prompt)
            .await
            .expect("prompt timed out after cancel")
            .expect("prompt result")
    };
    assert_eq!(outcome.stop_reason, "cancelled");
    session.close().await.expect("close");
}

// ------------------------------- CLI --------------------------------

fn cli_spec(
    program: &str,
    args: Vec<String>,
    channel: PromptChannel,
    stream: StreamMode,
    cap: Option<usize>,
) -> AgentSpec {
    AgentSpec {
        id: "fixture-cli".into(),
        client_name: None,
        lane: LaneSpec::Cli(CliLaneSpec {
            args,
            prompt_channel: channel,
            stream,
        }),
        command: CommandSpec {
            program: program.into(),
            args: vec![],
            cwd: None,
            env: EnvPolicy::default(),
        },
        max_output_bytes: cap,
    }
}

#[tokio::test]
async fn cli_stdin_prompt_streams_stdout() {
    let adapter = Adapter::new(cli_spec(
        "cat",
        vec![],
        PromptChannel::Stdin,
        StreamMode::Stdio,
        None,
    ));
    let (tx, mut rx) = mpsc::unbounded_channel();
    let session = adapter
        .open_session(SessionRequest::default(), tx)
        .await
        .expect("open");
    let outcome = session.prompt(turn("hello-stdio")).await.expect("prompt");
    assert_eq!(outcome.stop_reason, "end_turn");
    assert_eq!(outcome.exit_code, Some(0));
    assert_eq!(outcome.text.trim_end(), "hello-stdio");
    assert!(collect_text(&mut rx).contains("hello-stdio"));
}

#[tokio::test]
async fn cli_argv_placeholder_skips_stdin() {
    let adapter = Adapter::new(cli_spec(
        "echo",
        vec!["{prompt}".into()],
        PromptChannel::ArgOnly,
        StreamMode::Stdio,
        None,
    ));
    let (tx, _rx) = mpsc::unbounded_channel();
    let session = adapter
        .open_session(SessionRequest::default(), tx)
        .await
        .expect("open");
    let outcome = session.prompt(turn("argv-prompt")).await.expect("prompt");
    assert_eq!(outcome.text.trim(), "argv-prompt");
}

#[cfg(unix)]
#[tokio::test]
async fn cli_pty_stream_mode_works() {
    // PTY lanes serve line-interactive CLIs: the program reads one line from
    // the terminal, prints, and exits — no stdin-EOF contract.
    let adapter = Adapter::new(cli_spec(
        "sh",
        vec![
            "-c".into(),
            "read line; printf 'got:%s\\n' \"$line\"".into(),
        ],
        PromptChannel::Stdin,
        StreamMode::Pty,
        None,
    ));
    let (tx, _rx) = mpsc::unbounded_channel();
    let session = adapter
        .open_session(SessionRequest::default(), tx)
        .await
        .expect("open");
    let outcome = session.prompt(turn("hello-pty")).await.expect("prompt");
    assert_eq!(outcome.text.trim_end(), "got:hello-pty");
}

#[tokio::test]
async fn cli_output_cap_truncates() {
    let adapter = Adapter::new(cli_spec(
        "sh",
        vec!["-c".into(), "head -c 2000 /dev/zero | tr '\\0' 'x'".into()],
        PromptChannel::ArgOrStdin,
        StreamMode::Stdio,
        Some(64),
    ));
    let (tx, mut rx) = mpsc::unbounded_channel();
    let session = adapter
        .open_session(SessionRequest::default(), tx)
        .await
        .expect("open");
    let outcome = session.prompt(turn("")).await.expect("prompt");
    assert!(outcome.truncated);
    assert_eq!(outcome.text.len(), 64);
    let mut saw_truncated = false;
    while let Ok(event) = rx.try_recv() {
        saw_truncated |= matches!(event, AgentEvent::Truncated);
    }
    assert!(saw_truncated, "Truncated event missing");
}

#[tokio::test]
async fn cli_output_cap_clamps_to_char_boundary() {
    // 100×3-byte scalars: a byte-cap clamp landing mid-scalar must round
    // down to a char boundary, never panic, and emit Truncated once.
    let adapter = Adapter::new(cli_spec(
        "sh",
        vec!["-c".into(), "printf '中%.0s' $(seq 100)".into()],
        PromptChannel::ArgOrStdin,
        StreamMode::Stdio,
        Some(64),
    ));
    let (tx, mut rx) = mpsc::unbounded_channel();
    let session = adapter
        .open_session(SessionRequest::default(), tx)
        .await
        .expect("open");
    let outcome =
        tokio::time::timeout(std::time::Duration::from_secs(10), session.prompt(turn("")))
            .await
            .expect("prompt hung")
            .expect("prompt");
    assert!(outcome.truncated);
    assert_eq!(outcome.text.len(), 63); // 21 chars × 3 bytes, clamped
    assert_eq!(outcome.text.chars().count(), 21);
    let truncated_events = std::iter::from_fn(|| rx.try_recv().ok())
        .filter(|event| matches!(event, AgentEvent::Truncated))
        .count();
    assert_eq!(truncated_events, 1, "Truncated must fire exactly once");
}

// ----------------------------- sidecar -------------------------------

struct SidecarEvents {
    notifications: mpsc::UnboundedSender<Value>,
    permission_granted: std::sync::atomic::AtomicBool,
}

#[async_trait::async_trait]
impl a4aa::RpcDelegate for SidecarEvents {
    async fn on_notification(&self, method: &str, params: Value) {
        if method == "session/event" {
            let _ = self.notifications.send(params);
        }
    }
    async fn on_request(
        &self,
        method: &str,
        _params: Value,
    ) -> Result<Value, a4aa::RpcErrorObject> {
        if method == "permission/request" {
            self.permission_granted
                .store(true, std::sync::atomic::Ordering::Relaxed);
            return Ok(json!({"selected": "allow"}));
        }
        Err(a4aa::RpcErrorObject::method_not_found(method))
    }
}

#[tokio::test]
async fn sidecar_drives_fixture_end_to_end() {
    let mut child = tokio::process::Command::new(FIXTURE)
        .arg("serve")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .expect("spawn a4aa serve");
    let (notice_tx, mut notice_rx) = mpsc::unbounded_channel();
    let delegate = Arc::new(SidecarEvents {
        notifications: notice_tx,
        permission_granted: std::sync::atomic::AtomicBool::new(false),
    });
    let peer = JsonRpcPeer::spawn(
        child.stdout.take().unwrap(),
        child.stdin.take().unwrap(),
        delegate.clone(),
        a4aa::observe::none(),
        a4aa::framing::DEFAULT_MAX_MESSAGE_BYTES,
    );

    let pong = peer.call("ping", json!({})).await.expect("ping");
    assert_eq!(pong["protocol"], "a4aa.v1");

    let opened = peer
        .call(
            "session/open",
            json!({"spec": serde_json::to_value(acp_spec()).unwrap()}),
        )
        .await
        .expect("session/open");
    let session_id = opened["sessionId"].as_str().unwrap().to_owned();

    // Collect streamed session/event notifications while the prompt runs.
    async fn collect(rx: &mut mpsc::UnboundedReceiver<Value>, want: &str) -> String {
        let mut text = String::new();
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while std::time::Instant::now() < deadline && text != want {
            match tokio::time::timeout(Duration::from_secs(5), rx.recv()).await {
                Ok(Some(params)) => {
                    if let Some(chunk) = params["event"]["text"].as_str() {
                        text.push_str(chunk);
                    }
                }
                _ => break,
            }
        }
        text
    }

    // Prompt 1: plain echo.
    let outcome = peer
        .call(
            "session/prompt",
            json!({"sessionId": session_id, "text": "hi"}),
        )
        .await
        .expect("session/prompt");
    assert_eq!(outcome["stopReason"], "end_turn");
    assert_eq!(collect(&mut notice_rx, "echo: hi").await, "echo: hi");

    // Prompt 2: permission path — the agent's request is relayed to this
    // client, which answers `allow`; the fixture echoes "granted:".
    let outcome = peer
        .call(
            "session/prompt",
            json!({"sessionId": session_id, "text": "needs permission"}),
        )
        .await
        .expect("permission prompt");
    assert_eq!(outcome["stopReason"], "end_turn");
    assert!(delegate
        .permission_granted
        .load(std::sync::atomic::Ordering::Relaxed));
    assert_eq!(
        collect(&mut notice_rx, "granted: needs permission").await,
        "granted: needs permission"
    );

    peer.call("session/close", json!({"sessionId": session_id}))
        .await
        .expect("session/close");
    peer.close().await;
    let _ = child.kill().await;
}
