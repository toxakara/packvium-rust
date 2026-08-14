//! Versioned eligibility rules compiled into the Rust constraint pipeline.
//!
//! Every assertion here is about a *packing*, not about a parsed rule set: a rule that is
//! resolved correctly and then never consulted is indistinguishable, from the outside,
//! from one that was ignored. Each test packs the same request twice -- once with the
//! rule participating and once without -- and asserts the two answers differ in the way
//! the rule says they should.
//!
//! Deliberately an integration test, outside `src/`: it exercises the crate through
//! `pack_json`, the same entry point the conformance harness drives, so nothing here can
//! reach past the public surface to check a private detail instead of a behaviour.

use packvium_core::pack_json;
use serde_json::{Value, json};

const AS_OF: i64 = 1_704_067_200_000;

fn request(rules: Value, items: Value, containers: Value) -> Value {
    json!({
        "units": {"length": "mm"},
        "configuration": {"time_limit_ms": 300000},
        "policy": {
            "as_of": AS_OF,
            "shipment": {"facility": "SEA1", "carrier": "ups"},
            "rules": rules,
        },
        "items": items,
        "containers": containers,
    })
}

fn cube(id: &str, quantity: i64, tags: Value) -> Value {
    json!({
        "id": id, "quantity": quantity, "tags": tags,
        "dimensions": {"length": "100", "width": "100", "height": "100"},
    })
}

fn pallet(quantity: i64, tags: Value) -> Value {
    json!([{
        "id": "pallet", "quantity": quantity, "tags": tags,
        "inner_dimensions": {"length": "300", "width": "300", "height": "300"},
    }])
}

fn pack(request: &Value) -> Value {
    serde_json::from_str(&pack_json(&request.to_string()).expect("the request should pack"))
        .expect("a JSON result")
}

fn containers_used(request: &Value) -> i64 {
    pack(request)["summary"]["container_count"]
        .as_i64()
        .expect("a count")
}

fn without_policy(request: &Value) -> Value {
    let mut stripped = request.clone();
    stripped
        .as_object_mut()
        .expect("an object")
        .remove("policy");
    stripped
}

fn segregation() -> Value {
    json!([{
        "id": "hazmat-food-segregation", "version": 1, "effective_at": AS_OF, "priority": 100,
        "separate_tags": {"tag": "hazmat", "from_tag": "food"},
    }])
}

fn mixed_load() -> Value {
    json!([
        cube("drum", 1, json!(["hazmat"])),
        cube("carton", 1, json!(["food"]))
    ])
}

#[test]
fn a_segregation_rule_opens_a_container_the_geometry_did_not_need() {
    // Both items fit in one pallet by geometry and weight alone, so a second container is
    // the rule's doing and nothing else's.
    let scene = request(segregation(), mixed_load(), pallet(4, json!([])));
    assert_eq!(containers_used(&without_policy(&scene)), 1);
    assert_eq!(containers_used(&scene), 2);
}

#[test]
fn a_container_tag_rule_leaves_an_item_behind_rather_than_routing_it_wrongly() {
    let rules = json!([{
        "id": "cold-chain", "version": 1, "effective_at": AS_OF, "priority": 10,
        "require_container_tag": {"item_tag": "food", "container_tag": "reefer"},
    }]);
    let scene = request(rules.clone(), mixed_load(), pallet(4, json!([])));
    let result = pack(&scene);
    let unpacked = result["unpacked_items"].as_array().expect("an array");
    assert_eq!(unpacked.len(), 1);
    assert_eq!(unpacked[0]["item_type"], "carton");

    // The same request against a container that carries the tag packs everything.
    let tagged = request(rules, mixed_load(), pallet(4, json!(["reefer"])));
    assert_eq!(pack(&tagged)["summary"]["unpacked_item_count"], 0);
}

