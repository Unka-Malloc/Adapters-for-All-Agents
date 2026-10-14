//! `a4aa-gemini` — the `gemini` dialect descriptor for a4aa's mechanism
//! adaptation engine.
//!
//! One agent, one crate: this package owns only the `gemini` mechanism
//! surface (hook store, skills/commands/subagents locations, permission and
//! MCP config shapes). It depends on `a4aa` core for the descriptor
//! vocabulary and the shared [`Dialect::adapt`] engine; it depends on no
//! other agent adapter. Install it alone when `gemini` is the only agent a
//! host drives.
//!
//! [`Dialect::adapt`]: a4aa::Dialect::adapt

use a4aa::dialect::{
    CommandSurface, HookSurface, McpSurface, PermissionSurface, PluginSurface, SubagentSurface,
    Target,
};
use a4aa::{AdaptationPlan, ArtifactScope, Dialect, HookEvent, MechanismSet};

/// The `gemini` dialect: Gemini named-bundle hooks.json, GEMINI.md, skills, TOML commands, settings.json MCP.
pub const DIALECT: Dialect = Dialect {
    id: "gemini",
    hooks: HookSurface::NamedBundle {
        file: ".gemini/config/hooks.json",
        map: &[
            (HookEvent::SessionStart, "SessionStart"),
            (HookEvent::PreToolUse, "BeforeTool"),
            (HookEvent::PostToolUse, "PostToolUse"),
            (HookEvent::Stop, "Stop"),
        ],
    },
    rules: Some(Target {
        scope: ArtifactScope::Project,
        path: "GEMINI.md",
    }),
    skills_dir: Some(".gemini/skills"),
    commands: CommandSurface::TomlDir(".gemini/commands"),
    subagents: SubagentSurface::None,
    plugins: PluginSurface::None,
    permissions: PermissionSurface::None,
    mcp: McpSurface::JsonMerge {
        file: ".gemini/settings.json",
        scope: ArtifactScope::UserHome,
        key: "mcpServers",
        adapted: false,
    },
    native_profiles: false,
};

/// Adapt semantic declarations into the `gemini` artifact plan. Equivalent to
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
        assert_eq!(plan.dialect, "gemini");
        assert!(plan.artifacts.is_empty());
    }
}
