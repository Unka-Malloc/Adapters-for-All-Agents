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
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex};

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
use crate::template::{contains_placeholder, extract_path, substitute, vars, VarSpec, Vars};
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
    /// Retained and delivered output budget per turn; None is unbounded.
    pub max_output_bytes: Option<usize>,
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
    max_output_bytes: Option<usize>,
    delivery: Arc<TurnDelivery>,
    session_model: Option<String>,
    session_effort: Option<String>,
    session_id: Option<String>,
    cwd: String,
    /// Inbound notifications, for awaiting declared completion methods.
    notifications: TurnNotifications,
    /// One turn's identity and explicit cancellation intent; cleared on every
    /// return path, including setup/transport errors.
    control: std::sync::Mutex<TurnControl>,
    /// Serializes prompt turns.
    turn_lock: Mutex<()>,
}

#[derive(Default)]
struct TurnControl {
    active: bool,
    dispatched: bool,
    turn_id: Option<String>,
    pending_cancel: bool,
    killed: bool,
}
struct TurnScope<'a>(&'a std::sync::Mutex<TurnControl>);
impl Drop for TurnScope<'_> {
    fn drop(&mut self) {
        *self.0.lock().unwrap() = TurnControl::default();
    }
}

// Bounded backpressure for relevant turn notifications; terminal messages
// share reliable delivery and are never dropped to make room for telemetry.
const TURN_NOTIFICATION_CAPACITY: usize = 64;
type TurnNotifications = Arc<std::sync::Mutex<Option<mpsc::Sender<(String, Value)>>>>;

struct TurnSubscription(TurnNotifications);
impl Drop for TurnSubscription {
    fn drop(&mut self) {
        self.0.lock().unwrap().take();
    }
}

fn cancelled_outcome(text: String, truncated: bool) -> TurnOutcome {
    TurnOutcome {
        stop_reason: "cancelled".into(),
        text,
        truncated,
        exit_code: None,
        usage: None,
    }
}

struct DeliveryBudget {
    remaining: Option<usize>,
    truncated: bool,
}
impl DeliveryBudget {
    fn new(remaining: Option<usize>) -> Self {
        Self {
            remaining,
            truncated: false,
        }
    }
    fn truncate(&mut self) -> bool {
        let first = !self.truncated;
        self.truncated = true;
        first
    }
}

struct TurnDelivery {
    delegate: Arc<dyn RpcSessionDelegate>,
    budget: std::sync::Mutex<DeliveryBudget>,
}
impl TurnDelivery {
    fn truncated(&self) -> bool {
        self.budget.lock().unwrap().truncated
    }
    fn prepare(&self, event: AgentEvent) -> (Option<AgentEvent>, bool) {
        let mut budget = self.budget.lock().unwrap();
        match event {
            AgentEvent::MessageChunk { text } => {
                let prefix = bounds::bounded_prefix(&text, budget.remaining);
                let length = prefix.len();
                let event = (!prefix.is_empty()).then(|| AgentEvent::MessageChunk {
                    text: prefix.to_owned(),
                });
                if let Some(remaining) = &mut budget.remaining {
                    *remaining -= length;
                }
                let truncated = length < text.len() && budget.truncate();
                (event, truncated)
            }
            AgentEvent::Raw { raw } => {
                if let Some(remaining) = budget.remaining {
                    // Once exhausted, do not serialize discarded telemetry.
                    let length = if remaining == 0 {
                        0
                    } else {
                        raw.to_string().len()
                    };
                    if remaining == 0 || length > remaining {
                        budget.remaining = Some(0);
                        return (None, budget.truncate());
                    }
                    budget.remaining = Some(remaining - length);
                }
                (Some(AgentEvent::Raw { raw }), false)
            }
            event => (Some(event), false),
        }
    }
    async fn dispatch(&self, (event, truncated): (Option<AgentEvent>, bool)) {
        if let Some(event) = event {
            self.delegate.on_event(event).await;
        }
        if truncated {
            self.delegate.on_event(AgentEvent::Truncated).await;
        }
    }
    async fn mark_truncated(&self) {
        let first = self.budget.lock().unwrap().truncate();
        self.dispatch((None, first)).await;
    }
}

