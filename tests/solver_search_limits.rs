//! Solver branches that only a limit reaches: an item cap on the container, a lattice that
//! may open only so many containers, a budget that runs out inside the block lookahead or
//! between the exact search's containers, and the staged `quality` search pinned to a
//! single solver.
//!
//! Every scene goes through `pack_json`, which refuses any result its independent validator
//! rejects, so "it solved" already asserts the answer is valid; each test adds the property
//! its scene exists for. Counted work bounds every search and the time limit is only a fuse.

use packvium_core::pack_json;
use serde_json::{Value, json};

const GENEROUS: u64 = 1_000_000;

fn solve_with_budget(mut request: Value, budget: (u64, u64, u64)) -> Value {
    let configuration = request
        .as_object_mut()
        .expect("request object")
        .entry("configuration")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .expect("configuration object");
    configuration.insert("time_limit_ms".into(), json!(300_000));
    configuration.insert(
        "effort_budget".into(),
        json!({
            "max_candidates_evaluated": budget.0,
            "max_placement_attempts": budget.1,
            "max_search_nodes": budget.2
        }),
    );
    let output =
        pack_json(&request.to_string()).unwrap_or_else(|error| panic!("{request}: {error}"));
    serde_json::from_str(&output).expect("result JSON")
}

fn solve(request: Value) -> Value {
    solve_with_budget(request, (GENEROUS, GENEROUS, GENEROUS))
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
        .map(|container| {
            container["placements"]
                .as_array()
                .map_or(0, Vec::len)
                .max(container["placement_count"].as_u64().unwrap_or(0) as usize)
        })
        .sum()
}

fn unpacked_reasons(result: &Value) -> Vec<String> {
    result["unpacked_items"]
        .as_array()
        .expect("unpacked items")
        .iter()
        .map(|item| item["reason"].as_str().expect("reason").to_owned())
        .collect()
}

#[test]
fn the_block_solver_stops_at_the_container_item_cap() {
    for profile in ["fast", "quality"] {
        let result = solve(json!({
            "items": [cube("a", 10, 10), cube("b", 20, 2)],
            "containers": [{ "id": "box", "quantity": 1, "max_items": 3,
                             "inner_dimensions": { "length": "100", "width": "100", "height": "100" } }],
            "configuration": { "solver_profile": profile, "solvers": ["homogeneous_blocks"] }
        }));
        assert_eq!(placed_count(&result), 3, "{profile}: {result}");
        assert_eq!(unpacked_reasons(&result).len(), 9, "{profile}: {result}");
    }
}

fn mixed_block_order() -> Value {
    json!({
        "items": [cube("a", 30, 6), cube("b", 20, 9), cube("c", 15, 8), cube("d", 40, 2)],
        "containers": [{ "id": "box", "quantity": 1,
                         "inner_dimensions": { "length": "90", "width": "60", "height": "60" } }],
        "configuration": { "solver_profile": "quality", "solvers": ["homogeneous_blocks"] }
    })
}

#[test]
fn the_block_lookahead_never_places_past_the_placement_budget() {
    let full = placed_count(&solve(mixed_block_order()));
    for attempts in 1..=40 {
        let result = solve_with_budget(mixed_block_order(), (GENEROUS, attempts, GENEROUS));
        let placed = placed_count(&result);
        assert!(placed <= full, "{attempts}: {result}");
        let attempted = result["algorithm"]["placements_attempted"]
            .as_u64()
            .expect("attempts");
        assert!(attempted <= attempts, "{attempts}: {result}");
        if placed < full {
            assert_eq!(
                result["algorithm"]["effort_limit_reached"], true,
                "{result}"
            );
        }
    }
}

#[test]
fn the_block_lookahead_stands_on_its_best_completion_when_nodes_run_out() {
    let full = placed_count(&solve(mixed_block_order()));
    for nodes in 1..=40 {
        let result = solve_with_budget(mixed_block_order(), (GENEROUS, GENEROUS, nodes));
        assert!(placed_count(&result) <= full, "{nodes}: {result}");
        assert!(
            result["algorithm"]["metrics"]["search_nodes_expanded"]
                .as_u64()
                .is_none_or(|expanded| expanded <= nodes),
            "{nodes}: {result}"
        );
    }
}

