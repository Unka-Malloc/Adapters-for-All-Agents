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

use a4aa::dialect::{
    CommandSurface, HookSurface, McpSurface, PermissionSurface, PluginSurface, SubagentSurface,
    Target,
};
use a4aa::{AdaptationPlan, ArtifactScope, Dialect, HookEvent, MechanismSet};

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
}
