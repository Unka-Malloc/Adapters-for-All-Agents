//! Native-capability table for `openclaw`. Last verified 2026-10-21.

use a4aa::native::{
    cell, AwpPrimitives, ConfigLocations, InteractionSurfaces, McpMechanism, McpRegistration,
    NativeCapability, NativeStatus,
};

/// Native surfaces and AwP primitives for OpenClaw.
pub const NATIVE: NativeCapability = NativeCapability {
    last_verified: "2026-10-21",
    surfaces: InteractionSurfaces {
        serve: cell(
            NativeStatus::Unadapted,
            "Gateway is a native hosted server; card drives exec/acp, not a Serve lane",
        ),
        sdk: cell(NativeStatus::NotOffered, "no official library surface"),
        acp: cell(
            NativeStatus::Native,
            "openclaw acp bridges ACP over a running Gateway",
        ),
        rpc: cell(
            NativeStatus::NotOffered,
            "Gateway protocol is not this card's rpc",
        ),
        cli: cell(
            NativeStatus::Native,
            "openclaw agent exec --json --thinking — primary, no Gateway required",
        ),
        pty: cell(NativeStatus::Native, "interactive openclaw TTY"),
    },
    mcp: McpRegistration {
        mechanism: McpMechanism::Unsupported,
        detail: "McpSurface::None",
    },
    config: ConfigLocations {
        settings: "{home}/.openclaw",
        hooks: "",
        permissions: "",
        model_catalog: "openclaw models",
    },
    primitives: AwpPrimitives {
        model_selection: cell(NativeStatus::Native, "--model; openclaw models"),
        effort: cell(NativeStatus::Native, "--thinking off|…|ultra"),
        session_open: cell(
            NativeStatus::Native,
            "exec returns sessionId; durable keys live on the Gateway",
        ),
        session_resume: cell(
            NativeStatus::NotOffered,
            "agent exec turns are ephemeral; Gateway session keys are remote",
        ),
        session_cancel: cell(NativeStatus::Native, "kill the exec process"),
        streaming: cell(
            NativeStatus::Native,
            "single-object --json envelope; ACP when Gateway is up",
        ),
        instruction_channel: cell(NativeStatus::Native, "compose into prompt"),
        permission_modes: cell(NativeStatus::NotOffered, "no permission selector"),
        skills: cell(NativeStatus::Native, ".openclaw/skills"),
        hooks: cell(NativeStatus::NotOffered, "HookSurface::None"),
        subagents: cell(NativeStatus::Native, ".openclaw/agents"),
        plugins: cell(NativeStatus::NotOffered, "PluginSurface::None"),
    },
};

/// The table, for hosts that compose over `&'static NativeCapability`.
pub const fn native() -> &'static NativeCapability {
    &NATIVE
}
