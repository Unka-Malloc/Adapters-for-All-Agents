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

use a4aa::dialect::{
    CommandSurface, ExtensionKind, HookSurface, McpSurface, PermissionSurface, PluginSurface,
    SubagentSurface, Target,
};
use a4aa::{AdaptationPlan, ArtifactScope, Dialect, HookEvent, MechanismSet};

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
