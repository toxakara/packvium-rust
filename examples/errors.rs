//! Errors: what you get back when the engine cannot answer, and how to branch on it.
//!
//!     cargo run --example errors
//!
//! There are two kinds of failure, and only one of them is an `Err`.
//!
//! - **The request is wrong.** A value is missing, mistyped, below its minimum, in an unknown
//!   unit, or the fixed placements it names cannot hold. Nothing was solved, and retrying the
//!   same request gives the same error. That is `PackError::InvalidRequest(RequestError)`,
//!   which names the rule it broke (`reason`) and where (`field`, a JSON Pointer into the
//!   request you sent).
//! - **The request is fine, but not everything fits.** That is not an error: the result says
//!   so, with a reason for every item left out.
//!
//! Every `PackError` also has `code()`, one closed string per kind of refusal. Branch on
//! `code`, `reason` and `field`; show the message to a person. The message is the same text
//! the Python, PHP and JavaScript engines produce for the same request.

use packvium_core::PackError;
use serde_json::{Value, json};

/// A small valid request: one carton, one kind of item. Each case below breaks one thing.
fn request() -> Value {
    json!({
        "units": { "length": "mm" },
        "configuration": {
            "time_limit_ms": 60000,
            "effort_budget": {
                "max_candidates_evaluated": 1000000,
                "max_placement_attempts": 1000000,
                "max_search_nodes": 1000000
            }
        },
        "items": [{ "id": "jar", "quantity": 2, "weight": "400 g",
                    "dimensions": { "length": "80", "width": "80", "height": "120" } }],
        "containers": [{ "id": "carton", "max_payload": "10 kg",
                         "inner_dimensions": { "length": "300", "width": "200", "height": "150" } }]
    })
}

/// Set the value at a JSON Pointer, adding the last key when it is not there yet, so each
/// case reads as the one edit it makes.
fn with(pointer: &str, value: Value) -> Value {
    let mut request = request();
    let (parent, key) = pointer.rsplit_once('/').unwrap_or(("", pointer));
    match request.pointer_mut(parent) {
        Some(Value::Object(map)) => {
            map.insert(key.to_owned(), value);
        }
        Some(Value::Array(list)) => {
            if let Some(slot) = key
                .parse::<usize>()
                .ok()
                .and_then(|index| list.get_mut(index))
            {
                *slot = value;
            }
        }
        _ => {}
    }
    request
}

/// What a caller does with each kind of answer.
fn describe(outcome: Result<String, PackError>) {
    match outcome {
        Ok(text) => {
            let result: Value = serde_json::from_str(&text).unwrap_or_default();
            let left_out = result["unpacked_items"]
                .as_array()
                .map_or(&[][..], Vec::as_slice);
            println!(
                "    Ok: status {}, {} item(s) left out",
                result["status"],
                left_out.len()
            );
            for unpacked in left_out {
                println!("      {} -- {}", unpacked["item_id"], unpacked["reason"]);
            }
        }
        // The one to branch on in code: highlight `field` in a form, or map `reason` to
        // your own message. `detail` is the fixed text the message ends with.
        Err(PackError::InvalidRequest(error)) => {
            println!("    code:   {}", error.code());
            println!("    reason: {}", error.reason());
            println!("    field:  {:?}", error.field());
            println!("    detail: {}", error.detail());
            println!("    shown:  {error}");
        }
        // Everything else still has a closed code. `PackError` is `#[non_exhaustive]`, so a
        // wildcard arm is required, and `code()` is how that arm stays specific.
        Err(other) => {
            println!("    code:   {}", other.code());
            println!("    shown:  {other}");
        }
    }
}

fn case(title: &str, request: &Value) {
    println!("\n{title}");
    describe(packvium_core::pack_json(&request.to_string()));
}

fn main() {
    println!("== The request is wrong: nothing was solved ==");
    case(
        "a negative measure",
        &with("/items/0/dimensions/width", json!("-80")),
    );
    case(
        "a quantity below its minimum",
        &with("/items/0/quantity", json!(0)),
    );
    case(
        "a mistyped value: a quantity sent as text",
        &with("/items/0/quantity", json!("2")),
    );
    case(
        "a unit nobody uses",
        &with("/units/length", json!("furlong")),
    );
    case(
        "a value outside its closed set",
        &with("/configuration/solver_profile", json!("thorough")),
    );
    case("a missing required field", &{
        let mut request = request();
        if let Some(dimensions) = request.pointer_mut("/items/0/dimensions") {
            dimensions.as_object_mut().map(|map| map.remove("height"));
        }
        request
    });

    // Items already in place are checked before any search: two jars fixed at the same
    // spot cannot both be there. The code is `invalid_fixed_placement`, and `field` points
    // at the whole list because the conflict is between entries, not inside one.
    let mut fixed = request();
    fixed["fixed_placements"] = json!([
        { "item_type": "jar", "container_type": "carton", "container_instance": 1,
          "position": { "x": "0", "y": "0", "z": "0" }, "orientation": "LWH" },
        { "item_type": "jar", "container_type": "carton", "container_instance": 1,
          "position": { "x": "0", "y": "0", "z": "0" }, "orientation": "LWH" }
    ]);
    case("two fixed placements in the same spot", &fixed);

    println!("\n== Other refusals: each has its own code ==");
    case(
        "a field this engine deliberately does not implement",
        &with("/containers/0/pallet_overhang_limit", json!("10")),
    );
    case("JSON, but a string instead of an object", &json!("items"));
    println!("\nthe bytes themselves are not JSON");
    describe(packvium_core::pack_json("{\"items\": ["));

    println!("\n== Not an error: the request is fine, the jar is too tall ==");
    case(
        "a jar taller than the carton in every orientation",
        &with("/items/0/dimensions/height", json!("400")),
    );
}
