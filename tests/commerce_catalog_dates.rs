//! Catalog dates are validated before the exported commerce API resolves history.

use packvium_core::commerce::catalog_version_info_json;
use serde_json::{Value, json};

fn document(rollback: Option<Value>) -> Value {
    let mut versions = vec![json!({"effective_at": 0, "published_at": 0, "snapshot": {}})];
    if let Some(rollback) = rollback {
        versions.push(rollback);
    }
    json!({"catalogs": [{"catalog_id": "c", "versions": versions}]})
}

#[test]
fn negative_rollback_effective_date_is_an_input_error() {
    let input = json!({
        "document": document(Some(json!({"rollback_to": 1, "published_at": 0, "effective_at": -1}))),
        "request": {"catalog_id": "c", "version": 2, "resolved_at": 0},
    });
    assert!(catalog_version_info_json(&input.to_string()).is_err());
}

#[test]
fn negative_rollback_publication_date_is_an_input_error() {
    let input = json!({
        "document": document(Some(json!({"rollback_to": 1, "published_at": -1, "effective_at": 0}))),
        "request": {"catalog_id": "c", "version": 2, "resolved_at": 0},
    });
    assert!(catalog_version_info_json(&input.to_string()).is_err());
}

#[test]
fn default_effective_date_cannot_admit_a_negative_rollback_publication_date() {
    let input = json!({
        "document": document(Some(json!({"rollback_to": 1, "published_at": -1}))),
        "request": {"catalog_id": "c", "version": 2, "resolved_at": 0},
    });
    assert!(catalog_version_info_json(&input.to_string()).is_err());
}

#[test]
fn negative_resolution_time_is_an_input_error_before_catalog_lookup() {
    for catalog_id in ["c", "missing"] {
        let input = json!({
            "document": document(None),
            "request": {"catalog_id": catalog_id, "version": 1, "resolved_at": -1},
        });
        let error = catalog_version_info_json(&input.to_string()).unwrap_err();
        assert_eq!(error.path, "request.resolved_at");
    }
}

#[test]
fn zero_dates_are_valid_for_a_rollback() {
    let input = json!({
        "document": document(Some(json!({"rollback_to": 1, "published_at": 0}))),
        "request": {"catalog_id": "c", "as_of": 0, "resolved_at": 0},
    });
    let result: Value =
        serde_json::from_str(&catalog_version_info_json(&input.to_string()).unwrap()).unwrap();
    assert_eq!(result["status"], "ok");
    assert_eq!(result["catalog"]["version"], 2);
    assert_eq!(result["catalog"]["effective_at"], 0);
    assert_eq!(result["catalog"]["published_at"], 0);
    assert_eq!(result["catalog"]["resolved_at"], 0);
}