#[test]
fn a_compact_lattice_opens_no_more_containers_than_allowed() {
    let result = solve(json!({
        "items": [cube("a", 50, 20)],
        "containers": [{ "id": "box",
                         "inner_dimensions": { "length": "100", "width": "100", "height": "100" } }],
        "configuration": { "solvers": ["grid"], "max_containers": 1,
                           "require_placement_coordinates": false,
                           "objective": "maximum_volume" }
    }));
    assert_eq!(
        result["containers"].as_array().expect("containers").len(),
        1
    );
    assert_eq!(result["algorithm"]["solver"], "grid", "{result}");
    assert_eq!(unpacked_reasons(&result).len(), 12, "{result}");
    // Under `maximum_volume` the unpacked share of the requested volume leads the score:
    // 12 of 20 equal cubes, rounded up to parts per million.
    assert_eq!(result["score"][0], 600_000, "{result}");
}

#[test]
fn the_exact_search_stops_between_containers_on_the_effort_budget() {
    let request = json!({
        "items": [cube("a", 10, 3), cube("b", 20, 1)],
        "containers": [
            { "id": "small", "quantity": 1,
              "inner_dimensions": { "length": "30", "width": "10", "height": "10" } },
            { "id": "large", "quantity": 1, "cost_minor": 100,
              "inner_dimensions": { "length": "50", "width": "20", "height": "20" } }
        ],
        "configuration": { "solver_profile": "exact_small", "solvers": ["exact_small"],
                           "max_containers": 1 }
    });
    for nodes in 1..=60 {
        let result = solve_with_budget(request.clone(), (GENEROUS, GENEROUS, nodes));
        let reasons = unpacked_reasons(&result);
        if result["algorithm"]["effort_limit_reached"] == true {
            assert!(
                reasons
                    .iter()
                    .all(|reason| reason != "exact_search_incomplete"),
                "{nodes}: {result}"
            );
        }
    }
}

#[test]
fn the_staged_quality_search_refines_an_order_when_extreme_points_are_not_selected() {
    let result = solve(json!({
        "items": [cube("a", 30, 4), cube("b", 20, 6)],
        "containers": [{ "id": "box",
                         "inner_dimensions": { "length": "60", "width": "60", "height": "60" } }],
        "configuration": { "solver_profile": "quality", "solvers": ["maximal_spaces"] }
    }));
    assert_eq!(placed_count(&result), 10, "{result}");
    assert!(
        result["algorithm"]["solver"]
            .as_str()
            .expect("solver")
            .starts_with("maximal_spaces"),
        "{result}"
    );
}

fn placement_of<'a>(result: &'a Value, item_type: &str) -> &'a Value {
    result["containers"][0]["placements"]
        .as_array()
        .expect("placements")
        .iter()
        .find(|placement| placement["item_type"] == item_type)
        .unwrap_or_else(|| panic!("{item_type} placed: {result}"))
}

fn ticks(placement: &Value, axis: &str) -> i64 {
    placement["position"][axis]["ticks"]
        .as_i64()
        .expect("ticks")
}

#[test]
fn a_crushable_item_beside_a_nesting_one_is_never_buried_under_a_brick() {
    // Nesting rewrites contact edges, so the load check rebuilds the scene from its
    // placements rather than sweeping a delta; the crush limit must hold there as well.
    let result = solve(json!({
        "units": { "length": "mm" },
        "items": [
            { "id": "tote", "quantity": 2, "nesting_height": "5", "allowed_rotations": ["LWH"],
              "dimensions": { "length": "10", "width": "10", "height": "10" } },
            { "id": "pillow", "quantity": 1, "shape_type": "compressible",
              "compression_ratio": 0.9, "max_compression_pressure_kpa": 1,
              "allowed_rotations": ["LWH"], "weight": "1",
              "dimensions": { "length": "10", "width": "10", "height": "10" } },
            { "id": "brick", "quantity": 1, "weight": { "value": "10", "unit": "kg" }, "allowed_rotations": ["LWH"],
              "dimensions": { "length": "10", "width": "10", "height": "10" } }
        ],
        "containers": [{ "id": "column", "quantity": 1,
                         "inner_dimensions": { "length": "20", "width": "10", "height": "30" } }],
        "configuration": { "solvers": ["extreme_points"] }
    }));
    assert_eq!(placed_count(&result), 4, "{result}");
    let pillow = placement_of(&result, "pillow");
    let brick = placement_of(&result, "brick");
    let same_column = ticks(pillow, "x") == ticks(brick, "x");
    assert!(
        !same_column || ticks(brick, "z") < ticks(pillow, "z"),
        "{result}"
    );
}

