//! `AgentCard` — the complete per-agent descriptor an `a4aa-<agent>` crate
//! exports.
//!
//! A [`Dialect`] describes where a host may *write* (hooks, rules, skills,
//! MCP config). The card carries everything else a host needs to know about
//! an agent as data:
//!
//! - [`DetectSurface`] — how to find the agent on a machine: binary and
//!   process names, bundled-binary directories, config markers, desktop
//!   bundles.
//! - [`AgentSurface`] — how to reach it: the lane-family declaration from
//!   which [`AgentCard::spec`] materializes a runtime [`AgentSpec`].
//! - [`FeatureSurface`] — what it natively supports: instruction channel,
//!   execution/permission modes, attachments, steering, model catalog.
//! - [`UsageSurface`] — where token accounting appears, live on the wire
//!   and in persisted session stores.
//! - [`HistoryRoot`] — where its conversation/session history lives.
//!
//! Cards are `const` data: a host reads them, a lane honors the declared
//! surface, and no secret or user path is ever stored in one (`{home}` and
//! `{sessionId}` are placeholders the consumer expands).

use serde::{Deserialize, Serialize};

use crate::dialect::Dialect;
use crate::env::{EnvPolicy, EnvSource};
use crate::model::{
    AgentSpec, BindingReceiptShape, CliLaneSpec, CommandSpec, LaneSpec, PromptChannel, RpcCallSpec,
    RpcCompletionSpec, RpcEnvelopeSpec, RpcLaneSpec, SessionScope, StreamMode,
};

/// The complete descriptor for one agent.
pub struct AgentCard {
    /// Catalog id — identical to `dialect.id`.
    pub id: &'static str,
    /// Display name.
    pub label: &'static str,
    /// Mechanism write surfaces (hooks, rules, skills, …).
    pub dialect: &'static Dialect,
    /// How to reach the agent.
    pub surface: AgentSurface,
    /// Machine-discovery facts.
    pub detect: DetectSurface,
    /// Natively supported features.
    pub features: FeatureSurface,
    /// Token-accounting locations.
    pub usage: UsageSurface,
    /// Session-store and history locations.
    pub history: &'static [HistoryRoot],
}

