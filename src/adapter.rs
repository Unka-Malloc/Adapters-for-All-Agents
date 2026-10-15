//! The unified adapter facade.
//!
//! `Adapter` is the single entry point a host holds: it owns one
//! [`AgentSpec`], resolves environment, spawns the lane, and returns a
//! uniform [`AgentSession`] regardless of wire form (ACP, generic JSON-RPC,
//! CLI stdio/PTY). Turn events stream through the `mpsc` sink supplied at
//! `open_session`; outcomes come back from `prompt`.

use std::sync::Arc;
use tokio::sync::mpsc;

use crate::env::SecretResolver;
use crate::error::AdapterError;
use crate::model::{
    AgentEvent, AgentSpec, Capabilities, LaneSpec, SelectorPhase, SelectorSupport,
    SelectorSupportSet, SessionRequest, TurnOutcome,
};
use crate::observe::SharedObserver;
use crate::proto::acp::{AcpLane, AcpSession, PermissionDecision, SessionDelegate};
use crate::proto::cli::CliLane;
use crate::proto::rpc::{RpcLane, RpcSession, RpcSessionDelegate};

pub use crate::model::TurnRequest;

/// Permission decisions host-side (ACP lane). Other lanes never call this.
#[async_trait::async_trait]
pub trait PermissionHandler: Send + Sync {
    async fn decide(&self, prompt: crate::proto::acp::PermissionPrompt) -> PermissionDecision;
}

/// Runs once per turn on every lane, before the prompt reaches the wire —
/// the hook for durable prompt reservations; an error aborts the turn
/// before the request is sent, so a crash never silently re-bills a prompt.
#[async_trait::async_trait]
pub trait BeforeTurn: Send + Sync {
    async fn run(&self) -> Result<(), AdapterError>;
}

/// Lane-erased open session. `before_turn` rides on the handle so the hook
/// fires on every lane — an error aborts the turn before anything reaches
/// the wire.
pub struct AgentSession {
    lane: LaneSession,
    before_turn: Option<Arc<dyn BeforeTurn>>,
}

/// The lane's own session handle behind the erased [`AgentSession`].
pub enum LaneSession {
    Acp(AcpSession),
    Rpc(Box<RpcSession>),
    Cli(CliSession),
}

/// CLI session: one process per turn.
pub struct CliSession {
    lane: Arc<CliLane>,
    request: SessionRequest,
    events: mpsc::UnboundedSender<AgentEvent>,
}

impl CliSession {
    /// The session binding the agent reported on stdout, or `""` until one
    /// arrives (`sessionIdPath` on the lane spec enables capture).
    pub fn session_id(&self) -> &str {
        self.lane.observed.get().map_or("", String::as_str)
    }
}

impl AgentSession {
    /// The concrete lane session, for callers that need lane-specific
    /// detail (e.g. negotiated capabilities).
    pub fn lane(&self) -> &LaneSession {
        &self.lane
    }

    pub fn session_id(&self) -> &str {
        match &self.lane {
            LaneSession::Acp(session) => session.session_id(),
            LaneSession::Rpc(session) => session.session_id().unwrap_or(""),
            LaneSession::Cli(session) => session.session_id(),
        }
    }

    /// Report this open session's selector support. ACP values reflect the
    /// session's negotiated configuration choices; other lanes use their
    /// active AgentSpec templates.
    pub fn selector_support(&self, phase: SelectorPhase) -> SelectorSupportSet {
        match &self.lane {
            LaneSession::Acp(session) => session.selector_support(phase),
            LaneSession::Rpc(session) => session.selector_support(phase),
            LaneSession::Cli(session) => {
                LaneSpec::Cli(Box::new(session.lane.lane.clone())).selector_support(phase)
            }
        }
    }

    /// Validate the effective turn selectors without prompting the
    /// agent. This runs before BeforeTurn, so unsupported selectors cannot
    /// reserve paid work.
    pub fn validate_turn_request(&self, request: &TurnRequest) -> Result<(), AdapterError> {
        match &self.lane {
            LaneSession::Acp(session) => session.validate_turn_request(request),
            _ => validate_turn_selectors(self.selector_support(SelectorPhase::Turn), request),
        }
    }

    /// Submit one prompt turn to completion. The `before_turn` hook runs
    /// first on every lane; an error aborts the turn before the agent is
    /// asked anything.
    pub async fn prompt(&self, request: TurnRequest) -> Result<TurnOutcome, AdapterError> {
        self.validate_turn_request(&request)?;
        if let Some(hook) = &self.before_turn {
            hook.run().await?;
        }
        match &self.lane {
            LaneSession::Acp(session) => session.prompt(request).await,
            LaneSession::Rpc(session) => {
                session
                    .prompt(
                        &request.text,
                        request.model.as_deref(),
                        request.effort.as_deref(),
                    )
                    .await
            }
            LaneSession::Cli(session) => {
                session
                    .lane
                    .run_turn(
                        &session.request,
                        &request.text,
                        request.model.as_deref(),
                        request.effort.as_deref(),
                        session.events.clone(),
                    )
                    .await
            }
        }
    }

