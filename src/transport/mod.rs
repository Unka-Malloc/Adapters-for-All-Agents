//! Byte transports to an agent process.
//!
//! `stdio` attaches lanes to piped stdin/stdout (NDJSON / JSON-RPC lanes and
//! plain CLI pipes). `pty` attaches the child to a raw-mode pseudo-terminal
//! slave for agents that only behave correctly on a TTY (`isatty` probes,
//! TUI-aware rendering). stderr always stays a real pipe: keeping it off the
//! terminal preserves both drain semantics and clean stdout framing.

#[cfg(unix)]
pub mod pty;
pub mod stdio;
