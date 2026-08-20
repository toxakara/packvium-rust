//! The guards `commerce.rs` and `commerce_document.rs` leave unreached.
//!
//! Those two files cover the shared fixtures and the tariff-side parser. Everything
//! below is what neither reaches: the per-field guards inside the *nested* catalog and
//! policy parsers, the request envelopes of the two non-quote operations, and the small
//! pure functions the JSON path only ever exercises along one branch. A parser whose
//! nested guards are never tested is one that accepts a malformed carton three levels
//! down and then prices against it.

use packvium_core::commerce::catalog::{CatalogRegistry, Snapshot};
use packvium_core::commerce::policy::{PolicyOperator, Predicate};
use packvium_core::commerce::rating::AccessorialCharge;
use packvium_core::commerce::{
    PolicyScope, catalog_version_info_json, evaluate_policy_json, quote_json,
};
use serde_json::{Map, Value, json};

// --------------------------------------------------------------------------- helpers

/// A document whose tariff half is always well-formed, so a catalog or policy guard is
/// the only thing a test can trip.
fn document(extra: Value) -> Value {
    let mut fields = json!({
        "tariffs": [{
            "carrier_id": "acme",
            "service_id": "ground",
            "versions": [{
                "effective_at": 0,
                "dimensional_weight_divisor": 5000,
                "cost_per_dimensional_kg_minor": {"zone-a": 450},
            }],
        }],
    });
    let target = fields
        .as_object_mut()
        .expect("the base document is an object");
    for (key, value) in extra.as_object().expect("extra fields are an object") {
        target.insert(key.clone(), value.clone());
    }
    fields
}

fn call(document: Value, request: Value) -> String {
    json!({"document": document, "request": request}).to_string()
}

/// One well-formed catalog version, so a test can break exactly one nested field.
fn snapshot(part: &str, entry: Value) -> Value {
    json!({
        "catalogs": [{
            "catalog_id": "dc-1",
            "versions": [{
                "effective_at": 0,
                "published_at": 0,
                "snapshot": {part: [entry]},
            }],
        }],
    })
}

fn catalog_request() -> Value {
    json!({"catalog_id": "dc-1", "version": 1, "resolved_at": 0})
}

#[track_caller]
fn catalog_refused(document: Value, fragment: &str) {
    let call = call(document, catalog_request());
    match catalog_version_info_json(&call) {
        Ok(answer) => panic!("expected a refusal mentioning {fragment:?}, got {answer}"),
        Err(error) => assert!(
            error.to_string().contains(fragment),
            "expected {fragment:?} in {error}",
        ),
    }
}

// ------------------------------------------------------------- nested catalog parsers

/// Every nested master-data parser validates its own key set. Without this, an unknown
/// key three levels inside a snapshot is silently ignored rather than named.
#[test]
fn each_nested_catalog_parser_names_its_own_unknown_key() {
    let cases = [
        (
            "items",
            json!({"id": "s", "dimensions_mm": [1, 1, 1], "weight_g": 1, "typo": 1}),
        ),
        (
            "cartons",
            json!({"id": "c", "inner_dimensions_mm": [1, 1, 1],
                           "max_payload_g": 1, "typo": 1}),
        ),
        (
            "pallets",
            json!({"id": "p", "deck_dimensions_mm": [1, 1],
                           "max_payload_g": 1, "typo": 1}),
        ),
        (
            "exclusions",
            json!({"id": "x", "scope": "item_carton", "subject_id": "s",
                              "excluded_id": "c", "typo": 1}),
        ),
        (
            "overrides",
            json!({"id": "o", "facility_id": "f", "entry_id": "c",
                             "kind": "carton", "typo": 1,
                             "override": {"id": "c", "inner_dimensions_mm": [1, 1, 1],
                                          "max_payload_g": 1}}),
        ),
    ];

    for (part, entry) in cases {
        catalog_refused(snapshot(part, entry), "unrecognised key");
    }
}

