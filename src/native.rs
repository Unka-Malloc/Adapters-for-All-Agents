//! Native-capability tables: the reviewable, machine-checkable record of
//! what each agent offers natively, what A4AA adapts, and what is still
//! missing or reached through a non-native mechanism.
//!
//! Every `a4aa-<agent>` crate publishes one [`NativeCapability`] constant.
//! [`a4aa-dialects`] aggregates them. `last_verified` is a `YYYY-MM-DD`
//! date that the release train refuses to ship stale — freshness is a
//! release gate, not a per-commit lint.

use serde::Serialize;

/// One cell in a native-capability table.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct Cell {
    /// How the agent offers this surface or primitive today.
    pub status: NativeStatus,
    /// Short evidence: native flag, config key, or why it is absent.
    pub note: &'static str,
}

/// Supported natively / not offered / unadapted / adapted via a non-native
/// mechanism (flagged for removal).
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum NativeStatus {
    /// The agent offers this natively and A4AA adapts that native form.
    Native,
    /// The agent does not offer this.
    NotOffered,
    /// The agent offers this natively; A4AA has not adapted it yet.
    Unadapted,
    /// Currently reached through a non-native plugin or external adapter.
    NonNative,
}

/// How MCP servers are declared on this agent.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum McpMechanism {
    /// A native config-file key (`mcpServers`, `mcp`, …).
    ConfigKey,
    /// A native CLI command registers servers.
    NativeCommand,
    /// Protocol-native negotiation (ACP `mcpServers` on session/new).
    Protocol,
    /// The agent has no MCP registration surface.
    Unsupported,
}

/// Interaction surfaces ordered by capability tier: serve > sdk > acp/rpc > cli.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct InteractionSurfaces {
    pub serve: Cell,
    pub sdk: Cell,
    pub acp: Cell,
    pub rpc: Cell,
    pub cli: Cell,
    /// PTY / interactive note — not a lane, a transport fact.
    pub pty: Cell,
}

/// MCP registration mechanism plus the config key or command that carries it.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct McpRegistration {
    pub mechanism: McpMechanism,
    pub detail: &'static str,
}

/// Settings, hooks, permissions and model-catalog locations the agent reads
/// natively. Empty string means the agent has no such file.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct ConfigLocations {
    pub settings: &'static str,
    pub hooks: &'static str,
    pub permissions: &'static str,
    pub model_catalog: &'static str,
}

/// Remaining Agents-With-Profiles primitives.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct AwpPrimitives {
    pub model_selection: Cell,
    pub effort: Cell,
    pub session_open: Cell,
    pub session_resume: Cell,
    pub session_cancel: Cell,
    pub streaming: Cell,
    pub instruction_channel: Cell,
    pub permission_modes: Cell,
    pub skills: Cell,
    pub hooks: Cell,
    pub subagents: Cell,
    pub plugins: Cell,
}

/// One dialect crate's native-capability table.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct NativeCapability {
    /// ISO date (`YYYY-MM-DD`) this table was last checked against the
    /// installed binary and vendor docs. The release train refuses a
    /// candidate whose tables are older than the freshness window.
    pub last_verified: &'static str,
    pub surfaces: InteractionSurfaces,
    pub mcp: McpRegistration,
    pub config: ConfigLocations,
    pub primitives: AwpPrimitives,
}

/// Build a table cell.
pub const fn cell(status: NativeStatus, note: &'static str) -> Cell {
    Cell { status, note }
}

impl NativeCapability {
    /// `true` when `last_verified` is a `YYYY-MM-DD` calendar date.
    pub fn date_is_iso(&self) -> bool {
        parse_iso_date(self.last_verified).is_some()
    }
}

/// Parse `YYYY-MM-DD` into (year, month, day). Used by the freshness check.
pub fn parse_iso_date(text: &str) -> Option<(i32, u32, u32)> {
    let bytes = text.as_bytes();
    if bytes.len() != 10 || bytes[4] != b'-' || bytes[7] != b'-' {
        return None;
    }
    let year: i32 = text[..4].parse().ok()?;
    let month: u32 = text[5..7].parse().ok()?;
    let day: u32 = text[8..10].parse().ok()?;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    Some((year, month, day))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iso_date_parser_accepts_calendar_days() {
        assert_eq!(parse_iso_date("2026-10-21"), Some((2026, 10, 21)));
        assert!(parse_iso_date("2026-13-01").is_none());
        assert!(parse_iso_date("yesterday").is_none());
    }
}