impl AgentCard {
    /// Materialize the runtime spec for this agent.
    ///
    /// `command.program` carries the canonical binary name; a host that
    /// resolved an absolute path during detection substitutes it before
    /// spawning. Session resume is request-driven: a host holding a binding
    /// sets `SessionRequest::resume` and the lane applies the declared
    /// `sessionResume`/`resumeArgs` surface — a spec that cannot express
    /// resume fails closed at open.
    pub fn spec(&self) -> AgentSpec {
        let lane = match &self.surface {
            AgentSurface::Acp {
                prefer_load,
                auth_key,
                ..
            } => LaneSpec::Acp {
                mcp_servers: Vec::new(),
                prefer_load: *prefer_load,
                auth_key: auth_key.map(AcpAuthKey::source),
            },
            AgentSurface::Cli(cli) => LaneSpec::Cli(Box::new(CliLaneSpec {
                args: cli.args.iter().map(|arg| (*arg).to_owned()).collect(),
                prompt_channel: cli.prompt_channel,
                stream: cli.stream,
                resume_args: cli
                    .resume_args
                    .iter()
                    .map(|arg| (*arg).to_owned())
                    .collect(),
                stdin_template: cli.stdin_template.map(|text| {
                    serde_json::from_str(text).expect("a4aa card stdin template is json")
                }),
                json_lines: cli.json_lines,
                session_id_path: cli.session_id_path.map(str::to_owned),
                usage_path: self.usage.live.as_ref().map(|usage| usage.path.to_owned()),
                open_args: cli.open_args.iter().map(|arg| (*arg).to_owned()).collect(),
                binding_file_env: cli.binding_file_env.map(str::to_owned),
                binding_receipt_shape: cli.binding_receipt_shape,
                result_selector: cli.result_selector.map(str::to_owned),
                result_selector_field: cli.result_selector_field.map(str::to_owned),
                result_path: cli.result_path.map(str::to_owned),
                execution_modes: self
                    .features
                    .execution_modes
                    .iter()
                    .map(|mode| (*mode).to_owned())
                    .collect(),
                permission_modes: self
                    .features
                    .permission_modes
                    .iter()
                    .map(|mode| (*mode).to_owned())
                    .collect(),
            })),
            AgentSurface::Rpc(rpc) => LaneSpec::Rpc(Box::new(RpcLaneSpec {
                initialize: rpc.initialize.map(RpcCallDecl::call),
                initialized: rpc.initialized.map(str::to_owned),
                session_open: rpc.session_open.map(RpcCallDecl::call),
                session_resume: rpc.session_resume.map(RpcCallDecl::call),
                prompt: rpc.prompt.call(),
                completion: rpc.completion.map(|done| RpcCompletionSpec {
                    method: done.method.to_owned(),
                    turn_id_path: done.turn_id_path.map(str::to_owned),
                    status_path: done.status_path.map(str::to_owned),
                    status_value: done.status_value.map(str::to_owned),
                }),
                cancel: rpc.cancel.map(RpcCallDecl::call),
                session_close: rpc.session_close.map(RpcCallDecl::call),
                text_event_method: rpc.text_event_method.map(str::to_owned),
                text_path: rpc.text_path.map(str::to_owned),
                result_text_path: rpc.result_text_path.map(str::to_owned),
                pre_turn: rpc.pre_turn.iter().map(|decl| decl.call()).collect(),
                session_scope: rpc.session_scope,
                kill_on_cancel: rpc.kill_on_cancel,
                envelope: rpc.envelope.map(|envelope| RpcEnvelopeSpec {
                    method_field: envelope.method_field.to_owned(),
                    string_ids: envelope.string_ids,
                    params_inline: envelope.params_inline,
                    response_method: envelope.response_method.map(str::to_owned),
                    data_field: envelope.data_field.map(str::to_owned),
                    result_field: envelope.result_field.map(str::to_owned),
                    error_field: envelope.error_field.map(str::to_owned),
                    success_field: envelope.success_field.map(str::to_owned),
                    command_field: envelope.command_field.map(str::to_owned),
                }),
                passthrough: rpc
                    .passthrough
                    .iter()
                    .map(|method| (*method).to_owned())
                    .collect(),
            })),
        };
        AgentSpec {
            id: self.id.to_owned(),
            client_name: None,
            lane,
            command: CommandSpec {
                program: self
                    .detect
                    .binaries
                    .first()
                    .copied()
                    .unwrap_or(self.id)
                    .to_owned(),
                args: self
                    .spawn_args()
                    .iter()
                    .map(|arg| (*arg).to_owned())
                    .collect(),
                cwd: None,
                env: EnvPolicy::default(),
            },
            max_output_bytes: None,
        }
    }

    /// Process argv prefix before the lane's own arguments (`app-server`
    /// for a `codex app-server` launch). Empty for agents whose argv is the
    /// lane template itself.
    fn spawn_args(&self) -> &'static [&'static str] {
        match &self.surface {
            AgentSurface::Acp { spawn_args, .. }
            | AgentSurface::Cli(CliSurface { spawn_args, .. }) => spawn_args,
            AgentSurface::Rpc(rpc) => rpc.spawn_args,
        }
    }
}

/// The lane family plus its agent-specific surface.
pub enum AgentSurface {
    /// Agent Client Protocol: the protocol supplies session semantics, so
    /// the only agent knowledge is the launch argv and `session/load`
    /// preference.
    Acp {
        prefer_load: bool,
        spawn_args: &'static [&'static str],
        /// Credential source for the `authenticate` retry: when session
        /// establishment is refused with auth-required, the lane sends the
        /// resolved value as `_meta.api_key` alongside the advertised
        /// method id (Devin's contract). `None` keeps the stock
        /// method-id-only attempt.
        auth_key: Option<AcpAuthKey>,
    },
    /// Argv/stdin CLI, one process per turn.
    Cli(CliSurface),
    /// Host-declared JSON-RPC methods over stdio. Cards are `const`, so the
    /// surface rides by reference — `Box` cannot be built in const context.
    Rpc(&'static RpcSurface),
}

