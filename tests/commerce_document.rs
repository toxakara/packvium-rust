//! Every way to hand the Rust commerce API something it should refuse.
//!
//! The shared fixtures in `commerce.rs` prove this implementation agrees with the other
//! three on what it *answers*. This file covers the other half from the inside: the
//! per-field guards in the document parser, which a caller reaches long before any
//! pricing happens. A parser that never sees malformed input cannot be told apart from
//! one that accepts anything.

use packvium_core::commerce::{catalog_version_info_json, evaluate_policy_json, quote_json};

/// A well-formed one-version tariff, so a test can vary exactly one field at a time.
const TARIFF: &str = r#"{"effective_at":0,"dimensional_weight_divisor":1000,
    "cost_per_dimensional_kg_minor":{"z":100},
    "accessorials":[{"accessorial_id":"lift","flat_charge_minor":5}]}"#;

fn quote_call(document: &str, request: &str) -> String {
    format!(r#"{{"document":{document},"request":{request}}}"#)
}

fn tariff_document(version: &str) -> String {
    format!(r#"{{"tariffs":[{{"carrier_id":"a","service_id":"g","versions":[{version}]}}]}}"#)
}

fn shipment() -> &'static str {
    r#"{"carrier_id":"a","service_id":"g","tariff_version":1,"zone":"z",
        "actual_weight_g":1000,"volume_mm3":1}"#
}

#[track_caller]
fn refused(call: &str, fragment: &str) {
    match quote_json(call) {
        Ok(answer) => panic!("expected a refusal mentioning {fragment:?}, got {answer}"),
        Err(error) => assert!(
            error.to_string().contains(fragment),
            "expected {fragment:?} in {error}",
        ),
    }
}

#[test]
fn the_call_envelope_itself_is_validated() {
    refused("{not json", "invalid JSON");
    refused(r#"["document","request"]"#, "expected an object");
    refused(r#"{"document":{}}"#, "missing required key");
    refused(
        r#"{"document":{},"request":{},"extra":1}"#,
        "unrecognised key",
    );
    refused(r#"{"document":7,"request":{}}"#, "expected an object");
}

#[test]
fn every_out_of_range_tariff_field_is_named() {
    for (version, fragment) in [
        (
            r#"{"effective_at":-1,"dimensional_weight_divisor":1,
                "cost_per_dimensional_kg_minor":{"z":1}}"#,
            "effective_at cannot be negative",
        ),
        (
            r#"{"effective_at":0,"dimensional_weight_divisor":0,
                "cost_per_dimensional_kg_minor":{"z":1}}"#,
            "dimensional_weight_divisor must be positive",
        ),
        (
            r#"{"effective_at":0,"dimensional_weight_divisor":1,
                "cost_per_dimensional_kg_minor":{"z":-1}}"#,
            "cannot be negative",
        ),
        (
            r#"{"effective_at":0,"dimensional_weight_divisor":1,
                "cost_per_dimensional_kg_minor":{"z":1},"minimum_charge_minor":-1}"#,
            "cannot be negative",
        ),
        (
            r#"{"effective_at":0,"dimensional_weight_divisor":1,
                "cost_per_dimensional_kg_minor":[]}"#,
            "expected an object",
        ),
        (
            r#"{"effective_at":true,"dimensional_weight_divisor":1,
                "cost_per_dimensional_kg_minor":{"z":1}}"#,
            "expected an exact integer",
        ),
        (
            r#"{"effective_at":1.5,"dimensional_weight_divisor":1,
                "cost_per_dimensional_kg_minor":{"z":1}}"#,
            "expected an exact integer",
        ),
    ] {
        refused(&quote_call(&tariff_document(version), shipment()), fragment);
    }
}

