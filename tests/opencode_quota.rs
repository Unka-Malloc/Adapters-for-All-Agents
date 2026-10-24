//! The OpenCode dialect's typed quota refusal must surface as
//! `agent_quota_exhausted`, while its other typed errors keep their
//! protocol-failure classification.
//!
//! Two fixtures cover the two production shapes: a synthetic serve-lane
//! record that exercises the typed classifier, and a sanitized recording of
//! the ACP error OpenCode actually returned for an exhausted free-usage
//! allowance (-32603, not `AuthRequired`). The recording keeps only the
//! vendor's code, message and cause type; stack frames, identifiers,
//! sessions, timestamps and host paths were removed.

use serde_json::Value;

const SYNTHETIC: &str =
    include_str!("../crates/a4aa-opencode/tests/fixtures/opencode-quota-refusal-synthetic.json");
const RECORDED_ACP: &str = include_str!("fixtures/opencode-acp-quota-refusal.json");

#[test]
fn synthetic_opencode_quota_refusal_classifies_as_quota_exhausted() {
    let fixture: Value = serde_json::from_str(SYNTHETIC).expect("synthetic fixture is valid JSON");
    assert_eq!(
        fixture["synthetic"], true,
        "the fixture must declare itself synthetic"
    );
    let record = &fixture["record"];
    let error_type = record["error"]["type"].as_str();
    let message = record["error"]["message"].as_str().expect("refusal text");

    assert!(
        a4aa::proto::serve::is_quota_refusal(error_type, message),
        "the synthetic OpenCode refusal must classify as a quota refusal"
    );
    for declared in a4aa_opencode::QUOTA_REFUSAL_ERROR_TYPES {
        assert_eq!(
            Some(*declared),
            error_type,
            "the dialect's declared quota class must match the synthetic server error"
        );
    }
}

#[test]
fn recorded_acp_quota_refusal_is_an_internal_error_with_quota_wording() {
    let fixture: Value =
        serde_json::from_str(RECORDED_ACP).expect("recorded fixture is valid JSON");
    let recorded = &fixture["recorded_error"];
    // The incident was a JSON-RPC InternalError (-32603), which the old
    // auth-only branch classified as a generic protocol failure.
    assert_eq!(recorded["code"].as_i64(), Some(-32603));
    assert_eq!(
        recorded["cause_type"].as_str(),
        Some("AI.Error.QuotaExceeded")
    );
    let message = recorded["message"].as_str().expect("refusal text");
    assert!(
        a4aa::proto::serve::is_quota_refusal(None, message),
        "the recorded refusal text must classify as a quota refusal"
    );
}

#[test]
fn typed_auth_failure_is_not_a_quota_refusal() {
    assert!(!a4aa::proto::serve::is_quota_refusal(
        Some("provider.auth"),
        "This API key has expired. Create a new API key in OpenCode Console."
    ));
}

#[test]
fn untyped_quota_wording_keeps_the_shared_fallback() {
    assert!(a4aa::proto::serve::is_quota_refusal(
        None,
        "AI.Error: Free usage limit reached. Subscribe to OpenCode Go to keep going."
    ));
}
