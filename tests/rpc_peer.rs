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

// ------------------ envelope failure classification ------------------

/// One scripted response classification case over an in-memory transport.
fn envelope_spec(success_field: Option<&str>, error_field: Option<&str>) -> a4aa::RpcEnvelopeSpec {
    a4aa::RpcEnvelopeSpec {
        method_field: "type".into(),
        string_ids: true,
        params_inline: true,
        response_method: Some("response".into()),
        result_field: Some("data".into()),
        success_field: success_field.map(str::to_owned),
        error_field: error_field.map(str::to_owned),
        ..Default::default()
    }
}

/// Drive one call whose scripted response frame is written by the server
/// side; returns the call's outcome for classification assertions.
async fn envelope_roundtrip(
    spec: a4aa::RpcEnvelopeSpec,
    response: Value,
) -> Result<Value, CallFailure> {
    use a4aa::framing::{read_frame, write_frame, DEFAULT_MAX_MESSAGE_BYTES};
    let method_field = spec.method_field.clone();
    let response_marker = spec.response_method.clone();
    let (host, agent) = tokio::io::duplex(64 * 1024);
    let (reader, writer) = tokio::io::split(host);
    let peer = JsonRpcPeer::spawn_with_envelope(
        reader,
        writer,
        Arc::new(Noop),
        None,
        DEFAULT_MAX_MESSAGE_BYTES,
        Some(spec),
    );
    let (read, mut write) = tokio::io::split(agent);
    let server = tokio::spawn(async move {
        let mut read = tokio::io::BufReader::new(read);
        let request = read_frame(&mut read, DEFAULT_MAX_MESSAGE_BYTES)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let mut frame = response;
        frame["id"] = request["id"].clone();
        if let Some(marker) = &response_marker {
            frame[method_field.clone()] = json!(marker);
        }
        write_frame(&mut write, &frame).await.unwrap();
    });
    let outcome = peer.call("get_state", json!({"value": 7})).await;
    server.await.unwrap();
    peer.close().await;
    outcome
}

#[tokio::test]
async fn envelope_response_classification_matrix() {
    use a4aa::rpc::RpcErrorObject;
    tokio::time::timeout(test_settings::WAIT_BUDGET, async {
        // The payload a remote failure must carry through untouched.
        let remote_error = json!({"code": -32042, "message": "denied", "data": {"why": "test"}});
        let expect_remote = |outcome: Result<Value, CallFailure>| match outcome {
            Err(CallFailure::Remote(RpcErrorObject {
                code,
                message,
                data,
            })) => {
                assert_eq!(code, -32042);
                assert_eq!(message, "denied");
                assert_eq!(data, Some(json!({"why": "test"})));
            }
            other => panic!("expected remote error, got {other:?}"),
        };

        // success + error fields declared (the pi shape):
        let spec = || envelope_spec(Some("success"), Some("error"));
        let ok = envelope_roundtrip(spec(), json!({"success": true, "data": {"ok": 1} })).await;
        assert_eq!(ok.unwrap(), json!({"ok": 1}));
        expect_remote(
            envelope_roundtrip(
                spec(),
                json!({"success": false, "error": remote_error.clone()}),
            )
            .await,
        );
        // Contradictory success marker plus error content fails closed.
        expect_remote(
            envelope_roundtrip(
                spec(),
                json!({"success": true, "data": {"ok": 1}, "error": remote_error.clone()}),
            )
            .await,
        );
        // A missing declared success marker fails closed too.
        expect_remote(
            envelope_roundtrip(
                spec(),
                json!({"data": {"ok": 1}, "error": remote_error.clone()}),
            )
            .await,
        );
        match envelope_roundtrip(spec(), json!({"data": {"ok": 1}})).await {
            Err(CallFailure::Remote(_)) => {}
            other => panic!("missing success marker must fail closed, got {other:?}"),
        }

        // No success field: the configured error field alone drives failure.
        let spec = || envelope_spec(None, Some("error"));
        let ok = envelope_roundtrip(spec(), json!({"data": {"ok": 2}})).await;
        assert_eq!(ok.unwrap(), json!({"ok": 2}));
        expect_remote(
            envelope_roundtrip(
                spec(),
                json!({"error": remote_error.clone(), "data": {"ok": 2}}),
            )
            .await,
        );
        // A null error value is the absence of failure, not one.
        let ok = envelope_roundtrip(spec(), json!({"error": Value::Null, "data": {"ok": 3}})).await;
        assert_eq!(ok.unwrap(), json!({"ok": 3}));

        // A custom error field name is honoured on the read side.
        match envelope_roundtrip(
            envelope_spec(None, Some("failure")),
            json!({"failure": "plain refusal"}),
        )
        .await
        {
            Err(CallFailure::Remote(error)) => {
                assert_eq!(error.message, "plain refusal");
                assert_eq!(error.data, Some(json!("plain refusal")));
            }
            other => panic!("expected remote error, got {other:?}"),
        }
    })
    .await
    .expect("envelope classification matrix stalled");
}

