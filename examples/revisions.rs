//! Replan a half-loaded job without losing what was already done, or the record of why.
//!
//! Run it:
//!
//!     cargo run -p packvium-core --example revisions
//!
//! A plan is approved, and the dock starts loading. Then something changes: a slab is missing
//! from the shelf, and a cube that is already in the tote must stay exactly where it is. The
//! next plan has to keep the cube in place and pack around it, and anyone auditing the job later
//! has to see what changed, in what order, and which approved plan each change was recorded
//! against.
//!
//! A plan revision is that record. It is append-only and hash-chained: each revision names its
//! parent and the artifact it replaced by SHA-256, and carries the request the events produce.
//! The replan is an ordinary solve of that request. Like the rest of this crate it is JSON text
//! in, JSON text out, and Python, PHP and JavaScript compute the same revision bytes from the
//! same inputs.

use packvium_core::artifacts::build_artifact_json;
use packvium_core::revisions::{
    derive_revision_json, root_revision_json, verify_revision_chain_json,
};
use serde_json::{Value, json};

// Counted work decides where the search stops, not a clock: a plan the clock stopped could not be
// replayed, and its revision would say `replay: not_guaranteed`. The time limit is only a fuse,
// far above what this needs even on a slow or emulated host.
const REQUEST: &str = r#"{
  "units": {"length": "mm"},
  "configuration": {"effort_budget": {"max_search_nodes": 20000}, "time_limit_ms": 60000},
  "items": [
    {"id": "cube", "quantity": 4, "weight": "1 kg",
     "dimensions": {"length": "100", "width": "100", "height": "100"}},
    {"id": "slab", "quantity": 2, "weight": "2 kg",
     "dimensions": {"length": "200", "width": "100", "height": "50"}}
  ],
  "containers": [
    {"id": "tote", "quantity": 2,
     "inner_dimensions": {"length": "200", "width": "200", "height": "150"}}
  ]
}"#;

fn main() {
    section("1. The approved plan, and the revision that records it");
    let root = root_revision_json(REQUEST).unwrap_or_else(|error| fail(error.message()));
    let root_value = parse(&root);
    let approved = approve(&root_value["request"]);
    let first_step = &parse(&approved)["plan"]["containers"][0]["steps"][0]["placement"];
    println!(
        "  revision {}, parent {}",
        root_value["revision"], root_value["parent"]
    );
    println!(
        "  first step: a {} at x={} in container 0",
        text(&first_step["item_type"]),
        first_step["position_ticks"]["x"]
    );
    println!();
    println!("  The root records nothing against the request. It exists so the first change has a");
    println!("  parent to name.");

    section("2. What happened on the dock, recorded against that plan");
    let first_result = pack(&root_value["request"]);
    let loaded = &first_result["containers"][0]["placements"][0];
    let position = |axis: &str| loaded["position"][axis]["value"].clone();
    let events = json!([
        {"sequence": 1, "type": "placement_locked", "placement": {
            "item_type": "cube", "container_type": "tote", "container_instance": 1,
            "position": {"x": position("x"), "y": position("y"), "z": position("z")},
            "orientation": loaded["orientation"]}},
        {"sequence": 2, "type": "item_missing", "item_type": "slab", "quantity": 1}
    ]);
    let revision = derive_revision_json(&root, &approved, &events.to_string())
        .unwrap_or_else(|error| fail(error.message()));
    let revision_value = parse(&revision);
    println!(
        "  revision {}, parent {}...",
        revision_value["revision"],
        &text(&revision_value["parent"])[..23]
    );
    println!(
        "  approved artifact {}..., replay {}",
        &text(&revision_value["approved"]["artifact"])[..23],
        text(&revision_value["approved"]["replay"]["level"])
    );
    println!(
        "  slabs still to pack: {}",
        revision_value["request"]["items"][1]["quantity"]
    );
    println!(
        "  fixed placements:    {}",
        revision_value["request"]["fixed_placements"]
            .as_array()
            .map_or(0, Vec::len)
    );
    println!();
    println!("  The events are applied to the request, not to the result: one slab fewer, and the");
    println!("  cube becomes a fixed placement. The revision names its parent and the artifact it");
    println!("  replaces by SHA-256 over their RFC 8785 bytes, so every engine computes the same");
    println!("  digest.");

    section("3. The replan keeps the cube where it is");
    let replanned = pack(&revision_value["request"]);
    let mut placed = 0;
    for container in replanned["containers"].as_array().into_iter().flatten() {
        for placement in container["placements"].as_array().into_iter().flatten() {
            placed += 1;
            if placement["fixed"] == true {
                println!(
                    "  fixed in the answer: {} in {}",
                    text(&placement["item_id"]),
                    text(&container["id"])
                );
            }
        }
    }
    println!("  items placed:        {placed}");
    println!();
    println!("  An ordinary solve of the derived request, with nothing remembered from the first");
    println!("  one. The fixed cube is marked `fixed: true`, and the validator refuses any answer");
    println!("  that moves it.");

    section("4. An audit that notices tampering, and a refusal instead of a guess");
    let artifacts = json!([null, parse(&approved)]).to_string();
    let intact = format!("[{root},{revision}]");
    println!(
        "  intact chain: {}",
        codes(&verify_revision_chain_json(&intact, Some(&artifacts)))
    );
    let mut edited = revision_value.clone();
    edited["request"]["items"][1]["quantity"] = json!(2);
    let edited_chain = json!([root_value, edited]).to_string();
    println!(
        "  edited chain: {}",
        codes(&verify_revision_chain_json(&edited_chain, None))
    );
    let pallet =
        json!([{"sequence": 3, "type": "item_missing", "item_type": "pallet", "quantity": 1}]);
    let next_approved = approve(&revision_value["request"]);
    if let Err(error) = derive_revision_json(&revision, &next_approved, &pallet.to_string()) {
        println!(
            "  a pallet that was never requested: refused with {}",
            error.code()
        );
    }
    println!();
    println!("  An edited request no longer equals what its parent's request and its own events");
    println!("  produce, and any later revision would stop naming it as a parent. An event that");
    println!("  contradicts the request is refused by name rather than applied as a best guess.");
}

/// Solve a request and wrap the answer as the artifact the dock works from.
fn approve(request: &Value) -> String {
    let request = request.to_string();
    let result =
        packvium_core::pack_json(&request).unwrap_or_else(|error| fail(&error.to_string()));
    build_artifact_json(&request, &result, "{}").unwrap_or_else(|error| fail(error.message()))
}

fn pack(request: &Value) -> Value {
    parse(
        &packvium_core::pack_json(&request.to_string())
            .unwrap_or_else(|error| fail(&error.to_string())),
    )
}

fn codes<E>(issues: &Result<String, E>) -> String {
    let Ok(issues) = issues else {
        return "refused".to_owned();
    };
    let listed: Vec<String> = parse(issues)
        .as_array()
        .into_iter()
        .flatten()
        .map(|issue| text(&issue["code"]).to_owned())
        .collect();
    if listed.is_empty() {
        "no issues".to_owned()
    } else {
        listed.join(", ")
    }
}

fn parse(text: &str) -> Value {
    serde_json::from_str(text).unwrap_or_else(|error| fail(&error.to_string()))
}

fn section(title: &str) {
    let rule = "=".repeat(78);
    println!();
    println!("{rule}");
    println!("{title}");
    println!("{rule}");
}

fn text(value: &Value) -> &str {
    value.as_str().unwrap_or_default()
}

fn fail(message: &str) -> ! {
    eprintln!("{message}");
    std::process::exit(1);
}
