//! The 24 proper rotations of a `convex_hull` item (1.10, docs/IRREGULAR-ITEMS.md).
//!
//! Mirrors `packvium-python/tests/test_signed_rotations.py`. The cross-engine contract is held
//! by the conformance runners; this suite pins the table, each refusal on every path that
//! names an orientation, and the one packing a signed code makes possible.

use packvium_core::revisions::{RevisionErrorCode, apply_events_json};
use packvium_core::{
    Container, Dimensions, FixedPlacement, Item, Length, PackError, PackingConfig, PackingRequest,
    Point, Rotation, ShapeType, Weight, pack_json, pack_request, validate_json,
};
use serde_json::{Value, json};

const SIGNED: &str = "a signed rotation code requires shape_type convex_hull";

fn every_code() -> Vec<&'static str> {
    Rotation::EVERY
        .iter()
        .map(|rotation| rotation.as_str())
        .collect()
}

/// A wedge: the half of a cube under the plane x + y = 100. Its mirror image is a
/// different solid, so only proper rotations may turn it.
fn wedge(rotations: &[&str], keep_upright: bool) -> Value {
    let vertex = |x: &str, y: &str, z: &str| json!({"x": x, "y": y, "z": z});
    json!({"id": "wedge", "quantity": 2, "weight": {"value": "1", "unit": "kg"},
           "dimensions": {"length": "100", "width": "100", "height": "100"},
           "shape_type": "convex_hull", "keep_upright": keep_upright,
           "allowed_rotations": rotations,
           "hull_vertices": [vertex("0", "0", "0"), vertex("100", "0", "0"),
                             vertex("0", "100", "0"), vertex("0", "0", "100"),
                             vertex("100", "0", "100"), vertex("0", "100", "100")]})
}

fn crate_of(quantity: u64) -> Value {
    json!({"id": "crate", "quantity": quantity,
           "inner_dimensions": {"length": "100", "width": "100", "height": "100"}})
}

fn request(item: Value) -> Value {
    json!({"items": [item], "containers": [crate_of(1)],
           "configuration": {"solver_profile": "balanced", "time_limit_ms": 300000,
                             "effort_budget": {"max_candidates_evaluated": 1000000,
                                               "max_placement_attempts": 1000000,
                                               "max_search_nodes": 1000000}}})
}

fn cube(rotations: &[&str]) -> Value {
    json!({"id": "cube", "dimensions": {"length": "10", "width": "20", "height": "30"},
           "allowed_rotations": rotations})
}

fn refusal(request: &Value) -> (String, String, String, String) {
    match pack_json(&request.to_string()) {
        Err(PackError::InvalidRequest(error)) => (
            error.code().to_owned(),
            error.reason().to_owned(),
            error.field().to_owned(),
            error.to_string(),
        ),
        Err(other) => panic!("refused, but not as a request error: {other}"),
        Ok(_) => panic!("{request} was admitted"),
    }
}

fn packed(request: &Value) -> Value {
    serde_json::from_str(&pack_json(&request.to_string()).expect("packs")).expect("json")
}

