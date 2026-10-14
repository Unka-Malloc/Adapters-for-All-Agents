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
use crate::model::{AgentEvent, AgentSpec, Capabilities, LaneSpec, SessionRequest, TurnOutcome};
use crate::observe::SharedObserver;
use crate::proto::acp::{AcpLane, AcpSession, PermissionDecision, SessionDelegate};
use crate::proto::cli::CliLane;
use crate::proto::rpc::{RpcLane, RpcSession, RpcSessionDelegate};

/// One prompt submission.
#[derive(Clone, Debug, Default)]
pub struct TurnRequest {
    pub text: String,
    pub model: Option<String>,
    pub effort: Option<String>,
}

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
    pub fn session_id(&self) -> &str {
        ""
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

    /// Submit one prompt turn to completion. The `before_turn` hook runs
    /// first on every lane; an error aborts the turn before the agent is
    /// asked anything.
    pub async fn prompt(&self, request: TurnRequest) -> Result<TurnOutcome, AdapterError> {
        if let Some(hook) = &self.before_turn {
            hook.run().await?;
        }
        match &self.lane {
            LaneSession::Acp(session) => session.prompt(request.text).await,
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
            LaneSession::Rpc(session) => session.cancel(),
            LaneSession::Cli(_) => Err(AdapterError::permanent(
                "agent_cancel_unsupported",
                "session/cancel",
            )),
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
            LaneSpec::Cli(_) => Capabilities {
                streaming: true,
                cancellation: false,
                session_resume: false,
                permission_prompts: false,
                detail: None,
            },
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
                    lane: lane_spec.clone(),
                    secrets: self.secrets.clone(),
                    observer: self.observer.clone(),
                    max_output_bytes: self.spec.max_output_bytes,
                };
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
