//! `a4aa-factory` — the `factory` dialect descriptor for a4aa's mechanism
//! adaptation engine.
//!
//! One agent, one crate: this package owns only the `factory` mechanism
//! surface (hook store, skills/commands/subagents locations, permission and
//! MCP config shapes). It depends on `a4aa` core for the descriptor
//! vocabulary and the shared [`Dialect::adapt`] engine; it depends on no
//! other agent adapter. Install it alone when `factory` is the only agent a
//! host drives.
//!
//! [`Dialect::adapt`]: a4aa::Dialect::adapt

mod native;

use a4aa::card::{
    AcpAuthKey, AgentCard, AgentSurface, AlternateLane, AttachmentSupport, CliSurface,
    CompletionDecl, DetectSurface, EnvelopeDecl, FeatureSurface, HistoryRoot, InstructionChannel,
    LiveUsage, ModelCatalog, RpcCallDecl, RpcSurface, RpcTransportDecl, StoreKind, UsageSurface,
};
use a4aa::dialect::{
    CommandSurface, HookSurface, McpSurface, PermissionSurface, PluginSurface, SubagentSurface,
    Target,
};
use a4aa::model::{BindingReceiptShape, PromptChannel, StreamMode};
use a4aa::{AdaptationPlan, AgentSpec, ArtifactScope, Dialect, MechanismSet};

pub use native::{native, NATIVE};

/// The `factory` dialect: Factory Droid AGENTS.md, skills, commands, droid subagents, settings MCP.
pub const DIALECT: Dialect = Dialect {
    id: "factory",
    hooks: HookSurface::None,
    rules: Some(Target {
        scope: ArtifactScope::Project,
        path: "AGENTS.md",
    }),
    skills_dir: Some(".factory/skills"),
    commands: CommandSurface::MarkdownDir(".factory/commands"),
    subagents: SubagentSurface::MarkdownDir(".factory/droids"),
    plugins: PluginSurface::None,
    permissions: PermissionSurface::None,
    mcp: McpSurface::JsonMerge {
        file: ".factory/settings.json",
        scope: ArtifactScope::UserHome,
        key: "mcp",
        adapted: true,
    },
    native_profiles: false,
};

/// Adapt semantic declarations into the `factory` artifact plan. Equivalent to
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