fn orientations(result: &Value) -> Vec<String> {
    result["containers"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|container| {
            container["placements"]
                .as_array()
                .cloned()
                .unwrap_or_default()
        })
        .map(|placement| placement["orientation"].as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn the_table_has_24_proper_rotations_in_canonical_order() {
    let codes = every_code();
    assert_eq!(
        codes,
        [
            "LWH", "LHW", "WLH", "WHL", "HLW", "HWL", "+L-W-H", "-L+W-H", "-L-W+H", "+L-H+W",
            "+L+H-W", "-L-H-W", "+W-L+H", "+W+L-H", "-W-L-H", "+W-H-L", "-W+H-L", "-W-H+L",
            "+H-L-W", "-H+L-W", "-H-L+W", "+H-W+L", "+H+W-L", "-H-W-L",
        ]
    );
    for (index, rotation) in Rotation::EVERY.into_iter().enumerate() {
        let axes = rotation.source_axes();
        let signs = rotation.signs();
        let mut columns = [[0_i64; 3]; 3];
        for row in 0..3 {
            columns[row][axes[row]] = signs[row];
        }
        let [a, b, c] = columns;
        let determinant = a[0] * (b[1] * c[2] - b[2] * c[1]) - a[1] * (b[0] * c[2] - b[2] * c[0])
            + a[2] * (b[0] * c[1] - b[1] * c[0]);
        assert_eq!(determinant, 1, "{} is a reflection", rotation.as_str());
        assert_eq!(rotation.index(), index);
        assert_eq!(rotation.is_signed(), index >= 6);
        assert_eq!(Rotation::from_code(rotation.as_str()), Some(rotation));
        let wire: Rotation =
            serde_json::from_value(json!(rotation.as_str())).expect("serde names the code");
        assert_eq!(wire, rotation);
    }
    let upright: Vec<&str> = Rotation::EVERY
        .into_iter()
        .filter(|rotation| rotation.is_upright())
        .map(Rotation::as_str)
        .collect();
    assert_eq!(upright, ["LWH", "WLH", "-L-W+H", "+W-L+H"]);
    assert_eq!(Rotation::from_code("XYZ"), None);
    assert_eq!(Rotation::vocabulary(false), Rotation::ALL);
    assert_eq!(Rotation::vocabulary(true), Rotation::EVERY);
}

#[test]
fn a_box_ignores_the_signs_of_a_code() {
    let side = |mm: i64| Length(mm * Length::TICKS_PER_MM);
    let dimensions = Dimensions {
        length: side(10),
        width: side(20),
        height: side(30),
    };
    for rotation in Rotation::EVERY {
        let legacy = Rotation::ALL
            .into_iter()
            .find(|legacy| legacy.source_axes() == rotation.source_axes())
            .unwrap();
        assert_eq!(dimensions.rotated(rotation), dimensions.rotated(legacy));
    }
}

#[test]
fn a_chiral_wedge_pairs_with_its_half_turn_in_one_crate() {
    let result = packed(&request(wedge(&every_code(), false)));
    assert_eq!(result["unpacked_items"], json!([]), "{result}");
    // Rotations tie in table order, never by spelling: LWH first, then its half-turn.
    assert_eq!(orientations(&result), ["LWH", "-L-W+H"]);
    let request_text = request(wedge(&every_code(), false)).to_string();
    let issues = validate_json(&request_text, &result.to_string()).expect("validates");
    assert!(issues.is_empty(), "{issues:?}");
}

#[test]
fn the_six_legacy_codes_cannot_pair_two_wedges() {
    let legacy: Vec<&str> = Rotation::ALL
        .iter()
        .map(|rotation| rotation.as_str())
        .collect();
    let result = packed(&request(wedge(&legacy, false)));
    assert_eq!(orientations(&result).len(), 1, "{result}");
}

#[test]
fn keep_upright_keeps_the_codes_whose_z_is_plus_h() {
    let result = packed(&request(wedge(&every_code(), true)));
    for orientation in orientations(&result) {
        assert!(
            Rotation::from_code(&orientation).unwrap().is_upright(),
            "{orientation}"
        );
    }
}

#[test]
fn a_signed_code_on_a_non_hull_is_a_broken_rule() {
    for shape in [
        None,
        Some("rigid_cuboid"),
        Some("compressible"),
        Some("sphere"),
    ] {
        let mut item = cube(&["LWH", "-L-W+H"]);
        if let Some(shape) = shape {
            item["shape_type"] = json!(shape);
        }
        let refused = refusal(&json!({"items": [item], "containers": [crate_of(1)]}));
        assert_eq!(
            (refused.0.as_str(), refused.1.as_str(), refused.2.as_str()),
            (
                "invalid_request",
                "invalid_value",
                "/items/0/allowed_rotations/1"
            ),
            "{shape:?}"
        );
        assert!(refused.3.ends_with(SIGNED), "{}", refused.3);
    }
}

#[test]
fn an_unknown_code_lists_the_items_own_vocabulary() {
    let refused = refusal(&json!({"items": [cube(&["XYZ"])], "containers": [crate_of(1)]}));
    assert_eq!(refused.1, "not_allowed");
    assert!(
        refused
            .3
            .ends_with(r#"must be one of ["LWH","LHW","WLH","WHL","HLW","HWL"]"#),
        "{}",
        refused.3
    );
    let refused = refusal(&request(wedge(&["LWH", "+L+W-H"], false)));
    assert_eq!(
        (refused.1.as_str(), refused.2.as_str()),
        ("not_allowed", "/items/0/allowed_rotations/1")
    );
    assert!(refused.3.contains("\"-H-W-L\"]"), "{}", refused.3);
}

fn fixed(orientation: &str, item_type: &str) -> Value {
    json!({"item_type": item_type, "container_type": "crate", "orientation": orientation})
}

fn with_fixed(item: Value, placement: Value) -> Value {
    let mut request = request(item);
    request["fixed_placements"] = json!([placement]);
    request
}

#[test]
fn a_fixed_orientation_follows_the_items_vocabulary() {
    let on_a_box = with_fixed(cube(&["LWH"]), fixed("-L-W+H", "cube"));
    assert_eq!(
        refusal(&on_a_box),
        (
            "invalid_fixed_placement".into(),
            "invalid_value".into(),
            "/fixed_placements/0/orientation".into(),
            format!("invalid_fixed_placement: fixed_placements[0].orientation: {SIGNED}"),
        )
    );
    let reflection = with_fixed(wedge(&every_code(), false), fixed("+L+W-H", "wedge"));
    let refused = refusal(&reflection);
    assert_eq!(
        (refused.1.as_str(), refused.2.as_str()),
        ("malformed", "/fixed_placements/0/orientation")
    );
    assert!(
        refused.3.starts_with(
            "invalid_fixed_placement: fixed_placements[0].orientation must be one of [\"LWH\""
        ),
        "{}",
        refused.3
    );
    let not_allowed = with_fixed(wedge(&["LWH"], false), fixed("-L-W+H", "wedge"));
    let refused = refusal(&not_allowed);
    assert_eq!(refused.1, "cannot_hold");
    assert!(
        refused
            .3
            .ends_with("wedge may not be placed in orientation -L-W+H"),
        "{}",
        refused.3
    );
    let admitted = packed(&with_fixed(
        wedge(&every_code(), false),
        fixed("-L-W+H", "wedge"),
    ));
    assert!(orientations(&admitted).contains(&"-L-W+H".to_owned()));
}

fn typed_cube(rotations: Vec<Rotation>) -> PackingRequest {
    let side = |mm: i64| Length(mm * Length::TICKS_PER_MM);
    let dimensions = Dimensions {
        length: side(10),
        width: side(10),
        height: side(10),
    };
    let mut item = Item::new("cube", dimensions, Weight(0), 1);
    item.allowed_rotations = rotations;
    PackingRequest {
        items: vec![item],
        containers: vec![Container::new("crate", dimensions)],
        config: PackingConfig {
            time_limit_ms: 60_000,
            ..PackingConfig::default()
        },
        output_length_unit: "mm".into(),
        output_weight_unit: "g".into(),
        catalog_versions_used: Vec::new(),
        currency: None,
        fixed_placements: Vec::new(),
        fixed_containers: Vec::new(),
    }
}

fn typed_refusal(request: &PackingRequest) -> (String, String, String, String) {
    match pack_request(request) {
        Err(PackError::InvalidRequest(error)) => (
            error.code().to_owned(),
            error.reason().to_owned(),
            error.field().to_owned(),
            error.to_string(),
        ),
        Err(other) => panic!("refused, but not as a request error: {other}"),
        Ok(_) => panic!("admitted"),
    }
}

#[test]
fn the_typed_path_refuses_a_signed_code_on_a_box_for_the_same_reason() {
    let refused = typed_refusal(&typed_cube(vec![Rotation::NlNwPh]));
    assert_eq!(
        (refused.1.as_str(), refused.3.ends_with(SIGNED)),
        ("invalid_value", true),
        "{refused:?}"
    );
    let mut request = typed_cube(vec![Rotation::Lwh]);
    request.fixed_placements = vec![FixedPlacement {
        item_id: "cube".into(),
        container_id: "crate".into(),
        container_instance: 1,
        position: Point::ZERO,
        rotation: Rotation::PwNlPh,
        placement_id: None,
    }];
    let refused = typed_refusal(&request);
    assert_eq!(
        refused,
        (
            "invalid_fixed_placement".into(),
            "invalid_value".into(),
            "/fixed_placements/0/orientation".into(),
            format!("invalid_fixed_placement: fixed_placements[0].orientation: {SIGNED}"),
        )
    );
    let mut hull = typed_cube(vec![Rotation::Lwh, Rotation::NlNwPh]);
    hull.items[0].shape_type = ShapeType::ConvexHull;
    hull.items[0].hull_vertices = Some(vec![[0, 0, 0], [160, 0, 0], [0, 160, 0], [0, 0, 160]]);
    assert!(pack_request(&hull).is_ok());
}

fn locked(orientation: &str, item_type: &str) -> String {
    json!([{"sequence": 1, "type": "placement_locked",
            "placement": {"item_type": item_type, "container_type": "crate",
                          "orientation": orientation}}])
    .to_string()
}

#[test]
fn a_revision_event_follows_the_items_vocabulary() {
    let hull_request = request(wedge(&every_code(), false)).to_string();
    let error = apply_events_json(&hull_request, &locked("-L-W+H", "cube")).unwrap_err();
    assert_eq!(error.kind(), RevisionErrorCode::InvalidEvent);
    assert_eq!(error.message(), format!("placement.orientation: {SIGNED}"));
    let error = apply_events_json(&hull_request, &locked("+L+W-H", "wedge")).unwrap_err();
    assert!(
        error
            .message()
            .starts_with("placement.orientation must be one of [\"LWH\""),
        "{}",
        error.message()
    );
    assert!(
        error.message().ends_with("\"-H-W-L\"]"),
        "{}",
        error.message()
    );
    let applied = apply_events_json(&hull_request, &locked("-L-W+H", "wedge")).expect("applies");
    assert!(applied.contains("-L-W+H"));
}

#[test]
fn the_validator_reads_signed_codes_only_on_hulls() {
    let hull_request = request(wedge(&every_code(), false));
    let result = packed(&hull_request);
    let issues = |request: &Value, result: &Value| {
        validate_json(&request.to_string(), &result.to_string())
            .expect("validates")
            .into_iter()
            .map(|issue| issue.code)
            .collect::<Vec<_>>()
    };
    // An unknown code on a hull is an issue, never a crash.
    let mut unknown = result.clone();
    unknown["containers"][0]["placements"][0]["orientation"] = json!("+L+W-H");
    assert!(issues(&hull_request, &unknown).contains(&"unknown_orientation".to_owned()));
    // The same signed code beside a box is a rotation the item does not allow.
    let mut box_request = hull_request.clone();
    box_request["items"][0]
        .as_object_mut()
        .unwrap()
        .remove("shape_type");
    box_request["items"][0]
        .as_object_mut()
        .unwrap()
        .remove("hull_vertices");
    let codes = issues(&box_request, &result);
    assert!(
        codes.contains(&"forbidden_rotation".to_owned()),
        "{codes:?}"
    );
    // keep_upright narrows an explicit list too.
    let mut upright = hull_request.clone();
    upright["items"][0]["keep_upright"] = json!(true);
    let mut turned = result.clone();
    for placement in turned["containers"][0]["placements"]
        .as_array_mut()
        .unwrap()
    {
        if placement["orientation"] == json!("LWH") {
            placement["orientation"] = json!("+L-W-H");
        }
    }
    let codes = issues(&upright, &turned);
    assert!(
        codes.contains(&"forbidden_rotation".to_owned()),
        "{codes:?}"
    );
}
