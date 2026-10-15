//! End-to-end lane tests against `a4aa fixture-agent` — a deterministic ACP
//! agent shipped in this crate's binary. No network, no real agents.

use a4aa::rpc::JsonRpcPeer;
use a4aa::{
    Adapter, AgentEvent, AgentSpec, CliLaneSpec, CommandSpec, EnvPolicy, EnvValue, LaneSpec,
    PermissionDecision, PermissionHandler, PermissionPrompt, PromptChannel, RpcCallSpec,
    RpcLaneSpec, SelectorPhase, SelectorSupport, SessionConfig, SessionRequest, StreamMode,
    TurnRequest,
};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicUsize, Ordering};
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
            initialize: None,
            initialized: None,
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
            session_resume: None,
            completion: None,
            session_close: None,
            text_event_method: Some("session/update".into()),
            text_path: None,
            result_text_path: Some("stopReason".into()),
            pre_turn: vec![],
            session_scope: Default::default(),
            kill_on_cancel: false,
            envelope: None,
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

struct BeforeTurnCount(Arc<AtomicUsize>);

#[async_trait::async_trait]
impl a4aa::BeforeTurn for BeforeTurnCount {
    async fn run(&self) -> a4aa::Result<()> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
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
        lane: LaneSpec::Cli(Box::new(CliLaneSpec {
            args,
            prompt_channel: channel,
            stream,
            resume_args: vec![],
            stdin_template: None,
            json_lines: false,
            session_id_path: None,
            usage_path: None,
            open_args: vec![],
            binding_file_env: None,
            binding_receipt_shape: Default::default(),
            result_selector: None,
            result_selector_field: None,
            result_path: None,
            execution_modes: vec![],
            permission_modes: vec![],
        })),
        command: CommandSpec {
            program: program.into(),
            args: vec![],
            cwd: None,
            env: EnvPolicy::default(),
        },
        max_output_bytes: cap,
    }
}

#[test]
fn rpc_selector_support_distinguishes_session_defaults_from_turn_overrides() {
    let mut spec = rpc_spec();
    let LaneSpec::Rpc(rpc) = &mut spec.lane else {
        unreachable!()
    };
    rpc.initialize = Some(RpcCallSpec {
        method: "initialize".into(),
        params: json!({"model": "{model}"}),
    });
    rpc.prompt.params = json!({
        "prompt": "{prompt}",
        "effort": "{effort}",
    });

    let session = spec.selector_support(SelectorPhase::Session);
    assert_eq!(session.model, SelectorSupport::Applied);
    assert_eq!(session.effort, SelectorSupport::Applied);
    let turn = spec.selector_support(SelectorPhase::Turn);
    assert_eq!(turn.model, SelectorSupport::Unsupported);
    assert_eq!(turn.effort, SelectorSupport::Applied);
}

#[test]
fn cli_selector_support_counts_only_reachable_stdin_templates() {
    let mut argv_prompt = cli_spec(
        "cat",
        vec!["{prompt}".into()],
        PromptChannel::ArgOnly,
        StreamMode::Stdio,
        None,
    );
    let LaneSpec::Cli(cli) = &mut argv_prompt.lane else {
        unreachable!()
    };
    cli.stdin_template = Some(json!({"model": "{model}", "effort": "{effort}"}));
    let support = argv_prompt.selector_support(SelectorPhase::Turn);
    assert_eq!(support.model, SelectorSupport::Unsupported);
    assert_eq!(support.effort, SelectorSupport::Unsupported);

    let mut stdin_prompt = cli_spec("cat", vec![], PromptChannel::Stdin, StreamMode::Stdio, None);
    let LaneSpec::Cli(cli) = &mut stdin_prompt.lane else {
        unreachable!()
    };
    cli.stdin_template = Some(json!({"model": "{model}", "effort": "{effort}"}));
    let support = stdin_prompt.selector_support(SelectorPhase::Turn);
    assert_eq!(support.model, SelectorSupport::Applied);
    assert_eq!(support.effort, SelectorSupport::Applied);
}