#[test]
fn every_out_of_range_master_data_field_is_named() {
    let cases = [
        (
            "items",
            json!({"id": "s", "dimensions_mm": [0, 1, 1], "weight_g": 1}),
            "item dimensions must be positive",
        ),
        (
            "items",
            json!({"id": "s", "dimensions_mm": [1, 1, 1], "weight_g": 0}),
            "item weight must be positive",
        ),
        (
            "cartons",
            json!({"id": "c", "inner_dimensions_mm": [1, 0, 1], "max_payload_g": 1}),
            "carton dimensions must be positive",
        ),
        (
            "cartons",
            json!({"id": "c", "inner_dimensions_mm": [1, 1, 1], "max_payload_g": 0}),
            "carton max_payload_g must be positive",
        ),
        (
            "cartons",
            json!({"id": "c", "inner_dimensions_mm": [1, 1, 1],
                           "max_payload_g": 1, "cost_minor": -1}),
            "cost_minor cannot be negative",
        ),
        (
            "pallets",
            json!({"id": "p", "deck_dimensions_mm": [0, 1], "max_payload_g": 1}),
            "pallet dimensions must be positive",
        ),
        (
            "pallets",
            json!({"id": "p", "deck_dimensions_mm": [1, 1], "max_payload_g": 0}),
            "pallet max_payload_g must be positive",
        ),
        (
            "pallets",
            json!({"id": "p", "deck_dimensions_mm": [1, 1],
                           "max_payload_g": 1, "max_stack_height_mm": 0}),
            "max_stack_height_mm must be positive",
        ),
    ];

    for (part, entry, fragment) in cases {
        catalog_refused(snapshot(part, entry), fragment);
    }
}

/// Uniqueness is checked per master-data kind, not once over a merged pool, so each
/// kind needs its own case -- a single shared check would pass all five of these.
#[test]
fn duplicate_ids_are_refused_within_each_master_data_kind() {
    let duplicated = [
        (
            "items",
            json!({"id": "d", "dimensions_mm": [1, 1, 1], "weight_g": 1}),
            "item",
        ),
        (
            "cartons",
            json!({"id": "d", "inner_dimensions_mm": [1, 1, 1], "max_payload_g": 1}),
            "carton",
        ),
        (
            "pallets",
            json!({"id": "d", "deck_dimensions_mm": [1, 1], "max_payload_g": 1}),
            "pallet",
        ),
        (
            "exclusions",
            json!({"id": "d", "scope": "item_carton", "subject_id": "s",
                              "excluded_id": "c"}),
            "exclusion",
        ),
    ];

    for (part, entry, label) in duplicated {
        let document = json!({
            "catalogs": [{
                "catalog_id": "dc-1",
                "versions": [{
                    "effective_at": 0,
                    "published_at": 0,
                    "snapshot": {part: [entry.clone(), entry]},
                }],
            }],
        });

        catalog_refused(
            document,
            &format!("duplicate {label} ids in catalog snapshot"),
        );
    }
}

#[test]
fn a_catalog_history_is_validated_before_any_version_is_read() {
    catalog_refused(
        json!({"catalogs": [{"catalog_id": "", "versions": [{"effective_at": 0,
               "published_at": 0, "snapshot": {}}]}]}),
        "catalog_id is required",
    );
    catalog_refused(
        json!({"catalogs": [{"catalog_id": "dc-1", "versions": []}]}),
        "at least one version",
    );
    catalog_refused(
        json!({"catalogs": [
            {"catalog_id": "dc-1", "versions": [{"effective_at": 0, "published_at": 0,
             "snapshot": {}}]},
            {"catalog_id": "dc-1", "versions": [{"effective_at": 0, "published_at": 0,
             "snapshot": {}}]},
        ]}),
        "duplicate catalog history for 'dc-1'",
    );
    // A non-string id must be refused as a shape error before the emptiness check.
    catalog_refused(
        json!({"catalogs": [{"catalog_id": 7, "versions": []}]}),
        "expected a string",
    );
}

#[test]
fn a_catalog_version_and_its_snapshot_each_validate_their_key_set() {
    catalog_refused(
        json!({"catalogs": [{"catalog_id": "dc-1", "versions": [{"effective_at": 0,
               "published_at": 0, "snapshot": {}, "typo": 1}]}]}),
        "unrecognised key",
    );
    catalog_refused(
        json!({"catalogs": [{"catalog_id": "dc-1", "versions": [{"effective_at": 0,
               "published_at": 0, "snapshot": {"typo": []}}]}]}),
        "unrecognised key",
    );
    // The rollback branch has a key set of its own, taken before the ordinary one.
    catalog_refused(
        json!({"catalogs": [{"catalog_id": "dc-1", "versions": [
            {"effective_at": 0, "published_at": 0, "snapshot": {}},
            {"rollback_to": 1, "published_at": 1, "typo": 1},
        ]}]}),
        "unrecognised key",
    );
}

