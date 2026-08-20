//! The exported commercial and control-plane API.
//!
//! Rust is an independent implementation of the contract, not a port, so it is held to
//! producing a *valid* result that meets the shared fixture's objective floor. For a
//! quote that floor is an exact integer price, so "no worse than the floor" and "equal
//! to it" coincide: the assertions below compare against the committed golden documents
//! the reference implementation produced.
//!
//! The shared fixtures live in the surrounding workspace, which a published crate does
//! not carry; that half of the suite reports and returns when they are absent rather
//! than failing, exactly as the PHP suite does.

use std::path::{Path, PathBuf};

use packvium_core::commerce::{
    REJECTION_CODES, catalog_version_info_json, evaluate_policy_json, quote_json,
};

fn shared_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../../../conformance/commerce")
}

/// One shared fixture: its name, its operation, the `{document, request}` call, and
/// whether it is a well-formed request with an answer or a malformed one every
/// implementation must refuse.
#[derive(Debug)]
struct SharedCase {
    name: String,
    operation: String,
    call: String,
    malformed: bool,
}

fn shared_cases() -> Vec<SharedCase> {
    let directory = shared_root().join("fixtures");
    let Ok(entries) = std::fs::read_dir(&directory) else {
        return Vec::new();
    };
    let mut paths: Vec<PathBuf> = entries
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().and_then(|value| value.to_str()) == Some("json"))
        .collect();
    paths.sort();
    paths
        .into_iter()
        .map(|path| {
            let case: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
            let call = serde_json::json!({
                "document": case["document"],
                "request": case["request"],
            });
            SharedCase {
                name: path.file_stem().unwrap().to_string_lossy().into_owned(),
                operation: case["operation"].as_str().unwrap().to_owned(),
                call: call.to_string(),
                malformed: case.get("expects").and_then(serde_json::Value::as_str)
                    == Some("input_error"),
            }
        })
        .collect()
}

fn answer(
    operation: &str,
    call: &str,
) -> Result<String, packvium_core::commerce::CommerceInputError> {
    match operation {
        "quote" => quote_json(call),
        "evaluate_policy" => evaluate_policy_json(call),
        "catalog_version_info" => catalog_version_info_json(call),
        other => panic!("unknown operation {other}"),
    }
}

fn run(operation: &str, call: &str) -> String {
    answer(operation, call).unwrap_or_else(|error| panic!("{operation}: {error}"))
}

#[test]
fn every_shared_fixture_matches_the_golden_document() {
    let cases = shared_cases();
    if cases.is_empty() {
        eprintln!("shared commerce fixtures are not present; skipping");
        return;
    }
    for case in cases {
        if case.malformed {
            assert!(
                answer(&case.operation, &case.call).is_err(),
                "fixture {} answered malformed input",
                case.name
            );
            assert!(
                !shared_root()
                    .join(format!("golden/{}.json", case.name))
                    .exists(),
                "malformed fixture {} must not have a golden document",
                case.name
            );
            continue;
        }
        let golden =
            std::fs::read_to_string(shared_root().join(format!("golden/{}.json", case.name)))
                .unwrap_or_else(|_| panic!("no golden document for fixture {}", case.name));
        assert_eq!(
            run(&case.operation, &case.call),
            golden.trim(),
            "fixture {} diverged",
            case.name
        );
    }
}

#[test]
fn the_shared_fixtures_still_cover_every_rejection_code() {
    let cases = shared_cases();
    if cases.is_empty() {
        eprintln!("shared commerce fixtures are not present; skipping");
        return;
    }
    let mut produced: Vec<String> = cases
        .into_iter()
        .filter(|case| !case.malformed)
        .filter_map(|case| {
            let result: serde_json::Value =
                serde_json::from_str(&run(&case.operation, &case.call)).unwrap();
            result
                .get("error")
                .map(|error| error["code"].as_str().unwrap().to_owned())
        })
        .collect();
    produced.sort();
    produced.dedup();
    let mut expected: Vec<String> = REJECTION_CODES
        .iter()
        .map(|code| (*code).to_owned())
        .collect();
    expected.sort();
    assert_eq!(produced, expected);
}

#[test]
fn a_pinned_quote_is_reproducible_without_the_surrounding_workspace() {
    let call = r#"{"document":{"tariffs":[{"carrier_id":"acme","service_id":"ground","versions":[
        {"effective_at":0,"dimensional_weight_divisor":5000,
         "cost_per_dimensional_kg_minor":{"zone-a":450},"minimum_charge_minor":900,
         "fuel_surcharge_permille":120,
         "accessorials":[{"accessorial_id":"liftgate","flat_charge_minor":250}]}]}]},
        "request":{"carrier_id":"acme","service_id":"ground","tariff_version":1,"zone":"zone-a",
                   "actual_weight_g":1200,"volume_mm3":6000000,
                   "requested_accessorials":["liftgate"]}}"#;

    let result = quote_json(call).unwrap();

    assert_eq!(
        result,
        r#"{"api_version":1,"quote":{"accessorial_charges_minor":[["liftgate",250]],"actual_weight_g":1200,"base_charge_minor":900,"billed_weight_g":1200,"carrier_id":"acme","dimensional_weight_g":1200,"fuel_surcharge_minor":108,"minimum_charge_applied":true,"service_id":"ground","tariff_version":1,"total_minor":1258,"zone":"zone-a"},"status":"ok"}"#
    );
}

#[test]
fn a_malformed_request_is_an_error_while_an_unpriceable_zone_is_an_answer() {
    let document = r#"{"tariffs":[{"carrier_id":"acme","service_id":"ground","versions":[
        {"effective_at":0,"dimensional_weight_divisor":5000,
         "cost_per_dimensional_kg_minor":{"zone-a":450}}]}]}"#;

    let malformed = format!(
        r#"{{"document":{document},"request":{{"carrier_id":"acme","service_id":"ground",
           "tariff_version":1,"zone":"zone-a","actual_weight_g":-1,"volume_mm3":10}}}}"#
    );
    assert!(
        quote_json(&malformed).is_err(),
        "a negative weight is a caller error"
    );

    let unpriceable = format!(
        r#"{{"document":{document},"request":{{"carrier_id":"acme","service_id":"ground",
           "tariff_version":1,"zone":"zone-z","actual_weight_g":10,"volume_mm3":10}}}}"#
    );
    let result = quote_json(&unpriceable).expect("a rejection is a successful call");
    assert!(result.contains(r#""code":"unavailable_zone""#), "{result}");
}

#[test]
fn an_unrecognised_field_is_refused_rather_than_ignored() {
    let call = r#"{"document":{"tariffs":[]},"request":{},"extra":1}"#;

    assert!(quote_json(call).is_err());
}
