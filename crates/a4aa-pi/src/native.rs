//! Native-capability table for `pi`. Last verified 2026-10-21.

use a4aa::native::{
    cell, AwpPrimitives, ConfigLocations, InteractionSurfaces, McpMechanism, McpRegistration,
    NativeCapability, NativeStatus,
};

/// Native surfaces and AwP primitives for Pi.
pub const NATIVE: NativeCapability = NativeCapability {
    last_verified: "2026-10-21",
    surfaces: InteractionSurfaces {
        serve: cell(NativeStatus::NotOffered, "no first-party HTTP serve"),
        sdk: cell(NativeStatus::NotOffered, "no official library surface"),
        acp: cell(
            NativeStatus::NotOffered,
            "custom pi-acp adapter retired; native RPC replaces it",
        ),
        rpc: cell(
            NativeStatus::Native,
            "pi --mode rpc: flat type/id envelope, get_state/prompt/abort",
        ),
        cli: cell(
            NativeStatus::Native,
            "pi --print --model --thinking; --session resume",
        ),
        pty: cell(
            NativeStatus::Native,
            "interactive TTY is the default bare pi",
        ),
    },
    mcp: McpRegistration {
        mechanism: McpMechanism::Unsupported,
        detail: "McpSurface::None — Pi has no MCP config key",
    },
    config: ConfigLocations {
        settings: "{home}/.pi/agent/settings.json",
        hooks: "{home}/.pi/agent/extensions/*.ts",
        permissions: "",
        model_catalog: "rpc get_available_models",
    },
    primitives: AwpPrimitives {
        model_selection: cell(
            NativeStatus::Native,
            "set_model {provider,modelId}; CLI --model provider/id",
        ),
        effort: cell(
            NativeStatus::Native,
            "set_thinking_level {level}; CLI --thinking off|…|max",
        ),
        session_open: cell(NativeStatus::Native, "get_state returns data.sessionId"),
        session_resume: cell(NativeStatus::Native, "switch_session {sessionPath}"),
        session_cancel: cell(NativeStatus::Native, "abort; process survives for resume"),
        streaming: cell(
            NativeStatus::Native,
            "message_update assistantMessageEvent.delta",
        ),
        instruction_channel: cell(NativeStatus::Native, "compose into prompt"),
        permission_modes: cell(NativeStatus::NotOffered, "no permission selector"),
        skills: cell(NativeStatus::Native, ".pi/agent/skills"),
        hooks: cell(NativeStatus::Native, "Pi TS extensions, session_start"),
        subagents: cell(NativeStatus::NotOffered, "SubagentSurface::None"),
        plugins: cell(NativeStatus::Native, ".pi/agent/extensions/*.ts"),
    },
};

/// The table, for hosts that compose over `&'static NativeCapability`.
pub const fn native() -> &'static NativeCapability {
    &NATIVE
}
