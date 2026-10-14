//! Raw wire observation hooks.
//!
//! A host may attach an observer to record the bytes an adapter exchanges
//! with an agent (diagnostics, protocol audits). The hook is opt-in and
//! deliberately receives only bytes plus a direction label — interpretation
//! stays with the host. Observers must be cheap and non-blocking; a failing
//! observer never stalls the lane.

use std::fmt;
use std::sync::Arc;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Direction {
    Sent,
    Received,
    Stderr,
}

/// Callback sink for raw lane bytes.
pub trait Observer: Send + Sync {
    fn record(&self, source: &'static str, direction: Direction, bytes: &[u8]);
}

impl<F> Observer for F
where
    F: Fn(&'static str, Direction, &[u8]) + Send + Sync,
{
    fn record(&self, source: &'static str, direction: Direction, bytes: &[u8]) {
        self(source, direction, bytes)
    }
}

/// Shared optional observer. `None` records nothing.
pub type SharedObserver = Option<Arc<dyn Observer>>;

/// A no-observer singleton.
pub fn none() -> SharedObserver {
    None
}

impl fmt::Debug for dyn Observer {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Observer(..)")
    }
}
