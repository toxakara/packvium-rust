//! A malformed request is refused as one structured error: a code, a reason from a closed
//! set, the JSON Pointer of the bad value, and the same message the other engines give.
//!
//! The cross-engine contract is `conformance/request_errors/run.py`, which compares every
//! case with the Python reference; this suite pins representative reasons from outside
//! through `pack_json`, and the boundaries of what is and is not wrapped.

use packvium_core::{PackError, RequestError, pack_json};
use serde_json::{Value, json};

fn base() -> Value {
    json!({
        "items": [{ "id": "cube", "quantity": 2, "weight": "1000",
                    "dimensions": { "length": "100", "width": "100", "height": "100" } }],
        "containers": [{ "id": "box", "quantity": 2,
                         "inner_dimensions": { "length": "200", "width": "100", "height": "200" } }],
        "configuration": { "solver_profile": "fast" }
    })
}

/// One change that breaks one rule of the base request.
type Edit = fn(&mut Value);

fn refused(request: &Value) -> RequestError {
    match pack_json(&request.to_string()) {
        Err(PackError::InvalidRequest(error)) => error,
        Err(other) => panic!("refused, but not as a request error: {other}"),
        Ok(_) => panic!("{request} was admitted"),
    }
}

fn outcome(error: &RequestError) -> (&str, &str, &str, String) {
    (
        error.code(),
        error.reason(),
        error.field(),
        error.to_string(),
    )
}

#[test]
fn the_base_request_is_admitted() {
    pack_json(&base().to_string()).expect("the base request packs");
}

#[test]
fn the_error_carries_code_reason_field_and_detail() {
    let mut request = base();
    request["items"][0]["quantity"] = json!(0);
    let error = refused(&request);
    assert_eq!(error.code(), "invalid_request");
    assert_eq!(error.reason(), "below_minimum");
    assert_eq!(error.field(), "/items/0/quantity");
    assert_eq!(error.detail(), "must be at least 1");
    assert_eq!(
        error.to_string(),
        "invalid_request: /items/0/quantity: must be at least 1"
    );
}

#[test]
fn a_request_that_is_not_an_object_names_no_field() {
    let error = refused(&json!([base()]));
    assert_eq!(
        outcome(&error),
        (
            "invalid_request",
            "wrong_type",
            "",
            "invalid_request: must be an object".to_owned()
        )
    );
}

#[test]
fn representative_reasons_name_the_value_at_fault() {
    let cases: Vec<(Edit, &str, &str, &str)> = vec![
        (
            |r| {
                r.as_object_mut().unwrap().remove("items");
            },
            "missing_field",
            "/items",
            "is required",
        ),
        (
            |r| r["units"] = json!({ "length": "furlong" }),
            "invalid_unit",
            "/units/length",
            r#"has an unknown unit "furlong""#,
        ),
        (
            |r| r["containers"][0]["inner_dimensions"]["width"] = json!("-1"),
            "negative_measure",
            "/containers/0/inner_dimensions/width",
            "cannot be negative",
        ),
        (
            |r| r["items"][0]["dimensions"]["length"] = json!(10.5),
            "wrong_type",
            "/items/0/dimensions/length",
            "must be a measure",
        ),
        (
            |r| {
                r["configuration"]["top_k"] = json!(2);
                r["configuration"]["profile"] = json!("balanced");
            },
            "not_allowed",
            "/configuration/profile",
            "is not a known field",
        ),
        (
            |r| r["configuration"]["effort_budget"] = json!({"max_nodes": 1}),
            "not_allowed",
            "/configuration/effort_budget/max_nodes",
            "is not a known field",
        ),
        (
            |r| r["configuration"]["solver_profile"] = json!("fastest"),
            "not_allowed",
            "/configuration/solver_profile",
            r#"must be one of ["fast","balanced","quality","exact_small"]"#,
        ),
        (
            |r| r["items"][0]["quantity"] = json!(9_007_199_254_740_992_u64),
            "above_maximum",
            "/items/0/quantity",
            "must be at most 9007199254740991",
        ),
        (
            |r| r["items"][0]["minimum_support_ratio"] = json!(1.5),
            "above_maximum",
            "/items/0/minimum_support_ratio",
            "must be at most 1",
        ),
        (
            |r| r["configuration"]["effort_budget"] = json!({ "max_search_nodes": "100" }),
            "wrong_type",
            "/configuration/effort_budget/max_search_nodes",
            "must be an integer",
        ),
        (
            |r| r["containers"][0]["tag_limits"] = json!({ "a/b~c": 0 }),
            "below_minimum",
            "/containers/0/tag_limits/a~1b~0c",
            "must be at least 1",
        ),
    ];
    for (edit, reason, field, detail) in cases {
        let mut request = base();
        edit(&mut request);
        let error = refused(&request);
        assert_eq!(
            (error.reason(), error.field(), error.detail()),
            (reason, field, detail),
            "{request}"
        );
    }
}

