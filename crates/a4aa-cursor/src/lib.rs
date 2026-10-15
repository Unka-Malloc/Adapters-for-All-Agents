//! `a4aa-cursor` — the `cursor` dialect descriptor for a4aa's mechanism
//! adaptation engine.
//!
//! One agent, one crate: this package owns only the `cursor` mechanism
//! surface (hook store, skills/commands/subagents locations, permission and
//! MCP config shapes). It depends on `a4aa` core for the descriptor
//! vocabulary and the shared [`Dialect::adapt`] engine; it depends on no
//! other agent adapter. Install it alone when `cursor` is the only agent a
//! host drives.
//!
//! [`Dialect::adapt`]: a4aa::Dialect::adapt

use a4aa::card::{
    AgentCard, AgentSurface, AttachmentSupport, CliSurface, DetectSurface, FeatureSurface,
    HistoryRoot, InstructionChannel, ModelCatalog, OsPath, StoreKind, UsageSurface,
};
use a4aa::dialect::{
    CommandSurface, HookSurface, McpSurface, PermissionSurface, PluginSurface, SubagentSurface,
    Target,
};
use a4aa::{
    AdaptationPlan, AgentSpec, ArtifactScope, Dialect, HookEvent, MechanismSet, PromptChannel,
    StreamMode,
};

/// The `cursor` dialect: Cursor flat hooks.json, .mdc rules, skills, commands, subagents, cli-config permissions, mcp.json.
pub const DIALECT: Dialect = Dialect {
    id: "cursor",
    hooks: HookSurface::Flat {
        file: ".cursor/hooks.json",
        map: &[(HookEvent::UserPromptSubmit, "beforeSubmitPrompt")],
    },
    rules: Some(Target {
        scope: ArtifactScope::Project,
        path: ".cursor/rules/a4aa.mdc",
    }),
    skills_dir: Some(".cursor/skills"),
    commands: CommandSurface::MarkdownDir(".cursor/commands"),
    subagents: SubagentSurface::MarkdownDir(".cursor/agents"),
    plugins: PluginSurface::None,
    permissions: PermissionSurface::JsonMerge {
        file: ".cursor/cli-config.json",
        at: "permissions",
        mode_key: None,
    },
    mcp: McpSurface::JsonMerge {
        file: ".cursor/mcp.json",
        scope: ArtifactScope::UserHome,
        key: "mcpServers",
        adapted: false,
    },
    native_profiles: false,
};

/// Adapt semantic declarations into the `cursor` artifact plan. Equivalent to
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

