//!: made-to-measure containers, measured through `pack_json` because that is the path a
//! request takes. Mirrors `packvium-python/tests/test_made_to_measure.py`; the refusal texts are
//! asserted whole because they are compared across engines. The arithmetic and the rung chain
//! are unit-tested beside the code (`made_to_measure.rs`, `solvers/extreme.rs`).

mod support;

use packvium_core::{
    MadeToMeasure, PackError, PackingConfig, PackingRequest, Weight, pack_json, pack_request,
};
use serde_json::{Value, json};

fn mm(length: u64, width: u64, height: u64) -> Value {
    json!({"length": length.to_string(), "width": width.to_string(), "height": height.to_string()})
}

fn request(container: Value, configuration: Value) -> Value {
    let mut machine = json!({"id": "machine", "cost_minor": 40, "inner_dimensions": mm(1510, 1510, 1510),
        "made_to_measure": {"minimum": mm(60, 60, 60), "increment": mm(25, 25, 25),
                            "cost_per_square_metre_minor": 1000, "outer_allowance": mm(3, 3, 3)}});
    for (key, value) in container
        .as_object()
        .expect("container fields are an object")
    {
        machine[key] = value.clone();
    }
    let mut base = json!({"solver_profile": "fast", "minimum_support_ratio": 1, "time_limit_ms": 300000,
                          "effort_budget": {"max_search_nodes": 1000000}});
    for (key, value) in configuration
        .as_object()
        .expect("configuration is an object")
    {
        base[key] = value.clone();
    }
    json!({"units": {"length": "mm"}, "configuration": base, "containers": [machine],
           "items": [{"id": "cube", "quantity": 8, "weight": "100", "dimensions": mm(100, 100, 100)}]})
}

fn pack(request: &Value) -> Value {
    serde_json::from_str(&pack_json(&request.to_string()).expect("the request packs"))
        .expect("the result is JSON")
}

fn value(dimensions: &Value, side: &str) -> String {
    dimensions[side]["value"]
        .as_str()
        .expect("a measure")
        .to_string()
}

#[test]
fn the_worked_order_is_cut_to_a_210_mm_box_with_board_tare() {
    let mut data = request(json!({}), json!({}));
    data["containers"][0]["made_to_measure"]["tare_per_square_metre"] = json!("500");
    let result = pack(&data);
    let packed = &result["containers"][0];
    assert_eq!(value(&packed["inner_dimensions"], "length"), "210");
    assert_eq!(value(&packed["outer_dimensions"], "height"), "213");
    assert_eq!(value(&packed["achieved_dimensions"], "width"), "200");
    assert_eq!(packed["made_to_measure"]["cost_minor"], json!(305));
    assert_eq!(
        packed["made_to_measure"]["tare_weight"]["value"],
        json!("132.3")
    );
    assert_eq!(packed["gross_weight"]["value"], json!("932.3"));
    assert_eq!(result["score"][2], json!(305));
}

#[test]
fn the_round_key_reads_the_fabricated_price_in_every_profile() {
    for profile in ["fast", "balanced", "quality", "exact_small"] {
        let mut data = request(json!({}), json!({"solver_profile": profile}));
        data["containers"]
            .as_array_mut()
            .expect("containers")
            .push(json!({"id": "stock", "cost_minor": 300, "inner_dimensions": mm(210, 210, 210)}));
        if profile == "exact_small" {
            data["items"][0]["quantity"] = json!(2);
        }
        let result = pack(&data);
        assert_eq!(
            result["containers"].as_array().map(Vec::len),
            Some(1),
            "{profile}"
        );
        if profile != "exact_small" {
            assert_eq!(
                result["containers"][0]["container_type"],
                json!("stock"),
                "{profile}"
            );
        }
    }
}

