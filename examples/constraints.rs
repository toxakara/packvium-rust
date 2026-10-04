//! Constraints: how to say "this may not go there", and get told why.
//!
//!     cargo run --example constraints
//!
//! Most real packing rules are refusals -- this side up, nothing on top of that, keep the
//! bleach away from the flour -- and the useful part of an answer is often the item that did
//! *not* fit and the reason it did not. Every rule below is a field on an item or a
//! container in the request. None needs code of your own, and none changes how you call
//! `pack_json`.
//!
//! Each rule is shown twice: the same goods in the same container, once without the rule and
//! once with it. A constraint you cannot watch change the answer is one you have to take on
//! faith. Often the rule does not refuse anything -- the solver satisfies it by opening
//! another container, which costs money and is the answer you wanted to see coming.

use serde_json::{Value, json};

/// Counted work bounds the search, so the answer is the same on every host; the time limit
/// is only a safety fuse.
fn configuration() -> Value {
    json!({
        "time_limit_ms": 60000,
        "effort_budget": {
            "max_candidates_evaluated": 1000000,
            "max_placement_attempts": 1000000,
            "max_search_nodes": 1000000
        }
    })
}

fn pack(items: Value, containers: Value) -> Result<Value, Box<dyn std::error::Error>> {
    let request = json!({
        "units": { "length": "mm" },
        "configuration": configuration(),
        "items": items,
        "containers": containers,
    });
    Ok(serde_json::from_str(&packvium_core::pack_json(
        &request.to_string(),
    )?)?)
}

fn list(value: &Value) -> &[Value] {
    value.as_array().map_or(&[], Vec::as_slice)
}

fn text(value: &Value) -> &str {
    value.as_str().unwrap_or_default()
}

/// The reason code as a sentence, or the bare code for one this version cannot phrase.
fn explain(reason: &str) -> String {
    packvium_core::explain_reason(reason).map_or_else(|_| reason.to_owned(), str::to_owned)
}

fn print_refusals(result: &Value) {
    for unpacked in list(&result["unpacked_items"]) {
        println!(
            "      {} -- {}: {}",
            text(&unpacked["item_id"]),
            text(&unpacked["reason"]),
            explain(text(&unpacked["reason"]))
        );
    }
}

/// Pack the same goods with and without one rule, and print what the rule changed.
fn compare(
    rule: &str,
    without: Value,
    with_rule: Value,
    containers: &Value,
) -> Result<(), Box<dyn std::error::Error>> {
    println!("\n{rule}");
    for (label, items) in [
        ("without the rule", without),
        ("with the rule   ", with_rule),
    ] {
        let result = pack(items, containers.clone())?;
        let containers_used = list(&result["containers"]);
        let placed: usize = containers_used
            .iter()
            .map(|container| list(&container["placements"]).len())
            .sum();
        println!(
            "  {label}: {} container(s), {placed} placed, {} refused",
            containers_used.len(),
            list(&result["unpacked_items"]).len()
        );
        print_refusals(&result);
    }
    Ok(())
}

fn item(id: &str, length: &str, width: &str, height: &str, weight: &str) -> Value {
    json!({ "id": id, "quantity": 1, "weight": weight,
            "dimensions": { "length": length, "width": width, "height": height } })
}

/// `item` with extra fields merged in: the rule under test, or a quantity.
fn with(mut base: Value, rule: Value) -> Value {
    if let (Some(target), Some(extra)) = (base.as_object_mut(), rule.as_object()) {
        target.extend(extra.clone());
    }
    base
}

