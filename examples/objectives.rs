//! Objectives: six ways to be "best", and the scenes where they disagree.
//!
//!     cargo run --example objectives
//!
//! Every solve returns the arrangement that scores best -- but "best" is a choice, and it is
//! the setting most likely to make the engine look wrong when it is merely answering a
//! different question than you meant to ask. Each scene below is built so that two
//! objectives genuinely pick different containers, so the difference is visible rather than
//! asserted.
//!
//! The score is always a vector of exact integers compared left to right, never a float, and
//! its first key is always the number of unpacked items: no objective will leave an item
//! behind to save money. Ratios are parts per million.

use serde_json::{Value, json};

type Outcome = Result<Value, packvium_core::PackError>;

/// Solve `items` into `containers` under `objective`, with any extra configuration merged in.
fn solve(objective: &str, extra: Value, items: &Value, containers: Value) -> Outcome {
    let mut configuration = json!({
        "objective": objective,
        // Counted work bounds the search; the time limit is only a safety fuse.
        "time_limit_ms": 60000,
        "effort_budget": {
            "max_candidates_evaluated": 1000000,
            "max_placement_attempts": 1000000,
            "max_search_nodes": 1000000
        }
    });
    if let (Some(target), Some(extra)) = (configuration.as_object_mut(), extra.as_object()) {
        target.extend(extra.clone());
    }
    let request = json!({
        "units": { "length": "mm" },
        "configuration": configuration,
        "items": items,
        "containers": containers,
    });
    let text = packvium_core::pack_json(&request.to_string())?;
    Ok(serde_json::from_str(&text)?)
}

/// Which container type won, and the score that decided it.
fn report(label: &str, outcome: Outcome) {
    match outcome {
        Ok(result) => {
            let chosen = result["containers"][0]["container_type"]
                .as_str()
                .unwrap_or("none");
            println!("{label:<18} {chosen:<6} score {}", result["score"]);
        }
        Err(refusal) => println!("{label:<18} refused: {} -- {refusal}", refusal.code()),
    }
}

fn container(id: &str, edge: &str, fields: Value) -> Value {
    let mut container = json!({
        "id": id, "max_payload": "20 kg",
        "inner_dimensions": { "length": edge, "width": edge, "height": edge }
    });
    if let (Some(target), Some(fields)) = (container.as_object_mut(), fields.as_object()) {
        target.extend(fields.clone());
    }
    container
}

/// The divisor carriers bill dimensional weight with. Without one, the two carrier
/// objectives refuse rather than guess: a wrong divisor silently misprices every shipment.
fn dimensional() -> Value {
    json!({
        "dimensional_weight_divisor": 5000,
        "dimensional_weight_length_unit": "cm",
        "dimensional_weight_weight_unit": "kg"
    })
}

fn main() {
    let widgets = json!([{ "id": "widget", "quantity": 8, "weight": "500 g",
                           "dimensions": { "length": "100", "width": "100", "height": "100" } }]);
    let snug = container("snug", "300", json!({ "cost_minor": 500 }));
    let roomy = container("roomy", "400", json!({ "cost_minor": 150 }));
    let both = json!([snug, roomy]);

    // `default` -- fewest containers, then the tightest fit. What you want when the
    // containers are interchangeable and you are simply trying not to open another box.
    report(
        "default",
        solve("default", json!({}), &widgets, both.clone()),
    );

    // `lowest_cost` -- the cheapest *packaging*. `cost_minor` is what the box itself costs
    // you, so this is the objective for a warehouse buying cartons, not for a shipper paying
    // a carrier. It prefers the roomy box precisely because the snug one costs more.
    report(
        "lowest_cost",
        solve("lowest_cost", json!({}), &widgets, both.clone()),
    );

    // `shipping_cost` -- carrier-billable *weight*: the greater of gross weight and
    // dimensional weight, so a big light box can bill more than a small heavy one. The roomy
    // box is cheaper to buy and dearer to ship.
    report(
        "shipping_cost",
        solve("shipping_cost", dimensional(), &widgets, both.clone()),
    );

    // `lowest_landed_cost` -- carrier-billable *money*, from a rate card carried in the
    // request as data. Weight and money do not always agree: here the roomy box bills
    // heavier (12,800 g dimensional against 5,400 g) and still costs less, because the snug
    // box's carrier charges a steep first bracket. Rank by weight and you pick the wrong box.
    let rated = |id: &str, edge: &str, prices: [i64; 2]| {
        container(
            id,
            edge,
            json!({ "rate_table": { "weight_brackets_g": [6000, 20000], "prices_minor": prices } }),
        )
    };
    report(
        "lowest_landed_cost",
        solve(
            "lowest_landed_cost",
            dimensional(),
            &widgets,
            json!([
                rated("snug", "300", [2400, 3100]),
                rated("roomy", "400", [900, 1500])
            ]),
        ),
    );

    // A rate card that stops short of the shipment is a refusal, never a silent clamp to the
    // top bracket: you would otherwise be quoted a price the carrier never published.
    let too_narrow = container(
        "roomy",
        "400",
        json!({ "rate_table": { "weight_brackets_g": [2000], "prices_minor": [900] } }),
    );
    report(
        "  (narrow card)",
        solve(
            "lowest_landed_cost",
            dimensional(),
            &widgets,
            json!([too_narrow]),
        ),
    );

    // `open_dimension_height` -- the shortest stack. For a container with no lid, or a
    // pallet whose load must stay under a doorway.
    report(
        "open_dimension",
        solve("open_dimension_height", json!({}), &widgets, both),
    );

    // `maximum_value` -- when not everything fits, leave the *cheap* things behind. Ranked
    // by value forgone, after unpacked count. `quantity: 1` on the container is what makes
    // this a choice at all: with unlimited boxes the packer would simply open a second one.
    let mixed = json!([
        { "id": "gold", "quantity": 2, "weight": "500 g", "value": 90000,
          "dimensions": { "length": "100", "width": "100", "height": "100" } },
        { "id": "gravel", "quantity": 2, "weight": "500 g", "value": 10,
          "dimensions": { "length": "100", "width": "100", "height": "100" } }
    ]);
    let tiny = json!([{ "id": "tiny", "quantity": 1, "max_payload": "20 kg",
                        "inner_dimensions": { "length": "200", "width": "100", "height": "100" } }]);
    match solve("maximum_value", json!({}), &mixed, tiny) {
        Ok(result) => {
            let ids = |list: &Value, key: &str| -> Vec<String> {
                let mut ids: Vec<String> = list
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|entry| entry[key].as_str().map(str::to_owned))
                    .collect();
                ids.sort();
                ids
            };
            let kept = ids(&result["containers"][0]["placements"], "item_type");
            let left = ids(&result["unpacked_items"], "item_type");
            println!("maximum_value      packed {kept:?}, left behind {left:?}");
        }
        Err(refusal) => println!("maximum_value      refused: {refusal}"),
    }
}