#[test]
fn the_container_plan_beam_stops_inside_a_round_on_its_node_limit() {
    let request = json!({
        "items": [cube("a", 50, 7)],
        "containers": [
            { "id": "pair", "inner_dimensions": { "length": "100", "width": "50", "height": "50" } },
            { "id": "quad", "cost_minor": 10,
              "inner_dimensions": { "length": "100", "width": "100", "height": "50" } }
        ],
        "configuration": { "solver_profile": "quality", "solvers": ["extreme_points"],
                           "container_plan_beam_width": 2, "container_plan_node_limit": 1 }
    });
    let result = solve(request);
    // The node limit stops the plan search early; the greedy completion still packs every
    // cube, and the validator behind `pack_json` holds the answer to the request.
    assert_eq!(placed_count(&result), 7, "{result}");
}

#[test]
fn the_exact_search_takes_the_next_container_only_while_the_budget_lasts() {
    // `flat` cannot seat four 15 mm cubes side by side, yet its volume floor says it might,
    // so it is searched first; `square` holds all four at a cost.
    let request = json!({
        "items": [cube("a", 15, 4)],
        "containers": [
            { "id": "flat", "quantity": 1,
              "inner_dimensions": { "length": "25", "width": "25", "height": "40" } },
            { "id": "square", "quantity": 1, "cost_minor": 100,
              "inner_dimensions": { "length": "30", "width": "30", "height": "15" } }
        ],
        "configuration": { "solver_profile": "exact_small", "solvers": ["exact_small"],
                           "max_containers": 1 }
    });
    let unlimited = solve(request.clone());
    assert_eq!(
        unlimited["containers"][0]["container_type"], "square",
        "{unlimited}"
    );
    let mut stopped_short = 0;
    let budgets = (1..=200).flat_map(|limit| {
        [
            (limit, GENEROUS, GENEROUS),
            (GENEROUS, limit, GENEROUS),
            (GENEROUS, GENEROUS, limit),
        ]
    });
    for budget in budgets {
        let nodes = format!("{budget:?}");
        let result = solve_with_budget(request.clone(), budget);
        if result["algorithm"]["effort_limit_reached"] == true {
            stopped_short += 1;
            assert!(
                unpacked_reasons(&result)
                    .iter()
                    .all(|reason| reason == "effort_limit"),
                "{nodes}: {result}"
            );
        } else {
            assert_eq!(result["score"], unlimited["score"], "{nodes}: {result}");
        }
    }
    assert!(stopped_short > 0);
}

#[test]
fn the_exact_volume_floor_holds_a_container_nothing_fits_and_a_compressible_item() {
    let request = json!({
        "items": [
            cube("a", 20, 2),
            { "id": "pillow", "quantity": 1, "shape_type": "compressible",
              "compression_ratio": 0.5, "max_compression_pressure_kpa": 1000000,
              "dimensions": { "length": "20", "width": "20", "height": "20" } }
        ],
        "containers": [
            { "id": "tiny", "quantity": 1,
              "inner_dimensions": { "length": "10", "width": "10", "height": "10" } },
            { "id": "box", "quantity": 1,
              "inner_dimensions": { "length": "60", "width": "20", "height": "20" } }
        ],
        "configuration": { "solver_profile": "exact_small", "solvers": ["exact_small"],
                           "objective": "maximum_volume", "max_containers": 1 }
    });
    let result = solve(request);
    assert_eq!(result["containers"][0]["container_type"], "box", "{result}");
    assert_eq!(placed_count(&result), 3, "{result}");
    assert_eq!(result["score"][0], 0, "{result}");
}

#[test]
fn a_portfolio_that_places_nothing_says_whether_that_is_proven() {
    // The exact search stands aside for a group, so an exact-only selection runs no start.
    let request = |mut item: Value| {
        item["group"] = json!("set");
        json!({
            "items": [item],
            "containers": [{ "id": "box",
                             "inner_dimensions": { "length": "100", "width": "100", "height": "100" } }],
            "configuration": { "solver_profile": "balanced", "solvers": ["exact_small"] }
        })
    };
    let oversize = solve(request(cube("boulder", 200, 2)));
    assert_eq!(oversize["status"], "infeasible", "{oversize}");
    assert_eq!(
        unpacked_reasons(&oversize),
        ["no_compatible_container_dimensions"; 2]
    );

    let unsearched = solve(request(cube("cube", 50, 2)));
    assert_eq!(unsearched["status"], "best_found", "{unsearched}");
    assert_eq!(placed_count(&unsearched), 0, "{unsearched}");
    assert_eq!(unpacked_reasons(&unsearched).len(), 2, "{unsearched}");
}
