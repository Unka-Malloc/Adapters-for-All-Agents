//! Bidirectional JSON-RPC 2.0 peer over NDJSON framing.
//!
//! This is the shared engine for `rpc` lanes and for the `a4aa serve`
//! sidecar. Both directions are first-class: agents that issue
//! server-initiated requests (permission prompts, capability probes) are
//! dispatched to the [`RpcDelegate`], and outbound calls correlate on
//! monotonically increasing numeric ids.
//!
//! Shutdown semantics: transport EOF or [`JsonRpcPeer::close`] ends the
//! reader, completes every pending call with `peer_closed`, and drops the
//! writer — which sends EOF to the child. Pending calls never hang.

use serde_json::{json, Value};
use std::collections::HashMap;
use std::io;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::sync::Mutex;
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

use crate::framing::{self, FrameError};
use crate::observe::{Direction, SharedObserver};

/// Wire error object for a failed request.
#[derive(Clone, Debug)]
pub struct RpcErrorObject {
    pub code: i64,
    pub message: String,
    pub data: Option<Value>,
}

impl RpcErrorObject {
    pub fn method_not_found(method: &str) -> Self {
        Self {
            code: -32601,
            message: format!("method not found: {method}"),
            data: None,
        }
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self {
            code: -32603,
            message: message.into(),
            data: None,
        }
    }
}

/// Handles peer-initiated traffic. Default implementations are fail-closed:
/// unknown requests are rejected and unhandled notifications are dropped.
#[async_trait::async_trait]
pub trait RpcDelegate: Send + Sync {
    async fn on_request(&self, method: &str, _params: Value) -> Result<Value, RpcErrorObject> {
        Err(RpcErrorObject::method_not_found(method))
    }

    async fn on_notification(&self, _method: &str, _params: Value) {}

    /// A malformed inbound frame arrived. The stream continues.
    async fn on_protocol_error(&self, _error: String) {}

    /// The inbound stream reached EOF or failed. Delivered exactly once.
    async fn on_closed(&self) {}
}

/// Outcome of an outbound call.
#[derive(Debug)]
pub enum CallFailure {
    /// The peer returned a JSON-RPC error object.
    Remote(RpcErrorObject),
    /// The transport closed or failed before a response arrived.
    Transport(String),
}

struct Pending {
    map: HashMap<u64, oneshot::Sender<Result<Value, CallFailure>>>,
}

/// One JSON-RPC peer. Cheap to clone? No — one peer per transport; share via
/// `Arc`.
pub struct JsonRpcPeer {
    /// `None` once `close` has taken the sender — that drop ends the writer
    /// task, which half-closes the transport.
    writer: Mutex<Option<mpsc::UnboundedSender<Value>>>,
    pending: Mutex<Pending>,
    next_id: AtomicU64,
    reader: Mutex<Option<JoinHandle<()>>>,
    writer_task: Mutex<Option<JoinHandle<io::Result<()>>>>,
    closed: std::sync::atomic::AtomicBool,
}

impl JsonRpcPeer {
    /// Start the peer on a framed transport. `reader`/`writer` are the two
    /// independent halves of the child stdio (or the sidecar's own stdio).
    pub fn spawn<R, W>(
        reader: R,
        writer: W,
        delegate: Arc<dyn RpcDelegate>,
        observer: SharedObserver,
        max_message_bytes: usize,
    ) -> Arc<Self>
    where
        R: AsyncRead + Unpin + Send + 'static,
        W: AsyncWrite + Unpin + Send + 'static,
    {
        let (writer_tx, mut writer_rx) = mpsc::unbounded_channel::<Value>();
        let writer_observer = observer.clone();
        let writer_task = tokio::spawn(async move {
            let mut writer = writer;
            while let Some(frame) = writer_rx.recv().await {
                if let Some(observer) = writer_observer.as_ref() {
                    observer.record("rpc", Direction::Sent, frame.to_string().as_bytes());
                }
                framing::write_frame(&mut writer, &frame).await?;
            }
            let _ = writer.shutdown().await;
            Ok(())
        });

        let peer = Arc::new(Self {
            writer: Mutex::new(Some(writer_tx)),
            pending: Mutex::new(Pending {
                map: HashMap::new(),
            }),
            next_id: AtomicU64::new(1),
            reader: Mutex::new(None),
            writer_task: Mutex::new(Some(writer_task)),
            closed: std::sync::atomic::AtomicBool::new(false),
        });

        let reader_peer = Arc::clone(&peer);
        let reader_task = tokio::spawn(async move {
            let mut reader = BufReader::new(reader);
            loop {
                match framing::read_frame(&mut reader, max_message_bytes).await {
                    Ok(Some(Ok(frame))) => {
                        if let Some(observer) = observer.as_ref() {
                            observer.record(
                                "rpc",
                                Direction::Received,
                                frame.to_string().as_bytes(),
                            );
                        }
                        reader_peer.dispatch(frame, Arc::clone(&delegate));
                    }
                    Ok(Some(Err(FrameError::Oversized(size)))) => {
                        delegate
                            .on_protocol_error(format!("oversized frame ({size} bytes)"))
                            .await;
                    }
                    Ok(Some(Err(FrameError::Invalid(error)))) => {
                        delegate
                            .on_protocol_error(format!("invalid frame: {error}"))
                            .await;
                    }
                    Ok(Some(Err(FrameError::Io(error)))) | Err(error) => {
                        delegate
                            .on_protocol_error(format!("transport error: {error}"))
                            .await;
                        break;
                    }
                    Ok(None) => break,
                }
            }
            reader_peer.fail_pending("peer_closed");
            delegate.on_closed().await;
        });
        *peer.reader.lock().unwrap() = Some(reader_task);
        peer
    }