#[test]
fn a_tag_cap_splits_a_container_the_cap_would_otherwise_overfill() {
    let rules = json!([{
        "id": "lithium-cap", "version": 1, "effective_at": AS_OF, "priority": 10,
        "limit_tag_per_container": {"tag": "lithium", "max": 2},
    }]);
    let scene = request(
        rules,
        json!([cube("cell", 3, json!(["lithium"]))]),
        pallet(4, json!([])),
    );
    assert_eq!(containers_used(&without_policy(&scene)), 1);
    assert_eq!(containers_used(&scene), 2);
}

#[test]
fn a_rule_that_is_not_yet_effective_does_not_participate() {
    let mut scene = request(segregation(), mixed_load(), pallet(4, json!([])));
    scene["policy"]["as_of"] = json!(AS_OF - 1);
    assert_eq!(containers_used(&scene), 1);
}

#[test]
fn a_rule_scoped_to_another_shipment_does_not_participate() {
    for (fact, other) in [("facility", "PDX9"), ("carrier", "fedex")] {
        let mut scene = request(segregation(), mixed_load(), pallet(4, json!([])));
        scene["policy"]["rules"][0]["applies_to"] = json!({fact: other});
        assert_eq!(containers_used(&scene), 1, "{fact} should not match");

        let declared = scene["policy"]["shipment"][fact].clone();
        scene["policy"]["rules"][0]["applies_to"] = json!({fact: declared});
        assert_eq!(containers_used(&scene), 2, "{fact} should match");
    }
}

#[test]
fn a_rule_scoped_to_a_fact_the_shipment_never_declared_does_not_participate() {
    // An undeclared fact is not a wildcard. Treating it as one would let a rule written
    // for one customer silently apply to a shipment that never named a customer at all.
    let mut scene = request(segregation(), mixed_load(), pallet(4, json!([])));
    scene["policy"]["rules"][0]["applies_to"] = json!({"customer": "acme"});
    assert_eq!(containers_used(&scene), 1);
}

#[test]
fn the_highest_effective_version_of_one_id_wins() {
    // Append-only per id: the later version replaces the earlier one rather than both
    // being enforced, which is what makes a published rule set replayable.
    let mut scene = request(segregation(), mixed_load(), pallet(4, json!([])));
    let mut superseding = scene["policy"]["rules"][0].clone();
    superseding["version"] = json!(2);
    superseding["separate_tags"] = json!({"tag": "hazmat", "from_tag": "nothing-here"});
    scene["policy"]["rules"]
        .as_array_mut()
        .expect("an array")
        .push(superseding);
    assert_eq!(containers_used(&scene), 1);

    // A superseding version that is not yet effective leaves the older one in force.
    scene["policy"]["rules"][1]["effective_at"] = json!(AS_OF + 1);
    assert_eq!(containers_used(&scene), 2);
}

#[test]
fn a_rejection_names_the_rule_and_version_that_caused_it() {
    let rules = json!([{
        "id": "cold-chain", "version": 7, "effective_at": AS_OF, "priority": 10,
        "require_container_tag": {"item_tag": "food", "container_tag": "reefer"},
    }]);
    let result = pack(&request(rules, mixed_load(), pallet(4, json!([]))));
    let unpacked = &result["unpacked_items"][0];
    assert_eq!(unpacked["item_type"], "carton");
    assert_eq!(unpacked["reason"], "policy_rule");
    // Version, not just id: replaying a past decision needs to know which text was in
    // force, and two versions of one rule can forbid different things.
    assert_eq!(
        unpacked["details"],
        json!([
            "cold-chain@7: requires a container tagged 'reefer', which none of the \
             containers offered carries"
        ])
    );
    // Proven rather than observed: no search outcome can make the item placeable.
    assert_eq!(unpacked["proof"]["level"], "proven");
    assert_eq!(unpacked["proof"]["observations"][0]["code"], "policy_rule");
}

