//! Trucking: a multi-stop delivery van, loaded so each stop can be unloaded in turn and
//! neither axle is overloaded.
//!
//!     cargo run --example trucking
//!
//! Three request fields turn a box-packing problem into a vehicle-loading one:
//!
//! - `stop_index` on an item: the stop it comes off at. Stops are visited in ascending order,
//!   so nothing due later may rest on top of something due earlier.
//! - `access_directions` on a container: the walls it can be unloaded through. With a door
//!   named, nothing due later may stand between an earlier item and that door either. A
//!   container that names no door is not checked for this at all -- which is not the same as
//!   one with every wall open.
//! - `axles` on a container: exactly two, front and rear, each at a position along the
//!   container's length with an optional `max_load`. The load on each is solved as a
//!   two-point beam over the gross weight -- the container's tare plus everything in it.
//!
//! All three are data in the request, so the same JSON loads the same van in the Python, PHP
//! and JavaScript engines.

use serde_json::{Value, json};

/// Weight ticks per kilogram: a weight is an exact count of eighths of a microgram.
const TICKS_PER_KG: i128 = 8_000_000_000;

fn configuration() -> Value {
    // Counted work bounds the search; the time limit is only a safety fuse.
    json!({
        "time_limit_ms": 60000,
        // Without a support rule an item may rest on nothing at all: the default ratio is 0,
        // which suits a solver test and never a truck. Ask for three quarters of every base.
        "minimum_support_ratio": 0.75,
        "effort_budget": {
            "max_candidates_evaluated": 1000000,
            "max_placement_attempts": 1000000,
            "max_search_nodes": 1000000
        }
    })
}

/// A 600 x 800 crate that must stay upright, due off at `stop` when it has one.
fn crate_for(stop: Option<u64>, id: &str, quantity: u64, weight: &str) -> Value {
    let mut item = json!({ "id": id, "quantity": quantity, "weight": weight, "keep_upright": true,
                           "dimensions": { "length": "600", "width": "800", "height": "700" } });
    if let Some(stop) = stop {
        item["stop_index"] = json!(stop);
    }
    item
}

/// One van, one crate wide and one crate high: a 3.2 m cargo box whose door is named by
/// `doors` (`+x` is the far end of its length, the back). The front axle sits 400 mm behind
/// the bulkhead and the rear axle 2.6 m behind that; the front one may carry a rating.
fn van(doors: &[&str], front_limit: Option<&str>) -> Value {
    let mut front = json!({ "position": "400" });
    if let Some(limit) = front_limit {
        front["max_load"] = json!(limit);
    }
    json!({
        "id": "van", "quantity": 1, "tare_weight": "900 kg", "max_payload": "1500 kg",
        "inner_dimensions": { "length": "3200", "width": "850", "height": "800" },
        "access_directions": doors,
        "axles": [front, { "position": "3000" }]
    })
}

fn pack(items: &Value, van: Value) -> Result<Value, Box<dyn std::error::Error>> {
    let request = json!({
        "units": { "length": "mm" },
        "configuration": configuration(),
        "items": items,
        "containers": [van],
    });
    let text = packvium_core::pack_json(&request.to_string())?;
    Ok(serde_json::from_str(&text)?)
}

fn integer(value: &Value) -> i128 {
    value
        .as_str()
        .and_then(|text| text.parse().ok())
        .unwrap_or(0)
}

/// Axle loads come back as exact fractions of weight ticks; truncate to kilograms to print.
fn axle_kg(van: &Value) -> (i128, i128) {
    let reactions = &van["axle_reactions"];
    let denominator = integer(&reactions["denominator"]).max(1) * TICKS_PER_KG;
    (
        integer(&reactions["front_numerator"]) / denominator,
        integer(&reactions["rear_numerator"]) / denominator,
    )
}

fn show(title: &str, result: &Value, stops: &Value) {
    println!("\n{title}");
    let stop_of = |item_type: &str| {
        stops
            .as_array()
            .into_iter()
            .flatten()
            .find(|item| item["id"] == item_type)
            .and_then(|item| item["stop_index"].as_u64())
            .map_or_else(|| "-".to_owned(), |stop| stop.to_string())
    };
    for van in result["containers"].as_array().into_iter().flatten() {
        let mut placements: Vec<&Value> =
            van["placements"].as_array().into_iter().flatten().collect();
        // Front of the van first, so the door is at the bottom of the list.
        placements.sort_by_key(|placement| {
            let at = &placement["position"];
            (at["x"]["ticks"].as_i64(), at["z"]["ticks"].as_i64())
        });
        for placement in placements {
            let at = &placement["position"];
            println!(
                "  stop {:<2} {:<12} x {:>4} mm  z {:>3} mm",
                stop_of(placement["item_type"].as_str().unwrap_or_default()),
                placement["item_id"].as_str().unwrap_or_default(),
                at["x"]["value"].as_str().unwrap_or_default(),
                at["z"]["value"].as_str().unwrap_or_default(),
            );
        }
        let (front, rear) = axle_kg(van);
        println!("  axles: front {front} kg, rear {rear} kg");
    }
    for unpacked in result["unpacked_items"].as_array().into_iter().flatten() {
        println!(
            "  left behind: {} ({})",
            unpacked["item_id"].as_str().unwrap_or_default(),
            unpacked["reason"].as_str().unwrap_or_default()
        );
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Three drops on one route. Stop 0 is visited first, so its crates must come out first.
    let route = json!([
        crate_for(Some(0), "bakery", 2, "120 kg"),
        crate_for(Some(1), "pharmacy", 1, "90 kg"),
        crate_for(Some(2), "hardware", 2, "200 kg"),
    ]);

    // ------------------------------------------------------------ the door decides
    //
    // The van is one crate wide and one crate high, so every crate is in the same lane.
    // With the door at the back, the first stop's crates must be the ones nearest the back.
    // Move the door to the front bulkhead (`-x`) and the same route loads the other way
    // round. The crates are the same; only the door moved.
    show(
        "door at the back (+x): stop 0 nearest the door",
        &pack(&route, van(&["+x"], None))?,
        &route,
    );
    show(
        "door at the front (-x): the same route, mirrored",
        &pack(&route, van(&["-x"], None))?,
        &route,
    );

    // ------------------------------------------------------------ the axles decide
    //
    // No route this time: four light crates and an engine block that weighs more than all
    // of them together. Packing prefers the front of the van, and right behind the bulkhead
    // is exactly where 600 kg overloads the front axle once it is rated for less. With a
    // limit set, the engine moves back towards the middle of the wheelbase, and both
    // reactions stay inside their ratings.
    let workshop = json!([
        crate_for(None, "engine", 1, "600 kg"),
        crate_for(None, "parts", 4, "60 kg"),
    ]);
    show(
        "an engine block, axles unrated",
        &pack(&workshop, van(&["+x"], None))?,
        &workshop,
    );
    show(
        "the same load, front axle rated 900 kg",
        &pack(&workshop, van(&["+x"], Some("900 kg")))?,
        &workshop,
    );
    Ok(())
}
