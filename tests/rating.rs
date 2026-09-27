//! Carrier rate cards as request data, and the landed-cost objective.
//!
//! The tariff parser had no `cargo test` coverage at all until this file: it is reached
//! only by the conformance corpus, which drives the built binary out of process, so the
//! whole of `parse_rate_table` ran every day and was measured by nothing. A parser that
//! is never unit-tested is a parser whose rejections are guesses -- and a rate card that
//! parses wrongly misprices silently rather than failing.
//!
//! Deliberately an integration test, outside `src/`: it exercises the crate through
//! `pack_json`, the same entry point the harness drives.

use packvium_core::pack_json;
use serde_json::{Value, json};

fn request(rate_table: Value) -> Value {
    json!({
        "units": {"length": "mm"},
        "configuration": {
            "objective": "lowest_landed_cost",
            "dimensional_weight_divisor": 5000,
            "dimensional_weight_length_unit": "cm",
            "dimensional_weight_weight_unit": "kg",
            "time_limit_ms": 300000,
        },
        "items": [{
            "id": "dense", "quantity": 1, "weight": "500 g",
            "dimensions": {"length": "100", "width": "100", "height": "100"},
        }],
        "containers": [{
            "id": "small",
            "inner_dimensions": {"length": "300", "width": "300", "height": "300"},
            "rate_table": rate_table,
        }],
    })
}

fn pack(request: &Value) -> Value {
    serde_json::from_str(&pack_json(&request.to_string()).expect("the request should pack"))
        .expect("a JSON result")
}

fn refusal(rate_table: Value) -> String {
    pack_json(&request(rate_table).to_string())
        .expect_err("a malformed tariff is refused")
        .to_string()
}

#[test]
fn a_landed_cost_is_hand_checkable_from_the_bracket_it_falls_into() {
    // A 300mm cube = 30cm/side -> 27,000cm^3 / 5,000 = 5.4kg = 5,400g billed, which beats
    // the single 500g item. 5,400 is exactly the second bound, and `<=` puts it inside
    // that band rather than the next: 1,400, floored by nothing, plus a 100-permille fuel
    // surcharge of 140.
    let result = pack(&request(json!({
        "weight_brackets_g": [2000, 5400, 20000],
        "prices_minor": [900, 1400, 2500],
        "fuel_surcharge_permille": 100,
    })));
    assert_eq!(result["objective"], "lowest_landed_cost");
    assert_eq!(result["score"][1], 1_540);
}

#[test]
fn the_minimum_charge_is_a_floor_the_surcharge_is_then_taken_on() {
    // The published 900 never applies: the floor lifts the base to 2,000 and the 100
    // permille is a share of the floored base, not of the price the table lists.
    let result = pack(&request(json!({
        "weight_brackets_g": [20000],
        "prices_minor": [900],
        "minimum_charge_minor": 2000,
        "fuel_surcharge_permille": 100,
    })));
    assert_eq!(result["score"][1], 2_200);
}

#[test]
fn a_price_band_may_dip_because_a_promotional_rate_card_is_real() {
    // Read by bracket, not by comparing prices, so a cheaper upper band prices correctly
    // instead of being rejected as malformed.
    let result = pack(&request(json!({
        "weight_brackets_g": [2000, 20000],
        "prices_minor": [9000, 300],
    })));
    assert_eq!(result["score"][1], 300);
}

#[test]
fn a_malformed_tariff_is_refused_rather_than_mispriced() {
    // Each case is one guard in the parser. A tariff that parses wrongly does not fail;
    // it quotes a number nobody published, which is the failure mode worth this table.
    let cases: [(Value, &str); 7] = [
        (json!("2000:900"), "must be an object"),
        (
            json!({"prices_minor": [900]}),
            "weight_brackets_g must be an array",
        ),
        (
            json!({"weight_brackets_g": [2000]}),
            "prices_minor must be an array",
        ),
        (
            json!({"weight_brackets_g": ["2000"], "prices_minor": [900]}),
            "/rate_table/weight_brackets_g/0: must be an integer",
        ),
        (
            json!({"weight_brackets_g": [], "prices_minor": []}),
            "at least one weight bracket",
        ),
        (
            json!({"weight_brackets_g": [2000, 5000], "prices_minor": [900]}),
            "must be the same length",
        ),
        (
            json!({"weight_brackets_g": [5000, 2000], "prices_minor": [900, 1400]}),
            "strictly ascending and positive",
        ),
    ];
    for (table, expected) in cases {
        let error = refusal(table);
        assert!(error.contains(expected), "{expected} missing from {error}");
    }
}