#[tokio::test]
async fn rpc_session_selectors_require_a_reachable_setup_call() {
    for (resuming, process_scope) in [(false, false), (true, false), (false, true)] {
        for selector in ["model", "effort", "mode", "permission"] {
            let mut spec = rpc_spec();
            // A valid preflight must reject before this program can spawn.
            spec.command.program = "a4aa-unreachable-setup-fixture".into();
            let LaneSpec::Rpc(rpc) = &mut spec.lane else {
                unreachable!()
            };
            let selected = RpcCallSpec {
                method: "session/new".into(),
                params: json!({(selector): format!("{{{selector}}}")}),
            };
            let unselected = RpcCallSpec {
                method: "session/resume".into(),
                params: json!({}),
            };
            if resuming || process_scope {
                rpc.session_open = Some(selected);
                rpc.session_resume = Some(unselected);
            } else {
                rpc.session_open = Some(unselected);
                rpc.session_resume = Some(selected);
            }
            if process_scope {
                rpc.session_scope = a4aa::SessionScope::Process;
            }
            let mut config = SessionConfig::default();
            match selector {
                "model" => config.model = Some("selected".into()),
                "effort" => config.effort = Some("selected".into()),
                "mode" => config.mode = Some("selected".into()),
                "permission" => config.permission = Some("selected".into()),
                _ => unreachable!(),
            }
            let (tx, _rx) = mpsc::unbounded_channel();
            let result = Adapter::new(spec)
                .open_session(
                    SessionRequest {
                        resume: resuming.then(|| "prior-session".into()),
                        config,
                        ..Default::default()
                    },
                    tx,
                )
                .await;
            let error = match result {
                Ok(_) => panic!("unused setup selectors must fail before spawn"),
                Err(error) => error,
            };
            assert_eq!(error.code, format!("agent_{selector}_unsupported"));
        }
    }
}

#[tokio::test]
async fn blank_selectors_fail_before_session_spawn_or_turn_reservation() {
    let spec = cli_spec(
        "a4aa-unreachable-selector-fixture",
        vec![
            "--model".into(),
            "{model}".into(),
            "--effort".into(),
            "{effort}".into(),
        ],
        PromptChannel::Stdin,
        StreamMode::Stdio,
        None,
    );
    let reservations = Arc::new(AtomicUsize::new(0));
    let adapter = Adapter::new(spec).before_turn(Arc::new(BeforeTurnCount(reservations.clone())));
    for selector in ["model", "effort"] {
        let config = SessionConfig {
            model: (selector == "model").then(|| "  ".into()),
            effort: (selector == "effort").then(|| "  ".into()),
            ..Default::default()
        };
        let (tx, _rx) = mpsc::unbounded_channel();
        let error = match adapter
            .open_session(
                SessionRequest {
                    config,
                    ..Default::default()
                },
                tx,
            )
            .await
        {
            Ok(_) => panic!("blank session selector must be rejected"),
            Err(error) => error,
        };
        assert_eq!(error.code, format!("agent_{selector}_unsupported"));
    }
    let (tx, _rx) = mpsc::unbounded_channel();
    let session = adapter
        .open_session(SessionRequest::default(), tx)
        .await
        .expect("CLI session");
    for (model, effort, code) in [
        (Some(" ".into()), None, "agent_model_unsupported"),
        (None, Some(" ".into()), "agent_effort_unsupported"),
    ] {
        let error = session
            .prompt(TurnRequest {
                text: "task".into(),
                model,
                effort,
            })
            .await
            .expect_err("blank turn selector must be rejected");
        assert_eq!(error.code, code);
    }
    assert_eq!(reservations.load(Ordering::SeqCst), 0);
    session.close().await.expect("close");
}

#[tokio::test]
async fn generic_rpc_turn_selectors_are_applied_by_prompt_templates() {
    let script = r#"
mode=""
permission=""
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
  case "$line" in
    *'"method":"initialize"'*)
      mode=$(printf '%s' "$line" | sed -n 's/.*"mode":"\([^"]*\)".*/\1/p')
      permission=$(printf '%s' "$line" | sed -n 's/.*"permission":"\([^"]*\)".*/\1/p')
      printf '{"jsonrpc":"2.0","id":%s,"result":{}}\n' "$id" ;;
    *'"method":"session/new"'*)
      printf '{"jsonrpc":"2.0","id":%s,"result":{"sessionId":"generic-rpc"}}\n' "$id" ;;
    *'"method":"session/prompt"'*)
      model=$(printf '%s' "$line" | sed -n 's/.*"model":"\([^"]*\)".*/\1/p')
      effort=$(printf '%s' "$line" | sed -n 's/.*"effort":"\([^"]*\)".*/\1/p')
      printf '{"jsonrpc":"2.0","id":%s,"result":{"text":"model=%s;effort=%s;mode=%s;permission=%s"}}\n' "$id" "$model" "$effort" "$mode" "$permission" ;;
  esac
