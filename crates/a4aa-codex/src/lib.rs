//! `a4aa-codex` — the `codex` dialect descriptor for a4aa's mechanism
//! adaptation engine.
//!
//! One agent, one crate: this package owns only the `codex` mechanism
//! surface (hook store, skills/commands/subagents locations, permission and
//! MCP config shapes). It depends on `a4aa` core for the descriptor
//! vocabulary and the shared [`Dialect::adapt`] engine; it depends on no
//! other agent adapter. Install it alone when `codex` is the only agent a
//! host drives.
//!
//! [`Dialect::adapt`]: a4aa::Dialect::adapt

use a4aa::card::{
    AgentCard, AgentSurface, AttachmentSupport, CompletionDecl, DetectSurface, FeatureSurface,
    HistoryRoot, InstructionChannel, ModelCatalog, OsPath, RpcCallDecl, RpcSurface, StoreKind,
    TranscriptUsage, UsageSurface,
};
use a4aa::dialect::{
    CommandSurface, HookSurface, McpSurface, PermissionSurface, PluginSurface, SubagentSurface,
    Target,
};
use a4aa::{AdaptationPlan, AgentSpec, ArtifactScope, Dialect, HookEvent, MechanismSet};

/// The `codex` dialect: Codex hooks.json (Claude store), AGENTS.md, TOML subagents, custom prompts, native profiles.
pub const DIALECT: Dialect = Dialect {
    id: "codex",
    hooks: HookSurface::Claude {
        file: ".codex/hooks.json",
        at: "",
        events: &[
            HookEvent::SessionStart,
            HookEvent::UserPromptSubmit,
            HookEvent::PreToolUse,
            HookEvent::PermissionRequest,
            HookEvent::PostToolUse,
            HookEvent::Stop,
            HookEvent::SubagentStart,
            HookEvent::SubagentStop,
        ],
    },
    rules: Some(Target {
        scope: ArtifactScope::Project,
        path: "AGENTS.md",
    }),
    skills_dir: Some(".codex/skills"),
    commands: CommandSurface::PromptDir(".codex/prompts"),
    subagents: SubagentSurface::CodexToml(".codex/agents"),
    plugins: PluginSurface::None,
    permissions: PermissionSurface::None,
    mcp: McpSurface::None,
    native_profiles: true,
};

/// Adapt semantic declarations into the `codex` artifact plan. Equivalent to
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

