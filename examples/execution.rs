//! Execution plans: turn a packing result into instructions someone can follow on a dock.
//!
//!     cargo run --example execution
//!
//! `pack_json` answers where every box goes. That is not yet a work order: it does not say
//! what to lift first, and it does not separate what the solver *decided* from what a screen
//! should *say*. The execution plan is that second document. It is derived from a result that
//! was already validated -- it calls no solver, reads no clock, and the same result always
//! gives the same bytes, in this crate and in the Python, PHP and JavaScript packages.
//!
//! The step order is not invented by the plan. You compute a safe loading order from the
//! placed geometry with `safe_loading_order` and hand it in; without one, the plan lists
//! every placement unnumbered rather than pass off array order as a safe order to lift in.

use packvium_core::execution::build_plan_json;
use packvium_core::{Aabb, Dimensions, Length, Point, safe_loading_order};
use serde_json::{Value, json};

/// One crate, a printer that must stay upright, four toner cartridges, and a pallet jack
/// that was never going to fit. A plan has to say what is *not* going on the truck as
/// clearly as what is.
fn request() -> Value {
    json!({
        "units": { "length": "mm" },
        "configuration": {
            // Counted work bounds the search; the time limit is only a safety fuse. The
            // pallet jack fits no orientation of the crate, so the search sets it aside
            // before it starts and finishes well inside this budget.
            "time_limit_ms": 60000,
            "effort_budget": {
                "max_candidates_evaluated": 100000,
                "max_placement_attempts": 100000,
                "max_search_nodes": 100000
            }
        },
        "items": [
            { "id": "printer", "quantity": 1, "weight": "9 kg", "keep_upright": true,
              "dimensions": { "length": "420", "width": "340", "height": "260" } },
            { "id": "toner", "quantity": 4, "weight": "900 g",
              "dimensions": { "length": "180", "width": "120", "height": "100" } },
            { "id": "pallet-jack", "quantity": 1, "weight": "80 kg",
              "dimensions": { "length": "1200", "width": "550", "height": "1200" } }
        ],
        "containers": [
            { "id": "crate", "quantity": 1, "max_payload": "30 kg",
              "inner_dimensions": { "length": "600", "width": "400", "height": "400" } }
        ]
    })
}

fn ticks(measure: &Value) -> i64 {
    measure["ticks"].as_i64().unwrap_or_default()
}

fn dimensions(value: &Value) -> Dimensions {
    Dimensions {
        length: Length(ticks(&value["length"])),
        width: Length(ticks(&value["width"])),
        height: Length(ticks(&value["height"])),
    }
}

/// The placed boxes of one result container, exactly, in the order the result lists them.
fn boxes(container: &Value) -> Vec<Aabb> {
    container["placements"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|placement| {
            let at = &placement["position"];
            Aabb {
                origin: Point {
                    x: ticks(&at["x"]),
                    y: ticks(&at["y"]),
                    z: ticks(&at["z"]),
                },
                dimensions: dimensions(&placement["dimensions"]),
            }
        })
        .collect()
}

fn text(value: &Value) -> &str {
    value.as_str().unwrap_or_default()
}

fn section(title: &str) {
    println!("\n{}\n{title}\n{}", "=".repeat(78), "=".repeat(78));
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let result_text = packvium_core::pack_json(&request().to_string())?;
    let result: Value = serde_json::from_str(&result_text)?;

    section("1. What the solver decided, kept apart from what a screen says");
    let plan: Value = serde_json::from_str(&build_plan_json(&result_text, "{}")?)?;
    println!("  format:          {}", text(&plan["format"]));
    println!("  status:          {}", text(&plan["facts"]["status"]));
    println!("  containers used: {}", plan["facts"]["container_count"]);
    println!("  score:           {}", plan["facts"]["score"]);
    println!();
    println!("  Everything above is under `facts`: the solver's own answer, copied rather than");
    println!("  re-derived, so a system that reads only `facts` loses nothing it may rely on.");

    section("2. The step order is computed from geometry and handed in");
    let unordered = &plan["containers"][0];
    println!(
        "  without an order: `order` is {}",
        text(&unordered["order"])
    );
    for step in unordered["steps"].as_array().into_iter().flatten() {
        println!("    -  {}", text(&step["placement"]["item_type"]));
    }

    // The crate is loaded from the top, so the only way in is down along -z; the order is
    // the reverse of a safe removal upwards, `+z`. It is replayed before it is returned, so
    // no box is ever lowered through another or set down before what it rests on.
    let crate_ = &result["containers"][0];
    let order = safe_loading_order(
        &boxes(crate_),
        dimensions(&crate_["inner_dimensions"]),
        &["+z"],
    )?;
    let orders = json!({ "0": order }).to_string();
    let ordered: Value = serde_json::from_str(&build_plan_json(&result_text, &orders)?)?;
    let container = &ordered["containers"][0];
    println!();
    println!(
        "  with an order:    `order` is {}",
        text(&container["order"])
    );
    for step in container["steps"].as_array().into_iter().flatten() {
        let placement = &step["placement"];
        let at = &placement["position_ticks"];
        println!(
            "    {}. {:<8} {}  at ({}, {}, {}) ticks",
            step["sequence"],
            text(&placement["item_type"]),
            text(&placement["orientation"]),
            at["x"],
            at["y"],
            at["z"]
        );
    }

    section("3. Every sentence names the fields it was built from");
    for entry in plan["unplaced"].as_array().into_iter().flatten() {
        let facts = &entry["facts"];
        let presentation = &entry["presentation"];
        println!("  facts:        item_type={}", facts["item_type"]);
        println!(
            "                reason={} proof_level={}",
            facts["reason"], facts["proof_level"]
        );
        println!("  presentation: {}", text(&presentation["summary"]));
        println!("  cites:        {}", presentation["cites"]);
    }
    println!();
    println!("  `proven` is a claim about the search: no orientation of the pallet jack fits");
    println!("  any offered crate, so nothing needed to be tried. A sentence with no citation");
    println!("  would be one nobody can check, which is why `cites` is part of the format.");

    section("4. The same result gives the same bytes, in every language");
    let first = build_plan_json(&result_text, &orders)?;
    let second = build_plan_json(&result_text, &orders)?;
    println!(
        "  canonical form: {} bytes, built twice, identical: {}",
        first.len(),
        first == second
    );
    println!("    {}...", &first[..first.len().min(68)]);
    println!();
    println!("  That is a promise about the plan, not about the search: given the same result,");
    println!("  every engine's plan is byte-identical; given the same request, engines may");
    println!("  reach different valid packings, and each plan describes its own faithfully.");
    Ok(())
}
