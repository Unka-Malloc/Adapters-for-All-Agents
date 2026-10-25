//! Agent Client Protocol lane.
//!
//! Built on the official `agent-client-protocol` crate: the schema and
//! JSON-RPC machinery are spec-tracked upstream, this module only adapts them
//! to the [`AgentSession`] contract — a Send handle driving one agent process
//! through `initialize` → `session/new|load|resume` → `session/prompt` turns.
//!
//! Session semantics:
//! - One `AcpSession` owns one child process and one protocol connection.
//! - Agent→client traffic (`session/update`, `session/request_permission`,
//!   and any unhandled request) is delegated to [`SessionDelegate`]. What the
//!   delegate does not claim fails closed (`method not found` / `cancelled`),
//!   never silently grants.
//! - `session/cancel` is only sent in response to an explicit `cancel()` —
//!   transport hiccups never masquerade as user cancellation.

use agent_client_protocol::schema::v1::{
    AuthMethodId, AuthenticateRequest, CancelNotification, ContentBlock, InitializeRequest,
    McpServer, PromptRequest, RequestPermissionOutcome, RequestPermissionRequest,
    RequestPermissionResponse, SelectedPermissionOutcome, SessionConfigKind, SessionConfigOption,
    SessionConfigOptionCategory, SessionConfigSelectOptions, SessionId, SessionModeState,
    SessionNotification, SessionUpdate, SetSessionConfigOptionRequest, SetSessionModeRequest,
    StopReason, TextContent, ToolCallContent,
};
use agent_client_protocol::schema::ProtocolVersion;
use agent_client_protocol::{Agent, ByteStreams, ConnectionTo};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot, Mutex, OwnedMutexGuard};
use tokio::task::JoinHandle;
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

use crate::bounds;
use crate::env::SecretResolver;
use crate::error::AdapterError;
use crate::model::{
    AgentEvent, AgentInfo, Capabilities, CommandSpec, SelectorPhase, SelectorSupport,
    SelectorSupportSet, SessionConfig, SessionRequest, TokenUsage, TurnOutcome, TurnRequest,
};
use crate::observe::SharedObserver;
use crate::supervision::SupervisedChild;
use crate::transport::stdio;

/// Session-establishment wire types. The official SDK types expose
/// `session/new` but its response drops the legacy `models` surface and
/// `_meta`; ACP v1 lets each establishment method return the same state
/// fields, so a4aa speaks them itself and parses every observed surface.
#[derive(Debug, Clone, Serialize, Deserialize, agent_client_protocol::JsonRpcRequest)]
#[request(method = "session/new", response = serde_json::Value)]
struct SessionNewWire {
    cwd: PathBuf,
    #[serde(rename = "mcpServers")]
    mcp_servers: Vec<McpServer>,
}

#[derive(Debug, Clone, Serialize, Deserialize, agent_client_protocol::JsonRpcRequest)]
#[request(method = "session/load", response = serde_json::Value)]
struct SessionLoadWire {
    #[serde(rename = "sessionId")]
    session_id: String,
    cwd: PathBuf,
    #[serde(rename = "mcpServers")]
    mcp_servers: Vec<McpServer>,
}

#[derive(Debug, Clone, Serialize, Deserialize, agent_client_protocol::JsonRpcRequest)]
#[request(method = "session/resume", response = serde_json::Value)]
struct SessionResumeWire {
    #[serde(rename = "sessionId")]
    session_id: String,
    cwd: PathBuf,
    #[serde(rename = "mcpServers")]
    mcp_servers: Vec<McpServer>,
}

/// Legacy `session/set_model` — absent from the SDK but still the only
/// model surface on some agents.
#[derive(Debug, Clone, Serialize, Deserialize, agent_client_protocol::JsonRpcRequest)]
#[request(method = "session/set_model", response = serde_json::Value)]
struct SetSessionModelWire {
    #[serde(rename = "sessionId")]
    session_id: String,
    #[serde(rename = "modelId")]
    model_id: String,
}

/// Legacy `models` session surface (current selection + availability).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct SessionModelState {
    current_model_id: String,
    available_models: Vec<SessionModelEntry>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct SessionModelEntry {
    #[serde(alias = "id")]
    model_id: String,
    name: String,
}

/// The session-configuration surfaces observed on establishment, updated
/// as `session/set_*` responses arrive. `None` for a surface means the
/// agent did not report it — unknown, not empty.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct SessionSetup {
    session_id: Option<String>,
    modes: Option<SessionModeState>,
    config_options: Option<Vec<SessionConfigOption>>,
    models: Option<SessionModelState>,
    #[serde(rename = "_meta")]
    meta: Option<Value>,
}

/// Serializable view of `SessionSetup` for hosts.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SessionSetupView<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    config_options: &'a Option<Vec<SessionConfigOption>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    modes: &'a Option<SessionModeState>,
    #[serde(skip_serializing_if = "Option::is_none")]
    models: &'a Option<SessionModelState>,
    #[serde(rename = "_meta", skip_serializing_if = "Option::is_none")]
    meta: &'a Option<Value>,
}

impl SessionSetup {
    fn parse(value: Value) -> Result<Self, AdapterError> {
        serde_json::from_value(value).map_err(|error| {
            AdapterError::permanent("agent_protocol_error", "session/establish").with_detail(
                format!("session setup response could not be decoded: {error}"),
            )
        })
    }

    fn view(&self) -> SessionSetupView<'_> {
        SessionSetupView {
            config_options: &self.config_options,
            modes: &self.modes,
            models: &self.models,
            meta: &self.meta,
        }
    }
}

/// What an agent's permission prompt looks like to the host.
#[derive(Clone, Debug)]
pub struct PermissionPrompt {
    pub session_id: String,
    pub tool_call_id: String,
    pub title: String,
    pub kind: Option<String>,
    /// Human-readable content extracted from the tool call — text blocks,
    /// rendered diffs, terminal references.
    pub content_texts: Vec<String>,
    /// Selectable options: `(option_id, name, kind)`.
    pub options: Vec<(String, String, String)>,
    /// The original request, serialized — full fidelity for consumers that
    /// render more than the summary.
    pub raw: Value,
}

pub enum PermissionDecision {
    Cancelled,
    Selected(String),
}

/// Host callbacks for one ACP session. Defaults keep the lane safe: updates
/// are dropped only if the host chooses not to observe them, and every
/// permission request is cancelled.
#[async_trait::async_trait]
pub trait SessionDelegate: Send + Sync {
    /// Admitted output, delivered in wire order within the turn's output cap.
    async fn on_event(&self, _event: AgentEvent) {}

    async fn on_permission(&self, _prompt: PermissionPrompt) -> PermissionDecision {
        PermissionDecision::Cancelled
    }
}

/// Delegate for sessions that only consume output.
pub struct CollectDelegate {
    pub events: mpsc::UnboundedSender<AgentEvent>,
}

#[async_trait::async_trait]
impl SessionDelegate for CollectDelegate {
    async fn on_event(&self, event: AgentEvent) {
        let _ = self.events.send(event);
    }
}

