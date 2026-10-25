//! Typed adapter failures.
//!
//! Every failure carries a stable machine-readable `code` and the pipeline
//! `stage` that produced it. `retryable` marks failures that may succeed on a
//! fresh attempt; `uncertain_effect` marks failures where the agent may have
//! already observed the request (a prompt dispatch that lost its response is
//! uncertain — retrying could re-bill or duplicate work).

use std::fmt;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AdapterError {
    pub code: &'static str,
    pub stage: &'static str,
    pub retryable: bool,
    pub uncertain_effect: bool,
    pub detail: Option<String>,
}

impl AdapterError {
    pub const fn permanent(code: &'static str, stage: &'static str) -> Self {
        Self {
            code,
            stage,
            retryable: false,
            uncertain_effect: false,
            detail: None,
        }
    }

    pub const fn retryable(code: &'static str, stage: &'static str) -> Self {
        Self {
            code,
            stage,
            retryable: true,
            uncertain_effect: false,
            detail: None,
        }
    }

    pub const fn uncertain(code: &'static str, stage: &'static str) -> Self {
        Self {
            code,
            stage,
            retryable: true,
            uncertain_effect: true,
            detail: None,
        }
    }

    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    pub(crate) fn io(code: &'static str, stage: &'static str, error: std::io::Error) -> Self {
        let kind = error.kind();
        let mut failure = match kind {
            std::io::ErrorKind::NotFound => Self::permanent(code, stage),
            std::io::ErrorKind::PermissionDenied => Self::permanent(code, stage),
            std::io::ErrorKind::TimedOut => Self::retryable(code, stage),
            _ => Self::uncertain(code, stage),
        };
        failure.detail = Some(error.to_string());
        failure
    }
}

impl fmt::Display for AdapterError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.detail {
            Some(detail) => write!(formatter, "{} [{}]: {}", self.code, self.stage, detail),
            None => write!(formatter, "{} [{}]", self.code, self.stage),
        }
    }
}

impl std::error::Error for AdapterError {}

/// Whether agent output or a protocol error names a quota, capacity or
/// model-availability refusal. A refusal is failover-eligible but not a
/// proven non-effect — text matching cannot tell whether billable work
/// ran before the refusal surfaced — so callers map it with
/// `AdapterError::uncertain`, never as a certain replay. The needles
/// match the refusal text vendors actually print (rate limits, usage
/// and spend caps, missing or unavailable models).
pub(crate) fn quota_or_capacity_refusal(text: &str) -> bool {
    let text = text.to_lowercase();
    [
        "rate limit",
        "ratelimit",
        "too many requests",
        "resource_exhausted",
        "quota",
        "insufficient_quota",
        "usage limit",
        "usage_limit",
        "spend limit",
        "overloaded",
        "at capacity",
        "capacity exceeded",
        "capacity exhausted",
        "capacity_exceeded",
        "model unavailable",
        "model not found",
        "model_not_found",
        "no such model",
        "model does not exist",
        "unsupported model",
    ]
    .iter()
    .any(|needle| text.contains(needle))
}

pub type Result<T> = std::result::Result<T, AdapterError>;
