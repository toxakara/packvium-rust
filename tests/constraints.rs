use packvium_core::{
    ExecutionOptions, ReasonProof, ResultFact, StartRecord, aggregate_termination, pack_json,
    pack_json_with,
};
use serde_json::{Value, json};

fn solve(mut value: Value) -> Value {
    // These are behavioural regressions, not deadline benchmarks. Keep wall time as a
    // remote hang ceiling and let counted work be the deterministic stopping contract;
    // cold linux/amd64 emulation can legitimately need more than the production default
    // second. Tests that exercise deadline behaviour set their own smaller limit below.
    let configuration = value
        .as_object_mut()
        .expect("packing request should be an object")
        .entry("configuration")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .expect("configuration should be an object");
    configuration
        .entry("time_limit_ms")
        .or_insert_with(|| json!(300_000));
    configuration.entry("effort_budget").or_insert_with(|| {
        json!({
            "max_candidates_evaluated": 1_000_000,
            "max_placement_attempts": 1_000_000,
            "max_search_nodes": 1_000_000
        })
    });
    let output = pack_json(&value.to_string()).expect("packing request should solve");
    serde_json::from_str(&output).expect("result should be JSON")
}

#[test]
fn maximum_value_scores_the_total_value_of_whatever_is_left_unpacked() {
    // `maximum_value`'s reported score must equal the sum of `value` across
    // whichever items the search actually left unpacked -- unlike Python/PHP's
    // multi-start portfolio, Rust is not required to reproduce their specific
    // choice of *which* item to leave behind (this holds Rust to the objective
    // formula and validity, not placement equality), only to compute the formula
    // correctly for whatever placement it did produce.
    let result = solve(json!({
        "configuration": {"objective": "maximum_value", "max_containers": 1},
        "items": [
            {"id": "a", "dimensions": {"length": "50", "width": "50", "height": "50"}, "value": 30},
            {"id": "b", "dimensions": {"length": "50", "width": "50", "height": "50"}, "value": 7}
        ],
        "containers": [{"id": "c", "inner_dimensions": {"length": "50", "width": "50", "height": "50"}}]
    }));
    assert_eq!(result["objective"], "maximum_value");
    let unpacked_value: i64 = result["unpacked_items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| {
            let id = item["item_type"].as_str().unwrap();
            if id == "a" { 30 } else { 7 }
        })
        .sum();
    assert_eq!(result["score"][1], unpacked_value);
    assert_eq!(result["summary"]["unpacked_item_count"], 1);
}

#[test]
fn maximum_value_decides_which_item_is_left_behind() {
    // The test above pins the *formula*, which a solver that ignores
    // `value` entirely still satisfies -- it scores whatever it happened to leave.
    // Ordering by value is a separate contract: with only one slot, the objective's
    // second key says the cheap item is the one to leave, and every engine agrees.
    let result = solve(json!({
        "configuration": {"objective": "maximum_value", "max_containers": 1},
        "items": [
            {"id": "cheap", "dimensions": {"length": "50", "width": "50", "height": "50"}, "value": 1},
            {"id": "precious", "dimensions": {"length": "50", "width": "50", "height": "50"}, "value": 500}
        ],
        "containers": [{"id": "c", "inner_dimensions": {"length": "50", "width": "50", "height": "50"}}]
    }));
    assert_eq!(result["score"][1], 1);
    let unpacked = result["unpacked_items"].as_array().unwrap();
    assert_eq!(unpacked.len(), 1);
    assert_eq!(unpacked[0]["item_type"], "cheap");
}

#[test]
fn an_undeclared_value_leaves_the_ordering_untouched() {
    // The lead key defaults to zero, so a `maximum_value` request that never sets
    // `value` must order exactly as the same request under the default objective.
    let items = json!([
        {"id": "first", "dimensions": {"length": "50", "width": "50", "height": "50"}},
        {"id": "second", "dimensions": {"length": "50", "width": "50", "height": "50"}}
    ]);
    let containers =
        json!([{"id": "c", "inner_dimensions": {"length": "50", "width": "50", "height": "50"}}]);
    let valued = solve(json!({
        "configuration": {"objective": "maximum_value", "max_containers": 1},
        "items": items, "containers": containers
    }));
    let plain = solve(json!({
        "configuration": {"max_containers": 1},
        "items": items, "containers": containers
    }));
    assert_eq!(valued["containers"], plain["containers"]);
    assert_eq!(
        valued["unpacked_items"][0]["item_type"],
        plain["unpacked_items"][0]["item_type"]
    );
}

