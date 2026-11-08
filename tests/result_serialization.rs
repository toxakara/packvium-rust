//! How a result is written out, for the statuses and shapes a default solve does not
//! produce: the closed-form lattice centre of mass, every status spelling and its derived
//! facts, a score past the `i64` range, a hand-built result with no solver name, and
//! container ids numbered per type by the lattice and block solvers.

mod support;

use packvium_core::{
    AlgorithmReport, ItemInstance, PackedContainer, PackingResult, PackingStatus, ResultFact,
    StartRecord, UnpackedItem, aggregate_termination, pack_json,
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
        currency: None,
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

/// A result with two containers, a fixed placement, unpacked items and two alternatives,
/// the first carrying alternatives of its own that the written form must leave out.
fn with_alternatives() -> PackingResult {
    let brick = item("brick");
    let mut fixed = placed(&brick, 3, 0, 0);
    fixed.fixed = true;
    let mut result = hand_built(PackingStatus::BestFound);
    result.containers.push(PackedContainer {
        container: container(),
        sequence: 2,
        placements: vec![fixed, placed(&brick, 4, 0, 10)],
        lattice_summary: None,
        lattice_items: Vec::new(),
    });
    result.unpacked = vec![UnpackedItem::new(
        ItemInstance {
            item: brick.clone(),
            sequence: 9,
        },
        "search_exhausted".into(),
        vec!["detail".into()],
    )];
    result.warnings = vec!["a warning".into()];
    let mut first = hand_built(PackingStatus::Feasible);
    first.alternatives = vec![hand_built(PackingStatus::TimeLimit)];
    result.alternatives = vec![first, hand_built(PackingStatus::Infeasible)];
    result
}

#[test]
fn the_written_text_is_the_tree_written_out_byte_for_byte() {
    for result in [hand_built(PackingStatus::Feasible), with_alternatives()] {
        for include_alternatives in [false, true] {
            for (length, weight) in [("mm", "g"), ("in", "lb")] {
                let tree = result.to_json(length, weight, include_alternatives);
                assert_eq!(
                    result
                        .to_json_string(length, weight, include_alternatives)
                        .expect("written"),
                    serde_json::to_string(&tree).expect("tree written"),
                );
            }
        }
    }
    let tree = with_alternatives().to_json("mm", "g", true);
    assert_eq!(tree["alternatives"].as_array().map(Vec::len), Some(2));
    assert_eq!(tree["alternatives"][0]["alternatives"], json!([]));
    assert_eq!(tree["containers"][1]["placements"][0]["fixed"], json!(true));
}

fn container_ids(request: &Value) -> Vec<String> {
    let result: Value =
        serde_json::from_str(&pack_json(&request.to_string()).expect("the request packs"))
            .expect("result JSON");
    result["containers"]
        .as_array()
        .expect("containers")
        .iter()
        .map(|container| container["id"].as_str().expect("id").to_owned())
        .collect()
}

#[test]
fn the_grid_lattice_numbers_containers_per_type() {
    let request = json!({
        "items": [{ "id": "cube", "quantity": 6, "weight": "1000",
                    "dimensions": { "length": "100", "width": "100", "height": "100" } }],
        "containers": [
            { "id": "quad", "inner_dimensions": { "length": "200", "width": "200", "height": "100" } },
            { "id": "pair", "inner_dimensions": { "length": "200", "width": "100", "height": "100" } }
        ],
        "configuration": { "solver_profile": "balanced", "time_limit_ms": 60000 }
    });

    assert_eq!(container_ids(&request), ["quad#1", "pair#1"]);
}

#[test]
fn the_block_loader_numbers_containers_per_type() {
    // The block loader opens nine `small` boxes before any `large`, which it once numbered
    // from ten.
    let request = json!({
        "units": { "length": "cm" },
        "configuration": {
            "objective": "lowest_landed_cost",
            "effort_budget": { "max_candidates_evaluated": 20000, "max_placement_attempts": 20000,
                               "max_search_nodes": 20000 },
            "time_limit_ms": 300000,
            "dimensional_weight_length_unit": "cm", "dimensional_weight_weight_unit": "kg",
            "dimensional_weight_divisor": 5000
        },
        "items": [
            { "id": "product-21", "quantity": 10, "weight": "1.4 kg",
              "dimensions": { "length": "22", "width": "18", "height": "25" } },
            { "id": "product-22", "quantity": 20, "weight": "0.25 kg",
              "dimensions": { "length": "9", "width": "9", "height": "10" } },
            { "id": "product-11", "quantity": 20, "weight": "0.35 kg",
              "dimensions": { "length": "10", "width": "8", "height": "9" } }
        ],
        "containers": [
            { "id": "small", "inner_dimensions": { "length": "25", "width": "20", "height": "15" },
              "cost_minor": 40,
              "rate_table": { "weight_brackets_g": [1000, 2000, 5000, 10000],
                              "prices_minor": [495, 695, 995, 1495] } },
            { "id": "large", "inner_dimensions": { "length": "40", "width": "30", "height": "30" },
              "outer_dimensions": { "length": "41", "width": "31", "height": "31" },
              "cost_minor": 90,
              "rate_table": { "weight_brackets_g": [3000, 20000], "prices_minor": [1200, 1900],
                              "minimum_charge_minor": 1000, "fuel_surcharge_permille": 85 } }
        ]
    });

    let ids = container_ids(&request);

    assert_eq!(
        ids.iter()
            .filter(|id| id.starts_with("large#"))
            .collect::<Vec<_>>(),
        ["large#1", "large#2", "large#3", "large#4", "large#5"]
    );
}

/// A fact read back from a result needs a code to be one; everything beside the code is its
/// attributes, kept as written.
#[test]
fn a_result_fact_is_read_back_only_with_a_code() {
    assert_eq!(ResultFact::from_json(&json!({"code": ""})), None);
    assert_eq!(ResultFact::from_json(&json!({"reason": "x"})), None);
    assert_eq!(ResultFact::from_json(&json!("all_items_packed")), None);
    let fact = ResultFact::from_json(&json!({"code": "gap_bounded", "gap": [0, 1]}))
        .expect("a fact with a code");
    assert_eq!(fact.code, "gap_bounded");
    assert_eq!(
        fact.attributes,
        std::collections::BTreeMap::from([("gap".to_owned(), json!([0, 1]))])
    );
}
