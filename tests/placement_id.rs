//! `placement_id` (1.10): the canonical JSON of `[container id, x, y, z, item type,
//! orientation]`, a pure function of a placement.
//!
//! Mirrors `packvium-python/tests/test_placement_id.py`: emitted on every result placement,
//! recomputed by the validator, restated optionally by a fixed placement, copied into the
//! execution plan, kept out of the closed operational artifact, and carried by a revision lock.

use packvium_core::artifacts::build_artifact_json;
use packvium_core::execution::build_plan_json;
use packvium_core::revisions::apply_events_json;
use packvium_core::{
    Container, Dimensions, FixedPlacement, Item, Length, PackError, PackingConfig, PackingRequest,
    Point, Rotation, Weight, pack_json, pack_request, validate_json,
};
use serde_json::{Value, json};

const MM: i64 = Length::TICKS_PER_MM;

fn request(container_id: &str) -> Value {
    json!({
        "configuration": {"solver_profile": "balanced", "time_limit_ms": 300_000, "alternatives": 3},
        "items": [{"id": "cube", "quantity": 4, "weight": "1000",
                   "dimensions": {"length": "100", "width": "100", "height": "100"}}],
        "containers": [{"id": container_id, "quantity": 3,
                        "inner_dimensions": {"length": "200", "width": "100", "height": "100"}}],
    })
}

fn pack(request: &Value) -> Value {
    serde_json::from_str(&pack_json(&request.to_string()).expect("packs")).expect("json")
}

fn ident(container_id: &str, x: i64, y: i64, z: i64, item_type: &str, orientation: &str) -> String {
    json!([container_id, x, y, z, item_type, orientation]).to_string()
}

fn expected_id(container: &Value, placement: &Value) -> String {
    let ticks = |axis: &str| {
        placement["position"][axis]["ticks"]
            .as_i64()
            .expect("ticks")
    };
    ident(
        container["id"].as_str().expect("id"),
        ticks("x"),
        ticks("y"),
        ticks("z"),
        placement["item_type"].as_str().expect("item_type"),
        placement["orientation"].as_str().expect("orientation"),
    )
}

fn documents(result: &Value) -> Vec<&Value> {
    let mut all = vec![result];
    all.extend(result["alternatives"].as_array().expect("alternatives"));
    all
}

#[test]
fn every_placement_of_the_answer_and_its_alternatives_carries_its_id() {
    for container_id in ["box", "a@b#c", "böx-箱", "q\"uo\\te\n\u{2028}\u{1}"] {
        let request = request(container_id);
        let result = pack(&request);
        let mut seen = 0;
        for document in documents(&result) {
            for container in document["containers"].as_array().expect("containers") {
                for placement in container["placements"].as_array().expect("placements") {
                    assert_eq!(placement["placement_id"], expected_id(container, placement));
                    seen += 1;
                }
            }
        }
        assert!(seen >= 4, "{container_id}: {seen} placements");
        let issues = validate_json(&request.to_string(), &result.to_string()).expect("judged");
        assert!(issues.is_empty(), "{container_id}: {issues:?}");
    }
}

fn fixed_request(placement_id: Value) -> Value {
    let mut data = request("box");
    data["fixed_placements"] = json!([{
        "item_type": "cube", "container_type": "box", "container_instance": 1,
        "position": {"x": "100"}, "orientation": "LWH", "placement_id": placement_id,
    }]);
    data
}

fn refusal(data: &Value) -> (String, String, String) {
    match pack_json(&data.to_string()).expect_err("refused") {
        PackError::InvalidRequest(error) => (
            error.reason().to_owned(),
            error.field().to_owned(),
            error.to_string(),
        ),
        other => panic!("not a request refusal: {other}"),
    }
}

#[test]
fn a_fixed_placement_may_restate_the_id_its_result_placement_carries() {
    let id = ident("box#1", 100 * MM, 0, 0, "cube", "LWH");
    for stated in [json!(id), Value::Null] {
        let result = pack(&fixed_request(stated));
        let fixed = result["containers"]
            .as_array()
            .expect("containers")
            .iter()
            .flat_map(|container| container["placements"].as_array().expect("placements"))
            .find(|placement| placement["fixed"] == json!(true))
            .expect("the fixed item");
        assert_eq!(fixed["placement_id"], json!(id));
    }
}

#[test]
fn a_fixed_placement_id_naming_another_placement_or_not_a_string_is_refused() {
    let (reason, field, message) = refusal(&fixed_request(json!(ident(
        "box#1", 0, 0, 0, "cube", "LWH"
    ))));
    assert_eq!(
        (reason.as_str(), field.as_str()),
        ("invalid_value", "/fixed_placements/0/placement_id")
    );
    let expected = json!(ident("box#1", 100 * MM, 0, 0, "cube", "LWH")).to_string();
    assert!(
        message.ends_with(&format!(
            "fixed_placements[0].placement_id must be {expected}"
        )),
        "{message}"
    );
    let (reason, field, message) = refusal(&fixed_request(json!(1)));
    assert_eq!(
        (reason.as_str(), field.as_str()),
        ("malformed", "/fixed_placements/0/placement_id")
    );
    assert!(
        message.ends_with("fixed_placements[0].placement_id is a string"),
        "{message}"
    );
}

