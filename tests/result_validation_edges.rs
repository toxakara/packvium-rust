//! `validate_json` at the edges of its input: values the reference reads but this port cannot
//! compute exactly, request fields of the wrong type, and the rarer geometry and objective
//! branches. Every scene starts from a result the engine packed and is then edited by hand, so
//! each test changes exactly the fact it is about.

use packvium_core::{MalformedResult, RefusalKind, ResultIssue, pack_json, validate_json};
use serde_json::{Value, json};

const OVERFLOW: &str =
    "arithmetic overflow: a value is beyond what this validator computes exactly";

/// Two 10-tick cubes side by side in a 20 x 10 x 10 tick box, one gram each.
fn request() -> Value {
    json!({
        "units": {"length": "tick"},
        "items": [{"id": "a", "quantity": 2, "weight": "1",
                   "dimensions": {"length": "10", "width": "10", "height": "10"}}],
        "containers": [{"id": "box",
                        "inner_dimensions": {"length": "20", "width": "10", "height": "10"}}]
    })
}

fn packed(request: &Value) -> Value {
    serde_json::from_str(&pack_json(&request.to_string()).unwrap()).unwrap()
}

fn validate(request: &Value, result: &Value) -> Result<Vec<ResultIssue>, MalformedResult> {
    validate_json(&request.to_string(), &result.to_string())
}

fn codes(request: &Value, result: &Value) -> Vec<String> {
    validate(request, result)
        .unwrap()
        .into_iter()
        .map(|issue| issue.code)
        .collect()
}

fn refusal(request: &Value, result: &Value) -> MalformedResult {
    validate(request, result).unwrap_err()
}

fn assert_overflow(request: &Value, result: &Value) {
    let refused = refusal(request, result);
    assert_eq!(refused.kind, RefusalKind::Shape);
    assert_eq!(refused.message, OVERFLOW);
}

fn tick(value: impl Into<Value>) -> Value {
    json!({"ticks": value.into(), "unit": "tick"})
}

/// A placement at `position` with `sides`, all in ticks.
fn placement(id: &str, item_type: &str, position: [u64; 3], sides: [u64; 3]) -> Value {
    json!({
        "item_id": id, "item_type": item_type, "orientation": "LWH",
        "position": {"x": tick(position[0]), "y": tick(position[1]), "z": tick(position[2])},
        "dimensions": {"length": tick(sides[0]), "width": tick(sides[1]), "height": tick(sides[2])}
    })
}

/// The packed default scene with its one container's placements replaced.
fn with_placements(placements: Vec<Value>) -> Value {
    let mut result = packed(&request());
    result["containers"][0]["placements"] = Value::Array(placements);
    result
}

fn issue_detail(request: &Value, result: &Value, code: &str) -> Vec<String> {
    validate(request, result)
        .unwrap()
        .into_iter()
        .filter(|issue| issue.code == code)
        .map(|issue| issue.detail)
        .collect()
}

#[test]
fn the_unedited_scene_is_valid() {
    let request = request();
    assert_eq!(codes(&request, &packed(&request)), Vec::<String>::new());
}

// --- the request's own fields -------------------------------------------------------------

#[test]
fn a_tag_list_holding_a_non_string_is_refused_naming_the_field() {
    for field in ["eligible_container_tags", "incompatible_tags"] {
        let mut request = request();
        let result = packed(&request);
        request["items"][0][field] = json!([1]);
        let refused = refusal(&request, &result);
        assert_eq!(refused.kind, RefusalKind::Shape);
        assert_eq!(
            refused.message,
            format!("{field} must be a string, got an integer")
        );
    }
}

#[test]
fn an_unreadable_ratio_or_clearance_is_refused_as_python_refuses_the_fraction() {
    let mut reserve = request();
    let result = packed(&reserve);
    reserve["containers"][0]["void_fill_reserve_ratio"] = json!("half");
    assert_eq!(
        refusal(&reserve, &result).message,
        "Invalid literal for Fraction: \"half\""
    );
    let mut clearance = request();
    clearance["configuration"] = json!({"clearance": "wide"});
    assert_eq!(
        refusal(&clearance, &result).message,
        "Invalid literal for Fraction: \"wide\""
    );
}

