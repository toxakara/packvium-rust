//! 1.10: gusseted bags, admitted by the flat-bag rule with the width girth `b + c <= W + G`.
//!
//! Mirrors `packvium-python/tests/test_gusseted_bag.py` through `pack_json`, `pack_request`,
//! `validate_json` and the model's own oracle; refusal texts are asserted whole because they
//! are compared across engines.

use packvium_core::{
    Container, DeformableBoundary, Dimensions, IndependentValidator, Item, ItemInstance, Length,
    PackError, PackedContainer, PackingConfig, PackingRequest, Placement, Point, Rotation, Weight,
    pack_json, pack_request, validate_json,
};
use serde_json::{Value, json};

const MM: i64 = Length::TICKS_PER_MM;

fn mm(length: i64, width: i64, height: i64) -> Dimensions {
    Dimensions {
        length: Length(length * MM),
        width: Length(width * MM),
        height: Length(height * MM),
    }
}

fn gusseted(depth_ticks: i64) -> Option<DeformableBoundary> {
    Some(DeformableBoundary::GussetedBag {
        side_gusset_depth: Length(depth_ticks),
    })
}

fn pouch(depth_ticks: i64) -> Container {
    Container {
        deformable_boundary: gusseted(depth_ticks),
        ..Container::new("pouch", mm(300, 150, 100))
    }
}

fn book(quantity: usize) -> Item {
    Item {
        allowed_rotations: vec![Rotation::Lwh],
        ..Item::new("book", mm(200, 100, 20), Weight(300), quantity)
    }
}

fn request(quantity: u64, boundary: Value) -> Value {
    json!({
        "items": [{"id": "book", "quantity": quantity, "weight": "300 g", "allowed_rotations": ["LWH"],
                   "dimensions": {"length": "200", "width": "100", "height": "20"}}],
        "containers": [{"id": "pouch", "quantity": 1, "deformable_boundary": boundary,
                        "inner_dimensions": {"length": "300", "width": "150", "height": "100"}}],
        "configuration": {"solver_profile": "fast", "time_limit_ms": 60000,
                          "effort_budget": {"max_search_nodes": 100000}}
    })
}

fn gusset(depth: Value) -> Value {
    json!({"kind": "gusseted_bag", "side_gusset_depth": depth})
}

fn solve(request: &Value) -> Value {
    serde_json::from_str(&pack_json(&request.to_string()).expect("the request packs"))
        .expect("the result is JSON")
}

fn placed_counts(result: &Value) -> Vec<usize> {
    result["containers"]
        .as_array()
        .expect("containers")
        .iter()
        .map(|container| {
            container["placements"]
                .as_array()
                .expect("placements")
                .len()
        })
        .collect()
}

fn typed(container: Container, items: Vec<Item>) -> PackingRequest {
    PackingRequest {
        items,
        containers: vec![container],
        config: PackingConfig::default(),
        output_length_unit: "mm".into(),
        output_weight_unit: "g".into(),
        catalog_versions_used: Vec::new(),
        currency: None,
        fixed_placements: Vec::new(),
        fixed_containers: Vec::new(),
    }
}

/// Books stacked at these heights from the origin of `container`.
fn stacked(container: Container, levels: &[i64]) -> PackedContainer {
    let item = book(levels.len());
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
        container,
        sequence: 1,
        placements,
        lattice_summary: None,
        lattice_items: Vec::new(),
    }
}

#[test]
fn the_gusset_widens_only_the_width_girth() {
    let flat = Container {
        deformable_boundary: Some(DeformableBoundary::FlatBag),
        ..Container::new("pouch", mm(300, 150, 100))
    };
    assert!(!flat.girth_admits([200 * MM, 100 * MM, 60 * MM]));
    assert_eq!(flat.girth_width_limit(), 150 * MM);
    let bag = pouch(10 * MM);
    assert!(bag.is_flat_bag());
    assert_eq!(bag.girth_width_limit(), 160 * MM);
    assert!(bag.girth_admits([200 * MM, 100 * MM, 60 * MM]));
    assert!(!bag.girth_admits([200 * MM, 100 * MM, 60 * MM + 1]));
    assert!(!pouch(150 * MM).girth_admits([241 * MM, 0, 60 * MM]));
    assert_eq!(Container::new("box", mm(1, 1, 1)).girth_width_limit(), MM);
}

