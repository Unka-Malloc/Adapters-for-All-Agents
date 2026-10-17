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
pub mod ws;

/// RFC 4648 base64, standard alphabet — enough for basic-auth blobs and
/// WebSocket handshake keys without pulling an encoding dependency.
pub(crate) fn base64(data: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let mut group = [0u8; 3];
        group[..chunk.len()].copy_from_slice(chunk);
        let value = u32::from(group[0]) << 16 | u32::from(group[1]) << 8 | u32::from(group[2]);
        out.push(ALPHABET[(value >> 18) as usize & 63] as char);
        out.push(ALPHABET[(value >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[(value >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[value as usize & 63] as char
        } else {
            '='
        });
    }
    out
}
