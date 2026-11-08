//!: a pallet's overhang limit, its deck support rule and the deck-first trial.
//!
//! Mirrors the Python reference (`packvium-python/src/packvium/pallet_overhang.py`), measured
//! through `pack_json` because that is the path a request takes, and through `validate_json`
//! for the result validator's own rules.

use packvium_core::{
    Aabb, Container, Deck, DeformableBoundary, Dimensions, IndependentValidator, Item,
    ItemInstance, Length, Obstacle, OverhangLimit, PackError, PackedContainer, PackingConfig,
    PackingRequest, Placement, Point, RequestError, Rotation, Weight, pack_json, pack_request,
    validate_json,
};
use serde_json::{Value, json};
use std::path::Path;

const MM: i64 = 16_000;

fn conformance(kind: &str, name: &str) -> Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../../../conformance")
        .join(kind)
        .join(format!("feature-pallet-overhang-{name}.json"));
    serde_json::from_str(&std::fs::read_to_string(path).expect("the file exists"))
        .expect("the file is JSON")
}

fn crate_item() -> Value {
    json!({"id": "crate", "quantity": 1, "weight": "10 kg", "allowed_rotations": ["LWH"],
           "dimensions": {"length": "1250", "width": "850", "height": "400"}})
}

fn cartons(quantity: u64) -> Value {
    json!({"id": "carton", "quantity": quantity, "weight": "10 kg",
           "dimensions": {"length": "400", "width": "400", "height": "300"}})
}

fn pallet(limit: Value, extra: Value) -> Value {
    let mut container = json!({"id": "pallet", "max_payload": "1000 kg",
        "inner_dimensions": {"length": "1200", "width": "800", "height": "1000"},
        "pallet_overhang_limit": limit});
    for (key, value) in extra.as_object().expect("extra fields are an object") {
        container[key] = value.clone();
    }
    container
}

fn limit(length: &str, width: &str) -> Value {
    json!({"length": length, "width": width})
}

fn request(items: Vec<Value>, container: Value, configuration: Value) -> Value {
    let mut base = json!({"solver_profile": "balanced", "time_limit_ms": 300000,
        "effort_budget": {"max_search_nodes": 1000000}});
    for (key, value) in configuration
        .as_object()
        .expect("configuration is an object")
    {
        base[key] = value.clone();
    }
    json!({"items": items, "containers": [container], "configuration": base})
}

fn solve(request: &Value) -> Value {
    let result = pack_json(&request.to_string()).expect("the request packs");
    let issues = validate_json(&request.to_string(), &result).expect("the result is readable");
    assert!(
        issues.is_empty(),
        "the result validator refused it: {issues:?}"
    );
    serde_json::from_str(&result).expect("the result is JSON")
}

fn ticks(value: &Value) -> i64 {
    value["ticks"].as_i64().expect("ticks")
}

fn sides_mm(value: &Value) -> [i64; 3] {
    ["length", "width", "height"].map(|side| ticks(&value[side]) / MM)
}

fn placements(result: &Value) -> &Vec<Value> {
    result["containers"][0]["placements"]
        .as_array()
        .expect("placements")
}

