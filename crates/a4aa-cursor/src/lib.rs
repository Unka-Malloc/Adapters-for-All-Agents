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

use a4aa::dialect::{
    CommandSurface, HookSurface, McpSurface, PermissionSurface, PluginSurface, SubagentSurface,
    Target,
};
use a4aa::{AdaptationPlan, ArtifactScope, Dialect, HookEvent, MechanismSet};

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
