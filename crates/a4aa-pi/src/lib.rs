//! `a4aa-pi` — the `pi` dialect descriptor for a4aa's mechanism
//! adaptation engine.
//!
//! One agent, one crate: this package owns only the `pi` mechanism
//! surface (hook store, skills/commands/subagents locations, permission and
//! MCP config shapes). It depends on `a4aa` core for the descriptor
//! vocabulary and the shared [`Dialect::adapt`] engine; it depends on no
//! other agent adapter. Install it alone when `pi` is the only agent a
//! host drives.
//!
//! [`Dialect::adapt`]: a4aa::Dialect::adapt

use a4aa::dialect::{
    CommandSurface, ExtensionKind, HookSurface, McpSurface, PermissionSurface, PluginSurface,
    SubagentSurface,
};
use a4aa::{AdaptationPlan, Dialect, HookEvent, MechanismSet};

/// The `pi` dialect: Pi TS extension hooks, skills, .ts extension plugins.
pub const DIALECT: Dialect = Dialect {
    id: "pi",
    hooks: HookSurface::Extension {
        file: ".pi/agent/extensions/a4aa.ts",
        kind: ExtensionKind::Pi,
        map: &[(HookEvent::SessionStart, "session_start")],
    },
    rules: None,
    skills_dir: Some(".pi/agent/skills"),
    commands: CommandSurface::None,
    subagents: SubagentSurface::None,
    plugins: PluginSurface::ManifestFile {
        dir: ".pi/agent/extensions",
        extension: ".ts",
    },
    permissions: PermissionSurface::None,
    mcp: McpSurface::None,
    native_profiles: false,
};

/// Adapt semantic declarations into the `pi` artifact plan. Equivalent to
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
        assert_eq!(plan.dialect, "pi");
        assert!(plan.artifacts.is_empty());
    }
}