#[test]
fn an_accessorial_must_set_exactly_one_kind_of_charge() {
    for (accessorials, fragment) in [
        (r#"[{"accessorial_id":"x"}]"#, "exactly one"),
        (
            r#"[{"accessorial_id":"x","flat_charge_minor":1,"permille_of_base":1}]"#,
            "exactly one",
        ),
        (
            r#"[{"accessorial_id":"x","flat_charge_minor":-1}]"#,
            "cannot be negative",
        ),
        (
            r#"[{"accessorial_id":"x","flat_charge_minor":1},
               {"accessorial_id":"x","permille_of_base":1}]"#,
            "duplicate accessorial_id",
        ),
    ] {
        let version = format!(
            r#"{{"effective_at":0,"dimensional_weight_divisor":1,
                "cost_per_dimensional_kg_minor":{{"z":1}},"accessorials":{accessorials}}}"#
        );
        refused(
            &quote_call(&tariff_document(&version), shipment()),
            fragment,
        );
    }
}

#[test]
fn a_history_needs_an_identity_and_at_least_one_version() {
    refused(
        &quote_call(
            r#"{"tariffs":[{"carrier_id":"a","versions":[]}]}"#,
            shipment(),
        ),
        "missing required key",
    );
    refused(
        &quote_call(
            r#"{"tariffs":[{"carrier_id":"a","service_id":"g","versions":[]}]}"#,
            shipment(),
        ),
        "at least one version",
    );
    let duplicate = format!(
        r#"{{"tariffs":[{{"carrier_id":"a","service_id":"g","versions":[{TARIFF}]}},
            {{"carrier_id":"a","service_id":"g","versions":[{TARIFF}]}}]}}"#
    );
    refused(
        &quote_call(&duplicate, shipment()),
        "duplicate tariff history",
    );
}

#[test]
fn a_malformed_shipment_is_named_before_anything_is_priced() {
    for (request, fragment) in [
        (
            r#"{"carrier_id":"a","service_id":"g","zone":"z","actual_weight_g":1,
                "volume_mm3":1}"#,
            "exactly one of",
        ),
        (
            r#"{"carrier_id":"a","service_id":"g","tariff_version":1,"as_of":0,"zone":"z",
                "actual_weight_g":1,"volume_mm3":1}"#,
            "exactly one of",
        ),
        (
            r#"{"carrier_id":"a","service_id":"g","tariff_version":1,"zone":"",
                "actual_weight_g":1,"volume_mm3":1}"#,
            "zone is required",
        ),
        (
            r#"{"carrier_id":"a","service_id":"g","tariff_version":1,"zone":"z",
                "actual_weight_g":-1,"volume_mm3":1}"#,
            "cannot be negative",
        ),
        (
            r#"{"carrier_id":"a","service_id":"g","tariff_version":1,"zone":"z",
                "actual_weight_g":1,"volume_mm3":1,"requested_accessorials":"lift"}"#,
            "expected a list",
        ),
        (
            r#"{"carrier_id":"a","service_id":"g","tariff_version":1,"zone":"z",
                "actual_weight_g":1,"volume_mm3":1,"requested_accessorials":["lift","lift"]}"#,
            "must be unique",
        ),
        (
            r#"{"carrier_id":"a","service_id":"g","tariff_version":1,"zone":"z",
                "actual_weight_g":1,"volume_mm3":1,"requested_accessorials":[""]}"#,
            "non-empty",
        ),
    ] {
        refused(&quote_call(&tariff_document(TARIFF), request), fragment);
    }
}

