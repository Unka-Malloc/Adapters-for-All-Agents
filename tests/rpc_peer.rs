//! JsonRpcPeer over in-memory duplex transports: correlation, notifications,
//! bidirectional dispatch under load, and close semantics.

#[path = "support/settings.rs"]
mod test_settings;

use a4aa::rpc::{CallFailure, JsonRpcPeer, RpcDelegate, RpcErrorObject};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot, Mutex};

fn peers(
    client_delegate: Arc<dyn RpcDelegate>,
    server_delegate: Arc<dyn RpcDelegate>,
) -> (Arc<JsonRpcPeer>, Arc<JsonRpcPeer>) {
    let (a, b) = tokio::io::duplex(64 * 1024);
    let (ar, aw) = tokio::io::split(a);
    let (br, bw) = tokio::io::split(b);
    (
        JsonRpcPeer::spawn(
            ar,
            aw,
            client_delegate,
            a4aa::observe::none(),
            a4aa::framing::DEFAULT_MAX_MESSAGE_BYTES,
        ),
        JsonRpcPeer::spawn(
            br,
            bw,
            server_delegate,
            a4aa::observe::none(),
            a4aa::framing::DEFAULT_MAX_MESSAGE_BYTES,
        ),
    )
}

#[tokio::test]
async fn synchronous_start_enqueues_before_a_related_notification() {
    struct Sink;
    #[async_trait::async_trait]
    impl RpcDelegate for Sink {}
    tokio::time::timeout(test_settings::WAIT_BUDGET, async {
        let (client, server) = tokio::io::duplex(4096);
        let (reader, writer) = tokio::io::split(client);
        let peer = JsonRpcPeer::spawn(reader, writer, Arc::new(Sink), None, 4096);
        let pending = peer.start_call("prompt", json!({})).unwrap();
        peer.notify("cancel", json!({})).unwrap();
        let (reader, mut writer) = tokio::io::split(server);
        let mut reader = tokio::io::BufReader::new(reader);
        let first = a4aa::framing::read_frame(&mut reader, 4096)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let second = a4aa::framing::read_frame(&mut reader, 4096)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(first["method"], "prompt");
        assert_eq!(second["method"], "cancel");
        a4aa::framing::write_frame(
            &mut writer,
            &json!({"jsonrpc":"2.0","id":first["id"],"result":"done"}),
        )
        .await
        .unwrap();
        assert_eq!(pending.wait().await.unwrap(), json!("done"));
        peer.close().await;
    })
    .await
    .expect("request was not enqueued synchronously");
}

struct Noop;
#[async_trait::async_trait]
impl RpcDelegate for Noop {}

#[tokio::test]
async fn flat_parameters_cannot_override_protocol_control_fields() {
    use a4aa::framing::{read_frame, write_frame, DEFAULT_MAX_MESSAGE_BYTES};
    let (host, agent) = tokio::io::duplex(4096);
    let (reader, writer) = tokio::io::split(host);
    let peer = JsonRpcPeer::spawn_with_envelope(
        reader,
        writer,
        Arc::new(Noop),
        a4aa::observe::none(),
        DEFAULT_MAX_MESSAGE_BYTES,
        Some(a4aa::RpcEnvelopeSpec {
            method_field: "type".into(),
            string_ids: true,
            params_inline: true,
            response_method: Some("response".into()),
            result_field: Some("data".into()),
            success_field: Some("success".into()),
            ..Default::default()
        }),
    );
    let (read, mut write) = tokio::io::split(agent);
    let mut read = tokio::io::BufReader::new(read);
    let client = async {
        let result = peer
            .call(
                "get_state",
                json!({"type":"prompt","id":"forged","value":7}),
            )
            .await
            .unwrap();
        assert_eq!(result, json!({"ok":true}));
        peer.notify("ping", json!({"type":"prompt","id":"forged","value":9}))
            .unwrap();
    };
    let server = async {
        let frame = read_frame(&mut read, DEFAULT_MAX_MESSAGE_BYTES)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(frame["type"], "get_state");
        assert_ne!(frame["id"], "forged");
        assert_eq!(frame["value"], 7);
        write_frame(
            &mut write,
            &json!({"type":"response","id":frame["id"],"success":true,"data":{"ok":true}}),
        )
        .await
        .unwrap();
        let frame = read_frame(&mut read, DEFAULT_MAX_MESSAGE_BYTES)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(frame["type"], "ping");
        assert!(frame.get("id").is_none());
        assert_eq!(frame["value"], 9);
    };
    tokio::join!(client, server);
    peer.close().await;
}

