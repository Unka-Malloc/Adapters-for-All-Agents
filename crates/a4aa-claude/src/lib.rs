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

use a4aa::dialect::{
    CommandSurface, HookSurface, McpSurface, PermissionSurface, PluginSurface, SubagentSurface,
    Target,
};
use a4aa::{AdaptationPlan, ArtifactScope, Dialect, HookEvent, MechanismSet};

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
}
