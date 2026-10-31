//! Native-capability table for `commandcode`. Last verified 2026-10-21.

use a4aa::native::{
    cell, AwpPrimitives, ConfigLocations, InteractionSurfaces, McpMechanism, McpRegistration,
    NativeCapability, NativeStatus,
};

/// Native surfaces and AwP primitives for Command Code.
pub const NATIVE: NativeCapability = NativeCapability {
    last_verified: "2026-10-21",
    surfaces: InteractionSurfaces {
        serve: cell(NativeStatus::NotOffered, "no first-party HTTP serve"),
        sdk: cell(NativeStatus::NotOffered, "no official library surface"),
        acp: cell(NativeStatus::NotOffered, "ships no ACP"),
        rpc: cell(NativeStatus::NotOffered, "no JSON-RPC stdio"),
        cli: cell(
            NativeStatus::Native,
            "command-code --print --output-format json --model --effort --resume",
        ),
        pty: cell(NativeStatus::Native, "interactive command-code TTY"),
    },
    mcp: McpRegistration {
        mechanism: McpMechanism::Unsupported,
        detail: "McpSurface::None",
    },
    config: ConfigLocations {
        settings: "{home}/.commandcode/config.json",
        hooks: a4aa::hooks::harnesses::COMMAND_CODE.config_path,
        permissions: "--permission-mode standard|plan|auto-accept",
        model_catalog: "command-code --list-models",
    },
    primitives: AwpPrimitives {
        model_selection: cell(NativeStatus::Native, "--model {model}"),
        effort: cell(NativeStatus::Native, "--effort {effort}"),
        session_open: cell(NativeStatus::Native, "run_start sessionId"),
        session_resume: cell(NativeStatus::Native, "--resume {sessionId}"),
        session_cancel: cell(NativeStatus::Native, "kill the --print process"),
        streaming: cell(NativeStatus::Native, "newline-delimited run events"),
        instruction_channel: cell(NativeStatus::Native, "compose into prompt"),
        permission_modes: cell(NativeStatus::Native, "standard|plan|auto-accept"),
        skills: cell(NativeStatus::Native, ".commandcode/skills"),
        hooks: a4aa::native::hook_cell(a4aa::hooks::harnesses::COMMAND_CODE),
        subagents: cell(NativeStatus::NotOffered, "SubagentSurface::None"),
        plugins: cell(NativeStatus::NotOffered, "PluginSurface::None"),
    },
};

/// The table, for hosts that compose over `&'static NativeCapability`.
pub const fn native() -> &'static NativeCapability {
    &NATIVE
}
