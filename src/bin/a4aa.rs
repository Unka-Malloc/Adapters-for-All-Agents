//! `a4aa` — Adapter-For-All-Agents sidecar.
//!
//! `a4aa serve` runs an NDJSON JSON-RPC 2.0 service on its own stdio so
//! hosts in any language reuse this crate's adapters verbatim:
//!
//! - `session/open`   `{spec: AgentSpec, cwd?, env?}` → `{sessionId, lane, capabilities}`
//! - `session/prompt` `{sessionId, text, model?, effort?}` → `TurnOutcome`
//! - `session/cancel` `{sessionId}` → `{}`
//! - `session/close`  `{sessionId}` → `{}`
//! - `ping`           `{}` → `{protocol: "a4aa.v1", version}`
//!
//! During a prompt the server streams `session/event` notifications
//! `{sessionId, event}` and may call `permission/request` (client→answer
//! `{cancelled: true}` or `{selected: "<optionId>"}`).
//!
//! `a4aa fixture-agent` is a minimal deterministic ACP agent used by this
//! crate's tests and available to host integration tests: it echoes
//! prompts back as `agent_message_chunk` updates, exercises permission and
//! cancel paths on magic words, and never touches the network.

use a4aa::{
    Adapter, AgentEvent, AgentSession, AgentSpec, JsonRpcPeer, PermissionDecision,
    PermissionHandler, PermissionPrompt, RpcDelegate, RpcErrorObject, SessionRequest, TurnRequest,
};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex};

const PROTOCOL: &str = "a4aa.v1";
const VERSION: &str = env!("CARGO_PKG_VERSION");

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let mode = std::env::args().nth(1).unwrap_or_default();
    let code = match mode.as_str() {
        "serve" => serve().await,
        "fixture-agent" => fixture_agent(),
        _ => {
            eprintln!("usage: a4aa serve | a4aa fixture-agent");
            2
        }
    };
    std::process::exit(code);
}

// ------------------------------ serve ------------------------------

struct Sessions {
    inner: Mutex<HashMap<String, Arc<AgentSession>>>,
    next: std::sync::atomic::AtomicU64,
}

impl Sessions {
    fn new() -> Self {
        Self {
            inner: Mutex::new(HashMap::new()),
            next: std::sync::atomic::AtomicU64::new(1),
        }
    }
}

struct SidecarDelegate {
    sessions: Arc<Sessions>,
    peer: Arc<std::sync::OnceLock<Arc<JsonRpcPeer>>>,
    peer_ready: Arc<tokio::sync::Notify>,
    closed: Arc<tokio::sync::Notify>,
}

/// Waits for the shared peer cell; installed immediately after spawn.
async fn wait_peer(
    cell: &Arc<std::sync::OnceLock<Arc<JsonRpcPeer>>>,
    ready: &Arc<tokio::sync::Notify>,
) -> Arc<JsonRpcPeer> {
    loop {
        if let Some(peer) = cell.get() {
            return peer.clone();
        }
        ready.notified().await;
    }
}

impl SidecarDelegate {
    async fn peer(&self) -> Arc<JsonRpcPeer> {
        wait_peer(&self.peer, &self.peer_ready).await
    }

    /// Forward one turn's events as `session/event` notifications.
    async fn forwarder(&self, session_id: String) -> mpsc::UnboundedSender<AgentEvent> {
        let (tx, mut rx) = mpsc::unbounded_channel::<AgentEvent>();
        let peer = self.peer().await;
        tokio::spawn(async move {
            while let Some(event) = rx.recv().await {
                let _ = peer.notify(
                    "session/event",
                    json!({
                        "sessionId": session_id,
                        "event": serde_json::to_value(&event).unwrap_or(Value::Null),
                    }),
                );
            }
        });
        tx
    }
}

/// Permission bridge: relay the ACP permission prompt to the sidecar's
/// client and await its decision.
struct RemotePermission {
    peer: Arc<std::sync::OnceLock<Arc<JsonRpcPeer>>>,
    peer_ready: Arc<tokio::sync::Notify>,
}

#[async_trait::async_trait]
impl PermissionHandler for RemotePermission {
    async fn decide(&self, prompt: PermissionPrompt) -> PermissionDecision {
        let peer = wait_peer(&self.peer, &self.peer_ready).await;
        let params = json!({
            "sessionId": prompt.session_id,
            "toolCallId": prompt.tool_call_id,
            "title": prompt.title,
            "kind": prompt.kind,
            "contentTexts": prompt.content_texts,
            "options": prompt.options,
            "raw": prompt.raw,
        });
        let reply = peer.call("permission/request", params).await;
        match reply {
            Ok(value) => {
                if value.get("cancelled").and_then(Value::as_bool) == Some(true) {
                    return PermissionDecision::Cancelled;
                }
                match value.get("selected").and_then(Value::as_str) {
                    Some(option) => PermissionDecision::Selected(option.to_owned()),
                    None => PermissionDecision::Cancelled,
                }
            }
            Err(_) => PermissionDecision::Cancelled,
        }
    }
}