// --- the objective --------------------------------------------------------------------------

#[test]
fn an_objective_that_is_not_a_string_is_a_contract_break() {
    let mut request = request();
    let result = packed(&request);
    request["configuration"] = json!({"objective": 5});
    let refused = refusal(&request, &result);
    assert_eq!(refused.kind, RefusalKind::Contract);
    assert_eq!(refused.message, "unknown requested objective 5");
}

/// A container of no volume has no unused share and no height share: the reference skips it
/// rather than divide by its zero height.
#[test]
fn a_zero_volume_container_adds_only_its_count_and_cost_to_the_score() {
    let mut request = request();
    let result = packed(&request);
    request["containers"][0]["inner_dimensions"]["height"] = json!("0");
    assert_eq!(
        issue_detail(&request, &result, "score_mismatch"),
        ["reported [0, 1, 0, 0, 1000000], recomputed [0, 1, 0, 0, 0]"]
    );
}

fn with_ghost_unpacked(result: &mut Value) {
    result["unpacked_items"] = json!([{
        "item_id": "ghost#1", "item_type": "ghost", "reason": "no_feasible_placement",
        "proof": {"level": "inferred", "observations": ["searched"]}
    }]);
}

/// An unpacked item of a type the request never declared counts as unpacked but carries no
/// value and no volume into the objective.
#[test]
fn an_undeclared_unpacked_type_counts_but_weighs_nothing_in_value_and_volume_objectives() {
    for (objective, recomputed) in [
        ("maximum_value", "[1, 0, 1, 0, 0]"),
        ("maximum_volume", "[0, 1, 1, 0, 0]"),
    ] {
        let mut request = request();
        request["configuration"] = json!({"objective": objective});
        let mut result = packed(&request);
        with_ghost_unpacked(&mut result);
        let mismatch = issue_detail(&request, &result, "score_mismatch");
        assert_eq!(mismatch.len(), 1, "{objective}");
        assert!(
            mismatch[0].ends_with(&format!("recomputed {recomputed}")),
            "{objective}: {mismatch:?}"
        );
    }
}

#[test]
fn maximum_volume_with_nothing_requested_scores_no_unpacked_share() {
    let mut request = request();
    request["configuration"] = json!({"objective": "maximum_volume"});
    let mut result = packed(&request);
    with_ghost_unpacked(&mut result);
    request["items"][0]["quantity"] = json!(0);
    let mismatch = issue_detail(&request, &result, "score_mismatch");
    assert_eq!(mismatch.len(), 1);
    assert!(
        mismatch[0].ends_with("recomputed [0, 1, 1, 0, 0]"),
        "{mismatch:?}"
    );
}

#[test]
fn a_dimensional_weight_unit_outside_the_tables_is_refused() {
    let mut request = request();
    request["configuration"] =
        json!({"objective": "shipping_cost", "dimensional_weight_divisor": 5000});
    let result = packed(&request);
    request["configuration"]["dimensional_weight_length_unit"] = json!("furlong");
    let refused = refusal(&request, &result);
    assert_eq!(refused.kind, RefusalKind::Shape);
    assert_eq!(refused.message, "unknown unit \"furlong\"");
}

/// A flat bag is billed at the size it achieved, not at its flat size.
#[test]
fn a_flat_bag_is_billed_at_its_achieved_extent() {
    let mut request = json!({
        "units": {"length": "mm"},
        "configuration": {"objective": "shipping_cost", "dimensional_weight_divisor": 1,
                          "dimensional_weight_length_unit": "mm",
                          "dimensional_weight_weight_unit": "g"},
        "items": [{"id": "book", "quantity": 1, "weight": "1",
                   "dimensions": {"length": "20", "width": "10", "height": "5"}}],
        "containers": [{"id": "mailer", "deformable_boundary": {"kind": "flat_bag"},
                        "inner_dimensions": {"length": "100", "width": "100", "height": "100"}}]
    });
    let result = packed(&request);
    assert_eq!(codes(&request, &result), Vec::<String>::new());
    // Billed at 20 x 10 x 5 mm = 1000 g, the flat 100 x 100 x 100 mm would be 1000000 g.
    let billed = result["score"][1].as_i64().unwrap();
    assert_eq!(billed, 1000 * 8_000_000);
    request["containers"][0]
        .as_object_mut()
        .unwrap()
        .remove("deformable_boundary");
    let mismatch = issue_detail(&request, &result, "score_mismatch");
    assert!(
        mismatch[0].contains(&format!("recomputed [0, {}", 1_000_000i64 * 8_000_000)),
        "{mismatch:?}"
    );
}

