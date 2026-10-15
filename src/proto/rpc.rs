//! Generic JSON-RPC lane for stdio/RPC agents.
//!
//! Hosts declare the agent's method surface as templates (`{prompt}`,
//! `{sessionId}`, `{turnId}`, `{model}`, `{effort}`, `{instructions}`,
//! `{cwd}` are substituted into string leaves of the params value). Inbound
//! notifications map onto [`AgentEvent`]: the configured `textEvents` method
//! carries text, any other notification arrives raw — nothing is dropped.
//!
//! Turn semantics: `session/open` (if configured) runs once at open —
//! `sessionResume` replaces it when the request carries a binding;
//! `prompt` is awaited per turn, and when `completion` is declared the named
//! notification (not the call response) ends the turn; `cancel` (if
//! configured) is sent as a notification on explicit cancel only.

use serde_json::Value;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use tokio::sync::{broadcast, Mutex};

use crate::bounds;
use crate::env::SecretResolver;
use crate::error::AdapterError;
use crate::model::RpcCallSpec;
use crate::model::{
    AgentEvent, Capabilities, CommandSpec, LaneSpec, RpcLaneSpec, SelectorPhase,
    SelectorSupportSet, SessionRequest, SessionScope, TurnOutcome,
};
use crate::observe::SharedObserver;
use crate::rpc::{CallFailure, JsonRpcPeer, RpcDelegate, RpcErrorObject};
use crate::supervision::SupervisedChild;
use crate::template::{extract_path, substitute, vars, VarSpec, Vars};
use crate::transport::stdio;

/// Host callbacks for an RPC session.
#[async_trait::async_trait]
pub trait RpcSessionDelegate: Send + Sync {
    async fn on_event(&self, _event: AgentEvent) {}
    /// Agent→host requests. Default: `method not found` — fail closed.
    async fn on_request(&self, method: &str, _params: Value) -> Result<Value, RpcErrorObject> {
        Err(RpcErrorObject::method_not_found(method))
    }
}

pub struct RpcLane {
    pub command: CommandSpec,
    pub spec: RpcLaneSpec,
    pub secrets: Arc<dyn SecretResolver>,
    pub observer: SharedObserver,
}

/// One open RPC session: child process + peer + negotiated session id.
pub struct RpcSession {
    peer: Arc<JsonRpcPeer>,
    child: Mutex<SupervisedChild>,
    spec: RpcLaneSpec,
    session_model: Option<String>,
    session_effort: Option<String>,
    session_id: Option<String>,
    cwd: String,
    /// Inbound notifications, for awaiting declared completion methods.
    notifications: broadcast::Sender<(String, Value)>,
    /// Turn id captured from the in-flight prompt response; `{turnId}`
    /// substitutes it in `cancel` params.
    active_turn: std::sync::Mutex<Option<String>>,
    /// Set by `kill_on_cancel` teardown so the in-flight turn resolves as
    /// `cancelled` rather than a transport failure.
    cancelled: std::sync::atomic::AtomicBool,
    /// Serializes prompt turns.
    turn_lock: Mutex<()>,
}

impl RpcLane {
    pub async fn open(
        &self,
        request: SessionRequest,
        delegate: Arc<dyn RpcSessionDelegate>,
    ) -> Result<RpcSession, AdapterError> {
        let env = crate::env::resolve(&self.command.env, &request.env, &*self.secrets).await?;
        let mut command = SupervisedChild::command(&self.command, &env, request.cwd.as_deref());
        let mut child = SupervisedChild::spawn(&mut command, stdio::PROTOCOL_IO)?;
        let (stdin, stdout, stderr) = stdio::protocol_halves(&mut child)?;
        if let Some(stderr) = stderr {
            tokio::spawn(bounds::drain_stderr(stderr, self.observer.clone()));
        }
        let text_method = self.spec.text_event_method.clone();
        let observer = self.observer.clone();
        let (notifications, _) = broadcast::channel(64);
        struct Inner {
            delegate: Arc<dyn RpcSessionDelegate>,
            text_method: Option<String>,
            text_path: Option<String>,
            notifications: broadcast::Sender<(String, Value)>,
        }
        #[async_trait::async_trait]
        impl RpcDelegate for Inner {
            async fn on_request(
                &self,
                method: &str,
                params: Value,
            ) -> Result<Value, RpcErrorObject> {
                self.delegate.on_request(method, params).await
            }
            async fn on_closed(&self) {
                let _ = self.notifications.send((String::new(), Value::Null));
            }
            async fn on_notification(&self, method: &str, params: Value) {
                let _ = self.notifications.send((method.to_owned(), params.clone()));
                let event = if Some(method) == self.text_method.as_deref() {
                    notification_text(&params, self.text_path.as_deref())
                        .map(|text| AgentEvent::MessageChunk { text })
                } else {
                    None
                };
                self.delegate
                    .on_event(event.unwrap_or(AgentEvent::Raw {
                        raw: serde_json::json!({"method": method, "params": params}),
                    }))
                    .await;
            }
        }
        let peer = JsonRpcPeer::spawn_with_envelope(
            stdout,
            stdin,
            Arc::new(Inner {
                delegate,
                text_method,
                text_path: self.spec.text_path.clone(),
                notifications: notifications.clone(),
            }),
            observer,
            crate::framing::DEFAULT_MAX_MESSAGE_BYTES,
            self.spec.envelope.clone(),
        );

        let cwd = request
            .cwd
            .clone()
            .or_else(|| self.command.cwd.clone())
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| std::env::temp_dir().display().to_string());