done
"#;
    let mut spec = rpc_spec();
    spec.id = "custom.rpc".into();
    let LaneSpec::Rpc(rpc) = &mut spec.lane else {
        unreachable!()
    };
    rpc.initialize = Some(RpcCallSpec {
        method: "initialize".into(),
        params: json!({
            "mode": "{mode}",
            "permission": "{permission}",
        }),
    });
    rpc.initialized = Some("initialized".into());
    rpc.prompt.params = json!({
        "sessionId": "{sessionId}",
        "prompt": "{prompt}",
        "model": "{model}",
        "effort": "{effort}",
    });
    rpc.result_text_path = Some("text".into());
    spec.command = CommandSpec {
        program: "sh".into(),
        args: vec!["-c".into(), script.into()],
        cwd: None,
        env: EnvPolicy::default(),
    };

    let support = spec.selector_support(SelectorPhase::Turn);
    assert_eq!(support.model, SelectorSupport::Applied);
    assert_eq!(support.effort, SelectorSupport::Applied);
    let session_support = spec.selector_support(SelectorPhase::Session);
    assert_eq!(session_support.model, SelectorSupport::Applied);
    assert_eq!(session_support.effort, SelectorSupport::Applied);
    assert_eq!(session_support.mode, SelectorSupport::Applied);
    assert_eq!(session_support.permission, SelectorSupport::Applied);

    let (tx, _rx) = mpsc::unbounded_channel();
    let session = Adapter::new(spec)
        .open_session(
            SessionRequest {
                config: SessionConfig {
                    model: Some("profile-model".into()),
                    effort: Some("medium".into()),
                    mode: Some("plan".into()),
                    permission: Some("review".into()),
                    ..Default::default()
                },
                ..SessionRequest::default()
            },
            tx,
        )
        .await
        .expect("open generic RPC session");
    let outcome = session
        .prompt(turn("hello"))
        .await
        .expect("session model and effort fall back through turn template");
    assert_eq!(
        outcome.text,
        "model=profile-model;effort=medium;mode=plan;permission=review"
    );
    let override_outcome = session
        .prompt(TurnRequest {
            text: "override".into(),
            model: Some("model-x".into()),
            effort: Some("high".into()),
        })
        .await
        .expect("explicit turn values override session defaults");
    assert_eq!(
        override_outcome.text,
        "model=model-x;effort=high;mode=plan;permission=review"
    );
    session.close().await.expect("close");
}

#[tokio::test]
async fn generic_rpc_session_defaults_apply_open_only_and_reject_turn_override() {
    let script = r#"
model=""
effort=""
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
  case "$line" in
    *'"method":"session/new"'*)
      model=$(printf '%s' "$line" | sed -n 's/.*"model":"\([^"]*\)".*/\1/p')
      effort=$(printf '%s' "$line" | sed -n 's/.*"effort":"\([^"]*\)".*/\1/p')
      printf '{"jsonrpc":"2.0","id":%s,"result":{"sessionId":"session-defaults"}}\n' "$id" ;;
    *'"method":"session/prompt"'*)
      printf '{"jsonrpc":"2.0","id":%s,"result":{"text":"model=%s;effort=%s"}}\n' "$id" "$model" "$effort" ;;
  esac
