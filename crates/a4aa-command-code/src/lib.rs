//! `a4aa-command-code` — the `commandcode` dialect descriptor for a4aa's mechanism
//! adaptation engine.
//!
//! One agent, one crate: this package owns only the `commandcode` mechanism
//! surface (hook store, skills/commands/subagents locations, permission and
//! MCP config shapes). It depends on `a4aa` core for the descriptor
//! vocabulary and the shared [`Dialect::adapt`] engine; it depends on no
//! other agent adapter. Install it alone when `commandcode` is the only agent a
//! host drives.
//!
//! [`Dialect::adapt`]: a4aa::Dialect::adapt

mod native;

use a4aa::card::{
    AgentCard, AgentSurface, AttachmentSupport, CliSurface, DetectSurface, FeatureSurface,
    HistoryRoot, InstructionChannel, ModelCatalog, StoreKind, UsageSurface,
};
use a4aa::dialect::{
    CommandSurface, HookSurface, McpSurface, PermissionSurface, PluginSurface, SubagentSurface,
    Target,
};
use a4aa::model::{BindingReceiptShape, PromptChannel, StreamMode};
use a4aa::{AdaptationPlan, AgentSpec, ArtifactScope, Dialect, HookEvent, MechanismSet};

pub use native::{native, NATIVE};

/// The `commandcode` dialect: Command Code settings.json hooks, AGENTS.md, skills.
pub const DIALECT: Dialect = Dialect {
    id: "commandcode",
    hooks: HookSurface::Claude {
        file: ".commandcode/settings.json",
        at: "hooks",
        events: &[
            HookEvent::SessionStart,
            HookEvent::PreToolUse,
            HookEvent::PostToolUse,
            HookEvent::Stop,
        ],
    },
    rules: Some(Target {
        scope: ArtifactScope::Project,
        path: "AGENTS.md",
    }),
    skills_dir: Some(".commandcode/skills"),
    commands: CommandSurface::None,
    subagents: SubagentSurface::None,
    plugins: PluginSurface::None,
    permissions: PermissionSurface::None,
    mcp: McpSurface::None,
    native_profiles: false,
};

/// Adapt semantic declarations into the `commandcode` artifact plan. Equivalent to
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

/// The `command-code` agent card. Command Code ships no ACP, server or
/// SDK surface — `command-code --print` is the headless form, streaming
/// newline-delimited run events (`--output-format json`) whose opening
/// `run_start` record carries the session id a later `--resume` accepts.
pub const AGENT: AgentCard = AgentCard {
    id: "commandcode",
    label: "Command Code",
    dialect: &DIALECT,
    surface: AgentSurface::Cli(CliSurface {
        spawn_args: &[],
        args: &[
            "--print",
            "{prompt}",
            "--output-format",
            "json",
            "--model",
            "{model}",
            "--effort",
            "{effort}",
            "--permission-mode",
            "{permission}",
        ],
        prompt_channel: PromptChannel::ArgOnly,
        stream: StreamMode::Stdio,
        resume_spawn_args: &[],
        resume_args: &["--resume", "{sessionId}"],
        stdin_template: None,
        json_lines: true,
        session_id_path: Some("sessionId"),
        open_args: &[],
        binding_file_env: None,
        binding_receipt_shape: BindingReceiptShape::SessionId,
        result_selector: Some("result"),
        result_selector_field: Some("type"),
        result_path: None,
        usage_path: None,

        effort_model_suffixes: &[],
    }),
    alternates: &[],
    detect: DetectSurface {
        binaries: &["command-code"],
        processes: &["command-code"],
        binary_dirs: &[],
        config: &["{home}/.commandcode/config.json"],
        detection: &["{home}/.commandcode"],
        apps: &[],
    },
    features: FeatureSurface {
        instructions: InstructionChannel::Compose,
        execution_modes: &["plan"],
        permission_modes: &["standard", "plan", "auto-accept"],
        permission_default: Some("standard"),
        attachments: AttachmentSupport::None,
        // `--effort <level>`.
        effort: true,
        steer: false,
        models: ModelCatalog::CliArgs(&["--list-models"]),
    },
    usage: UsageSurface {
        live: None,
        transcript: None,
        hosted: None,
    },
    history: &[
        HistoryRoot {
            path: "{home}/.commandcode/projects",
            kind: "commandcode-transcripts",
            store: StoreKind::JsonlDir,
        },
        HistoryRoot {
            path: "{home}/.commandcode",
            kind: "commandcode-config",
            store: StoreKind::Directory,
        },
    ],
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
        assert_eq!(plan.dialect, "commandcode");
        assert!(plan.artifacts.is_empty());
    }

    #[test]
    fn spec_materializes_the_cli_lane() {
        let spec = spec();
        assert_eq!(spec.command.program, "command-code");
        let a4aa::LaneSpec::Cli(cli) = spec.lane else {
            panic!("command-code drives the cli lane")
        };
        assert!(cli.json_lines);
        assert_eq!(cli.session_id_path.as_deref(), Some("sessionId"));
        assert_eq!(
            cli.resume_args,
            vec!["--resume".to_owned(), "{sessionId}".to_owned()]
        );
    }
}