/// Encode/decode symmetry: an error the peer WRITES for an agent→host
/// request reads back as the same remote failure on the answering side.
#[tokio::test]
async fn envelope_error_roundtrip_is_symmetric() {
    let spec = envelope_spec(Some("success"), Some("error"));
    let (a, b) = tokio::io::duplex(64 * 1024);
    let (ar, aw) = tokio::io::split(a);
    let (br, bw) = tokio::io::split(b);
    let client = JsonRpcPeer::spawn_with_envelope(
        ar,
        aw,
        Arc::new(Noop),
        None,
        a4aa::framing::DEFAULT_MAX_MESSAGE_BYTES,
        Some(spec.clone()),
    );
    let server = JsonRpcPeer::spawn_with_envelope(
        br,
        bw,
        Arc::new(Echo),
        None,
        a4aa::framing::DEFAULT_MAX_MESSAGE_BYTES,
        Some(spec),
    );
    match client.call("fail", Value::Null).await {
        Err(CallFailure::Remote(error)) => {
            assert_eq!(error.code, -32000);
            assert_eq!(error.message, "deliberate");
            assert_eq!(error.data, Some(json!({"why": "test"})));
        }
        other => panic!("expected remote error, got {other:?}"),
    }
    client.close().await;
    server.close().await;
}

// --------------------- outbound send budget ---------------------

/// Queue past the budget with a non-reading remote: admission waits, nothing
/// is dropped, and draining the remote releases headroom in wire order.
#[tokio::test]
async fn outbound_budget_backpressure_and_exact_accounting() {
    use a4aa::framing::read_frame;
    tokio::time::timeout(test_settings::WAIT_BUDGET, async {
        let (client, server) = tokio::io::duplex(64 * 1024);
        let (reader, writer) = tokio::io::split(client);
        let peer = JsonRpcPeer::spawn(reader, writer, Arc::new(Noop), None, 4096);
        let (server_reader, _server_writer) = tokio::io::split(server);
        let mut server_reader = tokio::io::BufReader::new(server_reader);
        let payload = "x".repeat(1024 * 1024);
        // Five ~1 MiB frames exceed the four-frame budget.
        for index in 0..5 {
            peer.notify("note", json!({"index": index, "data": payload}))
                .unwrap();
        }
        assert!(peer.outbound_saturated());
        // Headroom never releases while the remote is not reading.
        {
            let headroom = peer.outbound_headroom();
            tokio::pin!(headroom);
            tokio::select! {
                _ = &mut headroom => panic!("headroom released without a drain"),
                _ = std::future::ready(()) => {}
            }
        }
        // Drain the remote: every frame arrives once, in queue order.
        for index in 0..5 {
            let frame = read_frame(&mut server_reader, 4 * 1024 * 1024)
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            assert_eq!(frame["method"], "note");
            assert_eq!(frame["params"]["index"], index);
            assert_eq!(frame["params"]["data"].as_str().unwrap().len(), 1024 * 1024);
        }
        // Every charge is released once its write completes.
        tokio::time::timeout(test_settings::WAIT_BUDGET, peer.outbound_headroom())
            .await
            .expect("headroom never released after the drain");
        assert!(!peer.outbound_saturated());
        peer.close().await;
    })
    .await
    .expect("outbound budget accounting stalled");
}

/// A writer failure with a saturated backlog still settles pending calls and
/// releases every queued charge exactly once (round-7 failure propagation).
#[tokio::test]
async fn writer_failure_releases_queued_budget_and_settles_pending() {
    tokio::time::timeout(test_settings::WAIT_BUDGET, async {
        let (client, server) = tokio::io::duplex(64 * 1024);
        let (reader, writer) = tokio::io::split(client);
        let peer = JsonRpcPeer::spawn(reader, writer, Arc::new(Noop), None, 4096);
        let payload = "x".repeat(1024 * 1024);
        for _ in 0..5 {
            peer.notify("note", json!({"data": payload})).unwrap();
        }
        assert!(peer.outbound_saturated());
        let pending = peer.start_call("open", json!({})).unwrap();
        // The remote vanishes: the next write fails for real.
        drop(server);
        assert!(matches!(
            pending.wait().await,
            Err(CallFailure::Transport(_))
        ));
        // The failure drain released every queued charge: the budget reads
        // unsaturated and headroom is immediate.
        assert!(!peer.outbound_saturated());
        peer.outbound_headroom().await;
        peer.close().await;
    })
    .await
    .expect("writer failure left the budget charged");
}

// ------------------ factory-style envelope ------------------

