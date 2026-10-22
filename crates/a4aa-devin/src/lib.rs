//! `a4aa-devin` — the `devin` card and dialect descriptor for a4aa's
//! mechanism adaptation engine.
//!
//! One agent, one crate: this package owns the `devin` mechanism surface
//! (hook store, skills/commands/subagents locations, permission and MCP
//! config shapes) and the `devin` runtime card (ACP lane, detection,
//! selectors, history). It depends on `a4aa` core for the descriptor
//! vocabulary and the shared [`Dialect::adapt`] engine; it depends on no
//! other agent adapter. Install it alone when `devin` is the only agent a
//! host drives.
//!
//! [`Dialect::adapt`]: a4aa::Dialect::adapt

mod native;

use a4aa::card::{
    AcpAuthKey, AgentCard, AgentSurface, AlternateLane, AttachmentSupport, CliSurface,
    DetectSurface, FeatureSurface, HistoryRoot, InstructionChannel, LiveUsage, ModelCatalog,
    OsPath, StoreKind, UsageSurface,
};
use a4aa::dialect::{
    CommandSurface, HookSurface, McpSurface, PermissionSurface, PluginSurface, SubagentSurface,
    Target,
};
use a4aa::model::{BindingReceiptShape, PromptChannel, StreamMode};
use a4aa::{AdaptationPlan, AgentSpec, ArtifactScope, Dialect, HookEvent, MechanismSet};

pub use native::{native, NATIVE};

/// The `devin` dialect: Devin config.json hooks, AGENTS.md, skills,
/// skill-form commands, markdown subagents, `permissions`, mcp_config.
pub const DIALECT: Dialect = Dialect {
    id: "devin",
    // `~/.config/devin/config.json` `hooks` — the Claude-dialect store.
    // Devin fires eight events; `SubagentStart`, `SubagentStop` and
    // `Notification` have no native counterpart and report unsupported.
    hooks: HookSurface::Claude {
        file: ".config/devin/config.json",
        at: "hooks",
        events: &[
            HookEvent::SessionStart,
            HookEvent::SessionEnd,
            HookEvent::UserPromptSubmit,
            HookEvent::PreToolUse,
            HookEvent::PostToolUse,
            HookEvent::Stop,
            HookEvent::PermissionRequest,
            HookEvent::PostCompaction,
        ],
    },
    rules: Some(Target {
        scope: ArtifactScope::Project,
        path: "AGENTS.md",
    }),
    // User-level skills dir; `.devin/skills/` is the project-level twin.
    skills_dir: Some(".config/devin/skills"),
    // Devin's slash command *is* a skill: `/name` runs the pack's SKILL.md.
    commands: CommandSurface::SkillDir(".config/devin/skills"),
    // `agents/<name>.md` (or `<name>/AGENT.md`) with name/description/
    // model frontmatter; `.devin/agents/` is the project-level twin.
    subagents: SubagentSurface::MarkdownDir(".config/devin/agents"),
    // `devin plugins install` is imperative — there is no manifest file
    // the plan can merge into.
    plugins: PluginSurface::None,
    permissions: PermissionSurface::JsonMerge {
        file: ".config/devin/config.json",
        at: "permissions",
        // Modes are spawn-level (`--permission-mode`, DEVIN_PERMISSION_MODE),
        // not config keys — a declared mode reports unsupported.
        mode_key: None,
    },
    mcp: McpSurface::JsonMerge {
        file: ".config/devin/mcp_config.json",
        scope: ArtifactScope::UserHome,
        key: "mcpServers",
        adapted: true,
    },
    native_profiles: false,
};

/// Adapt semantic declarations into the `devin` artifact plan. Equivalent to
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