#[test]
fn a_non_positive_first_bracket_is_refused_by_the_same_guard() {
    // A zero or negative bound would make the first band unreachable while still looking
    // priced. The request's rule table names the bracket before the parser's ascending
    // guard sees it, with the schema's floor.
    let error = refusal(json!({"weight_brackets_g": [0], "prices_minor": [900]}));
    assert!(
        error.ends_with("/rate_table/weight_brackets_g/0: must be at least 1"),
        "{error}"
    );
}

#[test]
fn a_weight_above_the_last_bracket_is_never_reported_as_an_answer() {
    // A weight past the last bracket still *ranks* worst during search rather than
    // aborting it -- that is what lets a priceable container win a round, and
    // `an_unpriceable_container_loses_to_a_priceable_one_at_eight_units` pins it. What
    // this asks is the other half: when no priceable alternative exists, the sentinel
    // must not surface. It used to: the run came back `feasible` with a landed
    // cost of `i64::MAX`, quoting a price the carrier never published. The single
    // container here is the only one on offer and cannot price the load, so the run is
    // refused instead, naming the container and the bracket it ran past.
    let error =
        pack_json(&request(json!({"weight_brackets_g": [1], "prices_minor": [900]})).to_string())
            .expect_err("a shipment with no priceable container is refused");
    let message = error.to_string();
    assert!(message.contains("\"small\""), "{message}");
    assert!(message.contains("bills at 5400 g"), "{message}");
    assert!(message.contains("last bracket (1 g)"), "{message}");
    assert!(message.contains("no published price"), "{message}");
}

#[test]
fn the_objective_refuses_a_container_it_cannot_price() {
    // Rating some containers and not others would compare a priced packing against an
    // unpriced one as though the unpriced were free.
    let mut scene = request(json!({"weight_brackets_g": [20000], "prices_minor": [900]}));
    scene["containers"][0]
        .as_object_mut()
        .expect("an object")
        .remove("rate_table");
    let error = pack_json(&scene.to_string()).expect_err("an unrated container is refused");
    assert!(error.to_string().contains("rate_table"), "{error}");
}

#[test]
fn a_request_that_omits_a_rate_table_entirely_is_untouched_by_this_feature() {
    // The whole feature is additive: without the objective, a container needs no tariff
    // and nothing about the answer changes.
    let mut scene = request(json!({"weight_brackets_g": [20000], "prices_minor": [900]}));
    scene["configuration"]
        .as_object_mut()
        .expect("an object")
        .remove("objective");
    scene["containers"][0]
        .as_object_mut()
        .expect("an object")
        .remove("rate_table");
    let result = pack(&scene);
    assert_eq!(result["summary"]["unpacked_item_count"], 0);
    assert_eq!(result["objective"], "default");
}

#[test]
fn the_exact_solver_prices_a_shipment_the_same_way_the_portfolio_does() {
    // `exact.rs` carries its own landed-cost accumulation, a second copy of the
    // arithmetic in `extreme.rs`. Two copies of a pricing rule that are never compared
    // are two rules, so this asks both for the same scene and requires the same money.
    let tariff = json!({
        "weight_brackets_g": [2000, 5400, 20000],
        "prices_minor": [900, 1400, 2500],
        "minimum_charge_minor": 1000,
        "fuel_surcharge_permille": 100,
    });
    let mut exact = request(tariff.clone());
    exact["configuration"]["solvers"] = json!(["exact_small"]);
    exact["configuration"]["exact_item_limit"] = json!(8);

    let portfolio_cost = pack(&request(tariff))["score"][1].clone();
    let exact_cost = pack(&exact)["score"][1].clone();
    assert_eq!(
        exact_cost, portfolio_cost,
        "two copies of one tariff must agree"
    );
    assert_eq!(exact_cost, json!(1_540));
}

