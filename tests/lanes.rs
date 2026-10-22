//! End-to-end lane tests against `a4aa fixture-agent` — a deterministic ACP
//! agent shipped in this crate's binary. No network, no real agents.

#[path = "support/settings.rs"]
mod test_settings;

use a4aa::rpc::JsonRpcPeer;
use a4aa::{
    Adapter, AgentEvent, AgentSpec, CliLaneSpec, CommandSpec, EnvPolicy, EnvValue, LaneSpec,
    PermissionDecision, PermissionHandler, PermissionPrompt, PromptChannel, RpcCallSpec,
    RpcLaneSpec, RpcTransport, SelectorPhase, SelectorSupport, SessionConfig, SessionRequest,
    StreamMode, TurnRequest,
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
            auth_key: None,
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
            transport: RpcTransport::Stdio,
            max_frame_bytes: None,
            usage_path: None,
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
    // Successful turns drain their admitted output before returning.
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
                let deadline = std::time::Instant::now() + test_settings::WAIT_BUDGET;
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
        tokio::time::timeout(test_settings::WAIT_BUDGET, &mut prompt)
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
        tokio::time::timeout(test_settings::WAIT_BUDGET, &mut prompt)
            .await
            .expect("prompt timed out after cancel")
            .expect("prompt result")
    };
    assert_eq!(outcome.stop_reason, "cancelled");
    session.close().await.expect("close");
}

