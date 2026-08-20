//! Read a commerce case as JSON on stdin, write the canonical result on stdout.
//!
//! Mirrors the Python, PHP and JavaScript adapters so the cross-language commerce
//! conformance runner can drive all four implementations through one interface.
//!
//!     cargo run -q -p packvium-core --example commerce-stdin < case.json
//!
//! A case is `{"operation": ..., "document": ..., "request": ...}`. An input error
//! exits non-zero with the message on stderr; a rejection is a normal result document
//! and exits zero, because a rejection is an answer.

use std::io::{Read, Write};

use serde_json::{Value, json};

fn main() -> std::process::ExitCode {
    let mut input = String::new();
    if let Err(error) = std::io::stdin().read_to_string(&mut input) {
        eprintln!("commerce-stdin: cannot read stdin: {error}");
        return std::process::ExitCode::from(2);
    }
    let case: Value = match serde_json::from_str(&input) {
        Ok(value) => value,
        Err(error) => {
            eprintln!("commerce-stdin: invalid JSON: {error}");
            return std::process::ExitCode::from(2);
        }
    };
    let call =
        json!({"document": case.get("document"), "request": case.get("request")}).to_string();
    let outcome = match case.get("operation").and_then(Value::as_str) {
        Some("quote") => packvium_core::commerce::quote_json(&call),
        Some("evaluate_policy") => packvium_core::commerce::evaluate_policy_json(&call),
        Some("catalog_version_info") => packvium_core::commerce::catalog_version_info_json(&call),
        other => {
            eprintln!("commerce-stdin: unknown operation {other:?}");
            return std::process::ExitCode::from(2);
        }
    };
    match outcome {
        Ok(result) => {
            let mut stdout = std::io::stdout();
            if stdout.write_all(result.as_bytes()).is_err() {
                return std::process::ExitCode::from(2);
            }
            std::process::ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("commerce-stdin: input error: {error}");
            std::process::ExitCode::from(1)
        }
    }
}
