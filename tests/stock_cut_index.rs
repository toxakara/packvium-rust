use packvium_core::artifacts::build_artifact_json;
use packvium_core::pack_json;
use packvium_core::revisions::{derive_revision_json, root_revision_json, stock_cut_boxes_json};
use serde_json::{Value, json};

#[test]
fn replacement_and_lock_refuse_unreadable_stock_fields() {
    let request = json!({"items":[{"id":"cube", "dimensions":{"length":"10","width":"10","height":"10"}}],
        "containers":[{"id":"machine", "inner_dimensions":{"length":"10","width":"10","height":"10"},
            "made_to_measure":{"minimum":{"length":"10","width":"10","height":"10"},"increment":{"length":"10","width":"10","height":"10"}}}]});
    let request_text = request.to_string();
    let result = pack_json(&request_text).unwrap();
    let artifact = build_artifact_json(&request_text, &result, "{}").unwrap();
    let parent = root_revision_json(&request_text).unwrap();
    for (field, value, detail) in [
        (
            "made_to_measure",
            json!(true),
            "made_to_measure is not an object",
        ),
        ("cost_minor", json!("price"), "cost_minor is not an integer"),
        (
            "tare_weight",
            json!("heavy"),
            "tare_weight is not a measure",
        ),
    ] {
        let mut replacement = request["containers"][0].clone();
        replacement[field] = value;
        let events = json!([{"sequence":1,"type":"container_substituted","container_type":"machine","replacement":replacement},
            {"sequence":2,"type":"placement_locked","placement":{"item_type":"cube","container_type":"machine","orientation":"LWH"}}]);
        let error = derive_revision_json(&parent, &artifact, &events.to_string()).unwrap_err();
        assert_eq!(error.code(), "invalid_revision");
        assert_eq!(
            error.message(),
            format!("a made-to-measure container's {detail}")
        );
    }
    let events = json!([{"sequence":1,"type":"placement_locked","placement":{"item_type":"cube","container_type":"machine","orientation":"LWH", "position":{"x":"999999"}}}]);
    let revision: Value = serde_json::from_str(
        &derive_revision_json(&parent, &artifact, &events.to_string()).unwrap(),
    )
    .unwrap();
    assert_eq!(
        revision["request"]["fixed_placements"][0]["position"]["x"],
        "999999"
    );
    for (units, detail) in [
        (json!([]), "units is not an object"),
        (json!({"length":[]}), "units.length is not a string"),
    ] {
        let mut fixed = request.clone();
        fixed["units"] = units;
        fixed["fixed_placements"] = json!([{"container_type":"machine"}]);
        let error = stock_cut_boxes_json(&fixed.to_string(), &artifact, 1).unwrap_err();
        assert_eq!(error.code(), "invalid_revision");
        assert_eq!(
            error.message(),
            format!("a made-to-measure container's {detail}")
        );
    }
    for (cost, expected) in [
        (Value::Null, 0),
        (json!(0), 0),
        (json!(1), 1),
        (json!(1.0), 1),
        (json!(-1), -1),
    ] {
        let mut fixed = request.clone();
        fixed["containers"][0]["cost_minor"] = cost;
        fixed["fixed_placements"] = json!([{"container_type":"machine"}]);
        let stocked: Value =
            serde_json::from_str(&stock_cut_boxes_json(&fixed.to_string(), &artifact, 1).unwrap())
                .unwrap();
        assert_eq!(stocked["containers"][1]["cost_minor"], expected);
    }
}

#[test]
fn stocking_keeps_first_lock_order() {
    let request = json!({
        "containers": [{"id":"machine", "quantity":100, "made_to_measure":{}}],
        "fixed_placements": (1..=100).rev().map(|i|json!({"container_type":"machine", "container_instance":i})).collect::<Vec<_>>()
    });
    let artifact = json!({
        "work_order":{"containers":(1..=100).map(|_|json!({"container_type":"machine"})).collect::<Vec<_>>()},
        "geometry":{"containers":(1..=100).map(|i|json!({"inner_dimensions":{"length":i.to_string(),"width":"2","height":"3"}})).collect::<Vec<_>>()}
    });
    let output = stock_cut_boxes_json(&request.to_string(), &artifact.to_string(), 7).unwrap();
    let stocked: Value = serde_json::from_str(&output).unwrap();
    assert_eq!(stocked["containers"].as_array().unwrap().len(), 100);
    assert_eq!(stocked["containers"][0]["id"], "machine@7#100");
    assert_eq!(stocked["containers"][99]["id"], "machine@7#1");
    assert_eq!(
        stocked["containers"][0]["inner_dimensions"]["length"]["value"],
        "100"
    );
    assert_eq!(
        stocked["containers"][99]["inner_dimensions"]["length"]["value"],
        "1"
    );
}

#[test]
fn stocking_joins_types_and_leaves_unselected_malformed_geometry_unread() {
    let request = json!({
        "containers":[{"id":"machine","made_to_measure":{},"cost_minor":1}, {"id":"machine","made_to_measure":{},"cost_minor":9}],
        "fixed_placements":[{"container_type":"machine","container_instance":2}]
    });
    let artifact = json!({
        "work_order":{"containers":[{"container_type":"machine"},{"container_type":"other"},{"container_type":"machine"},{"container_type":"machine"}]},
        "geometry":{"containers":[null,null,{"inner_dimensions":{"length":"1","width":"2","height":"3"}}]}
    });
    let output = stock_cut_boxes_json(&request.to_string(), &artifact.to_string(), 1).unwrap();
    let stocked: Value = serde_json::from_str(&output).unwrap();
    assert_eq!(stocked["containers"][1]["cost_minor"], 9);
    assert_eq!(
        stocked["containers"][1]["inner_dimensions"]["length"]["value"],
        "1"
    );
}
