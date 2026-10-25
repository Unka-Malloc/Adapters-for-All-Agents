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

mod command_code;
mod native;

use a4aa::card::{
    AgentCard, AgentSurface, AlternateLane, AttachmentSupport, CompletionDecl, DetectSurface,
    FeatureSurface, HistoryRoot, InstructionChannel, LiveUsage, ModelCatalog, RpcCallDecl,
    RpcSurface, RpcTransportDecl, StoreKind, TranscriptUsage, UsageSurface,
};
use a4aa::dialect::{
    CommandSurface, HookSurface, McpSurface, PermissionSurface, PluginSurface, SubagentSurface,
};
use a4aa::{AdaptationPlan, AgentSpec, Dialect, MechanismSet, SessionScope};

pub use command_code::{
    command_code_patch, command_code_patch_endpoint, credential_ref_names, has_command_code_key,
    mount_command_code, providers_from_config_options, AdvertisedModel, AdvertisedProvider,
    CommandCodeModel, CommandCodeMountError, MountPolicy, MountedCommandCode, ACP_PROFILE_PATCH,
    COMMAND_CODE_API_KEY_ENV, COMMAND_CODE_BASE_URL, COMMAND_CODE_PROVIDER, DSH_CREDENTIALS,
    DSH_PROFILE_PATCH, PI_AI_PATCH_ID, PI_AI_PLUGIN,
};

pub use native::{native, NATIVE};

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

/// Official SDK JSON-RPC stdio (`dsh --profile sdk` / `dsh sdk`).
///
/// `initialize` carries cwd/provider/model and optional `reasoningEffort`.
/// Turns are `session/prompt` with a caller-created `{sessionId}` — the
/// SDK records session ids durably and refuses re-admission after a
/// respawn, so the lane mints a process-scoped id. Completion is
/// `session.status` `idle`; assistant text arrives on `session.event`
/// `assistant/message` (no live deltas). There is no cancel method.
static SDK_RPC: RpcSurface = RpcSurface {
    initialize: Some(RpcCallDecl {
        method: "initialize",
        params: r#"{
            "cwd": "{cwd}",
            "provider": "{modelProvider}",
            "model": "{modelId}",
            "reasoningEffort": "{effort}"
        }"#,
    }),
    initialized: None,
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
    transport: RpcTransportDecl::Stdio,
    max_frame_bytes: None,
    usage_path: None,
    failure_path: None,
    failure_value: None,
    failure_message_path: None,
    mcp: None,
};

