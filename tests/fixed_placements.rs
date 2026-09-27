//! Fixed placements: items already in a known place before the solve (docs/PLAN-REVISIONS.md).
//!
//! Mirrors `packvium-python/tests/test_fixed_placements.py`, measured through `pack_json`
//! because that is the path a request takes. Rust is held to the fixed-placement contract and
//! to validity, not to the reference's arrangement of the free items.

use std::collections::{BTreeMap, BTreeSet};

use packvium_core::{
    Container, Dimensions, FixedPlacement, IndependentValidator, Item, Length, PackingConfig,
    PackingRequest, Point, Rotation, Weight, pack_json, pack_request, rebalance_weight,
};
use serde_json::{Value, json};

const MM: i64 = Length::TICKS_PER_MM;

fn cube(quantity: u64) -> Value {
    json!({"id": "cube", "quantity": quantity, "weight": "1000",
           "dimensions": {"length": "100", "width": "100", "height": "100"}})
}

fn boxes(quantity: u64) -> Value {
    json!({"id": "box", "quantity": quantity,
           "inner_dimensions": {"length": "200", "width": "100", "height": "200"}})
}

fn fixed(x: &str, z: &str, instance: u64) -> Value {
    json!({"item_type": "cube", "container_type": "box", "container_instance": instance,
           "position": {"x": x, "z": z}, "orientation": "LWH"})
}

fn request(items: Value, containers: Value, placements: Value, configuration: Value) -> Value {
    json!({"items": items, "containers": containers, "fixed_placements": placements,
           "configuration": configuration})
}

fn default_request() -> Value {
    request(
        json!([cube(5)]),
        json!([boxes(3)]),
        json!([fixed("100", "0", 1)]),
        json!({}),
    )
}

fn pack(request: &Value) -> Value {
    serde_json::from_str(&pack_json(&unhurried(request).to_string()).expect("packs")).expect("json")
}

/// These tests are about where items go, not about the clock. The default limit is one second
/// of wall time, which a debug build under x86_64 emulation, running the tests in parallel, can
/// spend before the first free item is placed.
fn unhurried(request: &Value) -> Value {
    let mut request = request.clone();
    let configuration = request
        .as_object_mut()
        .expect("a request object")
        .entry("configuration")
        .or_insert_with(|| json!({}));
    configuration
        .as_object_mut()
        .expect("a configuration object")
        .entry("time_limit_ms")
        .or_insert(json!(60_000));
    request
}

fn refusal(request: &Value) -> String {
    pack_json(&request.to_string())
        .expect_err("refused")
        .to_string()
}

/// `(container id, item id, x ticks, z ticks)` of every placement marked fixed.
fn fixed_rows(result: &Value) -> Vec<(String, String, i64, i64)> {
    let mut rows = Vec::new();
    for container in result["containers"].as_array().unwrap() {
        for placement in container["placements"].as_array().unwrap() {
            if placement["fixed"] == Value::Bool(true) {
                rows.push((
                    container["id"].as_str().unwrap().to_owned(),
                    placement["item_id"].as_str().unwrap().to_owned(),
                    placement["position"]["x"]["ticks"].as_i64().unwrap(),
                    placement["position"]["z"]["ticks"].as_i64().unwrap(),
                ));
            }
        }
    }
    rows
}

