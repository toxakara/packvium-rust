//! 1.10: a request may declare its currency, a rate table may say which it prices in, and the
//! result echoes the request's. Nothing converts, prices or ranks by it.
//!
//! The cross-engine contract is `conformance/request_errors/run.py` and
//! `conformance/result_validation/run.py`; this suite pins the same rules from outside the
//! crate, through `pack_json`, `pack_request` and `validate_json`.

use packvium_core::{
    Container, Dimensions, Item, Length, PackError, PackingConfig, PackingRequest, RequestError,
    Weight, pack_json, pack_request, validate_json,
};
use serde_json::{Value, json};

const THREE_LETTERS: &str = "must be three capital letters (ISO 4217)";

/// A request currency, a rate-table currency, and the reason, pointer and detail they earn.
type Refusal = (
    Option<Value>,
    Option<Value>,
    &'static str,
    &'static str,
    &'static str,
);

fn json_request(currency: Option<Value>, table: Option<Value>) -> Value {
    let mut request = json!({
        "items": [{ "id": "cube", "quantity": 2, "weight": "1000",
                    "dimensions": { "length": "100", "width": "100", "height": "100" } }],
        "containers": [{ "id": "box",
                         "inner_dimensions": { "length": "200", "width": "100", "height": "200" },
                         "rate_table": { "weight_brackets_g": [5000], "prices_minor": [900] } }],
        "configuration": { "solver_profile": "fast" }
    });
    if let Some(currency) = currency {
        request["currency"] = currency;
    }
    if let Some(table) = table {
        request["containers"][0]["rate_table"]["currency"] = table;
    }
    request
}

fn pack(request: &Value) -> Value {
    let text = pack_json(&request.to_string()).expect("the request packs");
    serde_json::from_str(&text).expect("a JSON result")
}

fn refused(request: &Value) -> RequestError {
    match pack_json(&request.to_string()) {
        Err(PackError::InvalidRequest(error)) => error,
        Err(other) => panic!("refused, but not as a request error: {other}"),
        Ok(_) => panic!("{request} was admitted"),
    }
}

fn typed_request(currency: Option<&str>) -> PackingRequest {
    let side = |millimetres: i64| Length(millimetres * Length::TICKS_PER_MM);
    let cube = Dimensions {
        length: side(100),
        width: side(100),
        height: side(100),
    };
    let box_ = Dimensions {
        length: side(200),
        width: side(100),
        height: side(200),
    };
    PackingRequest {
        items: vec![Item::new(
            "cube",
            cube,
            Weight(1000 * Weight::TICKS_PER_G),
            2,
        )],
        containers: vec![Container::new("box", box_)],
        config: PackingConfig::default(),
        output_length_unit: "mm".into(),
        output_weight_unit: "g".into(),
        catalog_versions_used: Vec::new(),
        currency: currency.map(str::to_owned),
        fixed_placements: Vec::new(),
        fixed_containers: Vec::new(),
    }
}

#[test]
fn the_result_echoes_the_request_currency_on_the_answer_and_every_alternative() {
    // An obstacle and two item shapes give the quality portfolio distinct runners-up.
    let request = json!({
        "currency": "EUR",
        "items": [
            { "id": "cube", "quantity": 4, "weight": "500",
              "dimensions": { "length": "100", "width": "100", "height": "100" } },
            { "id": "slab", "quantity": 2, "weight": "500",
              "dimensions": { "length": "100", "width": "100", "height": "50" } }
        ],
        "containers": [{
            "id": "crate",
            "inner_dimensions": { "length": "400", "width": "200", "height": "400" },
            "obstacles": [{ "id": "shelf", "origin": { "x": "200", "y": "0", "z": "0" },
                            "dimensions": { "length": "100", "width": "100", "height": "100" } }],
            "rate_table": { "weight_brackets_g": [5000], "prices_minor": [900], "currency": "EUR" }
        }],
        "configuration": { "solver_profile": "quality", "alternatives": 3,
                           "effort_budget": { "max_search_nodes": 20000 }, "time_limit_ms": 60000 }
    });
    let result = pack(&request);
    assert_eq!(result["currency"], "EUR");
    let alternatives = result["alternatives"].as_array().expect("a list");
    assert!(!alternatives.is_empty());
    assert!(
        alternatives
            .iter()
            .all(|alternative| alternative["currency"] == "EUR")
    );
    let issues = validate_json(&request.to_string(), &result.to_string()).expect("judged");
    assert!(issues.is_empty(), "{issues:?}");
}