#[test]
fn the_exact_solver_also_refuses_an_unpriceable_shipment() {
    // The same `i128::MAX` sentinel lives in `exact.rs` and reached the result through
    // the same narrowing that used to turn it into -1. Worth asking this solver directly
    // rather than trusting that guarding the shared exit guarded both callers.
    let mut scene = request(json!({"weight_brackets_g": [1], "prices_minor": [900]}));
    scene["configuration"]["solvers"] = json!(["exact_small"]);
    scene["configuration"]["exact_item_limit"] = json!(8);
    let error = pack_json(&scene.to_string()).expect_err("the exact solver refuses too");
    assert!(error.to_string().contains("no published price"), "{error}");
}

#[test]
fn exact_small_does_not_prune_a_heavier_promotional_rate_band() {
    let scene = json!({
        "units": {"length": "mm"},
        "configuration": {
            "solvers": ["exact_small"],
            "objective": "lowest_landed_cost",
            "exact_item_limit": 7,
            "max_containers": 1,
            "dimensional_weight_divisor": 10000,
            "dimensional_weight_length_unit": "cm",
            "dimensional_weight_weight_unit": "kg",
            "time_limit_ms": 300000,
        },
        "items": [
            {"id": "a-light", "weight": "100 g", "dimensions": {"length": "100", "width": "100", "height": "100"}},
            {"id": "b-light", "weight": "100 g", "dimensions": {"length": "100", "width": "100", "height": "100"}},
            {"id": "z-heavy", "weight": "800 g", "dimensions": {"length": "100", "width": "100", "height": "100"}}
        ],
        "containers": [{
            "id": "bin", "quantity": 1,
            "inner_dimensions": {"length": "200", "width": "100", "height": "100"},
            "rate_table": {"weight_brackets_g": [200, 900], "prices_minor": [100, 10]}
        }]
    });
    let result = pack(&scene);
    assert_eq!(result["score"][0], json!(1), "{result}");
    assert_eq!(result["score"][1], json!(10), "{result}");
    assert!(
        result["containers"][0]["placements"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|placement| placement["item_id"] == "z-heavy#1"),
        "{result}"
    );
}

#[test]
fn a_single_item_never_settles_for_an_unpriceable_container_over_a_priced_one() {
    // Found by adversarial review, not by reading: `grid_selection_key` in `grid.rs`
    // handled `shipping_cost` but had no arm for `lowest_landed_cost` at all, so a
    // single-item request -- exactly the shape this closed-form lattice solver takes --
    // fell through to the untagged default key (`cost_minor`/unused volume/height, no
    // reference to price) and could commit to whichever container that key preferred,
    // with nothing to correct it once chosen. The smaller of these two containers is
    // physically roomier per the default key's own terms (less wasted volume) and
    // genuinely cannot price this shipment; the larger one can, at 1500. A caller must
    // never receive the one it cannot ship over the one it can.
    let request = json!({
        "units": {"length": "mm"},
        "configuration": {
            "objective": "lowest_landed_cost",
            "dimensional_weight_divisor": 5000,
            "dimensional_weight_length_unit": "cm",
            "dimensional_weight_weight_unit": "kg",
            "time_limit_ms": 300000,
        },
        "items": [{
            "id": "dense", "quantity": 1, "weight": "500 g",
            "dimensions": {"length": "100", "width": "100", "height": "100"},
        }],
        "containers": [
            {
                "id": "small_unpriceable",
                "inner_dimensions": {"length": "150", "width": "150", "height": "150"},
                "rate_table": {"weight_brackets_g": [1], "prices_minor": [900]},
            },
            {
                "id": "big_priced",
                "inner_dimensions": {"length": "300", "width": "300", "height": "300"},
                "rate_table": {"weight_brackets_g": [20000], "prices_minor": [1500]},
            },
        ],
    });
    let result = pack(&request);
    assert_eq!(
        result["containers"][0]["container_type"], "big_priced",
        "chose the container it cannot price over the one it can: {result}"
    );
    assert_eq!(result["score"][1], 1_500);
}