/// One large box that fills the container's footprint, and small boxes that can only
/// use the same space without it: eight of them, for less volume than the large one.
fn large_against_small(objective: &str, small_quantity: usize, profile: &str) -> Value {
    solve(json!({
        "units": {"length": "mm"},
        "configuration": {"objective": objective, "solver_profile": profile},
        "items": [
            {"id": "large", "dimensions": {"length": "100", "width": "100", "height": "90"}},
            {"id": "small", "quantity": small_quantity,
             "dimensions": {"length": "40", "width": "40", "height": "40"}}
        ],
        "containers": [{"id": "c", "quantity": 1,
                        "inner_dimensions": {"length": "100", "width": "100", "height": "100"}}]
    }))
}

fn placed_types(result: &Value) -> Vec<String> {
    result["containers"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|container| container["placements"].as_array().unwrap())
        .map(|placement| placement["item_type"].as_str().unwrap().to_owned())
        .collect()
}

/// `algorithm.duration_ms` is wall clock, on the answer and on every alternative; nothing
/// else in a result may differ between two runs of one request.
fn without_wall_clock(mut result: Value) -> Value {
    for alternative in result["alternatives"].as_array_mut().into_iter().flatten() {
        alternative["algorithm"]
            .as_object_mut()
            .unwrap()
            .remove("duration_ms");
    }
    result["algorithm"]
        .as_object_mut()
        .unwrap()
        .remove("duration_ms");
    result
}

#[test]
fn maximum_volume_scores_the_share_of_requested_volume_left_unpacked() {
    // One of two equal cubes stays behind: half the requested volume, one piece, and
    // the default objective's container count, cost and unused volume after them.
    let result = solve(json!({
        "configuration": {"objective": "maximum_volume", "max_containers": 1},
        "items": [{"id": "a", "quantity": 2,
                   "dimensions": {"length": "50", "width": "50", "height": "50"}}],
        "containers": [{"id": "c", "cost_minor": 7,
                        "inner_dimensions": {"length": "50", "width": "50", "height": "50"}}]
    }));
    assert_eq!(result["objective"], "maximum_volume");
    assert_eq!(result["score"], json!([500_000, 1, 1, 7, 0]));
}

#[test]
fn maximum_volume_rounds_a_tiny_remainder_up_and_scores_a_complete_answer_zero() {
    // A freight container's cubic ticks overflow 64 bits, and one stray millimetre cube
    // is far less than a millionth of the load. It must still not score as complete.
    let request = |tiny_quantity: usize| {
        json!({
            "units": {"length": "mm"},
            "configuration": {"objective": "maximum_volume"},
            "items": [
                {"id": "load", "allowed_rotations": ["LWH"],
                 "dimensions": {"length": "12000", "width": "2300", "height": "2400"}},
                {"id": "tiny", "quantity": tiny_quantity,
                 "dimensions": {"length": "1", "width": "1", "height": "1"}}
            ],
            "containers": [{"id": "c", "quantity": tiny_quantity,
                            "inner_dimensions": {"length": "12000", "width": "2300", "height": "2400"}}]
        })
    };

    let incomplete = solve(request(1));
    assert_eq!(placed_types(&incomplete), ["load"]);
    assert_eq!(incomplete["score"], json!([1, 1, 1, 0, 0]));

    let complete = solve(request(2));
    assert_eq!(complete["summary"]["unpacked_item_count"], 0);
    assert_eq!(complete["score"][0], 0);
    assert_eq!(complete["score"][1], 0);
}

#[test]
fn maximum_volume_keeps_the_large_box_where_the_default_keeps_more_pieces() {
    // Within `exact_item_limit` the exhaustive search answers, so this holds both
    // objectives to their optimum: one box of 900 000 mm3 against five of 64 000.
    let by_volume = large_against_small("maximum_volume", 5, "balanced");
    assert_eq!(placed_types(&by_volume), ["large"]);
    assert_eq!(by_volume["summary"]["unpacked_item_count"], 5);

    let by_count = large_against_small("default", 5, "balanced");
    assert_eq!(placed_types(&by_count), ["small"; 5]);
    assert_eq!(by_count["unpacked_items"][0]["item_type"], "large");
}

