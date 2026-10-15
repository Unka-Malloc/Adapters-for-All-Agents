//! `a4aa-dsh` — the `dsh` dialect descriptor for a4aa's mechanism
//! adaptation engine.
//!
//! One agent, one crate: this package owns only the `dsh` mechanism
//! surface (hook store, skills/commands/subagents locations, permission and
//! MCP config shapes). It depends on `a4aa` core for the descriptor
//! vocabulary and the shared [`Dialect::adapt`] engine; it depends on no
//! other agent adapter. Install it alone when `dsh` is the only agent a
//! host drives.
//!
//! [`Dialect::adapt`]: a4aa::Dialect::adapt

use a4aa::card::{
    AgentCard, AgentSurface, AttachmentSupport, CompletionDecl, DetectSurface, FeatureSurface,
    HistoryRoot, InstructionChannel, ModelCatalog, RpcCallDecl, RpcSurface, StoreKind,
    TranscriptUsage, UsageSurface,
};
use a4aa::dialect::{
    CommandSurface, HookSurface, McpSurface, PermissionSurface, PluginSurface, SubagentSurface,
};
use a4aa::{AdaptationPlan, AgentSpec, Dialect, MechanismSet, SessionScope};

/// The `dsh` dialect: DeepSeek Harness layered native profiles (cordis), skills.
pub const DIALECT: Dialect = Dialect {
    id: "dsh",
    hooks: HookSurface::None,
    rules: None,
    skills_dir: Some(".dsh/skills"),
    commands: CommandSurface::None,
    subagents: SubagentSurface::None,
    plugins: PluginSurface::None,
    permissions: PermissionSurface::None,
    mcp: McpSurface::None,
    native_profiles: true,
};

/// Adapt semantic declarations into the `dsh` artifact plan. Equivalent to
/// `a4aa::adapt(&DIALECT, set)` — this free function is the entire public
/// surface most hosts need.
pub fn adapt(set: &MechanismSet) -> AdaptationPlan {
    DIALECT.adapt(set)
}

/// The descriptor itself, for hosts that compose over `&'static Dialect`
/// (registries, `a4aa-dialects`-style lookup tables).
pub const fn dialect() -> &'static Dialect {
    &DIALECT
}

/// The `dsh` agent card: `dsh --profile sdk` JSON-RPC over stdio.
///
/// Protocol: the SDK profile runs a
/// stock JSON-RPC handshake (`initialize` carries cwd/provider/model/
/// reasoning effort; the server answers `deepseek-harness-sdk-runtime`).
/// Sessions are process-scoped — the durable store rejects reusing an id
/// across process lifetimes — so the wire id is `{binding}-<nonce>`; a
/// turn is `session/prompt {sessionId, contentBlocks:[text]}` acknowledged
/// by a `messageId`, streams `session.event` frames, and ends at
/// `session.status` reporting `idle`. Cancellation is process teardown;
/// `shutdown` is the teardown call.
pub const AGENT: AgentCard = AgentCard {
    id: "dsh",
    label: "DeepSeek Harness",
    dialect: &DIALECT,
    surface: AgentSurface::Rpc(&RpcSurface {
        initialize: Some(RpcCallDecl {
            method: "initialize",
            // The SDK requires cwd + provider + model at handshake.
            params: r#"{
                "cwd": "{cwd}",
                "provider": "{provider}",
                "model": "{model}",
                "reasoningEffort": "{effort}"
            }"#,
        }),
        initialized: None,
        // No session call: the wire id is minted per process from the
        // host's durable binding.
        session_open: None,
        session_resume: None,
        prompt: RpcCallDecl {
            method: "session/prompt",
            params: r#"{
                "sessionId": "{sessionId}",
                "contentBlocks": [{"type": "text", "text": "{prompt}"}]
            }"#,
        },
        cancel: None,
        session_close: Some(RpcCallDecl {
            method: "shutdown",
            params: "{}",
        }),
        text_event_method: Some("session.event"),
        text_path: Some("event.data.message.content"),
        completion: Some(CompletionDecl {
            method: "session.status",
            turn_id_path: None,
            status_path: Some("status"),
            status_value: Some("idle"),
        }),
        result_text_path: None,
        pre_turn: &[],
        session_scope: SessionScope::Process,
        kill_on_cancel: true,
        envelope: None,
        passthrough: &[],
        spawn_args: &["--profile", "sdk"],
    }),
    detect: DetectSurface {
        binaries: &["dsh"],
        processes: &["dsh.exe", "dsh"],
        binary_dirs: &[],
        config: &[],
        detection: &["{home}/.dsh/sessions"],
        apps: &[],
    },
    features: FeatureSurface {
        instructions: InstructionChannel::Compose,
        execution_modes: &[],
        permission_modes: &[],
        permission_default: None,
        attachments: AttachmentSupport::None,
        effort: true,
        steer: false,
        models: ModelCatalog::None,
    },
    usage: UsageSurface {
        live: None,
        // `~/.dsh/sessions` message samples carry disjoint counter
        // buckets; reasoning rides inside `outputTokens` and the prompt
        // side is `total - output` — the host derives, the card names.
        transcript: Some(TranscriptUsage {
            root: "deepseek-harness-session-store",
            record: None,
            container: "usage",
            input: &["inputTokens"],
            cached: &["cacheReadTokens"],
            output: &["outputTokens"],
            total: &["totalTokens"],
        }),
        hosted: None,
    },
    history: &[HistoryRoot {
        path: "{home}/.dsh/sessions",
        kind: "deepseek-harness-session-store",
        store: StoreKind::JsonlDir,
    }],
};

