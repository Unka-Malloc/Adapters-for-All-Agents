//! JsonRpcPeer over in-memory duplex transports: correlation, notifications,
//! bidirectional dispatch under load, and close semantics.

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
        std::time::Duration::from_secs(5),
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
