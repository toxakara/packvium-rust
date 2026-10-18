//! `validate_json`: the reference validator's verdict on a packed result.
//!
//! Issue-for-issue agreement with `packvium.validate` over the whole conformance corpus is
//! `conformance/result_validation/run.py`'s job; these pin the public entry points.

use packvium_core::{RefusalKind, ResultIssue, pack_json, validate_json, validate_json_outcome};
use serde_json::Value;

const REQUEST: &str = r#"{
  "items": [{"id": "cube", "quantity": 2, "dimensions": {"length": "10", "width": "10", "height": "10"}}],
  "containers": [{"id": "box", "inner_dimensions": {"length": "20", "width": "10", "height": "10"}}]
}"#;

fn packed() -> Value {
    serde_json::from_str(&pack_json(REQUEST).unwrap()).unwrap()
}

#[test]
fn a_result_the_engine_packed_is_valid() {
    assert_eq!(
        validate_json(REQUEST, &packed().to_string()).unwrap(),
        Vec::<ResultIssue>::new()
    );
}

#[test]
fn two_items_in_one_place_are_a_collision_named_as_the_reference_names_it() {
    let mut result = packed();
    let first = result["containers"][0]["placements"][0]["position"].clone();
    result["containers"][0]["placements"][1]["position"] = first;
    let issues = validate_json(REQUEST, &result.to_string()).unwrap();
    assert_eq!(
        issues,
        vec![ResultIssue {
            code: "collision".to_owned(),
            detail: "cube#1 with cube#2 in box#1".to_owned(),
        }]
    );
}

#[test]
fn a_result_of_the_wrong_shape_is_refused_rather_than_judged() {
    let refusal = validate_json(REQUEST, r#"{"containers": "none"}"#).unwrap_err();
    assert_eq!(refusal.kind, RefusalKind::Contract);
    assert_eq!(refusal.message, "result is missing 'status'");
}

#[test]
fn the_outcome_form_carries_issues_or_a_refusal() {
    let valid: Value =
        serde_json::from_str(&validate_json_outcome(REQUEST, &packed().to_string())).unwrap();
    assert_eq!(valid, serde_json::json!({ "issues": [] }));
    let refused: Value = serde_json::from_str(&validate_json_outcome(REQUEST, "[]")).unwrap();
    assert_eq!(refused["refusal"]["kind"], "contract");
}

/// The audit's scene, where an item may rest on exactly half its base at a ratio that
/// rounds down to 500000 ppm.
#[test]
fn the_audit_reproduction_is_packed_and_valid() {
    let request = r#"{
      "configuration": {"minimum_support_ratio": 0.5000001, "solver_profile": "fast"},
      "items": [
        {"id": "A", "quantity": 1, "priority": 10, "allowed_rotations": ["LWH"],
         "dimensions": {"length": "100", "width": "50", "height": "50"}},
        {"id": "B", "quantity": 1, "allowed_rotations": ["LWH"],
         "dimensions": {"length": "100", "width": "100", "height": "50"}}
      ],
      "containers": [{"id": "box", "inner_dimensions": {"length": "100", "width": "100", "height": "100"}}]
    }"#;
    let result = pack_json(request).unwrap();
    assert_eq!(
        validate_json(request, &result).unwrap(),
        Vec::<ResultIssue>::new()
    );
}

const BAR_BASE: i64 = 10_000_000;

fn bar_request(ratio: f64) -> String {
    let bar = |id: &str| {
        serde_json::json!({
            "id": id, "quantity": 1, "allowed_rotations": ["LWH"],
            "dimensions": {"length": "1", "width": BAR_BASE.to_string(), "height": "1"}
        })
    };
    serde_json::json!({
        "units": {"length": "tick"},
        "configuration": {"minimum_support_ratio": ratio},
        "items": [bar("lower"), bar("upper")],
        "containers": [{"id": "box", "inner_dimensions": {
            "length": "1", "width": (2 * BAR_BASE).to_string(), "height": "2"}}]
    })
    .to_string()
}

