//!: flat bags, admitted by the girth rule `a + c <= L` and `b + c <= W`.
//!
//! Mirrors `packvium-python/tests/test_flat_bag.py`, measured through `pack_json` because that
//! is the path a request takes; the refusal texts are asserted whole because they are compared
//! across engines.

use packvium_core::{
    Container, DeformableBoundary, Dimensions, Item, ItemInstance, Length, PackError,
    PackedContainer, Placement, Point, Rotation, Weight, pack_json,
};
use serde_json::{Value, json};

const MM: i64 = 16_000;

fn books(quantity: u64) -> Value {
    json!({"id": "book", "quantity": quantity, "weight": "300 g", "allowed_rotations": ["LWH"],
           "dimensions": {"length": "200", "width": "100", "height": "20"}})
}

fn mailer(length: &str, width: &str, extra: Value) -> Value {
    let mut container = json!({"id": "mailer", "deformable_boundary": {"kind": "flat_bag"},
        "inner_dimensions": {"length": length, "width": width, "height": "100"}});
    for (key, value) in extra.as_object().expect("extra fields are an object") {
        container[key] = value.clone();
    }
    container
}

fn request(items: Vec<Value>, containers: Vec<Value>, configuration: Value) -> Value {
    let mut base = json!({"solver_profile": "fast", "time_limit_ms": 60000,
                          "effort_budget": {"max_search_nodes": 100000}});
    for (key, value) in configuration
        .as_object()
        .expect("configuration is an object")
    {
        base[key] = value.clone();
    }
    json!({"items": items, "containers": containers, "configuration": base})
}

fn solve(request: &Value) -> Value {
    serde_json::from_str(&pack_json(&request.to_string()).expect("the request packs"))
        .expect("the result is JSON")
}

fn extents_mm(result: &Value) -> Vec<[i64; 3]> {
    result["containers"]
        .as_array()
        .expect("containers")
        .iter()
        .map(|container| {
            ["length", "width", "height"].map(|side| {
                container["achieved_dimensions"][side]["ticks"]
                    .as_i64()
                    .expect("achieved ticks")
                    / MM
            })
        })
        .collect()
}

fn mm(length: i64, width: i64, height: i64) -> Dimensions {
    Dimensions {
        length: Length(length * MM),
        width: Length(width * MM),
        height: Length(height * MM),
    }
}

#[test]
fn the_worked_examples_of_the_oracle() {
    let mut bag = bag_container();
    assert!(bag.girth_admits([200 * MM, 100 * MM, 40 * MM]));
    assert!(!bag.girth_admits([200 * MM, 100 * MM, 40 * MM + 1]));
    bag.inner_dimensions = mm(300, 300, 100);
    assert!(!bag.fits_alone(mm(250, 150, 80)));
    assert!(bag.fits_alone(mm(220, 150, 80)));
    bag.deformable_boundary = None;
    assert!(bag.girth_admits([i64::MAX, i64::MAX, i64::MAX]));
}

/// Books stacked at these heights in one 240 x 140 x 100 mm bag, built field by field.
fn stacked_books(levels: &[i64]) -> PackedContainer {
    let item = Item {
        id: "book".into(),
        dimensions: mm(200, 100, 20),
        weight: Weight(300),
        quantity: levels.len(),
        ..empty_bag().as_item()
    };
    let placements = levels
        .iter()
        .enumerate()
        .map(|(index, z)| {
            let origin = Point {
                x: 0,
                y: 0,
                z: z * MM,
            };
            Placement {
                instance: ItemInstance {
                    item: item.clone(),
                    sequence: index + 1,
                },
                position: origin,
                rotation: Rotation::Lwh,
                dimensions: item.dimensions,
                envelope_origin: origin,
                envelope_dimensions: item.dimensions,
                support_ratio: 1.0,
                top_load: Weight(0),
                fixed: false,
            }
        })
        .collect();
    PackedContainer {
        placements,
        ..empty_bag()
    }
}

fn empty_bag() -> PackedContainer {
    PackedContainer {
        container: bag_container(),
        sequence: 1,
        placements: Vec::new(),
        lattice_summary: None,
        lattice_items: Vec::new(),
    }
}

fn bag_container() -> Container {
    Container {
        id: "mailer".into(),
        inner_dimensions: mm(240, 140, 100),
        outer_dimensions: None,
        tare_weight: Weight(0),
        max_payload: None,
        cost_minor: 0,
        quantity: None,
        obstacles: Vec::new(),
        tags: Default::default(),
        max_items: None,
        metadata: Default::default(),
        axles: None,
        void_fill_reserve_ppm: 0,
        tag_limits: Default::default(),
        max_stack_density: None,
        rate_table: None,
        access_directions: Vec::new(),
        preloaded: Vec::new(),
        deformable_boundary: Some(DeformableBoundary::FlatBag),
        made_to_measure: None,
    }
}

#[test]
fn a_nested_bag_is_an_item_of_its_achieved_size() {
    let packed = stacked_books(&[0, 20]);
    assert_eq!(packed.achieved_dimensions(), Some(mm(200, 100, 40)));
    assert_eq!(packed.billed_dimensions(), mm(200, 100, 40));
    assert_eq!(packed.as_item().dimensions, mm(200, 100, 40));
    let mut rigid = packed;
    rigid.container.deformable_boundary = None;
    assert_eq!(rigid.achieved_dimensions(), None);
    assert_eq!(rigid.billed_dimensions(), mm(240, 140, 100));
}

