//! How a result is written out, for the statuses and shapes a default solve does not
//! produce: the closed-form lattice centre of mass, every status spelling and its derived
//! facts, a score past the `i64` range, and a hand-built result with no solver name.

mod support;

use packvium_core::{
    AlgorithmReport, PackedContainer, PackingResult, PackingStatus, StartRecord,
    aggregate_termination, pack_json,
};
use serde_json::{Value, json};
use support::{container, item, placed};

fn solve(coordinates: bool) -> Value {
    let request = json!({
        "items": [{ "id": "brick", "quantity": 7, "weight": "250",
                    "dimensions": { "length": "100", "width": "100", "height": "100" } }],
        "containers": [{ "id": "crate",
                         "inner_dimensions": { "length": "300", "width": "300", "height": "300" } }],
        "configuration": {
            "solver_profile": "fast",
            "require_placement_coordinates": coordinates,
            "time_limit_ms": 60000,
            "effort_budget": { "max_candidates_evaluated": 100000,
                               "max_placement_attempts": 100000,
                               "max_search_nodes": 100000 }
        }
    });
    serde_json::from_str(&pack_json(&request.to_string()).expect("the request packs"))
        .expect("result JSON")
}

#[test]
fn a_weighted_lattice_reports_the_same_centre_of_mass_as_its_placements() {
    let compact = solve(false);
    let full = solve(true);
    assert!(
        compact["containers"][0].get("lattice_summary").is_some(),
        "{compact}"
    );
    let offset = &compact["containers"][0]["centre_of_mass_offset_ppm"];
    assert!(offset.as_i64().is_some_and(|ppm| ppm > 0), "{compact}");
    assert_eq!(offset, &full["containers"][0]["centre_of_mass_offset_ppm"]);
}

fn hand_built(status: PackingStatus) -> PackingResult {
    let brick = item("brick");
    PackingResult {
        status,
        containers: vec![PackedContainer {
            container: container(),
            sequence: 1,
            placements: vec![placed(&brick, 1, 0, 0)],
            lattice_summary: None,
            lattice_items: Vec::new(),
        }],
        unpacked: Vec::new(),
        algorithm: AlgorithmReport::default(),
        score: vec![i128::MIN, i128::MAX, 7],
        warnings: Vec::new(),
        alternatives: Vec::new(),
        feasibility: None,
        termination: None,
        optimality: None,
        objective: "default".into(),
        catalog_versions_used: Vec::new(),
    }
}

#[test]
fn every_status_is_written_with_the_facts_it_implies() {
    for (status, spelling, feasibility, termination, optimality) in [
        (
            PackingStatus::Optimal,
            "optimal",
            "feasible",
            "complete",
            "proven_optimal",
        ),
        (
            PackingStatus::Infeasible,
            "infeasible",
            "infeasible",
            "complete",
            "proven_infeasible",
        ),
        (
            PackingStatus::InvalidResult,
            "invalid_result",
            "unknown",
            "error",
            "not_proven",
        ),
    ] {
        let written = hand_built(status).to_json("mm", "g", false);
        assert_eq!(written["status"], spelling);
        assert_eq!(written["feasibility"]["code"], feasibility, "{written}");
        assert_eq!(written["termination"]["code"], termination, "{written}");
        assert_eq!(written["optimality"]["code"], optimality, "{written}");
    }
}

#[test]
fn a_score_past_the_i64_range_saturates_and_a_nameless_solver_reads_unknown() {
    let written = hand_built(PackingStatus::Feasible).to_json("mm", "g", false);
    assert_eq!(written["score"], json!([i64::MIN, i64::MAX, 7]));
    assert_eq!(
        written["termination"]["starts"][0]["id"], "unknown",
        "{written}"
    );
}

#[test]
fn a_placement_box_is_its_physical_extent() {
    let brick = item("brick");
    let placement = placed(&brick, 1, 5, 0);
    let box_ = placement.box_();
    assert_eq!(box_.origin, placement.position);
    assert_eq!(box_.dimensions, placement.dimensions);
}

#[test]
fn an_errored_run_terminates_as_an_error() {
    let start = StartRecord {
        id: "grid".into(),
        started: true,
        completed: true,
        truncated: false,
        selected: true,
        global_deadline_reached: false,
    };
    assert_eq!(aggregate_termination(&[start], true).code, "error");
}