#[test]
fn a_repeated_id_names_the_later_entry_once_every_entry_is_well_formed() {
    let mut request = base();
    let copy = request["items"][0].clone();
    request["items"].as_array_mut().unwrap().push(copy);
    let error = refused(&request);
    assert_eq!(
        (error.reason(), error.field(), error.detail()),
        ("duplicate_id", "/items/1/id", r#"repeats the id "cube""#)
    );
}

#[test]
fn tag_limits_are_checked_in_code_point_order() {
    let mut request = base();
    request["containers"][0]["tag_limits"] = json!({ "b": 0, "10": 0 });
    assert_eq!(refused(&request).field(), "/containers/0/tag_limits/10");
}

#[test]
fn units_are_checked_before_configuration_and_items() {
    let mut request = base();
    request["items"][0]["quantity"] = json!(0);
    request["configuration"]["time_limit_ms"] = json!(0);
    request["units"] = json!({ "length": 7 });
    assert_eq!(refused(&request).field(), "/units/length");
    request.as_object_mut().unwrap().remove("units");
    assert_eq!(refused(&request).field(), "/configuration/time_limit_ms");
}

#[test]
fn a_fixed_placement_refusal_keeps_its_message_and_gains_reason_and_field() {
    let mut request = base();
    request["fixed_placements"] = json!([{
        "item_type": "cube", "container_type": "box", "orientation": "LWH",
        "position": { "x": "0", "y": "-1" }
    }]);
    let error = refused(&request);
    assert_eq!(
        outcome(&error),
        (
            "invalid_fixed_placement",
            "malformed",
            "/fixed_placements/0/position/y",
            "invalid_fixed_placement: fixed_placements[0].position.y cannot be negative".to_owned()
        )
    );
    request["fixed_placements"][0]["position"] = json!({ "x": "ten" });
    assert_eq!(
        refused(&request).to_string(),
        "invalid_fixed_placement: fixed_placements[0].position.x is a measure"
    );
    request["fixed_placements"][0]["position"] = json!({ "x": "0" });
    request["fixed_placements"][0]["item_type"] = json!("crate");
    let error = refused(&request);
    assert_eq!(
        outcome(&error),
        (
            "invalid_fixed_placement",
            "cannot_hold",
            "/fixed_placements",
            r#"invalid_fixed_placement: unknown item type "crate""#.to_owned()
        )
    );
}

#[test]
fn a_parse_failure_the_rules_do_not_name_is_an_invalid_value_without_a_field() {
    let mut request = base();
    request["items"][0]["shape_type"] = json!("sphere");
    let error = refused(&request);
    assert_eq!(
        (error.code(), error.reason(), error.field()),
        ("invalid_request", "invalid_value", "")
    );
    assert!(error.detail().contains("item.shape_type"), "{error}");
}

#[test]
fn an_unknown_objective_or_access_direction_is_refused_with_not_allowed_and_pointer() {
    let mut request = base();
    request["configuration"]["objective"] = json!("cheapest");
    let error = refused(&request);
    assert_eq!(
        (error.code(), error.reason(), error.field()),
        ("invalid_request", "not_allowed", "/configuration/objective")
    );

    let mut request = base();
    request["containers"][0]["access_directions"] = json!(["+w"]);
    let error = refused(&request);
    assert_eq!(
        (error.code(), error.reason(), error.field()),
        (
            "invalid_request",
            "not_allowed",
            "/containers/0/access_directions/0"
        )
    );

    let mut request = base();
    request["containers"][0]["access_directions"] = json!("+x");
    let error = refused(&request);
    assert_eq!(
        (error.code(), error.reason(), error.field()),
        (
            "invalid_request",
            "wrong_type",
            "/containers/0/access_directions"
        )
    );
}

#[test]
fn an_unsupported_feature_and_a_solver_precondition_are_not_relabelled() {
    let mut request = base();
    request["containers"][0]["pallet_overhang_limit"] = json!(10);
    assert!(matches!(
        pack_json(&request.to_string()),
        Err(PackError::UnsupportedFeature(_))
    ));
    let mut request = base();
    request["configuration"]["objective"] = json!("lowest_landed_cost");
    request["configuration"]["dimensional_weight_divisor"] = json!(5000);
    assert!(matches!(
        pack_json(&request.to_string()),
        Err(PackError::InvalidInput(message)) if message.contains("requires a rate_table")
    ));
}

fn reason_and_field(edit: Edit) -> (String, String, String) {
    let mut request = base();
    edit(&mut request);
    let error = refused(&request);
    (
        error.reason().to_owned(),
        error.field().to_owned(),
        error.detail().to_owned(),
    )
}

#[test]
fn every_primitive_rule_names_its_reason() {
    let cases: [(Edit, &str, &str, &str); 12] = [
        (
            |r| r["items"][0]["quantity"] = json!(9_223_372_036_854_775_808_u64),
            "above_maximum",
            "/items/0/quantity",
            "must be at most 9007199254740991",
        ),
        (
            |r| r["items"][0]["quantity"] = json!(1e20),
            "above_maximum",
            "/items/0/quantity",
            "must be at most 9007199254740991",
        ),
        (
            |r| r["items"][0]["quantity"] = json!(-1e20),
            "below_minimum",
            "/items/0/quantity",
            "must be at least 1",
        ),
        (
            |r| r["items"][0]["quantity"] = json!(1.5),
            "wrong_type",
            "/items/0/quantity",
            "must be an integer",
        ),
        (
            |r| r["items"][0]["minimum_support_ratio"] = json!("half"),
            "wrong_type",
            "/items/0/minimum_support_ratio",
            "must be a number",
        ),
        (
            |r| r["configuration"]["minimum_support_ratio"] = json!(-0.5),
            "below_minimum",
            "/configuration/minimum_support_ratio",
            "must be at least 0",
        ),
        (
            |r| r["items"][0]["weight"] = json!(true),
            "wrong_type",
            "/items/0/weight",
            "must be a measure",
        ),
        (
            |r| r["items"][0]["weight"] = json!("heavy"),
            "wrong_type",
            "/items/0/weight",
            "must be a measure",
        ),
        (
            |r| r["items"][0]["weight"] = json!({ "unit": "kg" }),
            "wrong_type",
            "/items/0/weight",
            "must be a measure",
        ),
        (
            |r| r["items"][0]["weight"] = json!({ "value": "1", "unit": null }),
            "invalid_unit",
            "/items/0/weight",
            "has an unknown unit null",
        ),
        (
            |r| r["containers"][0]["id"] = json!(7),
            "wrong_type",
            "/containers/0/id",
            "must be a string",
        ),
        (
            |r| r["configuration"]["clearance"] = json!("-2"),
            "negative_measure",
            "/configuration/clearance",
            "cannot be negative",
        ),
    ];
    for (edit, reason, field, detail) in cases {
        assert_eq!(
            reason_and_field(edit),
            (reason.to_owned(), field.to_owned(), detail.to_owned())
        );
    }
}

#[test]
fn a_units_object_without_a_length_and_an_empty_fixed_list_are_accepted() {
    let mut request = base();
    request["units"] = json!({});
    request["fixed_placements"] = json!([]);
    assert!(pack_json(&request.to_string()).is_ok());
}

#[test]
fn a_fixed_coordinate_that_is_not_a_length_is_malformed() {
    for coordinate in [json!(1.5), json!({ "value": "1", "unit": 5 })] {
        let mut request = base();
        request["fixed_placements"] = json!([{ "item_type": "cube", "container_type": "box",
            "orientation": "LWH", "position": { "x": coordinate } }]);
        let error = refused(&request);
        assert_eq!(
            outcome(&error),
            (
                "invalid_fixed_placement",
                "malformed",
                "/fixed_placements/0/position/x",
                "invalid_fixed_placement: fixed_placements[0].position.x is a measure".to_owned()
            )
        );
    }
}