#[test]
fn a_coordinate_past_the_exact_range_leaves_the_id_unjudged() {
    let mut data = fixed_request(json!(ident("box#2", 0, 0, 0, "cube", "LWH")));
    data["fixed_placements"][0]["position"] =
        json!({"x": {"value": "9007199254740992", "unit": "ticks"}});
    let (reason, _, message) = refusal(&data);
    assert_eq!(reason, "cannot_hold", "{message}");
    assert!(message.contains("outside_container"), "{message}");
}

fn dims(length: i64, width: i64, height: i64) -> Dimensions {
    Dimensions {
        length: Length(length * MM),
        width: Length(width * MM),
        height: Length(height * MM),
    }
}

fn typed(placement_id: Option<String>) -> PackingRequest {
    let item = Item::new("cube", dims(100, 100, 100), Weight(1000), 2);
    let mut container = Container::new("box", dims(200, 100, 100));
    container.quantity = Some(1);
    PackingRequest {
        items: vec![item],
        containers: vec![container],
        config: PackingConfig::default(),
        output_length_unit: "mm".into(),
        output_weight_unit: "g".into(),
        catalog_versions_used: Vec::new(),
        currency: None,
        fixed_placements: vec![FixedPlacement {
            item_id: "cube".into(),
            container_id: "box".into(),
            container_instance: 1,
            position: Point {
                x: 100 * MM,
                y: 0,
                z: 0,
            },
            rotation: Rotation::Lwh,
            placement_id,
        }],
        fixed_containers: Vec::new(),
    }
}

#[test]
fn a_library_caller_is_held_to_the_same_id_rule() {
    let derived = typed(None).fixed_placements[0].derived_placement_id();
    assert_eq!(derived, ident("box#1", 100 * MM, 0, 0, "cube", "LWH"));
    assert!(pack_request(&typed(Some(derived))).is_ok());
    let error = pack_request(&typed(Some("[]".into()))).expect_err("refused");
    let PackError::InvalidRequest(error) = error else {
        panic!("not a request refusal: {error}");
    };
    assert_eq!(
        (error.reason(), error.field()),
        ("invalid_value", "/fixed_placements/0/placement_id")
    );
}

#[test]
fn a_typed_coordinate_past_the_exact_range_leaves_the_id_unjudged() {
    let mut request = typed(Some("[]".into()));
    request.fixed_placements[0].position.x = 1 << 53;
    let error = pack_request(&request).expect_err("refused").to_string();
    assert!(error.contains("outside_container"), "{error}");
}

#[test]
fn the_plan_copies_each_id_and_the_artifact_leaves_it_out() {
    let data = request("a@b#c");
    let result = pack(&data);
    let plan: Value =
        serde_json::from_str(&build_plan_json(&result.to_string(), "{}").expect("a plan"))
            .expect("json");
    let references: Vec<&Value> = plan["containers"]
        .as_array()
        .expect("containers")
        .iter()
        .flat_map(|container| container["steps"].as_array().expect("steps"))
        .map(|step| &step["placement"])
        .collect();
    assert!(!references.is_empty());
    for reference in &references {
        let index = reference["container_index"].as_u64().expect("index") as usize;
        assert!(
            reference["placement_id"]
                .as_str()
                .expect("an id")
                .starts_with(&format!("[{}", result["containers"][index]["id"])),
            "{reference}"
        );
    }
    let artifact =
        build_artifact_json(&data.to_string(), &result.to_string(), "{}").expect("an artifact");
    assert!(!artifact.contains("placement_id"), "{artifact}");
}

#[test]
fn a_plan_refuses_an_id_that_is_not_a_non_empty_string() {
    let mut result = pack(&request("box"));
    result["containers"][0]["placements"][0]["placement_id"] = json!("");
    let error = build_plan_json(&result.to_string(), "{}").expect_err("refused");
    assert_eq!(error.0, "placement_id '' is not a non-empty string");
    result["containers"][0]["placements"][0]["placement_id"] = json!(7);
    let error = build_plan_json(&result.to_string(), "{}").expect_err("refused");
    assert_eq!(error.0, "placement_id 7 is not a non-empty string");
}

fn lock(placement_id: Value) -> Value {
    json!({"type": "placement_locked", "sequence": 1, "placement": {
        "item_type": "cube", "container_type": "box", "position": {"x": "0"},
        "orientation": "LWH", "placement_id": placement_id,
    }})
}

#[test]
fn a_lock_may_quote_its_id_and_is_still_one_box() {
    let mut data = request("box");
    data["fixed_placements"] = json!([{"item_type": "cube", "container_type": "box",
                                       "position": {"x": "0"}, "orientation": "LWH"}]);
    let id = ident("box#1", 0, 0, 0, "cube", "LWH");
    let derived: Value = serde_json::from_str(
        &apply_events_json(&data.to_string(), &json!([lock(json!(id))]).to_string())
            .expect("applies"),
    )
    .expect("json");
    assert_eq!(
        derived["fixed_placements"].as_array().expect("fixed").len(),
        1
    );
    let refused = apply_events_json(&data.to_string(), &json!([lock(json!(1))]).to_string())
        .expect_err("refused");
    assert_eq!(
        (refused.code(), refused.message()),
        ("invalid_event", "placement.placement_id is a string")
    );
}
