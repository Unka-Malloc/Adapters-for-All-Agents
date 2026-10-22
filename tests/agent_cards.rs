//! Lane-honoring tests for the card-declared surfaces, using `sh` fixtures
//! exercising the production descriptor crates. No network, no real agents.

use a4aa::{
    Adapter, AgentEvent, AgentSpec, CommandSpec, EnvPolicy, LaneSession, LaneSpec, SessionConfig,
    SessionRequest, TurnRequest,
};
use std::time::Duration;
use tokio::sync::mpsc;

fn sh(script: &str) -> CommandSpec {
    CommandSpec {
        program: "sh".into(),
        args: vec!["-c".into(), script.to_owned()],
        cwd: None,
        env: EnvPolicy::default(),
    }
}

fn session() -> SessionRequest {
    SessionRequest::default()
}

fn turn(text: &str) -> TurnRequest {
    TurnRequest {
        text: text.to_owned(),
        model: None,
        effort: None,
    }
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

/// Codex-shaped rpc surface: `initialize`/`initialized` handshake, then
/// `thread/start`, then `turn/start` → async `turn/completed`.
fn codex_like_spec() -> AgentSpec {
    let script = r#"
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
  case "$line" in
    *'"method":"initialize"'*)
      printf '{"jsonrpc":"2.0","id":%s,"result":{"capabilities":{}}}\n' "$id" ;;
    *'"method":"initialized"'*) ;;
    *'"thread/start"'*)
      printf '{"jsonrpc":"2.0","id":%s,"result":{"thread":{"id":"t-1","status":"active"}}}\n' "$id" ;;
    *'"thread/resume"'*)
      printf '{"jsonrpc":"2.0","id":%s,"result":{"thread":{"id":"resumed-thread","status":"active"}}}\n' "$id" ;;
    *'"turn/start"'*)
      printf '{"jsonrpc":"2.0","id":%s,"result":{"turn":{"id":"u-1","status":"inProgress"}}}\n' "$id"
      printf '{"jsonrpc":"2.0","method":"item/agentMessage/delta","params":{"delta":"hello "}}\n'
      printf '{"jsonrpc":"2.0","method":"item/agentMessage/delta","params":{"delta":"world"}}\n'
      printf '{"jsonrpc":"2.0","method":"turn/completed","params":{"threadId":"t-1","turn":{"id":"u-1","status":"completed"}}}\n' ;;
    *'"turn/interrupt"'*) ;;
  esac
done
"#;
    let mut spec = a4aa_codex::spec();
    spec.command = sh(script);
    spec
}

/// Claude-shaped cli surface: stream-json in on stdin, NDJSON events out.
fn claude_like_spec() -> AgentSpec {
    let script = r#"
sid="claude-fresh-1"
resumed=""
while [ $# -gt 0 ]; do
  case "$1" in --resume) resumed="$2"; shift 2 ;; *) shift ;; esac
done
IFS= read -r _prompt
printf '{"type":"system","subtype":"init","session_id":"%s"}\n' "$sid"
printf '{"type":"assistant","message":{"content":[{"type":"text","text":"partial"}],"usage":{"input_tokens":9,"output_tokens":4}}}\n'
if [ -n "$resumed" ]; then
  printf '{"type":"result","subtype":"success","result":"resumed:%s","session_id":"%s","usage":{"input_tokens":120,"cache_read_input_tokens":30,"cache_creation_input_tokens":7,"output_tokens":45}}\n' "$resumed" "$sid"
else
  printf '{"type":"result","subtype":"success","result":"first","session_id":"%s","usage":{"input_tokens":120,"cache_read_input_tokens":30,"cache_creation_input_tokens":7,"output_tokens":45}}\n' "$sid"
fi
"#;
    let mut spec = a4aa_claude::spec();
    spec.command = CommandSpec {
        program: "sh".into(),
        args: vec!["-c".into(), script.to_owned(), "sh".into()],
        cwd: None,
        env: EnvPolicy::default(),
    };
    spec
}

#[tokio::test]
async fn rpc_handshake_open_completion_and_binding() {
    let (session, mut events) = open(codex_like_spec(), session()).await;
    // Nested `thread.id` becomes the session binding.
    assert_eq!(session.session_id(), "t-1");

    let outcome = session.prompt(turn("say hi")).await.expect("prompt");
    // The completion notification, not the turn/start ack, ends the turn.
    assert_eq!(outcome.stop_reason, "completed");
    assert_eq!(outcome.text, "hello world");

    // Streamed deltas arrived as MessageChunk events on the same surface.
    let mut chunks = String::new();
    while let Ok(event) = events.try_recv() {
        if let AgentEvent::MessageChunk { text } = event {
            chunks.push_str(&text);
        }
    }
    assert_eq!(chunks, "hello world");

    session.cancel().await.expect("cancel");
    session.close().await.expect("close");
}