#[test]
fn a_malformed_policy_document_or_request_is_named() {
    let call =
        |document: &str, request: &str| format!(r#"{{"document":{document},"request":{request}}}"#);
    let rule =
        |version: &str| format!(r#"{{"policy_rules":[{{"rule_id":"r","versions":[{version}]}}]}}"#);
    let ask = r#"{"scope":"hazmat","context":{},"as_of":0}"#;

    for (version, fragment) in [
        (
            r#"{"scope":"warehouse","action":"reject","priority":1,"effective_at":0,
                "predicates":[{"scope":"warehouse","field":"f","operator":"exists"}]}"#,
            "unsupported policy scope",
        ),
        (
            r#"{"scope":"hazmat","action":"maybe","priority":1,"effective_at":0,
                "predicates":[{"scope":"hazmat","field":"f","operator":"exists"}]}"#,
            "unsupported policy action",
        ),
        (
            r#"{"scope":"hazmat","action":"reject","priority":1,"effective_at":0,
                "predicates":[{"scope":"hazmat","field":"f","operator":"contains","value":1}]}"#,
            "unsupported policy operator",
        ),
        (
            r#"{"scope":"hazmat","action":"reject","priority":1,"effective_at":0,
                "predicates":[{"scope":"customer","field":"f","operator":"exists"}]}"#,
            "share the rule's own scope",
        ),
        (
            r#"{"scope":"hazmat","action":"reject","priority":1,"effective_at":0,
                "predicates":[{"scope":"hazmat","field":"","operator":"exists"}]}"#,
            "field is required",
        ),
        (
            r#"{"scope":"hazmat","action":"reject","priority":1,"effective_at":0,
                "predicates":[{"scope":"hazmat","field":"f","operator":"equals"}]}"#,
            "requires a value",
        ),
        (
            r#"{"scope":"hazmat","action":"reject","priority":1,"effective_at":0,
                "predicates":[]}"#,
            "at least one predicate",
        ),
    ] {
        let error = evaluate_policy_json(&call(&rule(version), ask)).unwrap_err();
        assert!(
            error.to_string().contains(fragment),
            "expected {fragment:?} in {error}"
        );
    }

    let valid = rule(
        r#"{"scope":"hazmat","action":"reject","priority":1,"effective_at":0,
            "predicates":[{"scope":"hazmat","field":"f","operator":"exists"}]}"#,
    );
    for (request, fragment) in [
        (
            r#"{"scope":"atlantis","context":{},"as_of":0}"#,
            "unsupported policy scope",
        ),
        (
            r#"{"scope":"hazmat","context":[],"as_of":0}"#,
            "expected an object",
        ),
        (r#"{"scope":"hazmat","context":{}}"#, "exactly one of"),
        (
            r#"{"scope":"hazmat","context":{},"rule_versions":[["r"]]}"#,
            "[rule_id, version] pair",
        ),
        (
            r#"{"scope":"hazmat","context":{},"rule_versions":[["r",1],["r",1]]}"#,
            "same rule id twice",
        ),
    ] {
        let error = evaluate_policy_json(&call(&valid, request)).unwrap_err();
        assert!(
            error.to_string().contains(fragment),
            "expected {fragment:?} in {error}"
        );
    }
}

