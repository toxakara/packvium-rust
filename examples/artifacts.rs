//! Hand a packing result to a warehouse system that has no engine.
//!
//! Run it:
//!
//!     cargo run -p packvium-core --example artifacts
//!
//! An execution plan says what to lift first. A warehouse or transport system needs more than
//! that before it can act on its own: how big each box is, a sheet to print for the dock, and a
//! record of which request and solver produced it.
//!
//! The operational artifact is that one document. It wraps the plan unchanged and adds
//! geometry, display values and provenance, and it exports to JSON, CSV and a printable HTML
//! work order without calling a solver, a renderer or a clock. Like the rest of this crate it
//! is JSON text in, JSON text out, and Python, PHP and JavaScript build the same bytes from the
//! same request and result.

use packvium_core::artifact_exports::{export_csv, export_json, export_work_order_html};
use packvium_core::artifacts::build_artifact_json;
use serde_json::Value;

const REQUEST: &str = r#"{
  "units": {"length": "mm"},
  "configuration": {"time_limit_ms": 60000},
  "items": [
    {"id": "printer", "quantity": 1, "weight": "12 kg",
     "dimensions": {"length": "420", "width": "300", "height": "250"},
     "metadata": {"sales_order": "SO-1042"}},
    {"id": "toner", "quantity": 3, "weight": "1.5 kg",
     "dimensions": {"length": "300", "width": "100", "height": "100"}}
  ],
  "containers": [
    {"id": "crate", "quantity": 1,
     "inner_dimensions": {"length": "800", "width": "400", "height": "400"}}
  ]
}"#;

fn main() {
    let result = packvium_core::pack_json(REQUEST).unwrap_or_else(|error| fail(&error.to_string()));
    // No loading order is passed, so the plan lists every placement unnumbered. The sequence
    // API supplies a safe order; the artifact never invents one.
    let artifact =
        build_artifact_json(REQUEST, &result, "{}").unwrap_or_else(|error| fail(error.message()));
    let document: Value =
        serde_json::from_str(&artifact).unwrap_or_else(|error| fail(&error.to_string()));
    let container = &document["plan"]["containers"][0];

    section("1. One document that carries everything a consumer needs");
    println!("  format:          {}", text(&document["format"]));
    println!(
        "  plan steps:      {}",
        container["steps"].as_array().map_or(0, Vec::len)
    );
    println!("  order:           {}", text(&container["order"]));
    println!(
        "  sales order:     {}",
        text(&document["provenance"]["request"]["items"][0]["metadata"]["sales_order"])
    );
    println!();
    println!(
        "  The request is inside the artifact, not a hash of it: only the request itself lets"
    );
    println!("  someone replay the artifact without a lookup.");

    section("2. A CSV a warehouse system can import");
    let csv = export_csv(&artifact).unwrap_or_else(|error| fail(error.message()));
    for row in csv.split("\r\n").take(3) {
        println!("  {row}");
    }

    section("3. A work order to print");
    let html = export_work_order_html(&artifact).unwrap_or_else(|error| fail(error.message()));
    let external = html.contains("<script") || html.contains("http");
    println!(
        "  {} bytes of HTML, one section per container, one checkbox per step",
        html.len()
    );
    println!(
        "  contains a script or an external resource: {}",
        if external { "yes" } else { "no" }
    );

    section("4. The same bytes in every engine, and a refusal instead of a guess");
    let canonical = export_json(&artifact).unwrap_or_else(|error| fail(error.message()));
    println!(
        "  canonical JSON: {} bytes, starting {}...",
        canonical.len(),
        &canonical[..40]
    );
    match export_csv(r#"{"format":"packvium-operational-artifact/v2"}"#) {
        Err(error) => println!("  a v2 document: refused with {}", error.code()),
        Ok(_) => fail("a v2 document was exported instead of refused"),
    }
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
