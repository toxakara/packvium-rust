//! Every `PackError` names a closed code, so a transport maps codes rather than messages.
//!
//! The hosted API turns these codes into HTTP statuses; a renamed code would silently change a
//! status, so each variant's code is pinned here.

use packvium_core::{PackError, pack_json};
use serde_json::json;

#[test]
fn every_variant_names_its_closed_code() {
    let malformed = serde_json::from_str::<serde_json::Value>("{").unwrap_err();
    let cases = [
        (PackError::InvalidInput("x".into()), "invalid_input"),
        (PackError::Serialization(malformed), "invalid_input"),
        (
            PackError::UnsupportedFeature("x".into()),
            "unsupported_feature",
        ),
        (PackError::UnsupportedUnit("x".into()), "unsupported_unit"),
        (PackError::InvalidNumber("x".into()), "invalid_number"),
        (
            PackError::InvalidSolution("x".into()),
            "solution_failed_validation",
        ),
        (PackError::TimeLimit, "time_limit"),
    ];
    for (error, code) in cases {
        assert_eq!(error.code(), code, "{error}");
        assert!(error.request_error().is_none(), "{error}");
    }
}

#[test]
fn a_named_refusal_carries_its_own_code_reason_and_field() {
    let request = json!({
        "items": [{ "id": "cube", "quantity": 0,
                    "dimensions": { "length": "1", "width": "1", "height": "1" } }],
        "containers": [{ "id": "box",
                         "inner_dimensions": { "length": "2", "width": "2", "height": "2" } }]
    });
    let error = pack_json(&request.to_string()).unwrap_err();
    let refusal = error.request_error().expect("a named refusal");
    assert_eq!(error.code(), "invalid_request");
    assert_eq!(error.code(), refusal.code());
    assert_eq!(refusal.field(), "/items/0/quantity");
    assert!(!refusal.reason().is_empty());
}