/// The `devin` agent card. `devin acp` runs a standard Agent Client
/// Protocol server over stdio — `initialize` reports `loadSession`,
/// `promptCapabilities` (image, embedded context), `mcpCapabilities`
/// (http, sse) and the `authenticate` method (the ACP host is the sole
/// credential source; local CLI credentials are not used). ACP is also the
/// protocol Devin's own REPL and `-p` mode drive internally, so it is the
/// canonical lane; hosts needing the `devin -p` batch surface build a Cli
/// spec themselves.
pub const AGENT: AgentCard = AgentCard {
    id: "devin",
    label: "Devin CLI",
    dialect: &DIALECT,
    surface: AgentSurface::Acp {
        prefer_load: true,
        spawn_args: &["acp"],
        // `devin acp` refuses session establishment until the host
        // authenticates: `_meta.api_key` on the `authenticate` request.
        // Local CLI credentials are intentionally not read in ACP mode —
        // the host supplies the key (here: a `DEVIN_API_KEY` host env).
        auth_key: Some(AcpAuthKey::HostEnv("DEVIN_API_KEY")),
    },
    alternates: &[AlternateLane {
        // `devin -p` non-interactive print mode: text on stdout, one
        // process per turn, `-r` resumes a host-known session id. Print
        // mode emits no session record, so bindings come only from the
        // host's own state; `--respect-workspace-trust false` is required
        // because print cannot answer the interactive trust prompt.
        surface: AgentSurface::Cli(CliSurface {
            args: &[
                "--print",
                "{prompt}",
                "--model",
                "{model}",
                "--permission-mode",
                "{permission}",
                "--respect-workspace-trust",
                "false",
            ],
            trusted_workspace_args: &[],
            prompt_channel: PromptChannel::ArgOnly,
            stream: StreamMode::Stdio,
            resume_spawn_args: &[],
            resume_args: &["-r", "{sessionId}"],
            stdin_template: None,
            json_lines: false,
            session_id_path: None,
            open_args: &[],
            binding_file_env: None,
            binding_receipt_shape: BindingReceiptShape::SessionId,
            result_selector: None,
            result_selector_field: None,
            result_path: None,
            usage_path: None,
            spawn_args: &[],
            effort_model_suffixes: &[],
        }),
        binary: None,
    }],
    detect: DetectSurface {
        binaries: &["devin"],
        processes: &["devin.exe", "devin", "Devin"],
        binary_dirs: &[
            OsPath {
                path: "/Applications/Devin.app/Contents/Resources/app/extensions/windsurf/devin/bin",
                oses: &["macos"],
            },
            OsPath {
                path: "{home}/Applications/Devin.app/Contents/Resources/app/extensions/windsurf/devin/bin",
                oses: &["macos"],
            },
        ],
        config: &[
            "{home}/.config/devin/config.json",
            "{home}/.config/devin/mcp_config.json",
            "{appdata}/devin/config.json",
        ],
        detection: &[
            "{home}/.config/devin",
            "{home}/.local/share/devin",
            "{xdg_config}/devin",
            "{xdg_data}/devin",
            "{appdata}/devin",
        ],
        apps: &[
            OsPath {
                path: "/Applications/Devin.app/Contents/MacOS/Devin",
                oses: &["macos"],
            },
            OsPath {
                path: "{home}/Applications/Devin.app/Contents/MacOS/Devin",
                oses: &["macos"],
            },
        ],
    },
    features: FeatureSurface {
        // No system-prompt flag or session param: guidance composes into
        // prompt text; standing rules arrive through AGENTS.md.
        instructions: InstructionChannel::Compose,
        // ACP session modes advertised at `session/new` that alter
        // execution rather than permission posture (`Code`/`Smart`/
        // `Bypass` are the permission-flavored ones on that surface).
        execution_modes: &["ask", "plan"],
        // `--permission-mode` / `DEVIN_PERMISSION_MODE` values the
        // installed CLI accepts; ACP exposes the same posture as session
        // modes (`accept-edits`, `smart`, `bypass`).
        permission_modes: &["auto", "accept-edits", "smart", "dangerous"],
        permission_default: Some("auto"),
        // Image prompts are protocol-native over ACP.
        attachments: AttachmentSupport::None,
        effort: false,
        steer: false,
        models: ModelCatalog::CliArgs(&["models"]),
    },
    usage: UsageSurface {
        // Standard ACP `session/update` usage field when reported.
        live: Some(LiveUsage {
            event: "session/update",
            path: "usage",
        }),
        transcript: None,
        hosted: None,
    },
    history: &[
        HistoryRoot {
            path: "{home}/.local/share/devin",
            kind: "devin-data-store",
            store: StoreKind::SqliteDb,
        },
        HistoryRoot {
            path: "{home}/.config/devin",
            kind: "devin-config",
            store: StoreKind::Directory,
        },
        HistoryRoot {
            path: "{xdg_data}/devin",
            kind: "devin-data-store",
            store: StoreKind::SqliteDb,
        },
        HistoryRoot {
            path: "{xdg_config}/devin",
            kind: "devin-config",
            store: StoreKind::Directory,
        },
        HistoryRoot {
            path: "{appdata}/devin",
            kind: "devin-data-store",
            store: StoreKind::Directory,
        },
    ],
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
    use a4aa::{HookDecl, HookIntent, PermissionDecl, SubagentDecl, Support};
    use std::path::Path;

    fn hook(event: HookEvent, command: &str) -> HookDecl {
        HookDecl {
            event,
            command: command.to_owned(),
            intent: HookIntent::default(),
            matcher: None,
            timeout: None,
        }
    }

    #[test]
    fn dialect_reports_identity() {
        let set = MechanismSet::default();
        let plan = adapt(&set);
        assert_eq!(plan.dialect, "devin");
        assert!(plan.artifacts.is_empty());
    }

    #[test]
    fn native_surfaces_cover_devins_eight_hook_events() {
        let set = MechanismSet {
            hooks: vec![
                hook(HookEvent::PreToolUse, "guard.sh"),
                hook(HookEvent::UserPromptSubmit, "inject.sh"),
                hook(HookEvent::PermissionRequest, "perm.sh"),
                hook(HookEvent::PostCompaction, "compact.sh"),
                hook(HookEvent::Notification, "notify.sh"),
            ],
            ..Default::default()
        };
        let plan = adapt(&set);
        for item in [
            "hooks.pre-tool-use",
            "hooks.user-prompt-submit",
            "hooks.permission-request",
            "hooks.post-compaction",
        ] {
            let entry = plan.report.iter().find(|e| e.item == item);
            assert!(
                matches!(entry, Some(e) if e.support == Support::Native),
                "{item}"
            );
        }
        let entry = plan
            .report
            .iter()
            .find(|e| e.item == "hooks.notification")
            .expect("notification entry");
        assert_eq!(entry.support, Support::Unsupported);
        let artifact = plan
            .artifacts
            .iter()
            .find(|a| a.path == Path::new(".config/devin/config.json"))
            .expect("hooks artifact");
        assert!(artifact.content.contains("PreToolUse"));
        assert!(artifact.content.contains("PostCompaction"));
    }

    #[test]
    fn skills_commands_subagents_permissions_materialize() {
        let set = MechanismSet {
            skills: vec![a4aa::SkillDecl {
                name: "pdf".to_owned(),
                content: "# pdf".to_owned(),
            }],
            commands: vec![a4aa::CommandDecl {
                name: "review".to_owned(),
                description: Some("Review the diff".to_owned()),
                content: "Run the review.".to_owned(),
            }],
            subagents: vec![SubagentDecl {
                name: "verifier".to_owned(),
                model: Some("opus".to_owned()),
                instructions: "Verify.".to_owned(),
                ..Default::default()
            }],
            permissions: Some(PermissionDecl {
                mode: Some("dangerous".to_owned()),
                allow: vec!["Exec(git)".to_owned()],
                deny: vec!["Exec(rm)".to_owned()],
                ask: vec!["Write(.env*)".to_owned()],
            }),
            ..Default::default()
        };
        let plan = adapt(&set);
        let has = |path: &str| plan.artifacts.iter().any(|a| a.path == Path::new(path));
        assert!(has(".config/devin/skills/pdf/SKILL.md"));
        assert!(has(".config/devin/skills/review/SKILL.md"));
        assert!(has(".config/devin/agents/verifier.md"));
        // A skill-form command stays user-invoked, never model-triggered.
        let command = plan
            .artifacts
            .iter()
            .find(|a| a.path == Path::new(".config/devin/skills/review/SKILL.md"))
            .expect("command artifact");
        assert!(command.content.contains("triggers:\n  - user"));
        // allow/deny/ask merge into config.json `permissions`; the
        // spawn-level mode reports unsupported rather than landing in a
        // key the agent never reads.
        let permissions = plan
            .artifacts
            .iter()
            .find(|a| {
                a.path == Path::new(".config/devin/config.json") && a.content.contains("\"allow\"")
            })
            .expect("permissions artifact");
        assert!(permissions.content.contains("\"ask\""));
        let mode = plan
            .report
            .iter()
            .find(|e| e.item == "permissions.mode")
            .expect("mode entry");
        assert_eq!(mode.support, Support::Unsupported);
    }
}

#[cfg(test)]
mod card_tests {
    use super::*;

    #[test]
    fn card_matches_dialect_identity() {
        assert_eq!(AGENT.id, DIALECT.id);
        assert!(std::ptr::eq(AGENT.dialect, &DIALECT));
    }

    #[test]
    fn spec_materializes_the_acp_lane() {
        let spec = spec();
        assert_eq!(spec.command.program, "devin");
        assert_eq!(spec.command.args, vec!["acp".to_string()]);
        let a4aa::LaneSpec::Acp {
            prefer_load,
            auth_key,
            ..
        } = spec.lane
        else {
            panic!("devin drives the acp lane")
        };
        assert!(prefer_load);
        // The ACP host is the sole credential source for `devin acp`; the
        // card declares the env indirection, never the key itself.
        assert_eq!(
            auth_key,
            Some(a4aa::EnvSource::HostEnv {
                host_env: "DEVIN_API_KEY".to_owned()
            })
        );
    }
}
