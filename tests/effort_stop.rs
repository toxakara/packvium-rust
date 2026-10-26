//! The effort flag means a guard stopped the search, and no counter passes its limit.
//!
//! A start that finishes having used exactly its budget was not stopped: it reports
//! `effort_limit_reached: false` and terminates `complete`. A budget one short stops it, and
//! every counted metric stays at or below its limit -- including the lattice and block paths,
//! which count a whole run of placements at once.
//!
//! "Exactly its budget" is the three limits set alike to the largest counter used, as a caller
//! sets them. Every guard reads all three counters, so a node or candidate limit equal to its
//! own use can still refuse the rotations that follow the last count -- a real stop, which is
//! why the per-counter cases below only claim the stop and the limits.

use packvium_core::{ExecutionOptions, pack_json, pack_json_with, validate_json};
use serde_json::{Value, json};

const GENEROUS: u64 = 1_000_000;

fn request(solver: &str, profile: &str, items: Value, container: [u32; 3]) -> Value {
    let [length, width, height] = container;
    json!({
        "configuration": {
            "solver_profile": profile,
            "solvers": [solver],
            "time_limit_ms": 60_000,
        },
        "items": items,
        "containers": [{
            "id": "box",
            "inner_dimensions": {
                "length": length.to_string(),
                "width": width.to_string(),
                "height": height.to_string(),
            },
        }],
    })
}

fn cube(id: &str, quantity: u32, side: u32) -> Value {
    json!({
        "id": id,
        "quantity": quantity,
        "dimensions": {
            "length": side.to_string(),
            "width": side.to_string(),
            "height": side.to_string(),
        },
    })
}

fn mixed_cart() -> Value {
    request(
        "extreme_points",
        "fast",
        json!([
            cube("large", 3, 30),
            cube("medium", 5, 20),
            {"id": "slab", "quantity": 4, "dimensions": {"length": "40", "width": "20", "height": "10"}},
        ]),
        [80, 60, 50],
    )
}

/// `request` with the three counted limits set to `(candidates, placements, nodes)`, packed,
/// and checked by the reference validator.
fn pack_with(request: &Value, (candidates, placements, nodes): (u64, u64, u64)) -> Value {
    let mut budgeted = request.clone();
    budgeted["configuration"]["effort_budget"] = json!({
        "max_candidates_evaluated": candidates,
        "max_placement_attempts": placements,
        "max_search_nodes": nodes,
    });
    let text = budgeted.to_string();
    let result = pack_json(&text).expect("a valid request packs");
    let issues = validate_json(&text, &result).expect("the result has the documented shape");
    assert!(issues.is_empty(), "{issues:?}");
    serde_json::from_str(&result).expect("result JSON")
}

fn counters(result: &Value) -> (u64, u64, u64) {
    let metrics = &result["algorithm"]["metrics"];
    let read = |name: &str| metrics[name].as_u64().expect("counted metric");
    (
        read("feasible_candidates"),
        read("orientations_considered"),
        read("search_nodes_expanded"),
    )
}

fn assert_within(result: &Value, (candidates, placements, nodes): (u64, u64, u64)) {
    let used = counters(result);
    assert!(
        used.0 <= candidates && used.1 <= placements && used.2 <= nodes,
        "used {used:?} past limits {:?}",
        (candidates, placements, nodes)
    );
}

fn assert_unstopped_answer(result: &Value, unlimited: &Value) {
    assert_eq!(result["algorithm"]["effort_limit_reached"], false);
    assert_eq!(result["termination"]["code"], "complete");
    assert_eq!(result["termination"]["any_start_truncated"], false);
    assert_eq!(result["score"], unlimited["score"]);
    assert_eq!(result["containers"], unlimited["containers"]);
}

fn assert_stopped(result: &Value, limits: (u64, u64, u64)) {
    assert_eq!(result["algorithm"]["effort_limit_reached"], true);
    assert_eq!(result["termination"]["code"], "effort_limit");
    assert_within(result, limits);
}

