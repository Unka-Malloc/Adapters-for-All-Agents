//! `a4aa-open-claw` — the `openclaw` dialect descriptor for a4aa's mechanism
//! adaptation engine.
//!
//! One agent, one crate: this package owns only the `openclaw` mechanism
//! surface (hook store, skills/commands/subagents locations, permission and
//! MCP config shapes). It depends on `a4aa` core for the descriptor
//! vocabulary and the shared [`Dialect::adapt`] engine; it depends on no
//! other agent adapter. Install it alone when `openclaw` is the only agent a
//! host drives.
//!
//! [`Dialect::adapt`]: a4aa::Dialect::adapt

mod native;

use a4aa::card::{
    AgentCard, AgentSurface, AlternateLane, AttachmentSupport, CliSurface, DetectSurface,
    FeatureSurface, HistoryRoot, InstructionChannel, ModelCatalog, StoreKind, UsageSurface,
};
use a4aa::dialect::{
    CommandSurface, HookSurface, McpSurface, PermissionSurface, PluginSurface, SubagentSurface,
    Target,
};
use a4aa::model::{BindingReceiptShape, PromptChannel, StreamMode};
use a4aa::{AdaptationPlan, AgentSpec, ArtifactScope, Dialect, MechanismSet};

pub use native::{native, NATIVE};

/// The `openclaw` dialect: OpenClaw AGENTS.md, skills, agents.
pub const DIALECT: Dialect = Dialect {
    id: "openclaw",
    hooks: HookSurface::None,
    rules: Some(Target {
        scope: ArtifactScope::Project,
        path: "AGENTS.md",
    }),
    skills_dir: Some(".openclaw/skills"),
    commands: CommandSurface::None,
    subagents: SubagentSurface::MarkdownDir(".openclaw/agents"),
    plugins: PluginSurface::None,
    permissions: PermissionSurface::None,
    mcp: McpSurface::None,
    native_profiles: false,
};

/// Adapt semantic declarations into the `openclaw` artifact plan. Equivalent to
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

/// The `openclaw` agent card. OpenClaw is gateway-centric: `openclaw acp`
/// bridges ACP over the running Gateway's WebSocket, so it only serves
/// when a Gateway is already up. The self-contained surface is
/// `openclaw agent exec` — one isolated turn per process, answering a
/// single `--json` envelope `{ok,status,final,usage,sessionId,…}` with no
/// Gateway requirement; that is the lane the card declares. Gateway-owned
/// session keys are a remote-session concept, not a resume binding the
/// exec surface honors.
pub const AGENT: AgentCard = AgentCard {
    id: "openclaw",
    label: "OpenClaw",
    dialect: &DIALECT,
    surface: AgentSurface::Cli(CliSurface {
        spawn_args: &["agent", "exec"],
        // `--message-file -` reads the prompt from stdin; `--json` returns
        // the stable single-object result envelope.
        args: &[
            "--message-file",
            "-",
            "--json",
            "--model",
            "{model}",
            "--thinking",
            "{effort}",
            "--cwd",
            "{cwd}",
        ],
        prompt_channel: PromptChannel::Stdin,
        stream: StreamMode::Stdio,
        // `agent exec` turns are ephemeral; durable sessions live behind
        // the Gateway's session keys.
        resume_spawn_args: &[],
        resume_args: &[],
        stdin_template: None,
        json_lines: true,
        session_id_path: Some("sessionId"),
        open_args: &[],
        binding_file_env: None,
        binding_receipt_shape: BindingReceiptShape::SessionId,
        result_selector: Some("ok"),
        result_selector_field: Some("status"),
        result_path: Some("final"),
        usage_path: None,

        effort_model_suffixes: &[],
    }),
    alternates: &[AlternateLane {
        // `openclaw acp` runs the agent's ACP bridge in front of the
        // Gateway: the canonical session protocol surface when the host
        // prefers protocol lanes over per-turn exec processes.
        surface: AgentSurface::Acp {
            prefer_load: false,
            spawn_args: &["acp"],
            auth_key: None,
        },
        binary: None,
    }],
    detect: DetectSurface {
        binaries: &["openclaw"],
        processes: &["openclaw"],
        binary_dirs: &[],
        config: &[],
        detection: &["{home}/.openclaw"],
        apps: &[],
    },
    features: FeatureSurface {
        instructions: InstructionChannel::Compose,
        execution_modes: &[],
        permission_modes: &[],
        permission_default: None,
        attachments: AttachmentSupport::None,
        // `--thinking off|…|ultra`.
        effort: true,
        steer: false,
        models: ModelCatalog::CliArgs(&["models"]),
    },
    usage: UsageSurface {
        live: Some(a4aa::card::LiveUsage {
            event: "",
            path: "usage",
        }),
        transcript: None,
        hosted: None,
    },
    history: &[HistoryRoot {
        path: "{home}/.openclaw",
        kind: "openclaw-store",
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
        assert_eq!(plan.dialect, "openclaw");
        assert!(plan.artifacts.is_empty());
    }

    #[test]
    fn spec_materializes_the_cli_lane() {
        let spec = spec();
        assert_eq!(spec.command.program, "openclaw");
        assert_eq!(spec.command.args, vec!["agent", "exec"]);
        let a4aa::LaneSpec::Cli(cli) = spec.lane else {
            panic!("openclaw drives the cli lane")
        };
        assert!(cli.json_lines);
        assert_eq!(cli.session_id_path.as_deref(), Some("sessionId"));
    }
}
