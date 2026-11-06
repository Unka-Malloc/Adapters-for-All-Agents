//! `a4aa-copilot` — the `copilot` dialect descriptor for a4aa's mechanism
//! adaptation engine.
//!
//! One agent, one crate: this package owns only the `copilot` mechanism
//! surface (instructions, skills/agents locations, permission and MCP config
//! shapes). It depends on `a4aa` core for the descriptor vocabulary and the
//! shared [`Dialect::adapt`] engine; it depends on no other agent adapter.
//! Install it alone when `copilot` is the only agent a host drives.
//!
//! [`Dialect::adapt`]: a4aa::Dialect::adapt

mod native;

use a4aa::card::{
    AgentCard, AgentSurface, AlternateLane, AttachmentSupport, CliSurface, DetectSurface,
    FeatureSurface, HistoryRoot, InstructionChannel, ModelCatalog, OsPath, StoreKind, UsageSurface,
};
use a4aa::dialect::{
    CommandSurface, McpSurface, PermissionSurface, PluginSurface, SubagentSurface, Target,
};
use a4aa::model::{BindingReceiptShape, PromptChannel, StreamMode};
use a4aa::{AdaptationPlan, AgentSpec, ArtifactScope, Dialect, MechanismSet};

pub use native::{native, NATIVE};

/// The `copilot` dialect: project `AGENTS.md`, `.github/skills` packs and
/// `.github/agents` markdown subagents, plus the home MCP document.
///
/// The hook store at `~/.copilot/hooks/*.json`
/// (`{version:1, hooks:{<Event>:[{type:"command",bash,timeoutSec}]}}`) has no
/// cardable surface yet — its `bash` field shape fits none of the dialect
/// hook variants, so AHE's copilot harness declares it `Unadapted` and
/// [`NATIVE`] reports it as such, not claimed.
pub const DIALECT: Dialect = Dialect {
    id: "copilot",
    hooks: a4aa::hooks::harnesses::COPILOT.surface,
    rules: Some(Target {
        scope: ArtifactScope::Project,
        path: "AGENTS.md",
    }),
    skills_dir: Some(".github/skills"),
    commands: CommandSurface::None,
    subagents: SubagentSurface::MarkdownDir(".github/agents"),
    plugins: PluginSurface::None,
    permissions: PermissionSurface::None,
    mcp: McpSurface::JsonMerge {
        file: ".copilot/mcp-config.json",
        scope: ArtifactScope::UserHome,
        key: "mcpServers",
        adapted: false,
    },
    native_profiles: false,
};

/// Adapt semantic declarations into the `copilot` artifact plan. Equivalent to
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

/// The `copilot` agent card. `copilot --acp --stdio --no-auto-update` is a
/// verified Agent Client Protocol endpoint (CLI 1.0.93): the richest session
/// surface on this card. Authentication is GitHub's own `copilot login`
/// device flow — no key an `authenticate` call can carry, so `auth_key` is
/// `None`.
pub const AGENT: AgentCard = AgentCard {
    id: "copilot",
    label: "GitHub Copilot",
    dialect: &DIALECT,
    surface: AgentSurface::Acp {
        prefer_load: false,
        spawn_args: &["--acp", "--stdio", "--no-auto-update"],
        auth_key: None,
    },
    alternates: &[AlternateLane {
        // `copilot -p` print mode: one prompt non-interactively,
        // `--output-format json` emits one JSON object per line, `-r`
        // resumes a session id the host already knows.
        surface: AgentSurface::Cli(CliSurface {
            args: &[
                "-p",
                "{prompt}",
                "--output-format",
                "json",
                "--model",
                "{model}",
            ],
            trusted_workspace_args: &[],
            prompt_channel: PromptChannel::ArgOnly,
            stream: StreamMode::Stdio,
            resume_spawn_args: &[],
            resume_args: &["-r", "{sessionId}"],
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
            spawn_args: &[],
            effort_model_suffixes: &[],
        }),
        binary: None,
    }],
    detect: DetectSurface {
        binaries: &["copilot"],
        processes: &["copilot"],
        binary_dirs: &[OsPath {
            path: "{home}/.copilot/bin",
            oses: &[],
        }],
        config: &["{home}/.copilot/config.json"],
        detection: &["{home}/.copilot"],
        apps: &[],
    },
    features: FeatureSurface {
        instructions: InstructionChannel::Compose,
        execution_modes: &[],
        // Permissions are flags and tool lists (--allow-all-tools,
        // --deny-tool, --allow-all), not a mode-valued selector.
        permission_modes: &[],
        permission_default: None,
        seatbelted_permission: None,
        attachments: AttachmentSupport::LocalImages,
        effort: true,
        steer: false,
        models: ModelCatalog::None,
    },
    usage: UsageSurface {
        live: None,
        transcript: None,
        hosted: None,
    },
    history: &[HistoryRoot {
        path: "{home}/.copilot",
        kind: "copilot-session-store",
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
        assert_eq!(plan.dialect, "copilot");
        assert!(plan.artifacts.is_empty());
    }

    #[test]
    fn spec_materializes_the_acp_lane() {
        let spec = spec();
        assert_eq!(spec.command.program, "copilot");
        assert_eq!(
            spec.command.args,
            vec!["--acp", "--stdio", "--no-auto-update"]
        );
        assert!(matches!(spec.lane, a4aa::LaneSpec::Acp { .. }));
    }
}