        if let Some(initialize) = &self.spec.initialize {
            call(
                &peer,
                initialize,
                &vars(VarSpec {
                    cwd: &cwd,
                    model: request.config.model.as_deref(),
                    effort: request.config.effort.as_deref(),
                    mode: request.config.mode.as_deref(),
                    instructions: request.config.instructions.as_deref(),
                    permission: request.config.permission.as_deref(),
                    provider: request.config.provider.as_deref(),
                    ..Default::default()
                }),
            )
            .await?;
            if let Some(initialized) = &self.spec.initialized {
                let _ = peer.notify(initialized, Value::Object(serde_json::Map::new()));
            }
        }

        let resume = request.resume.as_deref().filter(|v| !v.trim().is_empty());
        let instructions = request.config.instructions.as_deref();
        let permission = request.config.permission.as_deref();
        let model = request.config.model.as_deref();
        let effort = request.config.effort.as_deref();
        let mode = request.config.mode.as_deref();
        // A requested resume binding the agent cannot express fails closed —
        // silently opening a fresh session would misattribute the host's
        // session state. A `Process`-scoped lane can always seed the wire
        // id from the binding, so it accepts resume without a resume call.
        let resume_expressible =
            self.spec.session_resume.is_some() || self.spec.session_scope == SessionScope::Process;
        if resume.is_some() && !resume_expressible {
            return Err(AdapterError::permanent(
                "agent_session_resume_unsupported",
                "session/open",
            ));
        }
        let open_call = resume
            .and(self.spec.session_resume.as_ref())
            .or(self.spec.session_open.as_ref());
        let mut session_id = None;
        if self.spec.session_scope == SessionScope::Process {
            // Process-scoped ids never repeat across spawns: a durable
            // store that rejects cross-process reuse stays honoured.
            session_id = Some(scoped_session_id(resume));
        } else if let Some(open) = open_call {
            let result = call(
                &peer,
                open,
                &vars(VarSpec {
                    session_id: resume,
                    cwd: &cwd,
                    model,
                    effort,
                    mode,
                    instructions,
                    permission,
                    provider: request.config.provider.as_deref(),
                    ..Default::default()
                }),
            )
            .await?;
            session_id = resume.map(str::to_owned).or_else(|| binding(&result));
        }

        Ok(RpcSession {
            peer,
            child: Mutex::new(child),
            spec: self.spec.clone(),
            session_model: request.config.model.clone(),
            session_effort: request.config.effort.clone(),
            session_id,
            cwd,
            notifications,
            active_turn: std::sync::Mutex::new(None),
            cancelled: std::sync::atomic::AtomicBool::new(false),
            turn_lock: Mutex::new(()),
        })
    }
}

impl RpcSession {
    pub fn session_id(&self) -> Option<&str> {
        self.session_id.as_deref()
    }

    pub fn capabilities(&self) -> Capabilities {
        self.spec.capabilities()
    }

    pub fn selector_support(&self, phase: SelectorPhase) -> SelectorSupportSet {
        LaneSpec::Rpc(Box::new(self.spec.clone())).selector_support(phase)
    }

