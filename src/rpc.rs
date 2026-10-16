//! Bidirectional JSON-RPC 2.0 peer over NDJSON framing.
//!
//! This is the shared engine for `rpc` lanes and for the `a4aa serve`
//! sidecar. Both directions are first-class: agents that issue
//! server-initiated requests (permission prompts, capability probes) are
//! dispatched to the [`RpcDelegate`], and outbound calls correlate on
//! monotonically increasing numeric ids.
//!
//! Shutdown semantics: transport EOF, a writer failure, or
//! [`JsonRpcPeer::close`] marks the peer closed, completes every pending
//! call with a transport error, and rejects new calls. EOF or `close` drops
//! the writer — which sends EOF to the child. Pending calls never hang.

use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::sync::Mutex;
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, oneshot, Notify};
use tokio::task::JoinHandle;

use crate::framing::{self, FrameError};
use crate::model::RpcEnvelopeSpec;
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

    /// Synchronous ingress admission, before the ordered callback queue.
    /// A delegate may retain bounded state and return a small wake-up record,
    /// or consume the notification with `None`. Must not block: responses and
    /// permission requests on this transport must remain serviceable while
    /// an asynchronous notification callback is waiting for them.
    /// The default preserves every notification without a queue budget.
    fn prepare_notification(&self, method: String, params: Value) -> Option<(String, Value)> {
        Some((method, params))
    }

    /// Transport closure observed at ingress, independently of queued callbacks.
    fn on_input_closed(&self) {}

    /// An unreadable inbound frame closed the stream. Pending calls have
    /// already failed because the discarded frame cannot be correlated.
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

/// An already-enqueued request. Dropping the waiter does not cancel remote work.
pub struct PendingCall(oneshot::Receiver<Result<Value, CallFailure>>);
impl PendingCall {
    pub async fn wait(self) -> Result<Value, CallFailure> {
        self.0
            .await
            .unwrap_or_else(|_| Err(CallFailure::Transport("peer_closed".into())))
    }
}

struct Pending {
    /// Pending calls keyed by the normalized wire id — numeric and string
    /// ids share one map because both render to the same key string.
    map: HashMap<String, oneshot::Sender<Result<Value, CallFailure>>>,
}

/// Normalized pending-call key for a wire id of either shape.
fn id_key(id: &Value) -> Option<String> {
    id.as_u64()
        .map(|n| n.to_string())
        .or_else(|| id.as_str().map(str::to_owned))
}

/// Failure state shared with the writer task. The peer owns the writer's
/// `JoinHandle`, so the writer must not hold a strong `Arc<JsonRpcPeer>`
/// back — this state carries exactly what failure propagation needs.
struct PeerState {
    pending: Mutex<Pending>,
    closed: AtomicBool,
    closed_notice: Notify,
    notifications: mpsc::UnboundedSender<Option<(String, Value)>>,
    /// Guards the once-only ingress-closed hooks (`on_input_closed` and the
    /// notification-queue EOF) shared by the reader's and writer's failure
    /// paths and by explicit close.
    ingress_closed: AtomicBool,
}

impl PeerState {
    fn fail_pending(&self, reason: &'static str) {
        // Mark closed first so a caller that inserts under the pending
        // lock after the flag is visible rejects instead of orphaning.
        self.closed.store(true, Ordering::Relaxed);
        self.closed_notice.notify_waiters();
        let mut pending = self.pending.lock().unwrap();
        for (_, reply) in pending.map.drain() {
            let _ = reply.send(Err(CallFailure::Transport(reason.into())));
        }
    }

    /// The transport failed or ended: fail pending calls, then fire the
    /// ingress-closed hooks — exactly once across the reader's and the
    /// writer's failure paths.
    fn transport_failed(&self, reason: &'static str, delegate: &Arc<dyn RpcDelegate>) {
        self.fail_pending(reason);
        self.close_ingress(delegate);
    }

    fn close_ingress(&self, delegate: &Arc<dyn RpcDelegate>) {
        if self.ingress_closed.swap(true, Ordering::AcqRel) {
            return;
        }
        delegate.on_input_closed();
        let _ = self.notifications.send(None);
    }

    /// Explicit close ends the notification stream without an ingress hook.
    fn close_notifications(&self) {
        if !self.ingress_closed.swap(true, Ordering::AcqRel) {
            let _ = self.notifications.send(None);
        }
    }
}

