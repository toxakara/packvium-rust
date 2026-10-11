//! One reason code per container, axle and stop limit, and a restart
//! budget that changes the plan.
//!
//! An axle that refuses an item alone in every container otherwise admitting it is
//! `axle_overloaded`; every other limit only refuses an item a fresh container would take,
//! so once stock or `max_containers` runs out the item is `container_inventory_exhausted`.
//! The same scenes are pinned across all four engines in `conformance/unpacked-reasons/`.

use serde_json::{Value, json};

fn solve(items: Value, containers: Value, configuration: Value) -> Value {
    let mut config = json!({ "solver_profile": "fast", "time_limit_ms": 300000 });
    for (key, value) in configuration.as_object().into_iter().flatten() {
        config[key] = value.clone();
    }
    let request = json!({
        "units": { "length": "mm" },
        "configuration": config,
        "items": items,
        "containers": containers,
    });
    let output = packvium_core::pack_json(&request.to_string()).expect("the scene packs");
    serde_json::from_str(&output).expect("the result is JSON")
}

fn reasons(result: &Value) -> Vec<(String, String)> {
    let mut reasons: Vec<(String, String)> = result["unpacked_items"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|item| {
            (
                item["reason"].as_str().unwrap_or_default().to_owned(),
                item["proof"]["level"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned(),
            )
        })
        .collect();
    reasons.sort();
    reasons
}

fn cube(id: &str, quantity: u32, side: &str, fields: Value) -> Value {
    let mut item = json!({ "id": id, "quantity": quantity,
                           "dimensions": { "length": side, "width": side, "height": side } });
    for (key, value) in fields.as_object().into_iter().flatten() {
        item[key] = value.clone();
    }
    item
}

fn container(id: &str, sides: [&str; 3], fields: Value) -> Value {
    let mut container = json!({ "id": id, "inner_dimensions":
        { "length": sides[0], "width": sides[1], "height": sides[2] } });
    for (key, value) in fields.as_object().into_iter().flatten() {
        container[key] = value.clone();
    }
    container
}

fn axles(front: &str, rear: &str) -> Value {
    json!([{ "position": "0", "max_load": { "value": front, "unit": "g" } },
           { "position": "100", "max_load": { "value": rear, "unit": "g" } }])
}

fn inventory() -> Vec<(String, String)> {
    vec![("container_inventory_exhausted".into(), "inferred".into())]
}

#[test]
fn a_limit_a_fresh_container_would_lift_is_the_inventory() {
    let grams = |value: &str| json!({ "value": value, "unit": "g" });
    let cases = [
        (
            json!([cube("cube", 2, "10", json!({}))]),
            json!([container(
                "box",
                ["10", "10", "10"],
                json!({ "quantity": 1 })
            )]),
            json!({}),
        ),
        (
            json!([cube("cube", 2, "10", json!({}))]),
            json!([container("box", ["10", "10", "10"], json!({}))]),
            json!({ "max_containers": 1 }),
        ),
        (
            json!([cube("cube", 2, "10", json!({}))]),
            json!([container(
                "box",
                ["20", "20", "20"],
                json!({ "quantity": 1, "max_items": 1 })
            )]),
            json!({}),
        ),
        (
            json!([cube("cube", 2, "10", json!({ "weight": grams("600") }))]),
            json!([container(
                "box",
                ["20", "20", "20"],
                json!({ "quantity": 1, "max_payload": grams("1000") })
            )]),
            json!({}),
        ),
        (
            json!([cube("hazmat", 2, "10", json!({ "tags": ["hazmat"] }))]),
            json!([container(
                "box",
                ["20", "20", "20"],
                json!({ "quantity": 1, "tag_limits": { "hazmat": 1 } })
            )]),
            json!({}),
        ),
        (
            json!([
                cube("early", 1, "100", json!({ "stop_index": 0 })),
                cube(
                    "late",
                    1,
                    "100",
                    json!({ "stop_index": 1, "stackable": false })
                )
            ]),
            json!([container(
                "van",
                ["100", "100", "200"],
                json!({ "quantity": 1, "access_directions": ["+z"] })
            )]),
            json!({}),
        ),
        (
            json!([cube(
                "payload",
                2,
                "10",
                json!({ "weight": grams("500"), "allowed_rotations": ["LWH"] })
            )]),
            json!([container(
                "vehicle",
                ["100", "10", "10"],
                json!({ "quantity": 1, "axles": axles("600", "350") })
            )]),
            json!({}),
        ),
    ];
    for (index, (items, containers, configuration)) in cases.into_iter().enumerate() {
        assert_eq!(
            reasons(&solve(items, containers, configuration)),
            inventory(),
            "case {index}"
        );
    }
    let open_stock = solve(
        json!([cube("cube", 2, "10", json!({}))]),
        json!([container(
            "box",
            ["20", "20", "20"],
            json!({ "max_items": 1 })
        )]),
        json!({}),
    );
    assert!(reasons(&open_stock).is_empty());
}

#[test]
fn an_axle_that_refuses_the_item_alone_is_named() {
    // 1000 g over 800 g of axle capacity; then enough capacity, but the one centre a
    // full-length item can take overloads the front axle.
    for (length, front, rear) in [("10", "400", "400"), ("100", "400", "700")] {
        let item = json!({ "id": "load", "quantity": 1, "allowed_rotations": ["LWH"],
                           "weight": { "value": "1000", "unit": "g" },
                           "dimensions": { "length": length, "width": "10", "height": "10" } });
        let vehicle = container(
            "vehicle",
            ["100", "10", "10"],
            json!({ "axles": axles(front, rear) }),
        );
        assert_eq!(
            reasons(&solve(json!([item]), json!([vehicle]), json!({}))),
            vec![("axle_overloaded".to_owned(), "inferred".to_owned())],
            "{length}"
        );
    }
}

/// The shared conformance scene `feature-effort-restarts-first-start-loses` pins:
/// the first start leaves four items out, the full portfolio three.
#[test]
fn a_single_restart_packs_worse_than_the_full_portfolio() {
    let unpacked = |restarts: Option<u32>| {
        let mut budget = json!({ "max_candidates_evaluated": 1000000,
                                 "max_placement_attempts": 1000000, "max_search_nodes": 1000000 });
        if let Some(restarts) = restarts {
            budget["max_restarts"] = json!(restarts);
        }
        let result = solve(
            json!([
                { "id": "panel", "quantity": 4,
                  "dimensions": { "length": "20", "width": "25", "height": "50" } },
                { "id": "crate", "quantity": 4,
                  "dimensions": { "length": "60", "width": "40", "height": "40" } }
            ]),
            json!([container(
                "box",
                ["50", "100", "60"],
                json!({ "quantity": 1 })
            )]),
            json!({ "solver_profile": "balanced", "effort_budget": budget }),
        );
        result["unpacked_items"].as_array().map(Vec::len)
    };
    assert_eq!(unpacked(Some(1)), Some(4));
    assert_eq!(unpacked(None), Some(3));
}