done
"#;
    let mut spec = rpc_spec();
    spec.id = "custom.rpc.session-defaults".into();
    let LaneSpec::Rpc(rpc) = &mut spec.lane else {
        unreachable!()
    };
    rpc.session_open = Some(RpcCallSpec {
        method: "session/new".into(),
        params: json!({
            "cwd": "{cwd}",
            "model": "{model}",
            "effort": "{effort}",
        }),
    });
    rpc.prompt.params = json!({
        "sessionId": "{sessionId}",
        "prompt": "{prompt}",
    });
    rpc.result_text_path = Some("text".into());
    spec.command = CommandSpec {
        program: "sh".into(),
        args: vec!["-c".into(), script.into()],
        cwd: None,
        env: EnvPolicy::default(),
    };
    let support = spec.selector_support(SelectorPhase::Session);
    assert_eq!(support.model, SelectorSupport::Applied);
    assert_eq!(support.effort, SelectorSupport::Applied);
    let support = spec.selector_support(SelectorPhase::Turn);
    assert_eq!(support.model, SelectorSupport::Unsupported);
    assert_eq!(support.effort, SelectorSupport::Unsupported);

    let before_turn_count = Arc::new(AtomicUsize::new(0));
    let adapter =
        Adapter::new(spec).before_turn(Arc::new(BeforeTurnCount(before_turn_count.clone())));
    let (tx, _rx) = mpsc::unbounded_channel();
    let session = adapter
        .open_session(
            SessionRequest {
                config: SessionConfig {
                    model: Some("profile-model".into()),
                    effort: Some("medium".into()),
                    ..Default::default()
                },
                ..SessionRequest::default()
            },
            tx,
        )
        .await
        .expect("open session with setup selectors");
    let outcome = session
        .prompt(turn("default turn"))
        .await
        .expect("session defaults applied by session_open");
    assert_eq!(outcome.text, "model=profile-model;effort=medium");
    assert_eq!(before_turn_count.load(Ordering::SeqCst), 1);

    let error = session
        .prompt(TurnRequest {
            text: "override".into(),
            model: Some("operation-model".into()),
            effort: None,
        })
        .await
        .expect_err("an unconsumed operation override must fail closed");
    assert_eq!(error.code, "agent_model_unsupported");
    assert_eq!(before_turn_count.load(Ordering::SeqCst), 1);
    session.close().await.expect("close");
}