// The command driver owns each turn's lock even if its caller drops the
// response future. Cancellation and prompt dispatch share this state so a
// configuration await cannot erase an explicit cancellation request.
#[derive(Default)]
struct TurnState {
    active: bool,
    dispatched: bool,
    cancelled: bool,
    /// Any session/update admitted this turn — the provider did billable
    /// work, so a refusal arriving now may have already had effect.
    served: bool,
    remaining: Option<usize>,
    truncated: bool,
}
struct ActiveTurn {
    state: Arc<std::sync::Mutex<TurnState>>,
    _serial: OwnedMutexGuard<()>,
}
impl Drop for ActiveTurn {
    fn drop(&mut self) {
        let mut state = self.state.lock().unwrap();
        state.active = false;
        state.dispatched = false;
        state.cancelled = false;
    }
}
enum OutputCommand {
    Event(AgentEvent),
    Drain(oneshot::Sender<()>),
}
#[derive(Clone)]
struct OutputSink {
    state: Arc<std::sync::Mutex<TurnState>>,
    send: mpsc::UnboundedSender<OutputCommand>,
}
impl OutputSink {
    fn new(
        state: Arc<std::sync::Mutex<TurnState>>,
        delegate: Arc<dyn SessionDelegate>,
    ) -> (Self, JoinHandle<()>) {
        let (send, mut receive) = mpsc::unbounded_channel();
        let worker = tokio::spawn(async move {
            while let Some(command) = receive.recv().await {
                match command {
                    OutputCommand::Event(event) => delegate.on_event(event).await,
                    OutputCommand::Drain(reply) => {
                        let _ = reply.send(());
                    }
                }
            }
        });
        (Self { state, send }, worker)
    }
    // Runs inline in the SDK notification dispatcher. Admission happens
    // before enqueueing, so callbacks cannot retain unbounded source payloads
    // or block response/configuration/permission processing. None is the
    // caller's explicit choice of unbounded output.
    /// Mark that the turn produced billable evidence. Called only for
    /// output-bearing session updates — bookkeeping updates such as
    /// `AvailableCommandsUpdate` or `SessionInfoUpdate` do not prove the
    /// provider ran the request.
    fn mark_served(&self) {
        self.state.lock().unwrap().served = true;
    }

    fn deliver(&self, event: AgentEvent) -> Result<(), agent_client_protocol::Error> {
        let mut state = self.state.lock().unwrap();
        let (event, clipped) = match event {
            AgentEvent::MessageChunk { text } => {
                let prefix = bounds::bounded_prefix(&text, state.remaining);
                let length = prefix.len();
                let retained = (!prefix.is_empty()).then(|| AgentEvent::MessageChunk {
                    text: prefix.to_owned(),
                });
                if let Some(remaining) = &mut state.remaining {
                    *remaining -= length;
                }
                (retained, length < text.len())
            }
            event => {
                // Non-text updates carry tool/plan/raw payloads. Preserve an
                // entire admitted record, or drain it without queueing it.
                if let Some(remaining) = state.remaining {
                    let length = if remaining == 0 {
                        0
                    } else {
                        serde_json::to_vec(&event)
                            .map_err(|_| agent_client_protocol::Error::internal_error())?
                            .len()
                    };
                    if remaining == 0 || length > remaining {
                        (None, true)
                    } else {
                        state.remaining = Some(remaining - length);
                        (Some(event), false)
                    }
                } else {
                    (Some(event), false)
                }
            }
        };
        if let Some(event) = event {
            self.send
                .send(OutputCommand::Event(event))
                .map_err(|_| agent_client_protocol::Error::internal_error())?;
        }
        if clipped {
            state.remaining = Some(0);
            if !state.truncated {
                state.truncated = true;
                self.send
                    .send(OutputCommand::Event(AgentEvent::Truncated))
                    .map_err(|_| agent_client_protocol::Error::internal_error())?;
            }
        }
        Ok(())
    }
    async fn drain(&self) -> Result<(), AdapterError> {
        let (send, receive) = oneshot::channel();
        self.send
            .send(OutputCommand::Drain(send))
            .map_err(|_| output_failure())?;
        receive.await.map_err(|_| output_failure())
    }
}

fn output_failure() -> AdapterError {
    AdapterError::uncertain("agent_event_delivery_failed", "acp/events")
}

enum SessionCommand {
    Prompt {
        request: TurnRequest,
        turn: ActiveTurn,
        reply: oneshot::Sender<Result<TurnOutcome, AdapterError>>,
    },
    // Wake the command loop after shared process termination, including
    // when close was called from a protocol callback in the driver itself.
    Close,
}

async fn stop_owned_task(task: JoinHandle<()>) {
    if tokio::task::try_id() != Some(task.id()) {
        task.abort();
        let _ = task.await;
    }
}

/// An open ACP session: one process, one connection, one session id.
pub struct AcpSession {
    command: mpsc::UnboundedSender<SessionCommand>,
    connection: ConnectionTo<Agent>,
    driver: Mutex<Option<JoinHandle<()>>>,
    output_worker: Mutex<Option<JoinHandle<()>>>,
    child: Arc<Mutex<SupervisedChild>>,
    closed: AtomicBool,
    session_id: String,
    capabilities: Capabilities,
    agent_info: Option<AgentInfo>,
    state: Arc<std::sync::Mutex<SessionSetup>>,
    prompt_lock: Arc<Mutex<()>>,
    turn_state: Arc<std::sync::Mutex<TurnState>>,
    max_output_bytes: Option<usize>,
}

impl AcpSession {
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    pub fn capabilities(&self) -> &Capabilities {
        &self.capabilities
    }

    /// The agent's self-reported implementation identity from `initialize`.
    /// Self-reported — never an attestation.
    pub fn agent_info(&self) -> Option<&AgentInfo> {
        self.agent_info.as_ref()
    }

    /// The session's observed configuration surfaces (`configOptions`,
    /// `modes`, legacy `models`, `_meta`) as they stand after
    /// establishment and any applied `session/set_*` calls. Surfaces the
    /// agent did not report are absent — unknown, not empty.
    pub fn session_state(&self) -> Value {
        let state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        serde_json::to_value(state.view()).unwrap_or(Value::Null)
    }

    /// Report selector support from the configuration surfaces negotiated
    /// for this ACP session.
    pub fn selector_support(&self, phase: SelectorPhase) -> SelectorSupportSet {
        let state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        match phase {
            SelectorPhase::Session => SelectorSupportSet {
                model: selector_support(model_selector_available(&state)),
                effort: selector_support(config_selector_available(&state, "thought_level")),
                mode: selector_support(mode_selector_available(&state)),
                permission: selector_support(config_selector_available(&state, "permissions")),
            },
            SelectorPhase::Turn => SelectorSupportSet {
                model: selector_support(model_selector_available(&state)),
                effort: selector_support(config_selector_available(&state, "thought_level")),
                mode: SelectorSupport::Unsupported,
                permission: SelectorSupport::Unsupported,
            },
        }
    }

    /// Check turn-level selectors against the session's actual advertised
    /// options without changing the active configuration.
    pub fn validate_turn_request(&self, request: &TurnRequest) -> Result<(), AdapterError> {
        let state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        if let Some(model) = request.model.as_deref() {
            if !model_value_available(&state, model) {
                return Err(config_unavailable("model", model));
            }
        }
        if let Some(effort) = request.effort.as_deref() {
            if !config_value_available(&state, "thought_level", effort) {
                return Err(config_unavailable("thought_level", effort));
            }
        }
        Ok(())
    }