#[tokio::test]
async fn rpc_resume_uses_the_declared_resume_call() {
    let mut request = session();
    request.resume = Some("thread-9".into());
    let (session, _events) = open(codex_like_spec(), request).await;
    // The requested binding wins over whatever the call reported.
    assert_eq!(session.session_id(), "thread-9");
    session.close().await.expect("close");
}

#[tokio::test]
async fn rpc_resume_without_a_resume_surface_fails_closed() {
    let mut spec = codex_like_spec();
    let LaneSpec::Rpc(rpc) = &mut spec.lane else {
        unreachable!()
    };
    rpc.session_resume = None;
    let mut request = session();
    request.resume = Some("thread-9".into());
    let (tx, _rx) = mpsc::unbounded_channel();
    let error = match Adapter::new(spec).open_session(request, tx).await {
        Ok(_) => panic!("resume without a resume surface must fail closed"),
        Err(error) => error,
    };
    assert!(error
        .to_string()
        .contains("agent_session_resume_unsupported"));
}

#[tokio::test]
async fn cli_json_lines_capture_session_usage_and_raw_events() {
    let (session, mut events) = open(claude_like_spec(), session()).await;
    let outcome = session.prompt(turn("hi")).await.expect("prompt");

    assert_eq!(outcome.stop_reason, "end_turn");
    // The init line's session_id became the binding.
    assert_eq!(session.session_id(), "claude-fresh-1");
    // The result line's usage lands on the outcome; cache-creation is not
    // counted as cached input, and no total is invented.
    let usage = outcome.usage.expect("usage");
    assert_eq!(usage.input, Some(120));
    assert_eq!(usage.cached_input, Some(30));
    assert_eq!(usage.output, Some(45));
    assert_eq!(usage.total, None);

    let mut raw_lines = 0usize;
    let mut usage_events = 0usize;
    while let Ok(event) = events.try_recv() {
        match event {
            AgentEvent::Raw { raw } => raw_lines += usize::from(raw.is_object()),
            AgentEvent::Usage { .. } => usage_events += 1,
            _ => {}
        }
    }
    // init + assistant + result records.
    assert_eq!(raw_lines, 3);
    // Only the result line carries top-level `usage`; the assistant record's
    // per-response usage stays nested under `message.usage` (the real claude
    // shape) and is reachable through its Raw event, not double-counted.
    assert_eq!(usage_events, 1);

    // The next turn resumes the observed binding through `resume_args`.
    let outcome = session.prompt(turn("again")).await.expect("prompt");
    assert!(outcome.text.contains("resumed:claude-fresh-1"));
}

#[tokio::test]
async fn cli_cancel_terminates_the_in_flight_turn() {
    let mut spec = claude_like_spec();
    spec.command.args[1] = r#"
IFS= read -r _prompt
printf '{"type":"system","subtype":"init","session_id":"claude-sleep"}\n'
case "$_prompt" in
  *wait*) sleep 60 ;;
  *) printf '{"type":"result","result":"done"}\n' ;;
esac
"#
    .to_owned();
    let (session, _events) = open(spec, session()).await;
    {
        let mut prompt = std::pin::pin!(session.prompt(turn("wait")));
        tokio::select! {
            outcome = &mut prompt => {
                panic!("the turn cannot finish on its own: {outcome:?}");
            }
            _ = tokio::time::sleep(Duration::from_millis(200)) => {
                session.cancel().await.expect("cancel");
            }
        }
        let outcome = prompt.await.expect("prompt");
        assert_eq!(outcome.stop_reason, "cancelled");
    }

    // A cancelled session still accepts a fresh turn on a new process.
    let outcome = session.prompt(turn("after")).await.expect("prompt");
    assert_eq!(outcome.stop_reason, "end_turn");
    session.close().await.expect("close");
}

// ---- Second-batch agent surfaces ------------------------------------