#[test]
fn a_single_box_fits_a_gusseted_bag_its_flat_twin_refuses() {
    let inner = mm(200, 150, 100);
    let flat = Container {
        deformable_boundary: Some(DeformableBoundary::FlatBag),
        ..Container::new("pouch", inner)
    };
    let bag = Container {
        deformable_boundary: gusseted(10 * MM),
        ..Container::new("pouch", inner)
    };
    assert!(!flat.fits_alone(mm(100, 100, 60)));
    assert!(bag.fits_alone(mm(100, 100, 60)));
    let deep = Container {
        deformable_boundary: gusseted(150 * MM),
        ..Container::new("pouch", inner)
    };
    assert!(!deep.fits_alone(mm(100, 160, 10)));
}

#[test]
fn the_typed_path_refuses_a_gusset_off_its_range() {
    for (depth, detail) in [
        (0, "container pouch side_gusset_depth must be positive"),
        (-1, "container pouch side_gusset_depth must be positive"),
        (
            150 * MM + 1,
            "container pouch side_gusset_depth exceeds inner_dimensions width",
        ),
    ] {
        match pack_request(&typed(pouch(depth), vec![book(1)])) {
            Err(PackError::InvalidInput(message)) => assert_eq!(message, detail),
            other => panic!("depth {depth} was not refused: {other:?}"),
        }
    }
    let result = pack_request(&typed(pouch(150 * MM), vec![book(1)])).expect("admitted");
    assert!(result.unpacked.is_empty());
}

#[test]
fn the_json_path_reads_the_depth_in_the_request_unit() {
    let mut body = request(3, gusset(json!({"value": "1", "unit": "cm"})));
    body["units"] = json!({"length": "mm"});
    assert_eq!(solve(&body)["unpacked_items"], json!([]));
}

#[test]
fn the_gusset_admits_the_layer_the_flat_bag_refuses() {
    let flat = solve(&request(4, json!({"kind": "flat_bag"})));
    let wide = solve(&request(4, gusset(json!("25"))));
    assert_eq!(placed_counts(&flat), [2]);
    assert_eq!(placed_counts(&wide), [3]);
    assert_eq!(
        wide["containers"][0]["achieved_dimensions"]["height"]["value"],
        "60"
    );
    for profile in ["exact_small", "quality"] {
        let mut body = request(4, gusset(json!("25")));
        body["configuration"]["solver_profile"] = json!(profile);
        assert_eq!(placed_counts(&solve(&body)), [3], "{profile}");
    }
}

#[test]
fn an_item_only_the_gusset_admits_is_not_proven_unfit() {
    let mut body = request(1, json!({"kind": "flat_bag"}));
    body["items"] = json!([{"id": "brick", "allowed_rotations": ["LWH"],
                            "dimensions": {"length": "100", "width": "100", "height": "60"}}]);
    body["containers"][0]["inner_dimensions"]["length"] = json!("200");
    assert_eq!(
        solve(&body)["unpacked_items"][0]["reason"],
        "no_compatible_container_dimensions"
    );
    body["containers"][0]["deformable_boundary"] = gusset(json!("10"));
    assert_eq!(solve(&body)["unpacked_items"], json!([]));
}

#[test]
fn a_gusseted_bag_is_billed_on_its_achieved_package() {
    let packed = stacked(pouch(10 * MM), &[0]);
    assert_eq!(packed.billed_dimensions(), mm(200, 100, 20));
    assert_eq!(packed.achieved_dimensions(), Some(mm(200, 100, 20)));
}

#[test]
fn the_validator_reads_the_gusset() {
    let validator = IndependentValidator;
    let fits = stacked(pouch(10 * MM), &[0, 20, 40]);
    let report = validator.validate_containers(&typed(pouch(10 * MM), vec![book(3)]), &[fits]);
    assert!(report.issues.is_empty(), "{:?}", report.issues);
    let narrow = stacked(pouch(9 * MM), &[0, 20, 40]);
    let report = validator.validate_containers(&typed(pouch(9 * MM), vec![book(3)]), &[narrow]);
    let codes: Vec<_> = report
        .issues
        .iter()
        .map(|issue| (issue.code.as_str(), issue.message.as_str()))
        .collect();
    assert_eq!(codes, [("bag_girth_exceeded", "pouch#1")]);
}