#[test]
fn a_solver_reported_as_null_matches_no_requested_solver() {
    let mut request = request();
    request["configuration"] = json!({"solvers": ["grid"]});
    let mut result = packed(&request);
    result["algorithm"]["solver"] = Value::Null;
    assert_eq!(
        issue_detail(&request, &result, "solver_selection_mismatch"),
        ["reported None, requested one of ['grid']"]
    );
}

// --- fixed placements ---------------------------------------------------------------------

fn fixed(item_type: Value, x: &str) -> Value {
    json!({"container_type": "box", "item_type": item_type, "orientation": "LWH",
           "position": {"x": x, "y": "0", "z": "0"}})
}

#[test]
fn a_fixed_position_that_is_not_a_measure_is_refused() {
    let mut request = request();
    let result = packed(&request);
    request["fixed_placements"] = json!([fixed(json!("a"), "wide")]);
    assert_eq!(
        refusal(&request, &result).message,
        "Invalid literal for Fraction: \"wide\""
    );
}

/// Requested fixed placements are reported in sorted order, whatever order the request and
/// the hash map hold them in.
#[test]
fn missing_fixed_placements_are_reported_in_sorted_order() {
    let mut request = request();
    let result = packed(&request);
    request["fixed_placements"] = json!([fixed(json!("a"), "10"), fixed(json!("a"), "0")]);
    assert_eq!(
        issue_detail(&request, &result, "fixed_placement_moved"),
        [
            "a fixed in box#1 at (0, 0, 0) LWH",
            "a fixed in box#1 at (10, 0, 0) LWH"
        ]
    );
}

#[test]
fn fixed_placements_that_cannot_be_ordered_are_refused() {
    let mut request = request();
    let result = packed(&request);
    request["fixed_placements"] = json!([fixed(json!("a"), "10"), fixed(json!(7), "0")]);
    let refused = refusal(&request, &result);
    assert_eq!(refused.kind, RefusalKind::Shape);
    assert!(refused.message.starts_with("cannot order"), "{refused:?}");
}

/// A boolean tick is Python's `int` for geometry, but prints as the boolean it is.
#[test]
fn an_unexpected_fixed_placement_prints_a_non_integer_tick_as_python_prints_it() {
    let request = request();
    let mut result = packed(&request);
    let first = &mut result["containers"][0]["placements"][0];
    first["fixed"] = json!(true);
    first["position"]["x"]["ticks"] = json!(false);
    assert_eq!(
        issue_detail(&request, &result, "unexpected_fixed_placement"),
        ["a in box#1 at (False, 0, 0) LWH"]
    );
}

// --- policy rules ---------------------------------------------------------------------------

fn policy_scene(policy: Value) -> (Value, Value) {
    let mut request = request();
    let mut result = packed(&request);
    request["items"][0]["quantity"] = json!(3);
    request["items"][0]["tags"] = json!(["cold"]);
    request["containers"][0]["tags"] = json!(["cold"]);
    request["policy"] = policy;
    result["unpacked_items"] = json!([{
        "item_id": "a#3", "item_type": "a", "reason": "policy_rule",
        "proof": {"level": "proven", "observations": ["rule"]}
    }]);
    (request, result)
}

const NO_RULE: &str = "a#3: no participating policy rule rules it out of every container";

fn rule(extra: Value) -> Value {
    let mut rule = json!({"id": "cold-chain",
        "require_container_tag": {"item_tag": "cold", "container_tag": "cold"}});
    for (key, value) in extra.as_object().unwrap() {
        rule[key] = value.clone();
    }
    rule
}

