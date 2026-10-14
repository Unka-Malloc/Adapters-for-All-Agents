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
    CancelNotification, ContentBlock, InitializeRequest, LoadSessionRequest, NewSessionRequest,
    PromptRequest, RequestPermissionOutcome, RequestPermissionRequest, RequestPermissionResponse,
    ResumeSessionRequest, SelectedPermissionOutcome, SessionNotification, SessionUpdate,
    StopReason, TextContent, ToolCallContent,
};
use agent_client_protocol::schema::ProtocolVersion;
use agent_client_protocol::{Agent, ByteStreams, ConnectionTo};
use serde_json::Value;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot, Mutex};
use tokio::task::JoinHandle;
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

use crate::bounds;
use crate::env::SecretResolver;
use crate::error::AdapterError;
use crate::model::{AgentEvent, Capabilities, CommandSpec, SessionRequest, TurnOutcome};
use crate::observe::SharedObserver;
use crate::supervision::SupervisedChild;
use crate::transport::stdio;

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
    /// Streamed agent output — already dispatched as it arrives.
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

enum SessionCommand {
    Prompt {
        text: String,
        reply: oneshot::Sender<Result<TurnOutcome, AdapterError>>,
    },
    Close,
}

/// An open ACP session: one process, one connection, one session id.
pub struct AcpSession {
    command: mpsc::UnboundedSender<SessionCommand>,
    connection: ConnectionTo<Agent>,
    driver: JoinHandle<()>,
    session_id: String,
    capabilities: Capabilities,
    prompt_lock: Mutex<()>,
}

impl AcpSession {
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    pub fn capabilities(&self) -> &Capabilities {
        &self.capabilities
    }

