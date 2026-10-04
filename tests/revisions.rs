//! Plan revisions: an append-only, hash-chained record of exceptions (docs/PLAN-REVISIONS.md).
//!
//! Mirrors `packvium-python/tests/test_revisions.py` through the text entry points. The
//! byte-for-byte comparison with the other engines is the revision conformance run's.

use packvium_core::artifacts::build_artifact_json;
use packvium_core::pack_json;
use packvium_core::revisions::{
    FORMAT, RevisionErrorCode, apply_events_json, canonical_revision_json, derive_revision_json,
    document_digest_json, root_revision_json, verify_revision_chain_json,
};
use serde_json::{Value, json};

fn request() -> Value {
    json!({
        "items": [
            {"id": "cube", "quantity": 4, "weight": "1000",
             "dimensions": {"length": "100", "width": "100", "height": "100"}},
            {"id": "slab", "quantity": 1,
             "dimensions": {"length": "200", "width": "100", "height": "50"}}
        ],
        "containers": [
            {"id": "box", "quantity": 3,
             "inner_dimensions": {"length": "200", "width": "100", "height": "200"}},
            {"id": "crate", "inner_dimensions": {"length": "300", "width": "200", "height": "200"}}
        ],
        "configuration": {"solver_profile": "balanced", "minimum_support_ratio": 1.0},
        "output": {}
    })
}

fn artifact(request: &Value) -> String {
    let result = pack_json(&request.to_string()).expect("packs");
    build_artifact_json(&request.to_string(), &result, "{}").expect("artifact")
}

fn locked(kind: &str, sequence: i64) -> Value {
    json!({"sequence": sequence, "type": kind, "placement": {
        "item_type": "cube", "container_type": "box",
        "position": {"x": "0", "z": "0"}, "orientation": "LWH"}})
}

fn missing(item: &str, quantity: i64, sequence: i64) -> Value {
    json!({"sequence": sequence, "type": "item_missing", "item_type": item, "quantity": quantity})
}

fn parse(text: &str) -> Value {
    serde_json::from_str(text).expect("json")
}

/// Root, two revisions, and the artifacts each one approved.
fn chain() -> (Vec<Value>, Vec<Value>) {
    let root = parse(&root_revision_json(&request().to_string()).unwrap());
    let first_artifact = artifact(&root["request"]);
    let events = json!([locked("placement_locked", 1), missing("cube", 1, 2)]);
    let first = parse(
        &derive_revision_json(&root.to_string(), &first_artifact, &events.to_string()).unwrap(),
    );
    let second_artifact = artifact(&first["request"]);
    let events = json!([locked("placement_verified", 3)]);
    let second = parse(
        &derive_revision_json(&first.to_string(), &second_artifact, &events.to_string()).unwrap(),
    );
    (
        vec![root, first, second],
        vec![Value::Null, parse(&first_artifact), parse(&second_artifact)],
    )
}

fn codes(revisions: &[Value], artifacts: Option<&[Value]>) -> Vec<String> {
    let artifacts = artifacts.map(|list| Value::Array(list.to_vec()).to_string());
    let issues = verify_revision_chain_json(
        &Value::Array(revisions.to_vec()).to_string(),
        artifacts.as_deref(),
    )
    .unwrap();
    parse(&issues)
        .as_array()
        .unwrap()
        .iter()
        .map(|issue| issue["code"].as_str().unwrap().to_owned())
        .collect()
}

fn refusal(result: Result<String, packvium_core::revisions::RevisionError>) -> &'static str {
    result.expect_err("refused").code()
}

/// A refusal's code and message, the two things the reference is compared on.
fn said(result: Result<String, packvium_core::revisions::RevisionError>) -> (&'static str, String) {
    let error = result.expect_err("refused");
    (error.code(), error.message().to_owned())
}