#[test]
fn maximum_volume_steers_the_heuristic_search_as_well_as_the_exact_one() {
    // Eleven instances are past `exact_item_limit`, so orderings, the beam and the block
    // solver decide. Eight small boxes fit without the large one: 512 000 mm3 to 900 000.
    let by_volume = large_against_small("maximum_volume", 10, "quality");
    assert_eq!(placed_types(&by_volume), ["large"]);
    assert_eq!(by_volume["score"], json!([415_585, 10, 1, 0, 100_000]));

    let by_count = large_against_small("default", 10, "quality");
    assert_eq!(placed_types(&by_count), ["small"; 8]);
    assert_eq!(by_count["score"][0], 3);
}

#[test]
fn maximum_volume_gives_one_answer_for_one_request() {
    let first = large_against_small("maximum_volume", 10, "quality");
    let second = large_against_small("maximum_volume", 10, "quality");
    assert_eq!(without_wall_clock(first), without_wall_clock(second));
}

#[test]
fn an_item_with_no_stop_never_buries_one_that_has_a_stop() {
    // `route_contact_allowed` used to wave through any pair where either
    // side declared no stop, so a stop-free item parked on top of a stop-0 item was
    // accepted -- and since nothing here self-validates, it came back `feasible`. The
    // shared validator disagreed. A placement with no stop is never scheduled for
    // removal, so it blocks for the whole route.
    let result = solve(json!({
        "configuration": {
            "solver_profile": "fast",
            "max_containers": 1,
            // Keep the wall clock a remote hang ceiling. The assertion is about route
            // order, not whether an emulated Linux runner finishes inside the default
            // second; the counted budget makes the ordinary stopping path explicit.
            "time_limit_ms": 300000,
            "effort_budget": {
                "max_candidates_evaluated": 1000000,
                "max_placement_attempts": 1000000,
                "max_search_nodes": 1000000
            }
        },
        "items": [
            {"id": "first-stop", "stop_index": 0, "dimensions": {"length": "10", "width": "10", "height": "10"}},
            {"id": "rides-along", "dimensions": {"length": "10", "width": "10", "height": "10"}}
        ],
        "containers": [{"id": "column", "inner_dimensions": {"length": "10", "width": "10", "height": "20"}}]
    }));
    let placements = result["containers"][0]["placements"].as_array().unwrap();
    assert_eq!(placements.len(), 2);
    let floor = placements
        .iter()
        .find(|p| p["position"]["z"]["ticks"] == 0)
        .expect("one item rests on the floor");
    assert_eq!(floor["item_type"], "rides-along");
}

#[test]
fn a_negative_value_fails_admission_instead_of_being_ignored() {
    let output = pack_json(
        &json!({
            "items": [{"id": "a", "dimensions": {"length": "10", "width": "10", "height": "10"}, "value": -1}],
            "containers": [{"id": "c", "inner_dimensions": {"length": "10", "width": "10", "height": "10"}}]
        })
        .to_string(),
    );
    assert!(output.is_err());
}

#[test]
fn non_stackable_items_use_separate_narrow_containers() {
    let result = solve(json!({
        "configuration": {"solver_profile": "fast"},
        "items": [{
            "id": "fragile",
            "quantity": 2,
            "stackable": false,
            "dimensions": {"length": "100", "width": "100", "height": "100"}
        }],
        "containers": [{
            "id": "tower",
            "inner_dimensions": {"length": "100", "width": "100", "height": "200"}
        }]
    }));
    assert_eq!(result["complete"], true);
    assert_eq!(result["summary"]["container_count"], 2);
}

#[test]
fn the_extreme_point_solver_consolidates_into_the_container_that_holds_the_most() {
    // A small, cheap container that can only hold one item must not be
    // preferred, one unit at a time, over a larger available container that could
    // hold every remaining item in a single opening. `extreme_points` is forced
    // explicitly so this exercises `pack_order`'s own container selection rather
    // than the separate `grid` fast path.
    let result = solve(json!({
        // This test verifies container selection, not wall-clock termination. The
        // explicit ceiling keeps the oracle stable on slow linux/amd64 emulation;
        // effort-boundary behaviour has dedicated injected-clock tests.
        "configuration": {"solvers": ["extreme_points"], "time_limit_ms": 60000},
        "items": [{
            "id": "cube",
            "quantity": 10,
            "dimensions": {"length": "40", "width": "40", "height": "40"}
        }],
        "containers": [
            {"id": "small", "inner_dimensions": {"length": "40", "width": "40", "height": "40"}},
            {"id": "large", "inner_dimensions": {"length": "200", "width": "200", "height": "200"}}
        ]
    }));
    assert_eq!(result["complete"], true);
    assert_eq!(result["summary"]["container_count"], 1);
    assert_eq!(result["containers"][0]["container_type"], "large");
}