struct Echo;
#[async_trait::async_trait]
impl RpcDelegate for Echo {
    async fn on_request(&self, method: &str, params: Value) -> Result<Value, RpcErrorObject> {
        match method {
            "echo" => Ok(json!({"echo": params})),
            "fail" => Err(RpcErrorObject {
                code: -32000,
                message: "deliberate".into(),
                data: Some(json!({"why": "test"})),
            }),
            _ => Err(RpcErrorObject::method_not_found(method)),
        }
    }
}

#[tokio::test]
async fn call_correlates_responses() {
    let (client, server) = peers(Arc::new(Noop), Arc::new(Echo));
    let result = client
        .call("echo", json!({"n": 1}))
        .await
        .expect("echo call");
    assert_eq!(result, json!({"echo": {"n": 1}}));

    match client.call("fail", Value::Null).await {
        Err(CallFailure::Remote(error)) => {
            assert_eq!(error.code, -32000);
            assert_eq!(error.data, Some(json!({"why": "test"})));
        }
        other => panic!("expected remote error, got {other:?}"),
    }
    match client.call("missing", Value::Null).await {
        Err(CallFailure::Remote(error)) => assert_eq!(error.code, -32601),
        other => panic!("expected method-not-found, got {other:?}"),
    }
    client.close().await;
    server.close().await;
}

#[tokio::test]
async fn notifications_arrive_out_of_band() {
    struct Sink {
        seen: Mutex<Vec<(String, Value)>>,
        hit: mpsc::UnboundedSender<()>,
    }
    #[async_trait::async_trait]
    impl RpcDelegate for Sink {
        async fn on_notification(&self, method: &str, params: Value) {
            self.seen.lock().await.push((method.to_owned(), params));
            let _ = self.hit.send(());
        }
    }
    let (hit_tx, mut hit_rx) = mpsc::unbounded_channel();
    let sink = Arc::new(Sink {
        seen: Mutex::new(Vec::new()),
        hit: hit_tx,
    });
    let (client, server) = peers(sink.clone(), Arc::new(Noop));
    server.notify("session/event", json!({"i": 0})).unwrap();
    server.notify("session/event", json!({"i": 1})).unwrap();
    for _ in 0..2 {
        hit_rx.recv().await.expect("notification");
    }
    assert_eq!(sink.seen.lock().await.len(), 2);
    client.close().await;
    server.close().await;
}

/// Regression: a delegate that answers slowly must not block the read loop —
/// a second inbound request still gets served while the first is in flight.
#[tokio::test]
async fn slow_delegate_does_not_starve_control_messages() {
    struct Slow {
        gate: Mutex<Option<oneshot::Receiver<()>>>,
        answered: AtomicUsize,
    }
    #[async_trait::async_trait]
    impl RpcDelegate for Slow {
        async fn on_request(&self, method: &str, _params: Value) -> Result<Value, RpcErrorObject> {
            match method {
                "slow" => {
                    let gate = self.gate.lock().await.take();
                    if let Some(gate) = gate {
                        let _ = gate.await; // stays parked until released
                    }
                    Ok(json!({"slow": true}))
                }
                "control" => {
                    self.answered.fetch_add(1, Ordering::Relaxed);
                    Ok(json!({"control": true}))
                }
                _ => Err(RpcErrorObject::method_not_found(method)),
            }
        }
    }
    let (release, gate) = oneshot::channel();
    let slow = Arc::new(Slow {
        gate: Mutex::new(Some(gate)),
        answered: AtomicUsize::new(0),
    });
    let (client, server) = peers(slow.clone(), Arc::new(Noop));

    // Server issues a long-running request, then a control request behind it.
    let slow_call = tokio::spawn({
        let server = server.clone();
        async move { server.call("slow", Value::Null).await }
    });
    // Give the slow request a moment to be dispatched.
    tokio::task::yield_now().await;
    let control = tokio::time::timeout(
        test_settings::WAIT_BUDGET,
        server.call("control", Value::Null),
    )
    .await
    .expect("control call timed out — read loop starved")
    .expect("control result");
    assert_eq!(control, json!({"control": true}));

    release.send(()).unwrap();
    assert_eq!(slow_call.await.unwrap().unwrap(), json!({"slow": true}));
    client.close().await;
    server.close().await;
}