/// The `cursor` agent card: `cursor-agent` stream-json CLI over a PTY.
///
/// Wire shape verified against a production host: a session opens with
/// `cursor-agent create-chat` — the first non-empty stdout line is the
/// binding — and every turn is one process
/// `cursor-agent --print --output-format stream-json --trust --force
/// --approve-mcps --stream-partial-output --workspace <dir> --resume <id>
/// <prompt> [--model <m>] [--reasoning-effort <e>]`.
/// The stream echoes the bound `session_id` on every record and carries
/// the reply text on the terminal `{type:"result"}` record. Token
/// accounting lives only in Cursor's hosted usage-events API; the local
/// transcript stores carry no token counters.
pub const AGENT: AgentCard = AgentCard {
    id: "cursor",
    label: "Cursor",
    dialect: &DIALECT,
    surface: AgentSurface::Cli(CliSurface {
        args: &[
            "--print",
            "--output-format",
            "stream-json",
            "--trust",
            "--force",
            "--approve-mcps",
            "--stream-partial-output",
            "--workspace",
            "{cwd}",
            "--resume",
            "{sessionId}",
            "{prompt}",
            "--model",
            "{model}",
            "--reasoning-effort",
            "{effort}",
        ],
        prompt_channel: PromptChannel::ArgOnly,
        stream: StreamMode::Pty,
        // `--resume {sessionId}` rides the argv template itself: every
        // cursor-agent turn is bound to a chat id.
        resume_args: &[],
        stdin_template: None,
        json_lines: true,
        session_id_path: Some("session_id"),
        usage_path: None,
        open_args: &["create-chat"],
        binding_file_env: None,
        binding_receipt_shape: a4aa::BindingReceiptShape::SessionId,
        result_selector: Some("result"),
        result_selector_field: None,
        result_path: Some("result"),
        spawn_args: &[],
    }),
    detect: DetectSurface {
        binaries: &["cursor-agent", "cursor"],
        processes: &["cursor-agent.exe", "cursor-agent", "cursor.exe", "cursor"],
        binary_dirs: &[
            OsPath {
                path: "/Applications/Cursor.app/Contents/Resources/app/bin",
                oses: &["macos"],
            },
            OsPath {
                path: "{home}/Applications/Cursor.app/Contents/Resources/app/bin",
                oses: &["macos"],
            },
            OsPath {
                path: "{home}/.local/share/cursor-agent",
                oses: &[],
            },
        ],
        config: &[
            "{home}/Library/Application Support/Cursor/User/globalStorage/saoudrizwan.claude-dev/settings/cline_mcp_settings.json",
            "{appdata}/Cursor/User/globalStorage/saoudrizwan.claude-dev/settings/cline_mcp_settings.json",
            "{xdg_config}/Cursor/User/globalStorage/saoudrizwan.claude-dev/settings/cline_mcp_settings.json",
        ],
        detection: &[
            "{home}/Library/Application Support/Cursor",
            "{appdata}/Cursor",
            "{xdg_config}/Cursor",
        ],
        apps: &[
            OsPath {
                path: "/Applications/Cursor.app/Contents/MacOS/Cursor",
                oses: &["macos"],
            },
            OsPath {
                path: "{home}/Applications/Cursor.app/Contents/MacOS/Cursor",
                oses: &["macos"],
            },
        ],
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
        // Local chat stores carry no token counters (`tokenCount` bubbles
        // are unused and `usageData` is empty); the hosted usage-events
        // feed is the only accounting source.
        transcript: None,
        hosted: Some("https://cursor.com/api/dashboard/get-filtered-usage-events"),
    },
    history: &[
        HistoryRoot {
            path: "{home}/.cursor/agents",
            kind: "cursor-agents",
            store: StoreKind::Directory,
        },
        HistoryRoot {
            path: "{home}/.cursor/chats",
            kind: "cursor-cli-chats",
            store: StoreKind::Directory,
        },
        HistoryRoot {
            path: "{home}/.cursor/projects",
            kind: "cursor-cli-projects",
            store: StoreKind::Directory,
        },
        HistoryRoot {
            path: "{home}/Library/Application Support/Cursor/User/workspaceStorage",
            kind: "cursor-workspace-storage",
            store: StoreKind::Directory,
        },
        HistoryRoot {
            path: "{home}/Library/Application Support/Cursor/User/globalStorage",
            kind: "cursor-global-storage",
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
        assert_eq!(plan.dialect, "cursor");
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
    fn spec_materializes_create_chat_plus_resume_argv() {
        let spec = spec();
        assert_eq!(spec.command.program, "cursor-agent");
        let a4aa::LaneSpec::Cli(cli) = spec.lane else {
            panic!("cursor drives the cli lane")
        };
        assert_eq!(cli.open_args, vec!["create-chat".to_string()]);
        assert_eq!(cli.stream, StreamMode::Pty);
        assert!(cli.json_lines);
        assert_eq!(cli.session_id_path.as_deref(), Some("session_id"));
        assert_eq!(cli.result_selector.as_deref(), Some("result"));
        assert_eq!(cli.result_path.as_deref(), Some("result"));
        // `--resume {sessionId}` lives inside the argv template — every
        // cursor-agent turn is a bound turn.
        let joined = cli.args.join(" ");
        assert!(joined.contains("--resume {sessionId}"));
        assert!(joined.contains("--stream-partial-output"));
        assert!(joined.contains("{prompt}"));
        assert!(cli.capabilities().session_resume);
        assert!(cli.capabilities().cancellation);
    }

    #[test]
    fn usage_points_at_the_hosted_ledger() {
        assert!(AGENT.usage.live.is_none());
        assert!(AGENT.usage.transcript.is_none());
        assert_eq!(
            AGENT.usage.hosted,
            Some("https://cursor.com/api/dashboard/get-filtered-usage-events")
        );
    }
}
