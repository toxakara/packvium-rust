//! Quote a shipment, apply a policy rule, and inspect a catalog version.
//!
//! Run it:
//!
//!     cargo run -p packvium-core --example commerce
//!
//! Everything the three functions need arrives in one *commerce document*: the tariffs
//! you publish, the eligibility rules you publish, and the catalog versions you publish.
//! Each history is a list, and a version's number is simply its position in that list
//! starting at 1 -- so `"tariff_version": 2` always means "the second entry under this
//! carrier and service", with no separate numbering to keep in sync.
//!
//! The Rust surface is JSON in, JSON out: one `{"document": ..., "request": ...}` string
//! per call. That is the same shape the C ABI, the N-API addon and the WASM build take,
//! so a caller can move between them without changing a payload.

use serde_json::{Value, json};

use packvium_core::commerce::{catalog_version_info_json, evaluate_policy_json, quote_json};

fn document() -> Value {
    json!({
        "tariffs": [{
            "carrier_id": "acme",
            "service_id": "ground",
            // Two published versions. The second takes effect at instant 1000.
            "versions": [
                {
                    "effective_at": 0,
                    // Volume in mm^3 over this divisor is dimensional weight in grams.
                    "dimensional_weight_divisor": 5000,
                    // Minor currency units (cents) per billed kilogram, per zone.
                    "cost_per_dimensional_kg_minor": {"zone-a": 450, "zone-b": 610},
                    "minimum_charge_minor": 900,
                    // Permille: 120 means 12.0%.
                    "fuel_surcharge_permille": 120,
                    "accessorials": [
                        {"accessorial_id": "liftgate", "flat_charge_minor": 250},
                        {"accessorial_id": "residential", "permille_of_base": 75},
                    ],
                },
                {
                    "effective_at": 1000,
                    "dimensional_weight_divisor": 4000,
                    "cost_per_dimensional_kg_minor": {"zone-a": 480},
                    "minimum_charge_minor": 950,
                    "fuel_surcharge_permille": 140,
                    "accessorials": [{"accessorial_id": "liftgate", "flat_charge_minor": 275}],
                },
            ],
        }],
        "policy_rules": [{
            "rule_id": "no-hazmat-air",
            "versions": [{
                "scope": "hazmat",
                "action": "reject",
                "priority": 10,
                "effective_at": 0,
                "reason": "class 1.4 is not accepted on air services",
                "predicates": [
                    {"scope": "hazmat", "field": "un_class", "operator": "equals", "value": "1.4"},
                ],
            }],
        }],
        "catalogs": [{
            "catalog_id": "dc-12",
            "versions": [
                {
                    "effective_at": 0, "published_at": 0, "note": "initial",
                    "snapshot": {
                        "items": [{"id": "sku-1", "dimensions_mm": [100, 200, 300],
                                   "weight_g": 1200}],
                        "cartons": [{"id": "box-m", "inner_dimensions_mm": [320, 240, 180],
                                     "max_payload_g": 15000, "cost_minor": 85}],
                    },
                },
                // A rollback is a new, higher-numbered version, never an edit of history.
                {"rollback_to": 1, "published_at": 900, "effective_at": 900,
                 "note": "revert the weight correction"},
            ],
        }],
    })
}

fn call(request: Value) -> String {
    json!({"document": document(), "request": request}).to_string()
}

fn show(title: &str, raw: &str) {
    let parsed: Value = serde_json::from_str(raw).expect("a result document is JSON");
    println!("\n== {title}");
    println!("{}", serde_json::to_string_pretty(&parsed).unwrap());
}

fn main() {
    // 1. Quote: what does this shipment cost?
    let pinned = quote_json(&call(json!({
        "carrier_id": "acme",
        "service_id": "ground",
        "tariff_version": 1,          // replay against exactly this version...
        "zone": "zone-a",
        "actual_weight_g": 1200,
        "volume_mm3": 6_000_000,
        "requested_accessorials": ["liftgate"],
    })))
    .expect("this request is well formed");
    show("a quote pinned to tariff version 1", &pinned);

    let effective = quote_json(&call(json!({
        "carrier_id": "acme",
        "service_id": "ground",
        "as_of": 1500,                // ...or whatever was in force at this instant
        "zone": "zone-a",
        "actual_weight_g": 1200,
        "volume_mm3": 6_000_000,
        "requested_accessorials": ["liftgate"],
    })))
    .expect("this request is well formed");
    let effective: Value = serde_json::from_str(&effective).unwrap();
    println!(
        "\n   as of instant 1500 the tariff is version {}, and the price is {}",
        effective["quote"]["tariff_version"], effective["quote"]["total_minor"],
    );

    // A request the model cannot answer is not an error. It is a result with a status,
    // a code from a closed set, and the structured fields that say what was missing.
    let unpriceable = quote_json(&call(json!({
        "carrier_id": "acme", "service_id": "ground", "tariff_version": 1,
        "zone": "zone-nowhere", "actual_weight_g": 1200, "volume_mm3": 6_000_000,
    })))
    .expect("a rejection is a successful call");
    show("a zone this tariff does not price", &unpriceable);

    // A *malformed* request is a different thing entirely: that is the caller's bug.
    let refused = quote_json(&call(json!({
        "carrier_id": "acme", "service_id": "ground", "tariff_version": 1,
        "zone": "zone-a", "actual_weight_g": -1, "volume_mm3": 6_000_000,
    })));
    println!(
        "\n   a negative weight is refused before anything is priced: {}",
        refused.unwrap_err(),
    );

    // 2. Policy: may this shipment go at all?
    let decision = evaluate_policy_json(&call(json!({
        "scope": "hazmat",
        "context": {"un_class": "1.4"},
        "as_of": 0,
    })))
    .expect("this request is well formed");
    show("a policy decision, with the rule that made it", &decision);

    // 3. Catalog: which master data was this decision made against?
    let catalog = catalog_version_info_json(&call(json!({
        "catalog_id": "dc-12",
        "version": 2,
        "resolved_at": 1700,
    })))
    .expect("this request is well formed");
    show("catalog version metadata", &catalog);

    // Every result is already in the canonical form: sorted keys, no padding. It is what
    // you store, log and compare -- two callers that agree produce the same bytes.
    println!("\n== the canonical form is what you store, log and compare");
    println!("{pinned}");
}
