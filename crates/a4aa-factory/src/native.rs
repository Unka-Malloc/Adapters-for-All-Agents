//! Native-capability table for `factory`. Last verified 2026-10-21.

use a4aa::native::{
    cell, AwpPrimitives, ConfigLocations, InteractionSurfaces, McpMechanism, McpRegistration,
    NativeCapability, NativeStatus,
};

/// Native surfaces and AwP primitives for Factory Droid.
pub const NATIVE: NativeCapability = NativeCapability {
    last_verified: "2026-10-21",
    surfaces: InteractionSurfaces {
        serve: cell(
            NativeStatus::Native,
            "droid daemon --port is a first-party WebSocket server (rpc transport)",
        ),
        sdk: cell(NativeStatus::NotOffered, "no official library surface"),
        acp: cell(
            NativeStatus::Native,
            "droid exec --output-format acp is a first-party subcommand, not a plugin",
        ),
        rpc: cell(
            NativeStatus::Native,
            "stream-jsonrpc stdio and daemon WebSocket (droid.* / daemon.*)",
        ),
        cli: cell(NativeStatus::Native, "droid exec -o stream-json"),
        pty: cell(NativeStatus::Native, "interactive droid TTY"),
    },
    mcp: McpRegistration {
        mechanism: McpMechanism::ConfigKey,
        detail: ".factory/settings.json mcp (adapted merge)",
    },
    config: ConfigLocations {
        settings: "{home}/.factory/settings.json",
        hooks: "",
        permissions: "--auto autonomy low|medium|high",
        model_catalog: "droid.list_models passthrough; no catalog verb on CLI",
    },
    primitives: AwpPrimitives {
        model_selection: cell(NativeStatus::Native, "-m / modelId on session open"),
        effort: cell(NativeStatus::Native, "-r / reasoningEffort"),
        session_open: cell(
            NativeStatus::Native,
            "ACP session/new; droid.initialize_session",
        ),
        session_resume: cell(
            NativeStatus::Native,
            "ACP loadSession; droid.load_session; -s",
        ),
        session_cancel: cell(NativeStatus::Native, "droid.interrupt_session"),
        streaming: cell(
            NativeStatus::Native,
            "ACP session/update; assistant_text_delta",
        ),
        instruction_channel: cell(NativeStatus::Native, "compose into prompt"),
        permission_modes: cell(NativeStatus::Native, "autonomy low|medium|high"),
        skills: cell(NativeStatus::Native, ".factory/skills"),
        hooks: cell(NativeStatus::NotOffered, "HookSurface::None"),
        subagents: cell(NativeStatus::Native, ".factory/droids"),
        plugins: cell(NativeStatus::NotOffered, "PluginSurface::None"),
    },
};

/// The table, for hosts that compose over `&'static NativeCapability`.
pub const fn native() -> &'static NativeCapability {
    &NATIVE
}