#[tokio::test]
async fn peer_close_fails_pending_calls() {
    struct Hang;
    #[async_trait::async_trait]
    impl RpcDelegate for Hang {
        async fn on_request(&self, _method: &str, _params: Value) -> Result<Value, RpcErrorObject> {
            std::future::pending::<()>().await;
            unreachable!()
        }
    }
    let (client, server) = peers(Arc::new(Noop), Arc::new(Hang));
    let pending = tokio::spawn({
        let client = client.clone();
        async move { client.call("never", Value::Null).await }
    });
    tokio::task::yield_now().await;
    server.close().await; // EOF to client: pending resolves
    match pending.await.unwrap() {
        Err(CallFailure::Transport(_)) => {}
        other => panic!("expected transport failure, got {other:?}"),
    }
    client.close().await;
}

#[tokio::test]
async fn unreadable_responses_fail_pending_calls_without_waiting_for_eof() {
    use a4aa::framing::{read_frame, write_frame};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    tokio::time::timeout(test_settings::WAIT_BUDGET, async {
        for oversized in [false, true] {
            let (client, server) = tokio::io::duplex(4096);
            let (reader, writer) = tokio::io::split(client);
            let peer = JsonRpcPeer::spawn(reader, writer, Arc::new(Noop), None, 256);
            let pending = peer.start_call("thread/resume", json!({})).unwrap();
            let (reader, mut writer) = tokio::io::split(server);
            let mut reader = tokio::io::BufReader::new(reader);
            let request = read_frame(&mut reader, 4096).await.unwrap().unwrap().unwrap();
            let expected = if oversized {
                write_frame(&mut writer, &json!({"id":request["id"],"result":"x".repeat(1024)}))
                    .await.unwrap();
                "oversized_frame"
            } else {
                writer.write_all(b"invalid response\n").await.unwrap();
                "invalid_frame"
            };
            // Keep the remote writer alive: only protocol failure can settle
            // this pending call; EOF must not be required for recovery.
            assert!(matches!(pending.wait().await, Err(CallFailure::Transport(reason)) if reason == expected));
            assert!(peer.start_call("another", json!({})).is_err());
            peer.close().await;
            assert_eq!(reader.read_to_end(&mut Vec::new()).await.unwrap(), 0);
            drop(writer);
        }
    }).await.expect("discarded RPC response left its caller waiting");
}

#[tokio::test]
async fn malformed_frames_surface_as_protocol_errors() {
    struct Collect {
        errors: Mutex<Vec<String>>,
        hit: mpsc::UnboundedSender<()>,
    }
    #[async_trait::async_trait]
    impl RpcDelegate for Collect {
        async fn on_protocol_error(&self, error: String) {
            self.errors.lock().await.push(error);
            let _ = self.hit.send(());
        }
    }
    let (hit_tx, mut hit_rx) = mpsc::unbounded_channel();
    let collector = Arc::new(Collect {
        errors: Mutex::new(Vec::new()),
        hit: hit_tx,
    });
    let (mut raw, peer_side) = tokio::io::duplex(4096);
    let (pr, pw) = tokio::io::split(peer_side);
    let _peer = JsonRpcPeer::spawn(
        pr,
        pw,
        collector.clone(),
        a4aa::observe::none(),
        a4aa::framing::DEFAULT_MAX_MESSAGE_BYTES,
    );
    use tokio::io::AsyncWriteExt;
    raw.write_all(b"garbage line\n").await.unwrap();
    hit_rx.recv().await.expect("protocol error");
    assert!(collector.errors.lock().await[0].contains("invalid"));
}

// ------------------------- writer failure -------------------------

struct CloseHooks {
    input_closed: AtomicUsize,
    closed: AtomicUsize,
    protocol_errors: AtomicUsize,
    input_signal: tokio::sync::Notify,
    closed_signal: tokio::sync::Notify,
    protocol_signal: tokio::sync::Notify,
}

impl CloseHooks {
    fn new() -> Self {
        Self {
            input_closed: AtomicUsize::new(0),
            closed: AtomicUsize::new(0),
            protocol_errors: AtomicUsize::new(0),
            input_signal: tokio::sync::Notify::new(),
            closed_signal: tokio::sync::Notify::new(),
            protocol_signal: tokio::sync::Notify::new(),
        }
    }
}

