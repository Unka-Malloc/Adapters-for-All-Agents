//! The OpenCode dialect's typed quota refusal classifies as
//! `agent_quota_exhausted`.
//!
//! The fixture is hand-authored and explicitly synthetic: its id and
//! timestamp are fabricated, while `error.type` is OpenCode's typed
//! `provider.quota` class (the mapping it documents for
//! `AI.Error.QuotaExceeded`) and `error.message` reproduces the vendor's
//! refusal wording.

use serde_json::Value;

const SYNTHETIC: &str = include_str!("fixtures/opencode-quota-refusal-synthetic.json");

#[test]
fn synthetic_quota_refusal_carries_the_typed_error() {
    let fixture: Value = serde_json::from_str(SYNTHETIC).expect("synthetic fixture is valid JSON");
    assert_eq!(
        fixture["synthetic"], true,
        "the fixture must declare itself synthetic"
    );
    let record = &fixture["record"];

    assert_eq!(record["type"], "idle");
    assert_eq!(record["outcome"], "failed");

    let error_type = record["error"]["type"]
        .as_str()
        .expect("a typed OpenCode error");
    assert!(
        a4aa_opencode::QUOTA_REFUSAL_ERROR_TYPES.contains(&error_type),
        "synthetic error class {error_type:?} is not declared as a quota refusal"
    );
    assert!(
        record["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("Free usage limit reached")),
        "the vendor refusal wording is the fixture input"
    );
}

#[test]
fn non_quota_opencode_errors_are_not_declared_as_quota() {
    for kind in ["provider.auth", "provider.no-route", "provider.timeout"] {
        assert!(
            !a4aa_opencode::QUOTA_REFUSAL_ERROR_TYPES.contains(&kind),
            "{kind} must keep its non-quota classification"
        );
    }
}