    /// Submit one prompt turn. Serialized: a second `prompt` waits for the
    /// in-flight one rather than interleaving on the wire. Turn-level
    /// `model`/`effort` values are applied through the agent's advertised
    /// configuration surface before the prompt; a requested value the
    /// agent cannot express fails the turn rather than being ignored.
    pub async fn prompt(&self, request: TurnRequest) -> Result<TurnOutcome, AdapterError> {
        let serial = self.prompt_lock.clone().lock_owned().await;
        if self.closed.load(Ordering::Acquire) {
            return Err(AdapterError::permanent(
                "agent_session_closed",
                "session/prompt",
            ));
        }
        {
            let mut state = self.turn_state.lock().unwrap();
            *state = TurnState {
                active: true,
                remaining: self.max_output_bytes,
                ..Default::default()
            };
        }
        let turn = ActiveTurn {
            state: self.turn_state.clone(),
            _serial: serial,
        };
        let (reply, outcome) = oneshot::channel();
        self.command
            .send(SessionCommand::Prompt {
                request,
                turn,
                reply,
            })
            .map_err(|_| AdapterError::uncertain("agent_connection_closed", "session/prompt"))?;
        outcome
            .await
            .map_err(|_| AdapterError::uncertain("agent_connection_closed", "session/prompt"))?
    }

    /// Request cancellation of the in-flight turn (`session/cancel`).
    /// Explicit user cancellation only — callers must not invoke this for
    /// transport errors or observer disconnects.
    pub fn cancel(&self) -> Result<(), AdapterError> {
        let mut state = self.turn_state.lock().unwrap();
        if !state.active {
            return Ok(());
        }
        state.cancelled = true;
        if !state.dispatched {
            return Ok(());
        }
        self.connection
            .send_notification(CancelNotification::new(self.session_id.clone()))
            .map_err(|error| {
                AdapterError::uncertain("agent_cancel_failed", "session/cancel")
                    .with_detail(error.to_string())
            })
    }

    /// Shared, idempotent close: terminate the process even during a prompt
    /// or a blocked host callback, then stop this session's protocol tasks.
    pub async fn close(&self) -> Result<(), AdapterError> {
        self.closed.store(true, Ordering::Release);
        let mut driver = self.driver.lock().await;
        self.child.lock().await.terminate().await?;
        let _ = self.command.send(SessionCommand::Close);
        if let Some(driver) = driver.take() {
            stop_owned_task(driver).await;
        }
        if let Some(worker) = self.output_worker.lock().await.take() {
            // A host may close from on_event itself. Do not abort/await the
            // callback's own task; its closed queue ends after it returns.
            stop_owned_task(worker).await;
        }
        Ok(())
    }
}

/// ACP lane bound to one command spec.
pub struct AcpLane {
    /// Per-turn retained/delivered output cap; None explicitly permits unbounded output.
    pub max_output_bytes: Option<usize>,
    pub command: CommandSpec,
    pub mcp_servers: Vec<Value>,
    pub prefer_load: bool,
    /// Resolved lazily on an auth-required refusal and sent as
    /// `authenticate`'s `_meta.api_key`.
    pub auth_key: Option<crate::env::EnvSource>,
    pub client_name: String,
    pub secrets: Arc<dyn SecretResolver>,
    pub observer: SharedObserver,
}

impl AcpLane {
    /// Spawn the agent and run `initialize` + session establishment.
    pub async fn open(
        &self,
        request: SessionRequest,
        delegate: Arc<dyn SessionDelegate>,
    ) -> Result<AcpSession, AdapterError> {
        let env = crate::env::resolve(&self.command.env, &request.env, &*self.secrets).await?;
        let mut command = SupervisedChild::command(&self.command, &env, request.cwd.as_deref());
        let mut child = SupervisedChild::spawn(&mut command, stdio::PROTOCOL_IO)?;
        let (child_stdin, child_stdout, child_stderr) = stdio::protocol_halves(&mut child)?;
        if let Some(stderr) = child_stderr {
            tokio::spawn(bounds::drain_stderr(
                stderr,
                self.observer.clone(),
                bounds::StderrTail::default(),
            ));
        }

        let client_name = self.client_name.clone();
        let mcp_servers = self.mcp_servers.clone();
        let prefer_load = self.prefer_load;
        let resume = request.resume.clone();
        let cwd = request
            .cwd
            .clone()
            .or_else(|| self.command.cwd.clone())
            .unwrap_or_else(std::env::temp_dir);
        let (ready_tx, ready_rx) = oneshot::channel();
        let ready = Arc::new(ReadySlot::new(Some(ready_tx)));
        let (command_tx, command_rx) = mpsc::unbounded_channel();
        let turn_state = Arc::new(std::sync::Mutex::new(TurnState {
            remaining: self.max_output_bytes,
            ..Default::default()
        }));
        let (output, output_worker) = OutputSink::new(turn_state.clone(), delegate.clone());
        let child = Arc::new(Mutex::new(child));

        let driver = tokio::spawn(drive(
            client_name,
            delegate,
            Arc::downgrade(&child),
            child_stdin,
            child_stdout,
            ServeContext {
                cwd,
                mcp_servers,
                prefer_load,
                resume,
                config: request.config,
                output,
                auth_key: self.auth_key.clone(),
                env,
                secrets: self.secrets.clone(),
            },
            command_rx,
            ready,
        ));

        let opened = ready_rx
            .await
            .map_err(|_| AdapterError::uncertain("agent_connection_closed", "session/open"))
            .and_then(|opened| opened);
        let opened = match opened {
            Ok(opened) => opened,
            Err(error) => {
                // Finish owned process cleanup before aborting protocol tasks;
                // an interrupted cleanup must not obscure the setup failure.
                child.lock().await.terminate().await?;
                driver.abort();
                let _ = driver.await;
                output_worker.abort();
                let _ = output_worker.await;
                return Err(error);
            }
        };
        Ok(AcpSession {
            command: command_tx,
            connection: opened.connection,
            driver: Mutex::new(Some(driver)),
            output_worker: Mutex::new(Some(output_worker)),
            child,
            closed: AtomicBool::new(false),
            session_id: opened.session_id,
            capabilities: opened.capabilities,
            agent_info: opened.agent_info,
            state: opened.state,
            prompt_lock: Arc::new(Mutex::new(())),
            turn_state,
            max_output_bytes: self.max_output_bytes,
        })
    }
}

struct Opened {
    connection: ConnectionTo<Agent>,
    session_id: String,
    capabilities: Capabilities,
    agent_info: Option<AgentInfo>,
    state: Arc<std::sync::Mutex<SessionSetup>>,
}

/// Handshake result channel — `serve` delivers `Opened` once; a connection
/// failure before that is delivered by `drive` after `connect_with` returns.
type ReadySlot = std::sync::Mutex<Option<oneshot::Sender<Result<Opened, AdapterError>>>>;

/// Everything `serve` needs beyond the connection and command channel.
struct ServeContext {
    cwd: PathBuf,
    mcp_servers: Vec<Value>,
    prefer_load: bool,
    resume: Option<String>,
    config: SessionConfig,
    output: OutputSink,
    auth_key: Option<crate::env::EnvSource>,
    env: std::collections::BTreeMap<String, std::ffi::OsString>,
    secrets: Arc<dyn SecretResolver>,
}