    /// Explicit user cancellation of the in-flight turn.
    ///
    /// Lanes without a cancel surface report `agent_cancel_unsupported`.
    /// Never call this for transport errors — a broken pipe is not a user
    /// cancellation.
    pub async fn cancel(&self) -> Result<(), AdapterError> {
        match &self.lane {
            LaneSession::Acp(session) => session.cancel(),
            LaneSession::Rpc(session) => session.cancel().await,
            LaneSession::Cli(session) => {
                session.lane.cancelled.send_replace(());
                Ok(())
            }
        }
    }

    /// Close the session and terminate the process tree.
    pub async fn close(self) -> Result<(), AdapterError> {
        match self.lane {
            LaneSession::Acp(session) => session.close().await,
            LaneSession::Rpc(session) => session.close().await,
            LaneSession::Cli(_) => Ok(()),
        }
    }
}

/// One configured agent adapter.
pub struct Adapter {
    spec: AgentSpec,
    secrets: Arc<dyn SecretResolver>,
    observer: SharedObserver,
    permission: Option<Arc<dyn PermissionHandler>>,
    before_turn: Option<Arc<dyn BeforeTurn>>,
}

fn validate_session_config(spec: &AgentSpec, request: &SessionRequest) -> Result<(), AdapterError> {
    let config = &request.config;
    let support = match &spec.lane {
        LaneSpec::Rpc(rpc) => rpc.session_selector_support(
            request
                .resume
                .as_deref()
                .is_some_and(|value| !value.trim().is_empty()),
        ),
        _ => spec.selector_support(SelectorPhase::Session),
    };
    validate_session_selector(
        config.model.as_deref(),
        support.model,
        "agent_model_unsupported",
    )?;
    validate_session_selector(
        config.effort.as_deref(),
        support.effort,
        "agent_effort_unsupported",
    )?;
    validate_session_selector(
        config.mode.as_deref(),
        support.mode,
        "agent_mode_unsupported",
    )?;
    validate_session_selector(
        config.permission.as_deref(),
        support.permission,
        "agent_permission_unsupported",
    )?;

    if let LaneSpec::Cli(lane) = &spec.lane {
        validate_cli_allowlist(
            config.mode.as_deref(),
            &lane.execution_modes,
            "agent_mode_unsupported",
        )?;
        validate_cli_allowlist(
            config.permission.as_deref(),
            &lane.permission_modes,
            "agent_permission_unsupported",
        )?;
    }
    Ok(())
}

fn validate_session_selector(
    value: Option<&str>,
    support: SelectorSupport,
    code: &'static str,
) -> Result<(), AdapterError> {
    if value.is_some_and(|value| value.trim().is_empty() || support == SelectorSupport::Unsupported)
    {
        return Err(AdapterError::permanent(code, "session/configuration"));
    }
    Ok(())
}

fn validate_cli_allowlist(
    value: Option<&str>,
    allowed: &[String],
    code: &'static str,
) -> Result<(), AdapterError> {
    if let Some(value) = value.filter(|value| !value.trim().is_empty()) {
        if !allowed.is_empty() && !allowed.iter().any(|candidate| candidate == value) {
            return Err(AdapterError::permanent(code, "session/configuration"));
        }
    }
    Ok(())
}

fn validate_turn_selectors(
    support: SelectorSupportSet,
    request: &TurnRequest,
) -> Result<(), AdapterError> {
    validate_turn_selector(
        request.model.as_deref(),
        support.model,
        "agent_model_unsupported",
    )?;
    validate_turn_selector(
        request.effort.as_deref(),
        support.effort,
        "agent_effort_unsupported",
    )
}

fn validate_turn_selector(
    value: Option<&str>,
    support: SelectorSupport,
    code: &'static str,
) -> Result<(), AdapterError> {
    if value.is_some_and(|value| value.trim().is_empty() || support == SelectorSupport::Unsupported)
    {
        return Err(AdapterError::permanent(code, "turn/configuration"));
    }
    Ok(())
}

impl Adapter {
    pub fn new(spec: AgentSpec) -> Self {
        Self {
            spec,
            secrets: Arc::new(crate::env::NoSecrets),
            observer: crate::observe::none(),
            permission: None,
            before_turn: None,
        }
    }

    pub fn secrets(mut self, secrets: Arc<dyn SecretResolver>) -> Self {
        self.secrets = secrets;
        self
    }