#[test]
fn the_lattice_and_the_block_loader_stand_down_and_nothing_claims_optimality() {
    let compact = pack(&request(
        json!({}),
        json!({"require_placement_coordinates": false}),
    ));
    assert!(compact["containers"][0].get("lattice_summary").is_none());
    assert_eq!(
        compact["containers"][0]["placements"]
            .as_array()
            .map(Vec::len),
        Some(8)
    );
    let pinned = pack(&request(
        json!({}),
        json!({"solvers": ["homogeneous_blocks"]}),
    ));
    assert_eq!(
        pinned["containers"][0]["made_to_measure"]["cost_minor"],
        json!(305)
    );
    let mut exact = request(json!({}), json!({"solver_profile": "exact_small"}));
    exact["items"][0]["quantity"] = json!(2);
    assert_eq!(pack(&exact)["optimality"]["code"], json!("not_proven"));
}

#[test]
fn a_later_round_gets_its_own_compact_rungs() {
    let mut data = request(
        json!({"inner_dimensions": mm(300, 300, 300),
               "made_to_measure": {"minimum": mm(50, 50, 50), "increment": mm(10, 10, 10),
                                   "cost_per_square_metre_minor": 1000}}),
        json!({}),
    );
    data["items"][0]["quantity"] = json!(30);
    let result = pack(&data);
    let costs = result["containers"]
        .as_array()
        .expect("containers")
        .iter()
        .map(|packed| packed["made_to_measure"]["cost_minor"].clone())
        .collect::<Vec<_>>();
    assert_eq!(costs, vec![json!(580), json!(200)]);
    let plans = pack(&{
        let mut quality = data.clone();
        quality["configuration"]["solver_profile"] = json!("quality");
        quality
    });
    assert_eq!(plans["unpacked_items"], json!([]));
}

#[test]
fn each_refused_combination_is_named() {
    for (extra, name) in [
        (
            json!({"axles": [{"position": "0"}, {"position": "1510"}]}),
            "container.made_to_measure+axles",
        ),
        (
            json!({"obstacles": [{"id": "post", "dimensions": mm(10, 10, 10)}]}),
            "container.made_to_measure+obstacles",
        ),
        (
            json!({"void_fill_reserve_ratio": 0.5}),
            "container.made_to_measure+void_fill_reserve_ratio",
        ),
    ] {
        match pack_json(&request(extra, json!({})).to_string()) {
            Err(PackError::UnsupportedFeature(message)) => {
                assert!(message.contains(name), "{message}")
            }
            other => panic!("{name} was not refused: {other:?}"),
        }
    }
    let mut fixed = request(json!({}), json!({}));
    fixed["fixed_placements"] =
        json!([{"item_type": "cube", "container_type": "machine", "orientation": "LWH"}]);
    match pack_json(&fixed.to_string()) {
        Err(PackError::UnsupportedFeature(message)) => {
            assert!(
                message.contains("fixed_placements+container.made_to_measure"),
                "{message}"
            )
        }
        other => panic!("a fixed placement was not refused: {other:?}"),
    }
    pack(&request(
        json!({"obstacles": [], "void_fill_reserve_ratio": 0}),
        json!({}),
    ));
}

