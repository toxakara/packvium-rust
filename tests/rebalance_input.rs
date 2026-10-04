//! `rebalance_json` reads a result document the caller hands back, so every malformed
//! shape of that document is refused with an input error naming what is wrong, and a
//! well-formed one round-trips through the reader without losing any item.

use packvium_core::{PackError, pack_json, rebalance_json};
use serde_json::{Value, json};

fn request() -> Value {
    json!({
        "items": [{ "id": "brick", "quantity": 2, "weight": "1000",
                    "dimensions": { "length": "100", "width": "100", "height": "100" } }],
        "containers": [{ "id": "box",
                         "inner_dimensions": { "length": "200", "width": "100", "height": "100" } }],
        "configuration": {
            "time_limit_ms": 60000,
            "effort_budget": { "max_candidates_evaluated": 100000,
                               "max_placement_attempts": 100000,
                               "max_search_nodes": 100000 }
        }
    })
}

fn packed() -> Value {
    serde_json::from_str(&pack_json(&request().to_string()).expect("the request packs"))
        .expect("result JSON")
}

fn refusal(result: &Value) -> String {
    match rebalance_json(&request().to_string(), &result.to_string(), 4) {
        Err(PackError::InvalidInput(message)) => message,
        Err(other) => panic!("refused, but not as invalid input: {other}"),
        Ok(output) => panic!("{result} was accepted: {output}"),
    }
}

fn placement(result: &mut Value) -> &mut Value {
    &mut result["containers"][0]["placements"][0]
}

#[test]
fn a_packed_result_rebalances_and_keeps_its_containers() {
    let mut result = packed();
    result["status"] = json!("optimal");
    placement(&mut result)["position"]["x"] = json!({ "ticks": 0 });
    let output: Value = serde_json::from_str(
        &rebalance_json(&request().to_string(), &result.to_string(), 4).expect("rebalances"),
    )
    .expect("rebalance JSON");
    assert_eq!(output["containers"].as_array().map(Vec::len), Some(1));
    assert_eq!(output["improved"], json!(false));
}

#[test]
fn every_status_spelling_is_read_back() {
    for status in [
        "optimal",
        "best_found",
        "time_limit",
        "infeasible",
        "invalid_result",
    ] {
        let mut result = packed();
        result["status"] = json!(status);
        rebalance_json(&request().to_string(), &result.to_string(), 0)
            .unwrap_or_else(|error| panic!("{status}: {error}"));
    }
}

#[test]
fn the_result_and_its_containers_must_be_objects() {
    assert_eq!(refusal(&json!([])), "result must be an object");
    assert_eq!(
        refusal(&json!({ "containers": {} })),
        "result.containers must be an array"
    );
    assert_eq!(
        refusal(&json!({ "containers": [1] })),
        "result.containers[0] must be an object"
    );
}

#[test]
fn a_container_must_name_a_known_type_and_list_materialized_placements() {
    let mut lattice = packed();
    lattice["containers"][0]["lattice_summary"] = json!({});
    assert_eq!(
        refusal(&lattice),
        "rebalance requires materialized placement coordinates"
    );

    let mut untyped = packed();
    untyped["containers"][0]
        .as_object_mut()
        .expect("container object")
        .remove("container_type");
    assert_eq!(
        refusal(&untyped),
        "result.containers[0].container_type is required"
    );

    let mut unknown = packed();
    unknown["containers"][0]["container_type"] = json!("crate");
    assert_eq!(refusal(&unknown), "unknown result container type \"crate\"");

    let mut unlisted = packed();
    unlisted["containers"][0]["placements"] = json!({});
    assert_eq!(
        refusal(&unlisted),
        "result.containers[0].placements must be an array"
    );

    let mut scalar = packed();
    scalar["containers"][0]["placements"][0] = json!(7);
    assert_eq!(
        refusal(&scalar),
        "result.containers[0].placements[0] must be an object"
    );
}

#[test]
fn a_placement_must_reference_each_requested_instance_once() {
    let mut anonymous = packed();
    placement(&mut anonymous)
        .as_object_mut()
        .expect("placement object")
        .remove("item_id");
    assert_eq!(refusal(&anonymous), "placement.item_id is required");

    let mut duplicated = packed();
    let first = duplicated["containers"][0]["placements"][0]["item_id"].clone();
    duplicated["containers"][0]["placements"][1]["item_id"] = first.clone();
    assert_eq!(
        refusal(&duplicated),
        format!("result duplicates or references unknown item instance {first}")
    );

    let mut missing = packed();
    missing["containers"][0]["placements"]
        .as_array_mut()
        .expect("placements array")
        .pop();
    assert_eq!(
        refusal(&missing),
        "result does not account for every requested item instance"
    );
}

