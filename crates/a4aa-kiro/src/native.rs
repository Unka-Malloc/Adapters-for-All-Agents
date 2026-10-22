//! Native-capability table for `kiro`. Last verified 2026-10-21.
//!
//! `kiro-cli-chat serve` WebSocket is a third native surface not adapted.

use a4aa::native::{
    cell, AwpPrimitives, ConfigLocations, InteractionSurfaces, McpMechanism, McpRegistration,
    NativeCapability, NativeStatus,
};

/// Native surfaces and AwP primitives for Kiro CLI.
pub const NATIVE: NativeCapability = NativeCapability {
    last_verified: "2026-10-21",
    surfaces: InteractionSurfaces {
        serve: cell(
            NativeStatus::Unadapted,
            "kiro-cli-chat serve WebSocket is native; not on this card",
        ),
        sdk: cell(NativeStatus::NotOffered, "no official library surface"),
        acp: cell(
            NativeStatus::Native,
            "kiro-cli acp delegates to bundled kiro-cli-chat ACP",
        ),
        rpc: cell(
            NativeStatus::Unadapted,
            "serve WebSocket would be an rpc/serve transport; not adapted",
        ),
        cli: cell(
            NativeStatus::Native,
            "kiro-cli-chat chat --output-format stream-json --resume-id",
        ),
        pty: cell(NativeStatus::Native, "interactive kiro-cli TTY"),
    },
    mcp: McpRegistration {
        mechanism: McpMechanism::ConfigKey,
        detail: ".kiro/settings/mcp.json mcpServers (adapted merge)",
    },
    config: ConfigLocations {
        settings: "{home}/.kiro",
        hooks: "IDE trigger hooks unmapped",
        permissions: "",
        model_catalog: "kiro-cli-chat chat --list-models",
    },
    primitives: AwpPrimitives {
        model_selection: cell(NativeStatus::Native, "--model; --list-models"),
        effort: cell(NativeStatus::Native, "--effort low|medium|high|xhigh|max"),
        session_open: cell(NativeStatus::Native, "ACP session/new"),
        session_resume: cell(NativeStatus::Native, "ACP session/load; CLI --resume-id"),
        session_cancel: cell(NativeStatus::Native, "ACP session/cancel"),
        streaming: cell(NativeStatus::Native, "ACP session/update; CLI stream-json"),
        instruction_channel: cell(NativeStatus::Native, "compose into prompt"),
        permission_modes: cell(NativeStatus::NotOffered, "no permission selector"),
        skills: cell(NativeStatus::Native, ".kiro/skills"),
        hooks: cell(NativeStatus::NotOffered, "IDE trigger hooks unmapped"),
        subagents: cell(NativeStatus::Native, ".kiro/agents JSON"),
        plugins: cell(NativeStatus::NotOffered, "PluginSurface::None"),
    },
};

/// The table, for hosts that compose over `&'static NativeCapability`.
pub const fn native() -> &'static NativeCapability {
    &NATIVE
}
