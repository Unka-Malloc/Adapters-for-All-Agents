//! Deterministic resource and lifecycle regressions. No real agents or network.
#![cfg(unix)]

use a4aa::observe::Direction;
use a4aa::{
    Adapter, CliLaneSpec, CommandSpec, EnvPolicy, LaneSpec, PromptChannel, SessionRequest,
    StreamMode, TurnRequest,
};
use std::sync::Arc;
use std::time::Duration;
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
    let result = tokio::time::timeout(Duration::from_secs(10), prompt)
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

#[tokio::test]
async fn rpc_preserves_text_and_completion_before_acknowledgement() {
    use a4aa::{RpcCallSpec, RpcCompletionSpec, RpcLaneSpec};
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
    let spec = a4aa::AgentSpec {
        id: "rpc-barrier".into(),
        client_name: None,
        max_output_bytes: None,
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
    };
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
        let result = tokio::time::timeout(Duration::from_secs(10), prompt)
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
    let mut spec = a4aa_cursor::spec();
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
    for bypass in ["--trust", "--force", "--approve-mcps"] {
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
