//! `a4aa-antigravity` — the `antigravity` dialect descriptor for a4aa's mechanism
//! adaptation engine.
//!
//! One agent, one crate: this package owns only the `antigravity` mechanism
//! surface (hook store, skills/commands/subagents locations, permission and
//! MCP config shapes). It depends on `a4aa` core for the descriptor
//! vocabulary and the shared [`Dialect::adapt`] engine; it depends on no
//! other agent adapter. Install it alone when `antigravity` is the only agent a
//! host drives.
//!
//! [`Dialect::adapt`]: a4aa::Dialect::adapt

mod native;

use a4aa::card::{
    AgentCard, AgentSurface, AttachmentSupport, CliSurface, DetectSurface, FeatureSurface,
    HistoryRoot, InstructionChannel, LiveUsage, ModelCatalog, StoreKind, UsageSurface,
};
use a4aa::dialect::{
    CommandSurface, HookSurface, McpSurface, PermissionSurface, PluginSurface, SubagentSurface,
    Target,
};
use a4aa::{AdaptationPlan, AgentSpec, ArtifactScope, Dialect, HookEvent, MechanismSet};
use a4aa::{PromptChannel, StreamMode};

pub use native::{native, NATIVE};

/// The Antigravity CLI's managed customization surfaces.
pub const DIALECT: Dialect = Dialect {
    id: "antigravity",
    hooks: HookSurface::NamedBundle {
        file: ".gemini/config/hooks.json",
        map: &[
            (HookEvent::SessionStart, "SessionStart"),
            (HookEvent::PreToolUse, "PreToolUse"),
            (HookEvent::PostToolUse, "PostToolUse"),
            (HookEvent::Stop, "Stop"),
        ],
    },
    rules: Some(Target {
        scope: ArtifactScope::Project,
        path: "GEMINI.md",
    }),
    skills_dir: Some(".gemini/antigravity-cli/skills"),
    commands: CommandSurface::MarkdownDir(".gemini/config/workflows"),
    subagents: SubagentSurface::MarkdownDir(".gemini/config/agents"),
    plugins: PluginSurface::None,
    permissions: PermissionSurface::None,
    mcp: McpSurface::JsonMerge {
        file: ".gemini/config/mcp_config.json",
        scope: ArtifactScope::UserHome,
        key: "mcpServers",
        adapted: true,
    },
    native_profiles: false,
};

/// Adapt semantic declarations into the `antigravity` artifact plan. Equivalent to
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

/// The Antigravity agent card. `agy --print --output-format stream-json`
/// is the native headless lane: `--model`, `--effort`, `--mode` on argv,
/// `--conversation {sessionId}` resumes, `conversation_id` binds the
/// session, `result`/`result.usage` select the turn. The former
/// `agy_acp_server.par` ACP bundle is an external plugin binary and is
/// not a card surface.
pub const AGENT: AgentCard = AgentCard {
    id: "antigravity",
    label: "Antigravity",
    dialect: &DIALECT,
    surface: AgentSurface::Cli(CliSurface {
        args: &[
            "--print",
            "{prompt}",
            "--output-format",
            "stream-json",
            "--model",
            "{model}",
            "--effort",
            "{effort}",
            "--mode",
            "{mode}",
        ],
        trusted_workspace_args: &[],
        prompt_channel: PromptChannel::ArgOnly,
        stream: StreamMode::Stdio,
        resume_spawn_args: &[],
        resume_args: &["--conversation", "{sessionId}"],
        stdin_template: None,
        json_lines: true,
        session_id_path: Some("conversation_id"),
        open_args: &[],
        binding_file_env: None,
        binding_receipt_shape: a4aa::BindingReceiptShape::SessionId,
        result_selector: Some("result"),
        result_selector_field: Some("event"),
        result_path: Some("result.response"),
        usage_path: Some("result.usage"),
        spawn_args: &[],
        effort_model_suffixes: &[],
    }),
    alternates: &[],
    detect: DetectSurface {
        binaries: &["agy"],
        processes: &["agy.exe", "agy"],
        binary_dirs: &[],
        config: &[
            "{home}/.gemini/antigravity-cli/settings.json",
            "{home}/.gemini/config/hooks.json",
            "{home}/.gemini/config/mcp_config.json",
        ],
        detection: &["{home}/.gemini/antigravity-cli"],
        apps: &[],
    },
    features: FeatureSurface {
        instructions: InstructionChannel::Compose,
        execution_modes: &["accept-edits", "auto", "plan"],
        // Headless approval is governed by the user's settings. The CLI
        // cannot accept a per-request permission preset except by globally
        // approving every tool with `--dangerously-skip-permissions`.
        permission_modes: &[],
        permission_default: None,
        attachments: AttachmentSupport::None,
        effort: true,
        steer: false,
        models: ModelCatalog::CliArgs(&["models"]),
    },
    usage: UsageSurface {
        live: Some(LiveUsage {
            event: "result",
            path: "result.usage",
        }),
        transcript: None,
        hosted: None,
    },
    history: &[HistoryRoot {
        path: "{home}/.gemini/antigravity-cli",
        kind: "antigravity-cli-session-store",
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
        assert_eq!(plan.dialect, "antigravity");
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
    fn spec_materializes_the_stream_json_cli() {
        let spec = spec();
        assert_eq!(spec.command.program, "agy");
        let a4aa::LaneSpec::Cli(cli) = spec.lane else {
            panic!("antigravity drives the cli lane")
        };
        assert!(cli.args.iter().any(|arg| arg == "--print"));
        assert!(cli.args.iter().any(|arg| arg == "stream-json"));
        assert!(cli.args.iter().any(|arg| arg == "--effort"));
        assert!(cli.args.iter().any(|arg| arg == "{effort}"));
        assert_eq!(
            cli.resume_args,
            vec!["--conversation".to_owned(), "{sessionId}".to_owned()]
        );
        assert_eq!(cli.session_id_path.as_deref(), Some("conversation_id"));
        assert_eq!(AGENT.detect.binaries, &["agy"]);
        assert!(AGENT.alternates.is_empty());
    }

    #[test]
    fn profile_capabilities_are_truthful_and_paths_are_current() {
        assert_eq!(DIALECT.skills_dir, Some(".gemini/antigravity-cli/skills"));
        assert!(matches!(
            DIALECT.commands,
            CommandSurface::MarkdownDir(".gemini/config/workflows")
        ));
        assert!(matches!(
            DIALECT.subagents,
            SubagentSurface::MarkdownDir(".gemini/config/agents")
        ));
        assert!(matches!(DIALECT.permissions, PermissionSurface::None));
        assert_eq!(
            AGENT.features.execution_modes,
            &["accept-edits", "auto", "plan"]
        );
        assert!(AGENT.features.permission_modes.is_empty());
        assert_eq!(AGENT.features.permission_default, None);
        assert_eq!(AGENT.features.models, ModelCatalog::CliArgs(&["models"]));
        assert_eq!(AGENT.detect.binaries, &["agy"]);
        assert!(AGENT
            .detect
            .config
            .contains(&"{home}/.gemini/config/mcp_config.json"));
    }
}
