//! Deterministic resource and lifecycle regressions. No real agents or network.
#![cfg(unix)]

#[path = "support/settings.rs"]
mod test_settings;

use a4aa::observe::Direction;
use a4aa::{
    Adapter, CliLaneSpec, CommandSpec, EnvPolicy, LaneSpec, PromptChannel, SessionRequest,
    StreamMode, TurnRequest,
};
use std::sync::Arc;
use tokio::sync::{mpsc, Notify};

fn cli(script: &str, stream: StreamMode, input: bool) -> a4aa::AgentSpec {
    a4aa::AgentSpec {
        id: "fixture".into(),
        client_name: None,
        max_output_bytes: None,
        command: CommandSpec {
            program: "sh".into(),
            args: vec!["-c".into(), script.into(), "sh".into()],
            cwd: None,
            env: EnvPolicy::default(),
        },
        lane: LaneSpec::Cli(Box::new(CliLaneSpec {
            stream,
            args: if input {
                vec![]
            } else {
                vec!["{prompt}".into()]
            },
            prompt_channel: if input {
                PromptChannel::Stdin
            } else {
                PromptChannel::ArgOnly
            },
            ..Default::default()
        })),
    }
}

async fn cancel_at(script: &str, stream: StreamMode, input: bool, direction: Direction) {
    let ready = Arc::new(Notify::new());
    let signal = ready.clone();
    let adapter = Adapter::new(cli(script, stream, input)).observer(Some(Arc::new(
        move |_: &'static str, seen: Direction, _: &[u8]| {
            if seen == direction {
                signal.notify_one();
            }
        },
    )));
    let (tx, _rx) = mpsc::unbounded_channel();
    let session = adapter
        .open_session(SessionRequest::default(), tx)
        .await
        .unwrap();
    let text = if input {
        "x".repeat(8 * 1024 * 1024)
    } else {
        "run".into()
    };
    let mut prompt = std::pin::pin!(session.prompt(TurnRequest {
        text,
        ..Default::default()
    }));
    tokio::select! {
        result = &mut prompt => panic!("finished before the fixture barrier: {result:?}"),
        _ = ready.notified() => {}
    }
    session.cancel().await.unwrap();
    let result = tokio::time::timeout(test_settings::WAIT_BUDGET, prompt)
        .await
        .expect("explicit cancellation did not finish")
        .unwrap();
    assert_eq!(result.stop_reason, "cancelled");
}

#[tokio::test]
async fn cli_cancel_during_backpressured_prompt_write() {
    for stream in [StreamMode::Stdio, StreamMode::Pty] {
        cancel_at("exec sleep 60", stream, true, Direction::Sent).await;
    }
}

#[tokio::test]
async fn cli_cancel_after_output_eof_reaps_the_process() {
    for stream in [StreamMode::Stdio, StreamMode::Pty] {
        cancel_at(
            "exec 0<&- 1>&-; printf ready >&2; exec sleep 60",
            stream,
            false,
            Direction::Stderr,
        )
        .await;
    }
}

