//! `a4aa-hermes` — the `hermes` dialect descriptor for a4aa's mechanism
//! adaptation engine.
//!
//! One agent, one crate: this package owns only the `hermes` mechanism
//! surface (hook store, skills/commands/subagents locations, permission and
//! MCP config shapes). It depends on `a4aa` core for the descriptor
//! vocabulary and the shared [`Dialect::adapt`] engine; it depends on no
//! other agent adapter. Install it alone when `hermes` is the only agent a
//! host drives.
//!
//! [`Dialect::adapt`]: a4aa::Dialect::adapt

mod native;

use a4aa::card::{
    AgentCard, AgentSurface, AttachmentSupport, DetectSurface, FeatureSurface, HistoryRoot,
    InstructionChannel, LiveUsage, ModelCatalog, StoreKind, UsageSurface,
};
use a4aa::dialect::{
    CommandSurface, HookSurface, McpSurface, PermissionSurface, PluginSurface, SubagentSurface,
};
use a4aa::{AdaptationPlan, AgentSpec, Dialect, MechanismSet};

pub use native::{native, NATIVE};

/// The `hermes` dialect: Hermes skills and plugin.yaml manifests (event-bus hooks unmapped).
pub const DIALECT: Dialect = Dialect {
    id: "hermes",
    hooks: HookSurface::None,
    rules: None,
    skills_dir: Some(".hermes/skills"),
    commands: CommandSurface::None,
    subagents: SubagentSurface::None,
    plugins: PluginSurface::ManifestFile {
        dir: ".hermes/plugins",
        extension: ".yaml",
    },
    permissions: PermissionSurface::None,
    mcp: McpSurface::None,
    native_profiles: false,
};

/// Adapt semantic declarations into the `hermes` artifact plan. Equivalent to
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

/// The `hermes` agent card. `hermes acp` (also shipped as `hermes-acp`) is
/// a verified Agent Client Protocol endpoint answering `initialize` over
/// stdio. `loadSession` is not confirmed on this surface, so resume is
/// declared conservatively: an unresumable binding opens fresh rather than
/// guessing at Hermes's session naming. Authentication is terminal-side
/// (`hermes acp --setup`), never a key the host presents.
pub const AGENT: AgentCard = AgentCard {
    id: "hermes",
    label: "Hermes Agent",
    dialect: &DIALECT,
    surface: AgentSurface::Acp {
        prefer_load: false,
        spawn_args: &["acp"],
        auth_key: None,
    },
    alternates: &[],
    detect: DetectSurface {
        binaries: &["hermes"],
        processes: &["hermes"],
        binary_dirs: &[],
        config: &["{home}/.hermes/config.yaml"],
        detection: &["{home}/.hermes"],
        apps: &[],
    },
    features: FeatureSurface {
        instructions: InstructionChannel::Compose,
        execution_modes: &[],
        permission_modes: &[],
        permission_default: None,
        attachments: AttachmentSupport::None,
        // `--reasoning none|minimal|low|medium|high|xhigh|max|ultra`.
        effort: true,
        steer: false,
        models: ModelCatalog::None,
    },
    usage: UsageSurface {
        live: Some(LiveUsage {
            event: "session/update",
            path: "usage",
        }),
        transcript: None,
        hosted: None,
    },
    history: &[HistoryRoot {
        path: "{home}/.hermes",
        kind: "hermes-profile-store",
        store: StoreKind::Directory,
    }],
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
        assert_eq!(plan.dialect, "hermes");
        assert!(plan.artifacts.is_empty());
    }

    #[test]
    fn spec_materializes_the_acp_lane() {
        let spec = spec();
        assert_eq!(spec.command.program, "hermes");
        assert_eq!(spec.command.args, vec!["acp"]);
        let a4aa::LaneSpec::Acp { prefer_load, .. } = spec.lane else {
            panic!("hermes drives the acp lane")
        };
        assert!(!prefer_load);
    }
}