/// One JSON-RPC peer. Cheap to clone? No — one peer per transport; share via
/// `Arc`.
pub struct JsonRpcPeer {
    /// `None` once `close` has taken the sender — that drop ends the writer
    /// task, which half-closes the transport.
    writer: Mutex<Option<mpsc::UnboundedSender<Value>>>,
    state: Arc<PeerState>,
    next_id: AtomicU64,
    reader: Mutex<Option<JoinHandle<()>>>,
    writer_task: Mutex<Option<JoinHandle<()>>>,
    /// Non-stock envelope conventions (`None` = plain JSON-RPC 2.0).
    envelope: Option<RpcEnvelopeSpec>,
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
        Self::spawn_with_envelope(reader, writer, delegate, observer, max_message_bytes, None)
    }

    /// Start the peer with explicit wire conventions. `envelope: None` is
    /// plain JSON-RPC 2.0; a spec describes flat `{type,id,…}` envelopes.
    pub fn spawn_with_envelope<R, W>(
        reader: R,
        writer: W,
        delegate: Arc<dyn RpcDelegate>,
        observer: SharedObserver,
        max_message_bytes: usize,
        envelope: Option<RpcEnvelopeSpec>,
    ) -> Arc<Self>
    where
        R: AsyncRead + Unpin + Send + 'static,
        W: AsyncWrite + Unpin + Send + 'static,
    {
        let (writer_tx, mut writer_rx) = mpsc::unbounded_channel::<Value>();
        // Preserve notification order without blocking responses or inbound
        // requests. EOF is queued after the last event, so completion cannot
        // be overtaken by the transport closing.
        let (notifications, mut notification_rx) =
            mpsc::unbounded_channel::<Option<(String, Value)>>();
        let state = Arc::new(PeerState {
            pending: Mutex::new(Pending {
                map: HashMap::new(),
            }),
            closed: AtomicBool::new(false),
            closed_notice: Notify::new(),
            notifications,
            ingress_closed: AtomicBool::new(false),
        });

        // A real write/flush failure on the outbound transport must reach
        // the peer: settle pending calls, mark closed, and run the
        // ingress-closed hooks (once-guarded against the reader's path).
        let writer_observer = observer.clone();
        let writer_state = Arc::clone(&state);
        let writer_delegate = Arc::clone(&delegate);
        let writer_task = tokio::spawn(async move {
            let mut writer = writer;
            let mut failed = false;
            while let Some(frame) = writer_rx.recv().await {
                if let Some(observer) = writer_observer.as_ref() {
                    observer.record("rpc", Direction::Sent, frame.to_string().as_bytes());
                }
                if framing::write_frame(&mut writer, &frame).await.is_err() {
                    failed = true;
                    break;
                }
            }
            // Channel end half-closes the transport. A failed final flush
            // means the agent may never have seen the last accepted frames
            // — propagate it like any other writer failure. (`close` aborts
            // this task, so an explicit close never reaches this path.)
            if !failed && writer.shutdown().await.is_err() {
                failed = true;
            }
            if failed {
                writer_state.transport_failed("writer_failed", &writer_delegate);
            }
        });

        let event_delegate = delegate.clone();
        tokio::spawn(async move {
            while let Some(Some((method, params))) = notification_rx.recv().await {
                event_delegate.on_notification(&method, params).await;
            }
            event_delegate.on_closed().await;
        });
        let peer = Arc::new(Self {
            writer: Mutex::new(Some(writer_tx)),
            state,
            next_id: AtomicU64::new(1),
            reader: Mutex::new(None),
            writer_task: Mutex::new(Some(writer_task)),
            envelope,
        });

        let reader_peer = Arc::clone(&peer);
        let reader_task = tokio::spawn(async move {
            let mut reader = BufReader::new(reader);
            let failure = loop {
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
                        break Some(("oversized_frame", format!("oversized frame ({size} bytes)")));
                    }
                    Ok(Some(Err(FrameError::Invalid(error)))) => {
                        break Some(("invalid_frame", format!("invalid frame: {error}")));
                    }
                    Ok(Some(Err(FrameError::Io(error)))) | Err(error) => {
                        break Some(("peer_closed", format!("transport error: {error}")));
                    }
                    Ok(None) => break None,
                }
            };
            reader_peer.state.transport_failed(
                failure.as_ref().map_or("peer_closed", |(code, _)| code),
                &delegate,
            );
            if let Some((_, message)) = failure {
                delegate.on_protocol_error(message).await;
            }
        });
        *peer.reader.lock().unwrap() = Some(reader_task);
        peer
    }

    /// Route one inbound frame. Responses complete pending calls inline;
    /// requests run on independent tasks and notifications use an ordered
    /// queue, so a slow delegate never blocks the read loop (a `session/prompt` request
    /// must not starve a concurrent `session/cancel`).
    fn dispatch(self: &Arc<Self>, frame: Value, delegate: Arc<dyn RpcDelegate>) {
        if self.envelope.is_some() {
            return self.dispatch_envelope(frame, delegate);
        }
        let is_response = frame.get("id").is_some()
            && frame.get("method").is_none()
            && (frame.get("result").is_some() || frame.get("error").is_some());
        if is_response {
            let key = frame.get("id").and_then(id_key);
            let reply = key.and_then(|id| self.state.pending.lock().unwrap().map.remove(&id));
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
                if let Some(notification) = delegate.prepare_notification(method, params) {
                    let _ = self.state.notifications.send(Some(notification));
                }
            }
        }
    }

    /// Inbound classification for a declared non-stock envelope: a frame
    /// whose `method_field` equals `response_method` completes a pending
    /// call; any other `id`-bearing frame is an agent→host request; an
    /// id-less frame is a notification.
    fn dispatch_envelope(&self, frame: Value, delegate: Arc<dyn RpcDelegate>) {
        let Some(envelope) = &self.envelope else {
            return;
        };
        let data_field = envelope.data_field.as_deref().unwrap_or("params");
        let method = frame
            .get(envelope.method_field.as_str())
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        let id = frame.get("id").cloned();
        let params = frame.get(data_field).cloned().unwrap_or(Value::Null);
        let is_response =
            id.is_some() && envelope.response_method.as_deref() == Some(method.as_str());
        if let (Some(id), true) = (id.clone(), is_response) {
            let reply =
                id_key(&id).and_then(|key| self.state.pending.lock().unwrap().map.remove(&key));
            if let Some(reply) = reply {
                let failed = envelope
                    .success_field
                    .as_deref()
                    .is_some_and(|field| frame.get(field) == Some(&Value::Bool(false)));
                let outcome = if failed {
                    let error = frame
                        .get(envelope.error_field.as_deref().unwrap_or("error"))
                        .cloned()
                        .unwrap_or(Value::Null);
                    Err(CallFailure::Remote(RpcErrorObject {
                        code: error.get("code").and_then(Value::as_i64).unwrap_or(-32603),
                        message: error
                            .as_str()
                            .map(str::to_owned)
                            .or_else(|| {
                                error
                                    .get("message")
                                    .and_then(Value::as_str)
                                    .map(str::to_owned)
                            })
                            .unwrap_or_else(|| "remote error".to_owned()),
                        data: (!error.is_null()).then_some(error),
                    }))
                } else {
                    Ok(frame
                        .get(envelope.result_field.as_deref().unwrap_or("result"))
                        .cloned()
                        .unwrap_or(Value::Null))
                };
                let _ = reply.send(outcome);
            }
            return;
        }
        match id {
            Some(id) => {
                let Some(writer) = self.writer.lock().unwrap().clone() else {
                    return;
                };
                let method_field = envelope.method_field.clone();
                let response_method = envelope.response_method.clone();
                let command_field = envelope.command_field.clone();
                let result_field = envelope
                    .result_field
                    .clone()
                    .unwrap_or_else(|| "result".to_owned());
                let error_field = envelope
                    .error_field
                    .clone()
                    .unwrap_or_else(|| "error".to_owned());
                let success_field = envelope.success_field.clone();
                let request_method = method.clone();
                tokio::spawn(async move {
                    let outcome = match tokio::spawn(async move {
                        delegate.on_request(&request_method, params).await
                    })
                    .await
                    {
                        Ok(outcome) => outcome,
                        Err(_) => Err(RpcErrorObject::internal("delegate panicked")),
                    };
                    let mut response = serde_json::Map::new();
                    if let Some(marker) = response_method {
                        response.insert(method_field, Value::String(marker));
                    }
                    if let Some(field) = &command_field {
                        response.insert(field.clone(), json!(method));
                    }
                    response.insert("id".into(), id);
                    match outcome {
                        Ok(result) => {
                            if let Some(field) = &success_field {
                                response.insert(field.clone(), json!(true));
                            }
                            response.insert(result_field, result);
                        }
                        Err(error) => {
                            if let Some(field) = &success_field {
                                response.insert(field.clone(), json!(false));
                            }
                            response.insert(
                                error_field,
                                json!({"code": error.code, "message": error.message, "data": error.data}),
                            );
                        }
                    }
                    let _ = writer.send(Value::Object(response));
                });
            }
            None => {
                if let Some(notification) = delegate.prepare_notification(method, params) {
                    let _ = self.state.notifications.send(Some(notification));
                }
            }
        }
    }

    /// Encode one outbound frame under the active envelope.
    fn encode_frame(&self, method: &str, params: Value, id: Option<Value>) -> Value {
        match &self.envelope {
            None => {
                let mut frame = json!({"jsonrpc": "2.0", "method": method, "params": params});
                if let Some(id) = id {
                    frame["id"] = id;
                }
                frame
            }
            Some(envelope) => {
                let mut frame = serde_json::Map::new();
                if envelope.params_inline {
                    if let Value::Object(map) = params {
                        frame.extend(map);
                    } else if !params.is_null() {
                        frame.insert("params".into(), params);
                    }
                } else {
                    frame.insert("params".into(), params);
                }
                // Protocol control fields belong to the peer, never to
                // caller-supplied flat parameters. Notifications have no id.
                frame.insert(
                    envelope.method_field.clone(),
                    Value::String(method.to_owned()),
                );
                frame.remove("id");
                if let Some(id) = id {
                    frame.insert("id".into(), id);
                }
                Value::Object(frame)
            }
        }
    }

    /// Send a request and await its response.
    pub async fn call(&self, method: &str, params: Value) -> Result<Value, CallFailure> {
        self.start_call(method, params)?.wait().await
    }

    /// Enqueue synchronously, then let the owner decide where to await the
    /// response. This allows dispatch and cancellation to share a critical
    /// section without holding a lock across an await.
    pub fn start_call(&self, method: &str, params: Value) -> Result<PendingCall, CallFailure> {
        if self.state.closed.load(Ordering::Relaxed) {
            return Err(CallFailure::Transport("peer_closed".into()));
        }
        let Some(writer) = self.writer.lock().unwrap().clone() else {
            return Err(CallFailure::Transport("peer_closed".into()));
        };
        let serial = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (wire_id, key) = if self
            .envelope
            .as_ref()
            .is_some_and(|envelope| envelope.string_ids)
        {
            let wire = format!("a4aa-{serial}");
            (json!(wire.clone()), wire)
        } else {
            (json!(serial), serial.to_string())
        };
        let (tx, rx) = oneshot::channel();
        {
            // `closed` is set before fail_pending drains the map, so once
            // we hold this lock either closed is already visible or the
            // drain that follows our insert will complete this entry —
            // a call racing peer shutdown cannot be orphaned.
            let mut pending = self.state.pending.lock().unwrap();
            if self.state.closed.load(Ordering::Relaxed) {
                return Err(CallFailure::Transport("peer_closed".into()));
            }
            pending.map.insert(key.clone(), tx);
        }
        let frame = self.encode_frame(method, params, Some(wire_id));
        if writer.send(frame).is_err() {
            self.state.pending.lock().unwrap().map.remove(&key);
            return Err(CallFailure::Transport("writer_closed".into()));
        }
        Ok(PendingCall(rx))
    }

    /// Send a notification (no response expected).
    pub fn notify(&self, method: &str, params: Value) -> Result<(), CallFailure> {
        if self.state.closed.load(Ordering::Relaxed) {
            return Err(CallFailure::Transport("peer_closed".into()));
        }
        let Some(writer) = self.writer.lock().unwrap().clone() else {
            return Err(CallFailure::Transport("peer_closed".into()));
        };
        let frame = self.encode_frame(method, params, None);
        writer
            .send(frame)
            .map_err(|_| CallFailure::Transport("writer_closed".into()))
    }

    /// Wait for transport closure, including explicit close after an ack.
    pub async fn closed(&self) {
        loop {
            let notice = self.state.closed_notice.notified();
            tokio::pin!(notice);
            notice.as_mut().enable();
            if self.state.closed.load(Ordering::Relaxed) {
                return;
            }
            notice.await;
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
        // EOF or an unreadable frame already marks calls closed. Transport
        // tasks still need releasing when the owner closes that failed peer.
        // fail_pending and the ingress hooks are once-guarded, so a peer
        // that already failed through its reader or writer stays settled.
        self.state.fail_pending("peer_closed");
        self.writer.lock().unwrap().take();
        if let Some(task) = self.writer_task.lock().unwrap().take() {
            task.abort();
        }
        if let Some(task) = self.reader.lock().unwrap().take() {
            task.abort();
        }
        self.state.close_notifications();
    }
}