/// `description` is the one optional string on a master-data entry, so its absent branch
/// is only taken when an item omits it entirely.
#[test]
fn an_item_without_a_description_is_accepted() {
    let call = call(
        snapshot(
            "items",
            json!({"id": "s", "dimensions_mm": [1, 1, 1], "weight_g": 1}),
        ),
        catalog_request(),
    );

    let answer = catalog_version_info_json(&call).expect("a description is optional");
    assert!(answer.contains("\"item_ids\":[\"s\"]"), "got {answer}");
}

/// Facility overrides are the fifth uniqueness check, and the only one whose label is
/// two words -- a copy-pasted check reusing another kind's iterator would still pass the
/// other four.
#[test]
fn duplicate_facility_override_ids_are_refused() {
    let entry = json!({"id": "o", "facility_id": "f", "entry_id": "c", "kind": "carton",
                       "override": {"id": "c", "inner_dimensions_mm": [1, 1, 1],
                                    "max_payload_g": 1}});
    let document = json!({
        "catalogs": [{
            "catalog_id": "dc-1",
            "versions": [{
                "effective_at": 0,
                "published_at": 0,
                "snapshot": {"overrides": [entry.clone(), entry]},
            }],
        }],
    });

    catalog_refused(
        document,
        "duplicate facility override ids in catalog snapshot",
    );
}

/// `dimensions_mm` is a fixed-length axis tuple, not a free list. A wrong arity has to
/// be refused where it is read, or a two-axis carton reaches the packer as a three-axis
/// one with a garbage third value.
#[test]
fn an_axis_tuple_of_the_wrong_shape_is_refused() {
    for dimensions in [
        json!([1, 1]),
        json!([1, 1, 1, 1]),
        json!("1x1x1"),
        json!(null),
    ] {
        catalog_refused(
            snapshot(
                "cartons",
                json!({"id": "c", "inner_dimensions_mm": dimensions, "max_payload_g": 1}),
            ),
            "expected",
        );
    }
}

// -------------------------------------------------------------------- tariff internals

#[track_caller]
fn quote_refused(document: Value, fragment: &str) {
    let request = json!({"carrier_id": "acme", "service_id": "ground", "tariff_version": 1,
                         "zone": "zone-a", "actual_weight_g": 1000, "volume_mm3": 1});
    match quote_json(&call(document, request)) {
        Ok(answer) => panic!("expected a refusal mentioning {fragment:?}, got {answer}"),
        Err(error) => assert!(
            error.to_string().contains(fragment),
            "expected {fragment:?} in {error}",
        ),
    }
}

fn tariff(version: Value) -> Value {
    json!({"tariffs": [{"carrier_id": "acme", "service_id": "ground",
                        "versions": [version]}]})
}

/// A tariff version and its accessorial list each police their own key set and field
/// types. These are the fields a quote is computed from, so a silently ignored key here
/// is a silently wrong price.
#[test]
fn a_tariff_version_validates_its_own_keys_and_field_types() {
    quote_refused(
        tariff(
            json!({"effective_at": 0, "dimensional_weight_divisor": 5000,
                      "cost_per_dimensional_kg_minor": {"zone-a": 1}, "typo": 1}),
        ),
        "unrecognised key",
    );
    quote_refused(
        tariff(
            json!({"effective_at": 0, "dimensional_weight_divisor": "5000",
                      "cost_per_dimensional_kg_minor": {"zone-a": 1}}),
        ),
        "expected an exact integer",
    );
    quote_refused(
        tariff(
            json!({"effective_at": 0, "dimensional_weight_divisor": 5000,
                      "cost_per_dimensional_kg_minor": {"zone-a": 1},
                      "accessorials": [{"accessorial_id": "lift", "flat_charge_minor": 1,
                                        "typo": 1}]}),
        ),
        "unrecognised key",
    );
}

/// A predicate carries its own scope, which must both parse and match the rule's. The
/// parse failure is a separate branch from the mismatch, and only the mismatch was
/// reachable through the fixture corpus.
#[test]
fn a_predicate_scope_must_parse_before_it_can_be_compared() {
    let document = json!({"policy_rules": [{"rule_id": "r", "versions": [
        {"scope": "carrier", "action": "allow", "priority": 0, "effective_at": 0,
         "predicates": [{"scope": "nowhere", "field": "f", "operator": "exists"}]},
    ]}]});
    let request = json!({"scope": "carrier", "context": {}, "as_of": 0});

    let error = evaluate_policy_json(&call(document, request))
        .expect_err("an unparseable predicate scope is an input error");

    assert!(
        error
            .to_string()
            .contains("unsupported policy scope 'nowhere'"),
        "got {error}",
    );
}

// -------------------------------------------------------------- nested policy parsers