#[async_trait::async_trait]
impl RpcDelegate for CloseHooks {
    fn on_input_closed(&self) {
        self.input_closed.fetch_add(1, Ordering::Relaxed);
        self.input_signal.notify_one();
    }
    async fn on_closed(&self) {
        self.closed.fetch_add(1, Ordering::Relaxed);
        self.closed_signal.notify_one();
    }
    async fn on_protocol_error(&self, _error: String) {
        self.protocol_errors.fetch_add(1, Ordering::Relaxed);
        self.protocol_signal.notify_one();
    }
}

/// Spawn a peer over a real child whose stdin is already closed. `script`
/// must close fd 0 before writing the readiness byte to stderr, so the
/// first outbound frame deterministically hits a broken pipe.
async fn spawn_stdin_closed_peer(
    script: &str,
    hooks: Arc<CloseHooks>,
) -> (tokio::process::Child, Arc<JsonRpcPeer>) {
    use tokio::io::AsyncReadExt;
    let mut child = tokio::process::Command::new("sh")
        .args(["-c", script])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    // Readiness barrier: stdin is guaranteed closed once this byte lands.
    child
        .stderr
        .as_mut()
        .unwrap()
        .read_exact(&mut [0u8; 1])
        .await
        .unwrap();
    let peer = JsonRpcPeer::spawn(
        child.stdout.take().unwrap(),
        child.stdin.take().unwrap(),
        hooks,
        None,
        4096,
    );
    (child, peer)
}

/// Regression: a real writer failure (the agent closed its stdin) must
/// settle pending calls promptly, reject new traffic, and fire the
/// ingress-closed hooks — no reader EOF is involved at all.
#[tokio::test]
async fn writer_failure_fails_pending_calls_and_rejects_new_traffic() {
    tokio::time::timeout(test_settings::WAIT_BUDGET, async {
        let hooks = Arc::new(CloseHooks::new());
        // stdout stays open and silent: only the writer failure can settle.
        let (mut child, peer) =
            spawn_stdin_closed_peer("exec 0<&-; printf x >&2; exec sleep 60", hooks.clone()).await;
        let pending = peer.start_call("open", json!({})).unwrap();
        match pending.wait().await {
            Err(CallFailure::Transport(_)) => {}
            other => panic!("expected transport failure, got {other:?}"),
        }
        // New traffic is rejected immediately on the failed transport.
        assert!(matches!(
            peer.start_call("next", json!({})),
            Err(CallFailure::Transport(_))
        ));
        assert!(matches!(
            peer.notify("note", json!({})),
            Err(CallFailure::Transport(_))
        ));
        peer.closed().await;
        hooks.input_signal.notified().await;
        hooks.closed_signal.notified().await;
        assert_eq!(hooks.input_closed.load(Ordering::Relaxed), 1);
        assert_eq!(hooks.closed.load(Ordering::Relaxed), 1);
        // An explicit close after the failure must not re-fire the hooks.
        peer.close().await;
        assert_eq!(hooks.input_closed.load(Ordering::Relaxed), 1);
        assert_eq!(hooks.closed.load(Ordering::Relaxed), 1);
        let _ = child.kill().await;
    })
    .await
    .expect("writer failure did not settle the peer");
}

/// The reader's and writer's failure paths share one close notification: a
/// garbage inbound frame plus a broken outbound pipe still fire
/// `on_input_closed`/`on_closed` exactly once.
#[tokio::test]
async fn writer_and_reader_failures_share_one_close_notification() {
    tokio::time::timeout(test_settings::WAIT_BUDGET, async {
        let hooks = Arc::new(CloseHooks::new());
        let (mut child, peer) = spawn_stdin_closed_peer(
            "exec 0<&-; printf 'not json\\n'; printf x >&2; exec sleep 60",
            hooks.clone(),
        )
        .await;
        // The garbage line may fail the peer before this call is enqueued;
        // either failure shape settles the caller with a transport error.
        let outcome = match peer.start_call("open", json!({})) {
            Ok(pending) => pending.wait().await,
            Err(failure) => Err(failure),
        };
        assert!(matches!(outcome, Err(CallFailure::Transport(_))));
        // Both failure paths ran; the close hooks fired exactly once.
        hooks.protocol_signal.notified().await;
        hooks.closed_signal.notified().await;
        assert_eq!(hooks.input_closed.load(Ordering::Relaxed), 1);
        assert_eq!(hooks.closed.load(Ordering::Relaxed), 1);
        assert_eq!(hooks.protocol_errors.load(Ordering::Relaxed), 1);
        peer.close().await;
        let _ = child.kill().await;
    })
    .await
    .expect("reader/writer failure coordination stalled");
}