#[test]
fn a_rule_some_container_satisfies_rules_nothing_out() {
    let (request, result) = policy_scene(json!({"as_of": 0, "rules": [rule(json!({}))]}));
    assert_eq!(
        issue_detail(&request, &result, "invalid_proven_reason"),
        [NO_RULE]
    );
}

#[test]
fn a_container_tag_that_cannot_be_hashed_is_refused() {
    let mut cold_chain = rule(json!({}));
    cold_chain["require_container_tag"]["container_tag"] = json!(["cold"]);
    let (request, result) = policy_scene(json!({"as_of": 0, "rules": [cold_chain]}));
    assert_eq!(refusal(&request, &result).message, "a list is unhashable");
}

#[test]
fn a_policy_whose_rules_are_not_a_list_declares_no_rule() {
    let (request, result) =
        policy_scene(json!({"as_of": 0, "rules": {"cold-chain": rule(json!({}))}}));
    assert_eq!(
        issue_detail(&request, &result, "invalid_proven_reason"),
        [NO_RULE]
    );
}

#[test]
fn a_rule_whose_applies_to_is_not_an_object_is_refused() {
    let (request, result) =
        policy_scene(json!({"as_of": 0, "rules": [rule(json!({"applies_to": [1]}))]}));
    let refused = refusal(&request, &result);
    assert_eq!(refused.kind, RefusalKind::Shape);
    assert_eq!(refused.message, "applies_to must be an object");
}

/// The proven-reason checks read the item's declaration; an undeclared type has none to
/// contradict the claim.
#[test]
fn a_proven_reason_for_an_undeclared_type_is_not_second_guessed() {
    let request = request();
    let mut result = packed(&request);
    let unpacked = |item_type: &str| {
        json!([{"item_id": format!("{item_type}#9"), "item_type": item_type,
                "reason": "no_compatible_container_dimensions",
                "proof": {"level": "proven", "observations": ["measured"]}}])
    };
    result["unpacked_items"] = unpacked("a");
    assert_eq!(
        issue_detail(&request, &result, "invalid_proven_reason"),
        [
            "a#9: at least one allowed orientation fits a container",
            "a#9: fits at least one container once every rotation is tried, not only its own allowed set -- should be rotation_restricted"
        ]
    );
    result["unpacked_items"] = unpacked("ghost");
    assert_eq!(
        issue_detail(&request, &result, "invalid_proven_reason"),
        Vec::<String>::new()
    );
}

// --- effort and status facts ----------------------------------------------------------------

#[test]
fn a_budget_is_judged_only_against_the_metrics_reported() {
    let mut request = request();
    let mut result = packed(&request);
    request["configuration"] = json!({"effort_budget": {"max_candidates_evaluated": 0}});
    result["algorithm"]["metrics"]["feasible_candidates"] = json!(2);
    assert_eq!(
        issue_detail(&request, &result, "effort_budget_exceeded"),
        ["feasible_candidates is 2, above max_candidates_evaluated=0"]
    );
    result["algorithm"]["metrics"]
        .as_object_mut()
        .unwrap()
        .remove("feasible_candidates");
    assert_eq!(
        issue_detail(&request, &result, "effort_budget_exceeded"),
        Vec::<String>::new()
    );
}

#[test]
fn an_infeasible_status_with_every_rejection_proven_is_not_unproven() {
    let mut request = request();
    request["items"][0]["dimensions"]["length"] = json!("30");
    let mut result = packed(&request);
    assert_eq!(result["unpacked_items"][1]["proof"]["level"], "proven");
    result["status"] = json!("infeasible");
    assert!(!codes(&request, &result).contains(&"unproven_infeasible".to_owned()));
    result["unpacked_items"][1]["proof"]["level"] = json!("inferred");
    assert!(codes(&request, &result).contains(&"unproven_infeasible".to_owned()));
}

// --- support, load and nesting --------------------------------------------------------------

