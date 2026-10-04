//! Reproducibility: bound the search by counted work, and keep the clock as a fuse.
//!
//!     cargo run --example reproducibility
//!
//! The engine is deterministic: the same request, configuration and seed give the same
//! placements. There is one way to lose that, and it is the setting most callers reach for
//! first. `time_limit_ms` stops the search when a wall clock says so, and how far a search
//! gets in 200 ms depends on the machine and on what else it is doing. Two runs of the same
//! request can then keep different best answers.
//!
//! `effort_budget` stops the search by counting work instead -- candidates evaluated,
//! placements attempted, search nodes expanded -- and a count does not depend on the host.
//! The pattern is to set the budget you mean and a time limit far above what the budget can
//! take, so the clock never decides anything and only guards against a genuine hang.

use packvium_core::artifacts::build_artifact_json;
use serde_json::{Value, json};

/// A mixed order: several item sizes, two carton sizes, more than one carton needed.
fn request(configuration: Value) -> Value {
    json!({
        "units": { "length": "mm" },
        "configuration": configuration,
        "items": [
            { "id": "large", "quantity": 4, "weight": "3 kg",
              "dimensions": { "length": "300", "width": "250", "height": "200" } },
            { "id": "medium", "quantity": 8, "weight": "1500 g",
              "dimensions": { "length": "220", "width": "160", "height": "120" } },
            { "id": "flat", "quantity": 6, "weight": "800 g",
              "dimensions": { "length": "350", "width": "250", "height": "40" } },
            { "id": "small", "quantity": 12, "weight": "300 g",
              "dimensions": { "length": "110", "width": "90", "height": "70" } }
        ],
        "containers": [
            { "id": "carton-m", "cost_minor": 90,
              "inner_dimensions": { "length": "400", "width": "300", "height": "300" } },
            { "id": "carton-l", "cost_minor": 140,
              "inner_dimensions": { "length": "600", "width": "400", "height": "400" } }
        ]
    })
}

fn solve(request: &Value) -> Result<(String, Value), Box<dyn std::error::Error>> {
    let text = packvium_core::pack_json(&request.to_string())?;
    let mut result: Value = serde_json::from_str(&text)?;
    without_durations(&mut result);
    Ok((text, result))
}

/// How long a solve took is a measurement of the run, not part of the answer, and it is the
/// one number that legitimately differs between two identical solves. Drop every
/// `duration_ms` -- the result's own and each alternative's -- before comparing.
fn without_durations(value: &mut Value) {
    match value {
        Value::Object(map) => {
            map.remove("duration_ms");
            map.values_mut().for_each(without_durations);
        }
        Value::Array(list) => list.iter_mut().for_each(without_durations),
        _ => {}
    }
}

fn placed(result: &Value) -> usize {
    result["containers"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|container| container["placements"].as_array().map_or(0, Vec::len))
        .sum()
}

/// Solve twice and report what a caller can rely on.
fn twice(title: &str, configuration: Value) -> Result<(), Box<dyn std::error::Error>> {
    let request = request(configuration);
    let (text, first) = solve(&request)?;
    let (_, second) = solve(&request)?;
    println!("\n{title}");
    println!("  termination:     {}", first["termination"]["code"]);
    println!(
        "  effort limit:    {}",
        first["algorithm"]["effort_limit_reached"]
    );
    println!(
        "  placed:          {} in {} container(s), {} left out",
        placed(&first),
        first["containers"].as_array().map_or(0, Vec::len),
        first["unpacked_items"].as_array().map_or(0, Vec::len)
    );
    println!("  score:           {}", first["score"]);
    println!("  second run identical: {}", first == second);
    // The operational artifact records whether a replay of the embedded request is promised
    // to reproduce this result. A search that the clock stopped is never promised one.
    let artifact: Value =
        serde_json::from_str(&build_artifact_json(&request.to_string(), &text, "{}")?)?;
    println!(
        "  artifact replay: {}",
        artifact["provenance"]["replay"]["level"]
    );
    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // A budget far above what this order needs, and a 60 s fuse. The search finishes on its
    // own, and nothing about the host can change the answer.
    twice(
        "effort budget, 60 s fuse",
        json!({
            "solver_profile": "balanced",
            "time_limit_ms": 60000,
            "effort_budget": {
                "max_candidates_evaluated": 1000000,
                "max_placement_attempts": 1000000,
                "max_search_nodes": 1000000
            }
        }),
    )?;

    // A budget small enough to bind. The search stops early and says so -- `effort_limit`,
    // not `complete` -- and it still stops at exactly the same point on every run and every
    // machine, so the shorter answer is just as reproducible.
    twice(
        "a budget that binds",
        json!({
            "solver_profile": "balanced",
            "time_limit_ms": 60000,
            "effort_budget": { "max_search_nodes": 12 }
        }),
    )?;

    // What not to do is not printed here, because its output is not the same on every run:
    // a request with only `"time_limit_ms": 200` answers whatever the search reached in
    // 200 ms on this machine at this moment. When the clock does stop such a search, the
    // result says `time_limit`, and its artifact says the replay is `not_guaranteed`.
    Ok(())
}