#[test]
fn a_malformed_catalog_document_or_request_is_named() {
    let call =
        |document: &str, request: &str| format!(r#"{{"document":{document},"request":{request}}}"#);
    let catalog =
        |versions: &str| format!(r#"{{"catalogs":[{{"catalog_id":"c","versions":{versions}}}]}}"#);
    let ask = r#"{"catalog_id":"c","resolved_at":1}"#;

    for (versions, fragment) in [
        (
            r#"[{"effective_at":0,"published_at":0,"snapshot":{"items":[
               {"id":"i","dimensions_mm":[1,1],"weight_g":1}]}}]"#,
            "exactly 3 axes",
        ),
        (
            r#"[{"effective_at":0,"published_at":0,"snapshot":{"items":[
               {"id":"i","dimensions_mm":[0,1,1],"weight_g":1}]}}]"#,
            "item dimensions must be positive",
        ),
        (
            r#"[{"effective_at":0,"published_at":0,"snapshot":{"items":[
               {"id":"","dimensions_mm":[1,1,1],"weight_g":1}]}}]"#,
            "item id is required",
        ),
        (
            r#"[{"effective_at":0,"published_at":0,"snapshot":{"cartons":[
               {"id":"c","inner_dimensions_mm":[1,1,1],"max_payload_g":0}]}}]"#,
            "carton max_payload_g must be positive",
        ),
        (
            r#"[{"effective_at":0,"published_at":0,"snapshot":{"pallets":[
               {"id":"p","deck_dimensions_mm":[1,1,1],"max_payload_g":1}]}}]"#,
            "exactly 2 axes",
        ),
        (
            r#"[{"effective_at":0,"published_at":0,"snapshot":{"pallets":[
               {"id":"p","deck_dimensions_mm":[1,1],"max_payload_g":1,
                "max_stack_height_mm":0}]}}]"#,
            "max_stack_height_mm must be positive",
        ),
        (
            r#"[{"effective_at":0,"published_at":0,"snapshot":{"exclusions":[
               {"id":"x","scope":"item_wheelbarrow","subject_id":"a","excluded_id":"b"}]}}]"#,
            "unsupported exclusion scope",
        ),
        (
            r#"[{"effective_at":0,"published_at":0,"snapshot":{"exclusions":[
               {"id":"x","scope":"item_carton","subject_id":"","excluded_id":"b"}]}}]"#,
            "must reference both",
        ),
        (
            r#"[{"effective_at":0,"published_at":0,"snapshot":{"overrides":[
               {"id":"o","facility_id":"F","entry_id":"e","kind":"crate",
                "override":{"id":"e","dimensions_mm":[1,1,1],"weight_g":1}}]}}]"#,
            "expected one of",
        ),
        (
            r#"[{"effective_at":0,"published_at":0,"snapshot":{"overrides":[
               {"id":"o","facility_id":"F","entry_id":"other","kind":"item",
                "override":{"id":"e","dimensions_mm":[1,1,1],"weight_g":1}}]}}]"#,
            "entry_id must match",
        ),
        (
            r#"[{"effective_at":0,"published_at":0,"snapshot":{"overrides":[
               {"id":"o","facility_id":"","entry_id":"e","kind":"item",
                "override":{"id":"e","dimensions_mm":[1,1,1],"weight_g":1}}]}}]"#,
            "facility_id is required",
        ),
        (
            r#"[{"effective_at":0,"published_at":0,"snapshot":{"items":[
               {"id":"d","dimensions_mm":[1,1,1],"weight_g":1},
               {"id":"d","dimensions_mm":[1,1,1],"weight_g":1}]}}]"#,
            "duplicate item ids",
        ),
        (
            r#"[{"effective_at":-1,"published_at":0,"snapshot":{}}]"#,
            "cannot be negative",
        ),
        (
            r#"[{"rollback_to":2,"published_at":1}]"#,
            "not published yet",
        ),
    ] {
        let error = catalog_version_info_json(&call(&catalog(versions), ask)).unwrap_err();
        assert!(
            error.to_string().contains(fragment),
            "expected {fragment:?} in {error}"
        );
    }

    let one = catalog(r#"[{"effective_at":0,"published_at":0,"snapshot":{}}]"#);
    let error = catalog_version_info_json(&call(
        &one,
        r#"{"catalog_id":"c","resolved_at":1,"version":1,"as_of":1}"#,
    ))
    .unwrap_err();
    assert!(error.to_string().contains("at most one of"), "{error}");
}

#[test]
fn an_override_of_every_kind_is_admitted() {
    for (kind, payload) in [
        ("item", r#"{"id":"e","dimensions_mm":[1,1,1],"weight_g":1}"#),
        (
            "carton",
            r#"{"id":"e","inner_dimensions_mm":[1,1,1],"max_payload_g":1}"#,
        ),
        (
            "pallet",
            r#"{"id":"e","deck_dimensions_mm":[1,1],"max_payload_g":1}"#,
        ),
    ] {
        let document = format!(
            r#"{{"catalogs":[{{"catalog_id":"c","versions":[{{"effective_at":0,
               "published_at":0,"snapshot":{{"overrides":[{{"id":"o","facility_id":"F",
               "entry_id":"e","kind":"{kind}","override":{payload}}}]}}}}]}}]}}"#
        );
        let answer = catalog_version_info_json(&format!(
            r#"{{"document":{document},"request":{{"catalog_id":"c","resolved_at":1}}}}"#
        ))
        .unwrap_or_else(|error| panic!("a {kind} override is legal: {error}"));

        assert!(answer.contains(r#""overrides":1"#), "{answer}");
    }
}