#[test]
fn a_revision_links_its_parent_and_approved_artifact_by_digest() {
    let (revisions, artifacts) = chain();
    assert_eq!(revisions[0]["format"], FORMAT);
    assert_eq!(revisions[0]["parent"], Value::Null);
    assert_eq!(revisions[1]["revision"], 1);
    let parent = document_digest_json(&revisions[0].to_string()).unwrap();
    assert_eq!(revisions[1]["parent"], parent.as_str());
    let approved = document_digest_json(&artifacts[1].to_string()).unwrap();
    assert_eq!(revisions[1]["approved"]["artifact"], approved.as_str());
    assert!(parent.starts_with("sha256:") && parent.len() == 71);
}

#[test]
fn an_empty_object_keeps_its_digest() {
    let root = root_revision_json(&request().to_string()).unwrap();
    assert!(root.contains(r#""output":{}"#));
    let pretty = serde_json::to_string_pretty(&parse(&root)).unwrap();
    assert_eq!(
        document_digest_json(&root).unwrap(),
        document_digest_json(&pretty).unwrap()
    );
}

#[test]
fn events_derive_the_next_request() {
    let lowered = parse(
        &apply_events_json(
            &request().to_string(),
            &json!([missing("cube", 3, 1)]).to_string(),
        )
        .unwrap(),
    );
    assert_eq!(lowered["items"][0]["quantity"], 1);
    let removed = parse(
        &apply_events_json(
            &request().to_string(),
            &json!([missing("slab", 1, 1)]).to_string(),
        )
        .unwrap(),
    );
    assert_eq!(removed["items"].as_array().unwrap().len(), 1);
    let fixed = parse(
        &apply_events_json(
            &request().to_string(),
            &json!([
                locked("placement_locked", 1),
                locked("placement_verified", 2)
            ])
            .to_string(),
        )
        .unwrap(),
    );
    assert_eq!(fixed["fixed_placements"].as_array().unwrap().len(), 1);
}

fn locked_at(x: Value) -> Value {
    let mut event = locked("placement_locked", 1);
    event["placement"]["position"]["x"] = x;
    event
}

fn apply_one(
    request: &Value,
    event: Value,
) -> Result<String, packvium_core::revisions::RevisionError> {
    apply_events_json(&request.to_string(), &json!([event]).to_string())
}

#[test]
fn a_distinct_lock_is_appended_to_the_fixed_set() {
    let mut fixed = request();
    fixed["fixed_placements"] = json!([locked_at(json!("100"))["placement"]]);
    let derived = parse(&apply_one(&fixed, locked_at(json!("0"))).unwrap());
    assert_eq!(
        derived["fixed_placements"],
        json!([
            locked_at(json!("100"))["placement"],
            locked_at(json!("0"))["placement"]
        ])
    );
}

/// Python, PHP and JavaScript hand such a request back for admission to refuse; this engine
/// returns canonical JSON, so the refusal comes as the derived request is written -- after the
/// first-match fallback has run rather than as a panic inside it.
#[test]
fn a_fixed_set_with_no_canonical_form_is_refused_once_derived() {
    let mut fixed = request();
    fixed["fixed_placements"] = json!([
        locked_at(json!("0"))["placement"],
        locked_at(json!(1u64 << 60))["placement"]
    ]);
    assert_eq!(
        refusal(apply_one(&fixed, locked_at(json!("0")))),
        "number_out_of_range"
    );
}

#[test]
fn a_lock_with_no_canonical_form_is_refused_once_derived() {
    assert_eq!(
        refusal(apply_one(&request(), locked_at(json!(1u64 << 60)))),
        "number_out_of_range"
    );
}

#[test]
fn a_derived_request_solves_with_its_fixed_items_in_place() {
    let (revisions, _) = chain();
    let result = parse(&pack_json(&revisions[2]["request"].to_string()).unwrap());
    let fixed = result["containers"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|container| {
            container["placements"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|placement| placement["fixed"] == true)
                .map(move |placement| (container["id"].clone(), placement["item_id"].clone()))
        })
        .collect::<Vec<_>>();
    assert_eq!(fixed, [(json!("box#1"), json!("cube#1"))]);
}

#[test]
fn contradictions_are_event_conflicts() {
    let fixed = apply_events_json(
        &request().to_string(),
        &json!([locked("placement_locked", 1)]).to_string(),
    )
    .unwrap();
    for (request, event) in [
        (request().to_string(), missing("pallet", 1, 1)),
        (fixed.clone(), missing("cube", 4, 2)),
        (
            fixed,
            json!({"sequence": 2, "type": "container_substituted", "container_type": "box",
                   "replacement": {"id": "box-b"}}),
        ),
    ] {
        let events = Value::Array(vec![event]).to_string();
        assert_eq!(
            refusal(apply_events_json(&request, &events)),
            "event_conflict"
        );
    }
}

#[test]
fn malformed_input_is_refused_with_its_code() {
    let (revisions, artifacts) = chain();
    let root = revisions[0].to_string();
    let approved = artifacts[1].to_string();
    for events in [
        json!([]),
        json!([locked("placement_locked", 2)]),
        json!([locked("placement_moved", 1)]),
        json!([{"sequence": 1, "type": "item_missing", "item_type": "cube"}]),
        json!([{"sequence": true, "type": "item_missing", "item_type": "cube", "quantity": 1}]),
    ] {
        assert_eq!(
            refusal(derive_revision_json(&root, &approved, &events.to_string())),
            "invalid_event"
        );
    }
    let events = json!([locked("placement_locked", 1)]).to_string();
    let mut other = request();
    other["items"][0]["quantity"] = json!(3);
    assert_eq!(
        refusal(derive_revision_json(&root, &artifact(&other), &events)),
        "invalid_artifact"
    );
    assert_eq!(
        refusal(derive_revision_json(
            &request().to_string(),
            &approved,
            &events
        )),
        "invalid_revision"
    );
    assert_eq!(refusal(root_revision_json("[1]")), "invalid_revision");
    assert_eq!(refusal(root_revision_json("{")), "invalid_json");
}

#[test]
fn an_intact_chain_verifies_clean_and_tampering_is_caught() {
    let (revisions, artifacts) = chain();
    assert!(codes(&revisions, Some(&artifacts)).is_empty());

    let mut reordered = revisions.clone();
    let events = reordered[1]["events"].as_array_mut().unwrap();
    events.swap(0, 1);
    assert!(codes(&reordered, None).contains(&"sequence_gap".into()));

    let mut altered = revisions.clone();
    altered[1]["request"]["items"][0]["quantity"] = json!(9);
    let found = codes(&altered, None);
    assert!(found.contains(&"parent_mismatch".into()));
    assert!(found.contains(&"request_mismatch".into()));

    let mut dropped = revisions.clone();
    dropped.remove(1);
    assert!(codes(&dropped, None).contains(&"revision_number".into()));

    let mut swapped = artifacts.clone();
    swapped[2] = artifacts[1].clone();
    assert!(codes(&revisions, Some(&swapped)).contains(&"artifact_mismatch".into()));
}

#[test]
fn an_error_carries_its_kind_code_and_message() {
    let error = root_revision_json("[1]").expect_err("refused");
    assert_eq!(error.kind(), RevisionErrorCode::InvalidRevision);
    assert_eq!(error.message(), "a request is a JSON object");
    assert_eq!(
        error.to_string(),
        "invalid_revision: a request is a JSON object"
    );
    assert_eq!(
        refusal(root_revision_json(r#"{"n": 1e400}"#)),
        "number_out_of_range"
    );
    assert_eq!(
        refusal(root_revision_json(r#"{"s": "\ud800"}"#)),
        "invalid_string"
    );
}

#[test]
fn the_canonical_form_is_what_the_digest_hashes() {
    let root = root_revision_json(&request().to_string()).unwrap();
    let pretty = serde_json::to_string_pretty(&parse(&root)).unwrap();
    assert_eq!(canonical_revision_json(&pretty).unwrap(), root);
}

#[test]
fn a_parent_that_is_not_a_revision_is_refused() {
    let (revisions, artifacts) = chain();
    let approved = artifacts[1].to_string();
    let events = json!([locked("placement_locked", 1)]).to_string();
    let broken = |edit: fn(&mut Value)| {
        let mut parent = revisions[0].clone();
        edit(&mut parent);
        refusal(derive_revision_json(
            &parent.to_string(),
            &approved,
            &events,
        ))
    };
    assert_eq!(
        broken(|parent| parent["revision"] = json!(-1)),
        "invalid_revision"
    );
    assert_eq!(
        broken(|parent| parent["request"] = json!([])),
        "invalid_revision"
    );
    assert_eq!(
        broken(|parent| parent["events"] = json!({})),
        "invalid_revision"
    );

    let mut unnumbered = revisions[1].clone();
    unnumbered["events"][1]["sequence"] = Value::Null;
    let next = artifacts[2].to_string();
    let later = json!([locked("placement_verified", 3)]).to_string();
    assert_eq!(
        refusal(derive_revision_json(&unnumbered.to_string(), &next, &later)),
        "invalid_revision"
    );
    let mut empty = revisions[1].clone();
    empty["events"] = json!([]);
    assert_eq!(
        refusal(derive_revision_json(&empty.to_string(), &next, &later)),
        "invalid_revision"
    );
}

#[test]
fn an_artifact_without_its_format_or_replay_is_refused() {
    let (revisions, artifacts) = chain();
    let root = revisions[0].to_string();
    let events = json!([locked("placement_locked", 1)]).to_string();
    let mut unformatted = artifacts[1].clone();
    unformatted["format"] = json!("something-else/v1");
    let mut unreplayable = artifacts[1].clone();
    unreplayable["provenance"]["replay"] = Value::Null;
    for artifact in [unformatted, unreplayable] {
        assert_eq!(
            refusal(derive_revision_json(&root, &artifact.to_string(), &events)),
            "invalid_artifact"
        );
    }
}

#[test]
fn every_malformed_event_shape_is_an_invalid_event() {
    let placement = |edit: fn(&mut Value)| {
        let mut event = locked("placement_locked", 1);
        edit(&mut event["placement"]);
        event
    };
    for event in [
        json!("placement_locked"),
        json!({"sequence": 1, "type": "item_missing", "item_type": "cube", "quantity": 1,
               "note": "extra"}),
        json!({"sequence": 1, "type": "item_missing", "item_type": "", "quantity": 1}),
        json!({"sequence": 1, "type": "item_missing", "item_type": "cube", "quantity": 0}),
        json!({"sequence": 1, "type": "container_substituted", "container_type": "box",
               "replacement": "box-b"}),
        json!({"sequence": 1, "type": "container_substituted", "container_type": "box",
               "replacement": {"quantity": 1}}),
        json!({"sequence": 1, "type": "placement_locked", "placement": []}),
        placement(|placement| placement["rotation"] = json!(90)),
        placement(|placement| placement["orientation"] = json!("XYZ")),
        placement(|placement| placement["container_instance"] = json!(0)),
        placement(|placement| placement["position"] = json!([0, 0, 0])),
    ] {
        let events = Value::Array(vec![event.clone()]).to_string();
        assert_eq!(
            refusal(apply_events_json(&request().to_string(), &events)),
            "invalid_event",
            "{event}"
        );
    }
    assert_eq!(
        refusal(apply_events_json(&request().to_string(), "{}")),
        "invalid_event"
    );
}

#[test]
fn a_substitution_replaces_the_container_unless_its_new_id_is_taken() {
    let substitute = |id: &str| {
        json!([{"sequence": 1, "type": "container_substituted", "container_type": "box",
                "replacement": {"id": id, "inner_dimensions":
                    {"length": "250", "width": "100", "height": "200"}}}])
        .to_string()
    };
    let replaced = parse(&apply_events_json(&request().to_string(), &substitute("box-b")).unwrap());
    assert_eq!(replaced["containers"][0]["id"], "box-b");
    assert_eq!(
        refusal(apply_events_json(
            &request().to_string(),
            &substitute("crate")
        )),
        "event_conflict"
    );
}

#[test]
fn a_request_the_events_cannot_edit_is_refused() {
    let mut single = request();
    single["items"] = json!([single["items"][1].clone()]);
    let gone = json!([missing("slab", 1, 1)]).to_string();
    assert_eq!(
        refusal(apply_events_json(&single.to_string(), &gone)),
        "event_conflict"
    );
    let mut unlisted = request();
    unlisted["items"] = json!({});
    assert_eq!(
        refusal(apply_events_json(&unlisted.to_string(), &gone)),
        "invalid_revision"
    );
    let lock = json!([locked("placement_locked", 1)]).to_string();
    assert_eq!(refusal(apply_events_json("[]", &lock)), "invalid_revision");
    let mut unset = request();
    unset["fixed_placements"] = Value::Null;
    let fixed = parse(&apply_events_json(&unset.to_string(), &lock).unwrap());
    assert_eq!(fixed["fixed_placements"].as_array().unwrap().len(), 1);
    for malformed in [json!({"k": 1}), json!([5]), json!("x")] {
        let mut request = request();
        request["fixed_placements"] = malformed;
        for events in [lock.clone(), json!([missing("cube", 1, 1)]).to_string()] {
            assert_eq!(
                said(apply_events_json(&request.to_string(), &events)),
                (
                    "invalid_revision",
                    "request.fixed_placements is a list of objects".into()
                )
            );
        }
    }
}

#[test]
fn the_audit_reports_structure_it_cannot_trust() {
    let (revisions, artifacts) = chain();
    assert_eq!(
        refusal(verify_revision_chain_json("{}", None)),
        "invalid_revision"
    );

    let mut unformatted = revisions.clone();
    unformatted[1]["format"] = json!("other/v1");
    assert_eq!(codes(&unformatted, None), ["invalid_revision"]);

    let mut busy_root = revisions.clone();
    busy_root[0]["events"] = json!([locked("placement_locked", 1)]);
    assert!(codes(&busy_root, None).contains(&"sequence_gap".into()));

    let mut silent = revisions.clone();
    silent[2]["events"] = json!([]);
    assert!(codes(&silent, None).contains(&"sequence_gap".into()));

    let mut conflicting = revisions.clone();
    conflicting[1]["events"][1] = missing("pallet", 1, 2);
    assert!(codes(&conflicting, None).contains(&"request_mismatch".into()));

    let mut replay = revisions.clone();
    replay[1]["approved"]["replay"] = json!({"level": "not_guaranteed"});
    assert!(codes(&replay, Some(&artifacts)).contains(&"artifact_mismatch".into()));

    let mut other = request();
    other["items"][0]["quantity"] = json!(3);
    let foreign = parse(&artifact(&other));
    let mut approved = revisions.clone();
    approved[1]["approved"]["artifact"] =
        json!(document_digest_json(&foreign.to_string()).unwrap());
    let mut swapped = artifacts.clone();
    swapped[1] = foreign;
    assert!(codes(&approved, Some(&swapped)).contains(&"artifact_mismatch".into()));
}

#[test]
fn a_refusal_quotes_values_by_their_canonical_json() {
    let (revisions, artifacts) = chain();
    let root = revisions[0].to_string();
    let approved = artifacts[1].to_string();
    let lock = locked("placement_locked", 1);
    let with_placement = |edit: fn(&mut Value)| {
        let mut event = lock.clone();
        edit(&mut event["placement"]);
        event
    };
    let cases = [
        (
            json!([lock_with(&lock, "type", json!("placement_moved"))]),
            r#"unknown event type "placement_moved""#,
        ),
        (
            json!([lock_with(&lock, "type", json!(["placement_locked"]))]),
            r#"unknown event type ["placement_locked"]"#,
        ),
        (
            json!([lock_with(&lock, "note", json!("strapped"))]),
            r#"placement_locked does not carry ["note"]"#,
        ),
        (
            json!([{"sequence": 1, "type": "item_missing", "item_type": "cube"}]),
            r#"item_missing needs ["quantity"]"#,
        ),
        (
            json!([lock_with(&lock, "sequence", json!(true))]),
            "event sequence true does not continue the chain at 1",
        ),
        (
            json!([lock_with(
                &lock,
                "sequence",
                json!(9_007_199_254_740_992_u64)
            )]),
            "event sequence an out-of-range number does not continue the chain at 1",
        ),
        (
            json!([with_placement(|p| p["position"] = json!({"w": "5"}))]),
            r#"placement.position cannot carry ["w"]"#,
        ),
        (
            json!([with_placement(|p| p["position"] = json!({"x": true}))]),
            "placement.position.x is a measure",
        ),
        (
            json!([with_placement(|p| p["position"] = json!([0, 0, 0]))]),
            "placement.position is a point object",
        ),
        (
            json!([with_placement(|p| p["zone"] = json!("a"))]),
            r#"a placement does not carry ["zone"]"#,
        ),
    ];
    for (events, message) in cases {
        assert_eq!(
            said(derive_revision_json(&root, &approved, &events.to_string())),
            ("invalid_event", message.to_owned()),
            "{events}"
        );
    }
    assert_eq!(
        said(apply_events_json(
            &request().to_string(),
            &json!([missing("pallet", 1, 1)]).to_string()
        )),
        (
            "event_conflict",
            r#"the request has no item "pallet""#.into()
        )
    );
}

fn lock_with(event: &Value, key: &str, value: Value) -> Value {
    let mut event = event.clone();
    event[key] = value;
    event
}

#[test]
fn an_integral_float_is_the_integer_it_spells() {
    let (revisions, artifacts) = chain();
    let events = json!([lock_with(
        &locked("placement_locked", 1),
        "sequence",
        json!(1.0)
    )]);
    let derived = parse(
        &derive_revision_json(
            &revisions[0].to_string(),
            &artifacts[1].to_string(),
            &events.to_string(),
        )
        .unwrap(),
    );
    assert_eq!(derived["revision"], 1);
    let mut integral = revisions.clone();
    integral[1]["revision"] = json!(1.0);
    integral[1]["events"][0]["sequence"] = json!(1.0);
    assert_eq!(codes(&integral, None), Vec::<String>::new());
}

#[test]
fn apply_refuses_what_it_cannot_read_rather_than_defaulting() {
    let gone = json!([missing("cube", 1, 1)]).to_string();
    for (quantity, refused) in [
        (json!("3"), true),
        (json!(2.5), true),
        (Value::Null, true),
        (json!(3.0), false),
    ] {
        let mut request = request();
        request["items"][0]["quantity"] = quantity;
        let result = apply_events_json(&request.to_string(), &gone);
        if refused {
            assert_eq!(
                said(result),
                (
                    "invalid_revision",
                    "request.items[0].quantity is an integer".into()
                )
            );
        } else {
            assert_eq!(parse(&result.unwrap())["items"][0]["quantity"], 2);
        }
    }
    let mut absent = request();
    absent["items"][1]
        .as_object_mut()
        .unwrap()
        .remove("quantity");
    let slab = json!([missing("slab", 1, 1)]).to_string();
    let removed = parse(&apply_events_json(&absent.to_string(), &slab).unwrap());
    assert_eq!(removed["items"].as_array().unwrap().len(), 1);

    let request = request().to_string();
    assert_eq!(
        said(apply_events_json("[]", &gone)),
        ("invalid_revision", "a request is a JSON object".into())
    );
    assert_eq!(
        said(apply_events_json(&request, "{}")),
        ("invalid_event", "events are a JSON array".into())
    );
    for (sequence, spelled) in [
        (Value::Null, "null"),
        (json!("1"), r#""1""#),
        (json!(1.5), "1.5"),
    ] {
        let events = json!([lock_with(&missing("cube", 1, 1), "sequence", sequence)]);
        assert_eq!(
            said(apply_events_json(&request, &events.to_string())),
            (
                "invalid_event",
                format!("event sequence {spelled} is not an integer")
            )
        );
    }
    let mut unsequenced = missing("cube", 1, 1);
    unsequenced.as_object_mut().unwrap().remove("sequence");
    assert_eq!(
        said(apply_events_json(
            &request,
            &json!([unsequenced]).to_string()
        )),
        (
            "invalid_event",
            "event sequence null is not an integer".into()
        )
    );
}

#[test]
fn a_parent_whose_events_lack_an_integer_sequence_is_refused() {
    let (revisions, artifacts) = chain();
    let next = artifacts[2].to_string();
    let later = json!([locked("placement_verified", 3)]).to_string();
    for broken in [json!("1"), json!(true), json!(1.5)] {
        let mut parent = revisions[1].clone();
        parent["events"][0]["sequence"] = broken;
        assert_eq!(
            said(derive_revision_json(&parent.to_string(), &next, &later)),
            (
                "invalid_revision",
                "a revision's events each carry an integer sequence".into()
            )
        );
    }
    let mut not_objects = revisions[1].clone();
    not_objects["events"] = json!([5]);
    assert_eq!(
        refusal(derive_revision_json(
            &not_objects.to_string(),
            &next,
            &later
        )),
        "invalid_revision"
    );
    let mut boolean = revisions.clone();
    boolean[1]["events"][0]["sequence"] = json!(true);
    let issues =
        parse(&verify_revision_chain_json(&Value::Array(boolean).to_string(), None).unwrap());
    assert_eq!(
        issues,
        json!([{"code": "invalid_revision", "revision": 1,
                "detail": "a revision's events each carry an integer sequence"}])
    );
}

#[test]
fn the_audit_refuses_a_chain_or_artifact_list_that_is_not_an_array() {
    let (revisions, artifacts) = chain();
    let chain_text = Value::Array(revisions.clone()).to_string();
    assert_eq!(
        said(verify_revision_chain_json(r#"{"0": {}}"#, None)),
        ("invalid_revision", "a chain is a JSON array".into())
    );
    assert_eq!(
        said(verify_revision_chain_json(
            &chain_text,
            Some(r#"{"1": {}}"#)
        )),
        ("invalid_artifact", "artifacts is a JSON array".into())
    );
    assert_eq!(
        verify_revision_chain_json(&chain_text, Some("null")).unwrap(),
        "[]"
    );
    let mut odd = artifacts.clone();
    odd[1] = json!("x");
    odd[2] = json!({"n": 9_007_199_254_740_992_u64});
    let issues = parse(
        &verify_revision_chain_json(&chain_text, Some(&Value::Array(odd).to_string())).unwrap(),
    );
    let mismatch = "the artifact's digest is not the one this revision approved";
    assert_eq!(
        issues,
        json!([{"code": "artifact_mismatch", "revision": 1, "detail": mismatch},
               {"code": "artifact_mismatch", "revision": 2, "detail": mismatch}])
    );
}

#[test]
fn a_parent_mismatch_spells_what_it_found_and_expected() {
    let (revisions, _) = chain();
    let mut orphan = revisions.clone();
    orphan[0]["parent"] = json!(7);
    orphan[1]["parent"] = Value::Null;
    let issues =
        parse(&verify_revision_chain_json(&Value::Array(orphan).to_string(), None).unwrap());
    let details = issues
        .as_array()
        .unwrap()
        .iter()
        .filter(|issue| issue["code"] == "parent_mismatch")
        .map(|issue| issue["detail"].as_str().unwrap().to_owned())
        .collect::<Vec<_>>();
    assert_eq!(details[0], "parent 7, expected null");
    assert!(
        details[1].starts_with("parent null, expected sha256:"),
        "{}",
        details[1]
    );
}
