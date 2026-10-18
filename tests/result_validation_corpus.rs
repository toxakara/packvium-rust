//!: the Rust validator agrees with the reference on every pair of the shared corpus.
//!
//! Mirrors `packvium-python/tests/test_result_validation_corpus.py` and PHP's
//! `ResultValidationCorpusTest`, over the same `conformance/result_validation/corpus.json`
//! (written by `scripts/generate_result_validation_corpus.py`): the subset of the
//! cross-language corpus that reaches every arc, issue code and refusal kind the full corpus
//! reaches in the reference, each pair with the reference's outcome. Agreement is
//! `conformance/result_validation/run.py`'s rule: the same issues, code and detail, in order;
//! or, where the reference raised `MalformedResult`, a `contract` refusal with the same message
//! (up to a quoted Python exception); or, where it failed another way, any refusal.

use std::path::PathBuf;

use packvium_core::validate_json_outcome;
use serde_json::Value;

/// Reference refusals that end by quoting a Python exception compare up to the quote:
/// run.py's `^(placement is missing tick coordinates: |.*: lattice_summary is missing ...: )`.
const MISSING_TICKS: &str = "placement is missing tick coordinates: ";
const MISSING_LATTICE_FIELD: &str = ": lattice_summary is missing a canonical field: ";

fn corpus_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../../../conformance/result_validation/corpus.json")
}

/// `None` outside the workspace, where the shared corpus does not exist.
fn corpus() -> Option<Vec<Value>> {
    let path = corpus_path();
    if !path.is_file() {
        // A harness without its corpus is a broken workspace, not a published crate.
        assert!(
            !path.parent().unwrap().is_dir(),
            "{} is missing",
            path.display()
        );
        return None;
    }
    let text = std::fs::read_to_string(path).expect("the corpus is readable");
    let decoded: Value = serde_json::from_str(&text).expect("the corpus is JSON");
    assert_eq!(decoded["format"], "packvium-result-validation-corpus/v1");
    Some(
        decoded["cases"]
            .as_array()
            .expect("cases is a list")
            .clone(),
    )
}

fn quoted_prefix(message: &str) -> Option<&str> {
    if message.starts_with(MISSING_TICKS) {
        return Some(MISSING_TICKS);
    }
    let at = message.rfind(MISSING_LATTICE_FIELD)?;
    Some(&message[..at + MISSING_LATTICE_FIELD.len()])
}

fn same_refusal(expected: &Value, actual: &Value) -> bool {
    if actual["kind"] != expected["kind"] {
        return false;
    }
    let (want, got) = (
        expected["message"].as_str().unwrap_or_default(),
        actual["message"].as_str().unwrap_or_default(),
    );
    match quoted_prefix(want) {
        Some(prefix) => got.starts_with(prefix),
        None => got == want,
    }
}

fn disagreement(expected: &Value, actual: &Value) -> Option<String> {
    if let Some(issues) = expected.get("issues") {
        return match actual.get("issues") {
            Some(found) if found == issues => None,
            Some(found) => Some(format!("a different issue list: {found}")),
            None => Some(format!(
                "refused ({}) where the reference found issues",
                actual["refusal"]
            )),
        };
    }
    let reference = &expected["refusal"];
    let Some(refusal) = actual.get("refusal") else {
        return Some(format!(
            "answered where the reference refused ({reference})"
        ));
    };
    if reference["kind"] == "contract" && !same_refusal(reference, refusal) {
        return Some(format!(
            "refused with {refusal} where the reference raised {reference}"
        ));
    }
    None
}

#[test]
fn every_pair_gets_the_reference_verdict() {
    let Some(cases) = corpus() else {
        return;
    };
    let disagreements: Vec<String> = cases
        .iter()
        .enumerate()
        .filter_map(|(index, case)| {
            let outcome: Value = serde_json::from_str(&validate_json_outcome(
                case["request"].as_str().unwrap(),
                case["result"].as_str().unwrap(),
            ))
            .unwrap();
            disagreement(&case["expected"], &outcome)
                .map(|reason| format!("case {index}: {reason}"))
        })
        .collect();
    assert!(disagreements.is_empty(), "{disagreements:#?}");
}
