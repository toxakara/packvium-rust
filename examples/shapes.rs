//! Shapes: when an item is not its box.
//!
//!     cargo run --example shapes
//!
//! Every other example treats an item as the box it declares. That is the default and it
//! is right for almost everything, because a carton *is* a cuboid. Two kinds of goods are
//! not: a moulded or tapered part that leaves a usable void beside it, and a soft one that
//! gives way under whatever is stacked on it.
//!
//! `shape_type` narrows the box in one direction each -- `convex_hull` in space,
//! `compressible` in height under load -- and neither is ever inferred. An engine that
//! quietly packed a hull as its bounding box would return a plan that validates and does
//! not physically fit, so the value has to be asked for.
//!
//! These fields belong to the shared request contract, so the same JSON runs unchanged
//! against the Python, PHP and JavaScript engines.

use serde_json::{Value, json};

/// Pack one request and print only what the shape changed.
fn summarise(label: &str, request: Value) {
    let packed = packvium_core::pack_json(&request.to_string()).expect("well-formed request");
    let result: Value = serde_json::from_str(&packed).expect("the engine answers JSON");
    let containers = result["containers"].as_array().map_or(0, Vec::len);
    let placed: usize = result["containers"]
        .as_array()
        .map(|list| {
            list.iter()
                .map(|c| c["placements"].as_array().map_or(0, Vec::len))
                .sum()
        })
        .unwrap_or(0);
    let refused = result["unpacked_items"].as_array().map_or(0, Vec::len);
    let unused = result["score"][3].as_i64().unwrap_or(-1);
    println!(
        "  {label:<22} {containers} container(s), {placed} placed, {refused} refused, \
         unused volume {unused} ppm"
    );
}

fn crate_of(length: &str, width: &str, height: &str) -> Value {
    json!([{ "id": "crate",
             "inner_dimensions": { "length": length, "width": width, "height": height } }])
}

/// A 100 mm cube, optionally carrying the hull that says how much of it is solid.
fn wedge(id: &str, vertices: Option<Value>) -> Value {
    let mut item = json!({
        "id": id, "quantity": 1,
        "dimensions": { "length": "100", "width": "100", "height": "100" },
        "weight": { "value": "1", "unit": "kg" }
    });
    if let Some(hull) = vertices {
        item["shape_type"] = json!("convex_hull");
        item["hull_vertices"] = hull;
    }
    item
}

fn main() {
    // ------------------------------------------------------------------ convex_hull
    //
    // Two triangular prisms cut from the same cube along its diagonal. Their bounding
    // boxes are identical and each fills the crate on its own, so as cuboids the second
    // has nowhere to go. As hulls they are complementary halves and share the crate
    // exactly: collisions are decided by an exact integer separating-axis test on the
    // vertices, not by a box overlap.
    //
    // The hull is given in the item's own coordinates, in the request's length unit, and
    // must fit inside the declared dimensions. It does not replace them -- the box still
    // bounds the item, the hull only says how much of that box is solid.
    let lower = json!([
        {"x": "0", "y": "0", "z": "0"}, {"x": "100", "y": "0", "z": "0"},
        {"x": "0", "y": "100", "z": "0"}, {"x": "0", "y": "0", "z": "100"},
        {"x": "100", "y": "0", "z": "100"}, {"x": "0", "y": "100", "z": "100"}
    ]);
    let upper = json!([
        {"x": "100", "y": "100", "z": "0"}, {"x": "100", "y": "0", "z": "0"},
        {"x": "0", "y": "100", "z": "0"}, {"x": "100", "y": "100", "z": "100"},
        {"x": "100", "y": "0", "z": "100"}, {"x": "0", "y": "100", "z": "100"}
    ]);

    println!("convex_hull -- two complementary wedges cut from one cube");
    summarise(
        "as cuboids",
        json!({ "units": { "length": "mm" },
                "items": [wedge("wedge-lower", None), wedge("wedge-upper", None)],
                "containers": crate_of("100", "100", "100") }),
    );
    summarise(
        "as hulls",
        json!({ "units": { "length": "mm" },
                "items": [wedge("wedge-lower", Some(lower)), wedge("wedge-upper", Some(upper))],
                "containers": crate_of("100", "100", "100") }),
    );

    // One crate instead of two, for the same goods and the same crate. Nothing changed
    // except the claim that the items are wedges rather than blocks.

    // ----------------------------------------------------------------- compressible
    //
    // `compression_ratio` is the fraction of its own height an item may lose under load,
    // and `max_compression_pressure_kpa` is where yielding becomes crushing and the load
    // is refused instead. The mass above decides how much it actually gives, so the
    // occupied height of a compressible item is not a property of the item alone.
    //
    // `must_be_on_floor` is not decoration here: without it the solver may put the brick
    // underneath, nothing bears on the cushion, and the feature never engages.
    let cushion = json!({
        "id": "cushion", "quantity": 1,
        "dimensions": { "length": "100", "width": "100", "height": "100" },
        "weight": { "value": "2", "unit": "kg" },
        "must_be_on_floor": true,
        "shape_type": "compressible",
        "compression_ratio": 0.25,
        "max_compression_pressure_kpa": 100
    });
    let brick = |kilograms: u32| {
        json!({ "id": "brick", "quantity": 1,
                "dimensions": { "length": "100", "width": "100", "height": "100" },
                "weight": { "value": kilograms.to_string(), "unit": "kg" } })
    };

    // The crate is 100x100x200 and both items are 100 mm cubes, so rigidly they fill it
    // exactly and nothing is unused. Under 101 kg the cushion gives up part of its
    // quarter and the volume it stops occupying shows up as unused. One more kilogram
    // crosses 100 kPa over its 0.01 m^2 face: the stack is refused and the brick opens a
    // second crate.
    println!("\ncompressible -- a cushion that yields to the load above it");
    for kilograms in [101, 102] {
        summarise(
            &format!("brick {kilograms} kg"),
            json!({ "units": { "length": "mm" },
                    "items": [cushion.clone(), brick(kilograms)],
                    "containers": crate_of("100", "100", "200") }),
        );
    }

    // Both shapes are refused rather than approximated wherever the engine cannot honour
    // them exactly -- a hull on a route, a hull under a configured clearance, a
    // compressible item with `nesting_height`. A wrong answer that validates is worse
    // than a refusal that does not, which is why these are opt-in.
}