    /// Run one prompt turn. Serialized per session.
    pub async fn prompt(
        &self,
        text: &str,
        model: Option<&str>,
        effort: Option<&str>,
    ) -> Result<TurnOutcome, AdapterError> {
        let _guard = self.turn_lock.lock().await;
        let model = model.or(self.session_model.as_deref());
        let effort = effort.or(self.session_effort.as_deref());
        let vars = vars(VarSpec {
            session_id: self.session_id.as_deref(),
            cwd: &self.cwd,
            prompt: text,
            model,
            effort,
            ..Default::default()
        });
        // Per-turn configuration calls (for example model/thinking-level
        // selection on flat-envelope agents). A call whose params template
        // resolved to an empty object is skipped — nothing was requested.
        for pre in &self.spec.pre_turn {
            let params = substitute(&pre.params, &vars);
            let empty = params.as_object().is_some_and(|map| map.is_empty());
            let template_empty = pre.params.as_object().is_some_and(|map| map.is_empty());
            if empty && !template_empty {
                continue;
            }
            self.peer
                .call(&pre.method, params)
                .await
                .map_err(call_failure)?;
        }
        // Subscribe before the call: a completion emitted between the
        // response and subscription must not be missed.
        let feed = self
            .spec
            .completion
            .is_some()
            .then(|| self.notifications.subscribe());
        let result = match call(&self.peer, &self.spec.prompt, &vars).await {
            Ok(result) => result,
            Err(_) if self.cancelled.swap(false, Ordering::Relaxed) => {
                return Ok(TurnOutcome {
                    stop_reason: "cancelled".to_owned(),
                    text: String::new(),
                    truncated: false,
                    exit_code: None,
                    usage: None,
                });
            }
            Err(error) => return Err(error),
        };
        let turn_id = binding(&result);
        if let (Some(done), Some(feed)) = (&self.spec.completion, feed) {
            *self.active_turn.lock().unwrap_or_else(|e| e.into_inner()) = turn_id.clone();
            return self.await_completion(done, feed, turn_id.as_deref()).await;
        }
        let stop_reason = ["stopReason", "stop_reason", "status"]
            .iter()
            .find_map(|key| result.get(*key).and_then(Value::as_str))
            .unwrap_or("end_turn")
            .to_owned();
        Ok(TurnOutcome {
            stop_reason,
            text: extract_text(&result, self.spec.result_text_path.as_deref())
                .unwrap_or_else(|| result.to_string()),
            truncated: false,
            exit_code: None,
            usage: None,
        })
    }

    /// Await the declared completion notification, accumulating text-method
    /// payloads meanwhile so the outcome carries the same text the events
    /// already streamed.
    async fn await_completion(
        &self,
        done: &crate::model::RpcCompletionSpec,
        mut feed: broadcast::Receiver<(String, Value)>,
        turn_id: Option<&str>,
    ) -> Result<TurnOutcome, AdapterError> {
        let mut text = String::new();
        loop {
            match feed.recv().await {
                Ok((method, params)) if method == done.method => {
                    if let (Some(path), Some(want)) = (&done.turn_id_path, turn_id) {
                        if extract_text(&params, Some(path)).as_deref() != Some(want) {
                            continue;
                        }
                    }
                    let status = extract_text(&params, done.status_path.as_deref());
                    if let Some(want) = done.status_value.as_deref() {
                        if status.as_deref() != Some(want) {
                            continue;
                        }
                    }
                    *self.active_turn.lock().unwrap_or_else(|e| e.into_inner()) = None;
                    return Ok(TurnOutcome {
                        stop_reason: status.unwrap_or_else(|| "end_turn".to_owned()),
                        text,
                        truncated: false,
                        exit_code: None,
                        usage: None,
                    });
                }
                Ok((method, _)) if method.is_empty() => {
                    if self.cancelled.swap(false, Ordering::Relaxed) {
                        return Ok(TurnOutcome {
                            stop_reason: "cancelled".to_owned(),
                            text,
                            truncated: false,
                            exit_code: None,
                            usage: None,
                        });
                    }
                    return Err(AdapterError::uncertain(
                        "agent_transport_failed",
                        "rpc/read",
                    ));
                }
                Ok((method, params)) => {
                    if Some(method.as_str()) == self.spec.text_event_method.as_deref() {
                        if let Some(chunk) =
                            notification_text(&params, self.spec.text_path.as_deref())
                        {
                            text.push_str(&chunk);
                        }
                    }
                }
                // A lagged receiver re-syncs on the next notification; the
                // process stays supervised until it goes quiet for good.
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => {
                    if self.cancelled.swap(false, Ordering::Relaxed) {
                        return Ok(TurnOutcome {
                            stop_reason: "cancelled".to_owned(),
                            text,
                            truncated: false,
                            exit_code: None,
                            usage: None,
                        });
                    }
                    return Err(AdapterError::uncertain(
                        "agent_transport_failed",
                        "rpc/read",
                    ));
                }
            }
        }
    }

    /// Explicit user cancellation: sends the configured notification, or —
    /// on a `kill_on_cancel` lane — terminates the process, the agent's
    /// only honest cancel semantic. A killed session cannot run again.
    pub async fn cancel(&self) -> Result<(), AdapterError> {
        if self.spec.kill_on_cancel {
            self.cancelled.store(true, Ordering::Relaxed);
            self.child.lock().await.terminate().await?;
            return Ok(());
        }
        let Some(cancel) = &self.spec.cancel else {
            return Err(AdapterError::permanent(
                "agent_cancel_unsupported",
                "session/cancel",
            ));
        };
        let turn_id = self
            .active_turn
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let vars = vars(VarSpec {
            session_id: self.session_id.as_deref(),
            cwd: &self.cwd,
            turn_id: turn_id.as_deref(),
            ..Default::default()
        });
        let params = substitute(&cancel.params, &vars);
        self.peer.notify(&cancel.method, params).map_err(|error| {
            AdapterError::uncertain("agent_cancel_failed", "session/cancel")
                .with_detail(format!("{error:?}"))
        })
    }

