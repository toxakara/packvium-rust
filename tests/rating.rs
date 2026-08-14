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
            "weight_brackets_g must hold integers",
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
    // priced. It shares the ascending check's message deliberately: both describe the
    // same malformed ladder.
    let error = refusal(json!({"weight_brackets_g": [0], "prices_minor": [900]}));
    assert!(error.contains("strictly ascending and positive"), "{error}");
}

#[test]
fn a_weight_above_the_last_bracket_is_ranked_worst_rather_than_free() {
    // Unlike a missing rate card -- a static property of the request, refused at
    // admission -- this depends on how the search filled the box, so it must lose a
    // candidate rather than abort the run. Ranking it free would make the objective
    // prefer exactly the packing the caller cannot ship.
    let result = pack(&request(json!({
        "weight_brackets_g": [1],
        "prices_minor": [900],
    })));
    assert!(
        result["score"][1].as_i64().expect("a landed cost") >= i64::from(i32::MAX),
        "an unpriceable shipment must rank worst, got {}",
        result["score"][1]
    );
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
fn the_exact_solver_also_ranks_an_unpriceable_shipment_last() {
    // The same `i128::MAX` sentinel lives in `exact.rs`, and it reached the result
    // through the same narrowing that used to turn it into -1. Worth asking this solver
    // directly rather than trusting that fixing the shared boundary fixed both callers.
    let mut scene = request(json!({"weight_brackets_g": [1], "prices_minor": [900]}));
    scene["configuration"]["solvers"] = json!(["exact_small"]);
    scene["configuration"]["exact_item_limit"] = json!(8);
    let cost = pack(&scene)["score"][1].as_i64().expect("a landed cost");
    assert!(cost >= i64::from(i32::MAX), "must rank worst, got {cost}");
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
