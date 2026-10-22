//! `a4aa-opencode` — the `opencode` dialect descriptor for a4aa's mechanism
//! adaptation engine.
//!
//! One agent, one crate: this package owns only the `opencode` mechanism
//! surface (hook store, skills/commands/subagents locations, permission and
//! MCP config shapes). It depends on `a4aa` core for the descriptor
//! vocabulary and the shared [`Dialect::adapt`] engine; it depends on no
//! other agent adapter. Install it alone when `opencode` is the only agent a
//! host drives.
//!
//! [`Dialect::adapt`]: a4aa::Dialect::adapt

mod native;

use a4aa::card::{
    AgentCard, AgentSurface, AlternateLane, AttachmentSupport, DetectSurface, FeatureSurface,
    HistoryRoot, InstructionChannel, ModelCatalog, OsPath, StoreKind, UsageSurface,
};
use a4aa::dialect::{
    CommandSurface, ExtensionKind, HookSurface, McpSurface, PermissionSurface, PluginSurface,
    SubagentSurface, Target,
};
use a4aa::{AdaptationPlan, AgentSpec, ArtifactScope, Dialect, HookEvent, MechanismSet};

pub use native::{native, NATIVE};

/// The `opencode` dialect: OpenCode JS plugin hooks, AGENTS.md, skills, commands, subagents, opencode.json MCP.
pub const DIALECT: Dialect = Dialect {
    id: "opencode",
    hooks: HookSurface::Extension {
        file: ".config/opencode/plugins/a4aa.js",
        kind: ExtensionKind::OpenCode,
        map: &[
            (HookEvent::SessionStart, "session.created"),
            (HookEvent::PreToolUse, "tool.execute.before"),
            (HookEvent::PostToolUse, "tool.execute.after"),
        ],
    },
    rules: Some(Target {
        scope: ArtifactScope::Project,
        path: "AGENTS.md",
    }),
    skills_dir: Some(".opencode/skills"),
    commands: CommandSurface::MarkdownDir(".config/opencode/commands"),
    subagents: SubagentSurface::MarkdownDir(".config/opencode/agents"),
    plugins: PluginSurface::None,
    permissions: PermissionSurface::None,
    mcp: McpSurface::JsonMerge {
        file: ".config/opencode/opencode.json",
        scope: ArtifactScope::UserHome,
        key: "mcp",
        adapted: true,
    },
    native_profiles: false,
};

/// Adapt semantic declarations into the `opencode` artifact plan. Equivalent to
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

/// The `opencode` agent card: the official `opencode acp` NDJSON-stdio
/// surface.
///
/// Surfaces: the agent's own `opencode serve` HTTP server is the primary
/// lane — sessions, prompts and streaming ride its loopback REST API. When
/// the server cannot establish or complete transport, the ordered
/// alternate drops to `opencode acp` (Agent Client Protocol over stdio:
/// `session/new`, `session/load`, `session/prompt`, `session/update`
/// streaming, permission requests, `session/cancel`).
pub const AGENT: AgentCard = AgentCard {
    id: "opencode",
    label: "OpenCode",
    dialect: &DIALECT,
    surface: AgentSurface::Serve {
        // `serve --port 0` binds a free loopback port and announces the
        // endpoint and password on stdout for the lane to discover.
        spawn_args: &["serve", "--port", "0"],
    },
    alternates: &[AlternateLane {
        surface: AgentSurface::Acp {
            // `session/load` is the canonical resume path for an ACP server
            // that already persists sessions; prefer it over re-prompting.
            prefer_load: true,
            spawn_args: &["acp"],
            auth_key: None,
        },
        binary: None,
    }],
    detect: DetectSurface {
        binaries: &["opencode"],
        processes: &["opencode.exe", "opencode"],
        binary_dirs: &[OsPath {
            path: "{home}/.opencode/bin",
            oses: &[],
        }],
        config: &[
            "{xdg_config}/opencode/opencode.jsonc",
            "{home}/.config/opencode/opencode.jsonc",
            "{appdata}/opencode/opencode.jsonc",
        ],
        detection: &[
            "{home}/.config/opencode",
            "{home}/.local/share/opencode",
            "{xdg_config}/opencode",
            "{xdg_data}/opencode",
            "{appdata}/opencode",
        ],
        apps: &[],
    },
    features: FeatureSurface {
        // ACP `session/prompt` system guidance arrives through the
        // composed prompt; permission requests are protocol-native.
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
        // ACP `session/update` frames carry the live counter object when
        // the agent reports one.
        live: Some(a4aa::card::LiveUsage {
            event: "session/update",
            path: "usage",
        }),
        transcript: None,
        hosted: None,
    },
    history: &[
        HistoryRoot {
            path: "{home}/.local/share/opencode",
            kind: "opencode-data-store",
            store: StoreKind::Directory,
        },
        HistoryRoot {
            path: "{home}/.config/opencode",
            kind: "opencode-config",
            store: StoreKind::Directory,
        },
        HistoryRoot {
            path: "{xdg_data}/opencode",
            kind: "opencode-data-store",
            store: StoreKind::Directory,
        },
        HistoryRoot {
            path: "{xdg_config}/opencode",
            kind: "opencode-config",
            store: StoreKind::Directory,
        },
        HistoryRoot {
            path: "{appdata}/opencode",
            kind: "opencode-data-store",
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
        assert_eq!(plan.dialect, "opencode");
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
    fn spec_materializes_serve_first_then_acp() {
        let spec = spec();
        assert_eq!(spec.command.program, "opencode");
        assert_eq!(spec.command.args, vec!["serve", "--port", "0"]);
        assert!(matches!(spec.lane, a4aa::LaneSpec::Serve { .. }));
        let fallback = AGENT
            .specs()
            .into_iter()
            .nth(1)
            .expect("opencode declares the acp alternate");
        assert_eq!(fallback.command.args, vec!["acp".to_string()]);
        let a4aa::LaneSpec::Acp { prefer_load, .. } = fallback.lane else {
            panic!("opencode's alternate drives the acp lane")
        };
        assert!(prefer_load);
    }
}
