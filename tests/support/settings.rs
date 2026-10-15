//! Test watchdog budget, shared by unit and integration fixtures.
//! Production requests have no elapsed-time cutoff.
pub const WAIT_BUDGET: std::time::Duration = std::time::Duration::from_secs(10);

// Used only where the OS exposes no notification for an orphan's reaping.
#[allow(dead_code)]
pub const REAP_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(10);
