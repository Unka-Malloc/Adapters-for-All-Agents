//! Native-capability table for `dsh`. Last verified 2026-10-24.
//!
//! Capability order puts SDK above ACP, so `dsh --profile sdk` is the card's
//! primary lane: a first-party JSON-RPC stdio carrier with model/effort on
//! `initialize` but no live deltas and no cancel. ACP is the ordered
//! alternate and the richer conversation surface (durable resume, cancel,
//! live `session/update` streaming, `session/new` catalog). Neither profile
//! is an external plugin.

use a4aa::native::{
    cell, AwpPrimitives, ConfigLocations, InteractionSurfaces, McpMechanism, McpRegistration,
    NativeCapability, NativeStatus,
};

/// Native surfaces and AwP primitives for DeepSeek Harness.
pub const NATIVE: NativeCapability = NativeCapability {
    last_verified: "2026-10-24",
    surfaces: InteractionSurfaces {
        serve: cell(NativeStatus::NotOffered, "no first-party HTTP serve"),
        sdk: cell(
            NativeStatus::Native,
            "dsh --profile sdk: official JSON-RPC stdio (@deepseek-ai/dsh-sdk-app)",
        ),
        acp: cell(
            NativeStatus::Native,
            "dsh acp: first-party ACP profile (@deepseek-ai/dsh-acp-app)",
        ),
        rpc: cell(
            NativeStatus::Native,
            "sdk profile is the rpc lane; a first-party JSON-RPC stdio carrier",
        ),
        cli: cell(
            NativeStatus::NotOffered,
            "interactive dsh is not a headless lane",
        ),
        pty: cell(
            NativeStatus::Native,
            "interactive TTY exists; not the headless card",
        ),
    },
    mcp: McpRegistration {
        mechanism: McpMechanism::Unsupported,
        detail: "McpSurface::None",
    },
    config: ConfigLocations {
        settings: "{home}/.dsh/.credentials.yaml",
        hooks: "",
        permissions: "",
        model_catalog: "ACP session/new configOptions (shipped providers plus a Command Code route provisioned by `a4aa-dsh mount`); SDK initialize provider/model",
    },
    primitives: AwpPrimitives {
        model_selection: cell(
            NativeStatus::Native,
            "ACP configOptions; SDK initialize provider/model",
        ),
        effort: cell(
            NativeStatus::Native,
            "ACP reasoning_effort configOption; SDK initialize reasoningEffort",
        ),
        session_open: cell(
            NativeStatus::Native,
            "ACP session/new; SDK caller-created sessionId on session/prompt",
        ),
        session_resume: cell(
            NativeStatus::Native,
            "ACP session/resume durable; SDK process-scoped only",
        ),
        session_cancel: cell(
            NativeStatus::Native,
            "ACP session/cancel; SDK has no cancel (kill_on_cancel on the alternate)",
        ),
        streaming: cell(
            NativeStatus::Native,
            "ACP session/update; SDK completed assistant/message only",
        ),
        instruction_channel: cell(NativeStatus::Native, "compose into prompt"),
        permission_modes: cell(NativeStatus::NotOffered, "no permission selector"),
        skills: cell(NativeStatus::Native, ".dsh/skills"),
        hooks: cell(NativeStatus::NotOffered, "HookSurface::None"),
        subagents: cell(NativeStatus::NotOffered, "SubagentSurface::None"),
        plugins: cell(NativeStatus::NotOffered, "PluginSurface::None"),
    },
};

/// The table, for hosts that compose over `&'static NativeCapability`.
pub const fn native() -> &'static NativeCapability {
    &NATIVE
}