/// Const-declared `api_key` source for the acp `authenticate` retry;
/// materializes to an [`EnvSource`] on the spec so secret resolution
/// stays host-side.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AcpAuthKey {
    /// Read this host environment variable at session open.
    HostEnv(&'static str),
    /// Resolve this reference through the host's `SecretResolver`.
    Secret(&'static str),
}

impl AcpAuthKey {
    fn source(self) -> EnvSource {
        match self {
            Self::HostEnv(name) => EnvSource::HostEnv {
                host_env: name.to_owned(),
            },
            Self::Secret(reference) => EnvSource::Secret {
                secret: reference.to_owned(),
            },
        }
    }
}

/// Process-per-turn CLI contract.
pub struct CliSurface {
    /// Fixed argv template; `{prompt}`/`{model}`/`{effort}`/`{mode}`/
    /// `{permission}`/`{instructions}` placeholders substitute per turn.
    pub args: &'static [&'static str],
    pub prompt_channel: PromptChannel,
    pub stream: StreamMode,
    /// argv appended when a session binding exists (`{sessionId}`
    /// substitutes it). Empty: the CLI has no persisted-session resume.
    pub resume_args: &'static [&'static str],
    /// stdin payload template as static JSON text; replaces the bare prompt
    /// line when the prompt channel resolves to stdin. A leaf exactly
    /// `{prompt}` receives the prompt as a JSON string.
    pub stdin_template: Option<&'static str>,
    /// stdout is newline-delimited JSON events.
    pub json_lines: bool,
    /// Dotted path to the session id on a stdout line.
    pub session_id_path: Option<&'static str>,
    /// argv run once at open when no resume binding exists; the first
    /// non-empty stdout line becomes the session binding.
    pub open_args: &'static [&'static str],
    /// Environment variable the lane fills with a per-turn scratch file
    /// path; the agent's installed hook writes the session receipt there
    /// and the lane reads `conversationId`-family keys after the turn.
    pub binding_file_env: Option<&'static str>,
    /// What the receipt may carry beyond the session id.
    pub binding_receipt_shape: BindingReceiptShape,
    /// On a `json_lines` stream, the discriminator value of the record
    /// carrying the turn's authoritative text.
    pub result_selector: Option<&'static str>,
    /// Discriminator field for `result_selector`; defaults to `type`.
    pub result_selector_field: Option<&'static str>,
    /// Dotted path to the final text inside the `result_selector` record.
    pub result_path: Option<&'static str>,
    /// argv prefix before the lane args (for example a subcommand).
    pub spawn_args: &'static [&'static str],
}