#[test]
fn top_load_limit_is_hard() {
    let result = solve(json!({
        "configuration": {"solver_profile": "fast"},
        "items": [{
            "id": "weak",
            "quantity": 2,
            "weight": {"value": "1", "unit": "kg"},
            "max_top_load": {"value": "0.5", "unit": "kg"},
            "dimensions": {"length": "100", "width": "100", "height": "100"}
        }],
        "containers": [{
            "id": "tower",
            "inner_dimensions": {"length": "100", "width": "100", "height": "200"}
        }]
    }));
    assert_eq!(result["complete"], true);
    assert_eq!(result["summary"]["container_count"], 2);
}

#[test]
fn a_high_priority_item_leads_every_built_in_ordering() {
    // Priority is a preference, not a guarantee: a container that can only hold one of
    // the two items should hold the high-priority one, even though it is far smaller.
    // exact_item_limit is dropped to below the item count so the opportunistic exact
    // solver — which searches every subset for the objectively best score regardless
    // of item order — does not override the ordering preference this test targets.
    let result = solve(json!({
        "configuration": {"solver_profile": "fast", "exact_item_limit": 1},
        "items": [
            {
                "id": "big",
                "dimensions": {"length": "100", "width": "100", "height": "100"}
            },
            {
                "id": "small",
                "priority": 5,
                "dimensions": {"length": "10", "width": "10", "height": "10"}
            }
        ],
        "containers": [{
            "id": "box",
            "quantity": 1,
            "inner_dimensions": {"length": "100", "width": "100", "height": "100"}
        }]
    }));
    let placed: Vec<&str> = result["containers"][0]["placements"]
        .as_array()
        .expect("placements should be an array")
        .iter()
        .map(|p| p["item_id"].as_str().expect("item_id should be a string"))
        .collect();
    assert_eq!(placed, vec!["small#1"]);
}

#[test]
fn concurrent_starts_leave_the_answer_unchanged() {
    let request = json!({
        "configuration": {
            "solver_profile": "quality",
            "time_limit_ms": 300000,
            "seed": 77
        },
        "items": [
            {"id": "a", "quantity": 3, "dimensions": {"length": "60", "width": "40", "height": "20"}},
            {"id": "b", "quantity": 2, "dimensions": {"length": "40", "width": "40", "height": "40"}}
        ],
        "containers": [{"id": "box", "inner_dimensions": {"length": "200", "width": "100", "height": "100"}}]
    });
    let run = |parallel| -> Value {
        let output = pack_json_with(&request.to_string(), ExecutionOptions { parallel })
            .expect("packing request should solve");
        serde_json::from_str(&output).expect("result should be JSON")
    };
    let (concurrent, sequential) = (run(true), run(false));
    assert_eq!(concurrent["containers"], sequential["containers"]);
    assert_eq!(concurrent["unpacked_items"], sequential["unpacked_items"]);
    assert_eq!(concurrent["score"], sequential["score"]);
}

#[test]
fn parallel_is_an_execution_option_not_a_request_field() {
    let refused = pack_json(
        &json!({
            "configuration": {"parallel": false},
            "items": [{"id": "a", "dimensions": {"length": "10", "width": "10", "height": "10"}}],
            "containers": [{"id": "c", "inner_dimensions": {"length": "20", "width": "20", "height": "20"}}]
        })
        .to_string(),
    );
    let Err(packvium_core::PackError::InvalidRequest(error)) = refused else {
        panic!("a request naming `parallel` must be refused, got {refused:?}");
    };
    assert_eq!(
        (error.reason(), error.field()),
        ("not_allowed", "/configuration/parallel")
    );
}