#[tokio::test]
async fn generic_cli_selectors_follow_templates_and_fail_before_before_turn() {
    let script = r#"printf '%s %s' "$0" "$*""#;
    let mut spec = cli_spec(
        "sh",
        vec![
            "--model".into(),
            "{model}".into(),
            "--effort".into(),
            "{effort}".into(),
            "{prompt}".into(),
        ],
        PromptChannel::ArgOnly,
        StreamMode::Stdio,
        None,
    );
    spec.id = "custom.agent".into();
    spec.command.args = vec!["-c".into(), script.into()];
    let support = spec.selector_support(SelectorPhase::Turn);
    assert_eq!(support.model, SelectorSupport::Applied);
    assert_eq!(support.effort, SelectorSupport::Applied);
    let session_support = spec.selector_support(SelectorPhase::Session);
    assert_eq!(session_support.model, SelectorSupport::Applied);
    assert_eq!(session_support.effort, SelectorSupport::Applied);

    let before_turn_count = Arc::new(AtomicUsize::new(0));
    let adapter =
        Adapter::new(spec).before_turn(Arc::new(BeforeTurnCount(before_turn_count.clone())));
    let (tx, _rx) = mpsc::unbounded_channel();
    let session = adapter
        .open_session(
            SessionRequest {
                config: SessionConfig {
                    model: Some("profile-model".into()),
                    effort: Some("medium".into()),
                    ..Default::default()
                },
                ..SessionRequest::default()
            },
            tx,
        )
        .await
        .expect("open generic CLI session");
    let outcome = session
        .prompt(turn("hello"))
        .await
        .expect("session selectors fall back through turn template");
    assert!(outcome
        .text
        .contains("--model profile-model --effort medium hello"));
    let override_outcome = session
        .prompt(TurnRequest {
            text: "override".into(),
            model: Some("model-x".into()),
            effort: Some("high".into()),
        })
        .await
        .expect("explicit per-turn selectors override defaults");
    assert!(override_outcome
        .text
        .contains("--model model-x --effort high override"));
    assert_eq!(before_turn_count.load(Ordering::SeqCst), 2);
    session.close().await.expect("close");

    let mut spec = cli_spec(
        "sh",
        vec!["{prompt}".into()],
        PromptChannel::ArgOnly,
        StreamMode::Stdio,
        None,
    );
    spec.id = "custom.agent".into();
    spec.command.args = vec!["-c".into(), "printf called".into()];
    let before_turn_count = Arc::new(AtomicUsize::new(0));
    let adapter =
        Adapter::new(spec).before_turn(Arc::new(BeforeTurnCount(before_turn_count.clone())));
    let (tx, _rx) = mpsc::unbounded_channel();
    let session = adapter
        .open_session(SessionRequest::default(), tx)
        .await
        .expect("open generic CLI session");
    let model_error = session
        .prompt(TurnRequest {
            text: "hello".into(),
            model: Some("model-x".into()),
            effort: None,
        })
        .await
        .expect_err("unconsumed model must fail closed");
    assert_eq!(model_error.code, "agent_model_unsupported");
    let effort_error = session
        .prompt(TurnRequest {
            text: "hello".into(),
            model: None,
            effort: Some("high".into()),
        })
        .await
        .expect_err("unconsumed effort must fail closed");
    assert_eq!(effort_error.code, "agent_effort_unsupported");
    assert_eq!(before_turn_count.load(Ordering::SeqCst), 0);
    session.close().await.expect("close");
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

// --------------------- ACP session configuration ---------------------

fn fixture_env(vars: &[(&str, &str)]) -> EnvPolicy {
    EnvPolicy::Sanitized {
        inherit: vec![],
        set: vars
            .iter()
            .map(|(key, value)| (key.to_string(), EnvValue::Literal((*value).to_owned())))
            .collect(),
    }
}

fn acp_spec_with_env(vars: &[(&str, &str)]) -> AgentSpec {
    let mut spec = acp_spec();
    spec.command.env = fixture_env(vars);
    spec
}

fn options(
    model: Option<&str>,
    mode: Option<&str>,
    effort: Option<&str>,
    permission: Option<&str>,
) -> SessionConfig {
    SessionConfig {
        model: model.map(str::to_owned),
        mode: mode.map(str::to_owned),
        effort: effort.map(str::to_owned),
        permission: permission.map(str::to_owned),
        instructions: None,
        provider: None,
    }
}

async fn config_of(
    session: &a4aa::AgentSession,
    rx: &mut mpsc::UnboundedReceiver<AgentEvent>,
) -> String {
    session
        .prompt(turn("config-check"))
        .await
        .expect("config-check prompt");
    tokio::time::sleep(Duration::from_millis(20)).await;
    collect_text(rx)
}

#[tokio::test]
async fn acp_session_options_apply_via_config_options() {
    let adapter = Adapter::new(acp_spec());
    let (tx, mut rx) = mpsc::unbounded_channel();
    let request = SessionRequest {
        config: options(
            Some("fixture-large"),
            Some("ask"),
            Some("high"),
            Some("open"),
        ),
        ..SessionRequest::default()
    };
    let session = adapter.open_session(request, tx).await.expect("open");
    assert_eq!(
        config_of(&session, &mut rx).await,
        "echo: config:model=fixture-large,mode=ask,effort=high,perm=open"
    );
    session.close().await.expect("close");
}

#[tokio::test]
async fn acp_session_options_unadvertised_fail_closed() {
    let adapter = Adapter::new(acp_spec());
    let (tx, _rx) = mpsc::unbounded_channel();
    let request = SessionRequest {
        config: options(Some("no-such-model"), None, None, None),
        ..SessionRequest::default()
    };
    let error = match adapter.open_session(request, tx).await {
        Ok(_) => panic!("unadvertised model must fail"),
        Err(error) => error,
    };
    assert_eq!(error.code, "agent_configuration_unavailable");
}

#[tokio::test]
async fn acp_restored_unknown_options_reject_explicit_overrides() {
    for config in [
        options(Some("fixture-large"), None, None, None),
        options(None, Some("ask"), None, None),
        options(None, None, Some("high"), None),
        options(None, None, None, Some("safe")),
    ] {
        let adapter = Adapter::new(acp_spec_with_env(&[("A4AA_FIXTURE_RESTORE_UNKNOWN", "1")]));
        let (tx, _rx) = mpsc::unbounded_channel();
        let request = SessionRequest {
            resume: Some("saved-session".into()),
            config,
            ..Default::default()
        };
        let error = match adapter.open_session(request, tx).await {
            Ok(_) => panic!("an unconfirmed override must not open a restored session"),
            Err(error) => error,
        };
        assert_eq!(error.code, "agent_configuration_unavailable");
    }
    // Without an override the persisted configuration remains the agent's.
    let adapter = Adapter::new(acp_spec_with_env(&[("A4AA_FIXTURE_RESTORE_UNKNOWN", "1")]));
    let (tx, _rx) = mpsc::unbounded_channel();
    let session = adapter
        .open_session(
            SessionRequest {
                resume: Some("saved-session".into()),
                ..Default::default()
            },
            tx,
        )
        .await
        .unwrap();
    session.close().await.unwrap();
}

#[tokio::test]
async fn acp_restored_permission_override_is_applied_and_confirmed() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let session = Adapter::new(acp_spec())
        .open_session(
            SessionRequest {
                resume: Some("saved-session".into()),
                config: options(None, None, None, Some("open")),
                ..Default::default()
            },
            tx,
        )
        .await
        .unwrap();
    assert!(config_of(&session, &mut rx).await.ends_with("perm=open"));
    session.close().await.unwrap();
}

