//! Pack an order into cartons and read the answer as Rust types.
//!
//!     cargo run --example basic
//!
//! The engine speaks one JSON contract in every language: a request goes in as text and a
//! result comes out as text. In Rust you build the request with `serde_json::json!` and read
//! the result back into your own `#[derive(Deserialize)]` structs, naming only the fields
//! you use. Every length and weight arrives as a measure carrying exact integer `ticks`
//! beside a rendered decimal `value`, so you compute with the ticks and print the value.

use packvium_core::{PackError, explain_reason};
use serde::Deserialize;
use serde_json::json;

/// The part of a packing result this program reads. Unknown fields are ignored, so a
/// result that gains a field later still deserializes.
#[derive(Debug, Deserialize)]
struct Outcome {
    status: String,
    complete: bool,
    /// The objective vector: exact integers compared left to right, fewest unpacked first.
    score: Vec<i64>,
    termination: Fact,
    containers: Vec<PackedContainer>,
    unpacked_items: Vec<UnpackedItem>,
}

#[derive(Debug, Deserialize)]
struct Fact {
    code: String,
}

#[derive(Debug, Deserialize)]
struct PackedContainer {
    id: String,
    payload_weight: Measure,
    volume_utilization: String,
    placements: Vec<Placement>,
}

#[derive(Debug, Deserialize)]
struct Placement {
    item_id: String,
    orientation: String,
    position: Position,
}

#[derive(Debug, Deserialize)]
struct Position {
    x: Measure,
    y: Measure,
    z: Measure,
}

/// A length or a weight: `ticks` is the exact integer, `value` the same number rendered
/// in `unit` for a person.
#[derive(Debug, Deserialize)]
struct Measure {
    ticks: i64,
    value: String,
    unit: String,
}

#[derive(Debug, Deserialize)]
struct UnpackedItem {
    item_id: String,
    reason: String,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let request = json!({
        "units": { "length": "mm" },
        "configuration": {
            // Bound the search by counted work rather than by the clock, so the same request
            // gives the same answer on a fast laptop and on a loaded CI runner. The generous
            // time limit is only a safety fuse against a genuine hang.
            "time_limit_ms": 60000,
            "effort_budget": {
                "max_candidates_evaluated": 1000000,
                "max_placement_attempts": 1000000,
                "max_search_nodes": 1000000
            }
        },
        "items": [
            { "id": "book", "quantity": 4, "weight": "650 g",
              "dimensions": { "length": "210", "width": "140", "height": "30" } },
            { "id": "mug", "quantity": 2, "weight": "350 g",
              "dimensions": { "length": "120", "width": "90", "height": "100" } },
            // Longer than the carton in every orientation, so it cannot ship in one.
            { "id": "umbrella", "quantity": 1, "weight": "500 g",
              "dimensions": { "length": "900", "width": "80", "height": "80" } }
        ],
        "containers": [
            { "id": "carton", "max_payload": "10 kg",
              "inner_dimensions": { "length": "400", "width": "300", "height": "250" } }
        ]
    });

    let text = match packvium_core::pack_json(&request.to_string()) {
        Ok(text) => text,
        // A request the engine refuses names the offending field; see the errors example.
        Err(PackError::InvalidRequest(error)) => {
            return Err(format!("fix {}: {}", error.field(), error.detail()).into());
        }
        Err(other) => return Err(other.into()),
    };
    let outcome: Outcome = serde_json::from_str(&text)?;

    println!("status:      {}", outcome.status);
    println!("complete:    {}", outcome.complete);
    println!("termination: {}", outcome.termination.code);
    println!("score:       {:?}", outcome.score);

    for container in &outcome.containers {
        println!(
            "\n{}: {} item(s), {} {} payload, {} of the volume used",
            container.id,
            container.placements.len(),
            container.payload_weight.value,
            container.payload_weight.unit,
            container.volume_utilization,
        );
        for placement in &container.placements {
            let at = &placement.position;
            println!(
                "  {:<10} {}  at ({}, {}, {}) {}",
                placement.item_id,
                placement.orientation,
                at.x.value,
                at.y.value,
                at.z.value,
                at.x.unit,
            );
        }
        // Arithmetic belongs on ticks, never on the rendered decimals: the highest point
        // any item's origin reaches, exactly.
        let highest = container
            .placements
            .iter()
            .map(|placement| placement.position.z.ticks)
            .max()
            .unwrap_or(0);
        println!("  highest origin: {highest} ticks (1 mm = 16000 ticks)");
    }

    // An item that does not fit is part of the answer, not an error. Each one carries a
    // closed reason code, and `explain_reason` turns the code into a sentence.
    for unpacked in &outcome.unpacked_items {
        println!(
            "\nnot packed: {} -- {}: {}",
            unpacked.item_id,
            unpacked.reason,
            explain_reason(&unpacked.reason)?
        );
    }
    Ok(())
}