/// A library request: these items and containers, every other field as JSON defaults it.
fn library(items: Vec<Item>, containers: Vec<Container>) -> PackingRequest {
    PackingRequest {
        items,
        containers,
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

fn unsupported(request: &Value) -> String {
    match pack_json(&request.to_string()) {
        Err(PackError::UnsupportedFeature(message)) => message,
        other => panic!("not refused as unsupported: {other:?}"),
    }
}

fn refused(request: &Value) -> RequestError {
    match pack_json(&request.to_string()) {
        Err(PackError::InvalidRequest(error)) => error,
        other => panic!("not refused as a request error: {other:?}"),
    }
}

#[test]
fn every_fixture_scores_its_golden_and_passes_the_validator() {
    let corpus = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../../../conformance");
    // A Linux gate copies only conformance/scene/, so the directory alone proves nothing.
    if !corpus.join("fixtures").is_dir() {
        eprintln!(
            "skipping shared pallet golden comparison: the corpus lives in the workspace only"
        );
        return;
    }
    for name in [
        "needed",
        "deck-first",
        "support-binds",
        "zero-forbids",
        "billed",
    ] {
        let fixture = conformance("fixtures", name);
        let golden = conformance("golden", name);
        let result = solve(&fixture);
        assert_eq!(result["score"], golden["score"], "{name}");
        assert_eq!(result["summary"], golden["summary"], "{name}");
    }
}

#[test]
fn a_load_the_deck_holds_never_overhangs_it() {
    let result = solve(&request(
        vec![cartons(6)],
        pallet(limit("50", "50"), json!({})),
        json!({}),
    ));
    let container = &result["containers"][0];
    assert_eq!(sides_mm(&container["inner_dimensions"]), [1200, 800, 1000]);
    assert_eq!(sides_mm(&container["outer_dimensions"]), [1200, 800, 1000]);
    assert_eq!(
        sides_mm(&container["achieved_dimensions"]),
        [1200, 800, 300]
    );
    for placement in placements(&result) {
        let x = ticks(&placement["position"]["x"]) / MM;
        let y = ticks(&placement["position"]["y"]) / MM;
        assert!(
            (50..=850).contains(&x) && (50..=450).contains(&y),
            "{placement}"
        );
        assert_eq!(placement["support_ratio"], "1.000000");
    }
}

#[test]
fn the_overhang_is_used_only_when_it_places_more_and_reports_the_deck_share() {
    let result = solve(&request(
        vec![crate_item()],
        pallet(limit("50", "50"), json!({})),
        json!({"minimum_support_ratio": 0.75}),
    ));
    let crate_ = &placements(&result)[0];
    // 1200 x 800 of a 1250 x 850 base rests on the deck, seated centred on it.
    assert_eq!(crate_["support_ratio"], "0.903529");
    assert_eq!(ticks(&crate_["position"]["x"]) / MM, 25);
    assert_eq!(ticks(&crate_["position"]["y"]) / MM, 25);
    assert_eq!(
        sides_mm(&result["containers"][0]["achieved_dimensions"]),
        [1250, 850, 400]
    );
    assert_eq!(
        sides_mm(&result["containers"][0]["inner_dimensions"]),
        [1200, 800, 1000]
    );
}

#[test]
fn a_zero_limit_forbids_overhang_but_still_reports_the_achieved_size() {
    let result = solve(&request(
        vec![crate_item(), cartons(1)],
        pallet(limit("0", "0"), json!({})),
        json!({}),
    ));
    assert_eq!(
        result["unpacked_items"][0]["reason"],
        "no_compatible_container_dimensions"
    );
    assert_eq!(
        sides_mm(&result["containers"][0]["achieved_dimensions"]),
        [1200, 800, 300]
    );
}

#[test]
fn the_deck_alone_must_carry_the_support_ratio() {
    // 0.903529 of the crate rests on the deck: short of 0.95, and named so once a layout exists
    // for the support replay to observe.
    let result = solve(&request(
        vec![crate_item(), cartons(1)],
        pallet(limit("50", "50"), json!({})),
        json!({"minimum_support_ratio": 0.95}),
    ));
    let crate_ = result["unpacked_items"]
        .as_array()
        .expect("unpacked")
        .iter()
        .find(|item| item["item_type"] == "crate")
        .expect("the crate is left out");
    assert_eq!(crate_["reason"], "insufficient_support");
    // A plank longer than the deck: the frame search leaves it at the overhang corner, 0.872
    // on the deck, and the seat moves it flush on the deck's width and centred on its length.
    let plank = json!({"id": "plank", "quantity": 1, "weight": "1 kg",
        "allowed_rotations": ["LWH"],
        "dimensions": {"length": "1290", "width": "800", "height": "10"}});
    let result = solve(&request(
        vec![plank],
        pallet(limit("50", "50"), json!({})),
        json!({"minimum_support_ratio": 0.85}),
    ));
    let placed = &placements(&result)[0];
    assert_eq!(placed["support_ratio"], "0.930233");
    assert_eq!(ticks(&placed["position"]["x"]) / MM, 5);
    assert_eq!(ticks(&placed["position"]["y"]) / MM, 50);
}

#[test]
fn billing_uses_the_larger_of_the_declared_footprint_and_the_reached_one() {
    let billed = |outer: Value| {
        let mut extra = json!({"rate_table": {"weight_brackets_g": [100000, 1000000],
                                              "prices_minor": [1000, 2500]}});
        if !outer.is_null() {
            extra["outer_dimensions"] = outer;
        }
        let result = solve(&request(
            vec![crate_item()],
            pallet(limit("50", "50"), extra),
            json!({"objective": "shipping_cost", "dimensional_weight_divisor": 5000,
                   "dimensional_weight_length_unit": "cm",
                   "dimensional_weight_weight_unit": "kg"}),
        ));
        result["score"][1].as_i64().expect("billed weight")
    };
    const KG: i64 = 8_000_000_000;
    // 125 x 85 x 115 cm / 5000 against the stated outer height, 125 x 85 x 100 cm without.
    assert_eq!(
        billed(json!({"length": "1210", "width": "810", "height": "1150"})),
        244_375 * KG / 1000
    );
    assert_eq!(billed(Value::Null), 212_500 * KG / 1000);
    // An outer larger than the load is billed as stated.
    assert_eq!(
        billed(json!({"length": "1300", "width": "900", "height": "1150"})),
        130 * 90 * 115 * KG / 5000
    );
}

#[test]
fn a_packed_pallet_is_handed_up_at_its_billed_size() {
    let mut container = Container::new(
        "pallet",
        Dimensions {
            length: Length(1200 * MM),
            width: Length(800 * MM),
            height: Length(1000 * MM),
        },
    );
    container.pallet_overhang_limit = Some(OverhangLimit {
        length: Length(50 * MM),
        width: Length(50 * MM),
    });
    let item = Item {
        allowed_rotations: vec![Rotation::Lwh],
        ..Item::new(
            "crate",
            Dimensions {
                length: Length(1250 * MM),
                width: Length(850 * MM),
                height: Length(400 * MM),
            },
            Weight(0),
            1,
        )
    };
    let request = library(vec![item], vec![container]);
    let result = pack_request(&request).expect("the pallet packs");
    let packed = &result.containers[0];
    assert!(packed.container.deck.is_some());
    assert_eq!(packed.container.declared_inner().length, Length(1200 * MM));
    let handed = packed.as_item().dimensions;
    assert_eq!(
        [handed.length.0, handed.width.0, handed.height.0],
        [1250 * MM, 850 * MM, 1000 * MM]
    );
}

#[test]
fn every_combination_the_deck_cannot_honour_is_refused_first() {
    let mut base = request(
        vec![json!({"id": "carton", "stop_index": 1,
                    "dimensions": {"length": "100", "width": "100", "height": "100"}})],
        pallet(
            limit("50", "50"),
            json!({"access_directions": ["front"], "axles": [], "obstacles": [{"id": "o",
                   "dimensions": {"length": "10", "width": "10", "height": "10"}}],
                   "void_fill_reserve_ratio": 0.1,
                   "made_to_measure": {"minimum": {}, "increment": {}}}),
        ),
        json!({}),
    );
    let message = unsupported(&base);
    assert!(
        message.contains(
            "container.pallet_overhang_limit+access_directions, \
             container.pallet_overhang_limit+axles, \
             container.pallet_overhang_limit+item.stop_index, \
             container.pallet_overhang_limit+made_to_measure, \
             container.pallet_overhang_limit+obstacles, \
             container.pallet_overhang_limit+void_fill_reserve_ratio"
        ),
        "{message}"
    );
    // Empty lists and a boolean reserve are not combinations; the boolean is a request error.
    let container = &mut base["containers"][0];
    for key in ["axles", "made_to_measure"] {
        container.as_object_mut().expect("object").remove(key);
    }
    container["access_directions"] = json!([]);
    container["obstacles"] = json!([]);
    container["void_fill_reserve_ratio"] = json!(true);
    base["items"][0]["stop_index"] = Value::Null;
    assert_eq!(
        refused(&base).field(),
        "/containers/0/void_fill_reserve_ratio"
    );
}

#[test]
fn every_malformed_limit_names_its_rule() {
    let cases = [
        (json!("50"), "wrong_type", "", "must be an object"),
        (
            json!({"length": "1", "width": "1", "depth": "1", "axis": "1"}),
            "not_allowed",
            "/axis",
            "is not a known field",
        ),
        (
            json!({"width": "1"}),
            "missing_field",
            "/length",
            "is required",
        ),
        (
            json!({"length": "1"}),
            "missing_field",
            "/width",
            "is required",
        ),
        (
            json!({"length": "-1", "width": "1"}),
            "negative_measure",
            "/length",
            "cannot be negative",
        ),
        (
            json!({"length": "1", "width": {"value": "1", "unit": "furlong"}}),
            "invalid_unit",
            "/width",
            "has an unknown unit \"furlong\"",
        ),
        (
            json!({"length": "4503599627370496 ticks", "width": "0"}),
            "above_maximum",
            "/length",
            "must be at most 9007199254740991 ticks",
        ),
        (
            json!({"length": "0", "width": "4000000000000000 ticks"}),
            "above_maximum",
            "",
            "must enclose at most 10^30 cubic ticks",
        ),
    ];
    for (value, reason, suffix, detail) in cases {
        let error = refused(&request(
            vec![cartons(1)],
            pallet(value, json!({})),
            json!({}),
        ));
        assert_eq!(
            (error.reason(), error.field(), error.detail()),
            (
                reason,
                format!("/containers/0/pallet_overhang_limit{suffix}").as_str(),
                detail
            )
        );
    }
    let error = refused(&request(
        vec![cartons(1)],
        pallet(
            limit("1", "1"),
            json!({"deformable_boundary": {"kind": "flat_bag"}}),
        ),
        json!({}),
    ));
    assert_eq!(
        (error.reason(), error.field(), error.detail()),
        (
            "not_allowed",
            "/containers/0/pallet_overhang_limit",
            "cannot be combined with deformable_boundary"
        )
    );
}

#[test]
fn a_library_pallet_refuses_what_the_json_path_refuses() {
    let side = Length(100 * MM);
    let size = Dimensions {
        length: side,
        width: side,
        height: side,
    };
    let limit = Some(OverhangLimit {
        length: Length(MM),
        width: Length(MM),
    });
    let cube = || vec![Item::new("cube", size, Weight(0), 1)];
    let refusal = |container: Container| {
        let request = library(
            cube(),
            vec![Container {
                pallet_overhang_limit: limit,
                ..container
            }],
        );
        match pack_request(&request) {
            Err(PackError::InvalidInput(message)) => message,
            other => panic!("admitted: {other:?}"),
        }
    };
    let plain = Container::new("pallet", size);
    let obstacle = Obstacle {
        id: "post".into(),
        box_: Aabb {
            origin: Point::ZERO,
            dimensions: Dimensions {
                length: Length(MM),
                width: Length(MM),
                height: Length(MM),
            },
        },
        additional_boxes: Vec::new(),
    };
    assert_eq!(
        refusal(Container {
            obstacles: vec![obstacle],
            access_directions: vec!["front".into()],
            ..plain.clone()
        }),
        "container pallet: obstacles cannot be combined with pallet_overhang_limit"
    );
    assert_eq!(
        refusal(Container {
            access_directions: vec!["front".into()],
            void_fill_reserve_ppm: 1,
            ..plain.clone()
        }),
        "container pallet: access_directions cannot be combined with pallet_overhang_limit"
    );
    assert_eq!(
        refusal(Container {
            void_fill_reserve_ppm: 1,
            ..plain.clone()
        }),
        "container pallet: void_fill_reserve_ratio cannot be combined with pallet_overhang_limit"
    );
    assert_eq!(
        refusal(Container {
            deformable_boundary: Some(DeformableBoundary::FlatBag),
            ..plain.clone()
        }),
        "container pallet: pallet_overhang_limit cannot be combined with deformable_boundary"
    );
    let negative = library(
        cube(),
        vec![Container {
            pallet_overhang_limit: Some(OverhangLimit {
                length: Length(-1),
                width: Length(0),
            }),
            ..plain.clone()
        }],
    );
    assert!(matches!(
        pack_request(&negative),
        Err(PackError::InvalidInput(message)) if message.contains("cannot be negative")
    ));
    let wide = PackingRequest {
        containers: vec![Container {
            pallet_overhang_limit: Some(OverhangLimit {
                length: Length(i64::MAX / 2),
                width: Length(0),
            }),
            ..plain
        }],
        ..negative
    };
    assert!(matches!(
        pack_request(&wide),
        Err(PackError::InvalidInput(message)) if message.contains("widens the frame")
    ));
}

/// The deck-first fixture's own result, edited by `edit`, against the result validator.
fn issues_after(edit: impl FnOnce(&mut Value), configuration: Value) -> Vec<String> {
    let request = request(
        vec![cartons(1)],
        pallet(limit("50", "50"), json!({})),
        configuration,
    );
    let mut result = solve(&request);
    edit(&mut result);
    validate_json(&request.to_string(), &result.to_string())
        .expect("the result is readable")
        .into_iter()
        .map(|issue| issue.code)
        .collect()
}

fn move_to(result: &mut Value, x_mm: i64, y_mm: i64) {
    let placement = &mut result["containers"][0]["placements"][0];
    placement["position"]["x"] =
        json!({"ticks": x_mm * MM, "unit": "mm", "value": x_mm.to_string()});
    placement["position"]["y"] =
        json!({"ticks": y_mm * MM, "unit": "mm", "value": y_mm.to_string()});
}

#[test]
fn the_validator_holds_the_deck_rule_and_the_achieved_size() {
    let short = issues_after(
        |result| {
            move_to(result, 0, 0);
            result["containers"][0]["achieved_dimensions"]["length"]["ticks"] = json!(1250 * MM);
            result["containers"][0]["achieved_dimensions"]["width"]["ticks"] = json!(850 * MM);
        },
        json!({"minimum_support_ratio": 0.9}),
    );
    assert!(
        short.contains(&"insufficient_support".to_owned()),
        "{short:?}"
    );
    // 15 x 400 mm of a 400 x 400 base on the deck: enough for 0.01, but the centroid is off.
    let tipping = issues_after(
        |result| {
            move_to(result, 1235, 50);
            result["containers"][0]["achieved_dimensions"]["length"]["ticks"] = json!(1285 * MM);
            result["containers"][0]["placements"][0]["support_ratio"] = json!("0.037500");
        },
        json!({"minimum_support_ratio": 0.01}),
    );
    assert!(
        tipping.contains(&"centre_of_gravity_unsupported".to_owned()),
        "{tipping:?}"
    );
    let wrong = issues_after(
        |result| {
            result["containers"][0]["achieved_dimensions"]["length"]["ticks"] = json!(MM);
        },
        json!({}),
    );
    assert_eq!(wrong, ["achieved_dimensions_mismatch"]);
    let missing = issues_after(
        |result| {
            result["containers"][0]
                .as_object_mut()
                .expect("object")
                .remove("achieved_dimensions");
        },
        json!({}),
    );
    assert_eq!(missing, ["achieved_dimensions_mismatch"]);
}

#[test]
fn the_engine_validator_holds_floor_placements_to_the_deck() {
    let side = |mm: i64| Length(mm * MM);
    let cube = Dimensions {
        length: side(100),
        width: side(100),
        height: side(100),
    };
    let frame = Container {
        deck: Some(Deck {
            x: 70 * MM,
            y: 70 * MM,
            length: 200 * MM,
            width: 200 * MM,
        }),
        ..Container::new(
            "pallet",
            Dimensions {
                length: side(340),
                width: side(340),
                height: side(300),
            },
        )
    };
    let codes = |ratio: f64, x_mm: i64, y_mm: i64| {
        let item = Item {
            allowed_rotations: vec![Rotation::Lwh],
            minimum_support_ratio: ratio,
            ..Item::new("box", cube, Weight(0), 1)
        };
        let origin = Point {
            x: x_mm * MM,
            y: y_mm * MM,
            z: 0,
        };
        let packed = PackedContainer {
            container: frame.clone(),
            sequence: 1,
            placements: vec![Placement {
                instance: ItemInstance {
                    item: item.clone(),
                    sequence: 1,
                },
                position: origin,
                rotation: Rotation::Lwh,
                dimensions: cube,
                envelope_origin: origin,
                envelope_dimensions: cube,
                support_ratio: 1.0,
                top_load: Weight(0),
                fixed: false,
            }],
            lattice_summary: None,
            lattice_items: Vec::new(),
        };
        IndependentValidator
            .validate_containers(&library(vec![item], vec![frame.clone()]), &[packed])
            .issues
            .into_iter()
            .map(|issue| issue.code)
            .collect::<Vec<_>>()
    };
    assert!(codes(0.5, 70, 70).is_empty());
    // Nothing is enforced at a ratio of 0, even wholly off the deck.
    assert!(codes(0.0, 0, 0).is_empty());
    // 9% of the base on the deck.
    assert_eq!(codes(0.5, 0, 0), ["support"]);
    // Half on the deck, the centroid on its edge: carried.
    assert!(codes(0.5, 20, 70).is_empty());
    // 30 of 100 mm on the deck passes 0.3, but the centroid lies off it.
    assert_eq!(codes(0.3, 0, 70), ["centre_of_gravity_unsupported"]);
}

#[test]
fn a_frame_that_places_no_more_than_the_deck_leaves_the_load_on_the_deck() {
    let tower = json!({"id": "tower", "quantity": 1, "weight": "1 kg", "allowed_rotations": ["LWH"],
        "dimensions": {"length": "100", "width": "100", "height": "1100"}});
    let result = solve(&request(
        vec![cartons(6), tower],
        pallet(limit("50", "50"), json!({})),
        json!({}),
    ));
    assert_eq!(result["unpacked_items"][0]["item_type"], "tower");
    assert_eq!(
        sides_mm(&result["containers"][0]["achieved_dimensions"]),
        [1200, 800, 300]
    );
}

#[test]
fn a_library_request_refuses_a_routed_item_beside_a_pallet() {
    let size = Dimensions {
        length: Length(100 * MM),
        width: Length(100 * MM),
        height: Length(100 * MM),
    };
    let routed = Item {
        stop_index: Some(1),
        ..Item::new("routed", size, Weight(0), 1)
    };
    let request = library(
        vec![Item::new("plain", size, Weight(0), 1), routed],
        vec![Container {
            pallet_overhang_limit: Some(OverhangLimit {
                length: Length(MM),
                width: Length(MM),
            }),
            ..Container::new("pallet", size)
        }],
    );
    match pack_request(&request) {
        Err(PackError::InvalidRequest(error)) => assert_eq!(
            (error.reason(), error.field(), error.detail()),
            (
                "not_allowed",
                "/items/1/stop_index",
                "cannot be combined with pallet_overhang_limit"
            )
        ),
        other => panic!("admitted: {other:?}"),
    }
}