#[test]
fn a_malformed_bag_is_refused_with_its_pointer() {
    let cases = [
        (
            json!({"deformable_boundary": "flat_bag"}),
            "/deformable_boundary",
            "wrong_type",
            "must be an object",
        ),
        (
            json!({"deformable_boundary": {}}),
            "/deformable_boundary/kind",
            "missing_field",
            "is required",
        ),
        (
            json!({"deformable_boundary": {"kind": "pouch"}}),
            "/deformable_boundary/kind",
            "not_allowed",
            "must be one of [\"flat_bag\"]",
        ),
        (
            json!({"deformable_boundary": {"kind": "flat_bag", "seam": 1}}),
            "/deformable_boundary/seam",
            "not_allowed",
            "is not a known field",
        ),
        (
            json!({"made_to_measure": {}}),
            "/made_to_measure",
            "not_allowed",
            "cannot be combined with deformable_boundary",
        ),
        (
            json!({"axles": [{"position": "20"}, {"position": "180"}]}),
            "/axles",
            "not_allowed",
            "cannot be combined with deformable_boundary",
        ),
        (
            json!({"void_fill_reserve_ratio": 0.5}),
            "/void_fill_reserve_ratio",
            "not_allowed",
            "cannot be combined with deformable_boundary",
        ),
    ];
    for (extra, field, reason, detail) in cases {
        let request = request(vec![books(1)], vec![mailer("300", "200", extra)], json!({}));
        match pack_json(&request.to_string()) {
            Err(PackError::InvalidRequest(error)) => assert_eq!(
                (error.field(), error.reason(), error.detail()),
                (format!("/containers/0{field}").as_str(), reason, detail)
            ),
            other => panic!("{field} was not refused as a request error: {other:?}"),
        }
    }
    let inert = mailer(
        "240",
        "140",
        json!({"obstacles": [], "void_fill_reserve_ratio": 0}),
    );
    assert_eq!(
        extents_mm(&solve(&request(vec![books(1)], vec![inert], json!({})))),
        [[200, 100, 20]]
    );
}

#[test]
fn girth_not_the_frame_decides_how_high_a_bag_stacks() {
    let single = mailer("240", "140", json!({"quantity": 1}));
    let result = solve(&request(vec![books(3)], vec![single], json!({})));
    assert_eq!(extents_mm(&result), [[200, 100, 40]]);
    assert_eq!(result["unpacked_items"].as_array().map(Vec::len), Some(1));
    for profile in ["fast", "balanced", "quality", "exact_small"] {
        let opened = solve(&request(
            vec![books(3)],
            vec![mailer("240", "140", json!({}))],
            json!({"solver_profile": profile}),
        ));
        let mut extents = extents_mm(&opened);
        extents.sort();
        assert_eq!(extents, [[200, 100, 20], [200, 100, 40]], "{profile}");
    }
    let compact = solve(&request(
        vec![books(3)],
        vec![mailer("240", "140", json!({}))],
        json!({"require_placement_coordinates": false}),
    ));
    assert_eq!(compact["containers"].as_array().map(Vec::len), Some(2));
}

#[test]
fn an_item_no_rotation_admits_is_proven_unplaceable() {
    let carton =
        json!({"id": "carton", "dimensions": {"length": "250", "width": "150", "height": "80"}});
    let result = solve(&request(
        vec![carton.clone()],
        vec![mailer("300", "300", json!({}))],
        json!({}),
    ));
    assert_eq!(
        result["unpacked_items"][0]["reason"],
        "no_compatible_container_dimensions"
    );
    let mut restricted = carton;
    restricted["allowed_rotations"] = json!(["LHW"]);
    let result = solve(&request(
        vec![restricted],
        vec![mailer("330", "300", json!({}))],
        json!({}),
    ));
    assert_eq!(result["unpacked_items"][0]["reason"], "rotation_restricted");
}

#[test]
fn a_bag_is_billed_on_its_achieved_package() {
    let rate = json!({"weight_brackets_g": [500, 1000, 2000], "prices_minor": [400, 700, 1100]});
    let carton = json!({"id": "carton", "rate_table": rate,
        "inner_dimensions": {"length": "300", "width": "200", "height": "150"}});
    for (objective, expected) in [
        ("lowest_landed_cost", 400),
        ("shipping_cost", 300_i64 * 8_000_000),
    ] {
        for profile in ["fast", "exact_small", "quality"] {
            let result = solve(&request(
                vec![books(1)],
                vec![
                    carton.clone(),
                    mailer("300", "250", json!({"rate_table": rate})),
                ],
                json!({"objective": objective, "solver_profile": profile,
                       "dimensional_weight_divisor": 5000, "dimensional_weight_length_unit": "cm",
                       "dimensional_weight_weight_unit": "kg"}),
            ));
            assert_eq!(
                result["containers"][0]["container_type"], "mailer",
                "{objective} {profile}"
            );
            assert_eq!(result["score"][1], expected, "{objective} {profile}");
        }
    }
}

#[test]
fn a_fixed_set_past_its_girth_cannot_hold() {
    let fixed = [0, 20, 40].map(|z| {
        json!({"item_type": "book", "container_type": "mailer", "orientation": "LWH",
                        "position": {"x": "0", "y": "0", "z": z.to_string()}})
    });
    let mut request = request(
        vec![books(3)],
        vec![mailer("240", "140", json!({}))],
        json!({}),
    );
    request["fixed_placements"] = json!(fixed);
    match pack_json(&request.to_string()) {
        Err(error) => assert_eq!(
            error.to_string(),
            "invalid_fixed_placement: bag_girth_exceeded: mailer#1"
        ),
        Ok(_) => panic!("the fixed set was admitted"),
    }
}
