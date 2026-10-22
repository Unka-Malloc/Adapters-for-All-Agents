//! `a4aa-claude` — the `claude` dialect descriptor for a4aa's mechanism
//! adaptation engine.
//!
//! One agent, one crate: this package owns only the `claude` mechanism
//! surface (hook store, skills/commands/subagents locations, permission and
//! MCP config shapes). It depends on `a4aa` core for the descriptor
//! vocabulary and the shared [`Dialect::adapt`] engine; it depends on no
//! other agent adapter. Install it alone when `claude` is the only agent a
//! host drives.
//!
//! [`Dialect::adapt`]: a4aa::Dialect::adapt

mod native;

use a4aa::card::{
    AgentCard, AgentSurface, AttachmentSupport, CliSurface, DetectSurface, FeatureSurface,
    HistoryRoot, InstructionChannel, LiveUsage, ModelCatalog, OsPath, StoreKind, TranscriptUsage,
    UsageSurface,
};
use a4aa::dialect::{
    CommandSurface, HookSurface, McpSurface, PermissionSurface, PluginSurface, SubagentSurface,
    Target,
};
use a4aa::{
    AdaptationPlan, AgentSpec, ArtifactScope, Dialect, HookEvent, MechanismSet, PromptChannel,
    StreamMode,
};

pub use native::{native, NATIVE};

/// The `claude` dialect: Claude Code settings.json hooks, CLAUDE.md, skills, commands, subagents, permissions, .mcp.json.
pub const DIALECT: Dialect = Dialect {
    id: "claude",
    hooks: HookSurface::Claude {
        file: ".claude/settings.json",
        at: "hooks",
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
        path: "CLAUDE.md",
    }),
    skills_dir: Some(".claude/skills"),
    commands: CommandSurface::MarkdownDir(".claude/commands"),
    subagents: SubagentSurface::MarkdownDir(".claude/agents"),
    plugins: PluginSurface::None,
    permissions: PermissionSurface::JsonMerge {
        file: ".claude/settings.json",
        at: "permissions",
        mode_key: Some("defaultMode"),
    },
    mcp: McpSurface::JsonMerge {
        file: ".mcp.json",
        scope: ArtifactScope::Project,
        key: "mcpServers",
        adapted: false,
    },
    native_profiles: false,
};

/// Adapt semantic declarations into the `claude` artifact plan. Equivalent to
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

/// The `claude` agent card: `claude --print` stream-json over stdio.
///
/// Protocol: one process per turn,
/// fixed stream-json argv, the prompt as a `stream-json` user message on
/// stdin, NDJSON events on stdout (`system/init` carries `session_id`,
/// `result` carries the reply text and token usage). Resume is a fresh
/// process with `--resume <id>`; guidance rides `--append-system-prompt`.
/// Permissions follow the agent's own native setting unless the host
/// explicitly selects one: `bypassPermissions` (or any other
/// `--permission-mode` value) is passed only on an explicit selection.
pub const AGENT: AgentCard = AgentCard {
    id: "claude",
    label: "Claude Code",
    dialect: &DIALECT,
    surface: AgentSurface::Cli(CliSurface {
        args: &[
            "--print",
            "--input-format",
            "stream-json",
            "--output-format",
            "stream-json",
            "--verbose",
            "--include-partial-messages",
            "--model",
            "{model}",
            "--effort",
            "{effort}",
            "--permission-mode",
            "{permission}",
            "--append-system-prompt",
            "{instructions}",
        ],
        trusted_workspace_args: &[],
        prompt_channel: PromptChannel::Stdin,
        stream: StreamMode::Stdio,
        resume_spawn_args: &[],
        resume_args: &["--resume", "{sessionId}"],
        stdin_template: Some(
            r#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"{prompt}"}]}}"#,
        ),
        json_lines: true,
        session_id_path: Some("session_id"),
        open_args: &[],
        binding_file_env: None,
        binding_receipt_shape: a4aa::BindingReceiptShape::SessionId,
        // `{"type":"result"}` records carry the reply text in `result`.
        result_selector: Some("result"),
        result_selector_field: None,
        result_path: Some("result"),
        usage_path: None,
        spawn_args: &[],

        effort_model_suffixes: &[],
    }),
    alternates: &[],
    detect: DetectSurface {
        binaries: &["claude"],
        processes: &["claude.exe", "claude"],
        binary_dirs: &[
            OsPath {
                path: "{home}/.claude/local",
                oses: &[],
            },
            OsPath {
                path: "{home}/.claude/local/node_modules/.bin",
                oses: &[],
            },
        ],
        config: &["{home}/.claude/settings.json", "{home}/.claude.json"],
        detection: &["{home}/.claude"],
        apps: &[],
    },
    features: FeatureSurface {
        instructions: InstructionChannel::Flag("--append-system-prompt"),
        execution_modes: &[],
        permission_modes: &[
            "default",
            "manual",
            "acceptEdits",
            "plan",
            "auto",
            "dontAsk",
            "bypassPermissions",
        ],
        permission_default: None,
        attachments: AttachmentSupport::None,
        effort: true,
        steer: false,
        models: ModelCatalog::None,
    },
    usage: UsageSurface {
        live: Some(LiveUsage {
            event: "result",
            path: "usage",
        }),
        transcript: Some(TranscriptUsage {
            root: "claude-project-transcripts",
            record: Some(("type", "assistant")),
            container: "message.usage",
            input: &["input_tokens"],
            cached: &["cache_read_input_tokens", "cache_creation_input_tokens"],
            output: &["output_tokens"],
            total: &[],
        }),
        hosted: None,
    },
    history: &[
        HistoryRoot {
            path: "{home}/.claude/agents",
            kind: "claude-agents",
            store: StoreKind::Directory,
        },
        HistoryRoot {
            path: "{home}/.claude/projects",
            kind: "claude-project-transcripts",
            store: StoreKind::JsonlDir,
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
        assert_eq!(plan.dialect, "claude");
        assert!(plan.artifacts.is_empty());
    }

    #[test]
    fn card_matches_dialect_identity() {
        assert_eq!(AGENT.id, DIALECT.id);
        assert!(std::ptr::eq(AGENT.dialect, &DIALECT));
    }

    #[test]
    fn spec_materializes_the_stream_json_cli() {
        let spec = spec();
        assert_eq!(spec.command.program, "claude");
        let a4aa::LaneSpec::Cli(cli) = spec.lane else {
            panic!("claude drives the cli lane")
        };
        assert!(cli.args.iter().any(|arg| arg == "--print"));
        assert_eq!(cli.resume_args, vec!["--resume", "{sessionId}"]);
        assert!(cli.json_lines);
        assert_eq!(cli.session_id_path.as_deref(), Some("session_id"));
        assert_eq!(cli.usage_path.as_deref(), Some("usage"));
        assert!(cli.stdin_template.is_some());
        assert!(cli.capabilities().session_resume);
        assert!(cli.capabilities().cancellation);
    }

    #[test]
    fn usage_names_the_stream_json_result_event() {
        let live = AGENT.usage.live.as_ref().unwrap();
        assert_eq!(live.event, "result");
        assert_eq!(live.path, "usage");
        let transcript = AGENT.usage.transcript.as_ref().unwrap();
        assert_eq!(transcript.root, "claude-project-transcripts");
    }
}