/// Uncorrelated terminal records are kept only until the prompt ack arrives.
/// With a configured budget, excess control metadata is a protocol resource
/// violation, never a reason to silently lose a terminal and wait forever.
struct PendingTerminals {
    entries: HashMap<Option<String>, (String, usize, usize)>,
    bytes: usize,
    limit: Option<usize>,
    overflowed: bool,
}
impl PendingTerminals {
    fn new(limit: Option<usize>) -> Self {
        Self {
            entries: HashMap::new(),
            bytes: 0,
            limit,
            overflowed: false,
        }
    }
    fn insert(&mut self, id: Option<String>, status: String, end: usize) {
        if self.entries.contains_key(&id) {
            return;
        }
        let bytes = id.as_ref().map_or(0, String::len)
            + status.len()
            + std::mem::size_of::<(Option<String>, String, usize, usize)>();
        if self
            .limit
            .is_some_and(|limit| bytes > limit.saturating_sub(self.bytes))
        {
            self.overflowed = true;
            return;
        }
        let order = self.entries.len();
        self.bytes += bytes;
        self.entries.insert(id, (status, end, order));
    }
    fn take(&mut self, id: Option<&str>) -> Option<(String, usize)> {
        let selected = match id {
            Some(id) => self.entries.remove(&Some(id.to_owned())),
            None => self
                .entries
                .drain()
                .min_by_key(|(_, (_, _, order))| *order)
                .map(|(_, value)| value),
        };
        selected.map(|(status, end, _)| (status, end))
    }
    fn clear(&mut self) {
        self.entries.clear();
        self.bytes = 0;
    }
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
        let notifications: TurnNotifications = Arc::new(std::sync::Mutex::new(None));
        let delivery = Arc::new(TurnDelivery {
            delegate: delegate.clone(),
            budget: std::sync::Mutex::new(DeliveryBudget::new(self.max_output_bytes)),
        });
        struct Inner {
            delegate: Arc<dyn RpcSessionDelegate>,
            text_method: Option<String>,
            text_path: Option<String>,
            completion_method: Option<String>,
            notifications: TurnNotifications,
            delivery: Arc<TurnDelivery>,
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
                let sink = self.notifications.lock().unwrap().clone();
                if let Some(sink) = sink {
                    let _ = sink.send((String::new(), Value::Null)).await;
                }
            }
            async fn on_notification(&self, method: &str, params: Value) {
                let forwarded = if Some(method) == self.text_method.as_deref()
                    || Some(method) == self.completion_method.as_deref()
                {
                    Some(params.clone())
                } else {
                    None
                };
                let event = if Some(method) == self.text_method.as_deref() {
                    notification_text(&params, self.text_path.as_deref())
                        .map(|text| AgentEvent::MessageChunk { text })
                } else {
                    None
                };
                let prepared = self.delivery.prepare(event.unwrap_or(AgentEvent::Raw {
                    raw: serde_json::json!({"method": method, "params": params}),
                }));
                if let Some(params) = forwarded {
                    let sink = self.notifications.lock().unwrap().clone();
                    if let Some(sink) = sink {
                        let _ = sink.send((method.to_owned(), params)).await;
                    }
                }
                self.delivery.dispatch(prepared).await;
            }
        }
        let peer = JsonRpcPeer::spawn_with_envelope(
            stdout,
            stdin,
            Arc::new(Inner {
                delegate,
                text_method,
                text_path: self.spec.text_path.clone(),
                completion_method: self
                    .spec
                    .completion
                    .as_ref()
                    .map(|done| done.method.clone()),
                notifications: notifications.clone(),
                delivery: delivery.clone(),
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
            max_output_bytes: self.max_output_bytes,
            delivery,
            session_model: request.config.model.clone(),
            session_effort: request.config.effort.clone(),
            session_id,
            cwd,
            notifications,
            control: std::sync::Mutex::new(TurnControl::default()),
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
        *self.control.lock().unwrap() = TurnControl {
            active: true,
            ..Default::default()
        };
        let _turn_scope = TurnScope(&self.control);
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
        {
            let mut control = self.control.lock().unwrap();
            if control.pending_cancel {
                return Ok(cancelled_outcome(String::new(), false));
            }
            control.dispatched = true;
        }
        *self.delivery.budget.lock().unwrap() = DeliveryBudget::new(self.max_output_bytes);
        // Install the reliable per-turn feed before the prompt request. The
        // receiver is drained while awaiting its acknowledgement as well.
        if let Some(done) = &self.spec.completion {
            let (sink, feed) = mpsc::channel(TURN_NOTIFICATION_CAPACITY);
            *self.notifications.lock().unwrap() = Some(sink);
            let _subscription = TurnSubscription(self.notifications.clone());
            return self.await_completion(done, feed, &vars).await;
        }
        let result = match call(&self.peer, &self.spec.prompt, &vars).await {
            Ok(result) => result,
            Err(_) if self.control.lock().unwrap().killed => {
                return Ok(cancelled_outcome(String::new(), self.delivery.truncated()));
            }
            Err(error) => return Err(error),
        };
        let stop_reason = ["stopReason", "stop_reason", "status"]
            .iter()
            .find_map(|key| result.get(*key).and_then(Value::as_str))
            .unwrap_or("end_turn")
            .to_owned();
        let text = extract_text(&result, self.spec.result_text_path.as_deref())
            .unwrap_or_else(|| result.to_string());
        let retained = bounds::bounded_prefix(&text, self.max_output_bytes);
        if retained.len() < text.len() {
            self.delivery.mark_truncated().await;
        }
        Ok(TurnOutcome {
            stop_reason,
            text: retained.to_owned(),
            truncated: self.delivery.truncated(),
            exit_code: None,
            usage: None,
        })
    }

    /// Collect text and reliable terminal messages concurrently with the
    /// acknowledgement. Notification bursts cannot overwrite a completed turn.
    async fn await_completion(
        &self,
        done: &crate::model::RpcCompletionSpec,
        mut feed: mpsc::Receiver<(String, Value)>,
        vars: &Vars,
    ) -> Result<TurnOutcome, AdapterError> {
        let reply = call(&self.peer, &self.spec.prompt, vars);
        tokio::pin!(reply);
        let mut acknowledged = false;
        let mut turn_id = None;
        let mut text = String::new();
        // A terminal may precede its acknowledgement. Preserve its turn id and
        // text boundary until the response identifies the requested turn.
        let mut terminals = PendingTerminals::new(
            self.max_output_bytes
                .map(|limit| limit.max(crate::framing::DEFAULT_MAX_MESSAGE_BYTES)),
        );
        loop {
            if acknowledged {
                if let Some((status, end)) = terminals.take(if done.turn_id_path.is_some() {
                    turn_id.as_deref()
                } else {
                    None
                }) {
                    text.truncate(end);
                    return Ok(TurnOutcome {
                        stop_reason: status,
                        text,
                        truncated: self.delivery.truncated(),
                        exit_code: None,
                        usage: None,
                    });
                }
                if terminals.overflowed {
                    return Err(AdapterError::uncertain(
                        "agent_completion_buffer_exceeded",
                        "rpc/read",
                    ));
                }
                terminals.clear();
            }
            tokio::select! {
                biased;
                result = &mut reply, if !acknowledged => {
                    let result = match result {
                        Ok(result) => result,
                        Err(_) if self.control.lock().unwrap().killed => {
                            return Ok(cancelled_outcome(text, self.delivery.truncated()));
                        }
                        Err(error) => return Err(error),
                    };
                    turn_id = binding(&result);
                    let pending_cancel = {
                        let mut control = self.control.lock().unwrap();
                        control.turn_id = turn_id.clone();
                        std::mem::take(&mut control.pending_cancel)
                    };
                    acknowledged = true;
                    if pending_cancel { self.send_cancel(turn_id.as_deref())?; }
                }
                notification = feed.recv() => {
                    let Some((method, params)) = notification.filter(|(method, _)| !method.is_empty()) else {
                        if self.control.lock().unwrap().killed {
                            return Ok(cancelled_outcome(text, self.delivery.truncated()));
                        }
                        return Err(AdapterError::uncertain("agent_transport_failed", "rpc/read"));
                    };
                    if method == done.method {
                        let status = extract_text(&params, done.status_path.as_deref());
                        if done.status_value.as_deref().is_some_and(|want| status.as_deref() != Some(want)) {
                            continue;
                        }
                        let id = extract_text(&params, done.turn_id_path.as_deref());
                        if !acknowledged || done.turn_id_path.is_none() || turn_id.is_none() || id == turn_id {
                            terminals.insert(id, status.unwrap_or_else(|| "end_turn".to_owned()), text.len());
                        }
                    } else if Some(method.as_str()) == self.spec.text_event_method.as_deref() {
                        if let Some(chunk) = notification_text(&params, self.spec.text_path.as_deref()) {
                            let remaining = self.max_output_bytes.map(|limit| limit.saturating_sub(text.len()));
                            text.push_str(bounds::bounded_prefix(&chunk, remaining));
                        }
                    }
                }
            }
        }
    }

    /// Explicit user cancellation: sends the configured notification, or —
    /// on a `kill_on_cancel` lane — terminates the process, the agent's
    /// only honest cancel semantic. Before an acknowledgement, a turn-targeted
    /// cancellation is retained until the real id arrives. Idle cancellation
    /// is a no-op. A killed session cannot run again.
    pub async fn cancel(&self) -> Result<(), AdapterError> {
        let (kill, turn_id) = {
            let mut control = self.control.lock().unwrap();
            if !control.active {
                return Ok(());
            }
            if !control.dispatched {
                control.pending_cancel = true;
                return Ok(());
            }
            if self.spec.kill_on_cancel {
                control.killed = true;
                (true, None)
            } else {
                let cancel = self.spec.cancel.as_ref().ok_or_else(|| {
                    AdapterError::permanent("agent_cancel_unsupported", "session/cancel")
                })?;
                if contains_placeholder(&cancel.params, "turnId") && control.turn_id.is_none() {
                    if self.spec.completion.is_some() {
                        control.pending_cancel = true;
                        return Ok(());
                    }
                    return Err(AdapterError::permanent(
                        "agent_cancel_target_unavailable",
                        "session/cancel",
                    ));
                }
                (false, control.turn_id.clone())
            }
        };
        if kill {
            self.child.lock().await.terminate().await?;
            Ok(())
        } else {
            self.send_cancel(turn_id.as_deref())
        }
    }

    fn send_cancel(&self, turn_id: Option<&str>) -> Result<(), AdapterError> {
        let cancel =
            self.spec.cancel.as_ref().ok_or_else(|| {
                AdapterError::permanent("agent_cancel_unsupported", "session/cancel")
            })?;
        if (contains_placeholder(&cancel.params, "turnId") && turn_id.is_none())
            || (contains_placeholder(&cancel.params, "sessionId") && self.session_id.is_none())
        {
            return Err(AdapterError::permanent(
                "agent_cancel_target_unavailable",
                "session/cancel",
            ));
        }
        let vars = vars(VarSpec {
            session_id: self.session_id.as_deref(),
            cwd: &self.cwd,
            turn_id,
            ..Default::default()
        });
        self.peer
            .notify(&cancel.method, substitute(&cancel.params, &vars))
            .map_err(|error| {
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

#[cfg(test)]
mod completion_budget_tests {
    use super::PendingTerminals;

    #[test]
    fn pending_terminals_deduplicate_and_bound_uncorrelated_records() {
        let mut pending = PendingTerminals::new(Some(256));
        for _ in 0..1000 {
            pending.insert(Some("expected".into()), "completed".into(), 12);
        }
        assert_eq!(pending.entries.len(), 1);
        for i in 0..1000 {
            pending.insert(Some(format!("foreign-{i}")), "completed".into(), 99);
        }
        assert!(pending.overflowed);
        let payload_bytes: usize = pending
            .entries
            .iter()
            .map(|(id, (status, _, _))| id.as_ref().map_or(0, String::len) + status.len())
            .sum();
        assert!(payload_bytes <= 256);
        assert!(pending.entries.len() < 10);
        assert_eq!(
            pending.take(Some("expected")),
            Some(("completed".into(), 12))
        );
        assert!(pending.take(Some("not-retained")).is_none());
    }
}