    pub fn observer(mut self, observer: SharedObserver) -> Self {
        self.observer = observer;
        self
    }

    pub fn permission(mut self, handler: Arc<dyn PermissionHandler>) -> Self {
        self.permission = Some(handler);
        self
    }

    pub fn before_turn(mut self, hook: Arc<dyn BeforeTurn>) -> Self {
        self.before_turn = Some(hook);
        self
    }

    pub fn id(&self) -> &str {
        &self.spec.id
    }

    pub fn spec(&self) -> &AgentSpec {
        &self.spec
    }

    /// Negotiated-static capability picture; exact values for ACP arrive
    /// after `initialize` on the session.
    pub fn capabilities(&self) -> Capabilities {
        match &self.spec.lane {
            LaneSpec::Acp { .. } => Capabilities {
                streaming: true,
                cancellation: true,
                session_resume: true,
                permission_prompts: true,
                detail: None,
            },
            LaneSpec::Rpc(spec) => spec.capabilities(),
            LaneSpec::Cli(spec) => spec.capabilities(),
        }
    }

    /// Open one session: spawn the agent, establish the lane, return the
    /// uniform session handle. Events for every turn of this session go to
    /// `events`.
    pub async fn open_session(
        &self,
        request: SessionRequest,
        events: mpsc::UnboundedSender<AgentEvent>,
    ) -> Result<AgentSession, AdapterError> {
        validate_session_config(&self.spec, &request)?;
        match &self.spec.lane {
            LaneSpec::Acp {
                mcp_servers,
                prefer_load,
            } => {
                struct Bridge {
                    events: mpsc::UnboundedSender<AgentEvent>,
                    permission: Option<Arc<dyn PermissionHandler>>,
                }
                #[async_trait::async_trait]
                impl SessionDelegate for Bridge {
                    async fn on_event(&self, event: AgentEvent) {
                        let _ = self.events.send(event);
                    }
                    async fn on_permission(
                        &self,
                        prompt: crate::proto::acp::PermissionPrompt,
                    ) -> PermissionDecision {
                        match self.permission.as_ref() {
                            Some(handler) => handler.decide(prompt).await,
                            None => PermissionDecision::Cancelled,
                        }
                    }
                }
                let lane = AcpLane {
                    command: self.spec.command.clone(),
                    mcp_servers: mcp_servers.clone(),
                    prefer_load: *prefer_load,
                    client_name: self
                        .spec
                        .client_name
                        .clone()
                        .unwrap_or_else(|| "a4aa".to_owned()),
                    secrets: self.secrets.clone(),
                    observer: self.observer.clone(),
                };
                let session = lane
                    .open(
                        request,
                        Arc::new(Bridge {
                            events,
                            permission: self.permission.clone(),
                        }),
                    )
                    .await?;
                Ok(AgentSession {
                    lane: LaneSession::Acp(session),
                    before_turn: self.before_turn.clone(),
                })
            }
            LaneSpec::Rpc(spec) => {
                struct Bridge {
                    events: mpsc::UnboundedSender<AgentEvent>,
                }
                #[async_trait::async_trait]
                impl RpcSessionDelegate for Bridge {
                    async fn on_event(&self, event: AgentEvent) {
                        let _ = self.events.send(event);
                    }
                }
                let lane = RpcLane {
                    command: self.spec.command.clone(),
                    spec: (**spec).clone(),
                    secrets: self.secrets.clone(),
                    observer: self.observer.clone(),
                };
                let session = lane.open(request, Arc::new(Bridge { events })).await?;
                Ok(AgentSession {
                    lane: LaneSession::Rpc(Box::new(session)),
                    before_turn: self.before_turn.clone(),
                })
            }
            LaneSpec::Cli(lane_spec) => {
                let lane = CliLane {
                    command: self.spec.command.clone(),
                    lane: (**lane_spec).clone(),
                    secrets: self.secrets.clone(),
                    observer: self.observer.clone(),
                    max_output_bytes: self.spec.max_output_bytes,
                    cancelled: Arc::new(tokio::sync::watch::channel(()).0),
                    observed: Arc::new(std::sync::OnceLock::new()),
                };
                // An explicitly requested resume is already the session's
                // binding — report it before the first turn emits anything.
                if let Some(resume) = request.resume.as_deref().filter(|v| !v.trim().is_empty()) {
                    let _ = lane.observed.set(resume.to_owned());
                }
                // A declared session-creation command (`create-chat` style)
                // runs once here when the request carries no binding; the
                // observed id resumes every later turn of this session.
                lane.open_binding(&request).await?;
                Ok(AgentSession {
                    lane: LaneSession::Cli(CliSession {
                        lane: Arc::new(lane),
                        request,
                        events,
                    }),
                    before_turn: self.before_turn.clone(),
                })
            }
        }
    }
}