#[async_trait::async_trait]
impl RpcDelegate for SidecarDelegate {
    async fn on_request(&self, method: &str, params: Value) -> Result<Value, RpcErrorObject> {
        match method {
            "ping" => Ok(json!({"protocol": PROTOCOL, "version": VERSION})),
            "session/open" => self.session_open(params).await,
            "session/prompt" => self.session_prompt(params).await,
            "session/cancel" => self.session_cancel(params).await,
            "session/close" => self.session_close(params).await,
            _ => Err(RpcErrorObject::method_not_found(method)),
        }
    }

    async fn on_protocol_error(&self, error: String) {
        // Malformed/oversized inbound lines are diagnosed on stderr — the
        // protocol channel (stdout) must stay clean.
        eprintln!("a4aa: protocol error from host: {error}");
    }

    async fn on_closed(&self) {
        self.closed.notify_waiters();
    }
}

impl SidecarDelegate {
    async fn session_open(&self, params: Value) -> Result<Value, RpcErrorObject> {
        let spec: AgentSpec = serde_json::from_value(
            params.get("spec").cloned().unwrap_or(Value::Null),
        )
        .map_err(|error| RpcErrorObject {
            code: -32602,
            message: format!("invalid spec: {error}"),
            data: None,
        })?;
        // A remote sidecar client may not hand the agent every host
        // variable: inheritAll is a trusted-local-launcher option only.
        if matches!(spec.command.env, a4aa::EnvPolicy::InheritAll { .. }) {
            return Err(RpcErrorObject {
                code: -32602,
                message: "inheritAll env policy is not admitted over the sidecar".into(),
                data: None,
            });
        }
        let mut request = SessionRequest::default();
        if let Some(cwd) = params.get("cwd").and_then(Value::as_str) {
            request.cwd = Some(cwd.into());
        }
        if let Some(env) = params.get("env").and_then(Value::as_object) {
            request.env = env
                .iter()
                .filter_map(|(key, value)| value.as_str().map(|v| (key.clone(), v.to_owned())))
                .collect();
        }
        if let Some(resume) = params.get("resume").and_then(Value::as_str) {
            request.resume = Some(resume.to_owned());
        }

        let id = format!(
            "a4aa-{}",
            self.sessions
                .next
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        );
        let events = self.forwarder(id.clone()).await;
        let adapter = Adapter::new(spec).permission(Arc::new(RemotePermission {
            peer: self.peer.clone(),
            peer_ready: self.peer_ready.clone(),
        }));
        let session = adapter
            .open_session(request, events)
            .await
            .map_err(|error| RpcErrorObject {
                code: -32603,
                message: error.to_string(),
                data: None,
            })?;
        let capabilities = serde_json::to_value(match session.lane() {
            a4aa::LaneSession::Acp(session) => session.capabilities().clone(),
            a4aa::LaneSession::Rpc(session) => session.capabilities(),
            a4aa::LaneSession::Cli(_) => adapter.capabilities(),
        })
        .unwrap_or(Value::Null);
        let native_session_id = session.session_id().to_owned();
        self.sessions
            .inner
            .lock()
            .await
            .insert(id.clone(), Arc::new(session));
        Ok(json!({
            "sessionId": id,
            "nativeSessionId": native_session_id,
            "capabilities": capabilities,
        }))
    }

    async fn session_prompt(&self, params: Value) -> Result<Value, RpcErrorObject> {
        let session_id = params
            .get("sessionId")
            .and_then(Value::as_str)
            .ok_or_else(|| RpcErrorObject {
                code: -32602,
                message: "sessionId required".into(),
                data: None,
            })?;
        let text = params
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let session = {
            let sessions = self.sessions.inner.lock().await;
            sessions.get(session_id).cloned()
        }
        .ok_or_else(|| RpcErrorObject {
            code: -32602,
            message: format!("unknown session: {session_id}"),
            data: None,
        })?;
        let outcome = session
            .prompt(TurnRequest {
                text,
                model: params
                    .get("model")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                effort: params
                    .get("effort")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
            })
            .await
            .map_err(|error| RpcErrorObject {
                code: -32603,
                message: error.to_string(),
                data: None,
            })?;
        Ok(serde_json::to_value(outcome).unwrap_or(Value::Null))
    }

