//! `a4aa` — Adapter-For-All-Agents sidecar.
//!
//! `a4aa serve` runs an NDJSON JSON-RPC 2.0 service on its own stdio so
//! hosts in any language reuse this crate's adapters verbatim:
//!
//! - `session/open`   `{spec: AgentSpec, cwd?, env?, resume?, options?}`
//!   → `{sessionId, nativeSessionId, capabilities, agentInfo?, sessionState?}`
//!   where `options` is `{model?, mode?, effort?, permission?}` applied
//!   after session establishment on lanes that support it.
//! - `session/prompt` `{sessionId, text, model?, effort?}` → `TurnOutcome`
//! - `session/cancel` `{sessionId}` → `{}`
//! - `session/close`  `{sessionId}` → `{}`
//! - `ping`           `{}` → `{protocol: "a4aa.v1", version}`
//!
//! Errors carry `{code, stage, retryable, uncertainEffect}` in `data` so
//! hosts distinguish recovery-required transport failures from permanent
//! configuration errors without parsing message text.
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
    Adapter, AdapterError, AgentEvent, AgentSession, AgentSpec, JsonRpcPeer, PermissionDecision,
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

/// The sidecar-visible form of a typed `AdapterError`: the host reads
/// `data.code`/`data.stage` to decide recovery policy instead of parsing
/// message text.
fn adapter_error(error: &AdapterError) -> RpcErrorObject {
    RpcErrorObject {
        code: -32603,
        message: error.to_string(),
        data: Some(json!({
            "code": error.code,
            "stage": error.stage,
            "retryable": error.retryable,
            "uncertainEffect": error.uncertain_effect,
        })),
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
        // A remote sidecar client may not read host variables by name:
        // InheritAll is a trusted-local-launcher option, and per-name
        // inheritance or `hostEnv` indirections are confined to the spawn
        // allowlist — otherwise any client could exfiltrate host
        // credentials through the agent's environment.
        if !env_admitted_over_sidecar(&spec.command.env) {
            return Err(RpcErrorObject {
                code: -32602,
                message: "env policy not admitted over the sidecar: inheritAll, \
                          non-allowlisted inherit names, hostEnv outside the allowlist, \
                          and secret references are refused"
                    .into(),
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
        if let Some(options) = params.get("options").and_then(Value::as_object) {
            request.config.model = options
                .get("model")
                .and_then(Value::as_str)
                .map(str::to_owned);
            request.config.mode = options
                .get("mode")
                .and_then(Value::as_str)
                .map(str::to_owned);
            request.config.effort = options
                .get("effort")
                .and_then(Value::as_str)
                .map(str::to_owned);
            request.config.permission = options
                .get("permission")
                .and_then(Value::as_str)
                .map(str::to_owned);
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
            .map_err(|error| adapter_error(&error))?;
        let capabilities = serde_json::to_value(match session.lane() {
            a4aa::LaneSession::Acp(session) => session.capabilities().clone(),
            a4aa::LaneSession::Rpc(session) => session.capabilities(),
            a4aa::LaneSession::Cli(_) => adapter.capabilities(),
        })
        .unwrap_or(Value::Null);
        let (agent_info, session_state) = match session.lane() {
            a4aa::LaneSession::Acp(session) => (
                session
                    .agent_info()
                    .map(|info| json!({"name": info.name, "version": info.version})),
                Some(session.session_state()),
            ),
            _ => (None, None),
        };
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
            "agentInfo": agent_info.unwrap_or(Value::Null),
            "sessionState": session_state.unwrap_or(Value::Null),
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
            .map_err(|error| adapter_error(&error))?;
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
                session
                    .cancel()
                    .await
                    .map_err(|error| adapter_error(&error))?;
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
        let session = self.sessions.inner.lock().await.get(&session_id).cloned();
        match session {
            Some(session) => {
                session
                    .close()
                    .await
                    .map_err(|error| adapter_error(&error))?;
                self.sessions.inner.lock().await.remove(&session_id);
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
    let mut exit = 0;
    for session in sessions {
        if let Err(error) = session.close().await {
            eprintln!("session shutdown failed: {error}");
            exit = 1;
        }
    }
    exit
}

// --------------------------- fixture-agent ---------------------------

/// Minimal ACP agent for deterministic tests. Protocol: ACP v1 over NDJSON.
///
/// Behaviors:
/// - `initialize` → protocolVersion 1, loadSession true.
/// - `session/new|load|resume` → fixed/echoed session id plus the fixture's
///   configuration surfaces: `configOptions` (select lists for `model`,
///   `mode`, `thought_level`, `permissions`) and legacy `modes`/`models`
///   states. `A4AA_FIXTURE_LEGACY` drops the `model`/`mode` configOptions
///   so only the legacy surfaces carry those slots.
/// - `session/set_config_option`, `session/set_mode`, `session/set_model`
///   update the fixture's advertised state and echo it back.
/// - `A4AA_FIXTURE_AUTH` advertises auth method `fixture-auth` and fails
///   session establishment until `authenticate` succeeds for it.
/// - `session/prompt` → two `agent_message_chunk` updates echoing the text,
///   then `{"stopReason":"end_turn"}`. Magic words:
///   - `config-check` → echoes the current selections as
///     `config:model=X,mode=Y,effort=Z,permission=W`.
///   - contains `permission` → first issues `session/request_permission`
///     (option `allow`/`deny`); `deny` selection ends with `refusal`.
///   - contains `wait-for-cancel` → streams chunks until `session/cancel`,
///     then `{"stopReason":"cancelled"}`.
fn fixture_agent() -> i32 {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let legacy = std::env::var_os("A4AA_FIXTURE_LEGACY").is_some();
    let restore_unknown = std::env::var_os("A4AA_FIXTURE_RESTORE_UNKNOWN").is_some();
    let model_reply = std::env::var("A4AA_FIXTURE_MODEL_REPLY").unwrap_or_default();
    let require_auth = std::env::var_os("A4AA_FIXTURE_AUTH").is_some();
    let mut authenticated = !require_auth;
    // Current session configuration selections, keyed by slot.
    let mut selections = std::collections::BTreeMap::<String, String>::from([
        ("model".to_owned(), "fixture-small".to_owned()),
        ("mode".to_owned(), "code".to_owned()),
        ("thought_level".to_owned(), "low".to_owned()),
        ("permissions".to_owned(), "safe".to_owned()),
    ]);
    let option_id = |slot: &str| format!("cfg-{slot}");
    let option_values = |slot: &str| -> Vec<(&str, &str)> {
        match slot {
            "model" => vec![
                ("fixture-small", "Fixture Small"),
                ("fixture-large", "Fixture Large"),
            ],
            "mode" => vec![("code", "Code"), ("ask", "Ask")],
            "thought_level" => vec![("low", "Low"), ("high", "High")],
            "permissions" => vec![("safe", "Safe"), ("open", "Open")],
            _ => vec![],
        }
    };
    let config_options = |selections: &std::collections::BTreeMap<String, String>, legacy: bool| {
        let mut options = Vec::new();
        for slot in ["model", "mode", "thought_level", "permissions"] {
            if legacy && (slot == "model" || slot == "mode") {
                continue;
            }
            let values = option_values(slot)
                .into_iter()
                .map(|(value, name)| json!({"value": value, "name": name}))
                .collect::<Vec<_>>();
            options.push(json!({
                "id": option_id(slot),
                "name": slot,
                "category": slot,
                "type": "select",
                "currentValue": selections.get(slot).cloned().unwrap_or_default(),
                "options": values,
            }));
        }
        Value::Array(options)
    };
    let modes_state = |selections: &std::collections::BTreeMap<String, String>| {
        json!({
            "currentModeId": selections.get("mode").cloned().unwrap_or_default(),
            "availableModes": [
                {"id": "code", "name": "Code"},
                {"id": "ask", "name": "Ask"}
            ]
        })
    };
    let models_state = |selections: &std::collections::BTreeMap<String, String>| {
        json!({
            "currentModelId": selections.get("model").cloned().unwrap_or_default(),
            "availableModels": [
                {"modelId": "fixture-small", "name": "Fixture Small"},
                {"modelId": "fixture-large", "name": "Fixture Large"}
            ]
        })
    };
    let session_state = |session_id: Value,
                         selections: &std::collections::BTreeMap<String, String>,
                         legacy: bool| {
        json!({
            "sessionId": session_id,
            "configOptions": config_options(selections, legacy),
            "modes": modes_state(selections),
            "models": models_state(selections),
        })
    };
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
                        "authMethods": if require_auth {
                            json!([{"id": "fixture-auth", "name": "Fixture Auth"}])
                        } else {
                            json!([])
                        }
                    }
                }),
            ),
            "authenticate" => {
                let ok = params.get("methodId").and_then(Value::as_str) == Some("fixture-auth");
                if ok {
                    authenticated = true;
                }
                write(
                    &stdout,
                    json!({
                        "jsonrpc": "2.0", "id": id,
                        "result": {}
                    }),
                );
            }
            "session/new" => {
                if !authenticated {
                    write(
                        &stdout,
                        json!({
                            "jsonrpc": "2.0", "id": id,
                            "error": {"code": -32000, "message": "authentication required"}
                        }),
                    );
                    continue;
                }
                write(
                    &stdout,
                    json!({
                        "jsonrpc": "2.0", "id": id,
                        "result": session_state(json!("fixture-session-1"), &selections, legacy)
                    }),
                );
            }
            "session/load" | "session/resume" => {
                if !authenticated {
                    write(
                        &stdout,
                        json!({
                            "jsonrpc": "2.0", "id": id,
                            "error": {"code": -32000, "message": "authentication required"}
                        }),
                    );
                    continue;
                }
                write(
                    &stdout,
                    json!({
                        "jsonrpc": "2.0", "id": id,
                        "result": if restore_unknown { json!({}) } else { session_state(
                            params.get("sessionId").cloned().unwrap_or(json!("fixture-resumed")),
                            &selections, legacy) }
                    }),
                );
            }
            "session/set_config_option" => {
                let config_id = params.get("configId").and_then(Value::as_str).unwrap_or("");
                let value = params.get("value").and_then(Value::as_str).unwrap_or("");
                let slot = config_id.strip_prefix("cfg-").unwrap_or("");
                if option_values(slot).iter().any(|(v, _)| *v == value) {
                    selections.insert(slot.to_owned(), value.to_owned());
                    write(
                        &stdout,
                        json!({
                            "jsonrpc": "2.0", "id": id,
                            "result": {"configOptions": config_options(&selections, legacy)}
                        }),
                    );
                } else {
                    write(
                        &stdout,
                        json!({
                            "jsonrpc": "2.0", "id": id,
                            "error": {"code": -32602, "message": format!("unknown option value: {config_id}")}
                        }),
                    );
                }
            }
            "session/set_mode" => {
                let mode = params.get("modeId").and_then(Value::as_str).unwrap_or("");
                if option_values("mode").iter().any(|(v, _)| *v == mode) {
                    selections.insert("mode".to_owned(), mode.to_owned());
                    write(&stdout, json!({"jsonrpc": "2.0", "id": id, "result": {}}));
                } else {
                    write(
                        &stdout,
                        json!({"jsonrpc": "2.0", "id": id,
                        "error": {"code": -32602, "message": format!("unknown mode: {mode}")}}),
                    );
                }
            }
            "session/set_model" => {
                let model = params.get("modelId").and_then(Value::as_str).unwrap_or("");
                if option_values("model").iter().any(|(v, _)| *v == model) {
                    selections.insert("model".to_owned(), model.to_owned());
                    let response = match model_reply.as_str() {
                        "empty" => json!({}),
                        "mismatch" => {
                            selections.insert("model".to_owned(), "fixture-small".to_owned());
                            json!({"models":models_state(&selections)})
                        }
                        "malformed" => json!({"models":{"currentModelId":42}}),
                        _ => json!({"models":models_state(&selections)}),
                    };
                    write(
                        &stdout,
                        json!({"jsonrpc": "2.0", "id": id,
                            "result": response}),
                    );
                } else {
                    write(
                        &stdout,
                        json!({"jsonrpc": "2.0", "id": id,
                        "error": {"code": -32602, "message": format!("unknown model: {model}")}}),
                    );
                }
            }
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

                if prompt_text.contains("config-check") {
                    finish_prompt(
                        &stdout,
                        id.unwrap_or(Value::Null),
                        &session_id,
                        &format!(
                            "config:model={},mode={},effort={},perm={}",
                            selections.get("model").cloned().unwrap_or_default(),
                            selections.get("mode").cloned().unwrap_or_default(),
                            selections.get("thought_level").cloned().unwrap_or_default(),
                            selections.get("permissions").cloned().unwrap_or_default(),
                        ),
                        false,
                    );
                    continue;
                }

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

/// Whether a spec's env policy may cross the sidecar boundary. The remote
/// client is a same-uid peer we must not hand host state to: only the
/// documented spawn allowlist may be inherited by name.
fn env_admitted_over_sidecar(env: &a4aa::EnvPolicy) -> bool {
    let allowed = |name: &str| a4aa::env::INHERITED_ENV.contains(&name);
    match env {
        a4aa::EnvPolicy::InheritAll { .. } => false,
        a4aa::EnvPolicy::Sanitized { inherit, set } => {
            inherit.iter().all(|name| allowed(name))
                && set.values().all(|value| match value {
                    a4aa::EnvValue::Literal(_) => true,
                    a4aa::EnvValue::Source(a4aa::EnvSource::HostEnv { host_env }) => {
                        allowed(host_env)
                    }
                    a4aa::EnvValue::Source(a4aa::EnvSource::Secret { .. }) => false,
                })
        }
    }
}