/// The `insufficient_support` details when the upper bar rests on `supported` ticks of the lower.
fn support_details(ratio: f64, supported: i64) -> Vec<String> {
    let mut result: Value = serde_json::from_str(&pack_json(&bar_request(0.0)).unwrap()).unwrap();
    for placement in result["containers"][0]["placements"]
        .as_array_mut()
        .unwrap()
    {
        if placement["item_type"] == "upper" {
            let y = BAR_BASE - supported;
            placement["position"]["y"] =
                serde_json::json!({"ticks": y, "value": y.to_string(), "unit": "tick"});
            placement["position"]["z"] =
                serde_json::json!({"ticks": 1, "value": "1", "unit": "tick"});
        }
    }
    validate_json(&bar_request(ratio), &result.to_string())
        .unwrap()
        .into_iter()
        .filter(|issue| issue.code == "insufficient_support")
        .map(|issue| issue.detail)
        .collect()
}

/// Support is judged by the solvers' ppm rule, `floor(base * ppm / 10^6)` with
/// `ppm = floor(ratio * 10^6 + 0.5)` in doubles, not by the exact rational: on a 10^7 tick base
/// each ratio's ppm floor is admitted and one tick less is refused. `0.0001245` is 124 ppm in
/// doubles where the exact decimal rounds to 125.
#[test]
fn support_is_judged_by_the_solvers_ppm_rule() {
    for (ratio, threshold) in [
        (0.5000001, 5_000_000),
        (0.0001245, 1_240),
        (0.9999996, 10_000_000),
        (0.25, 2_500_000),
    ] {
        assert!(
            support_details(ratio, threshold).is_empty(),
            "ratio {ratio}"
        );
        assert_eq!(
            support_details(ratio, threshold - 1).len(),
            1,
            "ratio {ratio}"
        );
    }
    assert_eq!(
        support_details(0.5000001, 4_999_999),
        vec!["upper#1: 0.500000 < 0.500000".to_owned()]
    );
}

const RESERVE_VOLUME: i64 = 10_000_000;

fn reserve_request(ratio: f64, width: i64) -> String {
    serde_json::json!({
        "units": {"length": "tick"},
        "items": [{"id": "bar", "quantity": 1, "allowed_rotations": ["LWH"],
                   "dimensions": {"length": "1", "width": width.to_string(), "height": "1"}}],
        "containers": [{"id": "box", "void_fill_reserve_ratio": ratio, "inner_dimensions": {
            "length": "1", "width": RESERVE_VOLUME.to_string(), "height": "1"}}]
    })
    .to_string()
}

/// The `void_fill_reserve_*` issues when a bar `width` ticks wide reports `reserve`.
fn reserve_issues(ratio: f64, width: i64, reserve: i64) -> Vec<String> {
    let mut result: Value =
        serde_json::from_str(&pack_json(&reserve_request(0.0, 1)).unwrap()).unwrap();
    let container = &mut result["containers"][0];
    container["placements"][0]["dimensions"]["width"] =
        serde_json::json!({"ticks": width, "value": width.to_string(), "unit": "tick"});
    container["used_volume_ticks3"] = Value::String(width.to_string());
    container["void_fill_reserve_ticks3"] = Value::String(reserve.to_string());
    validate_json(&reserve_request(ratio, width), &result.to_string())
        .unwrap()
        .into_iter()
        .filter(|issue| issue.code.starts_with("void_fill_reserve"))
        .map(|issue| format!("{}: {}", issue.code, issue.detail))
        .collect()
}