#[test]
fn a_request_without_a_currency_keeps_its_bytes() {
    assert!(pack(&json_request(None, None)).get("currency").is_none());
    assert!(
        pack(&json_request(Some(Value::Null), None))
            .get("currency")
            .is_none()
    );
}

#[test]
fn the_currency_changes_nothing_but_the_echo() {
    let mut plain = pack(&json_request(None, None));
    let mut declared = pack(&json_request(Some(json!("JPY")), None));
    for result in [&mut plain, &mut declared] {
        result["algorithm"]["duration_ms"] = json!(0);
    }
    declared
        .as_object_mut()
        .expect("an object")
        .remove("currency");
    assert_eq!(declared, plain);
}

#[test]
fn a_bad_currency_is_refused_at_its_pointer() {
    let cases: [Refusal; 8] = [
        (
            Some(json!(978)),
            None,
            "wrong_type",
            "/currency",
            "must be a string",
        ),
        (
            Some(json!("usd")),
            None,
            "invalid_value",
            "/currency",
            THREE_LETTERS,
        ),
        (
            Some(json!("USD\n")),
            None,
            "invalid_value",
            "/currency",
            THREE_LETTERS,
        ),
        (
            Some(json!("ÜSD")),
            None,
            "invalid_value",
            "/currency",
            THREE_LETTERS,
        ),
        (
            Some(json!("USD")),
            Some(json!(840)),
            "wrong_type",
            "/containers/0/rate_table/currency",
            "must be a string",
        ),
        (
            Some(json!("USD")),
            Some(json!("EURO")),
            "invalid_value",
            "/containers/0/rate_table/currency",
            THREE_LETTERS,
        ),
        (
            None,
            Some(json!("USD")),
            "missing_field",
            "/currency",
            "is required when a rate_table declares a currency",
        ),
        (
            Some(json!("USD")),
            Some(json!("EUR")),
            "not_allowed",
            "/containers/0/rate_table/currency",
            "must equal the request currency \"USD\"",
        ),
    ];
    for (currency, table, reason, field, detail) in cases {
        let error = refused(&json_request(currency, table));
        assert_eq!(
            (error.reason(), error.field(), error.detail()),
            (reason, field, detail)
        );
    }
}

#[test]
fn the_request_currency_is_checked_before_the_configuration() {
    let mut request = json_request(Some(json!("usd")), None);
    request["configuration"]["time_limit_ms"] = json!(0);
    assert_eq!(refused(&request).field(), "/currency");
}

#[test]
fn a_rate_table_may_leave_its_currency_to_the_request() {
    assert_eq!(
        pack(&json_request(Some(json!("USD")), None))["currency"],
        "USD"
    );
    assert_eq!(
        pack(&json_request(Some(json!("USD")), Some(json!("USD"))))["currency"],
        "USD"
    );
}

#[test]
fn the_typed_request_carries_its_currency_to_the_result() {
    let result = pack_request(&typed_request(Some("EUR"))).expect("packs");
    assert_eq!(result.currency.as_deref(), Some("EUR"));
    assert_eq!(
        pack_request(&typed_request(None)).expect("packs").currency,
        None
    );
}

#[test]
fn the_typed_request_refuses_a_misshapen_currency_as_the_json_path_does() {
    match pack_request(&typed_request(Some("eur"))) {
        Err(PackError::InvalidRequest(error)) => assert_eq!(
            (error.reason(), error.field(), error.detail()),
            ("invalid_value", "/currency", THREE_LETTERS)
        ),
        other => panic!("expected a request error, got {other:?}"),
    }
}

#[test]
fn the_validator_names_a_currency_the_result_misreports() {
    let request = json_request(Some(json!("EUR")), None);
    let mut result = pack(&request);
    result["currency"] = json!("USD");
    let issues = validate_json(&request.to_string(), &result.to_string()).expect("judged");
    let mismatch = serde_json::to_value(&issues).expect("serialisable");
    assert_eq!(
        mismatch,
        json!([{ "code": "currency_mismatch", "detail": "reported 'USD', requested 'EUR'" }])
    );
    result
        .as_object_mut()
        .expect("an object")
        .remove("currency");
    let issues = validate_json(&request.to_string(), &result.to_string()).expect("judged");
    assert_eq!(
        serde_json::to_value(&issues).expect("serialisable"),
        json!([{ "code": "currency_mismatch", "detail": "reported None, requested 'EUR'" }])
    );
}