/// Pi-shaped rpc surface: flat `{type,id,...}` envelope — not JSON-RPC —
/// with `get_state` open, `switch_session` resume, per-turn
/// `set_model`/`set_thinking_level`, `prompt`, `message_update` deltas and
/// an `agent_settled` completion.
fn pi_like_spec() -> AgentSpec {
    let script = r#"
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":"\([^"]*\)".*/\1/p')
  case "$line" in
    *'"type":"get_state"'*)
      printf '{"type":"response","command":"get_state","id":"%s","success":true,"data":{"sessionId":"pi-s-1"}}\n' "$id" ;;
    *'"type":"switch_session"'*)
      printf '{"type":"response","command":"switch_session","id":"%s","success":true,"data":{}}\n' "$id" ;;
    *'"type":"set_model"'*|*'"type":"set_thinking_level"'*)
      printf '{"type":"response","id":"%s","success":true,"data":{}}\n' "$id" ;;
    *'"type":"prompt"'*)
      printf '{"type":"response","command":"prompt","id":"%s","success":true,"data":{}}\n' "$id"
      printf '{"type":"message_update","data":{"assistantMessageEvent":{"delta":"chunk-a "}}}\n'
      printf '{"type":"message_update","data":{"assistantMessageEvent":{"delta":"chunk-b"}}}\n'
      printf '{"type":"agent_settled","data":{}}\n' ;;
  esac
done
"#;
    let mut spec = a4aa_pi::spec();
    spec.command = sh(script);
    spec
}

/// DeepSeek-Harness ACP surface: the production `dsh acp` lane with the
/// crate's deterministic `fixture-agent` in place of the `dsh` binary.
/// `session/new` advertises `configOptions`; a turn streams
/// `session/update` chunks and ends at `end_turn`. Resume uses
/// `session/resume` (`prefer_load` is false — DSH does not implement
/// `session/load`).
fn dsh_like_spec() -> AgentSpec {
    let mut spec = a4aa_dsh::spec();
    spec.command = CommandSpec {
        program: env!("CARGO_BIN_EXE_a4aa").into(),
        args: vec!["fixture-agent".into()],
        cwd: None,
        env: EnvPolicy::default(),
    };
    spec
}

/// Cursor-shaped cli surface: `create-chat` mints the binding at open;
/// every turn is a `--resume <id>` stream-json process whose result record
/// carries the authoritative text.
fn cursor_like_spec() -> AgentSpec {
    let open_script = r#"printf 'chat-1\n'"#;
    let turn_script = r#"
sid=""
prev=""
while [ $# -gt 0 ]; do
  case "$1" in
    --resume) sid="$2"; shift 2 ;;
    *) prev="$1"; shift ;;
  esac
done
printf '{"type":"init","session_id":"%s"}\n' "$sid"
printf '{"type":"assistant","message":{"content":[{"type":"text","text":"partial"}]},"session_id":"%s"}\n' "$sid"
printf '{"type":"result","result":"answer for %s","session_id":"%s"}\n' "$prev" "$sid"
"#;
    // The card's declared primary carries the stream-json template
    // this fixture speaks.
    let mut spec = a4aa_cursor::card()
        .specs()
        .into_iter()
        .next()
        .expect("cursor declares the cli lane");
    spec.command = CommandSpec {
        program: "sh".into(),
        args: vec![
            "-c".into(),
            format!("if [ \"$1\" = create-chat ]; then {open_script}; exit 0; fi; {turn_script}"),
            "sh".into(),
        ],
        cwd: None,
        env: EnvPolicy::default(),
    };
    spec
}

/// Use the real Antigravity card with a deterministic shell process in place
/// of `agy`; the fixture emits the documented stream-json event shape.
fn antigravity_like_spec() -> AgentSpec {
    let script = r#"
model=""
effort=""
mode=""
resume=""
while [ $# -gt 0 ]; do
  case "$1" in
    --conversation) resume="$2"; shift 2 ;;
    --model) model="$2"; shift 2 ;;
    --effort) effort="$2"; shift 2 ;;
    --mode) mode="$2"; shift 2 ;;
    *) shift ;;
  esac
done
sid="agy-9"
printf '{"event":"init","conversation_id":"%s","init":{"permission_mode":"request-review"}}\n' "$sid"
printf '{"event":"step_update","step_update":{"step_type":"agent_response","state":"DONE","text_delta":"turn text\\n"}}\n'
response="model=$model;effort=$effort;mode=$mode;resume=$resume"
printf '{"event":"result","result":{"conversation_id":"%s","status":"SUCCESS","response":"%s","usage":{"input_tokens":10,"cache_read_tokens":2,"output_tokens":5,"total_tokens":17}}}\n' "$sid" "$response"
"#;
    let mut spec = a4aa_antigravity::spec();
    spec.command = sh(script);
    spec
}

#[tokio::test]
async fn pi_flat_envelope_turn() {
    let (session, mut events) = open(pi_like_spec(), session()).await;
    // `get_state`'s data.sessionId became the binding.
    assert_eq!(session.session_id(), "pi-s-1");

    let mut request = turn("hello");
    request.model = Some("openai/gpt-5".into());
    request.effort = Some("high".into());
    let outcome = session.prompt(request).await.expect("prompt");
    assert_eq!(outcome.text, "chunk-a chunk-b");
    let mut chunks = String::new();
    while let Ok(event) = events.try_recv() {
        if let AgentEvent::MessageChunk { text } = event {
            chunks.push_str(&text);
        }
    }
    assert_eq!(chunks, "chunk-a chunk-b");

    session.cancel().await.expect("cancel kills the process");
}

