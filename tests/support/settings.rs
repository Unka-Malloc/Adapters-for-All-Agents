//! Test watchdog budget, shared by unit and integration fixtures.
//! Production requests have no elapsed-time cutoff.
pub const WAIT_BUDGET: std::time::Duration = std::time::Duration::from_secs(10);