fn placed(result: &Value) -> usize {
    result["containers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|container| container["placements"].as_array().unwrap().len())
        .sum()
}

#[test]
fn a_fixed_item_is_reported_where_the_request_put_it_in_every_profile() {
    for profile in ["fast", "balanced", "quality", "exact_small"] {
        let mut data = default_request();
        data["configuration"] = json!({"solver_profile": profile});
        let result = pack(&data);
        assert_eq!(
            fixed_rows(&result),
            vec![("box#1".into(), "cube#1".into(), 100 * MM, 0)],
            "{profile}"
        );
        assert_eq!(placed(&result), 5, "{profile}");
    }
}

#[test]
fn only_fixed_placements_carry_the_flag() {
    let result = pack(&default_request());
    let flags = result["containers"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|container| container["placements"].as_array().unwrap())
        .map(|placement| placement.get("fixed").cloned())
        .collect::<Vec<_>>();
    assert_eq!(flags.iter().filter(|flag| flag.is_some()).count(), 1);
}

#[test]
fn fixed_containers_open_first_and_free_ones_are_numbered_after_them() {
    let data = request(
        json!([cube(9)]),
        json!([boxes(3)]),
        json!([fixed("0", "0", 1), fixed("0", "0", 2)]),
        json!({}),
    );
    let ids = pack(&data)["containers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|container| container["id"].as_str().unwrap().to_owned())
        .collect::<Vec<_>>();
    assert_eq!(ids, ["box#1", "box#2", "box#3"]);
}

#[test]
fn a_fixed_container_is_kept_when_no_free_item_fits_in_it() {
    let tray = json!({"id": "tray", "quantity": 1,
                      "inner_dimensions": {"length": "100", "width": "100", "height": "100"}});
    let mut placement = fixed("0", "0", 1);
    placement["container_type"] = json!("tray");
    let result = pack(&request(
        json!([cube(3)]),
        json!([tray, boxes(3)]),
        json!([placement]),
        json!({}),
    ));
    assert_eq!(result["containers"][0]["id"], "tray#1");
    assert_eq!(
        result["containers"][0]["placements"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn a_fixed_container_survives_a_search_that_runs_out_of_effort() {
    let data = request(
        json!([cube(40)]),
        json!([boxes(20)]),
        json!([fixed("0", "0", 1), fixed("0", "0", 2)]),
        json!({"effort_budget": {"max_search_nodes": 1}}),
    );
    let containers = fixed_rows(&pack(&data))
        .into_iter()
        .map(|row| row.0)
        .collect::<Vec<_>>();
    assert_eq!(containers, ["box#1", "box#2"]);
}

#[test]
fn a_free_item_resting_on_a_fixed_one_loads_it() {
    let mut container = boxes(1);
    container["inner_dimensions"]["length"] = json!("100");
    let data = request(
        json!([cube(2)]),
        json!([container]),
        json!([fixed("0", "0", 1)]),
        json!({"solvers": ["extreme_points"]}),
    );
    let result = pack(&data);
    let placements = result["containers"][0]["placements"].as_array().unwrap();
    let fixed = placements
        .iter()
        .find(|p| p["item_id"] == "cube#1")
        .unwrap();
    let free = placements
        .iter()
        .find(|p| p["item_id"] == "cube#2")
        .unwrap();
    assert_eq!(free["position"]["z"]["ticks"], 100 * MM);
    assert_eq!(fixed["top_load"]["value"], "1000");
}

#[test]
fn every_solver_packs_around_fixed_items() {
    for solver in [
        "grid",
        "homogeneous_blocks",
        "maximal_spaces",
        "layer",
        "exact_small",
    ] {
        let mut data = default_request();
        data["configuration"] = json!({"solvers": [solver]});
        let result = pack(&data);
        assert_eq!(
            fixed_rows(&result),
            vec![("box#1".into(), "cube#1".into(), 100 * MM, 0)],
            "{solver}"
        );
        assert_eq!(placed(&result), 5, "{solver}");
    }
}

/// One change to a valid request that makes its fixed set impossible.
type Mutation = Box<dyn Fn(&mut Value)>;

#[test]
fn a_fixed_set_that_cannot_hold_is_refused_before_search() {
    let cases: Vec<(&str, Mutation)> = vec![
        (
            "overlap",
            Box::new(|d| {
                d["fixed_placements"]
                    .as_array_mut()
                    .unwrap()
                    .push(fixed("100", "0", 1))
            }),
        ),
        (
            r#"unknown item type "crate""#,
            Box::new(|d| d["fixed_placements"][0]["item_type"] = json!("crate")),
        ),
        (
            r#"unknown container type "crate""#,
            Box::new(|d| d["fixed_placements"][0]["container_type"] = json!("crate")),
        ),
        (
            "not numbered 1..1",
            Box::new(|d| d["fixed_placements"][0]["container_instance"] = json!(2)),
        ),
        (
            "outside_container: cube#1",
            Box::new(|d| d["fixed_placements"][0]["position"]["x"] = json!("150")),
        ),
        (
            "2 cube fixed, 1 requested",
            Box::new(|d| {
                d["items"][0]["quantity"] = json!(1);
                d["fixed_placements"]
                    .as_array_mut()
                    .unwrap()
                    .push(fixed("0", "0", 1));
            }),
        ),
        (
            "orientation HWL",
            Box::new(|d| {
                d["items"][0]["keep_upright"] = json!(true);
                d["fixed_placements"][0]["orientation"] = json!("HWL");
            }),
        ),
        (
            "obstacle_collision",
            Box::new(|d| {
                d["containers"][0]["obstacles"] = json!([
            {"id": "p", "origin": {"x": "150"}, "dimensions": {"length": "10", "width": "10", "height": "10"}}])
            }),
        ),
        (
            "payload",
            Box::new(|d| d["containers"][0]["max_payload"] = json!("500")),
        ),
        (
            "outside_container: cube#1",
            Box::new(|d| d["configuration"] = json!({"clearance": "1"})),
        ),
        (
            "support",
            Box::new(|d| {
                d["configuration"] = json!({"minimum_support_ratio": 1});
                d["fixed_placements"][0]["position"]["z"] = json!("50");
            }),
        ),
        (
            "2 box named, 1 available",
            Box::new(|d| {
                d["containers"][0]["quantity"] = json!(1);
                d["fixed_placements"]
                    .as_array_mut()
                    .unwrap()
                    .push(fixed("0", "0", 2));
            }),
        ),
        (
            "max_containers is 1",
            Box::new(|d| {
                d["configuration"] = json!({"max_containers": 1});
                d["fixed_placements"]
                    .as_array_mut()
                    .unwrap()
                    .push(fixed("0", "0", 2));
            }),
        ),
    ];
    for (fragment, mutate) in cases {
        let mut data = default_request();
        mutate(&mut data);
        let message = refusal(&data);
        assert!(message.contains("invalid_fixed_placement: "), "{message}");
        assert!(message.contains(fragment), "{fragment}: {message}");
    }
}

fn domain(quantity: usize, weight_g: i64, boxes: usize) -> PackingRequest {
    let side = |mm: i64| Length(mm * MM);
    let item = Item {
        id: "cube".into(),
        dimensions: Dimensions {
            length: side(100),
            width: side(100),
            height: side(100),
        },
        weight: Weight(weight_g * Weight::TICKS_PER_G),
        quantity,
        allowed_rotations: vec![Rotation::Lwh],
        stackable: true,
        must_be_on_floor: false,
        max_top_load: None,
        minimum_support_ratio: 0.0,
        group: None,
        tags: BTreeSet::new(),
        incompatible_tags: BTreeSet::new(),
        priority: 0,
        metadata: BTreeMap::new(),
        nesting_height: None,
        max_stacked_items: None,
        ground_contact_rule: None,
        stop_index: None,
        eligible_container_tags: BTreeSet::new(),
        value: None,
        shape_type: packvium_core::ShapeType::RigidCuboid,
        hull_vertices: None,
        compression_ratio_ppm: None,
        max_compression_pressure_kpa: None,
    };
    let container = Container {
        id: "box".into(),
        inner_dimensions: Dimensions {
            length: side(200),
            width: side(100),
            height: side(200),
        },
        outer_dimensions: None,
        tare_weight: Weight(0),
        max_payload: None,
        cost_minor: 0,
        quantity: Some(boxes),
        obstacles: Vec::new(),
        tags: BTreeSet::new(),
        max_items: None,
        metadata: BTreeMap::new(),
        axles: None,
        void_fill_reserve_ppm: 0,
        tag_limits: BTreeMap::new(),
        max_stack_density: None,
        rate_table: None,
        access_directions: Vec::new(),
        preloaded: Vec::new(),
    };
    PackingRequest {
        items: vec![item],
        containers: vec![container],
        config: PackingConfig {
            time_limit_ms: 60_000,
            ..PackingConfig::default()
        },
        output_length_unit: "mm".into(),
        output_weight_unit: "g".into(),
        catalog_versions_used: Vec::new(),
        fixed_placements: Vec::new(),
        fixed_containers: Vec::new(),
    }
}

fn at(x: i64, y: i64, z: i64, instance: usize) -> FixedPlacement {
    FixedPlacement {
        item_id: "cube".into(),
        container_id: "box".into(),
        container_instance: instance,
        position: Point {
            x: x * MM,
            y: y * MM,
            z: z * MM,
        },
        rotation: Rotation::Lwh,
    }
}

#[test]
fn a_library_caller_gets_the_same_admission_as_the_json_api() {
    let mut request = domain(5, 1000, 3);
    request.fixed_placements = vec![at(100, 0, 0, 1), at(100, 0, 0, 1)];
    let error = pack_request(&request).expect_err("refused").to_string();
    assert!(error.contains("invalid_fixed_placement"), "{error}");
}

#[test]
fn the_validator_catches_a_moved_or_missing_fixed_item() {
    let mut request = domain(5, 1000, 3);
    request.fixed_placements = vec![at(100, 0, 0, 1)];
    let result = pack_request(&request).expect("packs");
    request.fixed_placements = vec![at(0, 0, 0, 1)];
    let codes = |request: &PackingRequest| {
        IndependentValidator
            .validate(request, &result)
            .issues
            .into_iter()
            .map(|issue| issue.code)
            .collect::<Vec<_>>()
    };
    assert_eq!(
        codes(&request),
        ["fixed_placement_moved", "unexpected_fixed_placement"]
    );
    request.fixed_placements = vec![at(100, 0, 0, 1), at(0, 0, 0, 3)];
    assert!(codes(&request).contains(&"fixed_container_missing".to_owned()));
}

#[test]
fn rebalancing_never_moves_a_fixed_item() {
    let mut request = domain(5, 5000, 2);
    request.fixed_placements = vec![
        at(0, 0, 0, 1),
        at(100, 0, 0, 1),
        at(0, 0, 100, 1),
        at(0, 0, 0, 2),
    ];
    let result = pack_request(&request).expect("packs");
    assert_eq!(result.containers.len(), 2);
    let rebalanced = rebalance_weight(&request, &result, 64);
    for fixed in ["cube#1", "cube#2", "cube#3", "cube#4"] {
        assert!(
            rebalanced.moves.iter().all(|m| m.item_id != fixed),
            "{fixed} moved"
        );
    }
    let mut after = result.clone();
    after.containers = rebalanced.containers;
    assert!(IndependentValidator.validate(&request, &after).valid);
}

fn entry() -> Value {
    json!({"item_type": "cube", "container_type": "box", "position": {"x": "100"},
           "orientation": "LWH"})
}

fn with(key: &str, value: Value) -> Value {
    let mut entry = entry();
    entry[key] = value;
    entry
}

fn admission(placements: Value) -> Result<String, String> {
    let mut data = default_request();
    data["configuration"] = json!({"solver_profile": "fast"});
    data["fixed_placements"] = placements;
    pack_json(&data.to_string()).map_err(|error| match error {
        packvium_core::PackError::InvalidRequest(refusal)
            if refusal.code() == "invalid_fixed_placement" =>
        {
            refusal.to_string()
        }
        other => panic!("not an admission refusal: {other}"),
    })
}

#[test]
fn a_fixed_placement_that_is_not_the_schema_shape_is_refused_by_name() {
    let mut no_orientation = entry();
    no_orientation
        .as_object_mut()
        .unwrap()
        .remove("orientation");
    let cases = [
        (json!("x"), "fixed_placements is a list"),
        (json!([5]), "fixed_placements[0] is an object"),
        (
            json!([with("zone", json!("a")),]),
            r#"fixed_placements[0] does not carry ["zone"]"#,
        ),
        (
            json!([no_orientation]),
            r#"fixed_placements[0] needs ["orientation"]"#,
        ),
        (
            json!([entry(), with("item_type", json!(""))]),
            "fixed_placements[1].item_type is a non-empty string",
        ),
        (
            json!([with("container_type", json!(5))]),
            "fixed_placements[0].container_type is a non-empty string",
        ),
        (
            json!([with("orientation", json!("XYZ"))]),
            "fixed_placements[0].orientation is one of the six codes",
        ),
        (
            json!([with("position", json!(["100", "0", "0"]))]),
            "fixed_placements[0].position is a point object",
        ),
        (
            json!([with("position", Value::Null)]),
            "fixed_placements[0].position is a point object",
        ),
        (
            json!([with("position", json!({"x": "100", "w": "5", "a": "1"}))]),
            r#"fixed_placements[0].position does not carry ["a","w"]"#,
        ),
        (
            json!([with("position", json!({"y": true}))]),
            "fixed_placements[0].position.y is a measure",
        ),
        (
            json!([with("position", json!({"z": [1]}))]),
            "fixed_placements[0].position.z is a measure",
        ),
        (
            json!([with("item_type", json!("crate"))]),
            r#"unknown item type "crate""#,
        ),
    ];
    for (placements, detail) in cases {
        assert_eq!(
            admission(placements.clone()),
            Err(format!("invalid_fixed_placement: {detail}")),
            "{placements}"
        );
    }
    for instance in [
        json!("1"),
        json!(true),
        json!(1.5),
        json!(0),
        json!(9_007_199_254_740_992_u64),
    ] {
        assert_eq!(
            admission(json!([with("container_instance", instance.clone())])),
            Err(
                "invalid_fixed_placement: fixed_placements[0].container_instance counts from 1"
                    .into()
            ),
            "{instance}"
        );
    }
}

#[test]
fn an_integral_float_instance_and_an_absent_or_null_list_are_admitted() {
    assert!(admission(json!([with("container_instance", json!(1.0))])).is_ok());
    assert!(admission(Value::Null).is_ok());
    let mut data = default_request();
    data.as_object_mut().unwrap().remove("fixed_placements");
    assert!(pack_json(&data.to_string()).is_ok());
}

#[test]
fn a_box_placed_past_the_integer_range_is_outside_not_wrapped_inside() {
    assert_eq!(
        admission(json!([with("position", json!({"x": "576460752303423"}))])),
        Err("invalid_fixed_placement: outside_container: cube#1".into())
    );
    let mut data = default_request();
    data["containers"][0]["obstacles"] = json!([{"id": "far", "origin": {"x": "576460752303423"},
        "dimensions": {"length": "10", "width": "10", "height": "10"}}]);
    let error = pack_json(&data.to_string())
        .expect_err("refused")
        .to_string();
    assert!(error.contains("obstacle far outside box"), "{error}");
}

#[test]
fn an_instance_gap_is_spelled_as_a_json_list() {
    let placements = json!([fixed("0", "0", 2), fixed("100", "0", 3)]);
    assert_eq!(
        admission(placements),
        Err("invalid_fixed_placement: box instances [2,3] are not numbered 1..2".into())
    );
}

#[test]
fn incompatible_fixed_items_are_refused_by_the_ordinary_validator() {
    let mut data = default_request();
    data["items"] = json!([
        {"id": "acid", "quantity": 1, "weight": "1000", "tags": ["acid"],
         "dimensions": {"length": "100", "width": "100", "height": "100"}},
        {"id": "base", "quantity": 1, "weight": "1000", "incompatible_tags": ["acid"],
         "dimensions": {"length": "100", "width": "100", "height": "100"}}
    ]);
    data["fixed_placements"] = json!([
        {"item_type": "acid", "container_type": "box", "position": {"x": "0"}, "orientation": "LWH"},
        {"item_type": "base", "container_type": "box", "position": {"x": "100"}, "orientation": "LWH"}
    ]);
    assert_eq!(
        refusal(&data),
        "invalid_fixed_placement: incompatible_items: acid#1: acid is incompatible with base"
    );
    data.as_object_mut().unwrap().remove("fixed_placements");
    let result = pack(&data);
    assert_eq!(result["containers"].as_array().unwrap().len(), 2);
}