#[test]
fn a_cap_that_leaves_an_item_behind_is_not_reported_as_proven() {
    // A per-container cap depends on what else was packed, so an item it leaves behind
    // was left behind by the search. Claiming `proven` would assert more than the engine
    // knows, so the generic reason stands and no rule is cited.
    let rules = json!([{
        "id": "lithium-cap", "version": 1, "effective_at": AS_OF, "priority": 10,
        "limit_tag_per_container": {"tag": "lithium", "max": 2},
    }]);
    let scene = request(
        rules,
        json!([cube("cell", 3, json!(["lithium"]))]),
        pallet(1, json!([])),
    );
    let result = pack(&scene);
    assert_eq!(result["summary"]["unpacked_item_count"], 1);
    assert_ne!(result["unpacked_items"][0]["reason"], "policy_rule");
}

#[test]
fn geometry_outranks_policy_in_the_reported_reason() {
    // An item too big for every container is impossible whatever a policy says, and
    // reporting the policy first would send a caller to fix the wrong thing.
    let rules = json!([{
        "id": "cold-chain", "version": 1, "effective_at": AS_OF, "priority": 10,
        "require_container_tag": {"item_tag": "food", "container_tag": "reefer"},
    }]);
    let slab = json!([{
        "id": "slab", "quantity": 1, "tags": ["food"],
        "dimensions": {"length": "9000", "width": "9000", "height": "9000"},
    }]);
    let result = pack(&request(rules, slab, pallet(4, json!([]))));
    assert_eq!(
        result["unpacked_items"][0]["reason"],
        "no_compatible_container_dimensions"
    );
}

#[test]
fn a_malformed_rule_fails_admission_rather_than_being_dropped() {
    // A rule quietly dropped for being malformed would let a request pack in a way its
    // own policy forbids -- the failure the whole contract exists to prevent.
    let cases: [(Value, &str); 12] = [
        (
            json!({"id": "", "version": 1, "effective_at": 0, "priority": 0,
                "separate_tags": {"tag": "a", "from_tag": "b"}}),
            "id must be a non-empty string",
        ),
        (
            json!({"id": "r", "version": 0, "effective_at": 0, "priority": 0,
                "separate_tags": {"tag": "a", "from_tag": "b"}}),
            "version must be an integer >= 1",
        ),
        (
            json!({"id": "r", "version": true, "effective_at": 0, "priority": 0,
                "separate_tags": {"tag": "a", "from_tag": "b"}}),
            "version must be an integer >= 1",
        ),
        (
            json!({"id": "r", "version": 1, "effective_at": -1, "priority": 0,
                "separate_tags": {"tag": "a", "from_tag": "b"}}),
            "effective_at must be an integer >= 0",
        ),
        (
            json!({"id": "r", "version": 1, "effective_at": 0, "priority": 0}),
            "exactly one rule form",
        ),
        (
            json!({"id": "r", "version": 1, "effective_at": 0, "priority": 0,
                "separate_tags": {"tag": "a", "from_tag": "b"},
                "limit_tag_per_container": {"tag": "a", "max": 1}}),
            "exactly one rule form",
        ),
        (
            json!({"id": "r", "version": 1, "effective_at": 0, "priority": 0,
                "separate_tags": {"tag": "a"}}),
            "is missing from_tag",
        ),
        (
            json!({"id": "r", "version": 1, "effective_at": 0, "priority": 0,
                "limit_tag_per_container": {"tag": "a", "max": -1}}),
            "max must be an integer >= 0",
        ),
        (
            json!({"id": "r", "version": 1, "effective_at": 0, "priority": 0,
                "applies_to": {"region": "eu"},
                "separate_tags": {"tag": "a", "from_tag": "b"}}),
            "unknown shipment facts: region",
        ),
        (
            json!({"id": "r", "version": 1, "effective_at": 0, "priority": 0,
                "separate_tags": {"tag": "a", "from_tag": "b", "extra": 1}}),
            "unknown keys: extra",
        ),
        // A tag is matched against item and container tags by identity, so a number or
        // an empty string would silently never match instead of failing loudly.
        (
            json!({"id": "r", "version": 1, "effective_at": 0, "priority": 0,
                "require_container_tag": {"item_tag": "a", "container_tag": ""}}),
            "container_tag must be a non-empty string",
        ),
        (
            json!({"id": "r", "version": 1, "effective_at": 0, "priority": 0,
                "applies_to": {"facility": 5},
                "separate_tags": {"tag": "a", "from_tag": "b"}}),
            "facility must be a non-empty string",
        ),
    ];
    for (rule, expected) in cases {
        let scene = request(json!([rule]), mixed_load(), pallet(4, json!([])));
        let error = pack_json(&scene.to_string()).expect_err("a malformed rule is refused");
        assert!(
            error.to_string().contains(expected),
            "{expected} missing from {error}"
        );
    }
}