/// Declared JSON-RPC method surface over stdio.
pub struct RpcSurface {
    /// Handshake before any session call; `initialized` names the
    /// notification sent once it resolves.
    pub initialize: Option<RpcCallDecl>,
    pub initialized: Option<&'static str>,
    /// Opens the session when the agent is session-oriented.
    pub session_open: Option<RpcCallDecl>,
    /// Opens by resuming an existing binding (`{sessionId}`).
    pub session_resume: Option<RpcCallDecl>,
    /// Per-turn call; its result ends the turn unless `completion` names
    /// a notification that does.
    pub prompt: RpcCallDecl,
    /// Notification sent on explicit user cancellation.
    pub cancel: Option<RpcCallDecl>,
    /// Teardown call, where the agent has one.
    pub session_close: Option<RpcCallDecl>,
    /// Notification method carrying streamed text.
    pub text_event_method: Option<&'static str>,
    /// Dotted path inside that notification to the text payload — a string
    /// leaf or an array of `{type,text}` blocks whose text concatenates.
    pub text_path: Option<&'static str>,
    /// Async turn completion when `prompt` only acknowledges the turn.
    pub completion: Option<CompletionDecl>,
    /// Dotted path to the text inside a prompt result.
    pub result_text_path: Option<&'static str>,
    /// Per-turn configuration calls issued before every `prompt` (for
    /// example model/thinking-level selection on flat-envelope agents). A
    /// call whose params resolve to an empty object is skipped that turn.
    pub pre_turn: &'static [RpcCallDecl],
    /// Whether a resume binding maps verbatim or seeds a fresh
    /// process-scoped wire id.
    pub session_scope: SessionScope,
    /// Explicit cancellation kills the process rather than issuing a
    /// `cancel` call — for agents whose only cancel semantic is teardown.
    pub kill_on_cancel: bool,
    /// Non-stock framing conventions (flat `{type,id,…}` envelopes);
    /// `None` is plain JSON-RPC 2.0.
    pub envelope: Option<&'static EnvelopeDecl>,
    /// Methods a consumer may invoke verbatim through the lane.
    pub passthrough: &'static [&'static str],
    /// argv prefix before the lane args (for example a subcommand).
    pub spawn_args: &'static [&'static str],
}

/// Wire conventions for an rpc lane that is not stock JSON-RPC 2.0 — see
/// [`crate::model::RpcEnvelopeSpec`] for field semantics.
#[derive(Clone, Copy)]
pub struct EnvelopeDecl {
    pub method_field: &'static str,
    pub string_ids: bool,
    pub params_inline: bool,
    pub response_method: Option<&'static str>,
    pub data_field: Option<&'static str>,
    pub result_field: Option<&'static str>,
    pub error_field: Option<&'static str>,
    pub success_field: Option<&'static str>,
    pub command_field: Option<&'static str>,
}

/// One declared call: method plus a static-JSON params template.
#[derive(Clone, Copy)]
pub struct RpcCallDecl {
    pub method: &'static str,
    /// JSON text; string leaves may carry `{prompt}`/`{sessionId}`/
    /// `{turnId}`/`{model}`/`{effort}`/`{instructions}`/`{cwd}`
    /// placeholders, substituted at call time.
    pub params: &'static str,
}

impl RpcCallDecl {
    fn call(self) -> RpcCallSpec {
        RpcCallSpec {
            method: self.method.to_owned(),
            params: serde_json::from_str(self.params).expect("a4aa card rpc params are json"),
        }
    }
}

/// Async turn completion: the prompt call is an acknowledgement and the
/// named notification ends the turn.
#[derive(Clone, Copy)]
pub struct CompletionDecl {
    pub method: &'static str,
    /// Dotted path to the turn id inside the notification params.
    pub turn_id_path: Option<&'static str>,
    /// Dotted path to the terminal status string; becomes the outcome's
    /// `stopReason` verbatim.
    pub status_path: Option<&'static str>,
    /// Only a notification whose `status_path` equals this value ends the
    /// turn.
    pub status_value: Option<&'static str>,
}

/// Machine-discovery facts for hosts that scan for installed agents. Path
/// templates are `{home}`-relative; `oses` limits a path to the listed
/// platforms (`macos`, `linux`, `windows`), and an empty list means all.
pub struct DetectSurface {
    /// Executable names to resolve on `PATH`.
    pub binaries: &'static [&'static str],
    /// Process image names observed while the agent runs.
    pub processes: &'static [&'static str],
    /// Directories hosting a bundled executable outside `PATH`.
    pub binary_dirs: &'static [OsPath],
    /// Configuration files whose presence marks the agent configured.
    pub config: &'static [&'static str],
    /// Marker paths proving prior use.
    pub detection: &'static [&'static str],
    /// Desktop application executables that bundle this agent.
    pub apps: &'static [OsPath],
}

/// A path valid only on the listed platforms; an empty `oses` means all.
pub struct OsPath {
    pub path: &'static str,
    pub oses: &'static [&'static str],
}