/// (audit F5): the void-fill reserve goes through the same conversion, reserving
/// `floor(volume * ppm / 10^6)`. On a 10^7 cubic tick container the engine reports that reserve,
/// a bar filling exactly the usable volume is valid and one tick wider is refused.
#[test]
fn the_void_fill_reserve_is_judged_by_the_solvers_ppm_rule() {
    for (ratio, reserve) in [
        (0.0001245, 1_240),
        (0.0002445, 2_440),
        (0.5000001, 5_000_000),
        (0.9999994, 9_999_990),
        (0.25, 2_500_000),
    ] {
        let request = reserve_request(ratio, 1);
        let packed: Value = serde_json::from_str(&pack_json(&request).unwrap()).unwrap();
        assert_eq!(
            packed["containers"][0]["void_fill_reserve_ticks3"],
            Value::String(reserve.to_string()),
            "ratio {ratio}"
        );
        assert_eq!(
            validate_json(&request, &packed.to_string()).unwrap(),
            Vec::<ResultIssue>::new(),
            "ratio {ratio}"
        );
        let usable = RESERVE_VOLUME - reserve;
        assert!(
            reserve_issues(ratio, usable, reserve).is_empty(),
            "ratio {ratio}"
        );
        assert_eq!(
            reserve_issues(ratio, usable + 1, reserve).len(),
            1,
            "ratio {ratio}"
        );
    }
    assert_eq!(
        reserve_issues(0.0001245, 9_998_761, 1_240),
        vec!["void_fill_reserve_exceeded: box#1: used 9998761 > usable 9998760".to_owned()]
    );
}

fn compression_request(ratio: f64) -> String {
    serde_json::json!({
        "items": [
            {"id": "pad", "quantity": 1, "must_be_on_floor": true, "allowed_rotations": ["LWH"],
             "dimensions": {"length": "100", "width": "100", "height": "1000"},
             "shape_type": "compressible", "compression_ratio": ratio,
             "max_compression_pressure_kpa": 10},
            {"id": "brick", "quantity": 1, "allowed_rotations": ["LWH"],
             "dimensions": {"length": "100", "width": "100", "height": "10"},
             "weight": {"value": "5", "unit": "kg"}}
        ],
        "containers": [{"id": "box", "inner_dimensions": {
            "length": "100", "width": "100", "height": "2000"}}]
    })
    .to_string()
}

fn compressed_used_volume(ratio: f64) -> Value {
    let result: Value =
        serde_json::from_str(&pack_json(&compression_request(ratio)).unwrap()).unwrap();
    result["containers"][0]["used_volume_ticks3"].clone()
}

/// `compression_ratio` goes through the same conversion. A 5 kg brick holds a 1 m
/// compressible pad at half its 10 kPa limit, where one ppm moves the occupied height by about
/// eight ticks: the engine compresses by the double's ppm and the validator agrees.
#[test]
fn compression_is_judged_by_the_solvers_ppm_rule() {
    for (ratio, ppm_ratio) in [
        (0.0001245, 0.000124),
        (0.0002445, 0.000244),
        (0.5000001, 0.5),
        (0.25, 0.25),
    ] {
        let request = compression_request(ratio);
        let result = pack_json(&request).unwrap();
        assert_eq!(
            validate_json(&request, &result).unwrap(),
            Vec::<ResultIssue>::new(),
            "ratio {ratio}"
        );
        assert_eq!(
            compressed_used_volume(ratio),
            compressed_used_volume(ppm_ratio),
            "ratio {ratio}"
        );
    }
    assert_ne!(
        compressed_used_volume(0.0001245),
        compressed_used_volume(0.000125)
    );
}

/// An issue and a refusal print as the reference's `str` of them.
#[test]
fn an_issue_and_a_refusal_print_as_the_reference_prints_them() {
    let mut result = packed();
    let first = result["containers"][0]["placements"][0]["position"].clone();
    result["containers"][0]["placements"][1]["position"] = first;
    let issues = validate_json(REQUEST, &result.to_string()).unwrap();
    assert_eq!(
        issues[0].to_string(),
        "collision: cube#1 with cube#2 in box#1"
    );
    let refusal = validate_json(REQUEST, r#"{"containers": "none"}"#).unwrap_err();
    assert_eq!(refusal.to_string(), "result is missing 'status'");
}

/// The reference reads a packed container's id only to name it in an issue, so a container
/// without one is refused once an issue has to name it.
#[test]
fn an_issue_on_a_container_without_an_id_is_refused() {
    let mut result = packed();
    let first = result["containers"][0]["placements"][0]["position"].clone();
    result["containers"][0]["placements"][1]["position"] = first;
    result["containers"][0]
        .as_object_mut()
        .unwrap()
        .remove("id");
    let refusal = validate_json(REQUEST, &result.to_string()).unwrap_err();
    assert_eq!(refusal.kind, RefusalKind::Shape);
}