fn container(id: &str, length: &str, width: &str, height: &str) -> Value {
    json!([{ "id": id, "max_payload": "40 kg",
             "inner_dimensions": { "length": length, "width": width, "height": height } }])
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // ------------------------------------------------------------ rotations
    //
    // `allowed_rotations` narrows the six orientations to the ones you permit; `LWH` and
    // `WLH` are the two that keep the item's own height vertical. `keep_upright` says the
    // same thing in one word, for anything with an open top or a "this side up" arrow. The
    // pole is 700 mm tall and the shelf 500 mm high, so it fits only lying down.
    let shelf = container("shelf", "800", "400", "500");
    let pole = item("pole", "90", "90", "700", "1 kg");
    compare(
        "allowed_rotations -- a pole that only fits lying down, forbidden from lying down",
        json!([pole.clone()]),
        json!([with(
            pole.clone(),
            json!({ "allowed_rotations": ["LWH", "WLH"] })
        )]),
        &shelf,
    )?;
    compare(
        "keep_upright -- the same rule, spelled for a paint tin rather than a pole",
        json!([pole.clone()]),
        json!([with(pole, json!({ "keep_upright": true }))]),
        &shelf,
    )?;

    // ------------------------------------------------------------ stacking
    //
    // `max_stacked_items` caps how many units may stand on one item -- a pallet pattern's
    // "three high, no more", not a weight limit. The column is one tin wide, so height is
    // the only way to fit more, and a second column is the price of the cap.
    let column = container("column", "160", "160", "600");
    let tins = with(
        item("tin", "150", "150", "120", "800 g"),
        json!({ "quantity": 5 }),
    );
    compare(
        "max_stacked_items -- five tins fit one column; three high needs two",
        json!([tins.clone()]),
        json!([with(tins, json!({ "max_stacked_items": 2 }))]),
        &column,
    )?;

    // `stackable: false` means nothing may rest on the item at all; `max_top_load` caps
    // the weight resting *directly* on it. A flat-packed glass tabletop fills the floor of
    // a tray, so the only place left for the 5 kg parcel is on top of it -- until the
    // tabletop says nothing may stand on it, or that 2 kg is its limit. (`keep_upright`
    // stops the solver from simply standing the tabletop on its edge.)
    let tray = container("tray", "400", "300", "300");
    let panel = with(
        item("tabletop", "400", "300", "40", "8 kg"),
        json!({ "must_be_on_floor": true, "keep_upright": true }),
    );
    let parcel = item("parcel", "400", "300", "200", "5 kg");
    compare(
        "stackable -- a tabletop that nothing may be put on",
        json!([panel.clone(), parcel.clone()]),
        json!([
            with(panel.clone(), json!({ "stackable": false })),
            parcel.clone()
        ]),
        &tray,
    )?;
    compare(
        "max_top_load -- a tabletop that carries 2 kg and no more",
        json!([panel.clone(), parcel.clone()]),
        json!([with(panel, json!({ "max_top_load": "2 kg" })), parcel]),
        &tray,
    )?;

    // ------------------------------------------------------------ support
    //
    // `minimum_support_ratio` is how much of an item's base must rest on something solid.
    // The plinth stands on the floor under a quarter of the ledge, and the ledge is too low
    // for the slab to stand on edge -- so the only place the slab fits is perched on the
    // plinth, on a quarter of its base. At 0.9 that is refused and a second ledge opens.
    let ledge = container("ledge", "400", "400", "350");
    let plinth = with(
        item("plinth", "200", "200", "300", "5 kg"),
        json!({ "must_be_on_floor": true }),
    );
    let slab = item("slab", "400", "400", "60", "9 kg");
    compare(
        "minimum_support_ratio -- a slab perched on a quarter of its base",
        json!([plinth.clone(), slab.clone()]),
        json!([plinth, with(slab, json!({ "minimum_support_ratio": 0.9 }))]),
        &ledge,
    )?;

    // ------------------------------------------------------------ separation
    //
    // Tags are how two items refuse each other. `incompatible_tags` is checked both ways, so
    // tagging one side is enough; the bleach opens a crate of its own rather than travel
    // with the flour.
    let crate_ = container("crate", "600", "500", "500");
    let bleach = with(
        item("bleach", "120", "120", "300", "2 kg"),
        json!({ "tags": ["hazmat"] }),
    );
    let flour = with(
        item("flour", "200", "150", "100", "1500 g"),
        json!({ "tags": ["food"] }),
    );
    compare(
        "incompatible_tags -- bleach that may not share a crate with food",
        json!([bleach.clone(), flour.clone()]),
        json!([
            with(bleach, json!({ "incompatible_tags": ["food"] })),
            flour
        ]),
        &crate_,
    )?;

    // Every reason printed above is a fact about the request, not a solver failure --
    // which is why it can be turned into a sentence a customer is allowed to read.
    Ok(())
}