#[test]
fn a_placement_position_must_be_exact_integer_ticks() {
    for (edit, expected) in [
        (json!(null), "placement.position is required"),
        (json!(3), "placement.position must be an object"),
        (
            json!({ "y": 0, "z": 0 }),
            "placement.position.x is required",
        ),
        (
            json!({ "x": 0, "z": 0 }),
            "placement.position.y is required",
        ),
        (
            json!({ "x": 0, "y": 0 }),
            "placement.position.z is required",
        ),
        (
            json!({ "x": 0.5, "y": 0, "z": 0 }),
            "placement.position.x must contain exact integer ticks",
        ),
        (
            json!({ "x": "half", "y": 0, "z": 0 }),
            "placement.position.x must contain exact integer ticks",
        ),
        (
            json!({ "x": true, "y": 0, "z": 0 }),
            "placement.position.x must contain exact integer ticks",
        ),
        (
            json!({ "x": 0, "y": [], "z": 0 }),
            "placement.position.y must contain exact integer ticks",
        ),
        (
            json!({ "x": 0, "y": 0, "z": [] }),
            "placement.position.z must contain exact integer ticks",
        ),
    ] {
        let mut result = packed();
        if edit.is_null() {
            placement(&mut result)
                .as_object_mut()
                .expect("placement object")
                .remove("position");
        } else {
            placement(&mut result)["position"] = edit;
        }
        assert_eq!(refusal(&result), expected);
    }
}

#[test]
fn a_placement_needs_dimensions_an_orientation_and_an_exact_top_load() {
    for (edit, expected) in [
        (json!(null), "placement.dimensions is required"),
        (json!(3), "placement.dimensions must be an object"),
        (
            json!({ "width": 1, "height": 1 }),
            "placement.dimensions.length is required",
        ),
        (
            json!({ "length": 1, "height": 1 }),
            "placement.dimensions.width is required",
        ),
        (
            json!({ "length": 1, "width": 1 }),
            "placement.dimensions.height is required",
        ),
        (
            json!({ "length": [], "width": 1, "height": 1 }),
            "placement.dimensions.length must contain exact integer ticks",
        ),
        (
            json!({ "length": 1, "width": [], "height": 1 }),
            "placement.dimensions.width must contain exact integer ticks",
        ),
        (
            json!({ "length": 1, "width": 1, "height": [] }),
            "placement.dimensions.height must contain exact integer ticks",
        ),
    ] {
        let mut result = packed();
        if edit.is_null() {
            placement(&mut result)
                .as_object_mut()
                .expect("placement object")
                .remove("dimensions");
        } else {
            placement(&mut result)["dimensions"] = edit;
        }
        assert_eq!(refusal(&result), expected);
    }

    let mut unoriented = packed();
    placement(&mut unoriented)["orientation"] = json!("diagonal");
    assert_eq!(refusal(&unoriented), "placement.orientation is invalid");

    let mut fractional_load = packed();
    placement(&mut fractional_load)["top_load"] = json!([]);
    assert_eq!(
        refusal(&fractional_load),
        "placement.top_load must contain exact integer ticks"
    );
}

#[test]
fn unpacked_items_are_read_back_with_their_reason_and_details() {
    let mut result = packed();
    let moved = result["containers"][0]["placements"]
        .as_array_mut()
        .expect("placements array")
        .pop()
        .expect("two placements");
    result["unpacked_items"] = json!([{
        "item_id": moved["item_id"],
        "details": ["kept aside", 3],
    }]);
    let output: Value = serde_json::from_str(
        &rebalance_json(&request().to_string(), &result.to_string(), 4).expect("rebalances"),
    )
    .expect("rebalance JSON");
    assert_eq!(output["improved"], json!(false));
}

#[test]
fn a_malformed_unpacked_item_is_refused() {
    let mut scalar = packed();
    scalar["unpacked_items"] = json!([1]);
    assert_eq!(refusal(&scalar), "unpacked item must be an object");

    let mut anonymous = packed();
    anonymous["unpacked_items"] = json!([{ "reason": "search_exhausted" }]);
    assert_eq!(refusal(&anonymous), "unpacked item_id is required");

    let mut unknown = packed();
    unknown["unpacked_items"] = json!([{ "item_id": "ghost#1" }]);
    assert_eq!(
        refusal(&unknown),
        "result duplicates or references unknown item instance \"ghost#1\""
    );
}

#[test]
fn either_document_that_is_not_json_is_refused() {
    let result = packed().to_string();
    for (request, result) in [("{", result.as_str()), (&request().to_string(), "{")] {
        assert!(matches!(
            rebalance_json(request, result, 4),
            Err(PackError::Serialization(_))
        ));
    }
    let mut empty = request();
    empty["items"] = json!([]);
    assert!(rebalance_json(&empty.to_string(), &result, 4).is_err());
}

#[test]
fn a_support_ratio_written_as_text_is_read_back() {
    let mut result = packed();
    placement(&mut result)["support_ratio"] = json!("0.5");
    rebalance_json(&request().to_string(), &result.to_string(), 4).expect("rebalances");
}
