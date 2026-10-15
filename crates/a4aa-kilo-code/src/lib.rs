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
//! `~/.config/kilo/` and `.kilo/`, and `kilo.json` merges `mcp` and
//! `permission` objects.
//!
//! [`Dialect::adapt`]: a4aa::Dialect::adapt

use a4aa::dialect::{
    CommandSurface, ExtensionKind, HookSurface, McpSurface, PermissionSurface, PluginSurface,
    SubagentSurface, Target,
};
use a4aa::{AdaptationPlan, ArtifactScope, Dialect, HookEvent, MechanismSet};

/// The `kilocode` dialect: Kilo plugin hooks, AGENTS.md, skills, command/agent dirs, kilo.json MCP+permissions.
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
    permissions: PermissionSurface::JsonMerge {
        file: ".config/kilo/kilo.json",
        at: "permission",
        mode_key: None,
    },
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
}
