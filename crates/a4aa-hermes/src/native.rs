//! Native-capability table for `hermes`. Last verified 2026-10-21.

use a4aa::native::{
    cell, AwpPrimitives, ConfigLocations, InteractionSurfaces, McpMechanism, McpRegistration,
    NativeCapability, NativeStatus,
};

/// Native surfaces and AwP primitives for Hermes.
pub const NATIVE: NativeCapability = NativeCapability {
    last_verified: "2026-10-21",
    surfaces: InteractionSurfaces {
        serve: cell(NativeStatus::NotOffered, "no first-party HTTP serve"),
        sdk: cell(NativeStatus::NotOffered, "no official library surface"),
        acp: cell(NativeStatus::Native, "hermes acp — stock native subcommand"),
        rpc: cell(NativeStatus::NotOffered, "ACP is the protocol surface"),
        cli: cell(NativeStatus::NotOffered, "no headless print lane declared"),
        pty: cell(NativeStatus::Native, "interactive hermes TTY"),
    },
    mcp: McpRegistration {
        mechanism: McpMechanism::Unsupported,
        detail: "McpSurface::None; HERMES_ACP_SKIP_CONFIGURED_MCP is a host env",
    },
    config: ConfigLocations {
        settings: "{home}/.hermes/config.yaml",
        hooks: "",
        permissions: "",
        model_catalog: "",
    },
    primitives: AwpPrimitives {
        model_selection: cell(NativeStatus::NotOffered, "ModelCatalog::None"),
        effort: cell(NativeStatus::Native, "--reasoning none|…|ultra"),
        session_open: cell(NativeStatus::Native, "ACP session/new"),
        session_resume: cell(
            NativeStatus::NotOffered,
            "loadSession not confirmed; unresumable binding opens fresh",
        ),
        session_cancel: cell(NativeStatus::Native, "ACP session/cancel"),
        streaming: cell(NativeStatus::Native, "ACP session/update"),
        instruction_channel: cell(NativeStatus::Native, "compose into prompt"),
        permission_modes: cell(NativeStatus::NotOffered, "no permission selector"),
        skills: cell(NativeStatus::Native, ".hermes/skills"),
        hooks: cell(NativeStatus::NotOffered, "event-bus hooks unmapped"),
        subagents: cell(NativeStatus::NotOffered, "SubagentSurface::None"),
        plugins: cell(NativeStatus::Native, ".hermes/plugins/*.yaml"),
    },
};

/// The table, for hosts that compose over `&'static NativeCapability`.
pub const fn native() -> &'static NativeCapability {
    &NATIVE
}