    async fn session_cancel(&self, params: Value) -> Result<Value, RpcErrorObject> {
        let session_id = params
            .get("sessionId")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let session = {
            let sessions = self.sessions.inner.lock().await;
            sessions.get(session_id).cloned()
        };
        match session {
            Some(session) => {
                session.cancel().await.map_err(|error| RpcErrorObject {
                    code: -32603,
                    message: error.to_string(),
                    data: None,
                })?;
                Ok(json!({}))
            }
            None => Err(RpcErrorObject {
                code: -32602,
                message: format!("unknown session: {session_id}"),
                data: None,
            }),
        }
    }

    async fn session_close(&self, params: Value) -> Result<Value, RpcErrorObject> {
        let session_id = params
            .get("sessionId")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let session = self.sessions.inner.lock().await.remove(&session_id);
        match session {
            Some(session) => {
                match Arc::try_unwrap(session) {
                    Ok(session) => session.close().await,
                    Err(_) => Ok(()), // still referenced by an in-flight turn
                }
                .map_err(|error| RpcErrorObject {
                    code: -32603,
                    message: error.to_string(),
                    data: None,
                })?;
                Ok(json!({}))
            }
            None => Ok(json!({})),
        }
    }
}

async fn serve() -> i32 {
    let closed = Arc::new(tokio::sync::Notify::new());
    let delegate = Arc::new(SidecarDelegate {
        sessions: Arc::new(Sessions::new()),
        peer: Arc::new(std::sync::OnceLock::new()),
        peer_ready: Arc::new(tokio::sync::Notify::new()),
        closed: closed.clone(),
    });
    let peer = JsonRpcPeer::spawn(
        tokio::io::stdin(),
        tokio::io::stdout(),
        delegate.clone(),
        a4aa::observe::none(),
        a4aa::framing::DEFAULT_MAX_MESSAGE_BYTES,
    );
    let _ = delegate.peer.set(peer);
    delegate.peer_ready.notify_waiters();
    closed.notified().await;

    // stdin closed: close every session, then finish pending calls.
    let sessions: Vec<Arc<AgentSession>> = {
        let mut sessions = delegate.sessions.inner.lock().await;
        sessions.drain().map(|(_, session)| session).collect()
    };
    for session in sessions {
        if let Ok(session) = Arc::try_unwrap(session) {
            let _ = session.close().await;
        }
    }
    0
}

// --------------------------- fixture-agent ---------------------------

