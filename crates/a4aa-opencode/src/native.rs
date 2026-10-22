//! Native-capability table for `opencode`. Last verified 2026-10-21.

use a4aa::native::{
    cell, AwpPrimitives, ConfigLocations, InteractionSurfaces, McpMechanism, McpRegistration,
    NativeCapability, NativeStatus,
};

/// Native surfaces and AwP primitives for OpenCode.
pub const NATIVE: NativeCapability = NativeCapability {
    last_verified: "2026-10-21",
    surfaces: InteractionSurfaces {
        serve: cell(
            NativeStatus::Native,
            "opencode serve --port 0 HTTP loopback",
        ),
        sdk: cell(NativeStatus::NotOffered, "no official library surface"),
        acp: cell(NativeStatus::Native, "opencode acp ordered alternate"),
        rpc: cell(NativeStatus::NotOffered, "serve REST, not JSON-RPC stdio"),
        cli: cell(NativeStatus::NotOffered, "no headless print lane declared"),
        pty: cell(NativeStatus::Native, "interactive opencode TTY"),
    },
    mcp: McpRegistration {
        mechanism: McpMechanism::ConfigKey,
        detail: ".config/opencode/opencode.json mcp (adapted merge)",
    },
    config: ConfigLocations {
        settings: "{home}/.config/opencode/opencode.jsonc",
        hooks: "{home}/.config/opencode/plugins/a4aa.js",
        permissions: "",
        model_catalog: "",
    },
    primitives: AwpPrimitives {
        model_selection: cell(NativeStatus::Native, "serve model endpoint; ACP negotiated"),
        effort: cell(NativeStatus::Native, "features.effort is true"),
        session_open: cell(
            NativeStatus::Native,
            "serve session create; ACP session/new",
        ),
        session_resume: cell(NativeStatus::Native, "ACP session/load"),
        session_cancel: cell(NativeStatus::Native, "ACP session/cancel"),
        streaming: cell(NativeStatus::Native, "serve REST; ACP session/update"),
        instruction_channel: cell(NativeStatus::Native, "compose into prompt"),
        permission_modes: cell(NativeStatus::Native, "protocol-native permission requests"),
        skills: cell(NativeStatus::Native, ".opencode/skills"),
        hooks: cell(NativeStatus::Native, "OpenCode JS plugin events"),
        subagents: cell(NativeStatus::Native, ".config/opencode/agents"),
        plugins: cell(
            NativeStatus::NotOffered,
            "PluginSurface::None; hooks ride Extension",
        ),
    },
};

/// The table, for hosts that compose over `&'static NativeCapability`.
pub const fn native() -> &'static NativeCapability {
    &NATIVE
}
