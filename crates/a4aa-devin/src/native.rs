//! Native-capability table for `devin`. Last verified 2026-10-21.

use a4aa::native::{
    cell, AwpPrimitives, ConfigLocations, InteractionSurfaces, McpMechanism, McpRegistration,
    NativeCapability, NativeStatus,
};

/// Native surfaces and AwP primitives for Devin CLI.
pub const NATIVE: NativeCapability = NativeCapability {
    last_verified: "2026-10-21",
    surfaces: InteractionSurfaces {
        serve: cell(NativeStatus::NotOffered, "no first-party HTTP serve"),
        sdk: cell(NativeStatus::NotOffered, "no official library surface"),
        acp: cell(NativeStatus::Native, "devin acp — stock native subcommand"),
        rpc: cell(NativeStatus::NotOffered, "ACP is the protocol surface"),
        cli: cell(NativeStatus::Native, "devin -p / --print batch alternate"),
        pty: cell(NativeStatus::Native, "interactive Devin TTY"),
    },
    mcp: McpRegistration {
        mechanism: McpMechanism::ConfigKey,
        detail: ".config/devin/mcp_config.json mcpServers (adapted merge)",
    },
    config: ConfigLocations {
        settings: "{home}/.config/devin/config.json",
        hooks: "{home}/.config/devin/config.json hooks",
        permissions: "{home}/.config/devin/config.json permissions; --permission-mode",
        model_catalog: "devin models",
    },
    primitives: AwpPrimitives {
        model_selection: cell(NativeStatus::Native, "ACP negotiated; CLI --model"),
        effort: cell(NativeStatus::NotOffered, "features.effort is false"),
        session_open: cell(
            NativeStatus::Native,
            "ACP session/new; loadSession advertised",
        ),
        session_resume: cell(NativeStatus::Native, "ACP session/load; CLI -r"),
        session_cancel: cell(NativeStatus::Native, "ACP session/cancel"),
        streaming: cell(NativeStatus::Native, "ACP session/update"),
        instruction_channel: cell(NativeStatus::Native, "compose into prompt"),
        permission_modes: cell(NativeStatus::Native, "auto|accept-edits|smart|dangerous"),
        skills: cell(NativeStatus::Native, ".config/devin/skills"),
        hooks: cell(NativeStatus::Native, "Claude-shape config.json hooks"),
        subagents: cell(NativeStatus::Native, ".config/devin/agents"),
        plugins: cell(
            NativeStatus::NotOffered,
            "devin plugins install is imperative; no mergeable manifest",
        ),
    },
};

/// The table, for hosts that compose over `&'static NativeCapability`.
pub const fn native() -> &'static NativeCapability {
    &NATIVE
}
