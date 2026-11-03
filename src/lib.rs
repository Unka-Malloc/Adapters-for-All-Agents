//! # a4aa — Adapter For All Agents
//!
//! One crate owns every agent-client adaptation path a host needs:
//!
//! - **supervision** — process-group spawn, full user environment inheritance
//!   with explicit overrides, bounded I/O, interrupt (SIGINT→grace→kill) and
//!   tree teardown;
//! - **discovery** — [`discovery`]: the allowlisted machine scan engine —
//!   `{token}`-rooted location templates, a host-supplied or card-projected
//!   [`discovery::ScanManifest`], deny/admission rules and bounded probes;
//! - **hostenv** — [`hostenv`]: the user's login-shell environment a
//!   fidelity launch resolves from (the GUI session's environment is not
//!   the user's);
//! - **transports** — piped stdio, Unix PTY (raw slave, streaming master),
//!   WebSocket and batch-only SSH remote spawn;
//! - **lanes** — [`proto::acp`] (official `agent-client-protocol` crate),
//!   [`proto::rpc`] (host-declared JSON-RPC methods), [`proto::cli`]
//!   (argv/stdin prompt, streamed output);
//! - **runtime contracts** — [`runtime`]: the host-neutral port vocabulary a
//!   composing host answers — provider identity, capability-evidence
//!   reducers, opaque session bindings, dispatch receipts and the
//!   caller-registration and target-runtime traits — plus
//!   [`protocol_selector`], the evidence-reduction engine that pins one
//!   attempt to one protocol immutably;
//! - **facade** — [`Adapter`] produces a lane-erased [`AgentSession`];
//! - **sidecar** — the `a4aa serve` binary speaks `a4aa.v1` NDJSON-RPC on its
//!   own stdio, so hosts in any language reuse the identical adapters by
//!   spawning this crate.
//!
//! Design invariants:
//! - **Zero-assumption output.** No lane forces a schema onto agent output;
//!   natural language is a valid turn result.
//! - **Fail closed.** Unknown inbound requests are rejected, unresolved
//!   secrets and unset permission handlers never grant.
//! - **Explicit cancellation only.** Cancel signals reach the agent solely
//!   from a caller's `cancel()`; a dropped observer or transport error is
//!   never rewritten as user cancellation.
//! - **Bounded resources.** Output caps drain-and-discard; a chatty agent
//!   can never exhaust host memory or stall its own pipes.

pub mod adapter;
pub mod bounds;
pub mod card;
pub mod dialect;
pub mod discovery;
pub mod env;
pub mod error;
pub mod framing;
pub mod hostenv;
pub mod mcp;
pub mod mechanisms;
pub mod model;
pub mod native;
pub mod observe;
pub mod proto;
pub mod protocol_selector;
pub mod rpc;
pub mod runtime;
pub mod supervision;
mod template;
pub mod transport;

pub use acop as context;
pub use adapter::{Adapter, AgentSession, BeforeTurn, LaneSession, PermissionHandler, TurnRequest};
pub use ahe::install as hooks;
pub use card::{
    AgentCard, AgentSurface, AlternateLane, AttachmentSupport, CliSurface, CompletionDecl,
    DetectSurface, FeatureSurface, HistoryRoot, InstructionChannel, LiveUsage, ModelCatalog,
    OsPath, RpcCallDecl, RpcMcpDecl, RpcSurface, StoreKind, TranscriptUsage, UsageSurface,
    UsageTemporality,
};
pub use dialect::{
    adapt, CommandSurface, Dialect, ExtensionKind, HookSurface, McpSurface, PermissionSurface,
    PluginSurface, SubagentSurface, Target,
};
pub use discovery::{DenySpec, HistoryScanRoot, HostRoots, ScanManifest};
pub use env::{EnvPolicy, EnvSource, EnvValue, NoSecrets, SecretResolver};
pub use error::{AdapterError, Result};
pub use hostenv::ShellEnvironmentStatus;
pub use mechanisms::{
    AdaptationPlan, Artifact, ArtifactScope, CommandDecl, HookDecl, HookEvent, HookIntent,
    MechanismSet, PermissionDecl, PluginDecl, RuleDecl, SkillDecl, SubagentDecl, Support,
    SupportEntry, WriteMode,
};
pub use model::{
    AgentEvent, AgentInfo, AgentSpec, BindingReceiptShape, Capabilities, CliLaneSpec, CommandSpec,
    LaneKind, LaneSpec, PromptChannel, RpcCallSpec, RpcCompletionSpec, RpcEnvelopeSpec,
    RpcLaneSpec, RpcMcpConfig, RpcMcpToolApproval, RpcTransport, SelectorPhase, SelectorSupport,
    SelectorSupportSet, SessionConfig, SessionRequest, SessionScope, StreamMode, TokenUsage,
    TurnOutcome,
};
pub use native::{
    cell, parse_iso_date, AwpPrimitives, Cell, ConfigLocations, InteractionSurfaces, McpMechanism,
    McpRegistration, NativeCapability, NativeStatus,
};
pub use observe::{Direction, Observer, SharedObserver};
pub use proto::acp::{PermissionDecision, PermissionPrompt, SessionDelegate};
pub use proto::rpc::RpcSessionDelegate;
pub use rpc::{CallFailure, JsonRpcPeer, RpcDelegate, RpcErrorObject};
pub use runtime::{
    reduce_execution_admission, reduce_readiness, AdapterFailure, CallerRegistrationPlan,
    CallerRegistrationReceipt, DurableNativeBinding, ExecutionAdmissionDecision,
    ExecutionAdmissionEvidence, InstructionPolicy, McpCallerIntegration, NativeResumeIdentity,
    PersistentTurnRuntime, ProviderId, ReadinessDecision, ReadinessEvidence, RegistrationApproval,
    RuntimeDispatchReceipt, RuntimeDriver, RuntimeDriverRegistry, RuntimeEventPart,
    RuntimeObservation, RuntimeTransition, SubagentCapabilities, SubagentContinueRequest,
    SubagentDispatchRequest, SubagentRuntimeAdapter,
};
pub use supervision::{IoPlan, StdioKind, SupervisedChild};
pub use transport::ssh::{SshRemote, SshRemoteError};

#[cfg(test)]
#[path = "../tests/support/settings.rs"]
mod test_settings;
