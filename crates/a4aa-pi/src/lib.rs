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

use a4aa::card::{
    AgentCard, AgentSurface, AttachmentSupport, CompletionDecl, DetectSurface, EnvelopeDecl,
    FeatureSurface, HistoryRoot, InstructionChannel, ModelCatalog, RpcCallDecl, RpcSurface,
    RpcTransportDecl, StoreKind, UsageSurface,
};
use a4aa::dialect::{
    CommandSurface, ExtensionKind, HookSurface, McpSurface, PermissionSurface, PluginSurface,
    SubagentSurface,
};
use a4aa::{AdaptationPlan, AgentSpec, Dialect, HookEvent, MechanismSet, SessionScope};

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

/// The `pi` agent card: a flat command envelope over stdio.
///
/// Protocol: frames are
/// `{type,id,...inline fields}` — **not** JSON-RPC 2.0 — with responses
/// `{type:"response",command,success,id,data|error}` and events carrying
/// their payload under `data`. A fresh open is `get_state` (its
/// `data.sessionId` becomes the binding); resume is
/// `switch_session {sessionPath}` — the resume placeholder carries the
/// session *file path* the host resolved from its own id (pi's session
/// store is `~/.pi/agent/sessions`). Each turn runs `set_model` and
/// `set_thinking_level` before `prompt`; text streams on `message_update`
/// and the turn ends at `agent_settled`. Cancellation is process
/// teardown: pi has no cancel command.
pub const AGENT: AgentCard = AgentCard {
    id: "pi",
    label: "Pi",
    dialect: &DIALECT,
    surface: AgentSurface::Rpc(&RpcSurface {
        initialize: None,
        initialized: None,
        session_open: Some(RpcCallDecl {
            method: "get_state",
            params: "{}",
        }),
        // `{sessionId}` is the session file path the host resolved from
        // its own binding — pi resumes by path, not by id.
        session_resume: Some(RpcCallDecl {
            method: "switch_session",
            params: r#"{"sessionPath": "{sessionId}"}"#,
        }),
        prompt: RpcCallDecl {
            method: "prompt",
            params: r#"{"message": "{prompt}"}"#,
        },
        // `{type:"abort"}` cancels the turn in-band — the session survives
        // for resume.
        cancel: Some(RpcCallDecl {
            method: "abort",
            params: "{}",
        }),
        session_close: None,
        text_event_method: Some("message_update"),
        text_path: Some("assistantMessageEvent.delta"),
        completion: Some(CompletionDecl {
            method: "agent_settled",
            turn_id_path: None,
            status_path: None,
            status_value: None,
        }),
        result_text_path: None,
        // Per-turn configuration: `{model}` must arrive qualified as
        // `provider/model` — pi's `set_model` addresses the two halves.
        pre_turn: &[
            RpcCallDecl {
                method: "set_model",
                params: r#"{"provider": "{modelProvider}", "modelId": "{modelId}"}"#,
            },
            RpcCallDecl {
                method: "set_thinking_level",
                params: r#"{"level": "{effort}"}"#,
            },
        ],
        session_scope: SessionScope::Agent,
        kill_on_cancel: false,
        envelope: Some(&EnvelopeDecl {
            method_field: "type",
            string_ids: true,
            params_inline: true,
            response_method: Some("response"),
            data_field: Some("data"),
            result_field: Some("data"),
            error_field: Some("error"),
            success_field: Some("success"),
            command_field: Some("command"),
            discriminant_field: None,
            request_marker: None,
            notification_marker: None,
            const_fields: &[],
        }),
        passthrough: &[
            "get_state",
            "get_available_models",
            "steer",
            "get_last_assistant_text",
            "switch_session",
        ],
        // `--mode rpc` selects the headless JSON protocol — a bare `pi`
        // on piped stdio falls back to one-shot print mode and never
        // answers `get_state`.
        spawn_args: &["--mode", "rpc"],
        transport: RpcTransportDecl::Stdio,
    }),
    alternates: &[],
    detect: DetectSurface {
        binaries: &["pi"],
        processes: &["pi.exe", "pi"],
        binary_dirs: &[],
        config: &["{home}/.pi/agent/settings.json"],
        detection: &[
            "{home}/.pi/agent/settings.json",
            "{home}/.pi/agent/sessions",
        ],
        apps: &[],
    },
    features: FeatureSurface {
        instructions: InstructionChannel::Compose,
        execution_modes: &[],
        permission_modes: &[],
        permission_default: None,
        attachments: AttachmentSupport::None,
        effort: true,
        steer: true,
        models: ModelCatalog::RpcMethod("get_available_models"),
    },
    usage: UsageSurface {
        live: None,
        transcript: None,
        hosted: None,
    },
    history: &[HistoryRoot {
        path: "{home}/.pi/agent/sessions",
        kind: "pi-session-store",
        store: StoreKind::JsonlDir,
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
        assert_eq!(plan.dialect, "pi");
        assert!(plan.artifacts.is_empty());
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
    fn spec_materializes_the_flat_envelope_lane() {
        let spec = spec();
        assert_eq!(spec.command.program, "pi");
        let a4aa::LaneSpec::Rpc(rpc) = spec.lane else {
            panic!("pi drives the rpc lane")
        };
        assert_eq!(rpc.session_open.as_ref().unwrap().method, "get_state");
        assert_eq!(
            rpc.session_resume.as_ref().unwrap().method,
            "switch_session"
        );
        assert_eq!(rpc.prompt.method, "prompt");
        assert_eq!(rpc.completion.as_ref().unwrap().method, "agent_settled");
        // `{type:"abort"}` cancels in-band — the process survives for the
        // next turn or a `switch_session` resume.
        assert!(!rpc.kill_on_cancel);
        assert_eq!(rpc.cancel.as_ref().unwrap().method, "abort");
        let envelope = rpc.envelope.as_ref().expect("flat envelope");
        assert_eq!(envelope.method_field, "type");
        assert!(envelope.string_ids);
        assert!(envelope.params_inline);
        assert_eq!(envelope.response_method.as_deref(), Some("response"));
        assert_eq!(
            rpc.pre_turn
                .iter()
                .map(|c| c.method.as_str())
                .collect::<Vec<_>>(),
            vec!["set_model", "set_thinking_level"]
        );
        assert!(rpc.passthrough.iter().any(|m| m == "steer"));
        assert!(rpc.capabilities().session_resume);
        assert!(rpc.capabilities().cancellation);
    }
}