#[test]
fn rules_without_an_as_of_fail_admission() {
    let mut scene = request(segregation(), mixed_load(), pallet(4, json!([])));
    scene["policy"]
        .as_object_mut()
        .expect("an object")
        .remove("as_of");
    let error = pack_json(&scene.to_string()).expect_err("dating needs a reference instant");
    assert!(error.to_string().contains("as_of is required"), "{error}");

    // An empty rule set needs no instant, because nothing can be dated.
    scene["policy"] = json!({"rules": []});
    assert_eq!(containers_used(&scene), 1);
}

#[test]
fn an_unknown_policy_key_fails_admission() {
    let mut scene = request(segregation(), mixed_load(), pallet(4, json!([])));
    scene["policy"]["effect"] = json!("deny");
    let error = pack_json(&scene.to_string()).expect_err("an unknown key is refused");
    assert!(
        error.to_string().contains("unknown keys: effect"),
        "{error}"
    );
}

#[test]
fn resolution_orders_rules_by_priority_then_id_never_by_declaration_order() {
    // Both rules reject the carton. The cited one is the tie-break winner -- the
    // lexicographically smallest id -- whichever order the caller wrote them in, because
    // a citation that depended on declaration order would name a different policy for
    // two requests that mean the same thing.
    let zebra = json!({
        "id": "zebra-routing", "version": 1, "effective_at": AS_OF, "priority": 10,
        "require_container_tag": {"item_tag": "food", "container_tag": "reefer"},
    });
    let alpha = json!({
        "id": "alpha-routing", "version": 1, "effective_at": AS_OF, "priority": 10,
        "require_container_tag": {"item_tag": "food", "container_tag": "dock"},
    });
    for declared in [
        json!([zebra.clone(), alpha.clone()]),
        json!([alpha.clone(), zebra.clone()]),
    ] {
        let result = pack(&request(declared, mixed_load(), pallet(4, json!([]))));
        let cited = result["unpacked_items"][0]["details"][0]
            .as_str()
            .expect("a citation");
        assert!(
            cited.starts_with("alpha-routing@1: "),
            "smallest id wins the tie, got {cited}"
        );
    }

    // Priority outranks the id tie-break.
    let mut prioritised = zebra.clone();
    prioritised["priority"] = json!(50);
    let result = pack(&request(
        json!([prioritised, alpha]),
        mixed_load(),
        pallet(4, json!([])),
    ));
    let cited = result["unpacked_items"][0]["details"][0]
        .as_str()
        .expect("a citation");
    assert!(
        cited.starts_with("zebra-routing@1: "),
        "higher priority wins, got {cited}"
    );
}