/// The full card, for hosts that compose over `&'static AgentCard`.
pub const fn card() -> &'static AgentCard {
    &AGENT
}

/// The materialized runtime spec — `Adapter::new(spec())` is the entire
/// launch path for hosts that keep no spec table of their own.
pub fn spec() -> AgentSpec {
    AGENT.spec()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dialect_reports_identity() {
        let set = MechanismSet::default();
        let plan = adapt(&set);
        assert_eq!(plan.dialect, "dsh");
        assert!(plan.artifacts.is_empty());
    }
}

#[cfg(test)]
mod card_tests {
    use super::*;

    #[test]
    fn card_matches_dialect_identity() {
        assert_eq!(AGENT.id, DIALECT.id);
        assert!(std::ptr::eq(AGENT.dialect, &DIALECT));
    }

    #[test]
    fn spec_materializes_the_sdk_profile_lane() {
        let spec = spec();
        assert_eq!(spec.command.program, "dsh");
        assert_eq!(
            spec.command.args,
            vec!["--profile".to_string(), "sdk".to_string()]
        );
        let a4aa::LaneSpec::Rpc(rpc) = spec.lane else {
            panic!("dsh drives the rpc lane")
        };
        assert_eq!(rpc.initialize.as_ref().unwrap().method, "initialize");
        assert_eq!(rpc.prompt.method, "session/prompt");
        assert_eq!(rpc.session_scope, SessionScope::Process);
        assert!(rpc.kill_on_cancel);
        assert_eq!(rpc.completion.as_ref().unwrap().method, "session.status");
        assert_eq!(
            rpc.completion.as_ref().unwrap().status_value.as_deref(),
            Some("idle")
        );
        assert_eq!(rpc.text_path.as_deref(), Some("event.data.message.content"));
        assert!(rpc.capabilities().session_resume);
        assert!(rpc.capabilities().cancellation);
    }

    #[test]
    fn transcript_usage_names_the_session_store_counters() {
        let usage = AGENT.usage.transcript.as_ref().unwrap();
        assert_eq!(usage.root, "deepseek-harness-session-store");
        assert_eq!(usage.input, &["inputTokens"]);
        assert_eq!(usage.cached, &["cacheReadTokens"]);
        assert_eq!(usage.output, &["outputTokens"]);
        assert_eq!(usage.total, &["totalTokens"]);
    }
}
