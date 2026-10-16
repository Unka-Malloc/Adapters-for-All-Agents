//! Deterministic ACP lifecycle and resource regressions; no real agents.
#![cfg(unix)]

#[path = "support/settings.rs"]
mod test_settings;

use a4aa::{Adapter, AgentEvent, AgentSpec, CommandSpec, LaneSpec, SessionRequest, TurnRequest};
use tokio::sync::mpsc;

#[tokio::test]
async fn host_callbacks_can_close_their_own_session() {
    use a4aa::proto::acp::{
        AcpLane, AcpSession, PermissionDecision, PermissionPrompt, SessionDelegate,
    };
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, OnceLock};
    struct Closer {
        session: OnceLock<Arc<AcpSession>>,
        permission: bool,
        requested: AtomicBool,
        finished: tokio::sync::Notify,
    }
    impl Closer {
        async fn close_once(&self) {
            if !self.requested.swap(true, Ordering::Relaxed) {
                self.session.get().unwrap().close().await.unwrap();
                self.finished.notify_one();
            }
        }
    }
    #[async_trait::async_trait]
    impl SessionDelegate for Closer {
        async fn on_event(&self, _: AgentEvent) {
            if !self.permission {
                self.close_once().await;
            }
        }
        async fn on_permission(&self, _: PermissionPrompt) -> PermissionDecision {
            if self.permission {
                self.close_once().await;
            }
            PermissionDecision::Cancelled
        }
    }
    for permission in [false, true] {
        tokio::time::timeout(test_settings::WAIT_BUDGET, async {
            let callback = Arc::new(Closer {
                session: OnceLock::new(),
                permission,
                requested: AtomicBool::new(false),
                finished: tokio::sync::Notify::new(),
            });
            let lane = AcpLane {
                max_output_bytes: Some(4096),
                command: CommandSpec {
                    program: env!("CARGO_BIN_EXE_a4aa").into(),
                    args: vec!["fixture-agent".into()],
                    cwd: None,
                    env: Default::default(),
                },
                mcp_servers: vec![],
                prefer_load: false,
                auth_key: None,
                client_name: "close-fixture".into(),
                secrets: Arc::new(a4aa::NoSecrets),
                observer: None,
            };
            let session = Arc::new(
                lane.open(SessionRequest::default(), callback.clone())
                    .await
                    .unwrap(),
            );
            assert!(callback.session.set(session.clone()).is_ok());
            let request = TurnRequest {
                text: if permission {
                    "needs permission"
                } else {
                    "hello"
                }
                .into(),
                ..Default::default()
            };
            let (_outcome, ()) =
                tokio::join!(session.prompt(request), callback.finished.notified());
            let error = session
                .prompt(TurnRequest {
                    text: "after close".into(),
                    ..Default::default()
                })
                .await
                .unwrap_err();
            assert_eq!(error.code, "agent_session_closed");
        })
        .await
        .expect("callback close awaited or aborted its own task");
    }
}

fn fixture(script: &str, cwd: &std::path::Path, limit: Option<usize>) -> Adapter {
    Adapter::new(AgentSpec {
        id: "acp-fixture".into(),
        client_name: None,
        max_output_bytes: limit,
        command: CommandSpec {
            program: "python3".into(),
            args: vec!["-u".into(), "-c".into(), script.into()],
            cwd: Some(cwd.to_path_buf()),
            env: Default::default(),
        },
        lane: LaneSpec::Acp {
            mcp_servers: vec![],
            prefer_load: false,
            auth_key: None,
        },
    })
}

