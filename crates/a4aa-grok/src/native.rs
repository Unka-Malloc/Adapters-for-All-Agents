//! Native-capability table for `grok`. Last verified 2026-10-21.

use a4aa::native::{
    cell, AwpPrimitives, ConfigLocations, InteractionSurfaces, McpMechanism, McpRegistration,
    NativeCapability, NativeStatus,
};

/// Native surfaces and AwP primitives for Grok.
pub const NATIVE: NativeCapability = NativeCapability {
    last_verified: "2026-10-21",
    surfaces: InteractionSurfaces {
        serve: cell(NativeStatus::NotOffered, "no first-party HTTP serve"),
        sdk: cell(NativeStatus::NotOffered, "no official library surface"),
        acp: cell(NativeStatus::Native, "grok agent stdio — stock native ACP"),
        rpc: cell(NativeStatus::NotOffered, "ACP is the protocol surface"),
        cli: cell(
            NativeStatus::Native,
            "grok -p --output-format streaming-json",
        ),
        pty: cell(NativeStatus::Native, "interactive grok TTY"),
    },
    mcp: McpRegistration {
        mechanism: McpMechanism::Protocol,
        detail: "ACP mcp http/sse; no config-file MCP merge",
    },
    config: ConfigLocations {
        settings: "{home}/.grok/config.toml",
        hooks: "{home}/.grok/hooks",
        permissions: "--permission-mode values the CLI accepts",
        model_catalog: "grok models",
    },
    primitives: AwpPrimitives {
        model_selection: cell(NativeStatus::Native, "-m {model}; grok models"),
        effort: cell(NativeStatus::Native, "--effort / --reasoning-effort"),
        session_open: cell(
            NativeStatus::Native,
            "ACP session/new; loadSession advertised",
        ),
        session_resume: cell(NativeStatus::Native, "ACP session/load; CLI -r"),
        session_cancel: cell(
            NativeStatus::Native,
            "ACP session/cancel; x.ai/cancelRewind",
        ),
        streaming: cell(NativeStatus::Native, "ACP session/update"),
        instruction_channel: cell(NativeStatus::Native, "compose into prompt"),
        permission_modes: cell(
            NativeStatus::Native,
            "default|acceptEdits|auto|dontAsk|bypassPermissions|plan",
        ),
        skills: cell(NativeStatus::Native, ".grok/skills"),
        hooks: cell(NativeStatus::Native, "Claude-bundle per-file hooks"),
        subagents: cell(NativeStatus::Native, ".grok/agents"),
        plugins: cell(NativeStatus::NotOffered, "PluginSurface::None"),
    },
};

/// The table, for hosts that compose over `&'static NativeCapability`.
pub const fn native() -> &'static NativeCapability {
    &NATIVE
}