#[tokio::test]
async fn pi_resume_switches_session_path() {
    let mut request = session();
    // Pi resumes by session-file path, not by opaque id. Keep the value
    // synthetic so the contract test does not look like a host path.
    request.resume = Some("session-store/s-1.jsonl".into());
    let (session, _events) = open(pi_like_spec(), request).await;
    assert_eq!(session.session_id(), "session-store/s-1.jsonl");
    session.close().await.expect("close");
}

#[tokio::test]
async fn dsh_acp_session_new_prompt_and_catalog() {
    let (session, mut events) = open(dsh_like_spec(), session()).await;
    assert_eq!(session.session_id(), "fixture-session-1");
    let LaneSession::Acp(acp) = session.lane() else {
        panic!("dsh drives the acp lane")
    };
    let providers = a4aa_dsh::providers_from_config_options(&acp.session_state()["configOptions"]);
    assert!(
        providers.iter().any(|provider| !provider.models.is_empty()),
        "session/new must advertise models via configOptions"
    );

    let outcome = session.prompt(turn("go")).await.expect("prompt");
    assert_eq!(outcome.stop_reason, "end_turn");
    // The ACP lane keeps TurnOutcome.text empty and delivers streamed
    // text as AgentEvent::MessageChunk so consumers do not double the body.
    assert_eq!(outcome.text, "");
    let mut chunks = String::new();
    while let Ok(event) = events.try_recv() {
        if let AgentEvent::MessageChunk { text } = event {
            chunks.push_str(&text);
        }
    }
    assert_eq!(chunks, "echo: go");
    session.close().await.expect("close");
}

#[tokio::test]
async fn dsh_acp_resume_reuses_the_binding() {
    let mut request = session();
    request.resume = Some("durable-42".into());
    let (session, _events) = open(dsh_like_spec(), request).await;
    // DSH ACP `session/resume` keeps the host binding; it does not mint a
    // process-scoped wire id.
    assert_eq!(session.session_id(), "durable-42");
    session.close().await.expect("close");
}

#[tokio::test]
async fn cursor_create_chat_then_resume_turns() {
    let (session, mut events) = open(cursor_like_spec(), session()).await;
    // `create-chat` ran at open; its stdout line became the binding.
    assert_eq!(session.session_id(), "chat-1");

    let outcome = session.prompt(turn("what is up")).await.expect("prompt");
    // The result record, not the raw stream, is the authoritative text.
    assert_eq!(outcome.text, "answer for what is up");
    // Stream identity stayed bound: init and assistant records repeated it.
    let mut raws = 0usize;
    while let Ok(event) = events.try_recv() {
        if let AgentEvent::Raw { .. } = event {
            raws += 1;
        }
    }
    assert_eq!(raws, 3);
    session.close().await.expect("close");
}

#[tokio::test]
async fn antigravity_stream_json_captures_config_session_usage_and_resumes() {
    let mut request = session();
    request.config = SessionConfig {
        mode: Some("plan".into()),
        ..Default::default()
    };
    let (session, mut events) = open(antigravity_like_spec(), request).await;
    assert!(session.session_id().is_empty());
    let mut first_turn = turn("hi");
    first_turn.model = Some("gemini-4-argon-high".into());
    first_turn.effort = Some("high".into());
    let outcome = session.prompt(first_turn).await.expect("prompt");
    assert_eq!(
        outcome.text,
        "model=gemini-4-argon-high;effort=high;mode=plan;resume="
    );
    assert_eq!(session.session_id(), "agy-9");
    let usage = outcome.usage.expect("stream usage");
    assert_eq!(usage.input, Some(10));
    assert_eq!(usage.cached_input, Some(2));
    assert_eq!(usage.output, Some(5));
    assert_eq!(usage.total, Some(17));
    let raw_lines = std::iter::from_fn(|| events.try_recv().ok())
        .filter(|event| matches!(event, AgentEvent::Raw { .. }))
        .count();
    assert_eq!(raw_lines, 3);

    // The next process resumes the durable id while preserving profile config.
    let mut second_turn = turn("again");
    second_turn.model = Some("gemini-4-argon-high".into());
    second_turn.effort = Some("high".into());
    let outcome = session.prompt(second_turn).await.expect("prompt");
    assert_eq!(
        outcome.text,
        "model=gemini-4-argon-high;effort=high;mode=plan;resume=agy-9"
    );
    assert_eq!(session.session_id(), "agy-9");
    session.close().await.expect("close");
}

