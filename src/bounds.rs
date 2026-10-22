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
/// Last bytes of a drained side channel: a failing child's own error text
/// stays available for classification instead of vanishing with the pipe.
const STDERR_TAIL_BYTES: usize = 16 * 1024;

#[derive(Clone, Default)]
pub struct StderrTail(std::sync::Arc<std::sync::Mutex<String>>);

impl StderrTail {
    pub fn text(&self) -> String {
        self.0.lock().unwrap().clone()
    }

    fn record(&self, bytes: &[u8]) {
        let mut tail = self.0.lock().unwrap();
        tail.push_str(&String::from_utf8_lossy(bytes));
        let len = tail.len();
        let mut excess = len.saturating_sub(STDERR_TAIL_BYTES);
        while excess < len && !tail.is_char_boundary(excess) {
            excess += 1;
        }
        if excess > 0 {
            tail.drain(..excess.min(len));
        }
    }
}

pub fn drain_stderr<R>(
    stderr: R,
    observer: SharedObserver,
    tail: StderrTail,
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
            tail.record(&buffer[..count]);
            if let Some(observer) = observer.as_ref() {
                observer.record("stderr", Direction::Stderr, &buffer[..count]);
            }
        }
    })
}

#[cfg(test)]
mod stderr_tail_tests {
    use super::*;

    #[test]
    fn multibyte_truncation_stays_on_char_boundaries() {
        let tail = StderrTail::default();
        // >16 KiB of multibyte text in awkward chunk sizes: the cut must
        // never land inside a character (a byte-counted drain panics).
        let chunk = "界面 interface 数据 ".repeat(97);
        for _ in 0..24 {
            tail.record(chunk.as_bytes());
        }
        let text = tail.text();
        assert!(text.len() <= STDERR_TAIL_BYTES);
        // Recording again must not observe a poisoned lock or panic.
        tail.record("tail".as_bytes());
        assert!(tail.text().ends_with("tail"));
    }
}