/// The connection foreground: performs the handshake, then serves turn
/// commands until close. The child is always terminated before return.
///
/// The child arrives as a weak handle: the opener (and later the session)
/// owns the strong one, so an opener dropped mid-establishment releases it
/// and its `Drop` kills the tree instead of orphaning a hung handshake.
#[allow(clippy::too_many_arguments)]
async fn drive(
    client_name: String,
    delegate: Arc<dyn SessionDelegate>,
    child: std::sync::Weak<Mutex<SupervisedChild>>,
    child_stdin: tokio::process::ChildStdin,
    child_stdout: tokio::process::ChildStdout,
    mut context: ServeContext,
    mut commands: mpsc::UnboundedReceiver<SessionCommand>,
    ready: Arc<ReadySlot>,
) {
    let notify_output = context.output.clone();
    let permission_delegate = delegate.clone();
    let serve_ready = ready.clone();
    let outcome = agent_client_protocol::Client
        .builder()
        .name(client_name)
        .on_receive_notification(
            async move |notification: SessionNotification, _cx| {
                // Session bookkeeping (command/mode/config/info echoes and
                // the user's own chunk) is not billable work; everything
                // else the agent produced — message, thought, tool call,
                // plan, usage or an unknown update — may have been.
                if !matches!(
                    notification.update,
                    SessionUpdate::UserMessageChunk(_)
                        | SessionUpdate::AvailableCommandsUpdate(_)
                        | SessionUpdate::CurrentModeUpdate(_)
                        | SessionUpdate::ConfigOptionUpdate(_)
                        | SessionUpdate::SessionInfoUpdate(_)
                ) {
                    notify_output.mark_served();
                }
                if let Some(event) = map_update(&notification.update) {
                    notify_output.deliver(event)?;
                }
                Ok(())
            },
            agent_client_protocol::on_receive_notification!(),
        )
        .on_receive_request(
            async move |request: RequestPermissionRequest, responder, _cx| {
                let decision = permission_delegate
                    .on_permission(permission_prompt(&request))
                    .await;
                let outcome = match decision {
                    PermissionDecision::Cancelled => RequestPermissionOutcome::Cancelled,
                    PermissionDecision::Selected(option_id) => RequestPermissionOutcome::Selected(
                        SelectedPermissionOutcome::new(option_id),
                    ),
                };
                let _ = responder.respond(RequestPermissionResponse::new(outcome));
                Ok(())
            },
            agent_client_protocol::on_receive_request!(),
        )
        .connect_with(
            ByteStreams::new(child_stdin.compat_write(), child_stdout.compat()),
            async move |connection: ConnectionTo<Agent>| {
                serve(connection, &mut context, &mut commands, serve_ready)
                    .await
                    .map_err(|error| {
                        agent_client_protocol::Error::internal_error().data(error.to_string())
                    })
            },
        )
        .await;

    // The connection is closed (or failed) — either way the process tree
    // must not outlive the session. If the opener was dropped first, its
    // dropped strong handle already killed the tree.
    if let Some(child) = child.upgrade() {
        let _ = child.lock().await.terminate().await;
    }

    if let Err(error) = outcome {
        // Handshake failures surface through `ready` if it is still pending;
        // later failures are delivered to the in-flight prompt reply inside
        // `serve`.
        if let Some(ready) = ready.lock().unwrap().take() {
            let _ = ready.send(Err(AdapterError::uncertain(
                "agent_protocol_failed",
                "session/open",
            )
            .with_detail(error.to_string())));
        }
    }
}

async fn serve(
    connection: ConnectionTo<Agent>,
    context: &mut ServeContext,
    commands: &mut mpsc::UnboundedReceiver<SessionCommand>,
    ready: Arc<ReadySlot>,
) -> Result<(), AdapterError> {
    let opened = match open_session(&connection, context).await {
        Ok(opened) => opened,
        Err(error) => {
            // Handshake failures keep their typed code/stage: deliver them
            // through `ready` rather than the connection's error channel,
            // which would flatten every failure into agent_protocol_failed.
            match ready.lock().unwrap().take() {
                Some(ready) => {
                    let _ = ready.send(Err(error));
                    return Ok(());
                }
                None => return Err(error),
            }
        }
    };
    let session_id = opened.session_id.clone();
    let state = opened.state.clone();
    match ready.lock().unwrap().take() {
        Some(ready) => {
            if ready.send(Ok(opened)).is_err() {
                // The opener went away; unwind without serving turns.
                return Ok(());
            }
        }
        None => return Ok(()),
    }

    while let Some(command) = commands.recv().await {
        let (request, turn, reply) = match command {
            SessionCommand::Prompt {
                request,
                turn,
                reply,
            } => (request, turn, reply),
            SessionCommand::Close => return Ok(()),
        };
        // A std::sync::MutexGuard is not Send across `.await`:
        // take a working copy, run the turn against it, then write
        // the resulting session state back.
        let mut working = state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone();
        let mut outcome =
            run_prompt(&connection, &session_id, &mut working, request, &turn.state).await;
        *state.lock().unwrap_or_else(|error| error.into_inner()) = working;
        if let Err(error) = context.output.drain().await {
            outcome = Err(error);
        }
        if let Ok(outcome) = &mut outcome {
            outcome.truncated = turn.state.lock().unwrap().truncated;
        }
        // Clear the old turn before allowing the caller to enqueue
        // another; retaining the owned guard also protects dropped callers.
        drop(turn);
        let _ = reply.send(outcome);
    }
    Ok(())
}

/// `initialize` + session establishment + requested configuration. Every
/// failure keeps its typed `AdapterError` for the opener.
async fn open_session(
    connection: &ConnectionTo<Agent>,
    context: &ServeContext,
) -> Result<Opened, AdapterError> {
    let initialize = InitializeRequest::new(ProtocolVersion::V1);
    let initialized = connection
        .send_request(initialize)
        .block_task()
        .await
        .map_err(|error| protocol_failure("initialize", error, false))?;
    if initialized.protocol_version != ProtocolVersion::V1 {
        return Err(AdapterError::permanent(
            "agent_protocol_version_unsupported",
            "initialize",
        ));
    }
    let load_capable = initialized.agent_capabilities.load_session;
    let resume_capable = initialized
        .agent_capabilities
        .session_capabilities
        .resume
        .is_some();
    let agent_info = initialized.agent_info.as_ref().map(|info| AgentInfo {
        name: info.name.clone(),
        version: info.version.clone(),
    });
    let auth_methods: Vec<AuthMethodId> = initialized
        .auth_methods
        .iter()
        .map(|method| method.id().clone())
        .collect();

    let mcp = decode_mcp_servers(context.mcp_servers.clone())?;
    let call = if let Some(binding) = context.resume.as_deref() {
        // Load/resume responses carry no session id — the binding we sent
        // is the id.
        if context.prefer_load && load_capable {
            SessionCall::Load {
                session_id: binding.to_owned(),
                cwd: context.cwd.clone(),
                mcp_servers: mcp,
            }
        } else if resume_capable {
            SessionCall::Resume {
                session_id: binding.to_owned(),
                cwd: context.cwd.clone(),
                mcp_servers: mcp,
            }
        } else if load_capable {
            SessionCall::Load {
                session_id: binding.to_owned(),
                cwd: context.cwd.clone(),
                mcp_servers: mcp,
            }
        } else {
            return Err(AdapterError::permanent(
                "agent_session_resume_unsupported",
                "session/resume",
            ));
        }
    } else {
        SessionCall::New {
            cwd: context.cwd.clone(),
            mcp_servers: mcp,
        }
    };
    let response = call_with_auth_retry(
        connection,
        &call,
        &auth_methods,
        context.auth_key.as_ref(),
        &context.env,
        &*context.secrets,
    )
    .await?;
    let mut setup = SessionSetup::parse(response)?;
    let session_id = match context.resume.as_deref() {
        Some(binding) => binding.to_owned(),
        None => setup.session_id.clone().ok_or_else(|| {
            AdapterError::permanent("agent_protocol_error", "session/new")
                .with_detail("session/new response did not report a sessionId")
        })?,
    };

    apply_session_config(connection, &session_id, &mut setup, &context.config).await?;

    Ok(Opened {
        connection: connection.clone(),
        session_id,
        capabilities: capabilities_from(&initialized),
        agent_info,
        state: Arc::new(std::sync::Mutex::new(setup)),
    })
}

