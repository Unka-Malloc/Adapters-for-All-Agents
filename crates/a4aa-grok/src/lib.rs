//! `a4aa-grok` — the `grok` dialect descriptor for a4aa's mechanism
//! adaptation engine.
//!
//! One agent, one crate: this package owns only the `grok` mechanism
//! surface (hook store, skills/commands/subagents locations, permission and
//! MCP config shapes). It depends on `a4aa` core for the descriptor
//! vocabulary and the shared [`Dialect::adapt`] engine; it depends on no
//! other agent adapter. Install it alone when `grok` is the only agent a
//! host drives.
//!
//! [`Dialect::adapt`]: a4aa::Dialect::adapt

mod native;

use a4aa::card::{
    AgentCard, AgentSurface, AlternateLane, AttachmentSupport, CliSurface, DetectSurface,
    FeatureSurface, HistoryRoot, InstructionChannel, LiveUsage, ModelCatalog, StoreKind,
    UsageSurface,
};
use a4aa::dialect::{
    CommandSurface, HookSurface, McpSurface, PermissionSurface, PluginSurface, SubagentSurface,
    Target,
};
use a4aa::model::{BindingReceiptShape, PromptChannel, StreamMode};
use a4aa::{AdaptationPlan, AgentSpec, ArtifactScope, Dialect, HookEvent, MechanismSet};

pub use native::{native, NATIVE};

/// The `grok` dialect: Grok per-file hook bundles, AGENTS.md, skills, commands, subagents.
pub const DIALECT: Dialect = Dialect {
    id: "grok",
    hooks: HookSurface::ClaudeBundle {
        dir: ".grok/hooks",
        events: &[
            HookEvent::SessionStart,
            HookEvent::SessionEnd,
            HookEvent::UserPromptSubmit,
            HookEvent::PreToolUse,
            HookEvent::PostToolUse,
            HookEvent::Stop,
            HookEvent::SubagentStart,
            HookEvent::SubagentStop,
            HookEvent::PermissionRequest,
            HookEvent::Notification,
        ],
    },
    rules: Some(Target {
        scope: ArtifactScope::Project,
        path: "AGENTS.md",
    }),
    skills_dir: Some(".grok/skills"),
    commands: CommandSurface::MarkdownDir(".grok/commands"),
    subagents: SubagentSurface::MarkdownDir(".grok/agents"),
    plugins: PluginSurface::None,
    permissions: PermissionSurface::None,
    mcp: McpSurface::None,
    native_profiles: false,
};

/// Adapt semantic declarations into the `grok` artifact plan. Equivalent to
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

/// The `grok` agent card. `grok agent stdio` is a verified Agent Client
/// Protocol endpoint: `initialize` reports `loadSession`, session
/// list/resume/close, `promptCapabilities.embeddedContext`, MCP http/sse
/// and the `x.ai/*` meta surface (hooks, model state, cancelRewind). The
/// `grok.com` auth method is OAuth — the terminal `grok login` flow, not a
/// key the host presents — so `auth_key` stays `None` and credential
/// absence surfaces as the agent's own auth-required refusal.
pub const AGENT: AgentCard = AgentCard {
    id: "grok",
    label: "Grok",
    dialect: &DIALECT,
    surface: AgentSurface::Acp {
        prefer_load: true,
        spawn_args: &["agent", "stdio"],
        auth_key: None,
    },
    alternates: &[AlternateLane {
        // `grok -p` headless mode: `--output-format streaming-json` emits
        // NDJSON ACP session updates (the agent's native wire), `-r`
        // resumes a session id, `-s` sets one explicitly.
        surface: AgentSurface::Cli(CliSurface {
            args: &[
                "-p",
                "{prompt}",
                "--output-format",
                "streaming-json",
                "-m",
                "{model}",
                "--effort",
                "{effort}",
                "--permission-mode",
                "{permission}",
            ],
            trusted_workspace_args: &[],
            prompt_channel: PromptChannel::ArgOnly,
            stream: StreamMode::Stdio,
            resume_spawn_args: &[],
            resume_args: &["-r", "{sessionId}"],
            stdin_template: None,
            json_lines: true,
            session_id_path: Some("sessionId"),
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
        binaries: &["grok"],
        processes: &["grok"],
        binary_dirs: &[],
        config: &["{home}/.grok/config.toml"],
        detection: &["{home}/.grok"],
        apps: &[],
    },
    features: FeatureSurface {
        instructions: InstructionChannel::Compose,
        execution_modes: &["plan"],
        // `--permission-mode` values the installed CLI accepts.
        permission_modes: &[
            "default",
            "acceptEdits",
            "auto",
            "dontAsk",
            "bypassPermissions",
            "plan",
        ],
        permission_default: Some("default"),
        attachments: AttachmentSupport::None,
        // `--reasoning-effort` / `--effort`.
        effort: true,
        steer: false,
        models: ModelCatalog::CliArgs(&["models"]),
    },
    usage: UsageSurface {
        live: Some(LiveUsage {
            event: "session/update",
            path: "usage",
        }),
        transcript: None,
        hosted: None,
    },
    history: &[
        HistoryRoot {
            path: "{home}/.grok/sessions",
            kind: "grok-session-store",
            store: StoreKind::Directory,
        },
        HistoryRoot {
            path: "{home}/.grok",
            kind: "grok-config",
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
        assert_eq!(plan.dialect, "grok");
        assert!(plan.artifacts.is_empty());
    }

    #[test]
    fn spec_materializes_the_acp_lane() {
        let spec = spec();
        assert_eq!(spec.command.program, "grok");
        assert_eq!(spec.command.args, vec!["agent", "stdio"]);
        let a4aa::LaneSpec::Acp { prefer_load, .. } = spec.lane else {
            panic!("grok drives the acp lane")
        };
        assert!(prefer_load);
    }
}
