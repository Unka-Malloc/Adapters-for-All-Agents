//! Bounded I/O helpers.
//!
//! An agent that writes unbounded output must never exhaust host memory:
//! output caps drain-and-discard, and stderr is never retained — only
//! forwarded to an attached observer (tag `stderr`) so diagnostics stay
//! possible without paying a resident buffer per child.

use std::io;
use tokio::io::{AsyncRead, AsyncReadExt};

use crate::observe::{Direction, SharedObserver};

/// I/O block size for stream drains and pty reads — one name, one place.
pub const READ_CHUNK: usize = 8192;

/// Largest UTF-8 prefix within an optional byte budget.
pub(crate) fn bounded_prefix(text: &str, limit: Option<usize>) -> &str {
    let mut end = limit.unwrap_or(text.len()).min(text.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// Spawn a background drain of a child stderr. Nothing is retained — the
/// drain exists only so a chatty agent never blocks on a full pipe. Raw
/// bytes are forwarded to `observer` under `Direction::Stderr` when
/// attached.
pub fn drain_stderr<R>(
    stderr: R,
    observer: SharedObserver,
) -> tokio::task::JoinHandle<io::Result<()>>
where
    R: AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let mut stderr = stderr;
        let mut buffer = [0u8; READ_CHUNK];
        loop {
            let count = stderr.read(&mut buffer).await?;
            if count == 0 {
                return Ok(());
            }
            if let Some(observer) = observer.as_ref() {
                observer.record("stderr", Direction::Stderr, &buffer[..count]);
            }
        }
    })
}
