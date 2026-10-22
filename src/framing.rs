//! Newline-delimited JSON framing.
//!
//! ACP and every stdio JSON-RPC agent lane in scope frame messages as one
//! JSON value per line. A line longer than `max_bytes` is a protocol
//! violation — it is discarded rather than delivered, so a hostile or broken
//! peer cannot force unbounded allocation.

use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};

/// Default bound per NDJSON frame: small on purpose so every active lane
/// does not reserve room for a stalled writer's huge frame. Agents that
/// legitimately emit large frames — pi's `agent_end` embeds the whole
/// session transcript — opt in per spec via `RpcCallSpec.max_frame_bytes`.
pub const DEFAULT_MAX_MESSAGE_BYTES: usize = 1024 * 1024;

/// The sidecar↔host link forwards agent payloads (including transcript
/// frames) upstream, so it carries the wider bound the largest declared
/// agent surface needs.
pub const SIDECAR_MAX_MESSAGE_BYTES: usize = 64 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum FrameError {
    #[error("frame exceeds {0} bytes")]
    Oversized(usize),
    #[error("invalid json: {0}")]
    Invalid(serde_json::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// Write one JSON value as a single NDJSON line.
pub async fn write_frame<W: AsyncWrite + Unpin>(
    writer: &mut W,
    value: &Value,
) -> std::io::Result<()> {
    let mut bytes = serde_json::to_vec(value).map_err(std::io::Error::other)?;
    bytes.push(b'\n');
    writer.write_all(&bytes).await?;
    writer.flush().await
}

/// Read one NDJSON line as a JSON value.
///
/// `Ok(None)` is clean EOF. `Ok(Some(Err(FrameError::Oversized)))` marks a
/// discarded oversized line; the stream remains usable.
pub async fn read_frame<R: AsyncRead + Unpin>(
    reader: &mut BufReader<R>,
    max_bytes: usize,
) -> std::io::Result<Option<Result<Value, FrameError>>> {
    let mut line = Vec::new();
    // `read_until` still buffers unboundedly for a line with no terminator;
    // poll the fill buffer in chunks so an oversized line is cut early.
    loop {
        let (chunk, complete) = {
            let available = reader.fill_buf().await?;
            if available.is_empty() {
                // EOF: a non-empty remnant is an unterminated trailing line.
                // Peers that close without a final newline still deliver it.
                return Ok(if line.is_empty() {
                    None
                } else {
                    Some(serde_json::from_slice::<Value>(&line).map_err(FrameError::Invalid))
                });
            }
            let take = available
                .iter()
                .position(|byte| *byte == b'\n')
                .map(|pos| pos + 1)
                .unwrap_or(available.len());
            (available[..take].to_vec(), available[take - 1] == b'\n')
        };
        line.extend_from_slice(&chunk);
        reader.consume(chunk.len());
        if line.len() > max_bytes {
            if complete {
                return Ok(Some(Err(FrameError::Oversized(line.len()))));
            }
            // Keep draining the oversized line to its terminator.
            loop {
                let (take, complete) = {
                    let available = reader.fill_buf().await?;
                    if available.is_empty() {
                        return Ok(Some(Err(FrameError::Oversized(line.len()))));
                    }
                    let take = available
                        .iter()
                        .position(|byte| *byte == b'\n')
                        .map(|pos| pos + 1)
                        .unwrap_or(available.len());
                    (take, available[take - 1] == b'\n')
                };
                reader.consume(take);
                if complete {
                    return Ok(Some(Err(FrameError::Oversized(line.len()))));
                }
            }
        }
        if complete {
            while line.last() == Some(&b'\n') || line.last() == Some(&b'\r') {
                line.pop();
            }
            if line.is_empty() {
                // Blank line between frames: keep reading.
                continue;
            }
            return Ok(Some(
                serde_json::from_slice::<Value>(&line).map_err(FrameError::Invalid),
            ));
        }
    }
}