#[tokio::test]
async fn antigravity_rejects_unavailable_permission_and_invalid_mode_on_open() {
    let mut request = session();
    request.config.permission = Some("request-review".into());
    let (tx, _rx) = mpsc::unbounded_channel();
    let error = match Adapter::new(antigravity_like_spec())
        .open_session(request, tx)
        .await
    {
        Ok(_) => panic!("the CLI card cannot apply a per-request permission preset"),
        Err(error) => error,
    };
    assert_eq!(error.code, "agent_permission_unsupported");

    let mut request = session();
    request.config.mode = Some("turbo".into());
    let (tx, _rx) = mpsc::unbounded_channel();
    let error = match Adapter::new(antigravity_like_spec())
        .open_session(request, tx)
        .await
    {
        Ok(_) => panic!("unsupported execution mode must be rejected"),
        Err(error) => error,
    };
    assert_eq!(error.code, "agent_mode_unsupported");
}

#[tokio::test]
async fn antigravity_cancel_terminates_the_streaming_process() {
    let script = r#"
printf '{"event":"init","conversation_id":"agy-cancel"}\n'
while :; do sleep 1; done
"#;
    let mut spec = a4aa_antigravity::spec();
    spec.command = sh(script);
    let (session, mut events) = open(spec, session()).await;
    let session = std::sync::Arc::new(session);
    let prompt_session = session.clone();
    let prompt = tokio::spawn(async move { prompt_session.prompt(turn("wait")).await });

    loop {
        match events.recv().await {
            Some(AgentEvent::Raw { raw }) if raw["event"] == "init" => break,
            Some(_) => {}
            None => panic!("the CLI exited before explicit cancellation"),
        }
    }
    session.cancel().await.expect("explicit cancellation");
    let outcome = prompt.await.expect("prompt task").expect("cancel outcome");
    assert_eq!(outcome.stop_reason, "cancelled");
    let session = match std::sync::Arc::try_unwrap(session) {
        Ok(session) => session,
        Err(_) => panic!("cancelled prompt released its session handle"),
    };
    session.close().await.expect("close");
}

#[tokio::test]
async fn antigravity_reports_cli_failure_with_the_bound_session() {
    let script = r#"
printf '{"event":"init","conversation_id":"agy-error"}\n'
printf '{"event":"result","result":{"conversation_id":"agy-error","status":"ERROR","response":"","error":"fixture failure"}}\n'
exit 1
"#;
    let mut spec = a4aa_antigravity::spec();
    spec.command = sh(script);
    let (session, _events) = open(spec, session()).await;
    let outcome = session.prompt(turn("fail")).await.expect("error outcome");
    assert_eq!(session.session_id(), "agy-error");
    assert_eq!(outcome.stop_reason, "error");
    assert_eq!(outcome.exit_code, Some(1));
    session.close().await.expect("close");
}

#[test]
fn registry_exposes_all_five_second_batch_cards() {
    for id in ["opencode", "dsh", "antigravity", "pi", "cursor"] {
        let card = a4aa_dialects::card(id).unwrap_or_else(|| panic!("card {id}"));
        assert_eq!(card.id, id);
        // Every card materializes a spec that names its canonical binary.
        let spec = card.spec();
        assert!(!spec.command.program.is_empty());
    }
    let opencode = a4aa_dialects::card("opencode").unwrap();
    let spec = opencode.spec();
    assert_eq!(spec.command.args, vec!["serve", "--port", "0"]);
    assert!(matches!(spec.lane, LaneSpec::Serve { .. }));
    let alternate = opencode.specs().into_iter().nth(1).unwrap();
    assert_eq!(alternate.command.args, vec!["acp".to_string()]);
    assert!(matches!(
        alternate.lane,
        LaneSpec::Acp {
            prefer_load: true,
            ..
        }
    ));
    let dsh = a4aa_dialects::card("dsh").unwrap();
    let spec = dsh.spec();
    assert_eq!(spec.command.program, "dsh");
    assert_eq!(spec.command.args, vec!["acp".to_string()]);
    assert!(matches!(
        spec.lane,
        LaneSpec::Acp {
            prefer_load: false,
            ..
        }
    ));
    assert_eq!(
        dsh.features.models,
        a4aa::ModelCatalog::RpcMethod("session/new")
    );
    let dsh_specs = dsh.specs();
    assert_eq!(dsh_specs.len(), 2);
    assert_eq!(
        dsh_specs[1].command.args,
        vec!["--profile".to_string(), "sdk".to_string()]
    );
    assert!(matches!(dsh_specs[1].lane, LaneSpec::Rpc(_)));

    let pi = a4aa_dialects::card("pi").unwrap();
    let pi_specs = pi.specs();
    assert_eq!(pi_specs.len(), 2);
    assert!(matches!(pi_specs[0].lane, LaneSpec::Rpc(_)));
    let LaneSpec::Cli(cli) = &pi_specs[1].lane else {
        panic!("pi's alternate is the print cli")
    };
    assert!(cli.args.iter().any(|arg| arg == "--print"));
    assert!(cli.args.iter().any(|arg| arg == "--thinking"));

    let antigravity = a4aa_dialects::card("antigravity").unwrap();
    let LaneSpec::Cli(cli) = antigravity.spec().lane else {
        panic!("antigravity drives the cli lane")
    };
    assert!(cli.args.iter().any(|arg| arg == "--effort"));
    assert_eq!(
        cli.resume_args,
        vec!["--conversation".to_string(), "{sessionId}".to_string()]
    );
}