#[tokio::test]
async fn acp_session_options_fall_back_to_legacy_surfaces() {
    // A4AA_FIXTURE_LEGACY drops the model/mode configOptions — the legacy
    // `models`/`modes` surfaces + `session/set_model`/`session/set_mode`
    // carry those slots instead.
    let adapter = Adapter::new(acp_spec_with_env(&[("A4AA_FIXTURE_LEGACY", "1")]));
    let (tx, mut rx) = mpsc::unbounded_channel();
    let request = SessionRequest {
        config: options(
            Some("fixture-large"),
            Some("ask"),
            Some("high"),
            Some("open"),
        ),
        ..SessionRequest::default()
    };
    let session = adapter.open_session(request, tx).await.expect("open");
    assert_eq!(
        config_of(&session, &mut rx).await,
        "echo: config:model=fixture-large,mode=ask,effort=high,perm=open"
    );
    session.close().await.expect("close");
}

#[tokio::test]
async fn acp_authenticate_retries_session_establishment() {
    // A4AA_FIXTURE_AUTH advertises `fixture-auth` and rejects session/new
    // until `authenticate` succeeds — the lane must try the advertised
    // methods and retry once.
    let adapter = Adapter::new(acp_spec_with_env(&[("A4AA_FIXTURE_AUTH", "1")]));
    let (tx, _rx) = mpsc::unbounded_channel();
    let session = adapter
        .open_session(SessionRequest::default(), tx)
        .await
        .expect("open after authenticate");
    let outcome = session.prompt(turn("hi")).await.expect("prompt");
    assert_eq!(outcome.stop_reason, "end_turn");
    session.close().await.expect("close");
}

#[tokio::test]
async fn acp_turn_level_model_applies_before_prompt() {
    let adapter = Adapter::new(acp_spec());
    let (tx, mut rx) = mpsc::unbounded_channel();
    let session = adapter
        .open_session(SessionRequest::default(), tx)
        .await
        .expect("open");
    let outcome = session
        .prompt(TurnRequest {
            text: "config-check".into(),
            model: Some("fixture-large".into()),
            effort: Some("high".into()),
        })
        .await
        .expect("prompt");
    assert_eq!(outcome.stop_reason, "end_turn");
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert_eq!(
        collect_text(&mut rx),
        "echo: config:model=fixture-large,mode=code,effort=high,perm=safe"
    );
    session.close().await.expect("close");
}

