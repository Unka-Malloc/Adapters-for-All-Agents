//! Generic JSON-RPC lane for stdio/RPC agents.
//!
//! Hosts declare the agent's method surface as templates (`{prompt}`,
//! `{sessionId}`, `{turnId}`, `{model}`, `{effort}`, `{instructions}`,
//! `{cwd}` are substituted into string leaves of the params value). Inbound
//! notifications map onto [`AgentEvent`]: the configured `textEvents` method
//! carries text, other notifications arrive raw within the configured output budget.
//!
//! Turn semantics: `session/open` (if configured) runs once at open —
//! `sessionResume` replaces it when the request carries a binding;
//! `prompt` is awaited per turn, and when `completion` is declared the named
//! notification (not the call response) ends the turn; `cancel` (if
//! configured) is sent as a notification on explicit cancel only.

use serde_json::Value;
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use tokio::sync::{Mutex, Notify};

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
    session_instructions: Option<String>,
    session_id: Option<String>,
    cwd: String,
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
struct TurnScope<'a>(&'a std::sync::Mutex<TurnControl>, &'a TurnDelivery);
impl Drop for TurnScope<'_> {
    fn drop(&mut self) {
        *self.0.lock().unwrap() = TurnControl::default();
        self.1.state.lock().unwrap().progress = None;
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

// Ingress owns turn state and the bounded event queue. The generic peer gets
// at most one wake-up record, never the original notification payload. Keeping
// this step synchronous leaves responses/permission calls independent of slow
// async event callbacks; awaiting a response inside a callback cannot deadlock.
struct TurnProgress {
    text: String,
    text_truncated: bool,
    terminals: PendingTerminals,
    expected: Option<String>,
}
struct DeliveryState {
    remaining: Option<usize>,
    truncated: bool,
    queue: VecDeque<AgentEvent>,
    wake_queued: bool,
    issued: u64,
    completed: u64,
    failed: bool,
    closed: bool,
    progress: Option<TurnProgress>,
}
impl DeliveryState {
    fn push(&mut self, event: AgentEvent) {
        self.queue.push_back(event);
        self.issued += 1;
    }
    fn truncate(&mut self) {
        if !self.truncated {
            self.truncated = true;
            self.push(AgentEvent::Truncated);
        }
    }
    fn retain(&mut self, event: AgentEvent) {
        match event {
            AgentEvent::MessageChunk { text } => {
                let prefix = bounds::bounded_prefix(&text, self.remaining);
                let length = prefix.len();
                let retained = (!prefix.is_empty()).then(|| prefix.to_owned());
                if let Some(remaining) = &mut self.remaining {
                    *remaining -= length;
                }
                if let Some(text) = retained {
                    self.push(AgentEvent::MessageChunk { text });
                }
                if length < text.len() {
                    // Once the prefix ends, later ASCII must not fill the
                    // bytes left over from a partial multibyte character.
                    self.remaining = Some(0);
                    self.truncate();
                }
            }
            AgentEvent::Raw { raw } => {
                if let Some(remaining) = self.remaining {
                    let length = if remaining == 0 {
                        0
                    } else {
                        raw.to_string().len()
                    };
                    if remaining == 0 || length > remaining {
                        self.remaining = Some(0);
                        self.truncate();
                        return;
                    }
                    self.remaining = Some(remaining - length);
                }
                self.push(AgentEvent::Raw { raw });
            }
            _ => unreachable!("ingress only retains text and raw notifications"),
        }
    }
}
struct TurnDelivery {
    delegate: Arc<dyn RpcSessionDelegate>,
    spec: RpcLaneSpec,
    limit: Option<usize>,
    state: std::sync::Mutex<DeliveryState>,
    changed: Notify,
}
impl TurnDelivery {
    fn new(delegate: Arc<dyn RpcSessionDelegate>, spec: RpcLaneSpec, limit: Option<usize>) -> Self {
        Self {
            delegate,
            spec,
            limit,
            state: std::sync::Mutex::new(DeliveryState {
                remaining: limit,
                truncated: false,
                queue: VecDeque::new(),
                wake_queued: false,
                issued: 0,
                completed: 0,
                failed: false,
                closed: false,
                progress: None,
            }),
            changed: Notify::new(),
        }
    }
    fn truncated(&self) -> bool {
        self.state.lock().unwrap().truncated
    }
    async fn begin(&self) -> Result<(), AdapterError> {
        loop {
            let changed = self.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            {
                let mut state = self.state.lock().unwrap();
                if state.failed {
                    return Err(delivery_failure());
                }
                if state.completed == state.issued {
                    state.remaining = self.limit;
                    state.truncated = false;
                    state.progress = Some(TurnProgress {
                        text: String::new(),
                        text_truncated: false,
                        expected: None,
                        terminals: PendingTerminals::new(
                            self.limit
                                .map(|limit| limit.max(crate::framing::DEFAULT_MAX_MESSAGE_BYTES)),
                        ),
                    });
                    return Ok(());
                }
            }
            changed.await;
        }
    }
    async fn fence(&self, issued: u64) -> Result<(), AdapterError> {
        loop {
            let changed = self.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            {
                let state = self.state.lock().unwrap();
                if state.failed {
                    return Err(delivery_failure());
                }
                if state.completed >= issued {
                    return Ok(());
                }
            }
            changed.await;
        }
    }
    fn snapshot(&self) -> (String, bool, u64) {
        let state = self.state.lock().unwrap();
        (
            state
                .progress
                .as_ref()
                .map_or_else(String::new, |p| p.text.clone()),
            state.truncated,
            state.issued,
        )
    }
    fn ingest(&self, method: &str, params: Value) -> bool {
        let mut state = self.state.lock().unwrap();
        if state.failed {
            return false;
        }
        let text = (Some(method) == self.spec.text_event_method.as_deref())
            .then(|| notification_text(&params, self.spec.text_path.as_deref()))
            .flatten();
        if let Some(progress) = &mut state.progress {
            if let Some(text) = &text {
                if !progress.text_truncated {
                    let remaining = self.limit.map(|n| n.saturating_sub(progress.text.len()));
                    let prefix = bounds::bounded_prefix(text, remaining);
                    progress.text.push_str(prefix);
                    progress.text_truncated = prefix.len() < text.len();
                }
            }
            if let Some(done) = self
                .spec
                .completion
                .as_ref()
                .filter(|done| done.method == method)
            {
                let status = extract_text(&params, done.status_path.as_deref());
                if done
                    .status_value
                    .as_deref()
                    .is_none_or(|want| status.as_deref() == Some(want))
                {
                    let id = extract_text(&params, done.turn_id_path.as_deref());
                    if progress.expected.is_none() || id == progress.expected {
                        progress.terminals.insert(
                            id,
                            status.unwrap_or_else(|| "end_turn".into()),
                            progress.text.len(),
                        );
                    }
                }
            }
        }
        state.retain(match text {
            Some(text) => AgentEvent::MessageChunk { text },
            None => AgentEvent::Raw {
                raw: serde_json::json!({"method": method, "params": params}),
            },
        });
        let wake = !state.wake_queued && !state.queue.is_empty();
        if wake {
            state.wake_queued = true;
        }
        drop(state);
        self.changed.notify_waiters();
        wake
    }
    async fn drain(&self) {
        loop {
            let event = {
                let mut state = self.state.lock().unwrap();
                match state.queue.pop_front() {
                    Some(event) => event,
                    None => {
                        state.wake_queued = false;
                        return;
                    }
                }
            };
            // If host code panics, fences fail explicitly instead of hanging
            // or returning a successful turn with silently lost callbacks.
            let mut guard = DeliveryGuard {
                delivery: self,
                complete: false,
            };
            self.delegate.on_event(event).await;
            self.state.lock().unwrap().completed += 1;
            guard.complete = true;
            self.changed.notify_waiters();
        }
    }
}
struct DeliveryGuard<'a> {
    delivery: &'a TurnDelivery,
    complete: bool,
}
impl Drop for DeliveryGuard<'_> {
    fn drop(&mut self) {
        if !self.complete {
            self.delivery.state.lock().unwrap().failed = true;
            self.delivery.changed.notify_waiters();
        }
    }
}
fn delivery_failure() -> AdapterError {
    AdapterError::uncertain("agent_event_delivery_failed", "rpc/events")
}
#[async_trait::async_trait]
impl RpcDelegate for TurnDelivery {
    async fn on_request(&self, method: &str, params: Value) -> Result<Value, RpcErrorObject> {
        self.delegate.on_request(method, params).await
    }
    fn prepare_notification(&self, method: String, params: Value) -> Option<(String, Value)> {
        self.ingest(&method, params)
            .then(|| (String::new(), Value::Null))
    }
    async fn on_notification(&self, _method: &str, _params: Value) {
        self.drain().await;
    }
    fn on_input_closed(&self) {
        self.state.lock().unwrap().closed = true;
        self.changed.notify_waiters();
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
        let resume = request.resume.as_deref().filter(|v| !v.trim().is_empty());
        if resume.is_some() && !self.spec.capabilities().session_resume {
            return Err(AdapterError::permanent(
                "agent_session_resume_unsupported",
                "session/open",
            ));
        }
        let env = crate::env::resolve(&self.command.env, &request.env, &*self.secrets).await?;
        let mut command = SupervisedChild::command(&self.command, &env, request.cwd.as_deref());
        let mut child = SupervisedChild::spawn(&mut command, stdio::PROTOCOL_IO)?;
        let (stdin, stdout, stderr) = stdio::protocol_halves(&mut child)?;
        if let Some(stderr) = stderr {
            tokio::spawn(bounds::drain_stderr(stderr, self.observer.clone()));
        }
        let delivery = Arc::new(TurnDelivery::new(
            delegate,
            self.spec.clone(),
            self.max_output_bytes,
        ));
        let peer = JsonRpcPeer::spawn_with_envelope(
            stdout,
            stdin,
            delivery.clone(),
            self.observer.clone(),
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

        let instructions = request.config.instructions.as_deref();
        let permission = request.config.permission.as_deref();
        let model = request.config.model.as_deref();
        let effort = request.config.effort.as_deref();
        let mode = request.config.mode.as_deref();
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
            session_instructions: request.config.instructions.clone(),
            session_id,
            cwd,
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
        let _turn_scope = TurnScope(&self.control, &self.delivery);
        self.delivery.begin().await?;
        let model = model.or(self.session_model.as_deref());
        let effort = effort.or(self.session_effort.as_deref());
        let vars = vars(VarSpec {
            session_id: self.session_id.as_deref(),
            cwd: &self.cwd,
            prompt: text,
            model,
            effort,
            instructions: self.session_instructions.as_deref(),
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
            match self.peer.call(&pre.method, params).await {
                Ok(_) => {}
                Err(_) if self.control.lock().unwrap().killed => {
                    return Ok(cancelled_outcome(String::new(), self.delivery.truncated()));
                }
                Err(error) => return Err(call_failure(error)),
            }
            let cancelled = {
                let control = self.control.lock().unwrap();
                control.pending_cancel || control.killed
            };
            if cancelled {
                return Ok(cancelled_outcome(String::new(), self.delivery.truncated()));
            }
        }
        {
            let mut control = self.control.lock().unwrap();
            if control.pending_cancel || control.killed {
                return Ok(cancelled_outcome(String::new(), false));
            }
            control.dispatched = true;
        }
        if let Some(done) = &self.spec.completion {
            return self.await_completion(done, &vars).await;
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
        let fence = self.delivery.snapshot().2;
        self.delivery.fence(fence).await?;
        if retained.len() < text.len() && !self.delivery.truncated() {
            self.delivery.state.lock().unwrap().truncated = true;
            self.delivery.delegate.on_event(AgentEvent::Truncated).await;
        }
        Ok(TurnOutcome {
            stop_reason,
            text: retained.to_owned(),
            truncated: self.delivery.truncated(),
            exit_code: None,
            usage: None,
        })
    }

    /// Await the acknowledgement and ingress-owned terminal state. Output
    /// callbacks run independently and drain before a successful turn returns.
    async fn await_completion(
        &self,
        done: &crate::model::RpcCompletionSpec,
        vars: &Vars,
    ) -> Result<TurnOutcome, AdapterError> {
        let reply = call(&self.peer, &self.spec.prompt, vars);
        tokio::pin!(reply);
        let mut acknowledged = false;
        let mut turn_id = None;
        loop {
            let changed = self.delivery.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            let terminal = {
                let mut state = self.delivery.state.lock().unwrap();
                if state.failed {
                    return Err(delivery_failure());
                }
                let truncated = state.truncated;
                let issued = state.issued;
                let progress = state.progress.as_mut().expect("active turn");
                let terminal = if acknowledged {
                    let terminal = progress.terminals.take(turn_id.as_deref());
                    if terminal.is_none() && progress.terminals.overflowed {
                        return Err(AdapterError::uncertain(
                            "agent_completion_buffer_exceeded",
                            "rpc/read",
                        ));
                    }
                    progress.terminals.clear();
                    terminal
                } else {
                    None
                };
                let result = terminal.map(|(status, end)| {
                    (status, progress.text[..end].to_owned(), truncated, issued)
                });
                if result.is_none() && acknowledged && state.closed {
                    if self.control.lock().unwrap().killed {
                        return Ok(cancelled_outcome(
                            state.progress.as_ref().unwrap().text.clone(),
                            state.truncated,
                        ));
                    }
                    return Err(AdapterError::uncertain(
                        "agent_transport_failed",
                        "rpc/read",
                    ));
                }
                result
            };
            if let Some((status, text, truncated, fence)) = terminal {
                self.delivery.fence(fence).await?;
                return Ok(TurnOutcome {
                    stop_reason: status,
                    text,
                    truncated,
                    exit_code: None,
                    usage: None,
                });
            }
            tokio::select! {
                biased;
                result = &mut reply, if !acknowledged => {
                    let result = match result {
                        Ok(result) => result,
                        Err(_) if self.control.lock().unwrap().killed => {
                            let (text, truncated, _) = self.delivery.snapshot();
                            return Ok(cancelled_outcome(text, truncated));
                        }
                        Err(error) => return Err(error),
                    };
                    turn_id = if done.turn_id_path.is_some() {
                        Some(binding(&result).filter(|id| !id.trim().is_empty()).ok_or_else(||
                            AdapterError::uncertain("agent_turn_id_missing", "rpc/prompt"))?)
                    } else { None };
                    self.delivery.state.lock().unwrap().progress.as_mut().unwrap().expected = turn_id.clone();
                    let pending_cancel = {
                        let mut control = self.control.lock().unwrap();
                        control.turn_id = turn_id.clone();
                        std::mem::take(&mut control.pending_cancel)
                    };
                    acknowledged = true;
                    if pending_cancel { self.send_cancel(turn_id.as_deref())?; }
                }
                _ = &mut changed => {}
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
            if self.spec.kill_on_cancel {
                control.killed = true;
                (true, None)
            } else {
                if !control.dispatched {
                    control.pending_cancel = true;
                    return Ok(());
                }
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

#[cfg(test)]
mod ingress_tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::io::{AsyncWriteExt, BufReader};

    #[derive(Default)]
    struct SlowHost {
        started: Notify,
        release: Notify,
        seen: AtomicUsize,
        peer: std::sync::Mutex<Option<std::sync::Weak<JsonRpcPeer>>>,
    }
    #[async_trait::async_trait]
    impl RpcSessionDelegate for SlowHost {
        async fn on_event(&self, _: AgentEvent) {
            if self.seen.fetch_add(1, Ordering::SeqCst) == 0 {
                self.started.notify_one();
                self.release.notified().await;
                let peer = self
                    .peer
                    .lock()
                    .unwrap()
                    .as_ref()
                    .unwrap()
                    .upgrade()
                    .unwrap();
                assert_eq!(peer.call("nested", json!({})).await.unwrap(), json!("ok"));
            }
        }
        async fn on_request(&self, _: &str, _: Value) -> Result<Value, RpcErrorObject> {
            Ok(json!("denied"))
        }
    }

    #[tokio::test]
    async fn ingress_bounds_pending_payload_while_callback_awaits_response() {
        tokio::time::timeout(crate::test_settings::WAIT_BUDGET, async {
            let host = Arc::new(SlowHost::default());
            let spec = RpcLaneSpec {
                text_event_method: Some("text".into()), text_path: Some("text".into()),
                completion: Some(crate::model::RpcCompletionSpec {
                    method: "done".into(), turn_id_path: Some("turn".into()),
                    status_path: Some("status".into()), status_value: None,
                }), ..Default::default()
            };
            let delivery = Arc::new(TurnDelivery::new(host.clone(), spec, Some(4096)));
            delivery.begin().await.unwrap();
            let (client, server) = tokio::io::duplex(128 * 1024);
            let (reader, writer) = tokio::io::split(client);
            let peer = JsonRpcPeer::spawn(reader, writer, delivery.clone(), None,
                crate::framing::DEFAULT_MAX_MESSAGE_BYTES);
            *host.peer.lock().unwrap() = Some(Arc::downgrade(&peer));
            let server = tokio::spawn(async move {
                let (reader, mut writer) = tokio::io::split(server);
                let mut reader = BufReader::new(reader);
                let request = crate::framing::read_frame(&mut reader, 1024).await.unwrap().unwrap().unwrap();
                let burst = json!({"jsonrpc":"2.0","method":"text","params":{"text":"x".repeat(65536)}});
                for _ in 0..256 {
                    crate::framing::write_frame(&mut writer, &burst).await.unwrap();
                }
                crate::framing::write_frame(&mut writer, &json!({"jsonrpc":"2.0","method":"done","params":{"turn":"t","status":"completed"}})).await.unwrap();
                crate::framing::write_frame(&mut writer, &json!({"jsonrpc":"2.0","id":request["id"],"result":{"id":"t"}})).await.unwrap();
                crate::framing::write_frame(&mut writer, &json!({"jsonrpc":"2.0","id":"permission","method":"permit","params":{}})).await.unwrap();
                let mut nested = false;
                let mut permission = false;
                while !nested || !permission {
                    let frame = crate::framing::read_frame(&mut reader, 1024).await.unwrap().unwrap().unwrap();
                    if frame["method"] == "nested" {
                        nested = true;
                        crate::framing::write_frame(&mut writer, &json!({"jsonrpc":"2.0","id":frame["id"],"result":"ok"})).await.unwrap();
                    } else {
                        assert_eq!(frame["id"], "permission");
                        assert_eq!(frame["result"], "denied");
                        permission = true;
                    }
                }
                writer.shutdown().await.unwrap();
            });
            let request_peer = peer.clone();
            let reply = tokio::spawn(async move { request_peer.call("prompt", json!({})).await });
            host.started.notified().await;
            // Ack is after the complete 16 MiB burst. It must arrive while
            // the callback is paused, without enqueueing that burst's payload.
            assert_eq!(reply.await.unwrap().unwrap(), json!({"id":"t"}));
            let fence = {
                let state = delivery.state.lock().unwrap();
                let queued_bytes: usize = state.queue.iter().map(|event| match event {
                    AgentEvent::MessageChunk { text } => text.len(),
                    AgentEvent::Raw { raw } => raw.to_string().len(),
                    _ => 0,
                }).sum();
                assert!(queued_bytes <= 4096);
                assert!(state.queue.len() <= 2);
                assert_eq!(state.issued, 2); // first chunk in flight + Truncated
                assert!(state.wake_queued); // one coalesced peer notification
                let progress = state.progress.as_ref().unwrap();
                assert_eq!(progress.text.len(), 4096);
                assert!(progress.terminals.entries.contains_key(&Some("t".into())));
                state.issued
            };
            host.release.notify_one();
            delivery.fence(fence).await.unwrap();
            server.await.unwrap();
            loop {
                let changed = delivery.changed.notified();
                tokio::pin!(changed);
                changed.as_mut().enable();
                if delivery.state.lock().unwrap().closed { break; }
                changed.await;
            }
            assert_eq!(host.seen.load(Ordering::SeqCst), 2);
            peer.close().await;
        }).await.expect("ingress/response deadlock");
    }

    #[test]
    fn utf8_truncation_keeps_a_prefix_across_later_chunks() {
        struct Sink;
        #[async_trait::async_trait]
        impl RpcSessionDelegate for Sink {}
        let delivery = TurnDelivery::new(
            Arc::new(Sink),
            RpcLaneSpec {
                text_event_method: Some("text".into()),
                ..Default::default()
            },
            Some(4),
        );
        assert!(delivery.ingest("text", json!({"text":"界界"})));
        assert!(!delivery.ingest("text", json!({"text":"a"})));
        let state = delivery.state.lock().unwrap();
        assert_eq!(
            state.queue.iter().cloned().collect::<Vec<_>>(),
            vec![
                AgentEvent::MessageChunk { text: "界".into() },
                AgentEvent::Truncated
            ]
        );
    }
}