    /// Submit one prompt turn. Serialized: a second `prompt` waits for the
    /// in-flight one rather than interleaving on the wire.
    pub async fn prompt(&self, text: impl Into<String>) -> Result<TurnOutcome, AdapterError> {
        let _guard = self.prompt_lock.lock().await;
        let (reply, outcome) = oneshot::channel();
        self.command
            .send(SessionCommand::Prompt {
                text: text.into(),
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
        self.connection
            .send_notification(CancelNotification::new(self.session_id.clone()))
            .map_err(|error| {
                AdapterError::uncertain("agent_cancel_failed", "session/cancel")
                    .with_detail(error.to_string())
            })
    }

    /// Close the session: end the command loop, let the connection finish,
    /// and terminate the process tree.
    pub async fn close(self) -> Result<(), AdapterError> {
        let _ = self.command.send(SessionCommand::Close);
        drop(self.command);
        let _ = self.driver.await;
        Ok(())
    }
}

/// ACP lane bound to one command spec.
pub struct AcpLane {
    pub command: CommandSpec,
    pub mcp_servers: Vec<Value>,
    pub prefer_load: bool,
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
            tokio::spawn(bounds::drain_stderr(stderr, self.observer.clone()));
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

        let driver = tokio::spawn(drive(
            client_name,
            delegate,
            child,
            child_stdin,
            child_stdout,
            ServeContext {
                cwd,
                mcp_servers,
                prefer_load,
                resume,
            },
            command_rx,
            ready,
        ));

        let opened = ready_rx
            .await
            .map_err(|_| AdapterError::uncertain("agent_connection_closed", "session/open"))??;
        Ok(AcpSession {
            command: command_tx,
            connection: opened.connection,
            driver,
            session_id: opened.session_id,
            capabilities: opened.capabilities,
            prompt_lock: Mutex::new(()),
        })
    }
}

struct Opened {
    connection: ConnectionTo<Agent>,
    session_id: String,
    capabilities: Capabilities,
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
}

/// The connection foreground: performs the handshake, then serves turn
/// commands until close. The child is always terminated before return.
#[allow(clippy::too_many_arguments)]
async fn drive(
    client_name: String,
    delegate: Arc<dyn SessionDelegate>,
    mut child: SupervisedChild,
    child_stdin: tokio::process::ChildStdin,
    child_stdout: tokio::process::ChildStdout,
    mut context: ServeContext,
    mut commands: mpsc::UnboundedReceiver<SessionCommand>,
    ready: Arc<ReadySlot>,
) {
    let notify_delegate = delegate.clone();
    let permission_delegate = delegate.clone();
    let serve_ready = ready.clone();
    let outcome = agent_client_protocol::Client
        .builder()
        .name(client_name)
        .on_receive_notification(
            async move |notification: SessionNotification, _cx| {
                if let Some(event) = map_update(&notification.update) {
                    notify_delegate.on_event(event).await;
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
    // must not outlive the session.
    let _ = child.terminate().await;

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
    let initialize = InitializeRequest::new(ProtocolVersion::V1);
    let initialized = connection
        .send_request(initialize)
        .block_task()
        .await
        .map_err(|error| protocol_failure("initialize", error))?;
    if initialized.protocol_version != ProtocolVersion::V1 {
        return Err(AdapterError::permanent(
            "agent_protocol_version_unsupported",
            "initialize",
        ));
    }
    let load_capable = initialized.agent_capabilities.load_session;

    let mcp = decode_mcp_servers(context.mcp_servers.clone())?;
    let mut session_id = None;
    if let Some(binding) = context.resume.as_deref() {
        // Load/resume responses carry no session id — the binding we sent
        // is the id.
        if context.prefer_load && load_capable {
            let mut request = LoadSessionRequest::new(binding.to_owned(), context.cwd.clone());
            request.mcp_servers = mcp.clone();
            connection
                .send_request(request)
                .block_task()
                .await
                .map_err(|error| protocol_failure("session/load", error))?;
        } else {
            let mut request = ResumeSessionRequest::new(binding.to_owned(), context.cwd.clone());
            request.mcp_servers = mcp.clone();
            connection
                .send_request(request)
                .block_task()
                .await
                .map_err(|error| protocol_failure("session/resume", error))?;
        }
        session_id = Some(binding.to_owned());
    }
    let session_id = match session_id {
        Some(id) => id,
        None => {
            let mut request = NewSessionRequest::new(context.cwd.clone());
            request.mcp_servers = mcp;
            connection
                .send_request(request)
                .block_task()
                .await
                .map_err(|error| protocol_failure("session/new", error))?
                .session_id
                .to_string()
        }
    };

    let capabilities = capabilities_from(&initialized);
    let opened = Opened {
        connection: connection.clone(),
        session_id: session_id.clone(),
        capabilities,
    };
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
        match command {
            SessionCommand::Prompt { text, reply } => {
                let outcome = run_prompt(&connection, &session_id, text).await;
                let _ = reply.send(outcome);
            }
            SessionCommand::Close => return Ok(()),
        }
    }
    Ok(())
}

async fn run_prompt(
    connection: &ConnectionTo<Agent>,
    session_id: &str,
    text: String,
) -> Result<TurnOutcome, AdapterError> {
    let response = connection
        .send_request(PromptRequest::new(
            session_id.to_owned(),
            vec![ContentBlock::Text(TextContent::new(text))],
        ))
        .block_task()
        .await
        .map_err(|error| protocol_failure("session/prompt", error))?;
    Ok(TurnOutcome {
        stop_reason: stop_reason(&response.stop_reason).to_owned(),
        // The streamed text lives in `AgentEvent::MessageChunk`; consumers
        // accumulate it there. We keep `text` empty rather than double it.
        text: String::new(),
        truncated: false,
        exit_code: None,
    })
}

fn protocol_failure(stage: &'static str, error: agent_client_protocol::Error) -> AdapterError {
    AdapterError::uncertain("agent_protocol_failed", stage).with_detail(error.to_string())
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
        session_resume: response.agent_capabilities.load_session,
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
/// updates that carry no canonical kind are forwarded raw — never dropped.
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