#[test]
fn the_public_validator_names_the_gusset_only_when_there_is_one() {
    let body = request(3, gusset(json!("10")));
    let result = solve(&body);
    assert!(
        validate_json(&body.to_string(), &result.to_string())
            .expect("judged")
            .is_empty()
    );
    let mut narrow = body.clone();
    narrow["containers"][0]["deformable_boundary"] =
        gusset(json!({"value": (10 * MM - 1).to_string(), "unit": "ticks"}));
    let issues = validate_json(&narrow.to_string(), &result.to_string()).expect("judged");
    let text = format!("{issues:?}");
    assert!(text.contains("bag_girth_exceeded"), "{text}");
    assert!(
        text.contains(&format!(", side gusset {}", 10 * MM - 1)),
        "{text}"
    );
    let mut flat = body;
    flat["containers"][0]["deformable_boundary"] = json!({"kind": "flat_bag"});
    let text = format!(
        "{:?}",
        validate_json(&flat.to_string(), &result.to_string()).expect("judged")
    );
    assert!(text.contains("bag_girth_exceeded"), "{text}");
    assert!(!text.contains("side gusset"), "{text}");
}

#[test]
fn a_fixed_set_one_tick_past_the_widened_girth_cannot_hold() {
    let fixed: Vec<Value> = [0, 20, 40]
        .iter()
        .map(|z| {
            json!({"item_type": "book", "container_type": "pouch", "orientation": "LWH",
                   "position": {"x": "0", "y": "0", "z": z.to_string()}})
        })
        .collect();
    let mut past = request(
        3,
        gusset(json!({"value": (10 * MM - 1).to_string(), "unit": "ticks"})),
    );
    past["fixed_placements"] = json!(fixed);
    match pack_json(&past.to_string()) {
        Err(error) => assert_eq!(
            error.to_string(),
            "invalid_fixed_placement: bag_girth_exceeded: pouch#1"
        ),
        Ok(_) => panic!("the fixed set was admitted"),
    }
    let mut at = request(3, gusset(json!("10")));
    at["fixed_placements"] = json!(fixed);
    assert_eq!(solve(&at)["unpacked_items"], json!([]));
}

#[test]
fn a_malformed_boundary_is_refused_in_the_contract_order() {
    let at = "/containers/0/deformable_boundary";
    let cases = [
        (
            json!("gusseted_bag"),
            at.to_owned(),
            "wrong_type",
            "must be an object",
        ),
        (
            json!({"side_gusset_depth": "10"}),
            format!("{at}/kind"),
            "missing_field",
            "is required",
        ),
        (
            json!({"kind": "gusseted"}),
            format!("{at}/kind"),
            "not_allowed",
            "must be one of [\"flat_bag\",\"gusseted_bag\"]",
        ),
        (
            json!({"kind": "flat_bag", "side_gusset_depth": "10"}),
            format!("{at}/side_gusset_depth"),
            "not_allowed",
            "is not a known field",
        ),
        (
            json!({"kind": "gusseted_bag", "side_gusset_depth": "10", "bottom": "5"}),
            format!("{at}/bottom"),
            "not_allowed",
            "is not a known field",
        ),
        (
            json!({"kind": "gusseted_bag", "side_gusset_depth": null}),
            format!("{at}/side_gusset_depth"),
            "missing_field",
            "is required",
        ),
        (
            gusset(json!(true)),
            format!("{at}/side_gusset_depth"),
            "wrong_type",
            "must be a measure",
        ),
        (
            gusset(json!("0")),
            format!("{at}/side_gusset_depth"),
            "invalid_value",
            "must be positive",
        ),
        (
            gusset(json!("0.00001")),
            format!("{at}/side_gusset_depth"),
            "invalid_value",
            "must be positive",
        ),
        (
            gusset(json!({"value": (150 * MM + 1).to_string(), "unit": "ticks"})),
            format!("{at}/side_gusset_depth"),
            "invalid_value",
            "exceeds inner_dimensions width",
        ),
    ];
    for (boundary, field, reason, detail) in cases {
        let body = request(1, boundary.clone());
        match pack_json(&body.to_string()) {
            Err(PackError::InvalidRequest(error)) => assert_eq!(
                (error.field(), error.reason(), error.detail()),
                (field.as_str(), reason, detail),
                "{boundary}"
            ),
            other => panic!("{boundary} was not refused as a request error: {other:?}"),
        }
    }
    let mut excluded = request(1, gusset(json!("10")));
    excluded["containers"][0]["outer_dimensions"] =
        json!({"length": "310", "width": "160", "height": "110"});
    match pack_json(&excluded.to_string()) {
        Err(PackError::InvalidRequest(error)) => assert_eq!(
            (error.field(), error.reason()),
            ("/containers/0/outer_dimensions", "not_allowed")
        ),
        other => panic!("a gusseted bag admitted outer_dimensions: {other:?}"),
    }
    assert_eq!(
        solve(&request(1, gusset(json!("150"))))["unpacked_items"],
        json!([])
    );
}
