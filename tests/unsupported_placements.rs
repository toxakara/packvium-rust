//!: the default `minimum_support_ratio` of 0 lets a placement rest on nothing, and says so.
//!
//! Mirrors `packvium-python/tests/test_unsupported_placements.py`, measured through `pack_json`
//! because that is the path a request takes. The warning is asserted whole because it is
//! compared across engines.

use packvium_core::pack_json;
use serde_json::{Value, json};

fn warning(count: usize) -> String {
    format!(
        "unsupported_placements: {count} placement(s) rest on nothing above the container floor; \
         set configuration.minimum_support_ratio above 0 to require support"
    )
}

fn cube(id: &str, quantity: u64, height: &str) -> Value {
    json!({"id": id, "quantity": quantity, "weight": "500",
           "dimensions": {"length": "100", "width": "100", "height": height}})
}

fn fixed(item_type: &str, x: &str, y: &str, z: &str) -> Value {
    json!({"item_type": item_type, "container_type": "crate",
           "position": {"x": x, "y": y, "z": z}, "orientation": "LWH"})
}

/// Two stacked bases, plus a slab in mid-air and a slab on an obstacle when floating.
fn scene(floating: bool, minimum_support_ratio: f64) -> Value {
    let mut items = vec![cube("base", 2, "100"), cube("loose", 4, "100")];
    let mut placements = vec![fixed("base", "0", "0", "0"), fixed("base", "0", "0", "100")];
    if floating {
        items.push(cube("slab", 2, "50"));
        placements.push(fixed("slab", "100", "100", "300"));
        placements.push(fixed("slab", "200", "0", "100"));
    }
    json!({
        "items": items,
        "containers": [{
            "id": "crate", "quantity": 1,
            "inner_dimensions": {"length": "400", "width": "200", "height": "400"},
            "obstacles": [{"id": "shelf", "origin": {"x": "200", "y": "0", "z": "0"},
                           "dimensions": {"length": "100", "width": "100", "height": "100"}}]
        }],
        "fixed_placements": placements,
        "configuration": {"solver_profile": "quality", "alternatives": 3,
                          "effort_budget": {"max_search_nodes": 20000}, "time_limit_ms": 60000,
                          "minimum_support_ratio": minimum_support_ratio}
    })
}

fn pack(request: &Value) -> Value {
    serde_json::from_str(&pack_json(&request.to_string()).expect("the scene packs"))
        .expect("the result is JSON")
}

fn unsupported_warnings(result: &Value) -> Vec<String> {
    result["warnings"]
        .as_array()
        .expect("warnings is an array")
        .iter()
        .filter_map(Value::as_str)
        .filter(|line| line.starts_with("unsupported_placements:"))
        .map(str::to_owned)
        .collect()
}

#[test]
fn a_placement_in_mid_air_and_one_on_an_obstacle_are_both_counted() {
    let result = pack(&scene(true, 0.0));
    let warnings = result["warnings"].as_array().expect("warnings is an array");
    assert_eq!(warnings.last(), Some(&Value::String(warning(2))));
    assert_eq!(unsupported_warnings(&result).len(), 1);
}

#[test]
fn every_alternative_carries_its_own_warning() {
    let result = pack(&scene(true, 0.0));
    let alternatives = result["alternatives"].as_array().expect("alternatives");
    assert!(!alternatives.is_empty());
    for alternative in alternatives {
        assert_eq!(unsupported_warnings(alternative), vec![warning(2)]);
    }
}

#[test]
fn a_fully_supported_packing_has_no_warning() {
    assert!(unsupported_warnings(&pack(&scene(false, 0.0))).is_empty());
}

#[test]
fn requiring_support_silences_the_warning() {
    let result = pack(&scene(false, 0.5));
    assert_eq!(result["warnings"], json!([]));
}