#[test]
fn an_unpriceable_container_loses_to_a_priceable_one_at_eight_units() {
    // The general greedy portfolio, not the closed-form lattice: `try_grid` already
    // stands down for this objective, and eight units is past the single-item shape the
    // fast paths take, so this lands in `extreme::container_selection_key`.
    // That key ranked by *billed weight*, and the smaller container bills lighter
    // (5400 g of dimensional weight against 12800 g) while its tariff runs out at
    // 2000 g. Ranking by grams therefore chose the one shipment the caller cannot buy,
    // over one available at 1500. Ranking by the money the finished score will charge
    // makes the unpriceable trial sort behind every priceable one instead.
    let request = json!({
        "units": {"length": "mm"},
        "configuration": {
            "objective": "lowest_landed_cost",
            "dimensional_weight_divisor": 5000,
            "dimensional_weight_length_unit": "cm",
            "dimensional_weight_weight_unit": "kg",
            "time_limit_ms": 300000,
        },
        "items": [{
            "id": "box", "quantity": 8, "weight": "500 g",
            "dimensions": {"length": "100", "width": "100", "height": "100"},
        }],
        "containers": [
            {
                "id": "alpha_unpriceable",
                "inner_dimensions": {"length": "300", "width": "300", "height": "300"},
                "rate_table": {"weight_brackets_g": [2000], "prices_minor": [900]},
            },
            {
                "id": "beta_priceable",
                "inner_dimensions": {"length": "400", "width": "400", "height": "400"},
                "rate_table": {"weight_brackets_g": [20000], "prices_minor": [1500]},
            },
        ],
    });
    let result = pack(&request);
    assert_eq!(
        result["containers"][0]["container_type"], "beta_priceable",
        "chose the container it cannot price over the one it can: {result}"
    );
    assert_eq!(result["score"][1], 1_500);
    assert_eq!(
        result["unpacked_items"].as_array().expect("an array").len(),
        0
    );
}

#[test]
fn a_bracket_step_makes_the_cheaper_shipment_the_heavier_one() {
    // Ranking by billed weight and ranking by money only agree while price rises
    // smoothly with weight. Here `heavy_but_cheap` bills at 12800 g and costs 400;
    // `light_but_dear` bills at 5400 g -- less than half -- and costs 900, because its
    // bracket steps just below. The old grams-based key had no way to see that, and
    // this is the case the objective exists for: it is named lowest_landed_*cost*.
    let request = json!({
        "units": {"length": "mm"},
        "configuration": {
            "objective": "lowest_landed_cost",
            "dimensional_weight_divisor": 5000,
            "dimensional_weight_length_unit": "cm",
            "dimensional_weight_weight_unit": "kg",
            "time_limit_ms": 300000,
        },
        "items": [{
            "id": "box", "quantity": 8, "weight": "500 g",
            "dimensions": {"length": "100", "width": "100", "height": "100"},
        }],
        "containers": [
            {
                "id": "light_but_dear",
                "inner_dimensions": {"length": "300", "width": "300", "height": "300"},
                "rate_table": {"weight_brackets_g": [20000], "prices_minor": [900]},
            },
            {
                "id": "heavy_but_cheap",
                "inner_dimensions": {"length": "400", "width": "400", "height": "400"},
                "rate_table": {"weight_brackets_g": [20000], "prices_minor": [400]},
            },
        ],
    });
    let result = pack(&request);
    assert_eq!(
        result["containers"][0]["container_type"], "heavy_but_cheap",
        "ranked in grams rather than in money: {result}"
    );
    assert_eq!(result["score"][1], 400);
}

