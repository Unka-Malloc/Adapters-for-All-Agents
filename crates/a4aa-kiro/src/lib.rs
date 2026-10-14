//! `a4aa-kiro` — the `kiro` dialect descriptor for a4aa's mechanism
//! adaptation engine.
//!
//! One agent, one crate: this package owns only the `kiro` mechanism
//! surface (hook store, skills/commands/subagents locations, permission and
//! MCP config shapes). It depends on `a4aa` core for the descriptor
//! vocabulary and the shared [`Dialect::adapt`] engine; it depends on no
//! other agent adapter. Install it alone when `kiro` is the only agent a
//! host drives.
//!
//! [`Dialect::adapt`]: a4aa::Dialect::adapt

use a4aa::dialect::{
    CommandSurface, HookSurface, McpSurface, PermissionSurface, PluginSurface, SubagentSurface,
    Target,
};
use a4aa::{AdaptationPlan, ArtifactScope, Dialect, MechanismSet};

/// The `kiro` dialect: Kiro steering rules, skills, JSON agents, mcp.json (IDE trigger hooks unmapped).
pub const DIALECT: Dialect = Dialect {
    id: "kiro",
    hooks: HookSurface::None,
    rules: Some(Target {
        scope: ArtifactScope::Project,
        path: ".kiro/steering/a4aa-rules.md",
    }),
    skills_dir: Some(".kiro/skills"),
    commands: CommandSurface::None,
    subagents: SubagentSurface::KiroJson(".kiro/agents"),
    plugins: PluginSurface::None,
    permissions: PermissionSurface::None,
    mcp: McpSurface::JsonMerge {
        file: ".kiro/settings/mcp.json",
        scope: ArtifactScope::UserHome,
        key: "mcpServers",
        adapted: true,
    },
    native_profiles: false,
};

/// Adapt semantic declarations into the `kiro` artifact plan. Equivalent to
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
        assert_eq!(plan.dialect, "kiro");
        assert!(plan.artifacts.is_empty());
    }
}
