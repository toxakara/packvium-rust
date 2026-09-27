//! Every request integer below its schema `minimum` is refused, not defaulted.
//!
//! The parser used to read these with `as_u64`/`as_i64` and fall back to the default when a
//! value was negative or of the wrong type, so `max_containers: -1` packed as if no budget
//! were given and a mistyped sign never reached the caller. The cross-engine contract is
//! `conformance/request_errors/run.py`; this suite pins the Rust half from outside through
//! `pack_json`, the path a request actually takes.

use packvium_core::{PackError, RequestError};
use serde_json::{Value, json};

fn base() -> Value {
    json!({
        "items": [{ "id": "cube", "quantity": 2, "weight": "1000",
                    "dimensions": { "length": "100", "width": "100", "height": "100" } }],
        "containers": [{ "id": "box", "quantity": 2,
                         "inner_dimensions": { "length": "200", "width": "100", "height": "200" } }],
        "configuration": { "solver_profile": "fast" }
    })
}

fn with_rate_table(fields: Value) -> Value {
    let mut table = json!({ "weight_brackets_g": [1000, 5000], "prices_minor": [500, 900] });
    for (key, value) in fields.as_object().expect("fields are an object") {
        table[key] = value.clone();
    }
    let mut request = base();
    request["containers"][0]["rate_table"] = table;
    request
}

fn edited(section: &str, key: &str, value: Value) -> Value {
    let mut request = base();
    let target = match section {
        "item" => &mut request["items"][0],
        "container" => &mut request["containers"][0],
        _ => &mut request["configuration"],
    };
    target[key] = value;
    request
}

fn refusal(request: &Value) -> RequestError {
    match packvium_core::pack_json(&request.to_string()) {
        Err(PackError::InvalidRequest(error)) => error,
        Err(other) => panic!("refused, but not as a request error: {other}"),
        Ok(_) => panic!("a number below its floor must be refused"),
    }
}

/// The JSON Pointer `edited` writes to.
fn pointer(section: &str, key: &str) -> String {
    match section {
        "item" => format!("/items/0/{key}"),
        "container" => format!("/containers/0/{key}"),
        _ => format!("/configuration/{key}"),
    }
}

const FLOORS: &[(&str, &str, i64)] = &[
    ("item", "quantity", 1),
    ("container", "quantity", 1),
    ("container", "max_items", 1),
    ("container", "cost_minor", 0),
    ("configuration", "time_limit_ms", 1),
    ("configuration", "alternatives", 1),
    ("configuration", "max_containers", 1),
    ("configuration", "exact_item_limit", 1),
    ("configuration", "multi_start_orders", 1),
    ("configuration", "max_candidates_per_item", 1),
    ("configuration", "max_candidate_points", 16),
    ("configuration", "container_plan_beam_width", 1),
    ("configuration", "container_plan_node_limit", 1),
];

#[test]
fn the_unedited_request_is_admitted() {
    packvium_core::pack_json(&base().to_string()).expect("the base request packs");
}

#[test]
fn a_value_at_each_floor_is_admitted() {
    for (section, key, floor) in FLOORS {
        packvium_core::pack_json(&edited(section, key, json!(floor)).to_string())
            .unwrap_or_else(|error| panic!("{section}.{key} = {floor} was refused: {error}"));
    }
}

#[test]
fn every_value_below_its_floor_is_refused_by_name() {
    for (section, key, floor) in FLOORS {
        for value in [floor - 1, -1] {
            let error = refusal(&edited(section, key, json!(value)));
            assert_eq!(
                (error.reason(), error.field(), error.detail()),
                (
                    "below_minimum",
                    pointer(section, key).as_str(),
                    format!("must be at least {floor}").as_str()
                ),
                "{section}.{key} = {value}"
            );
        }
    }
}

#[test]
fn a_wrong_typed_value_is_refused_rather_than_defaulted() {
    for (section, key, _) in FLOORS {
        for value in [json!("3"), json!(true), json!(2.5)] {
            let error = refusal(&edited(section, key, value.clone()));
            assert_eq!(
                (error.reason(), error.field()),
                ("wrong_type", pointer(section, key).as_str()),
                "{section}.{key} = {value}"
            );
        }
    }
}

#[test]
fn an_explicit_null_keeps_the_default() {
    for (section, key, _) in FLOORS {
        packvium_core::pack_json(&edited(section, key, Value::Null).to_string())
            .unwrap_or_else(|error| panic!("{section}.{key} = null was refused: {error}"));
    }
}

#[test]
fn a_zero_item_quantity_is_its_own_error_not_a_bad_id() {
    let error = refusal(&edited("item", "quantity", json!(0)));
    assert_eq!(
        error.to_string(),
        "invalid_request: /items/0/quantity: must be at least 1"
    );
}

#[test]
fn the_candidate_point_floor_is_sixteen() {
    let error = refusal(&edited("configuration", "max_candidate_points", json!(15)));
    assert_eq!(error.detail(), "must be at least 16");
}

#[test]
fn negative_rate_table_money_is_refused() {
    for (fields, field) in [
        (json!({ "prices_minor": [-1, 900] }), "prices_minor/0"),
        (
            json!({ "minimum_charge_minor": -1 }),
            "minimum_charge_minor",
        ),
        (
            json!({ "fuel_surcharge_permille": -1 }),
            "fuel_surcharge_permille",
        ),
    ] {
        let error = refusal(&with_rate_table(fields.clone()));
        assert_eq!(
            (error.reason(), error.field()),
            (
                "below_minimum",
                format!("/containers/0/rate_table/{field}").as_str()
            ),
            "{fields}"
        );
    }
}

#[test]
fn zero_rate_table_money_is_admitted() {
    let request = with_rate_table(json!({
        "prices_minor": [0, 0], "minimum_charge_minor": 0, "fuel_surcharge_permille": 0
    }));
    packvium_core::pack_json(&request.to_string()).expect("zero money is a price");
}