/// A support ratio so small it rounds to no required area still demands that the centroid
/// rest over something; with no supporter at all there is no hull to rest over.
#[test]
fn a_floating_item_under_a_vanishing_support_ratio_has_an_unsupported_centroid() {
    let mut request = request();
    request["items"][0]["minimum_support_ratio"] = json!("0.0000001");
    request["containers"][0]["inner_dimensions"]["height"] = json!("30");
    let result = with_placements(vec![
        placement("a#1", "a", [0, 0, 0], [10, 10, 10]),
        placement("a#2", "a", [10, 0, 15], [10, 10, 10]),
    ]);
    assert_eq!(
        issue_detail(&request, &result, "centre_of_gravity_unsupported"),
        ["a#2: centroid outside its support hull"]
    );
}

/// Items level with each other are ordered by id for load propagation, and ids Python cannot
/// order are its `TypeError`.
#[test]
fn level_items_with_ids_of_mixed_types_are_refused() {
    let request = request();
    let mut result = packed(&request);
    result["containers"][0]["placements"][0]["item_id"] = json!(7);
    let refused = refusal(&request, &result);
    assert_eq!(refused.kind, RefusalKind::Shape);
    assert_eq!(refused.message, "cannot order a string against an integer");
}

#[test]
fn an_item_id_that_cannot_be_hashed_is_refused() {
    let request = request();
    let mut result = packed(&request);
    result["containers"][0]["placements"][0]["item_id"] = json!(["a", 1]);
    let refused = refusal(&request, &result);
    assert_eq!(refused.kind, RefusalKind::Shape);
    assert_eq!(refused.message, "a list is unhashable");
}

/// Two items due at the same stop are unloaded by id, so their ids must be orderable.
#[test]
fn route_stops_whose_item_ids_cannot_be_ordered_are_refused() {
    let mut request = request();
    request["items"][0]["stop_index"] = json!(1);
    // Different heights, so load propagation never has to order the two ids.
    let mut shorter = placement("a#2", "a", [10, 0, 0], [10, 10, 5]);
    shorter["item_id"] = json!(2);
    let result = with_placements(vec![
        placement("a#1", "a", [0, 0, 0], [10, 10, 10]),
        shorter,
    ]);
    let refused = refusal(&request, &result);
    assert_eq!(refused.kind, RefusalKind::Shape);
    assert!(refused.message.starts_with("cannot order"), "{refused:?}");
}

#[test]
fn a_pressure_limit_without_a_compression_ratio_is_refused() {
    let mut request = request();
    let result = packed(&request);
    request["items"][0]["max_compression_pressure_kpa"] = json!(100);
    let refused = refusal(&request, &result);
    assert_eq!(refused.kind, RefusalKind::Shape);
    assert_eq!(
        refused.message,
        "a: max_compression_pressure_kpa without compression_ratio"
    );
}

fn nesting_request(types: &[&str], length: &str, height: &str) -> Value {
    let items: Vec<Value> = types
        .iter()
        .map(|id| {
            json!({"id": id, "quantity": 4 / types.len(), "nesting_height": "8",
                   "dimensions": {"length": "10", "width": "10", "height": "10"}})
        })
        .collect();
    json!({
        "units": {"length": "tick"},
        "configuration": {"solver_profile": "fast"},
        "items": items,
        "containers": [{"id": "box",
                        "inner_dimensions": {"length": length, "width": "20", "height": height}}]
    })
}

/// A nested column counts the volume its instances share once: four 10-tick cups in two
/// columns of two, each upper cup sunk 8 into the one below, occupy 4000 - 2 * 8 * 100 ticks^3.
#[test]
fn a_nested_column_counts_its_overlap_once() {
    let request = nesting_request(&["cup"], "10", "16");
    let result = packed(&request);
    assert_eq!(codes(&request, &result), Vec::<String>::new());
    assert_eq!(result["containers"][0]["used_volume_ticks3"], "2400");
}

/// With a clearance of half the nesting height the envelopes nest while the items only touch,
/// so nothing is subtracted.
#[test]
fn a_column_sunk_no_deeper_than_its_clearance_counts_whole() {
    let mut request = nesting_request(&["cup"], "10", "30");
    request["configuration"]["clearance"] = json!("4");
    let mut result = with_placements(vec![
        placement("cup#1", "cup", [0, 0, 0], [10, 10, 10]),
        placement("cup#2", "cup", [0, 0, 10], [10, 10, 10]),
    ]);
    result["containers"][0]["used_volume_ticks3"] = json!("0");
    assert_eq!(
        issue_detail(&request, &result, "used_volume_mismatch"),
        ["box#1: reported 0, computed 2000"]
    );
}

