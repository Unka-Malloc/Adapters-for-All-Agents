//! Native-capability table for `cursor`. Last verified 2026-10-21.

use a4aa::native::{
    cell, AwpPrimitives, ConfigLocations, InteractionSurfaces, McpMechanism, McpRegistration,
    NativeCapability, NativeStatus,
};

/// Native surfaces and AwP primitives for Cursor Agent.
pub const NATIVE: NativeCapability = NativeCapability {
    last_verified: "2026-10-21",
    surfaces: InteractionSurfaces {
        serve: cell(
            NativeStatus::NotOffered,
            "no first-party HTTP serve on cursor-agent",
        ),
        sdk: cell(NativeStatus::NotOffered, "no official library surface"),
        acp: cell(
            NativeStatus::NotOffered,
            "cursor-agent has no acp subcommand",
        ),
        rpc: cell(NativeStatus::NotOffered, "no JSON-RPC stdio"),
        cli: cell(
            NativeStatus::Native,
            "cursor-agent --print --output-format stream-json; create-chat open",
        ),
        pty: cell(NativeStatus::Native, "StreamMode::Pty on the print lane"),
    },
    mcp: McpRegistration {
        mechanism: McpMechanism::ConfigKey,
        detail: ".cursor/mcp.json mcpServers",
    },
    config: ConfigLocations {
        settings: "{home}/.cursor/cli-config.json",
        hooks: "{home}/.cursor/hooks.json",
        permissions: "{home}/.cursor/cli-config.json permissions",
        model_catalog: "effort encoded in model id suffix",
    },
    primitives: AwpPrimitives {
        model_selection: cell(NativeStatus::Native, "--model {model}"),
        effort: cell(
            NativeStatus::Native,
            "model-id suffix (cursor-grok-4.6-high); effort_model_suffixes",
        ),
        session_open: cell(NativeStatus::Native, "create-chat"),
        session_resume: cell(NativeStatus::Native, "--resume {sessionId} on argv"),
        session_cancel: cell(NativeStatus::Native, "kill the print process"),
        streaming: cell(NativeStatus::Native, "stream-json NDJSON"),
        instruction_channel: cell(NativeStatus::Native, "compose into prompt"),
        permission_modes: cell(
            NativeStatus::NotOffered,
            "no per-turn permission selector on CLI",
        ),
        skills: cell(NativeStatus::Native, ".cursor/skills"),
        hooks: cell(NativeStatus::Native, "flat hooks.json beforeSubmitPrompt"),
        subagents: cell(NativeStatus::Native, ".cursor/agents"),
        plugins: cell(NativeStatus::NotOffered, "PluginSurface::None"),
    },
};

/// The table, for hosts that compose over `&'static NativeCapability`.
pub const fn native() -> &'static NativeCapability {
    &NATIVE
}