#[test]
fn a_policy_rule_version_validates_its_own_key_set_and_scope() {
    let rule = |version: Value| json!({"policy_rules": [{"rule_id": "r", "versions": [version]}]});
    let request = json!({"scope": "carrier", "context": {}, "as_of": 0});
    let refused = |document: Value, fragment: &str| {
        let call = call(document, request.clone());
        match evaluate_policy_json(&call) {
            Ok(answer) => panic!("expected a refusal mentioning {fragment:?}, got {answer}"),
            Err(error) => assert!(
                error.to_string().contains(fragment),
                "expected {fragment:?} in {error}",
            ),
        }
    };
    let predicates = json!([{"scope": "carrier", "field": "f", "operator": "exists"}]);

    refused(
        rule(json!({"scope": "carrier", "action": "allow", "priority": 0,
                    "effective_at": 0, "predicates": predicates, "typo": 1})),
        "unrecognised key",
    );
    refused(
        rule(json!({"scope": "nowhere", "action": "allow", "priority": 0,
                    "effective_at": 0, "predicates": predicates})),
        "unsupported policy scope 'nowhere'",
    );
    refused(
        rule(json!({"scope": "carrier", "action": "allow", "priority": 0,
                    "effective_at": -1, "predicates": predicates})),
        "effective_at cannot be negative",
    );
    refused(
        json!({"policy_rules": [
            {"rule_id": "r", "versions": [{"scope": "carrier", "action": "allow",
             "priority": 0, "effective_at": 0, "predicates": predicates}]},
            {"rule_id": "r", "versions": [{"scope": "carrier", "action": "allow",
             "priority": 0, "effective_at": 0, "predicates": predicates}]},
        ]}),
        "duplicate rule history for 'r'",
    );
    refused(
        json!({"policy_rules": [{"rule_id": "r", "versions": []}]}),
        "at least one version",
    );
}

// ------------------------------------------------------------------ request envelopes

/// `quote`'s envelope is covered elsewhere; the other two operations have key sets of
/// their own, and an unchecked one would accept a misspelled pin and answer as if the
/// caller had not asked for it.
#[test]
fn the_policy_and_catalog_request_envelopes_are_validated() {
    let document = document(json!({}));

    assert!(
        evaluate_policy_json(&call(
            document.clone(),
            json!({"scope": "carrier", "context": {}, "as_of": 0, "typo": 1}),
        ))
        .is_err_and(|error| error.to_string().contains("unrecognised key")),
    );
    assert!(
        evaluate_policy_json(&call(document.clone(), json!({"scope": "carrier"})))
            .is_err_and(|error| error.to_string().contains("missing required key")),
    );
    assert!(
        catalog_version_info_json(&call(
            document.clone(),
            json!({"catalog_id": "dc-1", "resolved_at": 0, "version": 1, "typo": 1}),
        ))
        .is_err_and(|error| error.to_string().contains("unrecognised key")),
    );
    assert!(
        catalog_version_info_json(&call(document, json!({"catalog_id": "dc-1"})))
            .is_err_and(|error| error.to_string().contains("missing required key")),
    );
}

/// An unknown carrier is a structured rejection on both pin forms. The `tariff_version`
/// form is covered by the fixtures; the effective-dated form takes a different branch,
/// which had been reachable only through a code path no fixture exercised.
#[test]
fn an_unknown_carrier_is_rejected_on_the_effective_dated_pin_too() {
    let answer = quote_json(&call(
        document(json!({})),
        json!({"carrier_id": "nobody", "service_id": "ground", "as_of": 0,
               "zone": "zone-a", "actual_weight_g": 1000, "volume_mm3": 1}),
    ))
    .expect("an unknown carrier is an answer, not an input error");

    assert!(
        answer.contains("\"code\":\"tariff_not_found\""),
        "got {answer}"
    );
    assert!(
        !answer.contains("\"as_of\""),
        "the unresolved form carries no pin: {answer}"
    );
}

// ------------------------------------------------------------------- pure model units

/// The JSON parser refuses an accessorial carrying neither charge, so this arm is
/// reachable only by constructing one directly -- and it must be zero, not a panic.
#[test]
fn an_accessorial_with_no_charge_at_all_costs_nothing() {
    let charge = AccessorialCharge {
        accessorial_id: "ghost".to_owned(),
        flat_charge_minor: None,
        permille_of_base: None,
    };

    assert_eq!(charge.charge_minor(100_000), 0);
}

