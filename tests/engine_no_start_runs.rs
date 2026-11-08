//! A solver selection none of whose members applies to the request: no start runs, and the
//! answer is still a result, built from the request alone.

use serde_json::{Value, json};

fn solve(request: Value) -> Value {
    let output = packvium_core::pack_json(&request.to_string()).expect("the request packs");
    serde_json::from_str(&output).expect("the result is JSON")
}

/// The lattice does not take a mixed load, the block solver stands down under a support
/// ratio, and with two of them selected neither falls back to a general search. Every item is then proven
/// out by the request alone, so the answer is `infeasible` -- and a bound says nothing about
/// an answer that packs nothing it could have packed.
#[test]
fn a_selection_that_runs_no_start_proves_an_oversized_load_infeasible() {
    let result = solve(json!({
        "units": {"length": "mm"},
        "configuration": {"solvers": ["grid", "homogeneous_blocks"], "minimum_support_ratio": 0.5,
                          "time_limit_ms": 300000,
                          "effort_budget": {"max_candidates_evaluated": 100000,
                                            "max_placement_attempts": 100000,
                                            "max_search_nodes": 100000}},
        "items": [{"id": "crate", "quantity": 2,
                   "dimensions": {"length": "500", "width": "500", "height": "500"}},
                  {"id": "drum", "quantity": 1,
                   "dimensions": {"length": "300", "width": "400", "height": "200"}}],
        "containers": [{"id": "box",
                        "inner_dimensions": {"length": "100", "width": "100", "height": "100"}}]
    }));
    assert_eq!(result["status"], "infeasible");
    assert_eq!(result["algorithm"]["solver"], "portfolio");
    assert_eq!(result["containers"], json!([]));
    // The fact is the status's own; no bound or gap is stated beside it.
    assert_eq!(result["optimality"], json!({"code": "proven_infeasible"}));
    let reasons: Vec<(&str, &str)> = result["unpacked_items"]
        .as_array()
        .expect("unpacked items")
        .iter()
        .map(|item| {
            (
                item["reason"].as_str().unwrap_or_default(),
                item["proof"]["level"].as_str().unwrap_or_default(),
            )
        })
        .collect();
    assert_eq!(
        reasons,
        [("no_compatible_container_dimensions", "proven"); 3]
    );
}