/// Factory-shaped stream-jsonrpc surface: factory-tagged envelopes,
/// `droid.initialize_session` → `droid.add_user_message` → the
/// `agent_turn_completed` session notification ends the turn.
fn factory_jsonrpc_like_spec() -> AgentSpec {
    let script = r#"
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":"\([^"]*\)".*/\1/p')
  case "$line" in
    *'"method":"droid.initialize_session"'*)
      printf '{"type":"response","id":"%s","result":{"sessionId":"droid-s-1"}}\n' "$id" ;;
    *'"method":"droid.load_session"'*)
      printf '{"type":"response","id":"%s","result":{"session":{"messages":[]}}}\n' "$id" ;;
    *'"method":"droid.add_user_message"'*)
      printf '{"type":"response","id":"%s","result":{}}\n' "$id"
      printf '{"type":"notification","method":"droid.session_notification","params":{"sessionId":"droid-s-1","notification":{"type":"assistant_text_delta","textDelta":"factory "}}}\n'
      printf '{"type":"notification","method":"droid.session_notification","params":{"sessionId":"droid-s-1","notification":{"type":"assistant_text_delta","textDelta":"reply"}}}\n'
      printf '{"type":"notification","method":"droid.session_notification","params":{"sessionId":"droid-s-1","notification":{"type":"droid_working_state_changed","newState":"idle"}}}\n'
      printf '{"type":"notification","method":"droid.session_notification","params":{"sessionId":"droid-s-1","notification":{"type":"agent_turn_completed","reason":"end_turn"}}}\n' ;;
    *'"method":"droid.interrupt_session"'*|*'"method":"droid.close_session"'*)
      printf '{"type":"response","id":"%s","result":{}}\n' "$id" ;;
  esac
done
"#;
    let mut spec = a4aa_factory::card()
        .specs()
        .into_iter()
        .nth(2)
        .expect("factory declares the stream-jsonrpc alternate");
    spec.command = sh(script);
    spec
}

#[tokio::test]
async fn factory_jsonrpc_open_completion_and_binding() {
    let (session, mut events) = open(factory_jsonrpc_like_spec(), session()).await;
    assert_eq!(session.session_id(), "droid-s-1");

    let outcome = session.prompt(turn("hi")).await.expect("prompt");
    // `agent_turn_completed` ends the turn; working-state churn did not.
    assert_eq!(outcome.stop_reason, "agent_turn_completed");
    assert_eq!(outcome.text, "factory reply");
    let mut chunks = String::new();
    while let Ok(event) = events.try_recv() {
        if let AgentEvent::MessageChunk { text } = event {
            chunks.push_str(&text);
        }
    }
    assert_eq!(chunks, "factory reply");
    session.close().await.expect("close");
}

#[tokio::test]
async fn factory_jsonrpc_resume_keeps_the_requested_binding() {
    let mut request = session();
    request.resume = Some("durable-droid-9".into());
    let (session, _events) = open(factory_jsonrpc_like_spec(), request).await;
    // `droid.load_session` echoes no id — the requested binding stands.
    assert_eq!(session.session_id(), "durable-droid-9");
    session.close().await.expect("close");
}