#[test]
fn algorithm_metrics_are_structured_and_mirror_legacy_counters() {
    let result = solve(json!({
        "configuration": {"solver_profile": "balanced"},
        "items": [
            {"id": "a", "quantity": 2, "dimensions": {"length": "40", "width": "40", "height": "40"}},
            {"id": "b", "dimensions": {"length": "30", "width": "30", "height": "30"}}
        ],
        "containers": [{
            "id": "box",
            "inner_dimensions": {"length": "100", "width": "100", "height": "100"}
        }]
    }));
    let algorithm = &result["algorithm"];
    let metrics = &algorithm["metrics"];
    let fields = [
        "candidate_points_considered",
        "orientations_considered",
        "feasible_candidates",
        "collision_checks",
        "support_checks",
        "space_partitions",
        "search_nodes_expanded",
    ];
    assert!(fields.iter().all(|field| metrics[field].as_u64().is_some()));
    assert_eq!(
        metrics["orientations_considered"],
        algorithm["placements_attempted"]
    );
    assert_eq!(
        metrics["feasible_candidates"],
        algorithm["candidates_evaluated"]
    );
    assert!(metrics["candidate_points_considered"].as_u64().unwrap() > 0);
    assert!(metrics["search_nodes_expanded"].as_u64().unwrap() > 0);
}

#[test]
fn a_grid_deadline_sets_the_status_flag_and_unpacked_reasons() {
    let result = solve(json!({
        "configuration": {
            "solver_profile": "fast",
            "time_limit_ms": 1
        },
        "items": [
            {
                "id": "cube",
                "quantity": 20_000,
                "dimensions": {"length": "10", "width": "10", "height": "10"}
            },
            {
                "id": "oversized",
                "dimensions": {"length": "2000", "width": "2000", "height": "2000"}
            }
        ],
        "containers": [{
            "id": "warehouse",
            "inner_dimensions": {"length": "1000", "width": "1000", "height": "1000"}
        }]
    }));
    assert_eq!(result["status"], "time_limit");
    assert_eq!(result["algorithm"]["time_limit_reached"], true);
    assert_eq!(result["termination"]["code"], "time_limit");
    assert_eq!(result["termination"]["winning_start_truncated"], true);
    let unpacked = result["unpacked_items"]
        .as_array()
        .expect("unpacked items should be an array");
    assert!(!unpacked.is_empty());
    let oversized = unpacked
        .iter()
        .find(|item| item["item_type"] == "oversized")
        .expect("the oversized item should remain unpacked");
    assert_eq!(oversized["reason"], "no_compatible_container_dimensions");
    assert_eq!(oversized["proof"]["level"], "proven");
    assert!(
        unpacked
            .iter()
            .filter(|item| item["item_type"] == "cube")
            .all(|item| item["reason"].as_str() == Some("time_limit")
                && item["proof"]["level"].as_str() == Some("unknown_due_to_limit"))
    );
    assert!(unpacked.iter().all(|item| {
        item["proof"]["observations"]
            .as_array()
            .is_some_and(|observations| {
                observations
                    .iter()
                    .any(|observation| observation["code"] == item["reason"])
            })
    }));
}

#[test]
fn a_finished_winner_is_distinct_from_a_truncated_loser() {
    let fact = aggregate_termination(
        &[
            StartRecord {
                id: "winner".into(),
                started: true,
                completed: true,
                truncated: false,
                selected: true,
                global_deadline_reached: false,
            },
            StartRecord {
                id: "loser".into(),
                started: true,
                completed: false,
                truncated: true,
                selected: false,
                global_deadline_reached: false,
            },
        ],
        false,
    )
    .to_json();
    assert_eq!(fact["code"], "complete");
    assert_eq!(fact["any_start_truncated"], true);
    assert_eq!(fact["all_required_starts_completed"], false);
    assert_eq!(fact["winning_start_truncated"], false);
    assert_eq!(fact["global_deadline_reached"], false);
}

#[test]
fn a_truncated_winner_affects_the_returned_answer() {
    let fact = aggregate_termination(
        &[
            StartRecord {
                id: "winner".into(),
                started: true,
                completed: false,
                truncated: true,
                selected: true,
                global_deadline_reached: false,
            },
            StartRecord {
                id: "loser".into(),
                started: true,
                completed: true,
                truncated: false,
                selected: false,
                global_deadline_reached: false,
            },
        ],
        false,
    )
    .to_json();
    assert_eq!(fact["code"], "time_limit");
    assert_eq!(fact["any_start_truncated"], true);
    assert_eq!(fact["all_required_starts_completed"], false);
    assert_eq!(fact["winning_start_truncated"], true);
    assert_eq!(fact["global_deadline_reached"], false);
}