    /// Invoke a declared passthrough method verbatim.
    pub async fn call_passthrough(
        &self,
        method: &str,
        params: Value,
    ) -> Result<Value, AdapterError> {
        if !self.spec.passthrough.iter().any(|entry| entry == method) {
            return Err(AdapterError::permanent(
                "agent_method_not_admitted",
                "rpc/call",
            ));
        }
        self.peer.call(method, params).await.map_err(call_failure)
    }

    pub async fn close(self) -> Result<(), AdapterError> {
        if let Some(close) = &self.spec.session_close {
            let vars = vars(VarSpec {
                session_id: self.session_id.as_deref(),
                cwd: &self.cwd,
                ..Default::default()
            });
            let params = substitute(&close.params, &vars);
            let _ = self.peer.call(&close.method, params).await;
        }
        self.peer.close().await;
        let mut child = self.child.lock().await;
        let _ = child.terminate().await;
        Ok(())
    }
}

async fn call(peer: &JsonRpcPeer, spec: &RpcCallSpec, vars: &Vars) -> Result<Value, AdapterError> {
    if spec.method.is_empty() {
        return Err(AdapterError::permanent("agent_method_unset", "rpc/call"));
    }
    peer.call(&spec.method, substitute(&spec.params, vars))
        .await
        .map_err(call_failure)
}

/// Mint a process-scoped wire session id: the host's durable binding is a
/// seed, never the wire value itself (the agent's store rejects reusing an
/// id across process lifetimes).
fn scoped_session_id(resume: Option<&str>) -> String {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|span| span.as_nanos())
        .unwrap_or_default();
    let nonce = format!(
        "{:08x}",
        ((nonce ^ std::process::id() as u128) & 0xffff_ffff) as u64
    );
    match resume {
        Some(id) => format!("{id}-{nonce}"),
        None => format!("a4aa-{nonce}"),
    }
}

fn call_failure(failure: CallFailure) -> AdapterError {
    match failure {
        CallFailure::Remote(error) => AdapterError::permanent("agent_remote_error", "rpc/call")
            .with_detail(format!("{} ({})", error.message, error.code)),
        CallFailure::Transport(reason) => {
            AdapterError::uncertain("agent_transport_failed", "rpc/call").with_detail(reason)
        }
    }
}

fn extract_text(result: &Value, path: Option<&str>) -> Option<String> {
    extract_path(result, path?)?.as_str().map(str::to_owned)
}

/// Extract a session or turn binding from a call result: `sessionId`,
/// `session_id`, or `id` at the top level, else inside each nested object
/// one level down. First match wins; serde_json preserves document order.
fn binding(result: &Value) -> Option<String> {
    fn id_in(map: &serde_json::Map<String, Value>) -> Option<String> {
        ["sessionId", "session_id", "id"]
            .iter()
            .find_map(|key| map.get(*key)?.as_str().map(str::to_owned))
    }
    let map = result.as_object()?;
    id_in(map).or_else(|| {
        map.values()
            .find_map(|value| value.as_object().and_then(id_in))
    })
}

/// Text carried by a stream notification. A declared `text_path` resolves
/// first: a string leaf, or an array whose `{type:"text",text}` blocks
/// concatenate. Otherwise the conventional fields — `params.text`,
/// `params.content`, `params.delta`, `params.item.text`, or a bare string.
fn notification_text(params: &Value, text_path: Option<&str>) -> Option<String> {
    if let Some(path) = text_path {
        let found = extract_path(params, path);
        return match found {
            Some(Value::String(text)) => Some(text.clone()),
            Some(Value::Array(blocks)) => {
                let text: String = blocks
                    .iter()
                    .filter(|block| block.get("type").is_none_or(|t| t.as_str() == Some("text")))
                    .filter_map(|block| block.get("text").and_then(Value::as_str))
                    .collect();
                (!text.is_empty()).then_some(text)
            }
            _ => None,
        };
    }
    params
        .get("text")
        .or_else(|| params.get("content"))
        .or_else(|| params.get("delta"))
        .and_then(Value::as_str)
        .map(str::to_owned)
        .or_else(|| {
            extract_path(params, "item.text")?
                .as_str()
                .map(str::to_owned)
        })
        .or_else(|| params.as_str().map(str::to_owned))
}