/// `droid daemon --port <n>` — the WebSocket transport end to end. The test
/// hosts the socket itself; the fixture prints the daemon's announce line
/// and stays alive. Server-side frames exercise masking, the Factory
/// envelope discriminant, `daemon.authenticate` secrets, session binding
/// and notification-driven completion.
#[tokio::test]
async fn factory_daemon_websocket_lane() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    // Only the API key resolves — the absent `FACTORY_TOKEN` must drop its
    // leaf instead of failing or shipping a literal placeholder.
    struct TestSecrets;
    #[async_trait::async_trait]
    impl a4aa::SecretResolver for TestSecrets {
        async fn resolve(&self, reference: &str) -> Option<String> {
            (reference == "FACTORY_API_KEY").then(|| "fk-test".into())
        }
    }

    async fn read_frame(socket: &mut tokio::net::TcpStream) -> serde_json::Value {
        let mut head = [0u8; 2];
        socket.read_exact(&mut head).await.unwrap();
        let mut len = (head[1] & 0x7f) as u64;
        if len == 126 {
            let mut ext = [0u8; 2];
            socket.read_exact(&mut ext).await.unwrap();
            len = u16::from_be_bytes(ext) as u64;
        } else if len == 127 {
            let mut ext = [0u8; 8];
            socket.read_exact(&mut ext).await.unwrap();
            len = u64::from_be_bytes(ext);
        }
        assert!(head[1] & 0x80 != 0, "client frames must be masked");
        let mut mask = [0u8; 4];
        socket.read_exact(&mut mask).await.unwrap();
        let mut payload = vec![0u8; len as usize];
        socket.read_exact(&mut payload).await.unwrap();
        for (i, b) in payload.iter_mut().enumerate() {
            *b ^= mask[i % 4];
        }
        serde_json::from_slice(&payload).unwrap()
    }

    async fn write_frame(socket: &mut tokio::net::TcpStream, value: &serde_json::Value) {
        let payload = serde_json::to_vec(value).unwrap();
        let mut out = vec![0x81u8];
        if payload.len() < 126 {
            out.push(payload.len() as u8);
        } else {
            out.push(126);
            out.extend_from_slice(&(payload.len() as u16).to_be_bytes());
        }
        out.extend_from_slice(&payload);
        socket.write_all(&out).await.unwrap();
    }

    let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap();
    let endpoint = listener.local_addr().unwrap();
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::<serde_json::Value>::new()));
    let seen_clone = seen.clone();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut head = Vec::new();
        let mut byte = [0u8; 1];
        while !head.ends_with(b"\r\n\r\n") {
            socket.read_exact(&mut byte).await.unwrap();
            head.push(byte[0]);
        }
        socket
            .write_all(
                b"HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\n\
                  Connection: Upgrade\r\nSec-WebSocket-Accept: a4aa\r\n\r\n",
            )
            .await
            .unwrap();
        const TAGS: &[(&str, &str)] = &[
            ("jsonrpc", "2.0"),
            ("factoryApiVersion", "1.0.0"),
            ("factoryProtocolVersion", "1.269.0"),
        ];
        let respond = |id: serde_json::Value, result: serde_json::Value| {
            let mut frame = serde_json::Map::new();
            for (key, value) in TAGS {
                frame.insert((*key).into(), (*value).into());
            }
            frame.insert("type".into(), "response".into());
            frame.insert("id".into(), id);
            frame.insert("result".into(), result);
            serde_json::Value::Object(frame)
        };
        loop {
            let request = read_frame(&mut socket).await;
            seen_clone.lock().unwrap().push(request.clone());
            let id = request["id"].clone();
            match request["method"].as_str().unwrap_or("") {
                "daemon.authenticate" => {
                    write_frame(
                        &mut socket,
                        &respond(id, serde_json::json!({"userId":"u","orgId":"o"})),
                    )
                    .await;
                }
                "daemon.initialize_session" => {
                    write_frame(
                        &mut socket,
                        &respond(id, serde_json::json!({"sessionId":"droid-d-1"})),
                    )
                    .await;
                }
                "daemon.add_user_message" => {
                    write_frame(&mut socket, &respond(id, serde_json::json!({}))).await;
                    let mut note = serde_json::Map::new();
                    for (key, value) in TAGS {
                        note.insert((*key).into(), (*value).into());
                    }
                    note.insert("type".into(), "notification".into());
                    note.insert("method".into(), "daemon.session_notification".into());
                    note.insert(
                        "params".into(),
                        serde_json::json!({"sessionId":"droid-d-1","notification":{"type":"assistant_text_delta","textDelta":"ws reply"}}),
                    );
                    write_frame(&mut socket, &serde_json::Value::Object(note.clone())).await;
                    note.insert(
                        "params".into(),
                        serde_json::json!({"sessionId":"droid-d-1","notification":{"type":"agent_turn_completed","turnId":"t-9"}}),
                    );
                    write_frame(&mut socket, &serde_json::Value::Object(note)).await;
                }
                "daemon.close_session" => {
                    write_frame(&mut socket, &respond(id, serde_json::json!({}))).await;
                    return;
                }
                _ => {}
            }
        }
    });

    let script = format!("echo '  WebSocket endpoint: ws://{endpoint}'; exec sleep 60");
    let mut spec = a4aa_factory::card()
        .specs()
        .into_iter()
        .nth(3)
        .expect("factory declares the daemon WebSocket alternate");
    spec.command = sh(&script);
    let (tx, mut rx) = mpsc::unbounded_channel();
    let session = Adapter::new(spec)
        .secrets(std::sync::Arc::new(TestSecrets))
        .open_session(session(), tx)
        .await
        .expect("open_session over websocket");
    assert_eq!(session.session_id(), "droid-d-1");

    let outcome = session.prompt(turn("hi over ws")).await.expect("prompt");
    assert_eq!(outcome.stop_reason, "agent_turn_completed");
    assert_eq!(outcome.text, "ws reply");
    session.close().await.expect("close");
    server.await.expect("ws server");

    let seen = seen.lock().unwrap();
    // Order: authenticate → initialize → add_user_message → close.
    let methods: Vec<&str> = seen.iter().filter_map(|f| f["method"].as_str()).collect();
    assert_eq!(
        methods,
        [
            "daemon.authenticate",
            "daemon.initialize_session",
            "daemon.add_user_message",
            "daemon.close_session"
        ]
    );
    let auth = &seen[0]["params"];
    assert_eq!(auth["apiKey"], "fk-test");
    assert!(auth.get("token").is_none(), "unresolved secret must drop");
    assert_eq!(seen[0]["type"], "request");
    assert_eq!(seen[0]["factoryProtocolVersion"], "1.269.0");
    let mut chunks = String::new();
    while let Ok(event) = rx.try_recv() {
        if let AgentEvent::MessageChunk { text } = event {
            chunks.push_str(&text);
        }
    }
    assert_eq!(chunks, "ws reply");
}