/// One session-establishment call; load and resume share the same params
/// and response shape, new omits `sessionId`.
enum SessionCall {
    New {
        cwd: PathBuf,
        mcp_servers: Vec<McpServer>,
    },
    Load {
        session_id: String,
        cwd: PathBuf,
        mcp_servers: Vec<McpServer>,
    },
    Resume {
        session_id: String,
        cwd: PathBuf,
        mcp_servers: Vec<McpServer>,
    },
}

impl SessionCall {
    fn stage(&self) -> &'static str {
        match self {
            SessionCall::New { .. } => "session/new",
            SessionCall::Load { .. } => "session/load",
            SessionCall::Resume { .. } => "session/resume",
        }
    }

    async fn send(
        &self,
        connection: &ConnectionTo<Agent>,
    ) -> Result<Value, agent_client_protocol::Error> {
        match self {
            SessionCall::New { cwd, mcp_servers } => {
                connection
                    .send_request(SessionNewWire {
                        cwd: cwd.clone(),
                        mcp_servers: mcp_servers.clone(),
                    })
                    .block_task()
                    .await
            }
            SessionCall::Load {
                session_id,
                cwd,
                mcp_servers,
            } => {
                connection
                    .send_request(SessionLoadWire {
                        session_id: session_id.clone(),
                        cwd: cwd.clone(),
                        mcp_servers: mcp_servers.clone(),
                    })
                    .block_task()
                    .await
            }
            SessionCall::Resume {
                session_id,
                cwd,
                mcp_servers,
            } => {
                connection
                    .send_request(SessionResumeWire {
                        session_id: session_id.clone(),
                        cwd: cwd.clone(),
                        mcp_servers: mcp_servers.clone(),
                    })
                    .block_task()
                    .await
            }
        }
    }
}

/// Resolve the declared credential through the child's resolved environment
/// or its source; an unavailable key resolves to `None` and the caller
/// decides whether to still authenticate. A name the resolved environment
/// carries as empty counts as absent — the ambient host variable is only a
/// fallback for trusted launches whose spec never delivered the key, and
/// sidecar admission guarantees wire-supplied specs always carry theirs.
async fn resolve_auth_key(
    auth_key: Option<&crate::env::EnvSource>,
    env: &std::collections::BTreeMap<String, std::ffi::OsString>,
    secrets: &dyn SecretResolver,
) -> Option<String> {
    match auth_key {
        Some(crate::env::EnvSource::HostEnv { host_env }) => {
            match env.get(host_env).and_then(|v| v.to_str()) {
                Some(value) if !value.is_empty() => Some(value.to_owned()),
                Some(_) => None,
                None => std::env::var(host_env).ok().filter(|v| !v.is_empty()),
            }
        }
        Some(crate::env::EnvSource::Secret { secret }) => secrets.resolve(secret).await,
        None => None,
    }
}

/// One authenticate pass over the advertised methods: the resolved key
/// rides `_meta.api_key` when present, matching the credential-bearing
/// contract agents like Devin publish. Returns true once any method
/// succeeds.
async fn authenticate(
    connection: &ConnectionTo<Agent>,
    auth_methods: &[AuthMethodId],
    api_key: Option<&str>,
) -> bool {
    for method_id in auth_methods {
        let mut request = AuthenticateRequest::new(method_id.clone());
        if let Some(key) = api_key {
            request.meta = Some(serde_json::Map::from_iter([(
                "api_key".to_owned(),
                Value::String(key.to_owned()),
            )]));
        }
        if connection.send_request(request).block_task().await.is_ok() {
            return true;
        }
    }
    false
}

/// Send a session-establishment request. When the agent advertises auth
/// methods and the host declared a credential, authenticate *before* the
/// first establishment: some agents — `devin acp` among them — sometimes
/// accept an unauthenticated `session/new` and then answer with a degraded
/// model catalog, so waiting for an `AuthRequired` refusal is not enough.
/// Without a key the order stays demand-driven: establish first, then retry
/// once after `authenticate` if establishment fails with auth-required.
async fn call_with_auth_retry(
    connection: &ConnectionTo<Agent>,
    call: &SessionCall,
    auth_methods: &[AuthMethodId],
    auth_key: Option<&crate::env::EnvSource>,
    env: &std::collections::BTreeMap<String, std::ffi::OsString>,
    secrets: &dyn SecretResolver,
) -> Result<Value, AdapterError> {
    let api_key = resolve_auth_key(auth_key, env, secrets).await;
    if api_key.is_some() && !auth_methods.is_empty() {
        authenticate(connection, auth_methods, api_key.as_deref()).await;
    }
    let establish_error = match call.send(connection).await {
        Ok(response) => return Ok(response),
        Err(error) => error,
    };
    if auth_methods.is_empty()
        || establish_error.code != agent_client_protocol::ErrorCode::AuthRequired
    {
        return Err(protocol_failure(call.stage(), establish_error, false));
    }
    if !authenticate(connection, auth_methods, api_key.as_deref()).await {
        return Err(protocol_failure(call.stage(), establish_error, false));
    }
    call.send(connection)
        .await
        .map_err(|error| protocol_failure(call.stage(), error, false))
}

/// Apply the host's requested session configuration against the surfaces
/// the agent reported at establishment:
///
/// - `configOptions` categories win over the legacy `modes`/`models`
///   surfaces; a selectable option that lacks the requested value fails
///   closed rather than falling back.
/// - On a fresh session every surface is treated as reported, so an
///   unadvertised request fails.
/// - On a restored session an unreported surface stays unknown only when
///   the host did not request an override. Explicit selections must be
///   applied and confirmed, or session establishment fails closed.
async fn apply_session_config(
    connection: &ConnectionTo<Agent>,
    session_id: &str,
    state: &mut SessionSetup,
    requested: &SessionConfig,
) -> Result<(), AdapterError> {
    if let Some(model) = requested.model.as_deref() {
        apply_model(connection, session_id, state, model).await?;
    }
    if let Some(mode) = requested.mode.as_deref() {
        apply_mode(connection, session_id, state, mode).await?;
    }
    for (slot, requested_value) in [
        ("thought_level", requested.effort.as_deref()),
        ("permissions", requested.permission.as_deref()),
    ] {
        if let Some(value) = requested_value {
            apply_config_value(connection, session_id, state, slot, value).await?;
        }
    }
    Ok(())
}