/// One session-store or history location.
pub struct HistoryRoot {
    /// `{home}`-relative path template.
    pub path: &'static str,
    /// Stable kind label a host reports (`"claude-project-transcripts"`,
    /// `"codex-session-store"`, …).
    pub kind: &'static str,
    pub store: StoreKind,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum StoreKind {
    JsonlFile,
    JsonlDir,
    SqliteDb,
    MarkdownFile,
    Directory,
    Other,
}

/// Token-accounting locations. Declared facts only — a counter the agent
/// does not report stays absent everywhere downstream.
pub struct UsageSurface {
    /// Live stream: which event carries a usage object and where inside it.
    pub live: Option<LiveUsage>,
    /// Session store: how token counters appear in persisted records.
    pub transcript: Option<TranscriptUsage>,
    /// Vendors whose only token accounting is a hosted API: the canonical
    /// endpoint. The credential and request shape stay the consumer's —
    /// the card records where the authoritative numbers live.
    pub hosted: Option<&'static str>,
}

/// Usage on the wire.
pub struct LiveUsage {
    /// Event discriminator — a notification method on an rpc lane, or the
    /// value a stdout line's event/type field carries on a `jsonLines` cli
    /// lane.
    pub event: &'static str,
    /// Dotted path to the counter object inside the event.
    pub path: &'static str,
}

/// Usage inside a session-store record.
pub struct TranscriptUsage {
    /// `HistoryRoot::kind` holding token records.
    pub root: &'static str,
    /// Record discriminator `(dotted path, equals value)` — only matching
    /// records carry counters. `None`: any record bearing `container` is a
    /// usage record (for stores that mark usage by presence, not type).
    pub record: Option<(&'static str, &'static str)>,
    /// Dotted path to the counter object inside a matching record.
    pub container: &'static str,
    /// Field names read as input/prompt tokens; first present wins.
    pub input: &'static [&'static str],
    /// Field names read as cached input tokens.
    pub cached: &'static [&'static str],
    /// Field names read as output/completion tokens.
    pub output: &'static [&'static str],
    /// Field names read as a reported total; empty where the store has
    /// none — the host sums, the adapter never invents a total.
    pub total: &'static [&'static str],
}

/// Natively supported features — declared facts, not negotiated state.
/// Runtime-negotiated capabilities ride on [`crate::model::Capabilities`].
pub struct FeatureSurface {
    /// How session guidance reaches the agent.
    pub instructions: InstructionChannel,
    /// Accepted execution-mode selector values, distinct from permission
    /// presets (for example `plan` versus `request-review`).
    pub execution_modes: &'static [&'static str],
    /// Accepted permission/mode selector values.
    pub permission_modes: &'static [&'static str],
    /// Mode applied when the host selects none.
    pub permission_default: Option<&'static str>,
    /// Prompt attachments the agent accepts.
    pub attachments: AttachmentSupport,
    /// Reasoning-effort selection is supported.
    pub effort: bool,
    /// Mid-turn steering: follow-up input without ending the turn.
    pub steer: bool,
    /// Model enumeration route; `None` where the agent has no catalog API.
    pub models: ModelCatalog,
}

/// The channel session guidance takes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InstructionChannel {
    /// Guidance composes into prompt text — the default for agents with no
    /// native channel.
    Compose,
    /// A session-open parameter carries it (for example
    /// `developerInstructions`); the surface's params template declares the
    /// `{instructions}` placeholder.
    Param(&'static str),
    /// An argv flag carries it (for example `--append-system-prompt`); the
    /// lane's args template declares the `{instructions}` placeholder.
    Flag(&'static str),
}

/// Prompt attachment forms the agent accepts.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AttachmentSupport {
    None,
    /// Local file references (for example `localImage` input items).
    LocalImages,
}

/// How a host enumerates the agent's model list.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ModelCatalog {
    /// No catalog API — the host's own model knowledge applies.
    None,
    /// A JSON-RPC method lists models (rpc lane passthrough).
    RpcMethod(&'static str),
    /// argv prints the catalog.
    CliArgs(&'static [&'static str]),
}