#[test]
fn a_routing_rule_the_item_does_not_match_cites_nothing() {
    // The rule participates and is a routing rule, but this item does not carry its
    // item_tag, so it cannot be why anything was left behind. Skipping it rather than
    // citing it is what keeps a citation from naming an innocent policy.
    let rules = json!([{
        "id": "cold-chain", "version": 1, "effective_at": AS_OF, "priority": 10,
        "require_container_tag": {"item_tag": "frozen", "container_tag": "reefer"},
    }]);
    let oversized = json!([{
        "id": "slab", "quantity": 1, "tags": ["dry"],
        "dimensions": {"length": "9000", "width": "9000", "height": "9000"},
    }]);
    let result = pack(&request(rules, oversized, pallet(4, json!([]))));
    let unpacked = &result["unpacked_items"][0];
    assert_eq!(unpacked["reason"], "no_compatible_container_dimensions");
    assert_eq!(unpacked["details"], json!([]));
}

#[test]
fn segregation_holds_whichever_tag_enters_the_container_first() {
    // The rule names an unordered pair, so it must reject the hazmat drum joining a food
    // carton *and* the carton joining the drum. Testing one direction would leave the
    // other free to regress into a rule that only fires when the caller happens to list
    // the tags in the order the rule was written.
    for items in [
        json!([
            cube("drum", 1, json!(["hazmat"])),
            cube("carton", 1, json!(["food"]))
        ]),
        json!([
            cube("carton", 1, json!(["food"])),
            cube("drum", 1, json!(["hazmat"]))
        ]),
    ] {
        let scene = request(segregation(), items, pallet(4, json!([])));
        assert_eq!(containers_used(&without_policy(&scene)), 1);
        assert_eq!(containers_used(&scene), 2);
    }
}

#[test]
fn a_policy_block_without_a_rules_key_is_accepted_and_dates_nothing() {
    // `rules` absent is not the same as malformed. An empty rule set needs no `as_of`
    // either, because there is nothing to date -- so a caller may carry a shipment
    // context ahead of writing any rule against it.
    let mut scene = request(json!([]), mixed_load(), pallet(4, json!([])));
    let policy = scene["policy"].as_object_mut().expect("an object");
    policy.remove("rules");
    policy.remove("as_of");
    assert_eq!(containers_used(&scene), 1);
}

#[test]
fn a_superseded_version_declared_last_still_loses() {
    // Resolution is by (effective_at, version), not by position, so the newer text must
    // win whichever order the caller wrote the two versions in. Declaring them newest
    // first is the order the ascending case never exercises.
    let base = json!({
        "id": "segregation", "effective_at": AS_OF, "priority": 100,
        "separate_tags": {"tag": "hazmat", "from_tag": "food"},
    });
    let mut newer = base.clone();
    newer["version"] = json!(2);
    // The newer text forbids a pair these items do not carry, so they share a container.
    newer["separate_tags"] = json!({"tag": "frozen", "from_tag": "dry"});
    let mut older = base.clone();
    older["version"] = json!(1);

    for declared in [json!([older.clone(), newer.clone()]), json!([newer, older])] {
        let scene = request(declared, mixed_load(), pallet(4, json!([])));
        assert_eq!(containers_used(&scene), 1, "the version 2 text must win");
    }
}

#[test]
fn a_satisfied_routing_rule_cites_nothing() {
    // The item carries the rule's tag *and* an offered container carries the one it
    // requires, so the rule rules nothing out. The item is left behind by geometry, and
    // citing a rule that was satisfied would send someone to change a working policy.
    let rules = json!([{
        "id": "cold-chain", "version": 1, "effective_at": AS_OF, "priority": 10,
        "require_container_tag": {"item_tag": "frozen", "container_tag": "reefer"},
    }]);
    let oversized = json!([{
        "id": "slab", "quantity": 1, "tags": ["frozen"],
        "dimensions": {"length": "9000", "width": "9000", "height": "9000"},
    }]);
    let result = pack(&request(rules, oversized, pallet(4, json!(["reefer"]))));
    let unpacked = &result["unpacked_items"][0];
    assert_eq!(unpacked["reason"], "no_compatible_container_dimensions");
    assert_eq!(unpacked["details"], json!([]));
}