#[test]
fn structural_rejections_are_proven_and_search_rejections_are_not() {
    let result = solve(json!({
        "configuration": {"solver_profile": "fast", "time_limit_ms": 1},
        "items": [{
            "id": "oversized",
            "dimensions": {"length": "200", "width": "200", "height": "200"}
        }],
        "containers": [{
            "id": "small",
            "inner_dimensions": {"length": "100", "width": "100", "height": "100"}
        }]
    }));
    let rejected = &result["unpacked_items"][0];
    assert_eq!(rejected["reason"], "no_compatible_container_dimensions");
    assert_eq!(rejected["proof"]["level"], "proven");
    assert_eq!(result["score"], json!([1, 0, 0, 0, 0]));
    assert_eq!(
        rejected["proof"]["observations"][0]["code"],
        "no_compatible_container_dimensions"
    );

    assert_eq!(
        ReasonProof::for_reason("search_exhausted", &[]).level,
        "observed"
    );
    assert_eq!(
        ReasonProof::for_reason("group_cannot_fit_together", &[]).level,
        "inferred"
    );
    assert_eq!(
        ReasonProof::for_reason("time_limit", &[]).level,
        "unknown_due_to_limit"
    );
}

#[test]
fn an_unknown_future_result_fact_round_trips_verbatim() {
    let raw = json!({"code": "node_limit", "limit": 10_000});
    let fact = ResultFact::from_json(&raw).expect("open result fact should parse");
    assert_eq!(fact.to_json(), raw);
}

// ------------------------------------------------------------ stacking groups

fn sorted_types(result: &Value) -> Vec<String> {
    let mut types = placed_types(result);
    types.sort();
    types
}

fn two_tier(lower: Value, upper: Value) -> Value {
    let mut lower_item = json!({"id": "lower", "must_be_on_floor": true,
        "dimensions": {"length": "100", "width": "100", "height": "100"}});
    let mut upper_item = json!({"id": "upper",
        "dimensions": {"length": "100", "width": "100", "height": "100"}});
    for (target, extra) in [(&mut lower_item, lower), (&mut upper_item, upper)] {
        for (key, value) in extra.as_object().unwrap() {
            target[key] = value.clone();
        }
    }
    solve(json!({
        "items": [lower_item, upper_item],
        "containers": [{"id": "crate", "quantity": 1,
            "inner_dimensions": {"length": "100", "width": "100", "height": "200"}}]
    }))
}

#[test]
fn stacking_groups_admit_an_accepted_group_and_refuse_an_omitted_one() {
    let lower = json!({"stacking_group": "A", "accepts_stacking_groups": ["A", "B"]});
    assert_eq!(
        sorted_types(&two_tier(lower.clone(), json!({"stacking_group": "B"}))),
        ["lower", "upper"]
    );
    assert_eq!(
        sorted_types(&two_tier(lower, json!({"stacking_group": "C"}))),
        ["lower"]
    );
}

#[test]
fn stacking_groups_treat_an_empty_list_as_open_and_a_non_empty_one_as_closed() {
    for lower in [
        json!({}),
        json!({"accepts_stacking_groups": null}),
        json!({"accepts_stacking_groups": []}),
    ] {
        assert_eq!(
            sorted_types(&two_tier(lower, json!({}))),
            ["lower", "upper"]
        );
    }
    assert_eq!(
        sorted_types(&two_tier(
            json!({"accepts_stacking_groups": ["A"]}),
            json!({})
        )),
        ["lower"]
    );
    assert_eq!(
        sorted_types(&two_tier(
            json!({"accepts_stacking_groups": ["B"]}),
            json!({"stacking_group": "b"})
        )),
        ["lower"]
    );
}

#[test]
fn stacking_groups_yield_to_non_stackable() {
    let lower = json!({"stackable": false, "accepts_stacking_groups": ["B"]});
    assert_eq!(
        sorted_types(&two_tier(lower, json!({"stacking_group": "B"}))),
        ["lower"]
    );
}