/// Runs `request` unlimited, then with every limit at exactly the largest counter it used,
/// then one short of that, then one short on each counter alone.
fn assert_exact_boundary(request: &Value) {
    let unlimited = pack_with(request, (GENEROUS, GENEROUS, GENEROUS));
    assert_eq!(unlimited["termination"]["code"], "complete");
    let used = counters(&unlimited);
    let most = used.0.max(used.1).max(used.2);

    assert_unstopped_answer(&pack_with(request, (most, most, most)), &unlimited);
    let short = most - 1;
    assert_stopped(
        &pack_with(request, (short, short, short)),
        (short, short, short),
    );
    let (candidates, placements, nodes) = used;
    for alone in [
        (candidates - 1, GENEROUS, GENEROUS),
        (GENEROUS, placements - 1, GENEROUS),
        (GENEROUS, GENEROUS, nodes - 1),
    ] {
        assert_stopped(&pack_with(request, alone), alone);
    }
}

#[test]
fn a_search_that_uses_exactly_its_budget_finishes_and_one_short_stops_it() {
    assert_exact_boundary(&mixed_cart());
}

#[test]
fn a_block_start_meets_the_same_boundary() {
    assert_exact_boundary(&request(
        "homogeneous_blocks",
        "quality",
        json!([cube("a", 27, 10), cube("b", 8, 20)]),
        [60, 30, 30],
    ));
}

#[test]
fn a_block_commit_places_only_the_members_the_budget_has_room_for() {
    // One 3x3x3 block of 27: the commit counts every member at once.
    let request = request(
        "homogeneous_blocks",
        "quality",
        json!([cube("a", 27, 10)]),
        [30, 30, 30],
    );
    for placements in [1, 10, 26] {
        let limits = (GENEROUS, placements, GENEROUS);
        let result = pack_with(&request, limits);
        assert_stopped(&result, limits);
        assert_eq!(counters(&result).1, placements);
    }
}

#[test]
fn a_lattice_counted_at_once_is_taken_only_when_the_budget_covers_it() {
    let mut request = request("grid", "fast", json!([cube("a", 20, 10)]), [100, 100, 100]);
    // The closed form is the compact lattice, laid out only when coordinates may be left out.
    request["configuration"]["require_placement_coordinates"] = json!(false);
    let unlimited = pack_with(&request, (GENEROUS, GENEROUS, GENEROUS));
    assert_eq!(counters(&unlimited), (20, 20, 1));
    assert_unstopped_answer(&pack_with(&request, (20, 20, 1)), &unlimited);

    // Too small for the closed form: the per-item lattice stops exactly on the limit.
    for limits in [
        (GENEROUS, 7, GENEROUS),
        (7, GENEROUS, GENEROUS),
        (19, 19, GENEROUS),
    ] {
        let result = pack_with(&request, limits);
        assert_stopped(&result, limits);
    }
}

#[test]
fn parallel_starts_each_report_their_own_stop() {
    // Several `balanced` starts on threads: each start's flag comes from its own guards, so
    // the start records match the sequential run whatever the budget.
    let mut request = mixed_cart();
    request["configuration"]["solver_profile"] = json!("balanced");
    request["configuration"]["solvers"] = json!(["extreme_points", "maximal_spaces"]);
    for limit in [60, 200, 400, GENEROUS] {
        let mut budgeted = request.clone();
        budgeted["configuration"]["effort_budget"] = json!({
            "max_candidates_evaluated": limit,
            "max_placement_attempts": limit,
            "max_search_nodes": limit,
        });
        let text = budgeted.to_string();
        let run = |parallel| {
            let output = pack_json_with(&text, ExecutionOptions { parallel }).expect("packs");
            let mut result: Value = serde_json::from_str(&output).expect("result JSON");
            without_durations(&mut result);
            result
        };
        assert_eq!(run(true), run(false), "limit {limit}");
    }
}

/// Clears every `duration_ms`, the one field a wall clock writes.
fn without_durations(value: &mut Value) {
    match value {
        Value::Object(fields) => {
            fields.remove("duration_ms");
            fields.values_mut().for_each(without_durations);
        }
        Value::Array(values) => values.iter_mut().for_each(without_durations),
        _ => {}
    }
}