/// Instances side by side, or of different nesting types, are not a column.
#[test]
fn nesting_items_that_do_not_form_a_column_count_whole() {
    for types in [&["cup"][..], &["cup", "bowl"][..]] {
        let request = nesting_request(types, "20", "10");
        let result = packed(&request);
        assert_eq!(codes(&request, &result), Vec::<String>::new(), "{types:?}");
        assert_eq!(result["containers"][0]["used_volume_ticks3"], "4000");
    }
}

fn overlapping_hull_scene(shape_type: &str, clearance: Option<&str>) -> (Value, Value) {
    let mut request = request();
    request["items"][0]["shape_type"] = json!(shape_type);
    request["items"][0]["hull_vertices"] = json!([
        {"x": "0", "y": "0", "z": "0"}, {"x": "10", "y": "0", "z": "0"},
        {"x": "0", "y": "10", "z": "0"}, {"x": "0", "y": "0", "z": "10"}
    ]);
    if let Some(clearance) = clearance {
        request["configuration"] = json!({"clearance": clearance});
    }
    let result = with_placements(vec![
        placement("a#1", "a", [0, 0, 0], [10, 10, 10]),
        placement("a#2", "a", [9, 9, 9], [10, 10, 10]),
    ]);
    (request, result)
}

/// Two tetrahedra whose boxes overlap only at the corner far from both solids do not
/// collide -- but only an exact convex hull that matches its reported box is judged that way.
#[test]
fn hull_vertices_count_only_for_a_convex_hull_matching_its_box() {
    let (request, result) = overlapping_hull_scene("convex_hull", None);
    assert_eq!(
        issue_detail(&request, &result, "collision"),
        Vec::<String>::new()
    );
    let (request, result) = overlapping_hull_scene("rigid_cuboid", None);
    assert_eq!(
        issue_detail(&request, &result, "collision"),
        ["a#1 with a#2 in box#1"]
    );
    // A clearance grows the envelope past the declared box, so the box is used.
    let (request, result) = overlapping_hull_scene("convex_hull", Some("1"));
    assert_eq!(
        issue_detail(&request, &result, "collision"),
        ["a#1 with a#2 in box#1"]
    );
}

// --- made to measure, centre of mass and axles ----------------------------------------------

fn machine(request: &mut Value) {
    request["containers"][0]["made_to_measure"] = json!({
        "minimum": {"length": "1", "width": "1", "height": "1"},
        "increment": {"length": "1", "width": "1", "height": "1"}
    });
}

#[test]
fn a_reported_side_without_a_measure_reads_as_none() {
    let mut request = request();
    let mut result = packed(&request);
    machine(&mut request);
    result["containers"][0]["inner_dimensions"]["length"] = json!({});
    assert_eq!(
        issue_detail(&request, &result, "fabricated_dimensions_mismatch"),
        ["box#1: reported (None, 10, 10), computed (20, 10, 10)"]
    );
}

/// The centre-of-mass offset divides by the loaded weight times the side, so a container with
/// no length or no width cannot be judged: Python's `ZeroDivisionError`.
#[test]
fn a_weighted_container_with_no_length_or_width_is_refused() {
    for side in ["length", "width"] {
        let mut request = request();
        let result = packed(&request);
        request["containers"][0]["inner_dimensions"][side] = json!("0");
        let refused = refusal(&request, &result);
        assert_eq!(refused.kind, RefusalKind::Shape, "{side}");
        assert_eq!(refused.message, "division by zero", "{side}");
    }
}

#[test]
fn axles_other_than_one_front_and_one_rear_are_refused() {
    let mut request = request();
    let result = packed(&request);
    request["containers"][0]["axles"] = json!([{"position": "5"}]);
    let refused = refusal(&request, &result);
    assert_eq!(refused.kind, RefusalKind::Shape);
    assert_eq!(
        refused.message,
        "box#1: axles must name exactly a front and a rear axle"
    );
}

#[path = "result_validation_edges/exact_arithmetic.rs"]
mod exact_arithmetic;