/// The `droid` agent card (crate id `factory` names the vendor; the
/// installed binary is `droid`). `droid exec --output-format acp` is a
/// verified Agent Client Protocol endpoint: `initialize` reports
/// `loadSession`, session list/resume, image and embedded-context prompts.
/// Two auth methods are advertised — `device-pairing` (browser) and
/// `factory-api-key`; the latter takes the key a host supplies through
/// `FACTORY_API_KEY`.
///
/// Native alternates: `droid exec -o stream-json` batch CLI, the
/// `stream-jsonrpc` session protocol over stdio (`droid.*` methods), and
/// the hosted daemon over WebSocket (`droid daemon --port <n>`, `daemon.*`
/// methods, `daemon.authenticate` consumes a host-supplied
/// `FACTORY_API_KEY` or WorkOS `FACTORY_TOKEN` through `{secret:}`
/// placeholders). All three share Factory-tagged envelopes and
/// `agent_turn_completed` turn completion.
pub const AGENT: AgentCard = AgentCard {
    id: "factory",
    label: "Factory Droid",
    dialect: &DIALECT,
    surface: AgentSurface::Acp {
        prefer_load: true,
        spawn_args: &["exec", "--output-format", "acp"],
        auth_key: Some(AcpAuthKey::HostEnv("FACTORY_API_KEY")),
    },
    alternates: &[
        AlternateLane {
            // `droid exec -o stream-json` batch mode: NDJSON events —
            // `system.init` carries `session_id`, `-s` continues a known
            // session, `result` records carry the reply.
            surface: AgentSurface::Cli(CliSurface {
                args: &[
                    "-o",
                    "stream-json",
                    "-m",
                    "{model}",
                    "-r",
                    "{effort}",
                    "--auto",
                    "{permission}",
                    "{prompt}",
                ],
                prompt_channel: PromptChannel::ArgOnly,
                stream: StreamMode::Stdio,
                resume_spawn_args: &[],
                resume_args: &["-s", "{sessionId}"],
                stdin_template: None,
                json_lines: true,
                session_id_path: Some("session_id"),
                open_args: &[],
                binding_file_env: None,
                binding_receipt_shape: BindingReceiptShape::SessionId,
                result_selector: Some("result"),
                result_selector_field: Some("type"),
                result_path: Some("result"),
                usage_path: None,
                spawn_args: &["exec"],

                effort_model_suffixes: &[],
            }),
            binary: None,
        },
        AlternateLane {
            // `droid exec --input-format stream-jsonrpc -o stream-jsonrpc`:
            // the native session protocol (same `droid.*` method surface
            // the daemon speaks over its own transport). Frames carry
            // `type` + `factoryApiVersion`/`factoryProtocolVersion` tags;
            // `type` classifies inbound traffic and `method` names it.
            // `droid.initialize_session` opens (returns `sessionId`),
            // `droid.load_session` resumes a durable id, turns run
            // `droid.add_user_message` and end on the
            // `agent_turn_completed` session notification; text streams
            // via `assistant_text_delta`. `droid.interrupt_session`
            // cancels in place, `droid.close_session` tears down.
            surface: AgentSurface::Rpc(&RpcSurface {
                initialize: None,
                initialized: None,
                session_open: Some(RpcCallDecl {
                    method: "droid.initialize_session",
                    params: r#"{
                        "machineId": "a4aa",
                        "cwd": "{cwd}",
                        "modelId": "{model}",
                        "reasoningEffort": "{effort}",
                        "autonomyLevel": "{permission}",
                        "autoRejectPermissionRequests": true
                    }"#,
                }),
                session_resume: Some(RpcCallDecl {
                    method: "droid.load_session",
                    params: r#"{"sessionId": "{sessionId}"}"#,
                }),
                prompt: RpcCallDecl {
                    method: "droid.add_user_message",
                    params: r#"{"text": "{prompt}"}"#,
                },
                cancel: Some(RpcCallDecl {
                    method: "droid.interrupt_session",
                    params: "{}",
                }),
                session_close: Some(RpcCallDecl {
                    method: "droid.close_session",
                    params: "{}",
                }),
                text_event_method: Some("droid.session_notification"),
                text_path: Some("notification.textDelta"),
                completion: Some(CompletionDecl {
                    method: "droid.session_notification",
                    turn_id_path: None,
                    status_path: Some("notification.type"),
                    status_value: Some("agent_turn_completed"),
                }),
                result_text_path: None,
                pre_turn: &[],
                session_scope: a4aa::model::SessionScope::Agent,
                kill_on_cancel: false,
                envelope: Some(&EnvelopeDecl {
                    method_field: "method",
                    discriminant_field: Some("type"),
                    request_marker: Some("request"),
                    notification_marker: Some("notification"),
                    response_method: Some("response"),
                    string_ids: true,
                    params_inline: false,
                    data_field: None,
                    result_field: None,
                    error_field: None,
                    success_field: None,
                    command_field: None,
                    const_fields: &[
                        ("jsonrpc", "2.0"),
                        ("factoryApiVersion", "1.0.0"),
                        ("factoryProtocolVersion", "1.269.0"),
                    ],
                }),
                passthrough: &[
                    "droid.list_models",
                    "droid.update_session_settings",
                    "droid.fork_session",
                    "droid.get_context_stats",
                ],
                spawn_args: &[
                    "exec",
                    "--input-format",
                    "stream-jsonrpc",
                    "--output-format",
                    "stream-jsonrpc",
                ],
                transport: RpcTransportDecl::Stdio,
                max_frame_bytes: None,
                usage_path: None,
            }),
            binary: None,
        },
        AlternateLane {
            // `droid daemon --port <n>` — the hosted session server over
            // WebSocket. `daemon.authenticate` opens the connection (a
            // `FACTORY_API_KEY` or WorkOS `FACTORY_TOKEN` the host
            // supplies through its secret resolver); session-scoped calls
            // take `sessionId` explicitly. `daemon.load_session` resumes
            // a durable id; `daemon.session_notification` streams the
            // same notification family `droid.*` exec mode emits.
            surface: AgentSurface::Rpc(&RpcSurface {
                initialize: Some(RpcCallDecl {
                    method: "daemon.authenticate",
                    params: r#"{
                        "caller": "a4aa",
                        "apiKey": "{secret:FACTORY_API_KEY}",
                        "token": "{secret:FACTORY_TOKEN}"
                    }"#,
                }),
                initialized: None,
                session_open: Some(RpcCallDecl {
                    method: "daemon.initialize_session",
                    params: r#"{
                        "token": "{secret:FACTORY_TOKEN}",
                        "machineId": "a4aa",
                        "cwd": "{cwd}",
                        "modelId": "{model}",
                        "reasoningEffort": "{effort}",
                        "autonomyLevel": "{permission}",
                        "autoRejectPermissionRequests": true
                    }"#,
                }),
                session_resume: Some(RpcCallDecl {
                    method: "daemon.load_session",
                    params: r#"{
                        "token": "{secret:FACTORY_TOKEN}",
                        "sessionId": "{sessionId}"
                    }"#,
                }),
                prompt: RpcCallDecl {
                    method: "daemon.add_user_message",
                    params: r#"{"sessionId": "{sessionId}", "text": "{prompt}"}"#,
                },
                cancel: Some(RpcCallDecl {
                    method: "daemon.interrupt_session",
                    params: r#"{"sessionId": "{sessionId}"}"#,
                }),
                session_close: Some(RpcCallDecl {
                    method: "daemon.close_session",
                    params: r#"{"sessionId": "{sessionId}"}"#,
                }),
                text_event_method: Some("daemon.session_notification"),
                text_path: Some("notification.textDelta"),
                completion: Some(CompletionDecl {
                    method: "daemon.session_notification",
                    turn_id_path: None,
                    status_path: Some("notification.type"),
                    status_value: Some("agent_turn_completed"),
                }),
                result_text_path: None,
                pre_turn: &[],
                session_scope: a4aa::model::SessionScope::Agent,
                kill_on_cancel: false,
                envelope: Some(&EnvelopeDecl {
                    method_field: "method",
                    discriminant_field: Some("type"),
                    request_marker: Some("request"),
                    notification_marker: Some("notification"),
                    response_method: Some("response"),
                    string_ids: true,
                    params_inline: false,
                    data_field: None,
                    result_field: None,
                    error_field: None,
                    success_field: None,
                    command_field: None,
                    const_fields: &[
                        ("jsonrpc", "2.0"),
                        ("factoryApiVersion", "1.0.0"),
                        ("factoryProtocolVersion", "1.269.0"),
                    ],
                }),
                passthrough: &[
                    "daemon.list_available_sessions",
                    "daemon.search_sessions",
                    "daemon.list_models",
                    "daemon.archive_session",
                ],
                spawn_args: &["daemon", "--port", "{port}"],
                transport: RpcTransportDecl::WebSocket("WebSocket endpoint: "),
                max_frame_bytes: None,
                usage_path: None,
            }),
            binary: None,
        },
    ],
    detect: DetectSurface {
        binaries: &["droid"],
        processes: &["droid"],
        binary_dirs: &[],
        config: &["{home}/.factory/settings.json"],
        detection: &["{home}/.factory"],
        apps: &[],
    },
    features: FeatureSurface {
        instructions: InstructionChannel::Compose,
        // `--use-spec` selects droid's spec-driven execution track.
        execution_modes: &["spec"],
        // `--auto` autonomy tiers (the default is read-only).
        permission_modes: &["low", "medium", "high"],
        permission_default: None,
        attachments: AttachmentSupport::None,
        // `--reasoning-effort`.
        effort: true,
        steer: false,
        // No catalog verb: models come from the vendor's own naming.
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
    history: &[
        HistoryRoot {
            path: "{home}/.factory/sessions",
            kind: "factory-session-store",
            store: StoreKind::Directory,
        },
        HistoryRoot {
            path: "{home}/.factory",
            kind: "factory-config",
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

    #[test]
    fn dialect_reports_identity() {
        let set = MechanismSet::default();
        let plan = adapt(&set);
        assert_eq!(plan.dialect, "factory");
        assert!(plan.artifacts.is_empty());
    }

    #[test]
    fn spec_materializes_the_acp_lane() {
        let spec = spec();
        assert_eq!(spec.command.program, "droid");
        assert_eq!(spec.command.args, vec!["exec", "--output-format", "acp"]);
        let a4aa::LaneSpec::Acp {
            prefer_load,
            auth_key,
            ..
        } = spec.lane
        else {
            panic!("droid drives the acp lane")
        };
        assert!(prefer_load);
        assert_eq!(
            auth_key,
            Some(a4aa::EnvSource::HostEnv {
                host_env: "FACTORY_API_KEY".to_owned()
            })
        );
    }
}