#[tokio::test]
async fn rpc_usage_arrives_on_the_outcome_even_when_unstreamed() {
    let adapter = Adapter::new(rpc_spec());
    let (tx, _rx) = mpsc::unbounded_channel();
    let session = adapter
        .open_session(SessionRequest::default(), tx)
        .await
        .expect("open");
    let outcome = session.prompt(turn("usage-report")).await.expect("prompt");
    let usage = outcome.usage.expect("usage captured at ingress");
    assert_eq!(usage.input, Some(7));
    assert_eq!(usage.output, Some(3));
    assert_eq!(usage.cached_input, Some(5));
    assert_eq!(usage.total, Some(10));
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
            resume_spawn_args: vec![],
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
            effort_model_suffixes: vec![],
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

#[tokio::test]
async fn cli_effort_folds_into_the_model_suffix() {
    let mut spec = cli_spec(
        "echo",
        vec!["{prompt}".into(), "--model".into(), "{model}".into()],
        PromptChannel::ArgOnly,
        StreamMode::Stdio,
        None,
    );
    let LaneSpec::Cli(cli) = &mut spec.lane else {
        unreachable!()
    };
    cli.effort_model_suffixes = [
        "minimal",
        "low",
        "medium",
        "high",
        "extra-high",
        "xhigh",
        "max",
    ]
    .iter()
    .map(|level| (*level).to_owned())
    .collect();
    // The effort selector is applied through `{model}` — no placeholder needed.
    assert_eq!(
        spec.selector_support(SelectorPhase::Session).effort,
        SelectorSupport::Applied
    );
    assert_eq!(
        spec.selector_support(SelectorPhase::Turn).effort,
        SelectorSupport::Applied
    );

    let adapter = Adapter::new(spec);
    let (tx, _rx) = mpsc::unbounded_channel();
    let mut request = SessionRequest::default();
    request.config.model = Some("cursor-grok-4.6-high".into());
    request.config.effort = Some("low".into());
    let session = adapter.open_session(request, tx).await.expect("open");
    let outcome = session.prompt(turn("work")).await.expect("prompt");
    assert_eq!(outcome.text.trim(), "work --model cursor-grok-4.6-low");

    // A turn-level effort rewrites the session default too; `-fast` is preserved.
    let outcome = session
        .prompt(TurnRequest {
            text: "again".into(),
            model: Some("cursor-grok-4.6-high-fast".into()),
            effort: Some("xhigh".into()),
        })
        .await
        .expect("prompt");
    assert_eq!(
        outcome.text.trim(),
        "again --model cursor-grok-4.6-xhigh-fast"
    );
    session.close().await.expect("close");
}

#[tokio::test]
async fn cli_effort_suffix_fold_requires_a_model_carrier() {
    // Without a reachable `{model}` the suffix mechanism cannot express the
    // selector — support stays fail-closed.
    let mut spec = cli_spec(
        "echo",
        vec!["{prompt}".into()],
        PromptChannel::ArgOnly,
        StreamMode::Stdio,
        None,
    );
    let LaneSpec::Cli(cli) = &mut spec.lane else {
        unreachable!()
    };
    cli.effort_model_suffixes = vec!["high".into()];
    assert_eq!(
        spec.selector_support(SelectorPhase::Session).effort,
        SelectorSupport::Unsupported
    );
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
    let outcome = tokio::time::timeout(test_settings::WAIT_BUDGET, session.prompt(turn("")))
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

#[cfg(unix)]
#[tokio::test]
async fn sidecar_shared_close_terminates_active_turns_and_eof_closes_remaining_sessions() {
    struct FixtureCleanup(Vec<libc::pid_t>);
    impl Drop for FixtureCleanup {
        fn drop(&mut self) {
            for pid in &self.0 {
                unsafe {
                    libc::kill(*pid, libc::SIGKILL);
                }
            }
        }
    }
    let mut cleanup = FixtureCleanup(Vec::new());
    let modes = ["acp", "rpc", "cli", "pty", "eof"];
    tokio::time::timeout(test_settings::WAIT_BUDGET * modes.len() as u32,async {
        let mut child=tokio::process::Command::new(FIXTURE).arg("serve")
            .stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null()).kill_on_drop(true).spawn().unwrap();
        let (tx,mut rx)=mpsc::unbounded_channel();
        let peer=JsonRpcPeer::spawn(child.stdout.take().unwrap(),child.stdin.take().unwrap(),
            Arc::new(SidecarEvents {notifications:tx,permission_granted:std::sync::atomic::AtomicBool::new(false)}),None,a4aa::framing::DEFAULT_MAX_MESSAGE_BYTES);
        for mode in modes {
            let script=format!("mode={mode:?}\n{}",r#"
import os,sys,json,signal
def send(m): print(json.dumps(m),flush=True)
payload=json.dumps({'pid':os.getpid()})
if mode in ['cli','pty']:
    print(payload,flush=True)
    signal.pause()
else:
    for line in sys.stdin:
        m=json.loads(line)
        if m['method']=='initialize': result={'protocolVersion':1,'agentCapabilities':{}}
        elif m['method']=='session/new': result={'sessionId':'s'}
        else:
            if mode=='acp':
                send({'jsonrpc':'2.0','method':'session/update','params':{'sessionId':'s','update':{'sessionUpdate':'agent_message_chunk','content':{'type':'text','text':payload}}}})
            else: send({'jsonrpc':'2.0','method':'ready','params':{'text':payload}})
            signal.pause()
            raise AssertionError('fixture was not terminated')
        send({'jsonrpc':'2.0','id':m['id'],'result':result})
"#);
            let lane=match mode {
                "acp"=>LaneSpec::Acp {mcp_servers:vec![],prefer_load:false,auth_key:None},
                "cli"|"pty"=>LaneSpec::Cli(Box::new(CliLaneSpec {args:vec!["{prompt}".into()],prompt_channel:PromptChannel::ArgOnly,
                    stream:if mode=="pty" {StreamMode::Pty}else{StreamMode::Stdio},..Default::default()})),
                _=>LaneSpec::Rpc(Box::new(RpcLaneSpec {
            transport: RpcTransport::Stdio,
            max_frame_bytes: None,
            usage_path: None,prompt:RpcCallSpec {method:"prompt".into(),params:json!({"text":"{prompt}"})},
                    text_event_method:Some("ready".into()),text_path:Some("text".into()),..Default::default()})),
            };
            let spec=AgentSpec {id:mode.into(),client_name:None,max_output_bytes:Some(1024),lane,
                command:CommandSpec {program:"python3".into(),args:vec!["-u".into(),"-c".into(),script],cwd:None,env:EnvPolicy::default()}};
            let opened=peer.call("session/open",json!({"spec":spec})).await.unwrap();
            let id=opened["sessionId"].as_str().unwrap();
            let pending=peer.start_call("session/prompt",json!({"sessionId":id,"text":"run"})).unwrap();
            let mut text=String::new();
            let pid=loop {
                let event=rx.recv().await.unwrap();
                if event["sessionId"]==id {
                    if let Some(chunk)=event["event"]["text"].as_str() {
                        text.push_str(chunk);
                        if let Ok(value)=serde_json::from_str::<Value>(&text) {
                            if let Some(pid)=value["pid"].as_i64() {break pid as libc::pid_t;}
                        }
                    }
                }
            };
            cleanup.0.push(pid);
            if mode=="eof" {
                peer.close().await;
                assert!(child.wait().await.unwrap().success());
            } else {
                peer.call("session/close",json!({"sessionId":id})).await.unwrap();
                let outcome=pending.wait().await;
                assert!(outcome.as_ref().is_err() || outcome.as_ref().is_ok_and(|value|value["stopReason"]=="cancelled"));
                assert!(peer.call("session/prompt",json!({"sessionId":id,"text":"must not run"})).await.is_err());
                peer.call("session/close",json!({"sessionId":id})).await.unwrap();
            }
            assert_eq!(unsafe {libc::kill(pid,0)},-1,"{mode} process survived close acknowledgement");
            assert_eq!(std::io::Error::last_os_error().raw_os_error(),Some(libc::ESRCH));
            cleanup.0.pop();
        }
    }).await.expect("shared session close did not complete");
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
        let deadline = std::time::Instant::now() + test_settings::WAIT_BUDGET;
        while std::time::Instant::now() < deadline && text != want {
            match tokio::time::timeout(test_settings::WAIT_BUDGET, rx.recv()).await {
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
    assert_eq!(error.code, "agent_configuration_unavailable", "{error:?}");
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
async fn acp_legacy_model_ack_must_be_decodable_and_consistent() {
    for (reply, error_code) in [
        ("empty", None),
        ("matching", None),
        ("mismatch", Some("agent_configuration_rejected")),
        ("malformed", Some("agent_protocol_error")),
    ] {
        let adapter = Adapter::new(acp_spec_with_env(&[
            ("A4AA_FIXTURE_LEGACY", "1"),
            ("A4AA_FIXTURE_MODEL_REPLY", reply),
        ]));
        let (tx, mut rx) = mpsc::unbounded_channel();
        let result = adapter
            .open_session(
                SessionRequest {
                    config: options(Some("fixture-large"), None, None, None),
                    ..Default::default()
                },
                tx,
            )
            .await;
        if let Some(code) = error_code {
            let error = match result {
                Ok(_) => panic!("invalid model acknowledgement accepted"),
                Err(error) => error,
            };
            assert_eq!(error.code, code);
            assert_eq!(error.stage, "session/set_model");
        } else {
            let session = result.unwrap();
            assert!(config_of(&session, &mut rx)
                .await
                .contains("model=fixture-large"));
            session.close().await.unwrap();
        }
    }
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

#[cfg(unix)]
#[tokio::test]
async fn sidecar_eof_during_establishment_terminates_the_establishing_agent() {
    struct FixtureCleanup(Vec<libc::pid_t>);
    impl Drop for FixtureCleanup {
        fn drop(&mut self) {
            for pid in &self.0 {
                unsafe {
                    libc::kill(*pid, libc::SIGKILL);
                }
            }
        }
    }
    let mut cleanup = FixtureCleanup(Vec::new());
    tokio::time::timeout(test_settings::WAIT_BUDGET * 2, async {
        for mode in ["acp", "rpc"] {
            let directory = tempfile::tempdir().unwrap();
            let pidfile = directory.path().join("agent.pid");
            // The agent reports its pid, then never answers `initialize`:
            // establishment stays in flight with a live process.
            let script = r#"
import os,time,pathlib
pathlib.Path(os.environ['A4AA_TEST_PIDFILE']).write_text(str(os.getpid()))
time.sleep(3600)
"#;
            let lane = match mode {
                "acp" => LaneSpec::Acp {
                    mcp_servers: vec![],
                    prefer_load: false,
                    auth_key: None,
                },
                _ => LaneSpec::Rpc(Box::new(RpcLaneSpec {
                    transport: RpcTransport::Stdio,
                    max_frame_bytes: None,
                    usage_path: None,
                    initialize: Some(RpcCallSpec {
                        method: "initialize".into(),
                        params: json!({}),
                    }),
                    prompt: RpcCallSpec {
                        method: "prompt".into(),
                        params: json!({"text": "{prompt}"}),
                    },
                    ..Default::default()
                })),
            };
            let spec = AgentSpec {
                id: format!("establishing-{mode}"),
                client_name: None,
                max_output_bytes: Some(1024),
                lane,
                command: CommandSpec {
                    program: "python3".into(),
                    args: vec!["-u".into(), "-c".into(), script.into()],
                    cwd: None,
                    env: fixture_env(&[("A4AA_TEST_PIDFILE", pidfile.to_str().unwrap())]),
                },
            };
            let mut child = tokio::process::Command::new(FIXTURE)
                .arg("serve")
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::null())
                .kill_on_drop(true)
                .spawn()
                .unwrap();
            let (tx, _rx) = mpsc::unbounded_channel();
            let peer = JsonRpcPeer::spawn(
                child.stdout.take().unwrap(),
                child.stdin.take().unwrap(),
                Arc::new(SidecarEvents {
                    notifications: tx,
                    permission_granted: std::sync::atomic::AtomicBool::new(false),
                }),
                None,
                a4aa::framing::DEFAULT_MAX_MESSAGE_BYTES,
            );
            let pending = peer
                .start_call("session/open", json!({"spec": spec}))
                .unwrap();
            // Wait until the agent is spawned and its handshake is in flight.
            let pid: libc::pid_t = loop {
                if let Ok(text) = std::fs::read_to_string(&pidfile) {
                    if let Ok(pid) = text.trim().parse() {
                        break pid;
                    }
                }
                tokio::time::sleep(test_settings::REAP_POLL_INTERVAL).await;
            };
            cleanup.0.push(pid);
            // Host EOF: the sidecar must abort the in-flight establishment,
            // terminate and reap the establishing agent, and exit cleanly.
            peer.close().await;
            assert!(pending.wait().await.is_err());
            let status = child.wait().await.unwrap();
            assert!(status.success(), "{mode}: sidecar exit {status:?}");
            let reaped = tokio::time::timeout(test_settings::WAIT_BUDGET, async {
                while unsafe { libc::kill(pid, 0) } == 0 {
                    tokio::time::sleep(test_settings::REAP_POLL_INTERVAL).await;
                }
                assert_eq!(
                    std::io::Error::last_os_error().raw_os_error(),
                    Some(libc::ESRCH),
                    "{mode}"
                );
            })
            .await;
            if reaped.is_err() {
                unsafe { libc::kill(pid, libc::SIGKILL) };
            }
            reaped
                .unwrap_or_else(|_| panic!("{mode}: establishing agent survived sidecar shutdown"));
            cleanup.0.pop();
        }
    })
    .await
    .expect("sidecar EOF cleanup stalled");
}

#[tokio::test]
async fn acp_non_auth_establishment_error_never_attempts_authenticate() {
    // The agent advertises an auth method but fails session/new with a
    // generic internal error: authenticate must not be attempted, and the
    // original failure returns unchanged.
    let directory = tempfile::tempdir().unwrap();
    let log = directory.path().join("requests.log");
    let script = r#"
import json,sys,os
log=open(os.environ['A4AA_TEST_LOG'],'a')
for line in sys.stdin:
    m=json.loads(line)
    method=m.get('method','')
    log.write(method+'\n'); log.flush()
    if method=='initialize':
        result={'protocolVersion':1,'agentCapabilities':{},'authMethods':[{'id':'stub-auth','name':'Stub'}]}
        print(json.dumps({'jsonrpc':'2.0','id':m['id'],'result':result}),flush=True)
    elif method=='session/new':
        print(json.dumps({'jsonrpc':'2.0','id':m['id'],'error':{'code':-32603,'message':'generic failure'}}),flush=True)
    else:
        print(json.dumps({'jsonrpc':'2.0','id':m['id'],'result':{}}),flush=True)
"#;
    let mut spec = acp_spec();
    spec.command = CommandSpec {
        program: "python3".into(),
        args: vec!["-u".into(), "-c".into(), script.into()],
        cwd: None,
        env: fixture_env(&[("A4AA_TEST_LOG", log.to_str().unwrap())]),
    };
    let (tx, _rx) = mpsc::unbounded_channel();
    let error = match Adapter::new(spec)
        .open_session(SessionRequest::default(), tx)
        .await
    {
        Ok(_) => panic!("generic establishment failure must not open"),
        Err(error) => error,
    };
    assert_eq!(error.code, "agent_protocol_failed");
    let log = std::fs::read_to_string(&log).unwrap();
    assert!(log.contains("session/new"));
    assert!(
        !log.contains("authenticate"),
        "a non-auth failure triggered authenticate: {log}"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn sidecar_eof_after_completed_handoff_closes_the_inserted_session() {
    struct Guard(Option<libc::pid_t>);
    impl Drop for Guard {
        fn drop(&mut self) {
            if let Some(pid) = self.0 {
                unsafe {
                    libc::kill(pid, libc::SIGKILL);
                }
            }
        }
    }
    let mut guard = Guard(None);
    tokio::time::timeout(test_settings::WAIT_BUDGET, async {
        let directory = tempfile::tempdir().unwrap();
        let pidfile = directory.path().join("agent.pid");
        let release = directory.path().join("release");
        // The agent answers initialize only once the release file exists,
        // then stays alive: the establishment completes into a live session.
        let script = r#"
import os,sys,time,pathlib,json,signal
pathlib.Path(os.environ['A4AA_TEST_PID']).write_text(str(os.getpid()))
m=json.loads(sys.stdin.readline())
release=pathlib.Path(os.environ['A4AA_TEST_RELEASE'])
while not release.exists(): time.sleep(0.01)
print(json.dumps({'jsonrpc':'2.0','id':m['id'],'result':{'sessionId':'s'}}),flush=True)
signal.pause()
"#;
        let spec = AgentSpec {
            id: "gated-handoff".into(),
            client_name: None,
            max_output_bytes: Some(1024),
            lane: LaneSpec::Rpc(Box::new(RpcLaneSpec {
                transport: RpcTransport::Stdio,
                max_frame_bytes: None,
                usage_path: None,
                initialize: Some(RpcCallSpec {
                    method: "initialize".into(),
                    params: json!({}),
                }),
                prompt: RpcCallSpec {
                    method: "prompt".into(),
                    params: json!({"text": "{prompt}"}),
                },
                ..Default::default()
            })),
            command: CommandSpec {
                program: "python3".into(),
                args: vec!["-u".into(), "-c".into(), script.into()],
                cwd: None,
                env: fixture_env(&[
                    ("A4AA_TEST_PID", pidfile.to_str().unwrap()),
                    ("A4AA_TEST_RELEASE", release.to_str().unwrap()),
                ]),
            },
        };
        let mut child = tokio::process::Command::new(FIXTURE)
            .arg("serve")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let (tx, _rx) = mpsc::unbounded_channel();
        let peer = JsonRpcPeer::spawn(
            child.stdout.take().unwrap(),
            child.stdin.take().unwrap(),
            Arc::new(SidecarEvents {
                notifications: tx,
                permission_granted: std::sync::atomic::AtomicBool::new(false),
            }),
            None,
            a4aa::framing::DEFAULT_MAX_MESSAGE_BYTES,
        );
        let pending = peer
            .start_call("session/open", json!({"spec": spec}))
            .unwrap();
        let pid: libc::pid_t = loop {
            if let Ok(text) = std::fs::read_to_string(&pidfile) {
                if let Ok(pid) = text.trim().parse() {
                    break pid;
                }
            }
            tokio::time::sleep(test_settings::REAP_POLL_INTERVAL).await;
        };
        guard.0 = Some(pid);
        // Release the handshake while the host is still connected: the
        // establishment task inserts the session before replying, so an Ok
        // response proves the session is owned by the sessions map.
        std::fs::write(&release, b"1").unwrap();
        let opened = pending.wait().await.expect("session/open response");
        assert!(opened["sessionId"].as_str().unwrap().starts_with("a4aa-"));
        assert_eq!(unsafe { libc::kill(pid, 0) }, 0, "session agent not live");
        // Host EOF: shutdown closes the inserted session through the shared
        // close path and reaps the agent.
        peer.close().await;
        let status = child.wait().await.unwrap();
        assert!(status.success(), "sidecar exit {status:?}");
        tokio::time::timeout(test_settings::WAIT_BUDGET, async {
            while unsafe { libc::kill(pid, 0) } == 0 {
                tokio::time::sleep(test_settings::REAP_POLL_INTERVAL).await;
            }
            assert_eq!(
                std::io::Error::last_os_error().raw_os_error(),
                Some(libc::ESRCH)
            );
        })
        .await
        .expect("inserted session's agent survived sidecar shutdown");
        guard.0 = None;
    })
    .await
    .expect("completed-handoff shutdown stalled");
    drop(guard);
}

#[cfg(unix)]
#[tokio::test]
async fn sidecar_eof_racing_open_completion_never_strands_a_session() {
    struct Guard(Vec<libc::pid_t>);
    impl Drop for Guard {
        fn drop(&mut self) {
            for pid in &self.0 {
                unsafe {
                    libc::kill(*pid, libc::SIGKILL);
                }
            }
        }
    }
    let mut guard = Guard(Vec::new());
    tokio::time::timeout(test_settings::WAIT_BUDGET * 8, async {
        // Drive the completion-vs-shutdown race repeatedly: whichever side
        // wins, the session is either inserted and closed by shutdown or
        // closed by the establishment task — never left live and
        // unregistered.
        for _ in 0..8 {
            let directory = tempfile::tempdir().unwrap();
            let pidfile = directory.path().join("agent.pid");
            let release = directory.path().join("release");
            let script = r#"
import os,sys,time,pathlib,json,signal
pathlib.Path(os.environ['A4AA_TEST_PID']).write_text(str(os.getpid()))
m=json.loads(sys.stdin.readline())
release=pathlib.Path(os.environ['A4AA_TEST_RELEASE'])
while not release.exists(): time.sleep(0.005)
print(json.dumps({'jsonrpc':'2.0','id':m['id'],'result':{'sessionId':'s'}}),flush=True)
signal.pause()
"#;
            let spec = AgentSpec {
                id: "gated-handoff-race".into(),
                client_name: None,
                max_output_bytes: Some(1024),
                lane: LaneSpec::Rpc(Box::new(RpcLaneSpec {
                    transport: RpcTransport::Stdio,
                    max_frame_bytes: None,
                    usage_path: None,
                    initialize: Some(RpcCallSpec {
                        method: "initialize".into(),
                        params: json!({}),
                    }),
                    prompt: RpcCallSpec {
                        method: "prompt".into(),
                        params: json!({"text": "{prompt}"}),
                    },
                    ..Default::default()
                })),
                command: CommandSpec {
                    program: "python3".into(),
                    args: vec!["-u".into(), "-c".into(), script.into()],
                    cwd: None,
                    env: fixture_env(&[
                        ("A4AA_TEST_PID", pidfile.to_str().unwrap()),
                        ("A4AA_TEST_RELEASE", release.to_str().unwrap()),
                    ]),
                },
            };
            let mut child = tokio::process::Command::new(FIXTURE)
                .arg("serve")
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::null())
                .kill_on_drop(true)
                .spawn()
                .unwrap();
            let (tx, _rx) = mpsc::unbounded_channel();
            let peer = JsonRpcPeer::spawn(
                child.stdout.take().unwrap(),
                child.stdin.take().unwrap(),
                Arc::new(SidecarEvents {
                    notifications: tx,
                    permission_granted: std::sync::atomic::AtomicBool::new(false),
                }),
                None,
                a4aa::framing::DEFAULT_MAX_MESSAGE_BYTES,
            );
            let pending = peer
                .start_call("session/open", json!({"spec": spec}))
                .unwrap();
            let pid: libc::pid_t = loop {
                if let Ok(text) = std::fs::read_to_string(&pidfile) {
                    if let Ok(pid) = text.trim().parse() {
                        break pid;
                    }
                }
                tokio::time::sleep(test_settings::REAP_POLL_INTERVAL).await;
            };
            guard.0.push(pid);
            // Complete the handshake and start shutdown back to back.
            std::fs::write(&release, b"1").unwrap();
            peer.close().await;
            // The open resolves (inserted-then-closed by shutdown) or fails
            // (closed by the establishment task, or the host left first) —
            // both are the contract; an unregistered live session is not.
            let _ = pending.wait().await;
            let status = child.wait().await.unwrap();
            assert!(status.success(), "sidecar exit {status:?}");
            tokio::time::timeout(test_settings::WAIT_BUDGET, async {
                while unsafe { libc::kill(pid, 0) } == 0 {
                    tokio::time::sleep(test_settings::REAP_POLL_INTERVAL).await;
                }
                assert_eq!(
                    std::io::Error::last_os_error().raw_os_error(),
                    Some(libc::ESRCH)
                );
            })
            .await
            .expect("raced-open agent survived sidecar shutdown");
            guard.0.pop();
        }
    })
    .await
    .expect("completion/shutdown race stalled");
}

#[cfg(unix)]
#[tokio::test]
async fn sidecar_notification_cancel_warns_and_only_request_form_cancels() {
    use tokio::io::AsyncBufReadExt;
    tokio::time::timeout(test_settings::WAIT_BUDGET * 2, async {
        let script = r#"
import json,sys
for line in sys.stdin:
    m=json.loads(line)
    method=m.get('method')
    if method=='prompt':
        print(json.dumps({'jsonrpc':'2.0','method':'ready','params':{'text':'running'}}),flush=True)
        print(json.dumps({'jsonrpc':'2.0','id':m['id'],'result':{'id':'t'}}),flush=True)
    elif method=='cancel':
        print(json.dumps({'jsonrpc':'2.0','method':'done','params':{'turn':'t','status':'cancelled'}}),flush=True)
"#;
        let spec = AgentSpec {
            id: "notify-cancel".into(),
            client_name: None,
            max_output_bytes: Some(1024),
            lane: LaneSpec::Rpc(Box::new(RpcLaneSpec {
            transport: RpcTransport::Stdio,
            max_frame_bytes: None,
        usage_path: None,
                prompt: RpcCallSpec {
                    method: "prompt".into(),
                    params: json!({"text": "{prompt}"}),
                },
                cancel: Some(RpcCallSpec {
                    method: "cancel".into(),
                    params: json!({}),
                }),
                completion: Some(a4aa::RpcCompletionSpec {
                    method: "done".into(),
                    turn_id_path: Some("turn".into()),
                    status_path: Some("status".into()),
                    status_value: None,
                }),
                text_event_method: Some("ready".into()),
                text_path: Some("text".into()),
                ..Default::default()
            })),
            command: CommandSpec {
                program: "python3".into(),
                args: vec!["-u".into(), "-c".into(), script.into()],
                cwd: None,
                env: EnvPolicy::default(),
            },
        };
        let mut child = tokio::process::Command::new(FIXTURE)
            .arg("serve")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let (tx, mut rx) = mpsc::unbounded_channel();
        let peer = JsonRpcPeer::spawn(
            child.stdout.take().unwrap(),
            child.stdin.take().unwrap(),
            Arc::new(SidecarEvents {
                notifications: tx,
                permission_granted: std::sync::atomic::AtomicBool::new(false),
            }),
            None,
            a4aa::framing::DEFAULT_MAX_MESSAGE_BYTES,
        );
        let mut stderr = tokio::io::BufReader::new(child.stderr.take().unwrap());
        let opened = peer
            .call("session/open", json!({"spec": spec}))
            .await
            .unwrap();
        let id = opened["sessionId"].as_str().unwrap().to_owned();
        let pending = peer
            .start_call("session/prompt", json!({"sessionId": id, "text": "run"}))
            .unwrap();
        loop {
            let event = rx.recv().await.unwrap();
            if event["sessionId"] == id && event["event"]["text"].as_str() == Some("running") {
                break;
            }
        }
        // Notification-form cancel: ignored — with a visible stderr warning.
        peer.notify("session/cancel", json!({"sessionId": id}))
            .unwrap();
        let mut saw_warning = String::new();
        while !saw_warning.contains("notification-style session/cancel ignored") {
            let mut line = String::new();
            stderr.read_line(&mut line).await.unwrap();
            assert!(
                !line.is_empty(),
                "sidecar stderr closed before the cancel warning"
            );
            saw_warning.push_str(&line);
        }
        // The request form cancels: if the notification had cancelled the
        // turn, this cancel would be an idle no-op and the turn would hang.
        peer.call("session/cancel", json!({"sessionId": id}))
            .await
            .unwrap();
        let outcome = pending.wait().await.unwrap();
        assert_eq!(outcome["stopReason"], "cancelled");
        peer.call("session/close", json!({"sessionId": id}))
            .await
            .unwrap();
        peer.close().await;
        let _ = child.kill().await;
    })
    .await
    .expect("notification-style cancel did not follow the documented contract");
}