#[test]
fn stacking_groups_require_every_supporter_of_a_bridge_to_accept() {
    let request = |accepts: Value| {
        json!({
            "items": [
                {"id": "left", "must_be_on_floor": true, "allowed_rotations": ["LWH"],
                 "accepts_stacking_groups": accepts,
                 "dimensions": {"length": "50", "width": "100", "height": "100"}},
                {"id": "right", "must_be_on_floor": true, "allowed_rotations": ["LWH"],
                 "dimensions": {"length": "50", "width": "100", "height": "100"}},
                {"id": "wide", "keep_upright": true, "stacking_group": "B",
                 "dimensions": {"length": "100", "width": "100", "height": "40"}}
            ],
            "containers": [{"id": "crate", "quantity": 1,
                "inner_dimensions": {"length": "100", "width": "100", "height": "140"}}]
        })
    };
    assert_eq!(
        sorted_types(&solve(request(json!(["A"])))),
        ["left", "right"]
    );
    assert_eq!(
        sorted_types(&solve(request(json!(["B"])))),
        ["left", "right", "wide"]
    );
}

#[test]
fn stacking_groups_judge_a_candidate_slid_under_a_fixed_item_by_its_own_list() {
    let request = |accepts: Value| {
        json!({
            "items": [
                {"id": "drum", "stacking_group": "C",
                 "dimensions": {"length": "100", "width": "100", "height": "50"}},
                {"id": "crate", "accepts_stacking_groups": accepts,
                 "dimensions": {"length": "100", "width": "100", "height": "100"}}
            ],
            "containers": [{"id": "bay", "quantity": 1,
                "inner_dimensions": {"length": "100", "width": "100", "height": "150"}}],
            "fixed_placements": [{"item_type": "drum", "container_type": "bay",
                "orientation": "LWH", "position": {"x": "0", "y": "0", "z": "100"}}]
        })
    };
    assert_eq!(sorted_types(&solve(request(json!(["A"])))), ["drum"]);
    assert_eq!(
        sorted_types(&solve(request(json!(["C"])))),
        ["crate", "drum"]
    );
}

#[test]
fn stacking_groups_need_a_nesting_item_to_accept_its_own_group() {
    let request = |accepts: Value| {
        json!({
            "configuration": {"solvers": ["extreme_points"]},
            "items": [{"id": "tote", "quantity": 3, "nesting_height": "5",
                "allowed_rotations": ["LWH"], "stacking_group": "T",
                "accepts_stacking_groups": accepts,
                "dimensions": {"length": "10", "width": "10", "height": "10"}}],
            "containers": [{"id": "rack", "quantity": 1,
                "inner_dimensions": {"length": "10", "width": "10", "height": "20"}}]
        })
    };
    assert_eq!(placed_types(&solve(request(json!(["U"])))).len(), 1);
    assert_eq!(placed_types(&solve(request(json!(["T"])))).len(), 3);
}

#[test]
fn stacking_groups_cap_the_grid_lattice_when_a_type_refuses_its_own_group() {
    let request = |accepts: Value| {
        json!({
            "configuration": {"solver_profile": "fast"},
            "items": [{"id": "tile", "quantity": 8, "stacking_group": "A",
                "accepts_stacking_groups": accepts,
                "dimensions": {"length": "40", "width": "40", "height": "40"}}],
            "containers": [{"id": "box", "quantity": 1,
                "inner_dimensions": {"length": "100", "width": "100", "height": "100"}}]
        })
    };
    assert_eq!(placed_types(&solve(request(json!(["B"])))).len(), 4);
    assert_eq!(placed_types(&solve(request(json!(["A"])))).len(), 8);
}

#[test]
fn stacking_groups_make_the_block_solver_delegate() {
    let result = solve(json!({
        "configuration": {"solver_profile": "quality", "solvers": ["homogeneous_blocks"]},
        "items": [
            {"id": "a", "allowed_rotations": ["LWH"], "stacking_group": "A",
             "accepts_stacking_groups": ["A"],
             "dimensions": {"length": "100", "width": "100", "height": "50"}},
            {"id": "b", "quantity": 8, "stacking_group": "B",
             "dimensions": {"length": "50", "width": "50", "height": "50"}}
        ],
        "containers": [{"id": "cube", "quantity": 1,
            "inner_dimensions": {"length": "200", "width": "100", "height": "100"}}]
    }));
    assert_eq!(placed_types(&result).len(), 9);
}
