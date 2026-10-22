//! Native-capability table for `claude`. Last verified 2026-10-21.

use a4aa::native::{
    cell, AwpPrimitives, ConfigLocations, InteractionSurfaces, McpMechanism, McpRegistration,
    NativeCapability, NativeStatus,
};

/// Native surfaces and AwP primitives for Claude Code.
pub const NATIVE: NativeCapability = NativeCapability {
    last_verified: "2026-10-21",
    surfaces: InteractionSurfaces {
        serve: cell(NativeStatus::NotOffered, "no first-party HTTP serve"),
        sdk: cell(
            NativeStatus::NotOffered,
            "no official library surface in this card",
        ),
        acp: cell(
            NativeStatus::NotOffered,
            "vendored claude-agent-acp dropped; native CLI replaces it",
        ),
        rpc: cell(NativeStatus::NotOffered, "no host-declared JSON-RPC stdio"),
        cli: cell(
            NativeStatus::Native,
            "claude --print --input-format stream-json --output-format stream-json --resume",
        ),
        pty: cell(NativeStatus::Native, "interactive claude TTY"),
    },
    mcp: McpRegistration {
        mechanism: McpMechanism::ConfigKey,
        detail: ".mcp.json mcpServers (project, not adapted)",
    },
    config: ConfigLocations {
        settings: "{home}/.claude/settings.json",
        hooks: "{home}/.claude/settings.json hooks",
        permissions: "{home}/.claude/settings.json permissions.defaultMode",
        model_catalog: "",
    },
    primitives: AwpPrimitives {
        model_selection: cell(NativeStatus::Native, "--model {model}"),
        effort: cell(NativeStatus::Native, "--effort {effort}"),
        session_open: cell(
            NativeStatus::Native,
            "first stream-json turn; system/init session_id",
        ),
        session_resume: cell(NativeStatus::Native, "--resume {sessionId}"),
        session_cancel: cell(NativeStatus::Native, "kill the --print process"),
        streaming: cell(NativeStatus::Native, "stream-json partial messages"),
        instruction_channel: cell(NativeStatus::Native, "--append-system-prompt"),
        permission_modes: cell(
            NativeStatus::Native,
            "--permission-mode default|manual|acceptEdits|plan|auto|dontAsk|bypassPermissions",
        ),
        skills: cell(NativeStatus::Native, ".claude/skills"),
        hooks: cell(NativeStatus::Native, "Claude settings.json hooks"),
        subagents: cell(NativeStatus::Native, ".claude/agents"),
        plugins: cell(NativeStatus::NotOffered, "PluginSurface::None"),
    },
};

/// The table, for hosts that compose over `&'static NativeCapability`.
pub const fn native() -> &'static NativeCapability {
    &NATIVE
}