#[tokio::test]
async fn cancel_during_configuration_never_dispatches_the_cancelled_prompt() {
    tokio::time::timeout(test_settings::WAIT_BUDGET, async {
        let directory = tempfile::tempdir().unwrap();
        let script = r#"
import json,sys,os
models={'currentModelId':'base','availableModels':[{'modelId':'base','name':'Base'},{'modelId':'next','name':'Next'}]}
prompts=0
for line in sys.stdin:
    m=json.loads(line)
    method=m.get('method')
    if method=='initialize': result={'protocolVersion':1,'agentCapabilities':{}}
    elif method=='session/new': result={'sessionId':'s','models':models}
    elif method=='session/set_model':
        os.mkfifo('release')
        print(json.dumps({'jsonrpc':'2.0','method':'session/update','params':{'sessionId':'s','update':{'sessionUpdate':'agent_message_chunk','content':{'type':'text','text':'configuring'}}}}),flush=True)
        with open('release') as gate: gate.read()
        models['currentModelId']=m['params']['modelId']
        result={'models':models}
    elif method=='session/prompt':
        prompts+=1
        assert prompts==1, 'cancelled setup dispatched an extra prompt'
        print(json.dumps({'jsonrpc':'2.0','method':'session/update','params':{'sessionId':'s','update':{'sessionUpdate':'agent_message_chunk','content':{'type':'text','text':'only-live-prompt'}}}}),flush=True)
        result={'stopReason':'end_turn'}
    else: raise AssertionError('idle/pre-dispatch cancel reached wire: '+str(method))
    print(json.dumps({'jsonrpc':'2.0','id':m['id'],'result':result}),flush=True)
"#;
        let (tx, mut rx) = mpsc::unbounded_channel();
        let session = fixture(script, directory.path(), Some(1024)).open_session(SessionRequest::default(),tx).await.unwrap();
        {
            let mut prompt = std::pin::pin!(session.prompt(TurnRequest { text:"cancelled".into(), model:Some("next".into()), effort:None }));
            tokio::select! {
                result=&mut prompt => panic!("config was not released: {result:?}"),
                event=rx.recv() => assert_eq!(event.unwrap(),AgentEvent::MessageChunk { text:"configuring".into() })
            }
            session.cancel().await.unwrap();
            let barrier=directory.path().join("release");
            tokio::task::spawn_blocking(move || std::fs::write(barrier,b"release")).await.unwrap().unwrap();
            assert_eq!(prompt.await.unwrap().stop_reason,"cancelled");
        }
        session.cancel().await.unwrap(); // idle cancellation must not poison the next turn
        assert_eq!(session.prompt(TurnRequest { text:"live".into(), ..Default::default() }).await.unwrap().stop_reason,"end_turn");
        assert_eq!(rx.recv().await.unwrap(),AgentEvent::MessageChunk { text:"only-live-prompt".into() });
        session.close().await.unwrap();
    }).await.expect("ACP configuration cancellation stalled");
}

#[tokio::test]
async fn output_cap_preserves_permission_completion_and_resets_for_next_turn() {
    tokio::time::timeout(test_settings::WAIT_BUDGET,async {
        let directory=tempfile::tempdir().unwrap();
        let script=r#"
import json,sys
for line in sys.stdin:
    m=json.loads(line)
    method=m.get('method')
    if method=='initialize': result={'protocolVersion':1,'agentCapabilities':{'sessionCapabilities':{'resume':{}}}}
    elif method=='session/resume': result={}
    elif method=='session/prompt':
        for i in range(256):
            update={'sessionUpdate':'agent_message_chunk','content':{'type':'text','text':'界'*4096}}
            print(json.dumps({'jsonrpc':'2.0','method':'session/update','params':{'sessionId':'saved','update':update}},ensure_ascii=False),flush=True)
        update={'sessionUpdate':'tool_call','toolCallId':'tool','title':'x'*8192,'status':'pending'}
        print(json.dumps({'jsonrpc':'2.0','method':'session/update','params':{'sessionId':'saved','update':update}}),flush=True)
        print(json.dumps({'jsonrpc':'2.0','id':'permission','method':'session/request_permission','params':{'sessionId':'saved','toolCall':{'toolCallId':'tool','title':'Read fixture','status':'pending'},'options':[{'optionId':'deny','name':'Deny','kind':'reject_once'}]}}),flush=True)
        permission=json.loads(sys.stdin.readline())
        assert permission['id']=='permission'
        assert permission['result']['outcome']['outcome']=='cancelled'
        result={'stopReason':'end_turn'}
    else: raise AssertionError('unexpected cancellation or fresh session: '+str(method))
    print(json.dumps({'jsonrpc':'2.0','id':m['id'],'result':result}),flush=True)
"#;
        let (tx,mut rx)=mpsc::unbounded_channel();
        let session=fixture(script,directory.path(),Some(1024)).open_session(SessionRequest {resume:Some("saved".into()),..Default::default()},tx).await.unwrap();
        let a4aa::LaneSession::Acp(acp) = session.lane() else { unreachable!() };
        assert!(acp.capabilities().session_resume); // resume-only negotiated surface
        for _ in 0..2 {
            // Leave the host queue entirely unread during the burst.
            let outcome=session.prompt(TurnRequest {text:"run".into(),..Default::default()}).await.unwrap();
            assert_eq!(outcome.stop_reason,"end_turn");assert!(outcome.truncated);
            assert_eq!(rx.try_recv().unwrap(),AgentEvent::MessageChunk {text:"界".repeat(341)});
            assert_eq!(rx.try_recv().unwrap(),AgentEvent::Truncated);
            assert!(rx.try_recv().is_err());
        }
        session.close().await.unwrap();
    }).await.expect("ACP output cap blocked protocol progress");
}