/// The `dsh` agent card: capability ordering ranks Serve, then SDK, then
/// ACP or RPC, then CLI, so the auto-lane policy materializes the official
/// SDK JSON-RPC stdio profile (`dsh --profile sdk`) as primary. The ACP
/// profile (`dsh acp`) is the ordered alternate: it carries the richer
/// session lifecycle — durable `session/resume`, `session/cancel`, live
/// `session/update` streaming and the `session/new` catalog — but that is
/// an alternate capability, never a reason to select a lower-ranked lane.
/// Both profiles are first-party DSH (`~/.dsh/profiles/acp` and
/// `~/.dsh/profiles/sdk`); neither is an external plugin.
///
/// SDK protocol: `dsh --profile sdk` is the official library-style stdio
/// carrier a host drives in-process: model and reasoning effort on
/// `initialize`, caller-created session ids on `session/prompt`, no live
/// deltas and no cancel.
///
/// ACP alternate: `dsh <name>` abbreviates `dsh --profile <name>`, so
/// `dsh acp` boots the shipped ACP profile — a headless, non-interactive
/// stdio server that holds stdout for NDJSON JSON-RPC until disconnect. No
/// extra flags: `--help` is the profile app's own help and would exit
/// without serving. `initialize` advertises ACP v1 plus `session/list`,
/// `session/resume`, `session/close`; `session/load` is unsupported, so
/// resume uses `session/resume`. `session/new` returns the live catalog as
/// grouped `configOptions` (provider/model choices plus
/// `reasoning_effort` when the exact model declares one). Authentication is
/// a no-op; DSH resolves credentials itself.
///
/// Command Code is a hand-declared `@deepseek-ai/dsh-llm-pi-ai` route, not a
/// shipped catalog entry. Hosts provision it once with `a4aa-dsh mount`
/// ([`mount_command_code`]): the command reads only the
/// `COMMAND_CODE_API_KEY` reference name from `$DSH_HOME/.credentials.yaml`,
/// installs the route patch, and the next ACP `session/new` advertises the
/// `command-code` provider alongside the shipped ones.
pub const AGENT: AgentCard = AgentCard {
    id: "dsh",
    label: "DeepSeek Harness",
    dialect: &DIALECT,
    surface: AgentSurface::Rpc(&SDK_RPC),
    alternates: &[AlternateLane {
        surface: AgentSurface::Acp {
            prefer_load: false,
            spawn_args: &["acp"],
            auth_key: None,
        },
        binary: None,
    }],
    detect: DetectSurface {
        binaries: &["dsh"],
        processes: &["dsh.exe", "dsh"],
        binary_dirs: &[],
        config: &[
            "{home}/.dsh/.credentials.yaml",
            "{home}/.dsh/profiles/acp/cordis.patch.yml",
        ],
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
        // DSH ACP advertises the live LLM catalog as `configOptions` on
        // `session/new` — grouped by registered provider, including a
        // Command Code route mounted through `@deepseek-ai/dsh-llm-pi-ai`.
        models: ModelCatalog::RpcMethod("session/new"),
    },
    usage: UsageSurface {
        live: Some(LiveUsage {
            event: "session/update",
            path: "usage",
        }),
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
    fn spec_materializes_the_sdk_rpc_lane() {
        let spec = spec();
        assert_eq!(spec.command.program, "dsh");
        // Capability order puts SDK above ACP, so the auto-lane primary is
        // the official SDK stdio profile.
        assert_eq!(
            spec.command.args,
            vec!["--profile".to_owned(), "sdk".to_owned()]
        );
        let a4aa::LaneSpec::Rpc(rpc) = &spec.lane else {
            panic!("dsh's primary is the sdk rpc lane")
        };
        assert_eq!(rpc.initialize.as_ref().unwrap().method, "initialize");
        assert_eq!(rpc.prompt.method, "session/prompt");
        assert_eq!(rpc.completion.as_ref().unwrap().method, "session.status");
        assert_eq!(rpc.session_scope, a4aa::SessionScope::Process);
        assert!(rpc.kill_on_cancel);
        assert!(rpc.cancel.is_none());
        // The ACP alternate still names the catalog and streaming surface;
        // the SDK carrier receives model/effort on initialize.
        assert_eq!(
            AGENT.features.models,
            a4aa::ModelCatalog::RpcMethod("session/new")
        );
        assert_eq!(
            AGENT.usage.live.as_ref().map(|usage| usage.event),
            Some("session/update")
        );
    }

    #[test]
    fn native_table_records_both_first_party_surfaces() {
        assert_eq!(NATIVE.last_verified, "2026-10-24");
        assert_eq!(NATIVE.surfaces.acp.status, a4aa::NativeStatus::Native);
        assert_eq!(NATIVE.surfaces.sdk.status, a4aa::NativeStatus::Native);
        assert_eq!(NATIVE.surfaces.rpc.status, a4aa::NativeStatus::Native);
        assert_eq!(NATIVE.surfaces.serve.status, a4aa::NativeStatus::NotOffered);
    }

    #[test]
    fn spec_materializes_the_acp_alternate() {
        let specs = AGENT.specs();
        assert_eq!(specs.len(), 2);
        // `dsh acp` expands to `dsh --profile acp`: headless ACP stdio,
        // no extra flags.
        assert_eq!(specs[1].command.program, "dsh");
        assert_eq!(specs[1].command.args, vec!["acp".to_string()]);
        let a4aa::LaneSpec::Acp {
            prefer_load,
            auth_key,
            ..
        } = &specs[1].lane
        else {
            panic!("dsh's alternate is the acp lane")
        };
        assert!(!prefer_load);
        assert!(auth_key.is_none());
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