fn two_container_eight_unit_scene(extra_config: Value) -> Value {
    let mut request = json!({
        "units": {"length": "mm"},
        "configuration": {
            "objective": "lowest_landed_cost",
            "dimensional_weight_divisor": 5000,
            "dimensional_weight_length_unit": "cm",
            "dimensional_weight_weight_unit": "kg",
            "time_limit_ms": 300000,
        },
        "items": [{
            "id": "box", "quantity": 8, "weight": "500 g",
            "dimensions": {"length": "100", "width": "100", "height": "100"},
        }],
        "containers": [
            {
                "id": "alpha_unpriceable",
                "inner_dimensions": {"length": "300", "width": "300", "height": "300"},
                "rate_table": {"weight_brackets_g": [2000], "prices_minor": [900]},
            },
            {
                "id": "beta_priceable",
                "inner_dimensions": {"length": "400", "width": "400", "height": "400"},
                "rate_table": {"weight_brackets_g": [20000], "prices_minor": [1500]},
            },
        ],
    });
    if let Some(extra) = extra_config.as_object() {
        for (key, value) in extra {
            request["configuration"][key] = value.clone();
        }
    }
    request
}

#[test]
fn a_pinned_maximal_spaces_run_still_buys_the_priceable_container() {
    // The maximal-space walk opens the first container of a static, tariff-blind order,
    // so pinning it used to refuse this request outright -- "no published price" --
    // although beta ships it at 1500 (review). Under this objective the pin now
    // falls back to the money-ranked greedy, the same stand-down the lattice uses.
    let request = two_container_eight_unit_scene(json!({"solvers": ["maximal_spaces"]}));
    let result = pack(&request);
    assert_eq!(
        result["containers"][0]["container_type"], "beta_priceable",
        "the pin must not abort a shippable request: {result}"
    );
    assert_eq!(result["score"][1], 1_500);
}

#[test]
fn a_pinned_homogeneous_blocks_run_prices_the_round() {
    // The block loader ranked its round progress-first with no reference to the tariff,
    // so a pin committed the unpriceable container (review). Its round now ranks
    // in money first, like the general greedy, and keeps its own keys after that. The
    // node budget is raised because a pinned block search under the default plan budget
    // exhausts before committing anything (the starvation, objective-agnostic).
    let request = two_container_eight_unit_scene(json!({
        "solvers": ["homogeneous_blocks"],
        "container_plan_node_limit": 1000000,
    }));
    let result = pack(&request);
    assert_eq!(
        result["containers"][0]["container_type"], "beta_priceable",
        "the block round must rank in money: {result}"
    );
    assert_eq!(result["score"][1], 1_500);
}

#[test]
fn alternatives_never_quote_the_sentinel() {
    // The refusal guarded only the winner; `alternatives` (top_k defaults to 3) carried
    // feasible-status packings of the unpriceable container with i64::MAX as their
    // landed cost -- the exact number the objective exists to never invent (
    // review). Runner-ups the tariff cannot price are dropped before the slice.
    let mut request = two_container_eight_unit_scene(json!({"solver_profile": "quality"}));
    request["items"][0]["quantity"] = json!(1);
    let result = pack(&request);
    assert_eq!(result["score"][1], 1_500, "{result}");
    let alternatives = result["alternatives"]
        .as_array()
        .expect("alternatives array");
    for alternative in alternatives {
        assert_ne!(
            alternative["score"][1],
            json!(i64::MAX),
            "an alternative quotes the sentinel: {alternative}"
        );
        for container in alternative["containers"].as_array().into_iter().flatten() {
            assert_ne!(
                container["container_type"], "alpha_unpriceable",
                "an alternative carries the unpriceable container: {alternative}"
            );
        }
    }
}