/// Apply `model` through `configOptions` ("model" category) or, absent
/// that surface, the legacy `models`/`session/set_model` path.
async fn apply_model(
    connection: &ConnectionTo<Agent>,
    session_id: &str,
    state: &mut SessionSetup,
    model: &str,
) -> Result<(), AdapterError> {
    let options = state.config_options.as_deref().unwrap_or(&[]);
    if let Some(option) = find_option(options, "model") {
        if option_select(option).is_some() {
            if !option_offers(option, model) {
                return Err(config_unavailable("model", model));
            }
            if option_current(option) != Some(model) {
                set_config_option(connection, session_id, state, "model", model).await?;
            }
            return Ok(());
        }
    }
    let model_id = state.models.as_ref().and_then(|models| {
        models
            .available_models
            .iter()
            .find(|entry| entry.model_id == model || sanitize_model_id(&entry.model_id) == model)
            .map(|entry| entry.model_id.clone())
    });
    if let Some(model_id) = model_id {
        let current = state
            .models
            .as_ref()
            .map(|models| models.current_model_id.as_str());
        if current != Some(model_id.as_str()) {
            set_session_model(connection, session_id, state, &model_id).await?;
        }
        return Ok(());
    }
    Err(config_unavailable("model", model))
}

/// Apply `mode` through `configOptions` ("mode" category) or the
/// `modes`/`session/set_mode` path.
async fn apply_mode(
    connection: &ConnectionTo<Agent>,
    session_id: &str,
    state: &mut SessionSetup,
    mode: &str,
) -> Result<(), AdapterError> {
    let options = state.config_options.as_deref().unwrap_or(&[]);
    if let Some(option) = find_option(options, "mode") {
        if option_select(option).is_some() {
            if !option_offers(option, mode) {
                return Err(config_unavailable("mode", mode));
            }
            if option_current(option) != Some(mode) {
                set_config_option(connection, session_id, state, "mode", mode).await?;
            }
            return Ok(());
        }
    }
    let mode_offered = state.modes.as_ref().is_some_and(|modes| {
        modes
            .available_modes
            .iter()
            .any(|entry| &*entry.id.0 == mode)
    });
    if mode_offered {
        let current = state
            .modes
            .as_ref()
            .map(|modes| modes.current_mode_id.0.as_ref());
        if current != Some(mode) {
            let response = connection
                .send_request(SetSessionModeRequest::new(
                    SessionId::from(session_id.to_owned()),
                    agent_client_protocol::schema::v1::SessionModeId::from(mode.to_owned()),
                ))
                .block_task()
                .await
                .map_err(|error| protocol_failure("session/set_mode", error, false))?;
            if let Some(meta) = response.meta {
                state.meta = Some(Value::Object(meta));
            }
            if let Some(modes) = &mut state.modes {
                modes.current_mode_id =
                    agent_client_protocol::schema::v1::SessionModeId::from(mode.to_owned());
            }
        }
        return Ok(());
    }
    Err(config_unavailable("mode", mode))
}

/// Apply a value to one `configOptions` category (`thought_level`,
/// `permissions`). Fails closed when the surface is missing or does not
/// advertise the value.
async fn apply_config_value(
    connection: &ConnectionTo<Agent>,
    session_id: &str,
    state: &mut SessionSetup,
    slot: &str,
    value: &str,
) -> Result<(), AdapterError> {
    let options = state.config_options.as_deref().unwrap_or(&[]);
    match find_option(options, slot) {
        Some(option) if option_select(option).is_some() => {
            if !option_offers(option, value) {
                return Err(config_unavailable(slot, value));
            }
            if option_current(option) != Some(value) {
                set_config_option(connection, session_id, state, slot, value).await?;
            }
            Ok(())
        }
        _ => Err(config_unavailable(slot, value)),
    }
}

async fn set_config_option(
    connection: &ConnectionTo<Agent>,
    session_id: &str,
    state: &mut SessionSetup,
    slot: &str,
    value: &str,
) -> Result<(), AdapterError> {
    let config_id = find_option(state.config_options.as_deref().unwrap_or(&[]), slot)
        .map(|option| option.id.clone())
        .ok_or_else(|| config_unavailable(slot, value))?;
    let response = connection
        .send_request(SetSessionConfigOptionRequest::new(
            SessionId::from(session_id.to_owned()),
            config_id,
            value,
        ))
        .block_task()
        .await
        .map_err(|error| protocol_failure("session/set_config_option", error, false))?;
    if let Some(meta) = response.meta {
        state.meta = Some(Value::Object(meta));
    }
    state.config_options = Some(response.config_options);
    let applied = state.config_options.as_deref().unwrap_or(&[]);
    match find_option(applied, slot).and_then(option_current) {
        Some(current) if current == value => Ok(()),
        _ => Err(AdapterError::permanent(
            "agent_configuration_rejected",
            "session/set_config_option",
        )
        .with_detail(format!(
            "{slot} value {value:?} was accepted but did not read back as applied"
        ))),
    }
}

async fn set_session_model(
    connection: &ConnectionTo<Agent>,
    session_id: &str,
    state: &mut SessionSetup,
    model_id: &str,
) -> Result<(), AdapterError> {
    let response = connection
        .send_request(SetSessionModelWire {
            session_id: session_id.to_owned(),
            model_id: model_id.to_owned(),
        })
        .block_task()
        .await
        .map_err(|error| protocol_failure("session/set_model", error, false))?;
    let setup = SessionSetup::parse(response).map_err(|mut error| {
        error.stage = "session/set_model";
        error
    })?;
    if let Some(meta) = setup.meta {
        state.meta = Some(meta);
    }
    if let Some(models) = setup.models {
        let confirmed = models.current_model_id == model_id;
        state.models = Some(models);
        if !confirmed {
            return Err(AdapterError::permanent(
                "agent_configuration_rejected",
                "session/set_model",
            ));
        }
    } else if let Some(models) = &mut state.models {
        // The legacy method permits an empty success acknowledgement. Only
        // that case uses the requested value; explicit state is authoritative.
        models.current_model_id = model_id.to_owned();
    }
    Ok(())
}

fn find_option<'a>(
    options: &'a [SessionConfigOption],
    slot: &str,
) -> Option<&'a SessionConfigOption> {
    options
        .iter()
        .find(|option| option_category(option) == Some(slot))
        .or_else(|| options.iter().find(|option| &*option.id.0 == slot))
}

fn option_category(option: &SessionConfigOption) -> Option<&str> {
    match &option.category {
        Some(SessionConfigOptionCategory::Model) => Some("model"),
        Some(SessionConfigOptionCategory::Mode) => Some("mode"),
        Some(SessionConfigOptionCategory::ThoughtLevel) => Some("thought_level"),
        Some(SessionConfigOptionCategory::ModelConfig) => Some("model_config"),
        Some(SessionConfigOptionCategory::Other(name)) => Some(name.as_ref()),
        _ => None,
    }
}

fn option_select(
    option: &SessionConfigOption,
) -> Option<&agent_client_protocol::schema::v1::SessionConfigSelect> {
    match &option.kind {
        SessionConfigKind::Select(select) => Some(select),
        _ => None,
    }
}

fn option_current(option: &SessionConfigOption) -> Option<&str> {
    option_select(option).map(|select| select.current_value.0.as_ref())
}

/// True when `value` appears among the option's selectable values —
/// grouped and ungrouped option lists both count.
fn option_offers(option: &SessionConfigOption, value: &str) -> bool {
    let Some(select) = option_select(option) else {
        return false;
    };
    match &select.options {
        SessionConfigSelectOptions::Ungrouped(options) => {
            options.iter().any(|entry| &*entry.value.0 == value)
        }
        SessionConfigSelectOptions::Grouped(groups) => groups
            .iter()
            .flat_map(|group| group.options.iter())
            .any(|entry| &*entry.value.0 == value),
        _ => false,
    }
}