#[test]
fn each_request_error_names_its_rule() {
    let cases = [
        (
            json!({"extra": 1}),
            "/made_to_measure/extra",
            "not_allowed",
            "is not a known field".to_string(),
        ),
        (
            json!({"minimum": mm(60, 0, 60)}),
            "/made_to_measure/minimum/width",
            "invalid_value",
            "must be positive".into(),
        ),
        (
            json!({"increment": mm(25, 25, 0)}),
            "/made_to_measure/increment/height",
            "invalid_value",
            "must be positive".into(),
        ),
        (
            json!({"minimum": mm(1600, 60, 60)}),
            "/made_to_measure/minimum/length",
            "invalid_value",
            "exceeds inner_dimensions".into(),
        ),
        (
            json!({"increment": mm(25, 30, 25)}),
            "/made_to_measure/increment/width",
            "invalid_value",
            "does not divide inner_dimensions minus minimum".into(),
        ),
        (
            json!({"cost_per_square_metre_minor": 9_007_199_254_740_991_i64}),
            "/made_to_measure/cost_per_square_metre_minor",
            "invalid_value",
            "prices the largest box above 9007199254740991".into(),
        ),
        (
            json!({"tare_per_square_metre": "-1"}),
            "/made_to_measure/tare_per_square_metre",
            "negative_measure",
            "cannot be negative".into(),
        ),
    ];
    for (fields, field, reason, detail) in cases {
        let mut data = request(json!({}), json!({}));
        for (key, value) in fields.as_object().expect("fields") {
            data["containers"][0]["made_to_measure"][key] = value.clone();
        }
        match pack_json(&data.to_string()) {
            Err(PackError::InvalidRequest(error)) => assert_eq!(
                (error.field(), error.reason(), error.detail()),
                (
                    format!("/containers/0{field}").as_str(),
                    reason,
                    detail.as_str()
                )
            ),
            other => panic!("{field} was not refused as a request error: {other:?}"),
        }
    }
    match pack_json(
        &request(json!({"outer_dimensions": mm(1600, 1600, 1600)}), json!({})).to_string(),
    ) {
        Err(PackError::InvalidRequest(error)) => assert_eq!(
            (error.field(), error.detail()),
            (
                "/containers/0/outer_dimensions",
                "cannot be combined with made_to_measure"
            )
        ),
        other => panic!("outer_dimensions was not refused: {other:?}"),
    }
}

#[test]
fn a_machine_value_is_public_and_copyable() {
    let machine = MadeToMeasure {
        minimum: [1, 1, 1],
        increment: [1, 1, 1],
        cost_per_square_metre_minor: 0,
        tare_per_square_metre: 0,
        outer_allowance: [0, 0, 0],
    };
    assert_eq!(machine.grid(0, 5), 5);
    assert_eq!(
        machine.solving_envelopes([4, 4, 4], [[1, 1, 1]]).last(),
        Some(&[4, 4, 4])
    );
}

#[test]
fn a_library_machine_is_held_to_the_rules_a_request_is() {
    let side = support::SIDE;
    let valid = MadeToMeasure {
        minimum: [side; 3],
        increment: [side; 3],
        cost_per_square_metre_minor: 0,
        tare_per_square_metre: 0,
        outer_allowance: [0; 3],
    };
    let refusals = [
        MadeToMeasure {
            minimum: [0, side, side],
            ..valid
        },
        MadeToMeasure {
            increment: [7, side, side],
            ..valid
        },
        MadeToMeasure {
            outer_allowance: [-1, 0, 0],
            ..valid
        },
        MadeToMeasure {
            cost_per_square_metre_minor: i64::MAX,
            ..valid
        },
        MadeToMeasure {
            tare_per_square_metre: i64::MAX,
            ..valid
        },
    ];
    let packed = |machine: MadeToMeasure| {
        let mut container = support::container();
        // Near the top of the weight range, so any board at all would pass it.
        container.tare_weight = Weight(i64::MAX - (1 << 50));
        container.made_to_measure = Some(machine);
        pack_request(&PackingRequest {
            items: vec![support::item("cube")],
            containers: vec![container],
            config: PackingConfig::default(),
            output_length_unit: "mm".into(),
            output_weight_unit: "g".into(),
            catalog_versions_used: Vec::new(),
            fixed_placements: Vec::new(),
            fixed_containers: Vec::new(),
        })
    };
    for machine in refusals {
        assert!(
            matches!(packed(machine), Err(PackError::InvalidInput(_))),
            "{machine:?} was admitted"
        );
    }
    let result = packed(MadeToMeasure { ..valid }).expect("a machine on its own grid packs");
    assert_eq!(
        result.containers[0].container.inner_dimensions,
        support::cube(side)
    );
}
