//! Native-capability table for `kimicode`. Last verified 2026-10-21.

use a4aa::native::{
    cell, AwpPrimitives, ConfigLocations, InteractionSurfaces, McpMechanism, McpRegistration,
    NativeCapability, NativeStatus,
};

/// Native surfaces and AwP primitives for Kimi Code.
pub const NATIVE: NativeCapability = NativeCapability {
    last_verified: "2026-10-21",
    surfaces: InteractionSurfaces {
        serve: cell(NativeStatus::NotOffered, "no first-party HTTP serve"),
        sdk: cell(NativeStatus::NotOffered, "no official library surface"),
        acp: cell(NativeStatus::Native, "kimi acp — stock native subcommand"),
        rpc: cell(NativeStatus::NotOffered, "ACP is the protocol surface"),
        cli: cell(
            NativeStatus::Native,
            "kimi -p --output-format stream-json; -S resume",
        ),
        pty: cell(NativeStatus::Native, "interactive kimi TTY"),
    },
    mcp: McpRegistration {
        mechanism: McpMechanism::Protocol,
        detail: "ACP mcp http/sse; no config-file MCP merge",
    },
    config: ConfigLocations {
        settings: "{home}/.kimi-code/config.toml",
        hooks: "",
        permissions: "distinct flags --yolo/--auto/--plan, not a mode selector",
        model_catalog: "",
    },
    primitives: AwpPrimitives {
        model_selection: cell(NativeStatus::Native, "CLI --model; ACP negotiated"),
        effort: cell(NativeStatus::NotOffered, "features.effort is false"),
        session_open: cell(
            NativeStatus::Native,
            "ACP session/new; loadSession advertised",
        ),
        session_resume: cell(NativeStatus::Native, "ACP session/load; CLI -S"),
        session_cancel: cell(NativeStatus::Native, "ACP session/cancel"),
        streaming: cell(NativeStatus::Native, "ACP session/update"),
        instruction_channel: cell(NativeStatus::Native, "compose into prompt"),
        permission_modes: cell(
            NativeStatus::NotOffered,
            "flags, not a mode-valued selector",
        ),
        skills: cell(NativeStatus::Native, ".kimi-code/skills"),
        hooks: cell(NativeStatus::NotOffered, "HookSurface::None"),
        subagents: cell(NativeStatus::NotOffered, "SubagentSurface::None"),
        plugins: cell(NativeStatus::NotOffered, "PluginSurface::None"),
    },
};

/// The table, for hosts that compose over `&'static NativeCapability`.
pub const fn native() -> &'static NativeCapability {
    &NATIVE
}