/// Codex `exec` batch surface: `thread_id` binds the session,
/// `item.completed` agent_message text is the reply, `turn.completed`
/// carries usage. A resume request reorders argv to
/// `exec resume <id>` — verified as argv, not just intent.
#[tokio::test]
async fn codex_exec_resume_reorders_argv() {
    // The fixture is a real executable: `resume_spawn_args` replaces the
    // command's own args (the `exec` spawn prefix), so a `sh -c` wrapper
    // would lose its script. The file's argv then mirrors codex's exactly.
    let script = "#!/bin/sh\n\
printf '%s\\n' \"$@\" > \"$CODEX_ARGV_CAPTURE\"\n\
printf '{\"type\":\"thread.started\",\"thread_id\":\"codex-t-1\"}\\n'\n\
printf '{\"type\":\"item.completed\",\"item\":{\"type\":\"agent_message\",\"text\":\"argv-ok\"}}\\n'\n\
printf '{\"type\":\"turn.completed\",\"usage\":{\"input_tokens\":5,\"output_tokens\":2}}\\n'\n";
    let dir = std::env::temp_dir();
    let fixture = dir.join(format!("a4aa-codex-fixture-{}", std::process::id()));
    let capture = dir.join(format!("a4aa-codex-argv-{}", std::process::id()));
    std::fs::write(&fixture, script).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&fixture, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let mut spec = a4aa_codex::card()
        .specs()
        .into_iter()
        .nth(1)
        .expect("codex declares the exec alternate");
    spec.command = CommandSpec {
        program: fixture.display().to_string(),
        args: vec!["exec".into()],
        cwd: None,
        env: EnvPolicy::Sanitized {
            inherit: vec![],
            set: std::collections::BTreeMap::from([(
                "CODEX_ARGV_CAPTURE".to_owned(),
                a4aa::EnvValue::Literal(capture.display().to_string()),
            )]),
        },
    };

    let mut request = session();
    request.resume = Some("thread-resume-9".into());
    let (session, _events) = open(spec, request).await;
    let outcome = session.prompt(turn("say it")).await.expect("prompt");
    assert_eq!(outcome.text, "argv-ok");
    let argv = std::fs::read_to_string(&capture).expect("argv capture");
    // `resume_spawn_args` replaced the `exec` prefix; lane args follow.
    assert!(
        argv.starts_with("exec\nresume\nthread-resume-9\n"),
        "unexpected argv: {argv:?}"
    );
    assert!(argv.contains("say it"), "prompt must reach argv: {argv:?}");
    assert!(argv.contains("--json\n"), "lane flags must reach argv");
    let _ = std::fs::remove_file(&capture);
    let _ = std::fs::remove_file(&fixture);
    session.close().await.expect("close");
}