    /// Route one inbound frame. Responses complete pending calls inline;
    /// requests and notifications each run on their own task so a slow
    /// delegate can never block the read loop (a `session/prompt` request
    /// must not starve a concurrent `session/cancel`).
    fn dispatch(self: &Arc<Self>, frame: Value, delegate: Arc<dyn RpcDelegate>) {
        let is_response = frame.get("id").is_some()
            && frame.get("method").is_none()
            && (frame.get("result").is_some() || frame.get("error").is_some());
        if is_response {
            let id = frame.get("id").and_then(Value::as_u64);
            let reply = id.and_then(|id| self.pending.lock().unwrap().map.remove(&id));
            if let Some(reply) = reply {
                let outcome = if let Some(error) = frame.get("error") {
                    Err(CallFailure::Remote(RpcErrorObject {
                        code: error.get("code").and_then(Value::as_i64).unwrap_or(-32603),
                        message: error
                            .get("message")
                            .and_then(Value::as_str)
                            .unwrap_or("remote error")
                            .to_owned(),
                        data: error.get("data").cloned(),
                    }))
                } else {
                    Ok(frame.get("result").cloned().unwrap_or(Value::Null))
                };
                let _ = reply.send(outcome);
            }
            return;
        }
        let method = frame
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        let params = frame.get("params").cloned().unwrap_or(Value::Null);
        // Request ids are arbitrary JSON values, echoed back verbatim —
        // UUID strings are as legal as numbers. No id → notification.
        match frame.get("id") {
            Some(id) => {
                let id = id.clone();
                let Some(writer) = self.writer.lock().unwrap().clone() else {
                    return;
                };
                tokio::spawn(async move {
                    // The delegate runs on a nested task: a panic there
                    // surfaces as a JoinError here, so the peer always gets
                    // a response and its pending call can never hang on a
                    // delegate fault.
                    let outcome =
                        match tokio::spawn(
                            async move { delegate.on_request(&method, params).await },
                        )
                        .await
                        {
                            Ok(outcome) => outcome,
                            Err(_) => Err(RpcErrorObject::internal("delegate panicked")),
                        };
                    let response = match outcome {
                        Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
                        Err(error) => json!({
                            "jsonrpc": "2.0",
                            "id": id,
                            "error": {
                                "code": error.code,
                                "message": error.message,
                                "data": error.data,
                            }
                        }),
                    };
                    let _ = writer.send(response);
                });
            }
            None => {
                tokio::spawn(async move {
                    delegate.on_notification(&method, params).await;
                });
            }
        }
    }

    /// Send a request and await its response.
    pub async fn call(&self, method: &str, params: Value) -> Result<Value, CallFailure> {
        if self.closed.load(Ordering::Relaxed) {
            return Err(CallFailure::Transport("peer_closed".into()));
        }
        let Some(writer) = self.writer.lock().unwrap().clone() else {
            return Err(CallFailure::Transport("peer_closed".into()));
        };
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        {
            // `closed` is set before fail_pending drains the map, so once
            // we hold this lock either closed is already visible or the
            // drain that follows our insert will complete this entry —
            // a call racing peer shutdown cannot be orphaned.
            let mut pending = self.pending.lock().unwrap();
            if self.closed.load(Ordering::Relaxed) {
                return Err(CallFailure::Transport("peer_closed".into()));
            }
            pending.map.insert(id, tx);
        }
        let frame = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        if writer.send(frame).is_err() {
            self.pending.lock().unwrap().map.remove(&id);
            return Err(CallFailure::Transport("writer_closed".into()));
        }
        match rx.await {
            Ok(outcome) => outcome,
            Err(_) => Err(CallFailure::Transport("peer_closed".into())),
        }
    }

    /// Send a notification (no response expected).
    pub fn notify(&self, method: &str, params: Value) -> Result<(), CallFailure> {
        let Some(writer) = self.writer.lock().unwrap().clone() else {
            return Err(CallFailure::Transport("peer_closed".into()));
        };
        let frame = json!({"jsonrpc": "2.0", "method": method, "params": params});
        writer
            .send(frame)
            .map_err(|_| CallFailure::Transport("writer_closed".into()))
    }

    fn fail_pending(&self, reason: &'static str) {
        // Mark closed first so a caller that inserts under the pending
        // lock after the flag is visible rejects instead of orphaning.
        self.closed.store(true, Ordering::Relaxed);
        let mut pending = self.pending.lock().unwrap();
        for (_, reply) in pending.map.drain() {
            let _ = reply.send(Err(CallFailure::Transport(reason.into())));
        }
    }

    /// Close the peer: stop writing, stop reading, fail pending calls.
    ///
    /// Neither task is awaited: the reader's EOF depends on the peer closing
    /// its writer (may never happen), and the writer task can be kept alive
    /// by in-flight delegate dispatches that still hold a sender clone —
    /// awaiting either would deadlock a caller that closes first. Aborting
    /// drops the transport halves, which still sends the peer our EOF.
    /// In-flight delegate tasks finish in the background; their replies are
    /// simply unsendable.
    pub async fn close(&self) {
        if self.closed.swap(true, Ordering::Relaxed) {
            return;
        }
        self.writer.lock().unwrap().take();
        if let Some(task) = self.writer_task.lock().unwrap().take() {
            task.abort();
        }
        if let Some(task) = self.reader.lock().unwrap().take() {
            task.abort();
        }
        self.fail_pending("peer_closed");
    }
}