#[tokio::test]
async fn cli_idle_cancel_does_not_cancel_the_next_turn() {
    let (tx, _rx) = mpsc::unbounded_channel();
    let session = Adapter::new(cli("printf done", StreamMode::Stdio, false))
        .open_session(SessionRequest::default(), tx)
        .await
        .unwrap();
    session.cancel().await.unwrap();
    let result = session
        .prompt(TurnRequest {
            text: "run".into(),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(result.stop_reason, "end_turn");
    assert_eq!(result.text, "done");
}

#[tokio::test]
async fn cli_bounded_json_drains_oversized_records_and_keeps_control_data() {
    for stream in [StreamMode::Stdio, StreamMode::Pty] {
        let mut spec = cli(
            r#"
printf '{"type":"result","text":"'
head -c 8388608 /dev/zero | tr '\0' x
printf '"}\n{"session":"continued","usage":{"output_tokens":7}}\n'
"#,
            stream,
            false,
        );
        spec.max_output_bytes = Some(1024);
        let LaneSpec::Cli(lane) = &mut spec.lane else {
            unreachable!()
        };
        lane.json_lines = true;
        lane.result_selector = Some("result".into());
        lane.result_path = Some("text".into());
        lane.session_id_path = Some("session".into());
        lane.usage_path = Some("usage".into());
        let (tx, mut rx) = mpsc::unbounded_channel();
        let session = Adapter::new(spec)
            .open_session(SessionRequest::default(), tx)
            .await
            .unwrap();
        let result = session
            .prompt(TurnRequest {
                text: "run".into(),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(result.stop_reason, "end_turn");
        assert!(result.truncated);
        assert!(result.text.len() <= 1024);
        assert_eq!(session.session_id(), "continued");
        assert_eq!(result.usage.unwrap().output, Some(7));
        let mut truncated = 0;
        let mut raw_bytes = 0;
        while let Ok(event) = rx.try_recv() {
            match event {
                a4aa::AgentEvent::Truncated => truncated += 1,
                a4aa::AgentEvent::Raw { raw } => raw_bytes += raw.to_string().len(),
                _ => {}
            }
        }
        assert_eq!(truncated, 1);
        assert!(raw_bytes <= 1024);
    }
}

fn rpc_fixture(script: &str, limit: Option<usize>) -> a4aa::AgentSpec {
    use a4aa::{RpcCallSpec, RpcCompletionSpec, RpcLaneSpec};
    use serde_json::json;
    a4aa::AgentSpec {
        id: "rpc-barrier".into(),
        client_name: None,
        max_output_bytes: limit,
        command: CommandSpec {
            program: "python3".into(),
            args: vec!["-u".into(), "-c".into(), script.into()],
            cwd: None,
            env: EnvPolicy::default(),
        },
        lane: LaneSpec::Rpc(Box::new(RpcLaneSpec {
            prompt: RpcCallSpec {
                method: "prompt".into(),
                params: json!({"text":"{prompt}"}),
            },
            completion: Some(RpcCompletionSpec {
                method: "done".into(),
                turn_id_path: Some("turn".into()),
                status_path: Some("status".into()),
                status_value: None,
            }),
            text_event_method: Some("text".into()),
            text_path: Some("text".into()),
            passthrough: vec!["release".into()],
            ..Default::default()
        })),
    }
}

#[tokio::test]
async fn rpc_preserves_text_and_completion_before_acknowledgement() {
    use serde_json::json;
    let script = r#"
import json,sys
def send(v): print(json.dumps(v),flush=True)
pending=None
for line in sys.stdin:
    m=json.loads(line)
    if m.get('method')=='prompt':
        pending=m['id']
        for i in range(80):
            send({'jsonrpc':'2.0','method':'text','params':{'text':str(i)+','}})
        send({'jsonrpc':'2.0','method':'done','params':{'turn':'t','status':'completed'}})
        for i in range(80):
            send({'jsonrpc':'2.0','method':'noise','params':{'index':i}})
        send({'jsonrpc':'2.0','method':'barrier','params':{}})
    elif m.get('method')=='release':
        send({'jsonrpc':'2.0','id':pending,'result':{'id':'t'}})
        send({'jsonrpc':'2.0','id':m['id'],'result':{}})
"#;
    let spec = rpc_fixture(script, None);
    let (tx, mut rx) = mpsc::unbounded_channel();
    let session = Adapter::new(spec)
        .open_session(SessionRequest::default(), tx)
        .await
        .unwrap();
    for _ in 0..2 {
        let mut prompt = std::pin::pin!(session.prompt(TurnRequest {
            text: "run".into(),
            ..Default::default()
        }));
        // The child withholds its acknowledgement until the actual event
        // delegate has delivered every notification, including the terminal.
        loop {
            tokio::select! {
                result = &mut prompt => panic!("acknowledgement was not released: {result:?}"),
                event = rx.recv() => {
                    if matches!(event,Some(a4aa::AgentEvent::Raw { ref raw }) if raw["method"] == "barrier") { break; }
                }
            }
        }
        let a4aa::LaneSession::Rpc(rpc) = session.lane() else {
            unreachable!()
        };
        rpc.call_passthrough("release", json!({})).await.unwrap();
        let result = tokio::time::timeout(test_settings::WAIT_BUDGET, prompt)
            .await
            .expect("terminal notification was lost")
            .unwrap();
        assert_eq!(result.stop_reason, "completed");
        assert_eq!(
            result.text,
            (0..80).map(|i| format!("{i},")).collect::<String>()
        );
    }
    session.close().await.unwrap();
}

#[tokio::test]
async fn cursor_default_argv_preserves_native_approval_policy() {
    let mut spec = a4aa_cursor::card()
        .specs()
        .into_iter()
        .next()
        .expect("cursor declares the cli lane");
    spec.command.program = "sh".into();
    spec.command.args = vec![
        "-c".into(),
        "if [ \"$1\" = create-chat ]; then printf 'fixture-id\\n'; else printf '%s\\n' \"$@\"; fi"
            .into(),
        "sh".into(),
    ];
    let (tx, _rx) = mpsc::unbounded_channel();
    let session = Adapter::new(spec)
        .open_session(SessionRequest::default(), tx)
        .await
        .unwrap();
    let output = session
        .prompt(TurnRequest {
            text: "inspect".into(),
            ..Default::default()
        })
        .await
        .unwrap();
    let args: Vec<_> = output.text.lines().collect();
    assert!(args.contains(&"--print"));
    // --trust only admits the workspace directory the host dispatched —
    // headless cursor cannot run untrusted — while the permission-bypass
    // flags stay out of the default argv.
    assert!(args.contains(&"--trust"));
    for bypass in ["--force", "--approve-mcps", "--yolo"] {
        assert!(!args.contains(&bypass));
    }
}

#[tokio::test]
async fn cli_open_bounds_binding_and_drains_remaining_output() {
    for oversized in [false, true] {
        let script = if oversized {
            "head -c 8388608 /dev/zero | tr '\\0' x"
        } else {
            "printf 'fixture-id\\n'; head -c 8388608 /dev/zero | tr '\\0' x"
        };
        let mut spec = cli(script, StreamMode::Stdio, false);
        spec.max_output_bytes = Some(64);
        let LaneSpec::Cli(lane) = &mut spec.lane else {
            unreachable!()
        };
        lane.open_args = vec!["create-chat".into()];
        let (tx, _rx) = mpsc::unbounded_channel();
        let opened = Adapter::new(spec)
            .open_session(SessionRequest::default(), tx)
            .await;
        if oversized {
            let error = match opened {
                Ok(_) => panic!("oversized binding accepted"),
                Err(error) => error,
            };
            assert_eq!(error.code, "agent_session_id_too_large");
        } else {
            let session = opened.unwrap();
            assert_eq!(session.session_id(), "fixture-id");
            session.close().await.unwrap();
        }
    }
}

#[tokio::test]
async fn cli_result_text_respects_utf8_output_boundary() {
    let mut spec = cli(
        r#"printf '{"type":"result","text":"中文测试"}\n'"#,
        StreamMode::Stdio,
        false,
    );
    spec.max_output_bytes = Some(4);
    let LaneSpec::Cli(lane) = &mut spec.lane else {
        unreachable!()
    };
    lane.json_lines = true;
    lane.result_selector = Some("result".into());
    lane.result_path = Some("text".into());
    let (tx, _rx) = mpsc::unbounded_channel();
    let session = Adapter::new(spec)
        .open_session(SessionRequest::default(), tx)
        .await
        .unwrap();
    let output = session
        .prompt(TurnRequest {
            text: "run".into(),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(output.text, "中");
    assert!(output.truncated);
}

#[tokio::test]
async fn cli_empty_optional_selectors_preserve_adjacent_flags() {
    for args in [
        vec!["--print", "--model={model}", "{prompt}"],
        vec!["--print", "--{permission}", "{prompt}"],
        vec!["--print", "--model", "{model}", "{prompt}"],
    ] {
        let mut spec = cli("printf '%s\\n' \"$@\"", StreamMode::Stdio, false);
        let LaneSpec::Cli(lane) = &mut spec.lane else {
            unreachable!()
        };
        lane.args = args.into_iter().map(str::to_owned).collect();
        let (tx, _rx) = mpsc::unbounded_channel();
        let session = Adapter::new(spec)
            .open_session(SessionRequest::default(), tx)
            .await
            .unwrap();
        let output = session
            .prompt(TurnRequest {
                text: "run".into(),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(output.text.lines().collect::<Vec<_>>(), ["--print", "run"]);
    }
}

#[tokio::test]
async fn cli_templates_preserve_literal_tokens_in_user_values() {
    let mut spec = cli("printf '%s\\n' \"$@\"", StreamMode::Stdio, false);
    let LaneSpec::Cli(lane) = &mut spec.lane else {
        unreachable!()
    };
    lane.args = vec![
        "{prompt}".into(),
        "mix:{instructions}".into(),
        "id:{sessionId}".into(),
    ];
    let (tx, _rx) = mpsc::unbounded_channel();
    let session = Adapter::new(spec)
        .open_session(
            SessionRequest {
                resume: Some("binding-{provider}".into()),
                config: a4aa::SessionConfig {
                    instructions: Some("keep {sessionId}".into()),
                    ..Default::default()
                },
                ..Default::default()
            },
            tx,
        )
        .await
        .unwrap();
    let input = "explain {provider} \"quoted\"\nnext";
    let result = session
        .prompt(TurnRequest {
            text: input.into(),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(
        result.text,
        format!("{input}\nmix:keep {{sessionId}}\nid:binding-{{provider}}\n")
    );
}

#[tokio::test]
async fn cli_open_rejects_failed_or_empty_session_creation() {
    for (script, code) in [
        ("printf diagnostic; exit 1", "agent_session_open_failed"),
        ("exit 0", "agent_session_id_missing"),
    ] {
        let mut spec = cli(script, StreamMode::Stdio, false);
        let LaneSpec::Cli(lane) = &mut spec.lane else {
            unreachable!()
        };
        lane.open_args = vec!["create-chat".into()];
        let (tx, _rx) = mpsc::unbounded_channel();
        let error = match Adapter::new(spec)
            .open_session(SessionRequest::default(), tx)
            .await
        {
            Ok(_) => panic!("invalid session creation accepted"),
            Err(error) => error,
        };
        assert_eq!(error.code, code);
    }
}

#[tokio::test]
async fn cli_pumps_input_and_output_concurrently_without_replay() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    for (stream, output) in [
        (StreamMode::Stdio, "stdout"),
        (StreamMode::Stdio, "stderr"),
        (StreamMode::Pty, "stderr"),
        (StreamMode::Pty, "stdout"),
    ] {
        // Output-before-input is the synchronization barrier: both payloads
        // exceed pipe capacity, so serial pumping cannot finish naturally.
        let script = format!(
            r#"
import json,sys
sink=sys.{output}.buffer
sink.write(b'x' * (8*1024*1024) + b'\n')
sink.flush()
prompt=sys.stdin.buffer.readline()
assert len(prompt)==1048577, len(prompt)
print(json.dumps({{'type':'result','text':'received:1048576'}}),flush=True)
"#
        );
        let mut spec = cli("", stream, true);
        spec.command.program = "python3".into();
        spec.command.args = vec!["-u".into(), "-c".into(), script];
        spec.max_output_bytes = Some(128);
        let LaneSpec::Cli(lane) = &mut spec.lane else {
            unreachable!()
        };
        lane.json_lines = true;
        lane.result_selector = Some("result".into());
        lane.result_path = Some("text".into());
        let writes = Arc::new(AtomicUsize::new(0));
        let count = writes.clone();
        let adapter = Adapter::new(spec).observer(Some(Arc::new(
            move |_: &'static str, direction, _: &[u8]| {
                if direction == Direction::Sent {
                    count.fetch_add(1, Ordering::Relaxed);
                }
            },
        )));
        let (tx, _rx) = mpsc::unbounded_channel();
        let session = adapter
            .open_session(SessionRequest::default(), tx)
            .await
            .unwrap();
        let result = tokio::time::timeout(
            test_settings::WAIT_BUDGET,
            session.prompt(TurnRequest {
                text: "p".repeat(1024 * 1024),
                ..Default::default()
            }),
        )
        .await
        .expect("bidirectional I/O deadlocked")
        .unwrap();
        assert_eq!(result.stop_reason, "end_turn");
        assert_eq!(result.text, "received:1048576");
        assert_eq!(writes.load(Ordering::Relaxed), 1);
        session.close().await.unwrap();
    }
}

#[tokio::test]
async fn rpc_output_budget_drains_large_turn_without_replay_or_cancel() {
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};
    let script = r#"
import json,sys
def send(v): print(json.dumps(v,ensure_ascii=False),flush=True)
pending=None
for line in sys.stdin:
    m=json.loads(line)
    if m.get('method')=='prompt':
        assert pending is None
        pending=m['id']
        for _ in range(1400):
            send({'jsonrpc':'2.0','method':'text','params':{'text':'中'*1000}})
        for _ in range(10000):
            send({'jsonrpc':'2.0','method':'done','params':{'turn':'t','status':'completed'}})
        send({'jsonrpc':'2.0','method':'barrier','params':{}})
    elif m.get('method')=='release':
        send({'jsonrpc':'2.0','id':pending,'result':{'id':'t'}})
        send({'jsonrpc':'2.0','id':m['id'],'result':{}})
    else:
        raise AssertionError('unexpected request')
"#;
    let ready = Arc::new(Notify::new());
    let signal = ready.clone();
    let prompts = Arc::new(AtomicUsize::new(0));
    let calls = prompts.clone();
    let adapter = Adapter::new(rpc_fixture(script, Some(1024))).observer(Some(Arc::new(
        move |_: &'static str, direction: Direction, bytes: &[u8]| {
            let text = std::str::from_utf8(bytes).unwrap();
            if direction == Direction::Received && text.contains("\"method\":\"barrier\"") {
                signal.notify_one();
            }
            if direction == Direction::Sent && text.contains("\"method\":\"prompt\"") {
                calls.fetch_add(1, Ordering::Relaxed);
            }
        },
    )));
    let (tx, mut rx) = mpsc::unbounded_channel();
    let session = adapter
        .open_session(SessionRequest::default(), tx)
        .await
        .unwrap();
    let mut prompt = std::pin::pin!(session.prompt(TurnRequest {
        text: "run".into(),
        ..Default::default()
    }));
    let mut delivered = 0;
    let mut truncated = 0;
    loop {
        tokio::select! {
            result=&mut prompt=>panic!("ack not released: {result:?}"),
            _=ready.notified()=>break,
            event=rx.recv()=>match event.unwrap() {
                a4aa::AgentEvent::MessageChunk {text}=>delivered+=text.len(),
                a4aa::AgentEvent::Truncated=>truncated+=1,
                _=>{}
            }
        }
    }
    let a4aa::LaneSession::Rpc(rpc) = session.lane() else {
        unreachable!()
    };
    rpc.call_passthrough("release", json!({})).await.unwrap();
    let result = tokio::time::timeout(test_settings::WAIT_BUDGET, prompt)
        .await
        .unwrap()
        .unwrap();
    while let Ok(event) = rx.try_recv() {
        match event {
            a4aa::AgentEvent::MessageChunk { text } => delivered += text.len(),
            a4aa::AgentEvent::Truncated => truncated += 1,
            _ => {}
        }
    }
    assert_eq!(result.text, "中".repeat(341));
    assert_eq!(result.stop_reason, "completed");
    assert!(result.truncated);
    assert_eq!(truncated, 1);
    assert!(delivered <= 1024);
    assert_eq!(prompts.load(Ordering::Relaxed), 1);
}

#[tokio::test]
async fn rpc_cancel_before_ack_uses_real_turn_id_and_idle_cancel_is_harmless() {
    use serde_json::json;
    let script = r#"
import json,sys
def send(v): print(json.dumps(v),flush=True)
pending=None
released=False
for line in sys.stdin:
    m=json.loads(line)
    if m.get('method')=='prompt':
        if m['params']['text']=='after':
            send({'jsonrpc':'2.0','id':m['id'],'result':{'id':'next'}})
            send({'jsonrpc':'2.0','method':'done','params':{'turn':'next','status':'completed'}})
        else:
            pending=m['id']
            send({'jsonrpc':'2.0','method':'ready','params':{}})
    elif m.get('method')=='release':
        released=True
        send({'jsonrpc':'2.0','id':pending,'result':{'id':'real-turn'}})
        send({'jsonrpc':'2.0','id':m['id'],'result':{}})
    elif m.get('method')=='cancel':
        assert released and pending is not None
        assert m['params']['turnId']=='real-turn'
        pending=None
        send({'jsonrpc':'2.0','method':'done','params':{'turn':'real-turn','status':'cancelled'}})
"#;
    let mut spec = rpc_fixture(script, None);
    let LaneSpec::Rpc(lane) = &mut spec.lane else {
        unreachable!()
    };
    lane.cancel = Some(a4aa::RpcCallSpec {
        method: "cancel".into(),
        params: json!({"turnId":"{turnId}"}),
    });
    let (tx, mut rx) = mpsc::unbounded_channel();
    let session = Adapter::new(spec)
        .open_session(SessionRequest::default(), tx)
        .await
        .unwrap();
    {
        let mut prompt = std::pin::pin!(session.prompt(TurnRequest {
            text: "run".into(),
            ..Default::default()
        }));
        loop {
            tokio::select! {
                result=&mut prompt=>panic!("ack not released: {result:?}"),
                event=rx.recv()=>if matches!(event,Some(a4aa::AgentEvent::Raw {ref raw}) if raw["method"]=="ready") {break;}
            }
        }
        session.cancel().await.unwrap();
        let a4aa::LaneSession::Rpc(rpc) = session.lane() else {
            unreachable!()
        };
        rpc.call_passthrough("release", json!({})).await.unwrap();
        let result = tokio::time::timeout(test_settings::WAIT_BUDGET, prompt)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(result.stop_reason, "cancelled");
    }
    session.cancel().await.unwrap();
    let result = session
        .prompt(TurnRequest {
            text: "after".into(),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(result.stop_reason, "completed");
    session.close().await.unwrap();
}

#[tokio::test]
async fn rpc_cancel_during_setup_does_not_dispatch_a_prompt() {
    use serde_json::json;
    let script = r#"
import json,sys
pending=None
for line in sys.stdin:
    m=json.loads(line)
    if m.get('method')=='configure':
        pending=m['id']
        print(json.dumps({'jsonrpc':'2.0','method':'ready','params':{}}),flush=True)
    elif m.get('method')=='release':
        print(json.dumps({'jsonrpc':'2.0','id':pending,'result':{}}),flush=True)
        print(json.dumps({'jsonrpc':'2.0','id':m['id'],'result':{}}),flush=True)
    else:
        raise AssertionError('cancelled setup dispatched work')
"#;
    let mut spec = rpc_fixture(script, None);
    let LaneSpec::Rpc(lane) = &mut spec.lane else {
        unreachable!()
    };
    lane.pre_turn = vec![a4aa::RpcCallSpec {
        method: "configure".into(),
        params: json!({}),
    }];
    lane.cancel = Some(a4aa::RpcCallSpec {
        method: "cancel".into(),
        params: json!({"turnId":"{turnId}"}),
    });
    let (tx, mut rx) = mpsc::unbounded_channel();
    let session = Adapter::new(spec)
        .open_session(SessionRequest::default(), tx)
        .await
        .unwrap();
    let mut prompt = std::pin::pin!(session.prompt(TurnRequest {
        text: "run".into(),
        ..Default::default()
    }));
    loop {
        tokio::select! {
            result=&mut prompt=>panic!("setup not released: {result:?}"),
            event=rx.recv()=>if matches!(event,Some(a4aa::AgentEvent::Raw {ref raw}) if raw["method"]=="ready") {break;}
        }
    }
    session.cancel().await.unwrap();
    let a4aa::LaneSession::Rpc(rpc) = session.lane() else {
        unreachable!()
    };
    rpc.call_passthrough("release", json!({})).await.unwrap();
    assert_eq!(prompt.await.unwrap().stop_reason, "cancelled");
}

#[tokio::test]
async fn rpc_output_budget_preserves_explicit_process_cancellation() {
    let script = r#"
import json,sys
def send(v): print(json.dumps(v),flush=True)
for line in sys.stdin:
    m=json.loads(line)
    send({'jsonrpc':'2.0','id':m['id'],'result':{'id':'active'}})
    for _ in range(1000):
        send({'jsonrpc':'2.0','method':'text','params':{'text':'x'*4096}})
"#;
    let mut spec = rpc_fixture(script, Some(1024));
    let LaneSpec::Rpc(lane) = &mut spec.lane else {
        unreachable!()
    };
    lane.kill_on_cancel = true;
    let (tx, mut rx) = mpsc::unbounded_channel();
    let session = Adapter::new(spec)
        .open_session(SessionRequest::default(), tx)
        .await
        .unwrap();
    {
        let mut prompt = std::pin::pin!(session.prompt(TurnRequest {
            text: "run".into(),
            ..Default::default()
        }));
        loop {
            tokio::select! {
                result=&mut prompt=>panic!("fixture must wait for cancel: {result:?}"),
                event=rx.recv()=>if matches!(event,Some(a4aa::AgentEvent::Truncated)) {break;}
            }
        }
        session.cancel().await.unwrap();
        let result = tokio::time::timeout(test_settings::WAIT_BUDGET, prompt)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(result.stop_reason, "cancelled");
        assert!(result.truncated);
        assert!(result.text.len() <= 1024);
    }
    session.cancel().await.unwrap();
    assert!(session
        .prompt(TurnRequest {
            text: "after".into(),
            ..Default::default()
        })
        .await
        .is_err());
    session.close().await.unwrap();
}

fn receipt_fixture(mode: &str, shape: a4aa::BindingReceiptShape) -> a4aa::AgentSpec {
    let script = r#"
import json,os,pathlib,sys
sys.stdin.readline()
p=pathlib.Path(os.environ['A4AA_TEST_RECEIPT'])
mode=sys.argv[1]
payload={'conversationId':'receipt-session','usage':{'input_tokens':12,'output_tokens':3}}
if mode=='malformed': p.write_text('{broken')
elif mode=='nested-bad': p.write_text(json.dumps({'hookPayload':'broken'}))
elif mode=='unreadable': p.mkdir()
elif mode=='oversized': p.write_bytes(b'x'*(65*1024*1024))
elif mode=='no-id': p.write_text('{}')
elif mode!='missing':
    if mode=='nested': payload={'hookPayload':json.dumps(payload)}
    p.write_text(json.dumps(payload))
print(json.dumps({'receipt':str(p),'resumed':sys.argv[2] if len(sys.argv)>2 else ''}),flush=True)
"#;
    let mut spec = cli("", StreamMode::Stdio, true);
    spec.command.program = "python3".into();
    spec.command.args = vec!["-u".into(), "-c".into(), script.into(), mode.into()];
    let LaneSpec::Cli(lane) = &mut spec.lane else {
        unreachable!()
    };
    lane.binding_file_env = Some("A4AA_TEST_RECEIPT".into());
    lane.binding_receipt_shape = shape;
    lane.resume_args = vec!["{sessionId}".into()];
    spec
}

#[tokio::test]
async fn cli_receipts_supply_binding_resume_and_optional_usage() {
    use a4aa::BindingReceiptShape::{SessionId, SessionIdAndUsage};
    for mode in ["top", "nested"] {
        for shape in [SessionId, SessionIdAndUsage] {
            let (tx, _rx) = mpsc::unbounded_channel();
            let session = Adapter::new(receipt_fixture(mode, shape))
                .open_session(SessionRequest::default(), tx)
                .await
                .unwrap();
            for expected in ["", "receipt-session"] {
                let result = session
                    .prompt(TurnRequest {
                        text: "run".into(),
                        ..Default::default()
                    })
                    .await
                    .unwrap();
                let output: serde_json::Value = serde_json::from_str(&result.text).unwrap();
                assert_eq!(output["resumed"], expected);
                assert!(!std::path::Path::new(output["receipt"].as_str().unwrap()).exists());
                assert_eq!(session.session_id(), "receipt-session");
                if shape == SessionIdAndUsage {
                    let usage = result.usage.unwrap();
                    assert_eq!(usage.input, Some(12));
                    assert_eq!(usage.output, Some(3));
                    assert_eq!(usage.total, None);
                } else {
                    assert!(result.usage.is_none());
                }
            }
            session.close().await.unwrap();
        }
    }
}

#[tokio::test]
async fn cli_receipt_failures_are_visible_and_keep_evidence() {
    for (mode, code) in [
        ("malformed", "agent_receipt_invalid"),
        ("nested-bad", "agent_receipt_invalid"),
        ("unreadable", "agent_receipt_read_failed"),
        ("oversized", "agent_receipt_too_large"),
        ("no-id", "agent_receipt_session_id_missing"),
    ] {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let session = Adapter::new(receipt_fixture(
            mode,
            a4aa::BindingReceiptShape::SessionIdAndUsage,
        ))
        .open_session(SessionRequest::default(), tx)
        .await
        .unwrap();
        let error = match session
            .prompt(TurnRequest {
                text: "run".into(),
                ..Default::default()
            })
            .await
        {
            Ok(_) => panic!("invalid receipt accepted"),
            Err(error) => error,
        };
        assert_eq!(error.code, code);
        assert!(error.uncertain_effect);
        assert!(!error.retryable);
        let mut text = String::new();
        while let Ok(event) = rx.try_recv() {
            if let a4aa::AgentEvent::MessageChunk { text: chunk } = event {
                text.push_str(&chunk);
            }
        }
        let output: serde_json::Value = serde_json::from_str(&text).unwrap();
        let path = std::path::Path::new(output["receipt"].as_str().unwrap());
        assert!(path.exists());
        assert_eq!(session.session_id(), "");
        let directory = path.parent().unwrap();
        assert!(error
            .detail
            .as_ref()
            .unwrap()
            .contains(directory.to_str().unwrap()));
        assert!(directory.starts_with(std::env::temp_dir()));
        assert!(directory
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .starts_with("a4aa-receipt-"));
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(directory).unwrap().permissions().mode() & 0o777,
            0o700
        );
        std::fs::remove_dir_all(directory).unwrap();
        session.close().await.unwrap();
    }
}

#[tokio::test]
async fn cli_missing_optional_receipt_is_allowed() {
    let (tx, _rx) = mpsc::unbounded_channel();
    let session = Adapter::new(receipt_fixture(
        "missing",
        a4aa::BindingReceiptShape::SessionId,
    ))
    .open_session(SessionRequest::default(), tx)
    .await
    .unwrap();
    let result = session
        .prompt(TurnRequest {
            text: "run".into(),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(result.stop_reason, "end_turn");
    assert_eq!(session.session_id(), "");
    session.close().await.unwrap();
}

#[tokio::test]
async fn rpc_correlated_completion_rejects_missing_or_empty_ack_id() {
    for ack in ["{}", r#"{"turn":{"id":""}}"#, r#"{"turn":{"id":"   "}}"#] {
        let script = format!(
            r#"
import json,sys
for line in sys.stdin:
    m=json.loads(line)
    print(json.dumps({{'jsonrpc':'2.0','method':'done','params':{{'turn':'stale','status':'completed'}}}}),flush=True)
    print(json.dumps({{'jsonrpc':'2.0','id':m['id'],'result':json.loads('{ack}')}}),flush=True)
"#
        );
        let (tx, _rx) = mpsc::unbounded_channel();
        let session = Adapter::new(rpc_fixture(&script, Some(1024)))
            .open_session(SessionRequest::default(), tx)
            .await
            .unwrap();
        let error = session
            .prompt(TurnRequest {
                text: "run".into(),
                ..Default::default()
            })
            .await
            .unwrap_err();
        assert_eq!(error.code, "agent_turn_id_missing");
        assert!(error.uncertain_effect);
        session.close().await.unwrap();
    }
}

#[tokio::test]
async fn rpc_process_cancel_terminates_a_hung_pre_turn_call() {
    let script = r#"
import json,sys
for line in sys.stdin:
    m=json.loads(line)
    assert m['method']=='configure', 'prompt was dispatched after cancel'
    print(json.dumps({'jsonrpc':'2.0','method':'ready','params':{}}),flush=True)
"#;
    let mut spec = rpc_fixture(script, Some(1024));
    let LaneSpec::Rpc(lane) = &mut spec.lane else {
        unreachable!()
    };
    lane.pre_turn = vec![a4aa::RpcCallSpec {
        method: "configure".into(),
        params: serde_json::json!({}),
    }];
    lane.kill_on_cancel = true;
    let (tx, mut rx) = mpsc::unbounded_channel();
    let session = Adapter::new(spec)
        .open_session(SessionRequest::default(), tx)
        .await
        .unwrap();
    let mut prompt = std::pin::pin!(session.prompt(TurnRequest {
        text: "run".into(),
        ..Default::default()
    }));
    loop {
        tokio::select! {
            result = &mut prompt => panic!("setup ended before cancellation: {result:?}"),
            event = rx.recv() => if matches!(event, Some(a4aa::AgentEvent::Raw { ref raw }) if raw["method"]=="ready") { break; }
        }
    }
    session.cancel().await.unwrap();
    assert_eq!(
        tokio::time::timeout(test_settings::WAIT_BUDGET, prompt)
            .await
            .unwrap()
            .unwrap()
            .stop_reason,
        "cancelled"
    );
}

#[tokio::test]
async fn cli_resume_is_rejected_before_open_spawns_or_reports_a_binding() {
    let mut spec = cli("exit 99", StreamMode::Stdio, false);
    spec.command.program = "missing-fixture-executable".into();
    let LaneSpec::Cli(lane) = &mut spec.lane else {
        unreachable!()
    };
    lane.open_args = vec!["create-chat".into()];
    let adapter = Adapter::new(spec);
    assert!(!adapter.capabilities().session_resume);
    let (tx, _rx) = mpsc::unbounded_channel();
    let result = adapter
        .open_session(
            SessionRequest {
                resume: Some("existing".into()),
                ..Default::default()
            },
            tx,
        )
        .await;
    let Err(error) = result else {
        panic!("unsupported resume was accepted")
    };
    assert_eq!(error.code, "agent_session_resume_unsupported");
    assert_eq!(error.stage, "session/open");
}

#[tokio::test]
async fn claude_permission_bypass_requires_an_explicit_selection() {
    assert_eq!(a4aa_claude::card().features.permission_default, None);
    for permission in [None, Some("bypassPermissions")] {
        let mut spec = a4aa_claude::spec();
        spec.command = CommandSpec { program: "python3".into(), args: vec!["-c".into(),
            "import sys,json;sys.stdin.readline();print(json.dumps({'type':'result','result':json.dumps(sys.argv[1:])}))".into()],
            cwd: None, env: EnvPolicy::default() };
        let (tx, _rx) = mpsc::unbounded_channel();
        let session = Adapter::new(spec)
            .open_session(
                SessionRequest {
                    config: a4aa::SessionConfig {
                        permission: permission.map(str::to_owned),
                        ..Default::default()
                    },
                    ..Default::default()
                },
                tx,
            )
            .await
            .unwrap();
        let outcome = session
            .prompt(TurnRequest {
                text: "run".into(),
                ..Default::default()
            })
            .await
            .unwrap();
        let args: Vec<String> = serde_json::from_str(&outcome.text).unwrap();
        assert_eq!(
            args.iter().any(|arg| arg == "bypassPermissions"),
            permission.is_some()
        );
    }
}

#[tokio::test]
async fn rpc_session_instructions_reach_setup_and_prompt_templates() {
    let script = r#"
import json,sys
for line in sys.stdin:
    m=json.loads(line)
    assert m['params']['instructions']=='ROLE {prompt}', 'instructions were lost or expanded twice'
    if m['method']=='configure': result={}
    else: result={'text':m['params']['instructions']+'|'+m['params']['text']}
    print(json.dumps({'jsonrpc':'2.0','id':m['id'],'result':result}),flush=True)
"#;
    let mut spec = rpc_fixture(script, Some(1024));
    let LaneSpec::Rpc(lane) = &mut spec.lane else {
        unreachable!()
    };
    lane.completion = None;
    lane.result_text_path = Some("text".into());
    lane.prompt.params = serde_json::json!({"instructions":"{instructions}","text":"{prompt}"});
    lane.pre_turn = vec![a4aa::RpcCallSpec {
        method: "configure".into(),
        params: serde_json::json!({"instructions":"{instructions}"}),
    }];
    let (tx, _rx) = mpsc::unbounded_channel();
    let session = Adapter::new(spec)
        .open_session(
            SessionRequest {
                config: a4aa::SessionConfig {
                    instructions: Some("ROLE {prompt}".into()),
                    ..Default::default()
                },
                ..Default::default()
            },
            tx,
        )
        .await
        .unwrap();
    for text in ["first", "", "next"] {
        assert_eq!(
            session
                .prompt(TurnRequest {
                    text: text.into(),
                    ..Default::default()
                })
                .await
                .unwrap()
                .text,
            format!("ROLE {{prompt}}|{text}")
        );
    }
    session.close().await.unwrap();
}

#[tokio::test]
async fn rpc_dropped_waiter_preserves_the_active_turn_and_cancel_target() {
    tokio::time::timeout(test_settings::WAIT_BUDGET, async {
        let script = r#"
import json,sys
active=None
prompts=0
cancels=0
def send(m): print(json.dumps(m),flush=True)
for line in sys.stdin:
    m=json.loads(line)
    if m['method']=='prompt':
        assert active is None, 'new turn overlapped an abandoned waiter'
        prompts+=1; active='t-'+str(prompts)
        send({'jsonrpc':'2.0','id':m['id'],'result':{'id':active}})
        send({'jsonrpc':'2.0','method':'text','params':{'text':'started' if prompts==1 else 'next'}})
        if prompts==2:
            assert cancels==1
            send({'jsonrpc':'2.0','method':'done','params':{'turn':active,'status':'completed'}})
            active=None
    elif m['method']=='release':
        send({'jsonrpc':'2.0','id':m['id'],'result':{'active':active,'prompts':prompts,'cancels':cancels}})
    elif m['method']=='cancel':
        assert active=='t-1' and m['params']['turnId']==active
        cancels+=1
        send({'jsonrpc':'2.0','method':'done','params':{'turn':active,'status':'cancelled'}})
        active=None
    else: raise AssertionError(m)
"#;
        let mut spec=rpc_fixture(script,Some(1024));
        let LaneSpec::Rpc(lane)=&mut spec.lane else { unreachable!() };
        lane.cancel=Some(a4aa::RpcCallSpec { method:"cancel".into(),params:serde_json::json!({"turnId":"{turnId}"}) });
        let (tx,mut rx)=mpsc::unbounded_channel();
        let session=Adapter::new(spec).open_session(SessionRequest::default(),tx).await.unwrap();
        {
            let first=session.prompt(TurnRequest {text:"first".into(),..Default::default()});
            tokio::pin!(first);
            tokio::select! {
                result=&mut first=>panic!("first turn ended before explicit cancellation: {result:?}"),
                event=rx.recv()=>assert_eq!(event.unwrap(),a4aa::AgentEvent::MessageChunk {text:"started".into()})
            }
        } // Abandon only the waiter, without cancelling the agent.
        let a4aa::LaneSession::Rpc(rpc)=session.lane() else {unreachable!()};
        assert_eq!(rpc.call_passthrough("release",serde_json::json!({})).await.unwrap(),
            serde_json::json!({"active":"t-1","prompts":1,"cancels":0}));
        let second=session.prompt(TurnRequest {text:"second".into(),..Default::default()});
        tokio::pin!(second);
        tokio::select! {biased;
            result=&mut second=>panic!("second turn overtook the active first turn: {result:?}"),
            _=std::future::ready(())=>{}
        }
        session.cancel().await.unwrap();
        let outcome=second.await.unwrap();
        assert_eq!(outcome.stop_reason,"completed");
        assert_eq!(outcome.text,"next");
    }).await.expect("detached RPC turn did not preserve lifecycle progress");
}

#[tokio::test]
async fn cli_empty_prompt_keeps_headless_argv_and_rejects_empty_model() {
    for model in [None, Some("")] {
        let mut spec = cli("", StreamMode::Stdio, false);
        spec.command = CommandSpec {
            program: "python3".into(),
            args: vec![
                "-c".into(),
                "import json,sys;print(json.dumps(sys.argv[1:]))".into(),
            ],
            cwd: None,
            env: EnvPolicy::default(),
        };
        let LaneSpec::Cli(lane) = &mut spec.lane else {
            unreachable!()
        };
        lane.args = vec![
            "--print".into(),
            "{prompt}".into(),
            "--model".into(),
            "{model}".into(),
        ];
        let (tx, _rx) = mpsc::unbounded_channel();
        let session = Adapter::new(spec)
            .open_session(SessionRequest::default(), tx)
            .await
            .unwrap();
        let result = session
            .prompt(TurnRequest {
                text: String::new(),
                model: model.map(str::to_owned),
                effort: None,
            })
            .await;
        if model.is_some() {
            // The existing public selector contract rejects a blank model
            // before launching work; it is not treated as an omitted model.
            assert_eq!(result.unwrap_err().code, "agent_model_unsupported");
        } else {
            let args: Vec<String> = serde_json::from_str(&result.unwrap().text).unwrap();
            assert_eq!(args, vec!["--print", ""]);
        }
    }
}

#[tokio::test]
async fn optional_selector_never_removes_a_hyphenated_prompt() {
    let mut spec = cli("", StreamMode::Stdio, false);
    spec.command = CommandSpec {
        program: "python3".into(),
        args: vec![
            "-c".into(),
            "import json,sys;print(json.dumps(sys.argv[1:]))".into(),
        ],
        cwd: None,
        env: EnvPolicy::default(),
    };
    let LaneSpec::Cli(lane) = &mut spec.lane else {
        unreachable!()
    };
    lane.args = vec!["--print".into(), "{prompt}".into(), "{model}".into()];
    let (tx, _rx) = mpsc::unbounded_channel();
    let session = Adapter::new(spec)
        .open_session(SessionRequest::default(), tx)
        .await
        .unwrap();
    let result = session
        .prompt(TurnRequest {
            text: "--literal-user-text".into(),
            ..Default::default()
        })
        .await
        .unwrap();
    let args: Vec<String> = serde_json::from_str(&result.text).unwrap();
    assert_eq!(args, vec!["--print", "--literal-user-text"]);
}

#[tokio::test]
async fn cancellation_cleans_descendants_after_the_agent_leader_exits() {
    for mode in ["stdio", "pty", "rpc"] {
        tokio::time::timeout(test_settings::WAIT_BUDGET, async {
            let mut changes=tokio::signal::unix::signal(tokio::signal::unix::SignalKind::child()).unwrap();
            let script=format!("rpc={}\n{}",if mode=="rpc" {"True"}else{"False"},r#"
import os,sys,json,signal
request=json.loads(sys.stdin.readline()) if rpc else None
leader=os.getpid()
if os.fork(): os._exit(23)
signal.signal(signal.SIGINT,signal.SIG_IGN)
payload=json.dumps({'leader':leader,'descendant':os.getpid()})
if rpc:
    print(json.dumps({'jsonrpc':'2.0','id':request['id'],'result':{'id':'t'}}),flush=True)
    print(json.dumps({'jsonrpc':'2.0','method':'text','params':{'text':payload}}),flush=True)
else: print(payload,flush=True)
signal.pause()
"#);
            let spec=if mode=="rpc" {
                let mut spec=rpc_fixture(&script,Some(1024));
                let LaneSpec::Rpc(lane)=&mut spec.lane else {unreachable!()};
                lane.kill_on_cancel=true;
                spec
            } else {
                let mut spec=cli("",if mode=="pty" {StreamMode::Pty}else{StreamMode::Stdio},false);
                spec.command=CommandSpec {program:"python3".into(),args:vec!["-u".into(),"-c".into(),script],cwd:None,env:EnvPolicy::default()};
                spec
            };
            let (tx,mut rx)=mpsc::unbounded_channel();
            let session=Adapter::new(spec).open_session(SessionRequest::default(),tx).await.unwrap();
            let mut prompt=std::pin::pin!(session.prompt(TurnRequest {text:"run".into(),..Default::default()}));
            let mut text=String::new();
            let identity:serde_json::Value=loop {
                tokio::select! {
                    result=&mut prompt=>panic!("fixture ended before explicit cancel: {result:?}"),
                    event=rx.recv()=>if let Some(a4aa::AgentEvent::MessageChunk {text:chunk})=event {
                        text.push_str(&chunk);
                        if let Ok(value)=serde_json::from_str(&text) {break value;}
                    }
                }
            };
            let leader=identity["leader"].as_u64().unwrap() as libc::id_t;
            let descendant=identity["descendant"].as_i64().unwrap() as libc::pid_t;
            loop {
                let mut info:libc::siginfo_t=unsafe {std::mem::zeroed()};
                assert_eq!(unsafe {libc::waitid(libc::P_PID,leader,&mut info,libc::WEXITED|libc::WNOHANG|libc::WNOWAIT)},0);
                if info.si_signo!=0 {break;}
                changes.recv().await.unwrap();
            }
            session.cancel().await.unwrap();
            assert_eq!(prompt.await.unwrap().stop_reason,"cancelled","{mode}");
            let reaped=tokio::time::timeout(test_settings::WAIT_BUDGET,async {
                while unsafe {libc::kill(descendant,0)}==0 {
                    tokio::time::sleep(test_settings::REAP_POLL_INTERVAL).await;
                }
                assert_eq!(std::io::Error::last_os_error().raw_os_error(),Some(libc::ESRCH));
            }).await;
            if reaped.is_err() {unsafe {libc::kill(descendant,libc::SIGKILL);}}
            reaped.expect("descendant remained alive after successful cancellation");
        }).await.expect("exited-leader cancellation stalled");
    }
}

#[tokio::test]
async fn cli_signal_death_is_an_error_never_end_turn() {
    for stream in [StreamMode::Stdio, StreamMode::Pty] {
        let (tx, _rx) = mpsc::unbounded_channel();
        // Partial output, then death by SIGTERM: `ExitStatus::code()` is
        // `None`, and a missing code must never read as success.
        let session = Adapter::new(cli("printf partial; kill -TERM $$", stream, false))
            .open_session(SessionRequest::default(), tx)
            .await
            .unwrap();
        let outcome = session
            .prompt(TurnRequest {
                text: "run".into(),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(outcome.stop_reason, "error", "{stream:?}");
        assert_eq!(outcome.exit_code, None, "{stream:?}");
        assert_eq!(outcome.text, "partial", "{stream:?}");
        session.close().await.unwrap();
    }
}

#[tokio::test]
async fn cli_explicit_cancel_still_reports_cancelled_not_signal_error() {
    for stream in [StreamMode::Stdio, StreamMode::Pty] {
        // Termination of the process tree from an explicit cancel keeps the
        // `cancelled` outcome — the signal-driven exit status must not leak
        // into `stop_reason`.
        cancel_at(
            "printf ready >&2; exec sleep 60",
            stream,
            false,
            Direction::Stderr,
        )
        .await;
    }
}

#[tokio::test]
async fn rpc_writer_failure_settles_the_turn_and_requires_recovery() {
    use serde_json::json;
    tokio::time::timeout(test_settings::WAIT_BUDGET, async {
        // The agent answers session/new only after closing its own stdin,
        // so the following prompt write deterministically hits a broken
        // pipe while the reader keeps waiting on an open, silent stdout.
        let script = r#"
import json,sys,os,signal
line=sys.stdin.readline()
m=json.loads(line)
os.close(0)
print(json.dumps({'jsonrpc':'2.0','id':m['id'],'result':{'sessionId':'s'}}),flush=True)
signal.pause()
"#;
        let mut spec = rpc_fixture(script, None);
        let LaneSpec::Rpc(lane) = &mut spec.lane else {
            unreachable!()
        };
        lane.session_open = Some(a4aa::RpcCallSpec {
            method: "session/new".into(),
            params: json!({}),
        });
        lane.completion = None;
        let (tx, _rx) = mpsc::unbounded_channel();
        let session = Adapter::new(spec)
            .open_session(SessionRequest::default(), tx)
            .await
            .unwrap();
        let error = session
            .prompt(TurnRequest {
                text: "run".into(),
                ..Default::default()
            })
            .await
            .unwrap_err();
        assert_eq!(error.code, "agent_transport_failed");
        assert!(error.uncertain_effect);
        // The dispatched turn keeps its recovery-required isolation: the
        // next prompt is refused rather than re-billed on a dead transport.
        let again = session
            .prompt(TurnRequest {
                text: "again".into(),
                ..Default::default()
            })
            .await
            .unwrap_err();
        assert_eq!(again.code, "agent_session_recovery_required");
        session.close().await.unwrap();
    })
    .await
    .expect("writer failure did not settle the dispatched turn");
}

// ----------------- owned CLI turns (waiterless dispatch) -----------------

/// A stdin-prompted CLI spec whose agent receives `env` literals — used by
/// the waiterless-turn tests to make the agent's pid observable. `sh` keeps
/// these tests light: they run inside a heavily parallel suite.
fn pid_reporting_cli(script: &str, env: &[(&str, &std::path::Path)]) -> a4aa::AgentSpec {
    let mut spec = cli(script, StreamMode::Stdio, true);
    spec.command.env = EnvPolicy::Sanitized {
        inherit: vec![],
        set: env
            .iter()
            .map(|(key, path)| {
                (
                    key.to_string(),
                    a4aa::EnvValue::Literal(path.display().to_string()),
                )
            })
            .collect(),
    };
    spec
}

/// Drive the prompt until the agent's `started` chunk arrives, then drop
/// the waiter: the turn is now owned by the session alone.
async fn start_waiterless_turn(
    session: &a4aa::AgentSession,
    rx: &mut mpsc::UnboundedReceiver<a4aa::AgentEvent>,
) {
    {
        let mut prompt = std::pin::pin!(session.prompt(TurnRequest {
            text: "one".into(),
            ..Default::default()
        }));
        loop {
            tokio::select! {
                result = &mut prompt => panic!("turn finished before the barrier: {result:?}"),
                event = rx.recv() => {
                    if matches!(event, Some(a4aa::AgentEvent::MessageChunk { ref text }) if text.contains("started")) {
                        break;
                    }
                }
            }
        }
    }
}

fn pid_of(pidfile: &std::path::Path) -> libc::pid_t {
    std::fs::read_to_string(pidfile)
        .unwrap()
        .trim()
        .parse()
        .unwrap()
}

#[tokio::test]
async fn cli_dropped_waiter_leaves_the_owned_turn_to_complete() {
    tokio::time::timeout(test_settings::WAIT_BUDGET * 3, async {
        let directory = tempfile::tempdir().unwrap();
        let pidfile = directory.path().join("turn.pid");
        let script = r#"
printf '%s' $$ > "$A4AA_TEST_PID"
read line
echo started
sleep 1
echo finished
"#;
        let (tx, mut rx) = mpsc::unbounded_channel();
        let session = Adapter::new(pid_reporting_cli(script, &[("A4AA_TEST_PID", &pidfile)]))
            .open_session(SessionRequest::default(), tx)
            .await
            .unwrap();
        start_waiterless_turn(&session, &mut rx).await;
        let pid = pid_of(&pidfile);
        // The detached turn keeps streaming to the session's event sink and
        // runs to real completion without its waiter.
        loop {
            match rx.recv().await {
                Some(a4aa::AgentEvent::MessageChunk { text }) if text.contains("finished") => {
                    break;
                }
                Some(_) => {}
                other => panic!("detached turn did not complete: {other:?}"),
            }
        }
        // The turn driver awaited natural exit and reaped the child before
        // releasing serial admission, so once the next prompt completes the
        // first child is long gone.
        let second = session
            .prompt(TurnRequest {
                text: "two".into(),
                ..Default::default()
            })
            .await
            .expect("a cleanly completed detached turn must not poison the session");
        assert_eq!(second.stop_reason, "end_turn");
        assert!(second.text.contains("finished"));
        assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ESRCH)
        );
        session.close().await.unwrap();
    })
    .await
    .expect("detached CLI turn died with its waiter");
}

#[tokio::test]
async fn cli_cancel_terminates_a_waiterless_owned_turn() {
    tokio::time::timeout(test_settings::WAIT_BUDGET * 3, async {
        let directory = tempfile::tempdir().unwrap();
        let pidfile = directory.path().join("turn.pid");
        let once = directory.path().join("turn-once");
        // The first run pauses until cancelled; a later turn exits normally.
        // The marker is touched BEFORE the barrier-visible echo, so a turn
        // observed as started has always registered itself.
        let script = r#"
printf '%s' $$ > "$A4AA_TEST_PID"
read line
if [ -f "$A4AA_TEST_ONCE" ]; then
  echo started
  echo finished
else
  touch "$A4AA_TEST_ONCE"
  echo started
  exec sleep 60
fi
"#;
        let (tx, mut rx) = mpsc::unbounded_channel();
        let session = Adapter::new(pid_reporting_cli(
            script,
            &[("A4AA_TEST_PID", &pidfile), ("A4AA_TEST_ONCE", &once)],
        ))
        .open_session(SessionRequest::default(), tx)
        .await
        .unwrap();
        start_waiterless_turn(&session, &mut rx).await;
        let pid = pid_of(&pidfile);
        session.cancel().await.unwrap();
        // The cancel reaches the owned turn; the next prompt completing
        // proves the first turn's task terminated and reaped the process.
        let second = session
            .prompt(TurnRequest {
                text: "two".into(),
                ..Default::default()
            })
            .await
            .expect("an explicit cancel is a known outcome, not poisoning");
        assert_eq!(second.stop_reason, "end_turn");
        assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ESRCH)
        );
        session.close().await.unwrap();
    })
    .await
    .expect("cancel did not reach the owned turn");
}

#[tokio::test]
async fn cli_close_terminates_and_reaps_a_waiterless_turn() {
    tokio::time::timeout(test_settings::WAIT_BUDGET * 3, async {
        let directory = tempfile::tempdir().unwrap();
        let pidfile = directory.path().join("turn.pid");
        let script = r#"
printf '%s' $$ > "$A4AA_TEST_PID"
read line
echo started
exec sleep 60
"#;
        let (tx, mut rx) = mpsc::unbounded_channel();
        let session = Adapter::new(pid_reporting_cli(script, &[("A4AA_TEST_PID", &pidfile)]))
            .open_session(SessionRequest::default(), tx)
            .await
            .unwrap();
        start_waiterless_turn(&session, &mut rx).await;
        let pid = pid_of(&pidfile);
        // close() waits for the owned turn (cancelled via the watch) and
        // reaps it before returning.
        session.close().await.unwrap();
        assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ESRCH)
        );
        let error = session
            .prompt(TurnRequest {
                text: "after".into(),
                ..Default::default()
            })
            .await
            .unwrap_err();
        assert_eq!(error.code, "agent_session_closed");
    })
    .await
    .expect("close did not terminate the owned turn");
}

#[tokio::test]
async fn cli_waiterless_turn_with_uncertain_outcome_requires_recovery() {
    tokio::time::timeout(test_settings::WAIT_BUDGET * 3, async {
        // The malformed receipt fails the completed turn with an uncertain
        // receipt error after the agent exits — deterministic, no races.
        let spec = receipt_fixture("malformed", a4aa::BindingReceiptShape::SessionId);
        let (tx, mut rx) = mpsc::unbounded_channel();
        let session = Adapter::new(spec)
            .open_session(SessionRequest::default(), tx)
            .await
            .unwrap();
        {
            let mut prompt = std::pin::pin!(session.prompt(TurnRequest {
                text: "one".into(),
                ..Default::default()
            }));
            loop {
                tokio::select! {
                    result = &mut prompt => panic!("turn finished before the barrier: {result:?}"),
                    event = rx.recv() => {
                        // The agent prints its receipt JSON after reading the
                        // prompt — the turn is in flight when we detach.
                        if matches!(event, Some(a4aa::AgentEvent::MessageChunk { .. })) {
                            break;
                        }
                    }
                }
            }
        }
        let error = session
            .prompt(TurnRequest {
                text: "two".into(),
                ..Default::default()
            })
            .await
            .unwrap_err();
        assert_eq!(error.code, "agent_session_recovery_required");
        session.close().await.unwrap();
    })
    .await
    .expect("uncertain detached turn did not poison the next prompt");
}
