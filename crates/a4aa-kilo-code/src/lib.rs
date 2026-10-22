//! `a4aa-kilo-code` — the `kilocode` dialect descriptor for a4aa's mechanism
//! adaptation engine.
//!
//! One agent, one crate: this package owns only the Kilo Code mechanism
//! surface (plugin hooks, skills/commands/agents locations, permission and
//! MCP config shapes). It depends on `a4aa` core for the descriptor
//! vocabulary and the shared [`Dialect::adapt`] engine; it depends on no
//! other agent adapter. Install it alone when `kilocode` is the only agent a
//! host drives.
//!
//! Kilo Code's CLI is built on OpenCode, so its surface mirrors the
//! `opencode` dialect with `kilo`-scoped paths: plugins auto-register from
//! `~/.config/kilo/plugin/`, agents/commands/skills live under
//! `~/.config/kilo/` and `.kilo/`, and `kilo.json` merges `mcp` objects.
//!
//! Permissions are deliberately NOT adapted: OpenCode's native
//! `permission` config object maps tool names to actions
//! (`{"edit": "ask", "bash": {"*": "ask", "git status": "allow"}}` — see
//! <https://opencode.ai/docs/permissions/>). The generic `PermissionDecl`
//! allow/deny rule lists have no expressible shape in that schema: merging
//! `{allow, deny}` arrays under `permission` would write keys the runtime
//! never reads — a config that looks constraining but is not enforced.
//! So the surface is declared absent and permission declarations report
//! `Unsupported` (fail closed), exactly like other inexpressible semantics.
//!
//! [`Dialect::adapt`]: a4aa::Dialect::adapt

mod native;

use a4aa::card::{
    AgentCard, AgentSurface, AttachmentSupport, DetectSurface, FeatureSurface, HistoryRoot,
    InstructionChannel, LiveUsage, ModelCatalog, StoreKind, UsageSurface,
};
use a4aa::dialect::{
    CommandSurface, ExtensionKind, HookSurface, McpSurface, PermissionSurface, PluginSurface,
    SubagentSurface, Target,
};
use a4aa::{AdaptationPlan, AgentSpec, ArtifactScope, Dialect, HookEvent, MechanismSet};

pub use native::{native, NATIVE};

/// The `kilocode` dialect: Kilo plugin hooks, AGENTS.md, skills, command/agent dirs, kilo.json MCP.
/// No permission surface: see the crate-level note on OpenCode's per-tool
/// `permission` schema — the generic allow/deny lists are not expressible
/// in any form the runtime enforces.
pub const DIALECT: Dialect = Dialect {
    id: "kilocode",
    hooks: HookSurface::Extension {
        file: ".config/kilo/plugin/a4aa.js",
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
    skills_dir: Some(".kilo/skills"),
    commands: CommandSurface::MarkdownDir(".config/kilo/command"),
    subagents: SubagentSurface::MarkdownDir(".config/kilo/agents"),
    plugins: PluginSurface::ManifestFile {
        dir: ".config/kilo/plugin",
        extension: ".ts",
    },
    permissions: PermissionSurface::None,
    mcp: McpSurface::JsonMerge {
        file: ".config/kilo/kilo.json",
        scope: ArtifactScope::UserHome,
        key: "mcp",
        adapted: false,
    },
    native_profiles: false,
};

/// Adapt semantic declarations into the `kilocode` artifact plan. Equivalent
/// to `a4aa::adapt(&DIALECT, set)` — this free function is the entire public
/// surface most hosts need.
pub fn adapt(set: &MechanismSet) -> AdaptationPlan {
    DIALECT.adapt(set)
}

/// The descriptor itself, for hosts that compose over `&'static Dialect`
/// (registries, `a4aa-dialects`-style lookup tables).
pub const fn dialect() -> &'static Dialect {
    &DIALECT
}

/// The `kilo` agent card. `kilo acp` is a verified Agent Client Protocol
/// endpoint (the OpenCode-fork stock surface): `loadSession`, session
/// close/fork/list/resume, image and embedded-context prompts, MCP
/// http/sse. Its auth method is a terminal hint (`kilo auth login`), not a
/// key `authenticate` can carry.
pub const AGENT: AgentCard = AgentCard {
    id: "kilocode",
    label: "Kilo Code",
    dialect: &DIALECT,
    surface: AgentSurface::Acp {
        prefer_load: true,
        spawn_args: &["acp"],
        auth_key: None,
    },
    alternates: &[],
    detect: DetectSurface {
        binaries: &["kilo", "kilocode"],
        processes: &["kilo"],
        // The CLI ships inside the versioned VS Code extension
        // (`kilocode.kilo-code-<v>-<platform>/bin/kilo`); resolving the
        // latest extension directory is a host-side scan, not a literal
        // path a card may declare.
        binary_dirs: &[],
        config: &["{home}/.config/kilo/kilo.json"],
        detection: &["{home}/.config/kilo", "{home}/.local/share/kilo"],
        apps: &[],
    },
    features: FeatureSurface {
        instructions: InstructionChannel::Compose,
        execution_modes: &[],
        permission_modes: &[],
        permission_default: None,
        attachments: AttachmentSupport::None,
        // `--variant` carries reasoning effort.
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
            path: "{home}/.local/share/kilo",
            kind: "kilo-session-store",
            store: StoreKind::SqliteDb,
        },
        HistoryRoot {
            path: "{home}/.config/kilo",
            kind: "kilo-config",
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
        assert_eq!(plan.dialect, "kilocode");
        assert!(plan.artifacts.is_empty());
    }

    /// Evidence for the tightened permission surface: OpenCode/Kilo's native
    /// `permission` object maps tool names to `allow`/`ask`/`deny` actions
    /// (per-pattern records for `bash`/`edit`), so generic allow/deny rule
    /// lists have no enforced shape. The plan must report `Unsupported` and
    /// emit no `kilo.json` permission fragment that would merely look
    /// constraining; the enforced MCP merge surface still renders.
    #[test]
    fn permissions_fail_closed_and_mcp_still_merges() {
        let plan = adapt(&MechanismSet {
            permissions: Some(a4aa::PermissionDecl {
                mode: None,
                allow: vec!["Shell(ls)".into()],
                deny: vec!["Shell(rm)".into()],
                ask: vec![],
            }),
            mcp_servers: [(
                "local".to_owned(),
                serde_json::json!({"command": "fixture-mcp"}),
            )]
            .into_iter()
            .collect(),
            ..Default::default()
        });
        let entry = plan
            .report
            .iter()
            .find(|entry| entry.item == "permissions")
            .expect("permissions verdict");
        assert_eq!(entry.support, a4aa::Support::Unsupported);
        assert_eq!(
            entry.note.as_deref(),
            Some("no declarative permission surface for this dialect")
        );
        assert!(plan
            .artifacts
            .iter()
            .all(|artifact| !artifact.content.contains("permission")));
        let mcp = plan
            .artifacts
            .iter()
            .find(|artifact| artifact.path.ends_with("kilo.json"))
            .expect("kilo.json mcp merge");
        assert!(mcp.content.contains("fixture-mcp"));
        assert_eq!(
            plan.report
                .iter()
                .find(|entry| entry.item == "mcp-servers")
                .map(|entry| entry.support),
            Some(a4aa::Support::Native)
        );
    }
}
