//! `a4aa-kimi-code` — the `kimicode` dialect descriptor for a4aa's mechanism
//! adaptation engine.
//!
//! One agent, one crate: this package owns only the `kimicode` mechanism
//! surface (hook store, skills/commands/subagents locations, permission and
//! MCP config shapes). It depends on `a4aa` core for the descriptor
//! vocabulary and the shared [`Dialect::adapt`] engine; it depends on no
//! other agent adapter. Install it alone when `kimicode` is the only agent a
//! host drives.
//!
//! [`Dialect::adapt`]: a4aa::Dialect::adapt

use a4aa::card::{
    AgentCard, AgentSurface, AlternateLane, AttachmentSupport, CliSurface, DetectSurface,
    FeatureSurface, HistoryRoot, InstructionChannel, LiveUsage, ModelCatalog, OsPath, StoreKind,
    UsageSurface,
};
use a4aa::dialect::{
    CommandSurface, HookSurface, McpSurface, PermissionSurface, PluginSurface, SubagentSurface,
    Target,
};
use a4aa::model::{BindingReceiptShape, PromptChannel, StreamMode};
use a4aa::{AdaptationPlan, AgentSpec, ArtifactScope, Dialect, MechanismSet};

/// The `kimicode` dialect: kimicode rules file plus shared SKILL.md skills.
pub const DIALECT: Dialect = Dialect {
    id: "kimicode",
    hooks: HookSurface::None,
    rules: Some(Target {
        scope: ArtifactScope::Project,
        path: "AGENTS.md",
    }),
    skills_dir: Some(".kimi-code/skills"),
    commands: CommandSurface::None,
    subagents: SubagentSurface::None,
    plugins: PluginSurface::None,
    permissions: PermissionSurface::None,
    mcp: McpSurface::None,
    native_profiles: false,
};

/// Adapt semantic declarations into the `kimicode` artifact plan. Equivalent to
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

/// The `kimi` agent card. `kimi acp` is a verified Agent Client Protocol
/// endpoint — the richest session surface on this catalog: `loadSession`
/// plus session list/resume/close/delete/fork/additionalDirectories, image
/// and embedded-context prompts, MCP http/sse. Its auth method is a
/// `_meta.terminal-auth` contract — the host spawns `kimi login` in a
/// terminal — not a key `authenticate` can carry, so `auth_key` is `None`.
pub const AGENT: AgentCard = AgentCard {
    id: "kimicode",
    label: "Kimi Code",
    dialect: &DIALECT,
    surface: AgentSurface::Acp {
        prefer_load: true,
        spawn_args: &["acp"],
        auth_key: None,
    },
    alternates: &[AlternateLane {
        // `kimi -p` print mode: one prompt non-interactively,
        // `--output-format stream-json` emits NDJSON, `-S` resumes a
        // session id the host already knows.
        surface: AgentSurface::Cli(CliSurface {
            args: &[
                "-p",
                "{prompt}",
                "--output-format",
                "stream-json",
                "--model",
                "{model}",
            ],
            prompt_channel: PromptChannel::ArgOnly,
            stream: StreamMode::Stdio,
            resume_spawn_args: &[],
            resume_args: &["-S", "{sessionId}"],
            stdin_template: None,
            json_lines: true,
            session_id_path: None,
            open_args: &[],
            binding_file_env: None,
            binding_receipt_shape: BindingReceiptShape::SessionId,
            result_selector: None,
            result_selector_field: None,
            result_path: None,
            usage_path: None,
            spawn_args: &[],
        }),
        binary: None,
    }],
    detect: DetectSurface {
        binaries: &["kimi"],
        processes: &["kimi"],
        binary_dirs: &[OsPath {
            path: "{home}/.kimi-code/bin",
            oses: &[],
        }],
        config: &["{home}/.kimi-code/config.toml"],
        detection: &["{home}/.kimi-code"],
        apps: &[],
    },
    features: FeatureSurface {
        instructions: InstructionChannel::Compose,
        execution_modes: &["plan"],
        // Permission presets are distinct flags (`--yolo`/`--auto`/`--plan`),
        // not a mode-valued selector — ACP mode negotiation carries them.
        permission_modes: &[],
        permission_default: None,
        attachments: AttachmentSupport::None,
        effort: false,
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
        path: "{home}/.kimi-code",
        kind: "kimi-session-store",
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
        assert_eq!(plan.dialect, "kimicode");
        assert!(plan.artifacts.is_empty());
    }

    #[test]
    fn spec_materializes_the_acp_lane() {
        let spec = spec();
        assert_eq!(spec.command.program, "kimi");
        assert_eq!(spec.command.args, vec!["acp"]);
        let a4aa::LaneSpec::Acp { prefer_load, .. } = spec.lane else {
            panic!("kimi drives the acp lane")
        };
        assert!(prefer_load);
    }
}
