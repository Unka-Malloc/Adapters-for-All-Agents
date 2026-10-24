//! Native-capability table for `codex`. Last verified 2026-10-21.

use a4aa::native::{
    cell, AwpPrimitives, ConfigLocations, InteractionSurfaces, McpMechanism, McpRegistration,
    NativeCapability, NativeStatus,
};

/// Native surfaces and AwP primitives for Codex.
pub const NATIVE: NativeCapability = NativeCapability {
    last_verified: "2026-10-24",
    surfaces: InteractionSurfaces {
        serve: cell(NativeStatus::NotOffered, "no first-party HTTP serve"),
        sdk: cell(
            NativeStatus::NotOffered,
            "app-server is rpc, not a library SDK",
        ),
        acp: cell(
            NativeStatus::NotOffered,
            "vendored codex-acp dropped; native app-server replaces it",
        ),
        rpc: cell(
            NativeStatus::Native,
            "codex app-server --stdio: -c MCP registration, initialize, thread/start|resume, turn/start",
        ),
        cli: cell(NativeStatus::Native, "codex exec --json batch alternate"),
        pty: cell(NativeStatus::Native, "interactive codex TTY"),
    },
    mcp: McpRegistration {
        mechanism: McpMechanism::ConfigKey,
        detail: "codex app-server -c mcp_servers.<name>.{command,args,env_vars}; default_tools_approval_mode only for mcpConfig.toolsApprovalServers; ~/.codex/config.toml [mcp_servers]",
    },
    config: ConfigLocations {
        settings: "{home}/.codex/config.toml",
        hooks: "{home}/.codex/hooks.json",
        permissions: "sandbox flags read-only|workspace-write|danger-full-access",
        model_catalog: "rpc model/list",
    },
    primitives: AwpPrimitives {
        model_selection: cell(NativeStatus::Native, "turn/start model; model/list"),
        effort: cell(NativeStatus::Native, "turn/start effort"),
        session_open: cell(NativeStatus::Native, "thread/start"),
        session_resume: cell(
            NativeStatus::Native,
            "thread/resume {threadId, excludeTurns}",
        ),
        session_cancel: cell(NativeStatus::Native, "turn/interrupt"),
        streaming: cell(NativeStatus::Native, "item/agentMessage/delta"),
        instruction_channel: cell(
            NativeStatus::Native,
            "developerInstructions on thread/start",
        ),
        permission_modes: cell(
            NativeStatus::Native,
            "sandbox read-only|workspace-write|danger-full-access",
        ),
        skills: cell(NativeStatus::Native, ".codex/skills"),
        hooks: cell(NativeStatus::Native, "Claude-shape hooks.json"),
        subagents: cell(NativeStatus::Native, ".codex/agents TOML"),
        plugins: cell(NativeStatus::NotOffered, "PluginSurface::None"),
    },
};

/// The table, for hosts that compose over `&'static NativeCapability`.
pub const fn native() -> &'static NativeCapability {
    &NATIVE
}
