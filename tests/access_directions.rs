//! `container.access_directions` at its boundaries.
//!
//! The field was reserved at the 1.1.0 freeze and implemented in all four engines in one
//! change. What shipped beside the packing rule is a decode-and-canonicalise path per
//! engine, and coverage put `api.rs` at 82.98% with every refusal arm of
//! `parse_access_directions` unreached: the shared corpus exercises one well-formed door
//! list and nothing else.
//!
//! Canonicalisation is the half a fixture cannot assert. Every fixture states its doors
//! once, in one order, so a normalisation that quietly stopped working leaves all 399
//! green while making this engine order-sensitive — a determinism break, which is a
//! correctness failure here rather than a preference.
//!
//! Measured from outside through `pack_json`, like the other JSON-boundary suites, because
//! that is the path a request actually takes.

use serde_json::{Value, json};

fn request(doors: Option<Value>) -> String {
    let mut container = json!({
        "id": "van",
        "inner_dimensions": { "length": "200", "width": "100", "height": "100" }
    });
    if let Some(doors) = doors {
        container["access_directions"] = doors;
    }
    json!({
        "units": { "length": "mm" },
        "items": [{ "id": "cube", "quantity": 1,
                    "dimensions": { "length": "100", "width": "100", "height": "100" } }],
        "containers": [container]
    })
    .to_string()
}

/// Everything the contract promises to reproduce.
///
/// `algorithm.duration_ms` is wall clock and is the one field a determinism assertion must
/// not read: comparing whole documents passes or fails on how busy the machine is, which
/// reports the host rather than the engine.
fn without_wall_clock(packed: &str) -> Value {
    let mut result: Value = serde_json::from_str(packed).expect("the engine answers JSON");
    result["algorithm"]
        .as_object_mut()
        .expect("algorithm is an object")
        .remove("duration_ms");
    result
}

fn refusal(doors: Value) -> String {
    packvium_core::pack_json(&request(Some(doors)))
        .expect_err("a malformed door list must be refused")
        .to_string()
}

// ------------------------------------------------------------------- canonicalisation

#[test]
fn doors_are_deduplicated_into_the_canonical_order() {
    let one = packvium_core::pack_json(&request(Some(json!(["+z", "-x", "+z", "-x"]))))
        .expect("a well-formed door list is accepted");
    let other = packvium_core::pack_json(&request(Some(json!(["-x", "+z"]))))
        .expect("a well-formed door list is accepted");
    assert_eq!(without_wall_clock(&one), without_wall_clock(&other));
}

#[test]
fn a_request_naming_doors_in_either_order_gives_one_answer() {
    let one = packvium_core::pack_json(&request(Some(json!(["-x", "+z"]))))
        .expect("a well-formed door list is accepted");
    let other = packvium_core::pack_json(&request(Some(json!(["+z", "-x"]))))
        .expect("a well-formed door list is accepted");
    assert_eq!(without_wall_clock(&one), without_wall_clock(&other));
}

#[test]
fn every_legal_direction_is_accepted() {
    packvium_core::pack_json(&request(Some(json!(["-z", "+z", "-y", "+y", "-x", "+x"]))))
        .expect("all six walls are a legal, if unusual, container");
}

/// A container that names no doors is the pre-default: the rule is inert, not the
/// container sealed. `[]` is a caller saying "no doors stated" rather than a malformed
/// request, so it has to behave exactly like the absent field — otherwise the two
/// spellings of one default diverge.
#[test]
fn an_empty_door_list_is_accepted_and_matches_the_absent_field() {
    let stated =
        packvium_core::pack_json(&request(Some(json!([])))).expect("an empty list states no doors");
    let absent = packvium_core::pack_json(&request(None)).expect("the field is optional");
    assert_eq!(without_wall_clock(&stated), without_wall_clock(&absent));
}

// ------------------------------------------------------------------------- refusals

#[test]
fn a_door_list_that_is_not_an_array_is_refused() {
    assert!(refusal(json!("-x")).contains("must be an array"));
    assert!(refusal(json!({ "wall": "-x" })).contains("must be an array"));
}

#[test]
fn a_non_string_entry_is_refused() {
    assert!(refusal(json!([1])).contains("must be strings"));
    assert!(refusal(json!([null])).contains("must be strings"));
    assert!(refusal(json!([["-x"]])).contains("must be strings"));
}

/// Refused, not filtered out. Silently discarding an unrecognised door would leave a
/// container with fewer exits than the caller believes it has, and the packing would then
/// be legal for a vehicle that does not exist.
#[test]
fn an_unknown_direction_is_refused_rather_than_dropped() {
    for direction in ["north", "x", "+X", "+w", "", "-x "] {
        let message = refusal(json!([direction]));
        assert!(
            message.contains("unknown movement direction"),
            "{direction:?} produced {message}"
        );
    }
}

/// A partially honoured list is the worst outcome available: it validates and means
/// something the caller did not write.
#[test]
fn one_bad_direction_refuses_the_whole_list() {
    assert!(refusal(json!(["-x", "sideways", "+z"])).contains("unknown movement direction"));
}
