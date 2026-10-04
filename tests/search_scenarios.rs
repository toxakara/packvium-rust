//! Small scenes that steer the extreme-point search into branches the golden corpus never
//! reaches: route stops mixed with items that ride the whole route, covered ground contact,
//! a candidate-point budget smaller than the scene, an axle container whose floor fills up,
//! a hull beside an obstacle, grouped items under the beam, and dimensional weight in feet.
//!
//! Every scene goes through `pack_json`, which refuses any result its independent validator
//! rejects, so "it solved" already asserts the answer is valid; each test adds the property
//! its scene exists for. Counted work bounds every search and the time limit is only a fuse.

use packvium_core::pack_json;
use serde_json::{Value, json};

fn solve(mut request: Value) -> Value {
    let configuration = request
        .as_object_mut()
        .expect("request object")
        .entry("configuration")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .expect("configuration object");
    configuration.insert("time_limit_ms".into(), json!(60_000));
    configuration.entry("effort_budget").or_insert_with(|| {
        json!({
            "max_candidates_evaluated": 200_000,
            "max_placement_attempts": 200_000,
            "max_search_nodes": 200_000
        })
    });
    let output =
        pack_json(&request.to_string()).unwrap_or_else(|error| panic!("{request}: {error}"));
    serde_json::from_str(&output).expect("result JSON")
}

fn cube(id: &str, side: u32, quantity: u32) -> Value {
    json!({ "id": id, "quantity": quantity, "weight": "100",
            "dimensions": { "length": side.to_string(), "width": side.to_string(),
                            "height": side.to_string() } })
}

fn placed_count(result: &Value) -> usize {
    result["containers"]
        .as_array()
        .expect("containers")
        .iter()
        .map(|container| container["placements"].as_array().map_or(0, Vec::len))
        .sum()
}

#[test]
fn stops_share_a_van_with_items_that_ride_the_whole_route() {
    let mut first = cube("first", 50, 2);
    first["stop_index"] = json!(1);
    let mut second = cube("second", 50, 2);
    second["stop_index"] = json!(2);
    let result = solve(json!({
        "items": [first, second, cube("rider", 50, 2)],
        "containers": [{ "id": "van", "access_directions": ["-x"],
                         "inner_dimensions": { "length": "200", "width": "50", "height": "100" } }]
    }));
    assert_eq!(placed_count(&result), 6, "{result}");
}

#[test]
fn a_covered_item_stands_only_on_full_corner_support() {
    let mut lid = cube("lid", 100, 1);
    lid["ground_contact_rule"] = json!("covered");
    lid["dimensions"]["height"] = json!("10");
    let result = solve(json!({
        "items": [cube("base", 100, 1), lid],
        "containers": [{ "id": "box",
                         "inner_dimensions": { "length": "100", "width": "100", "height": "110" } }]
    }));
    assert_eq!(placed_count(&result), 2, "{result}");
}

#[test]
fn a_candidate_budget_smaller_than_the_scene_still_packs() {
    let items = (1..=8)
        .map(|index| cube(&format!("c{index}"), 10 + index * 3, 1))
        .collect::<Vec<_>>();
    let result = solve(json!({
        "items": items,
        "containers": [{ "id": "box",
                         "inner_dimensions": { "length": "120", "width": "120", "height": "120" } }],
        "configuration": { "max_candidate_points": 65 }
    }));
    assert_eq!(placed_count(&result), 8, "{result}");
}

#[test]
fn an_axle_container_keeps_packing_once_its_floor_is_full() {
    let result = solve(json!({
        "items": [cube("crate", 50, 8)],
        "containers": [{ "id": "trailer",
                         "inner_dimensions": { "length": "100", "width": "100", "height": "100" },
                         "axles": [{ "position": "10", "max_load": "100000" },
                                   { "position": "90", "max_load": "100000" }] }]
    }));
    assert_eq!(placed_count(&result), 8, "{result}");
}

#[test]
fn a_hull_is_kept_clear_of_an_obstacle() {
    let hull = json!({
        "id": "wedge", "quantity": 2, "shape_type": "convex_hull",
        "dimensions": { "length": "40", "width": "40", "height": "40" },
        "hull_vertices": [
            { "x": "0", "y": "0", "z": "0" }, { "x": "40", "y": "0", "z": "0" },
            { "x": "0", "y": "40", "z": "0" }, { "x": "0", "y": "0", "z": "40" }
        ]
    });
    let result = solve(json!({
        "items": [hull],
        "containers": [{ "id": "box",
                         "inner_dimensions": { "length": "100", "width": "40", "height": "40" },
                         "obstacles": [{ "id": "pillar", "origin": { "x": "35" },
                                         "dimensions": { "length": "10", "width": "10", "height": "40" } }] }]
    }));
    assert_eq!(placed_count(&result), 2, "{result}");
}

#[test]
fn a_group_is_packed_together_under_the_beam_or_not_at_all() {
    let mut kit = cube("kit", 50, 3);
    kit["group"] = json!("set");
    let result = solve(json!({
        "items": [kit, cube("filler", 50, 1)],
        "containers": [{ "id": "box",
                         "inner_dimensions": { "length": "100", "width": "50", "height": "50" } }],
        "configuration": { "solver_profile": "quality", "max_containers": 1 }
    }));
    let unpacked = result["unpacked_items"].as_array().expect("unpacked");
    assert!(
        unpacked.iter().all(|item| item["item_type"] == "kit"),
        "{result}"
    );
}

#[test]
fn quality_container_plans_respect_payload_node_limits_and_empty_trials() {
    let mut heavy = cube("heavy", 40, 4);
    heavy["weight"] = json!("30000");
    let blocked = json!({
        "id": "blocked", "inner_dimensions": { "length": "100", "width": "100", "height": "100" },
        "max_payload": "1000000",
        "obstacles": [{ "id": "fill",
                        "dimensions": { "length": "100", "width": "100", "height": "100" } }]
    });
    let open = json!({
        "id": "open", "inner_dimensions": { "length": "100", "width": "100", "height": "100" },
        "max_payload": "70000"
    });
    for node_limit in [1, 100] {
        let result = solve(json!({
            "items": [heavy],
            "containers": [blocked, open],
            "configuration": { "solver_profile": "quality",
                               "container_plan_node_limit": node_limit }
        }));
        assert!(
            result["containers"]
                .as_array()
                .expect("containers")
                .iter()
                .all(|container| container["container_type"] == "open"),
            "{result}"
        );
    }
}

#[test]
fn dimensional_weight_can_be_measured_in_feet() {
    let result = solve(json!({
        "items": [cube("parcel", 100, 1)],
        "containers": [{ "id": "carton",
                         "inner_dimensions": { "length": "200", "width": "200", "height": "200" },
                         "rate_table": { "weight_brackets_g": [1000000], "prices_minor": [500] } }],
        "configuration": { "objective": "shipping_cost", "dimensional_weight_divisor": 139,
                           "dimensional_weight_length_unit": "ft" }
    }));
    assert_eq!(result["objective"], "shipping_cost");
}
