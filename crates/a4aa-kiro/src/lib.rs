//! `a4aa-kiro` — the `kiro` dialect descriptor for a4aa's mechanism
//! adaptation engine.
//!
//! One agent, one crate: this package owns only the `kiro` mechanism
//! surface (hook store, skills/commands/subagents locations, permission and
//! MCP config shapes). It depends on `a4aa` core for the descriptor
//! vocabulary and the shared [`Dialect::adapt`] engine; it depends on no
//! other agent adapter. Install it alone when `kiro` is the only agent a
//! host drives.
//!
//! [`Dialect::adapt`]: a4aa::Dialect::adapt

use a4aa::card::{
    AgentCard, AgentSurface, AlternateLane, AttachmentSupport, CliSurface, DetectSurface,
    FeatureSurface, HistoryRoot, InstructionChannel, LiveUsage, ModelCatalog, OsPath, StoreKind,
    UsageSurface,
};
use a4aa::dialect::{
    CommandSurface, HookSurface, McpSurface, PermissionSurface, PluginSurface, SubagentSurface,
    Target,
};
use a4aa::model::{BindingReceiptShape, PromptChannel, StreamMode};
use a4aa::{AdaptationPlan, AgentSpec, ArtifactScope, Dialect, MechanismSet};

/// The `kiro` dialect: Kiro steering rules, skills, JSON agents, mcp.json (IDE trigger hooks unmapped).
pub const DIALECT: Dialect = Dialect {
    id: "kiro",
    hooks: HookSurface::None,
    rules: Some(Target {
        scope: ArtifactScope::Project,
        path: ".kiro/steering/a4aa-rules.md",
    }),
    skills_dir: Some(".kiro/skills"),
    commands: CommandSurface::None,
    subagents: SubagentSurface::KiroJson(".kiro/agents"),
    plugins: PluginSurface::None,
    permissions: PermissionSurface::None,
    mcp: McpSurface::JsonMerge {
        file: ".kiro/settings/mcp.json",
        scope: ArtifactScope::UserHome,
        key: "mcpServers",
        adapted: true,
    },
    native_profiles: false,
};

/// Adapt semantic declarations into the `kiro` artifact plan. Equivalent to
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

/// The `kiro-cli` agent card. `kiro-cli acp` delegates to the bundled
/// `kiro-cli-chat` ACP surface (session/new|prompt|load|cancel across the
/// v2 agent engine). Credential state is a spawn-time gate: a logged-out
/// install refuses before `initialize` with "You are not logged in", so
/// auth absence surfaces as process death, not an ACP `authMethods`
/// negotiation — `auth_key` stays `None` and the host detects the refusal
/// signature on stderr.
pub const AGENT: AgentCard = AgentCard {
    id: "kiro",
    label: "Kiro CLI",
    dialect: &DIALECT,
    surface: AgentSurface::Acp {
        prefer_load: true,
        spawn_args: &["acp"],
        auth_key: None,
    },
    alternates: &[AlternateLane {
        // `kiro-cli chat` headless turn: `--output-format stream-json`
        // implies `--no-interactive` and emits JSON Lines; `--resume-id`
        // binds a host-known session. Auth absence fails at spawn (the
        // same gate the ACP surface documents).
        surface: AgentSurface::Cli(CliSurface {
            args: &[
                "--no-interactive",
                "--output-format",
                "stream-json",
                "--model",
                "{model}",
                "--effort",
                "{effort}",
                "{prompt}",
            ],
            prompt_channel: PromptChannel::ArgOnly,
            stream: StreamMode::Stdio,
            resume_spawn_args: &[],
            resume_args: &["--resume-id", "{sessionId}"],
            stdin_template: None,
            json_lines: true,
            session_id_path: None,
            open_args: &[],
            binding_file_env: None,
            binding_receipt_shape: BindingReceiptShape::SessionId,
            result_selector: None,
            result_selector_field: None,
            result_path: None,
            usage_path: None,
            spawn_args: &["chat"],
        }),
        // `chat`/`acp` live on kiro-cli-chat behind the kiro-cli shim.
        binary: Some("kiro-cli-chat"),
    }],
    detect: DetectSurface {
        binaries: &["kiro-cli", "kiro-cli-chat"],
        processes: &["kiro-cli", "kiro-cli-chat"],
        binary_dirs: &[OsPath {
            path: "/Applications/Kiro CLI.app/Contents/MacOS",
            oses: &["macos"],
        }],
        config: &[],
        detection: &["{home}/.kiro"],
        apps: &[OsPath {
            path: "/Applications/Kiro CLI.app/Contents/MacOS/kiro-cli",
            oses: &["macos"],
        }],
    },
    features: FeatureSurface {
        instructions: InstructionChannel::Compose,
        // `--mode default|spec` on the agent engine.
        execution_modes: &["spec"],
        permission_modes: &[],
        permission_default: None,
        attachments: AttachmentSupport::None,
        // `--effort low|medium|high|xhigh|max` (spawn-time on `acp`).
        effort: true,
        steer: false,
        models: ModelCatalog::CliArgs(&["chat", "--list-models"]),
    },
    usage: UsageSurface {
        live: Some(LiveUsage {
            event: "session/update",
            path: "usage",
        }),
        transcript: None,
        hosted: None,
    },
    history: &[HistoryRoot {
        path: "{home}/.kiro",
        kind: "kiro-session-store",
        store: StoreKind::Directory,
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
        assert_eq!(plan.dialect, "kiro");
        assert!(plan.artifacts.is_empty());
    }
}