#[tokio::test]
async fn acp_negotiated_selector_validation_precedes_before_turn() {
    let before_turn_count = Arc::new(AtomicUsize::new(0));
    let adapter =
        Adapter::new(acp_spec()).before_turn(Arc::new(BeforeTurnCount(before_turn_count.clone())));
    let (tx, _rx) = mpsc::unbounded_channel();
    let session = adapter
        .open_session(SessionRequest::default(), tx)
        .await
        .expect("open ACP session");

    let session_support = session.selector_support(SelectorPhase::Session);
    assert_eq!(session_support.model, SelectorSupport::Applied);
    assert_eq!(session_support.effort, SelectorSupport::Applied);
    assert_eq!(session_support.mode, SelectorSupport::Applied);
    assert_eq!(session_support.permission, SelectorSupport::Applied);
    let turn_support = session.selector_support(SelectorPhase::Turn);
    assert_eq!(turn_support.model, SelectorSupport::Applied);
    assert_eq!(turn_support.effort, SelectorSupport::Applied);
    assert_eq!(turn_support.mode, SelectorSupport::Unsupported);
    assert_eq!(turn_support.permission, SelectorSupport::Unsupported);

    let error = session
        .prompt(TurnRequest {
            text: "must not be sent".into(),
            model: Some("not-advertised".into()),
            effort: None,
        })
        .await
        .expect_err("unadvertised ACP model must fail in preflight");
    assert_eq!(error.code, "agent_configuration_unavailable");
    assert_eq!(before_turn_count.load(Ordering::SeqCst), 0);
    session.close().await.expect("close");
}

#[tokio::test]
async fn acp_agent_info_and_session_state_are_exposed() {
    let adapter = Adapter::new(acp_spec());
    let (tx, _rx) = mpsc::unbounded_channel();
    let session = adapter
        .open_session(SessionRequest::default(), tx)
        .await
        .expect("open");
    let a4aa::LaneSession::Acp(acp) = session.lane() else {
        panic!("expected acp lane");
    };
    let info = acp.agent_info().expect("agentInfo");
    assert_eq!(info.name, "a4aa-fixture");
    let state = acp.session_state();
    assert!(
        state["configOptions"].is_array(),
        "sessionState must carry configOptions: {state}"
    );
    assert_eq!(
        state["modes"]["currentModeId"], "code",
        "sessionState must carry the modes surface: {state}"
    );
    session.close().await.expect("close");
}

#[tokio::test]
async fn sidecar_session_options_and_structured_errors() {
    let mut child = tokio::process::Command::new(FIXTURE)
        .arg("serve")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .expect("spawn a4aa serve");
    let (notice_tx, _notice_rx) = mpsc::unbounded_channel();
    let delegate = Arc::new(SidecarEvents {
        notifications: notice_tx,
        permission_granted: std::sync::atomic::AtomicBool::new(false),
    });
    let peer = JsonRpcPeer::spawn(
        child.stdout.take().unwrap(),
        child.stdin.take().unwrap(),
        delegate,
        a4aa::observe::none(),
        a4aa::framing::DEFAULT_MAX_MESSAGE_BYTES,
    );

    // Open with session options: applied through the fixture's surfaces.
    let opened = peer
        .call(
            "session/open",
            json!({
                "spec": serde_json::to_value(acp_spec()).unwrap(),
                "options": {"model": "fixture-large", "effort": "high"}
            }),
        )
        .await
        .expect("session/open with options");
    assert_eq!(opened["agentInfo"]["name"], "a4aa-fixture");
    let options = opened["sessionState"]["configOptions"]
        .as_array()
        .expect("configOptions surface");
    let model_option = options
        .iter()
        .find(|o| o["category"] == "model")
        .expect("model option");
    assert_eq!(model_option["currentValue"], "fixture-large");

    // An unadvertised option fails with a typed data.code.
    let failure = match peer
        .call(
            "session/open",
            json!({
                "spec": serde_json::to_value(acp_spec()).unwrap(),
                "options": {"model": "no-such-model"}
            }),
        )
        .await
    {
        Ok(_) => panic!("unadvertised model must error"),
        Err(failure) => failure,
    };
    let a4aa::rpc::CallFailure::Remote(error) = failure else {
        panic!("expected a remote error object, got {failure:?}");
    };
    assert_eq!(
        error
            .data
            .as_ref()
            .and_then(|d| d.get("code"))
            .and_then(Value::as_str),
        Some("agent_configuration_unavailable")
    );
    assert_eq!(
        error
            .data
            .as_ref()
            .and_then(|d| d.get("stage"))
            .and_then(Value::as_str),
        Some("session/configuration")
    );

    peer.close().await;
    let _ = child.kill().await;
}