/// Bounded identifier normalization for matching a requested model
/// against a legacy `models` entry — same allowlist hosts apply when
/// sanitizing provider ids.
fn sanitize_model_id(raw: &str) -> String {
    const MAX_NORMALIZED_MODEL_ID_CHARS: usize = 160;
    raw.chars()
        .filter(|character| character.is_ascii_alphanumeric() || "._-+:[]=,".contains(*character))
        .take(MAX_NORMALIZED_MODEL_ID_CHARS)
        .collect()
}

fn selector_support(applied: bool) -> SelectorSupport {
    if applied {
        SelectorSupport::Applied
    } else {
        SelectorSupport::Unsupported
    }
}

fn config_selector_available(state: &SessionSetup, category: &str) -> bool {
    find_option(state.config_options.as_deref().unwrap_or(&[]), category)
        .is_some_and(|option| option_select(option).is_some())
}

fn model_selector_available(state: &SessionSetup) -> bool {
    config_selector_available(state, "model")
        || state
            .models
            .as_ref()
            .is_some_and(|models| !models.available_models.is_empty())
}

fn mode_selector_available(state: &SessionSetup) -> bool {
    config_selector_available(state, "mode")
        || state
            .modes
            .as_ref()
            .is_some_and(|modes| !modes.available_modes.is_empty())
}

fn config_value_available(state: &SessionSetup, category: &str, value: &str) -> bool {
    find_option(state.config_options.as_deref().unwrap_or(&[]), category)
        .is_some_and(|option| option_select(option).is_some() && option_offers(option, value))
}

fn model_value_available(state: &SessionSetup, requested: &str) -> bool {
    let options = state.config_options.as_deref().unwrap_or(&[]);
    if let Some(option) = find_option(options, "model") {
        if option_select(option).is_some() {
            return option_offers(option, requested);
        }
    }
    state.models.as_ref().is_some_and(|models| {
        models.available_models.iter().any(|entry| {
            entry.model_id == requested || sanitize_model_id(&entry.model_id) == requested
        })
    })
}

fn config_unavailable(slot: &str, requested: &str) -> AdapterError {
    AdapterError::permanent("agent_configuration_unavailable", "session/configuration").with_detail(
        format!("{slot} value {requested:?} is not advertised by the agent",),
    )
}

async fn run_prompt(
    connection: &ConnectionTo<Agent>,
    session_id: &str,
    state: &mut SessionSetup,
    request: TurnRequest,
    control: &std::sync::Mutex<TurnState>,
) -> Result<TurnOutcome, AdapterError> {
    let cancelled = || control.lock().unwrap().cancelled;
    if cancelled() {
        return Ok(cancelled_prompt());
    }
    if let Some(model) = request.model.as_deref() {
        let result = apply_model(connection, session_id, state, model).await;
        if cancelled() {
            return Ok(cancelled_prompt());
        }
        result?;
    }
    if let Some(effort) = request.effort.as_deref() {
        let result =
            apply_config_value(connection, session_id, state, "thought_level", effort).await;
        if cancelled() {
            return Ok(cancelled_prompt());
        }
        result?;
    }
    let pending = {
        let mut control = control.lock().unwrap();
        if control.cancelled {
            return Ok(cancelled_prompt());
        }
        // SDK send_request enqueues synchronously. Holding the same lock as
        // cancel ensures its notification cannot overtake this dispatch.
        let pending = connection.send_request(PromptRequest::new(
            session_id.to_owned(),
            vec![ContentBlock::Text(TextContent::new(request.text))],
        ));
        control.dispatched = true;
        pending
    };
    let response = pending.block_task().await.map_err(|error| {
        let served = control.lock().unwrap().served;
        protocol_failure("session/prompt", error, served)
    })?;
    Ok(TurnOutcome {
        stop_reason: stop_reason(&response.stop_reason).to_owned(),
        // The streamed text lives in `AgentEvent::MessageChunk`; consumers
        // accumulate it there. We keep `text` empty rather than double it.
        text: String::new(),
        truncated: false,
        exit_code: None,
        // Reported verbatim when the agent fills the unstable end-turn
        // usage field; absent data stays absent, never a zero.
        usage: response.usage.as_ref().map(|usage| TokenUsage {
            input: Some(usage.input_tokens),
            cached_input: usage.cached_read_tokens,
            cached_write: usage.cached_write_tokens,
            output: Some(usage.output_tokens),
            thought: usage.thought_tokens,
            total: Some(usage.total_tokens),
        }),
    })
}

fn cancelled_prompt() -> TurnOutcome {
    TurnOutcome {
        stop_reason: "cancelled".into(),
        text: String::new(),
        truncated: false,
        exit_code: None,
        usage: None,
    }
}

fn protocol_failure(
    stage: &'static str,
    error: agent_client_protocol::Error,
    served: bool,
) -> AdapterError {
    // Quota, capacity and model-availability refusals are classified from
    // the refusal itself before the JSON-RPC code is considered. ACP uses
    // `AuthRequired` (-32000) for credential failures, but OpenCode emits
    // its exhausted free-usage refusal as `InternalError` (-32603); keying
    // on the code alone would leave that incident classified as a generic
    // protocol failure.
    //
    // `served` carries the turn's own evidence: an explicit refusal that
    // arrives before any session/update is an admission rejection — the
    // provider answered "no" without running the request, so another
    // candidate may safely replay it. A refusal after streamed work cannot
    // prove nothing billable ran, so it stays uncertain-effect.
    if crate::error::quota_or_capacity_refusal(&error.to_string()) {
        return if served {
            AdapterError::uncertain("agent_quota_exhausted", stage).with_detail(error.to_string())
        } else {
            AdapterError::retryable("agent_quota_exhausted", stage).with_detail(error.to_string())
        };
    }
    if error.code == agent_client_protocol::ErrorCode::AuthRequired {
        AdapterError::retryable("agent_auth_required", stage).with_detail(error.to_string())
    } else {
        AdapterError::uncertain("agent_protocol_failed", stage).with_detail(error.to_string())
    }
}

fn stop_reason(reason: &StopReason) -> &'static str {
    match reason {
        StopReason::EndTurn => "end_turn",
        StopReason::MaxTokens => "max_tokens",
        StopReason::MaxTurnRequests => "max_turn_requests",
        StopReason::Refusal => "refusal",
        StopReason::Cancelled => "cancelled",
        _ => "end_turn",
    }
}

fn capabilities_from(
    response: &agent_client_protocol::schema::v1::InitializeResponse,
) -> Capabilities {
    Capabilities {
        streaming: true,
        cancellation: true,
        session_resume: response.agent_capabilities.load_session
            || response
                .agent_capabilities
                .session_capabilities
                .resume
                .is_some(),
        permission_prompts: true,
        detail: serde_json::to_value(&response.agent_capabilities).ok(),
    }
}

fn decode_mcp_servers(
    servers: Vec<Value>,
) -> Result<Vec<agent_client_protocol::schema::v1::McpServer>, AdapterError> {
    servers
        .into_iter()
        .map(|value| {
            serde_json::from_value(value).map_err(|error| {
                AdapterError::permanent("agent_mcp_server_invalid", "session/new")
                    .with_detail(error.to_string())
            })
        })
        .collect()
}

