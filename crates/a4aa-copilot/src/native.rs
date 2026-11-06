//! Native-capability table for `copilot`. Last verified 2026-10-25
//! (GitHub Copilot CLI 1.0.93).

use a4aa::native::{
    cell, AwpPrimitives, ConfigLocations, InteractionSurfaces, McpMechanism, McpRegistration,
    NativeCapability, NativeStatus,
};

/// Native surfaces and AwP primitives for GitHub Copilot CLI.
pub const NATIVE: NativeCapability = NativeCapability {
    last_verified: "2026-10-25",
    surfaces: InteractionSurfaces {
        serve: cell(NativeStatus::NotOffered, "no first-party local serve"),
        sdk: cell(
            NativeStatus::Unadapted,
            "@github/copilot-sdk ships for extension subprocesses; no carded host surface",
        ),
        acp: cell(
            NativeStatus::Native,
            "copilot --acp --stdio --no-auto-update — stock ACP server",
        ),
        rpc: cell(NativeStatus::NotOffered, "ACP is the protocol surface"),
        cli: cell(
            NativeStatus::Native,
            "copilot -p --output-format json (JSONL); -r resume; --silent",
        ),
        pty: cell(NativeStatus::Native, "interactive copilot TTY"),
    },
    mcp: McpRegistration {
        mechanism: McpMechanism::ConfigKey,
        detail: "~/.copilot/mcp-config.json mcpServers; --additional-mcp-config per session",
    },
    config: ConfigLocations {
        settings: "{home}/.copilot/settings.json",
        hooks: a4aa::hooks::harnesses::COPILOT.config_path,
        permissions: "flag-based --allow-all-tools/--deny-tool/--allow-all, not a mode selector",
        model_catalog: "",
    },
    primitives: AwpPrimitives {
        model_selection: cell(
            NativeStatus::Native,
            "CLI --model / --auto-tier; ACP negotiated",
        ),
        effort: cell(NativeStatus::Native, "--reasoning-effort levels"),
        session_open: cell(NativeStatus::Native, "ACP session/new; -p one-shot"),
        session_resume: cell(NativeStatus::Native, "-r/--resume; copilot sessions"),
        session_cancel: cell(NativeStatus::Native, "ACP session/cancel"),
        streaming: cell(
            NativeStatus::Native,
            "--output-format json JSONL; ACP session/update",
        ),
        instruction_channel: cell(
            NativeStatus::Native,
            "AGENTS.md and related files (--no-custom-instructions disables)",
        ),
        permission_modes: cell(
            NativeStatus::NotOffered,
            "flags and tool lists, not a mode-valued selector",
        ),
        skills: cell(
            NativeStatus::Native,
            ".github/skills + ~/.copilot/skills; copilot skill",
        ),
        hooks: a4aa::native::hook_cell(a4aa::hooks::harnesses::COPILOT),
        subagents: cell(
            NativeStatus::Native,
            ".github/agents + ~/.copilot/agents *.agent.md; --agent",
        ),
        plugins: cell(
            NativeStatus::Native,
            "--plugin-dir; ~/.copilot/installed-plugins; copilot plugin",
        ),
    },
};

/// The table, for hosts that compose over `&'static NativeCapability`.
pub const fn native() -> &'static NativeCapability {
    &NATIVE
}
