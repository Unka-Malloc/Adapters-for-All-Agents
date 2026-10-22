//! Native-capability table for `kilocode`. Last verified 2026-10-21.
//!
//! `kilo acp` is a native OpenCode-fork subcommand, not an external plugin.
//! The `.config/kilo/plugin` hook artifact is the dialect's native plugin
//! directory. `kilo serve` HTTP+SSE is a first-party serve surface LicoUp
//! uses as primary and A4AA has not adapted yet — Serve should become
//! primary, ACP the ordered alternate.

use a4aa::native::{
    cell, AwpPrimitives, ConfigLocations, InteractionSurfaces, McpMechanism, McpRegistration,
    NativeCapability, NativeStatus,
};

/// Native surfaces and AwP primitives for Kilo Code.
pub const NATIVE: NativeCapability = NativeCapability {
    last_verified: "2026-10-21",
    surfaces: InteractionSurfaces {
        serve: cell(
            NativeStatus::Unadapted,
            "kilo serve HTTP+SSE is native; LicoUp primary; a4aa still uses acp",
        ),
        sdk: cell(
            NativeStatus::NotOffered,
            "OpenCode-fork, no separate SDK lane",
        ),
        acp: cell(
            NativeStatus::Native,
            "kilo acp — native OpenCode-fork subcommand",
        ),
        rpc: cell(NativeStatus::NotOffered, "ACP is the protocol surface"),
        cli: cell(NativeStatus::NotOffered, "no headless print lane declared"),
        pty: cell(NativeStatus::Native, "interactive kilo TTY"),
    },
    mcp: McpRegistration {
        mechanism: McpMechanism::ConfigKey,
        detail: ".config/kilo/kilo.json mcp",
    },
    config: ConfigLocations {
        settings: "{home}/.config/kilo/kilo.json",
        hooks: "{home}/.config/kilo/plugin/a4aa.js (native plugin dir)",
        permissions: "OpenCode per-tool permission object is inexpressible; fail closed",
        model_catalog: "kilo models; --variant carries effort",
    },
    primitives: AwpPrimitives {
        model_selection: cell(NativeStatus::Native, "kilo models"),
        effort: cell(NativeStatus::Native, "--variant reasoning effort"),
        session_open: cell(
            NativeStatus::Native,
            "ACP session/new; loadSession advertised",
        ),
        session_resume: cell(NativeStatus::Native, "ACP session/load"),
        session_cancel: cell(NativeStatus::Native, "ACP session/cancel"),
        streaming: cell(
            NativeStatus::Native,
            "ACP session/update; serve would be SSE",
        ),
        instruction_channel: cell(NativeStatus::Native, "compose into prompt"),
        permission_modes: cell(
            NativeStatus::NotOffered,
            "generic allow/deny lists have no enforced OpenCode shape",
        ),
        skills: cell(NativeStatus::Native, ".kilo/skills"),
        hooks: cell(
            NativeStatus::Native,
            "OpenCode plugin events in .config/kilo/plugin",
        ),
        subagents: cell(NativeStatus::Native, ".config/kilo/agents"),
        plugins: cell(NativeStatus::Native, ".config/kilo/plugin/*.ts"),
    },
};

/// The table, for hosts that compose over `&'static NativeCapability`.
pub const fn native() -> &'static NativeCapability {
    &NATIVE
}