fn permission_prompt(request: &RequestPermissionRequest) -> PermissionPrompt {
    let fields = &request.tool_call.fields;
    let content_texts = fields
        .content
        .clone()
        .unwrap_or_default()
        .iter()
        .flat_map(|item| match item {
            ToolCallContent::Content(content) => match &content.content {
                ContentBlock::Text(text) => vec![text.text.clone()],
                _ => Vec::new(),
            },
            ToolCallContent::Diff(diff) => vec![format!(
                "{}\n{}\n→\n{}",
                diff.path.display(),
                diff.old_text.as_deref().unwrap_or("(new file)"),
                diff.new_text
            )],
            ToolCallContent::Terminal(terminal) => {
                vec![format!("Terminal: {}", terminal.terminal_id)]
            }
            _ => Vec::new(),
        })
        .collect();
    PermissionPrompt {
        session_id: request.session_id.to_string(),
        tool_call_id: request.tool_call.tool_call_id.to_string(),
        title: fields
            .title
            .clone()
            .or_else(|| fields.name.clone())
            .unwrap_or_else(|| "Agent action".to_owned()),
        kind: fields
            .kind
            .as_ref()
            .and_then(|kind| serde_json::to_value(kind).ok())
            .and_then(|value| value.as_str().map(str::to_owned)),
        content_texts,
        options: request
            .options
            .iter()
            .map(|option| {
                (
                    option.option_id.to_string(),
                    option.name.clone(),
                    serde_json::to_value(option.kind)
                        .ok()
                        .and_then(|value| value.as_str().map(str::to_owned))
                        .unwrap_or_default(),
                )
            })
            .collect(),
        raw: serde_json::to_value(request).unwrap_or(Value::Null),
    }
}

/// Map a session update onto the canonical event model. Zero-assumption:
/// updates without a canonical kind become raw events, subject to the output budget.
fn map_update(update: &SessionUpdate) -> Option<AgentEvent> {
    let event = match update {
        SessionUpdate::AgentMessageChunk(chunk) => match &chunk.content {
            ContentBlock::Text(text) => AgentEvent::MessageChunk {
                text: text.text.clone(),
            },
            other => AgentEvent::Raw {
                raw: serde_json::to_value(other).unwrap_or(Value::Null),
            },
        },
        SessionUpdate::ToolCall(call) => AgentEvent::ToolCall {
            summary: {
                let title = call.title.trim();
                if title.is_empty() {
                    call.name
                        .clone()
                        .unwrap_or_else(|| "Agent tool call".to_owned())
                } else {
                    title.to_owned()
                }
            },
            raw: serde_json::to_value(call).ok(),
        },
        SessionUpdate::ToolCallUpdate(update) => AgentEvent::ToolCall {
            summary: "Agent tool call update".to_owned(),
            raw: serde_json::to_value(update).ok(),
        },
        SessionUpdate::Plan(plan) => AgentEvent::Plan {
            raw: serde_json::to_value(plan).unwrap_or(Value::Null),
        },
        other => AgentEvent::Raw {
            raw: serde_json::to_value(other).unwrap_or(Value::Null),
        },
    };
    Some(event)
}

#[cfg(test)]
mod protocol_failure_tests {
    use super::*;

    #[test]
    fn quota_rate_limit_is_retryable_quota_exhausted_not_auth() {
        let mut error = agent_client_protocol::Error::auth_required();
        error.message = "AI.Error: Rate limit exceeded. Please try again later.".into();
        // Refused before any turn output: an explicit "no" means nothing
        // ran, so custody may replay the request on the next candidate.
        let mapped = protocol_failure("session/prompt", error, false);
        assert_eq!(mapped.code, "agent_quota_exhausted");
        assert!(mapped.retryable);
        assert!(!mapped.uncertain_effect);
    }

    #[test]
    fn quota_refusal_after_turn_output_stays_uncertain() {
        let mut error = agent_client_protocol::Error::auth_required();
        error.message = "Rate limit exceeded".into();
        // A refusal arriving after the turn already produced work cannot
        // prove nothing billable ran — replaying it could double-charge.
        let mapped = protocol_failure("session/prompt", error, true);
        assert_eq!(mapped.code, "agent_quota_exhausted");
        assert!(mapped.retryable);
        assert!(mapped.uncertain_effect);
    }

    #[test]
    fn credential_auth_required_stays_auth() {
        let mapped = protocol_failure(
            "session/prompt",
            agent_client_protocol::Error::auth_required(),
            false,
        );
        assert_eq!(mapped.code, "agent_auth_required");
    }

    /// Recorded OpenCode refusal: a sanitized excerpt of the server's ACP
    /// error keeps its JSON-RPC code (-32603, not AuthRequired), message and
    /// cause type. The free-usage exhaustion must surface as a quota
    /// refusal so custody can fail over instead of recording a generic
    /// protocol failure.
    #[test]
    fn recorded_opencode_quota_refusal_on_internal_error_is_quota_exhausted() {
        const FIXTURE: &str = include_str!("../../tests/fixtures/opencode-acp-quota-refusal.json");
        let fixture: Value = serde_json::from_str(FIXTURE).expect("recorded refusal fixture");
        let recorded = &fixture["recorded_error"];
        let code = recorded["code"].as_i64().expect("error code") as i32;
        assert_ne!(
            code,
            i32::from(agent_client_protocol::ErrorCode::AuthRequired),
            "the incident was an InternalError, which the old auth-only branch missed"
        );
        let error = agent_client_protocol::Error::new(
            code,
            recorded["message"].as_str().expect("refusal message"),
        )
        .data(serde_json::json!({
            "type": recorded["cause_type"].as_str().expect("cause type"),
        }));
        let mapped = protocol_failure("session/prompt", error, false);
        assert_eq!(mapped.code, "agent_quota_exhausted");
        assert!(mapped.retryable);
        assert!(
            !mapped.uncertain_effect,
            "an admission refusal had no billable effect — custody may replay it"
        );
        assert!(
            mapped
                .detail
                .as_deref()
                .is_some_and(|detail| detail.contains("Free usage limit reached")),
            "{mapped}"
        );
    }

    #[test]
    fn internal_error_without_a_quota_refusal_stays_protocol_failed() {
        let mapped = protocol_failure(
            "session/prompt",
            agent_client_protocol::Error::new(-32603, "Internal server error"),
            false,
        );
        assert_eq!(mapped.code, "agent_protocol_failed");
        assert!(mapped.uncertain_effect);
    }

    #[tokio::test]
    async fn an_empty_resolved_auth_key_never_refills_from_the_host() {
        use std::ffi::OsString;
        // PATH is populated in every ambient process environment: a resolved
        // empty value must count as absent, not as an invitation to re-read
        // the host.
        let env = std::collections::BTreeMap::from([("PATH".to_owned(), OsString::from(""))]);
        let resolved = resolve_auth_key(
            Some(&crate::env::EnvSource::HostEnv {
                host_env: "PATH".to_owned(),
            }),
            &env,
            &crate::env::NoSecrets,
        )
        .await;
        assert_eq!(resolved, None);
    }

    #[tokio::test]
    async fn an_absent_auth_key_still_resolves_from_the_host() {
        // Trusted launches keep the ambient fallback: a name the resolved
        // environment does not carry at all still reads the host variable.
        let env = std::collections::BTreeMap::new();
        let resolved = resolve_auth_key(
            Some(&crate::env::EnvSource::HostEnv {
                host_env: "PATH".to_owned(),
            }),
            &env,
            &crate::env::NoSecrets,
        )
        .await;
        assert_eq!(resolved, std::env::var("PATH").ok());
    }
}