/// Minimal ACP agent for deterministic tests. Protocol: ACP v1 over NDJSON.
///
/// Behaviors:
/// - `initialize` → protocolVersion 1, loadSession true.
/// - `session/new|load|resume` → fixed/echoed session id.
/// - `session/prompt` → two `agent_message_chunk` updates echoing the text,
///   then `{"stopReason":"end_turn"}`. Magic words:
///   - contains `permission` → first issues `session/request_permission`
///     (option `allow`/`deny`); `deny` selection ends with `refusal`.
///   - contains `wait-for-cancel` → streams chunks until `session/cancel`,
///     then `{"stopReason":"cancelled"}`.
fn fixture_agent() -> i32 {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    // A prompt that streams until `session/cancel` arrives: the response is
    // deferred and answered with `cancelled`.
    let mut deferred_prompt: Option<(Value, String)> = None;
    // A prompt blocked on a permission reply: (prompt id, text, session id).
    // Resolved by the response frame with id 9000.
    let mut pending_permission: Option<(Value, String, String)> = None;

    let write = |stdout: &std::io::Stdout, value: Value| {
        let mut out = stdout.lock();
        let _ = out.write_all(value.to_string().as_bytes());
        let _ = out.write_all(b"\n");
        let _ = out.flush();
    };

    // Finish a prompt: echo chunks then the final response.
    fn finish_prompt(
        stdout: &std::io::Stdout,
        prompt_id: Value,
        session_id: &str,
        text: &str,
        granted: bool,
    ) {
        let write = |value: Value| {
            let mut out = stdout.lock();
            let _ = out.write_all(value.to_string().as_bytes());
            let _ = out.write_all(b"\n");
            let _ = out.flush();
        };
        let chunk = if granted {
            format!("granted: {text}")
        } else {
            format!("echo: {text}")
        };
        let mut mid = chunk.len() / 2;
        while !chunk.is_char_boundary(mid) {
            mid -= 1;
        }
        for piece in [&chunk[..mid], &chunk[mid..]] {
            write(json!({
                "jsonrpc": "2.0",
                "method": "session/update",
                "params": {
                    "sessionId": session_id,
                    "update": {"sessionUpdate": "agent_message_chunk",
                               "content": {"type": "text", "text": piece}}
                }
            }));
        }
        write(json!({
            "jsonrpc": "2.0", "id": prompt_id,
            "result": {
                "stopReason": if text.contains("permission") && !granted {
                    "refusal"
                } else {
                    "end_turn"
                }
            }
        }));
    }

    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let Ok(frame) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        // Request ids are arbitrary JSON values — echo them back verbatim.
        let id = frame.get("id").cloned();
        let method = frame.get("method").and_then(Value::as_str).unwrap_or("");
        let params = frame.get("params").cloned().unwrap_or(Value::Null);

        // Our own outbound request (permission) answers arrive as plain
        // response frames — finish the deferred prompt.
        if method.is_empty() && id == Some(json!(9_000)) {
            if let Some((prompt_id, text, session_id)) = pending_permission.take() {
                // Response shape: {"result": {"outcome": {"outcome":
                // "selected", "optionId": "allow"}}} — the internally-tagged
                // enum nests under the response's own `outcome` field.
                let result = frame.get("result").cloned().unwrap_or(Value::Null);
                let outcome = result.get("outcome").cloned().unwrap_or(Value::Null);
                let granted = outcome
                    .get("outcome")
                    .and_then(Value::as_str)
                    .is_some_and(|kind| kind == "selected")
                    && outcome.get("optionId").and_then(Value::as_str) == Some("allow");
                finish_prompt(&stdout, prompt_id, &session_id, &text, granted);
            }
            continue;
        }

        match method {
            "initialize" => write(
                &stdout,
                json!({
                    "jsonrpc": "2.0", "id": id,
                    "result": {
                        "protocolVersion": 1,
                        "agentCapabilities": {
                            "loadSession": true,
                            "promptCapabilities": {"image": false, "audio": false, "embeddedContext": true},
                            "mcpCapabilities": {"http": false, "sse": false}
                        },
                        "agentInfo": {"name": "a4aa-fixture", "version": VERSION},
                        "authMethods": []
                    }
                }),
            ),
            "session/new" => write(
                &stdout,
                json!({
                    "jsonrpc": "2.0", "id": id,
                    "result": {"sessionId": "fixture-session-1"}
                }),
            ),
            "session/load" | "session/resume" => write(
                &stdout,
                json!({
                    "jsonrpc": "2.0", "id": id,
                    "result": {"sessionId": params.get("sessionId").cloned().unwrap_or(json!("fixture-resumed"))}
                }),
            ),
            "session/cancel" => {
                if let Some((prompt_id, _)) = deferred_prompt.take() {
                    write(
                        &stdout,
                        json!({
                            "jsonrpc": "2.0", "id": prompt_id,
                            "result": {"stopReason": "cancelled"}
                        }),
                    );
                }
            }
            "session/prompt" => {
                let prompt_text = params
                    .get("prompt")
                    .and_then(Value::as_array)
                    .and_then(|blocks| blocks.first())
                    .and_then(|b| b.get("text"))
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
                let session_id = params
                    .get("sessionId")
                    .and_then(Value::as_str)
                    .unwrap_or("fixture-session-1")
                    .to_owned();

                if prompt_text.contains("permission") {
                    // Issue a permission request; the response frame arrives
                    // on a later loop iteration (matched by id 9000 above).
                    write(
                        &stdout,
                        json!({
                            "jsonrpc": "2.0", "id": 9000,
                            "method": "session/request_permission",
                            "params": {
                                "sessionId": session_id,
                                "toolCall": {
                                    "toolCallId": "fixture-call-1",
                                    "title": "Fixture tool call",
                                    "kind": "execute",
                                    "content": [],
                                    "locations": []
                                },
                                "options": [
                                    {"optionId": "allow", "name": "Allow", "kind": "allow_once"},
                                    {"optionId": "deny", "name": "Deny", "kind": "reject_once"}
                                ]
                            }
                        }),
                    );
                    pending_permission = Some((id.unwrap_or(Value::Null), prompt_text, session_id));
                    continue;
                }

                if prompt_text.contains("wait-for-cancel") {
                    // Stream a burst of chunks, then defer the response until
                    // `session/cancel` arrives (handled above).
                    for index in 0..3 {
                        write(
                            &stdout,
                            json!({
                                "jsonrpc": "2.0",
                                "method": "session/update",
                                "params": {
                                    "sessionId": session_id,
                                    "update": {"sessionUpdate": "agent_message_chunk",
                                               "content": {"type": "text", "text": format!("tick-{index} ")}}
                                }
                            }),
                        );
                    }
                    deferred_prompt = Some((id.unwrap_or(Value::Null), session_id));
                    continue;
                }
                finish_prompt(
                    &stdout,
                    id.unwrap_or(Value::Null),
                    &session_id,
                    &prompt_text,
                    false,
                );
            }
            _ => {
                // Notifications carry no id — never answer them.
                if let Some(id) = id {
                    write(
                        &stdout,
                        json!({
                            "jsonrpc": "2.0", "id": id,
                            "error": {"code": -32601, "message": "method not found"}
                        }),
                    );
                }
            }
        }
    }
    0
}