/// The `droid exec --input-format stream-jsonrpc` wire: requests carry a
/// constant protocol tag set (`type:"request"`, version members) plus a
/// `method` slot; inbound frames are classified by `type`, not by the
/// method slot. The session lifecycle runs `droid.initialize_session` →
/// `droid.add_user_message` with results under `result`.
#[tokio::test]
async fn factory_envelope_const_fields_and_discriminant() {
    use a4aa::framing::{read_frame, write_frame, DEFAULT_MAX_MESSAGE_BYTES};
    use std::collections::BTreeMap;

    struct Notices {
        got: Mutex<Vec<(String, Value)>>,
    }
    #[async_trait::async_trait]
    impl RpcDelegate for Notices {
        async fn on_notification(&self, method: &str, params: Value) {
            self.got.lock().await.push((method.to_owned(), params));
        }
    }

    tokio::time::timeout(test_settings::WAIT_BUDGET, async {
        let notices = Arc::new(Notices {
            got: Mutex::new(Vec::new()),
        });
        let (host, agent) = tokio::io::duplex(64 * 1024);
        let (reader, writer) = tokio::io::split(host);
        let peer = JsonRpcPeer::spawn_with_envelope(
            reader,
            writer,
            notices.clone(),
            a4aa::observe::none(),
            DEFAULT_MAX_MESSAGE_BYTES,
            Some(a4aa::RpcEnvelopeSpec {
                method_field: "method".into(),
                discriminant_field: Some("type".into()),
                request_marker: Some("request".into()),
                notification_marker: Some("notification".into()),
                string_ids: true,
                response_method: Some("response".into()),
                const_fields: BTreeMap::from([
                    ("jsonrpc".to_owned(), "2.0".to_owned()),
                    ("factoryApiVersion".to_owned(), "1.0.0".to_owned()),
                    ("factoryProtocolVersion".to_owned(), "1.269.0".to_owned()),
                ]),
                ..Default::default()
            }),
        );
        let (read, mut write) = tokio::io::split(agent);
        let server = tokio::spawn(async move {
            let mut read = tokio::io::BufReader::new(read);
            let request = read_frame(&mut read, DEFAULT_MAX_MESSAGE_BYTES)
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            // The request carries the full factory tag set: a constant
            // `type:"request"` discriminant, version members, the method
            // name in its own slot and params nested — nothing inlined.
            assert_eq!(request["type"], "request");
            assert_eq!(request["jsonrpc"], "2.0");
            assert_eq!(request["factoryApiVersion"], "1.0.0");
            assert_eq!(request["factoryProtocolVersion"], "1.269.0");
            assert_eq!(request["method"], "droid.initialize_session");
            assert_eq!(request["params"]["cwd"], "/tmp/work");
            assert!(request["id"].is_string());
            // The response is classified by `type`, not `method`.
            write_frame(
                &mut write,
                &json!({
                    "type": "response",
                    "id": request["id"],
                    "result": {"sessionId": "sess-1"},
                }),
            )
            .await
            .unwrap();
            // A session notification rides `method` with a nested
            // `notification` payload — it must reach the delegate, not be
            // mistaken for a response.
            write_frame(
                &mut write,
                &json!({
                    "type": "notification",
                    "method": "droid.session_notification",
                    "params": {
                        "sessionId": "sess-1",
                        "notification": {
                            "type": "assistant_text_delta",
                            "textDelta": "hi",
                        }
                    },
                }),
            )
            .await
            .unwrap();
        });
        let outcome = peer
            .call(
                "droid.initialize_session",
                json!({"machineId": "a4aa", "cwd": "/tmp/work"}),
            )
            .await
            .unwrap();
        assert_eq!(outcome["sessionId"], "sess-1");
        server.await.unwrap();
        // The ordered callback drains before close settles.
        peer.close().await;
        let got = notices.got.lock().await;
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].0, "droid.session_notification");
        assert_eq!(got[0].1["notification"]["textDelta"], "hi");
    })
    .await
    .expect("factory envelope roundtrip exceeded budget");
}

/// Constant members tag requests only — a notification must not inherit
/// the `"request"` discriminant.
#[tokio::test]
async fn factory_envelope_notifications_stamp_their_own_marker() {
    use a4aa::framing::{read_frame, DEFAULT_MAX_MESSAGE_BYTES};
    use std::collections::BTreeMap;

    tokio::time::timeout(test_settings::WAIT_BUDGET, async {
        let (host, agent) = tokio::io::duplex(4096);
        let (reader, writer) = tokio::io::split(host);
        let peer = JsonRpcPeer::spawn_with_envelope(
            reader,
            writer,
            Arc::new(Noop),
            a4aa::observe::none(),
            DEFAULT_MAX_MESSAGE_BYTES,
            Some(a4aa::RpcEnvelopeSpec {
                method_field: "method".into(),
                discriminant_field: Some("type".into()),
                request_marker: Some("request".into()),
                notification_marker: Some("notification".into()),
                response_method: Some("response".into()),
                const_fields: BTreeMap::from([("jsonrpc".to_owned(), "2.0".to_owned())]),
                ..Default::default()
            }),
        );
        peer.notify("droid.session_notification", json!({}))
            .unwrap();
        let (read, _write) = tokio::io::split(agent);
        let mut read = tokio::io::BufReader::new(read);
        let frame = read_frame(&mut read, DEFAULT_MAX_MESSAGE_BYTES)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(frame["method"], "droid.session_notification");
        // A notification has no id, stamps the discriminant with its own
        // marker, and still carries protocol identity tags.
        assert_eq!(frame["type"], "notification");
        assert_eq!(frame["jsonrpc"], "2.0");
        assert!(frame.get("id").is_none());
        peer.close().await;
    })
    .await
    .expect("notification frame");
}
