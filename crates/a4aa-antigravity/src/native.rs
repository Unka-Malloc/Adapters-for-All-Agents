//! Native-capability table for `antigravity`. Last verified 2026-10-21.

use a4aa::native::{
    cell, AwpPrimitives, ConfigLocations, InteractionSurfaces, McpMechanism, McpRegistration,
    NativeCapability, NativeStatus,
};

/// Native surfaces and AwP primitives for Antigravity (`agy`).
pub const NATIVE: NativeCapability = NativeCapability {
    last_verified: "2026-10-21",
    surfaces: InteractionSurfaces {
        serve: cell(NativeStatus::NotOffered, "no first-party HTTP serve"),
        sdk: cell(NativeStatus::NotOffered, "no official library surface"),
        acp: cell(
            NativeStatus::NotOffered,
            "agy_acp_server.par is an external plugin binary; dropped",
        ),
        rpc: cell(NativeStatus::NotOffered, "no host-declared JSON-RPC stdio"),
        cli: cell(
            NativeStatus::Native,
            "agy --print --output-format stream-json --model --effort --mode",
        ),
        pty: cell(
            NativeStatus::Native,
            "interactive agy TTY; LicoUp documents PTY --print/--conversation",
        ),
    },
    mcp: McpRegistration {
        mechanism: McpMechanism::ConfigKey,
        detail: ".gemini/config/mcp_config.json mcpServers (adapted merge)",
    },
    config: ConfigLocations {
        settings: "{home}/.gemini/antigravity-cli/settings.json",
        hooks: "{home}/.gemini/config/hooks.json",
        permissions: "",
        model_catalog: "agy models",
    },
    primitives: AwpPrimitives {
        model_selection: cell(NativeStatus::Native, "--model {model}"),
        effort: cell(NativeStatus::Native, "--effort {effort}"),
        session_open: cell(NativeStatus::Native, "first --print turn; conversation_id"),
        session_resume: cell(NativeStatus::Native, "--conversation {sessionId}"),
        session_cancel: cell(NativeStatus::Native, "kill the --print process"),
        streaming: cell(NativeStatus::Native, "stream-json NDJSON on stdout"),
        instruction_channel: cell(NativeStatus::Native, "compose into prompt"),
        permission_modes: cell(
            NativeStatus::NotOffered,
            "no per-request permission preset; --dangerously-skip-permissions is global",
        ),
        skills: cell(NativeStatus::Native, ".gemini/antigravity-cli/skills"),
        hooks: cell(
            NativeStatus::Native,
            "named bundle SessionStart/PreToolUse/…",
        ),
        subagents: cell(NativeStatus::Native, ".gemini/config/agents"),
        plugins: cell(NativeStatus::NotOffered, "PluginSurface::None"),
    },
};

/// The table, for hosts that compose over `&'static NativeCapability`.
pub const fn native() -> &'static NativeCapability {
    &NATIVE
}