/// The `codex` agent card: `codex app-server` stdio JSON-RPC.
///
/// Wire shape verified against a production host: `initialize`/`initialized`
/// handshake, then `thread/start` (fresh) or `thread/resume` (bound). Turns
/// run `turn/start` → async `turn/completed`; `turn/interrupt` cancels the
/// in-flight turn. `account/rateLimits/read`, `model/list`, `thread/unarchive`,
/// `thread/configure` and `turn/steer` stay available as passthrough.
pub const AGENT: AgentCard = AgentCard {
    id: "codex",
    label: "Codex",
    dialect: &DIALECT,
    surface: AgentSurface::Rpc(&RpcSurface {
        initialize: Some(RpcCallDecl {
            method: "initialize",
            params: r#"{"clientInfo":{"name":"a4aa","version":"0.1.0"}}"#,
        }),
        initialized: Some("initialized"),
        session_open: Some(RpcCallDecl {
            method: "thread/start",
            params: r#"{
                "cwd": "{cwd}",
                "sandbox": "{permission}",
                "developerInstructions": "{instructions}"
            }"#,
        }),
        session_resume: Some(RpcCallDecl {
            method: "thread/resume",
            params: r#"{"threadId": "{sessionId}"}"#,
        }),
        prompt: RpcCallDecl {
            method: "turn/start",
            params: r#"{
                "threadId": "{sessionId}",
                "input": [{"type": "text", "text": "{prompt}"}],
                "model": "{model}",
                "effort": "{effort}"
            }"#,
        },
        cancel: Some(RpcCallDecl {
            method: "turn/interrupt",
            params: r#"{"threadId": "{sessionId}", "turnId": "{turnId}"}"#,
        }),
        session_close: None,
        text_event_method: Some("item/agentMessage/delta"),
        completion: Some(CompletionDecl {
            method: "turn/completed",
            turn_id_path: Some("turn.id"),
            status_path: Some("turn.status"),
            status_value: None,
        }),
        result_text_path: None,
        text_path: None,
        pre_turn: &[],
        session_scope: a4aa::model::SessionScope::Agent,
        kill_on_cancel: false,
        envelope: None,
        passthrough: &[
            "account/rateLimits/read",
            "model/list",
            "thread/unarchive",
            "thread/configure",
            "turn/steer",
        ],
        spawn_args: &["app-server"],
    }),
    detect: DetectSurface {
        binaries: &["codex"],
        processes: &["codex.exe", "codex"],
        binary_dirs: &[
            OsPath {
                path: "/Applications/ChatGPT.app/Contents/Resources/codex-cli/bin",
                oses: &["macos"],
            },
            OsPath {
                path: "{home}/Applications/ChatGPT.app/Contents/Resources/codex-cli/bin",
                oses: &["macos"],
            },
        ],
        config: &["{home}/.codex/config.toml"],
        detection: &[],
        apps: &[
            OsPath {
                path: "/Applications/ChatGPT.app/Contents/MacOS/ChatGPT",
                oses: &["macos"],
            },
            OsPath {
                path: "{home}/Applications/ChatGPT.app/Contents/MacOS/ChatGPT",
                oses: &["macos"],
            },
        ],
    },
    features: FeatureSurface {
        instructions: InstructionChannel::Param("developerInstructions"),
        execution_modes: &[],
        permission_modes: &["read-only", "workspace-write", "danger-full-access"],
        permission_default: None,
        attachments: AttachmentSupport::LocalImages,
        effort: true,
        steer: true,
        models: ModelCatalog::RpcMethod("model/list"),
    },
    usage: UsageSurface {
        // `turn/completed` carries no verified usage object today; the
        // session store is the accounting source.
        live: None,
        transcript: Some(TranscriptUsage {
            root: "codex-session-store",
            record: Some(("payload.type", "token_count")),
            container: "payload.info.last_token_usage",
            input: &["input_tokens"],
            cached: &["cached_input_tokens"],
            output: &["output_tokens"],
            total: &[],
        }),
        // Token accounting is local-first: `last_token_usage` on session
        // transcript records; `account/rateLimits/read` is the quota route.
        hosted: None,
    },
    history: &[
        HistoryRoot {
            path: "{home}/.codex/agents",
            kind: "codex-agents",
            store: StoreKind::Directory,
        },
        HistoryRoot {
            path: "{home}/.codex/history.jsonl",
            kind: "codex-prompt-history",
            store: StoreKind::JsonlFile,
        },
        HistoryRoot {
            path: "{home}/.codex/session_index.jsonl",
            kind: "codex-session-index",
            store: StoreKind::JsonlFile,
        },
        HistoryRoot {
            path: "{home}/.codex/sessions",
            kind: "codex-session-store",
            store: StoreKind::JsonlDir,
        },
        HistoryRoot {
            path: "{home}/.codex/archived_sessions",
            kind: "codex-archived-session-store",
            store: StoreKind::JsonlDir,
        },
        HistoryRoot {
            path: "{home}/.codex/memories/MEMORY.md",
            kind: "codex-memory",
            store: StoreKind::MarkdownFile,
        },
        HistoryRoot {
            path: "{home}/.codex/memories/rollout_summaries",
            kind: "codex-rollout-summary",
            store: StoreKind::Directory,
        },
    ],
};

/// The full card, for hosts that compose over `&'static AgentCard`.
pub const fn card() -> &'static AgentCard {
    &AGENT
}

/// The materialized runtime spec — `Adapter::new(agent::spec())` is the
/// entire launch path for hosts that keep no spec table of their own.
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
        assert_eq!(plan.dialect, "codex");
        assert!(plan.artifacts.is_empty());
    }

    #[test]
    fn card_matches_dialect_identity() {
        assert_eq!(AGENT.id, DIALECT.id);
        assert!(std::ptr::eq(AGENT.dialect, &DIALECT));
    }

    #[test]
    fn spec_materializes_the_app_server_lane() {
        let spec = spec();
        assert_eq!(spec.command.program, "codex");
        assert_eq!(spec.command.args, vec!["app-server".to_string()]);
        let a4aa::LaneSpec::Rpc(rpc) = spec.lane else {
            panic!("codex drives the rpc lane")
        };
        assert_eq!(rpc.initialize.as_ref().unwrap().method, "initialize");
        assert_eq!(rpc.initialized.as_deref(), Some("initialized"));
        assert_eq!(rpc.session_open.as_ref().unwrap().method, "thread/start");
        assert_eq!(rpc.session_resume.as_ref().unwrap().method, "thread/resume");
        assert_eq!(rpc.prompt.method, "turn/start");
        assert_eq!(rpc.cancel.as_ref().unwrap().method, "turn/interrupt");
        let done = rpc.completion.as_ref().unwrap();
        assert_eq!(done.method, "turn/completed");
        assert_eq!(done.turn_id_path.as_deref(), Some("turn.id"));
        assert!(rpc.passthrough.iter().any(|m| m == "model/list"));
        assert!(rpc.capabilities().session_resume);
    }

    #[test]
    fn transcript_usage_names_the_token_count_record() {
        let usage = AGENT.usage.transcript.as_ref().unwrap();
        assert_eq!(usage.root, "codex-session-store");
        assert_eq!(usage.record, Some(("payload.type", "token_count")));
    }
}