fn rebalance_scene() -> Value {
    // Three bricks land in `wide` (3000 g of a 4000 g card), one in `narrow` (1000 g of
    // a 1500 g card). The only spread-improving move -- one brick into `narrow` -- would
    // bill it at 2000 g, past its last bracket.
    json!({
        "units": {"length": "mm"},
        "configuration": {
            "objective": "lowest_landed_cost",
            "dimensional_weight_divisor": 5000,
            "dimensional_weight_length_unit": "cm",
            "dimensional_weight_weight_unit": "kg",
            "time_limit_ms": 300000,
        },
        "items": [{
            "id": "brick", "quantity": 4, "weight": "1000 g",
            "dimensions": {"length": "100", "width": "100", "height": "100"},
        }],
        "containers": [
            {
                "id": "wide",
                "inner_dimensions": {"length": "300", "width": "100", "height": "100"},
                "rate_table": {"weight_brackets_g": [4000], "prices_minor": [500]},
            },
            {
                "id": "narrow",
                "inner_dimensions": {"length": "400", "width": "100", "height": "100"},
                "rate_table": {"weight_brackets_g": [1500], "prices_minor": [300]},
            },
        ],
    })
}

#[test]
fn a_rebalance_move_never_prices_a_container_past_its_bracket() {
    use packvium_core::rebalance_json;
    let request = rebalance_scene();
    let result = pack(&request);
    let rebalanced: Value = serde_json::from_str(
        &rebalance_json(&request.to_string(), &result.to_string(), 4)
            .expect("a priceable input rebalances"),
    )
    .expect("rebalance JSON");
    // The spread-improving move exists geometrically but would strand `narrow` past its
    // tariff, so under this objective it is not an improvement and must not be made.
    assert_eq!(rebalanced["moves"], json!([]), "{rebalanced}");
    assert_eq!(rebalanced["improved"], json!(false));
    // A control without the landed objective still moves: the veto is objective-gated,
    // not a general rebalance regression. The control's containers both hold three
    // bricks, because the default objective packs the whole load into the 400 mm box
    // and a one-container packing has nothing to rebalance.
    let mut plain = request.clone();
    plain["configuration"] = json!({"time_limit_ms": 300000});
    plain["containers"] = json!([
        {"id": "wide", "inner_dimensions": {"length": "300", "width": "100", "height": "100"}},
        {"id": "narrow", "inner_dimensions": {"length": "300", "width": "100", "height": "100"}},
    ]);
    let plain_result = pack(&plain);
    let plain_rebalanced: Value = serde_json::from_str(
        &rebalance_json(&plain.to_string(), &plain_result.to_string(), 4)
            .expect("the plain scene rebalances"),
    )
    .expect("rebalance JSON");
    assert_eq!(
        plain_rebalanced["moves"].as_array().map(Vec::len),
        Some(1),
        "{plain_rebalanced}"
    );
}

#[test]
fn rebalance_refuses_an_unpriceable_input() {
    use packvium_core::rebalance_json;
    // A caller handing rebalance a packing whose container already bills past its
    // bracket gets the same refusal `pack` gives on the way out, not a rebalanced
    // version of a shipment with no published price (review).
    let request = rebalance_scene();
    let result = pack(&request);
    let mut overweight = request.clone();
    overweight["containers"][0]["rate_table"] = json!({
        "weight_brackets_g": [1000], "prices_minor": [500],
    });
    let error = rebalance_json(&overweight.to_string(), &result.to_string(), 4)
        .expect_err("an unpriceable input is refused");
    assert!(error.to_string().contains("no published price"), "{error}");
}

#[test]
fn rebalance_applies_the_same_landed_cost_admission_as_pack() {
    use packvium_core::rebalance_json;
    let request = rebalance_scene();
    let result = pack(&request);

    let mut missing_divisor = request.clone();
    missing_divisor["configuration"]
        .as_object_mut()
        .expect("configuration object")
        .remove("dimensional_weight_divisor");
    let error = rebalance_json(&missing_divisor.to_string(), &result.to_string(), 4)
        .expect_err("a pricing objective without a divisor is refused");
    assert!(
        error.to_string().contains("dimensional_weight_divisor"),
        "{error}"
    );

    let mut untabled = request.clone();
    untabled["containers"]
        .as_array_mut()
        .expect("containers array")
        .push(json!({
            "id": "untabled",
            "inner_dimensions": {"length": "500", "width": "500", "height": "500"},
        }));
    let error = rebalance_json(&untabled.to_string(), &result.to_string(), 4)
        .expect_err("an unused untabled container still makes landed cost undefined");
    assert!(error.to_string().contains("untabled"), "{error}");
}
