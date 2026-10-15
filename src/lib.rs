//! # a4aa — Adapter For All Agents
//!
//! One crate owns every agent-client adaptation path a host needs:
//!
//! - **supervision** — process-group spawn, sanitized environment, bounded
//!   I/O, interrupt (SIGINT→grace→kill) and tree teardown;
//! - **transports** — piped stdio and Unix PTY (raw slave, streaming master);
//! - **lanes** — [`proto::acp`] (official `agent-client-protocol` crate),
//!   [`proto::rpc`] (host-declared JSON-RPC methods), [`proto::cli`]
//!   (argv/stdin prompt, streamed output);
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
pub mod env;
pub mod error;
pub mod framing;
pub mod mechanisms;
pub mod model;
pub mod observe;
pub mod proto;
pub mod rpc;
pub mod supervision;
mod template;
pub mod transport;

pub use adapter::{Adapter, AgentSession, BeforeTurn, LaneSession, PermissionHandler, TurnRequest};
pub use card::{
    AgentCard, AgentSurface, AttachmentSupport, CliSurface, CompletionDecl, DetectSurface,
    FeatureSurface, HistoryRoot, InstructionChannel, LiveUsage, ModelCatalog, OsPath, RpcCallDecl,
    RpcSurface, StoreKind, TranscriptUsage, UsageSurface,
};
pub use dialect::{
    adapt, CommandSurface, Dialect, ExtensionKind, HookSurface, McpSurface, PermissionSurface,
    PluginSurface, SubagentSurface, Target,
};
pub use env::{EnvPolicy, EnvSource, EnvValue, NoSecrets, SecretResolver};
pub use error::{AdapterError, Result};
pub use mechanisms::{
    AdaptationPlan, Artifact, ArtifactScope, CommandDecl, HookDecl, HookEvent, HookIntent,
    MechanismSet, PermissionDecl, PluginDecl, RuleDecl, SkillDecl, SubagentDecl, Support,
    SupportEntry, WriteMode,
};
pub use model::{
    AgentEvent, AgentInfo, AgentSpec, BindingReceiptShape, Capabilities, CliLaneSpec, CommandSpec,
    LaneKind, LaneSpec, PromptChannel, RpcCallSpec, RpcCompletionSpec, RpcEnvelopeSpec,
    RpcLaneSpec, SelectorPhase, SelectorSupport, SelectorSupportSet, SessionConfig, SessionRequest,
    SessionScope, StreamMode, TokenUsage, TurnOutcome,
};
pub use observe::{Direction, Observer, SharedObserver};
pub use proto::acp::{PermissionDecision, PermissionPrompt, SessionDelegate};
pub use proto::rpc::RpcSessionDelegate;
pub use rpc::{CallFailure, JsonRpcPeer, RpcDelegate, RpcErrorObject};
pub use supervision::{IoPlan, StdioKind, SupervisedChild};

#[cfg(test)]
#[path = "../tests/support/settings.rs"]
mod test_settings;