#[test]
fn a_catalog_registry_reports_its_published_versions() {
    let mut registry = CatalogRegistry::new("dc-1".to_owned());
    assert!(registry.versions().is_empty());

    registry.publish(Snapshot::default(), 0, 0, "first".to_owned());
    registry.publish(Snapshot::default(), 10, 10, "second".to_owned());

    let numbers: Vec<i64> = registry
        .versions()
        .iter()
        .map(|entry| entry.number)
        .collect();
    assert_eq!(
        numbers,
        vec![1, 2],
        "numbering is 1-based position in the history"
    );
}

/// Every scope must round-trip through its own wire spelling. A missing arm here would
/// surface as one scope silently reported as another in a decision document.
#[test]
fn every_policy_scope_round_trips_through_its_wire_name() {
    for name in [
        "facility",
        "customer",
        "carrier",
        "material",
        "hazmat",
        "temperature",
        "service",
    ] {
        let scope = PolicyScope::parse(name).expect("a documented scope must parse");
        assert_eq!(scope.as_str(), name);
    }
    assert!(PolicyScope::parse("nowhere").is_none());
}

fn context(entries: &[(&str, Value)]) -> Map<String, Value> {
    entries
        .iter()
        .map(|(key, value)| ((*key).to_owned(), value.clone()))
        .collect()
}

fn predicate(operator: PolicyOperator, value: Value) -> Predicate {
    Predicate {
        scope: PolicyScope::Carrier,
        field: "f".to_owned(),
        operator,
        value,
    }
}

/// The negated operators are not the positive ones with a `!` bolted on at the call
/// site -- they are separate arms, and a field the context does not carry at all must
/// not satisfy either of them.
#[test]
fn the_negated_operators_are_evaluated_on_their_own_arms() {
    let present = context(&[("f", json!("x"))]);
    let absent = context(&[]);

    assert!(predicate(PolicyOperator::NotEquals, json!("y")).matches(&present));
    assert!(!predicate(PolicyOperator::NotEquals, json!("x")).matches(&present));
    assert!(predicate(PolicyOperator::NotIn, json!(["y", "z"])).matches(&present));
    assert!(!predicate(PolicyOperator::NotIn, json!(["x", "z"])).matches(&present));

    // A missing field matches nothing, including a negation -- `not_equals` on an
    // absent field must not read as "true, it differs".
    assert!(!predicate(PolicyOperator::NotEquals, json!("y")).matches(&absent));
    assert!(!predicate(PolicyOperator::NotIn, json!(["y"])).matches(&absent));
}

/// Value equality has one deliberate asymmetry with JSON's own type system: a boolean
/// equals the integer it stands for. Every other cross-type pair must stay unequal.
#[test]
fn value_equality_covers_every_scalar_pairing() {
    let matches = |value: Value, actual: Value| {
        predicate(PolicyOperator::Equals, value).matches(&context(&[("f", actual)]))
    };

    assert!(matches(json!(true), json!(true)));
    assert!(!matches(json!(true), json!(false)));
    assert!(
        matches(json!(1), json!(true)),
        "a boolean equals the integer it stands for"
    );
    assert!(
        matches(json!(true), json!(1)),
        "and the comparison is symmetric"
    );
    assert!(!matches(json!(2), json!(true)));
    assert!(matches(json!(null), json!(null)));
    assert!(matches(json!([1, "a"]), json!([1, "a"])));
    assert!(
        !matches(json!([1]), json!([1, 2])),
        "length is compared before contents"
    );

    // Cross-type pairs that share no arm at all.
    assert!(!matches(json!("1"), json!(1)));
    assert!(!matches(json!(null), json!(0)));
    assert!(
        !matches(json!({"a": 1}), json!({"a": 1})),
        "objects are never equal"
    );
}

/// `in` accepts a list or a substring search, and must refuse anything else rather than
/// guessing. A number haystack silently reading as "no match" is correct; reading as a
/// panic or as a match is not.
#[test]
fn membership_is_defined_only_over_lists_and_strings() {
    let matches = |haystack: Value, actual: Value| {
        predicate(PolicyOperator::In, haystack).matches(&context(&[("f", actual)]))
    };

    assert!(matches(json!(["a", "b"]), json!("a")));
    assert!(
        matches(json!("haystack"), json!("stack")),
        "a string haystack is a substring test"
    );
    assert!(
        !matches(json!("haystack"), json!(7)),
        "a non-string needle cannot be a substring"
    );
    assert!(!matches(json!(7), json!(7)), "a number is not a container");
    assert!(
        !matches(json!({"a": 1}), json!("a")),
        "an object is not a container"
    );
    assert!(!matches(json!(null), json!(null)));
}
