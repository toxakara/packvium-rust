//! Packvium's exported commercial and control-plane API.
//!
//! Three deterministic functions over one canonical JSON document -- a quote, a policy
//! decision and catalog version metadata:
//!
//! ```
//! let input = r#"{"document":{"tariffs":[{"carrier_id":"acme","service_id":"ground",
//!   "versions":[{"effective_at":0,"dimensional_weight_divisor":5000,
//!   "cost_per_dimensional_kg_minor":{"zone-a":450}}]}]},
//!   "request":{"carrier_id":"acme","service_id":"ground","tariff_version":1,
//!   "zone":"zone-a","actual_weight_g":2000,"volume_mm3":1000000}}"#;
//! let result = packvium_core::commerce::quote_json(input).unwrap();
//! assert!(result.contains("\"total_minor\":900"));
//! ```
//!
//! This is an independent implementation of the contract in `docs/COMMERCE-API.md`, not
//! a port: it is held to producing a valid result that matches the fixture's objective
//! floor, which for an exact integer price means the same number.
//!
//! Results are emitted through `serde_json`, whose object map is a `BTreeMap`, so every
//! result document is already in the canonical sorted-key, compact form the conformance
//! harness compares byte for byte.

pub mod catalog;
pub mod document;
pub mod policy;
pub mod rating;

use serde_json::{Map, Value, json};

pub use catalog::CatalogRejection;
pub use document::CommerceInputError;
pub use policy::{Decision, PolicyScope, SnapshotError};
pub use rating::{RateBreakdown, RatingRejection, RatingRequest};

use document::{Document, fail, integer, keys, list, object, optional, text};
use policy::{PolicyRegistry, Rule};
use rating::{CarrierRegistry, Tariff};

/// The wire version every result document carries.
pub const API_VERSION: i64 = 1;

/// The closed set of rejection codes, in the order `docs/COMMERCE-API.md` tabulates them.
pub const REJECTION_CODES: [&str; 10] = [
    "tariff_not_found",
    "no_effective_tariff",
    "unavailable_zone",
    "unavailable_accessorial",
    "policy_rule_not_found",
    "policy_version_not_found",
    "catalog_not_found",
    "catalog_version_not_found",
    "no_effective_catalog_version",
    "ambiguous_catalog_reference",
];

/// A structured rejection travelling to the API boundary, where it becomes the
/// `{"status":"rejected", ...}` result document.
#[derive(Debug)]
struct Rejection {
    code: &'static str,
    fields: Map<String, Value>,
}

type Answer = Result<Value, Rejection>;

/// Price one shipment against one pinned or effective-dated tariff version.
///
/// Input is `{"document": ..., "request": ...}`; output is one canonical result
/// document. An `Err` is a malformed input -- a rejection is a successful `Ok`.
pub fn quote_json(input: &str) -> Result<String, CommerceInputError> {
    dispatch(input, |loaded, request| {
        keys(
            request,
            "request",
            &[
                "carrier_id",
                "service_id",
                "zone",
                "actual_weight_g",
                "volume_mm3",
            ],
            &["tariff_version", "as_of", "requested_accessorials"],
        )?;
        let pin = exactly_one(request, &["tariff_version", "as_of"])?;
        let carrier_id = text(&request["carrier_id"], "request.carrier_id")?;
        let service_id = text(&request["service_id"], "request.service_id")?;
        let rating_request = parse_rating_request(request)?;

        Ok(
            match resolve_tariff(&loaded.carriers, &carrier_id, &service_id, request, pin) {
                Err(rejection) => Err(rejection),
                Ok(tariff) => rate(tariff, &rating_request)
                    .map(|breakdown| json!({"quote": quote_payload(&breakdown)})),
            },
        )
    })
}

/// Decide one eligibility question against a pinned or effective-dated rule set.
pub fn evaluate_policy_json(input: &str) -> Result<String, CommerceInputError> {
    dispatch(input, |loaded, request| {
        keys(
            request,
            "request",
            &["scope", "context"],
            &["as_of", "rule_versions"],
        )?;
        let pin = exactly_one(request, &["as_of", "rule_versions"])?;
        let scope_text = text(&request["scope"], "request.scope")?;
        let scope = PolicyScope::parse(&scope_text).ok_or_else(|| CommerceInputError {
            path: "request.scope".to_owned(),
            message: format!("unsupported policy scope '{scope_text}'"),
        })?;
        let context = object(&request["context"], "request.context")?;

        let decision = if pin == "as_of" {
            Ok(loaded.policies.evaluate(
                scope,
                context,
                integer(&request["as_of"], "request.as_of")?,
            ))
        } else {
            match pinned_rules(&loaded.policies, &request["rule_versions"])? {
                Err(rejection) => Err(rejection),
                Ok(rules) => Ok(policy::decide(&rules, scope, context)),
            }
        };
        Ok(decision.map(|decision| json!({"decision": decision_payload(&decision)})))
    })
}

/// Report which catalog version a reference resolves to, and what it contains.
pub fn catalog_version_info_json(input: &str) -> Result<String, CommerceInputError> {
    dispatch(input, |loaded, request| {
        keys(
            request,
            "request",
            &["catalog_id", "resolved_at"],
            &["version", "as_of"],
        )?;
        let catalog_id = text(&request["catalog_id"], "request.catalog_id")?;
        let resolved_at = integer(&request["resolved_at"], "request.resolved_at")?;
        let version = optional(request, "version")
            .map(|found| integer(found, "request.version"))
            .transpose()?;
        let as_of = optional(request, "as_of")
            .map(|found| integer(found, "request.as_of"))
            .transpose()?;
        if version.is_some() && as_of.is_some() {
            return fail("request", "expected at most one of ['version', 'as_of']");
        }

        let Some(registry) = loaded.catalogs.get(&catalog_id) else {
            return Ok(Err(Rejection {
                code: "catalog_not_found",
                fields: selector(&catalog_id, None, None),
            }));
        };
        Ok(match registry.resolve(version, as_of) {
            Err(rejection) => Err(Rejection {
                code: match rejection {
                    CatalogRejection::VersionNotFound => "catalog_version_not_found",
                    CatalogRejection::NoEffectiveVersion => "no_effective_catalog_version",
                    CatalogRejection::AmbiguousReference => "ambiguous_catalog_reference",
                },
                fields: selector(&catalog_id, version, as_of),
            }),
            Ok(resolved) => Ok(json!({
                "catalog": catalog_payload(&catalog_id, resolved, resolved_at),
            })),
        })
    })
}

// -------------------------------------------------------------------------- internals

fn dispatch(
    input: &str,
    answer: impl FnOnce(&Document, &Map<String, Value>) -> Result<Answer, CommerceInputError>,
) -> Result<String, CommerceInputError> {
    let root: Value = serde_json::from_str(input).map_err(|error| CommerceInputError {
        path: "input".to_owned(),
        message: format!("invalid JSON: {error}"),
    })?;
    let call = object(&root, "input")?;
    keys(call, "input", &["document", "request"], &[])?;
    let loaded = document::load(&call["document"])?;
    let request = object(&call["request"], "request")?;

    let body = match answer(&loaded, request)? {
        Ok(payload) => merge(json!({"status": "ok"}), payload),
        Err(rejection) => merge(
            json!({"status": "rejected"}),
            json!({"error": {"code": rejection.code, "fields": Value::Object(rejection.fields)}}),
        ),
    };
    Ok(merge(json!({"api_version": API_VERSION}), body).to_string())
}

fn merge(mut into: Value, from: Value) -> Value {
    let (Value::Object(target), Value::Object(source)) = (&mut into, from) else {
        unreachable!("both halves of a result document are objects");
    };
    target.extend(source);
    into
}

fn exactly_one(
    request: &Map<String, Value>,
    names: &[&'static str],
) -> Result<&'static str, CommerceInputError> {
    let present: Vec<&&str> = names
        .iter()
        .filter(|name| optional(request, name).is_some())
        .collect();
    match present.as_slice() {
        [only] => Ok(**only),
        _ => fail("request", format!("expected exactly one of {names:?}")),
    }
}

fn parse_rating_request(request: &Map<String, Value>) -> Result<RatingRequest, CommerceInputError> {
    let mut requested_accessorials = Vec::new();
    if let Some(found) = optional(request, "requested_accessorials") {
        for (index, entry) in list(found, "request.requested_accessorials")?
            .iter()
            .enumerate()
        {
            let id = text(entry, &format!("request.requested_accessorials[{index}]"))?;
            if id.is_empty() {
                return fail(
                    "request.requested_accessorials",
                    "accessorial ids must be non-empty",
                );
            }
            if requested_accessorials.contains(&id) {
                return fail(
                    "request.requested_accessorials",
                    "accessorial ids must be unique",
                );
            }
            requested_accessorials.push(id);
        }
    }
    let zone = text(&request["zone"], "request.zone")?;
    let actual_weight_g = integer(&request["actual_weight_g"], "request.actual_weight_g")?;
    let volume_mm3 = integer(&request["volume_mm3"], "request.volume_mm3")?;
    if zone.is_empty() {
        return fail("request.zone", "zone is required");
    }
    if actual_weight_g < 0 || volume_mm3 < 0 {
        return fail(
            "request",
            "actual_weight_g and volume_mm3 cannot be negative",
        );
    }
    Ok(RatingRequest {
        zone,
        actual_weight_g,
        volume_mm3,
        requested_accessorials,
    })
}

fn resolve_tariff<'a>(
    carriers: &'a CarrierRegistry,
    carrier_id: &str,
    service_id: &str,
    request: &Map<String, Value>,
    pin: &str,
) -> Result<&'a Tariff, Rejection> {
    let identity = |extra: Option<(&str, i64)>| {
        let mut fields = Map::new();
        fields.insert("carrier_id".to_owned(), json!(carrier_id));
        fields.insert("service_id".to_owned(), json!(service_id));
        if let Some((key, value)) = extra {
            fields.insert(key.to_owned(), json!(value));
        }
        fields
    };
    if pin == "tariff_version" {
        // Already shape-checked by the caller's `keys`; a non-integer pin cannot reach here.
        let version = request["tariff_version"].as_i64().unwrap_or(-1);
        return carriers
            .tariff(carrier_id, service_id, version)
            .map_err(|_| Rejection {
                code: "tariff_not_found",
                fields: identity(Some(("tariff_version", version))),
            });
    }
    let as_of = request["as_of"].as_i64().unwrap_or(0);
    if !carriers.has(carrier_id, service_id) {
        return Err(Rejection {
            code: "tariff_not_found",
            fields: identity(None),
        });
    }
    carriers
        .effective_tariff(carrier_id, service_id, as_of)
        .map_err(|_| Rejection {
            code: "no_effective_tariff",
            fields: identity(Some(("as_of", as_of))),
        })
}

fn rate(tariff: &Tariff, request: &RatingRequest) -> Result<RateBreakdown, Rejection> {
    rating::rate_tariff(tariff, request).map_err(|rejection| {
        let mut fields = Map::new();
        fields.insert("carrier_id".to_owned(), json!(tariff.carrier_id));
        fields.insert("service_id".to_owned(), json!(tariff.service_id));
        fields.insert("tariff_version".to_owned(), json!(tariff.version));
        match rejection {
            RatingRejection::UnavailableZone(zone) => {
                fields.insert("zone".to_owned(), json!(zone));
                Rejection {
                    code: "unavailable_zone",
                    fields,
                }
            }
            RatingRejection::UnavailableAccessorials(ids) => {
                fields.insert("accessorial_ids".to_owned(), json!(ids));
                Rejection {
                    code: "unavailable_accessorial",
                    fields,
                }
            }
            // A resolved tariff cannot then be missing; both remaining variants are
            // produced only by resolution, which has already succeeded here.
            RatingRejection::TariffNotFound | RatingRejection::NoEffectiveTariff => Rejection {
                code: "tariff_not_found",
                fields,
            },
        }
    })
}

fn pinned_rules<'a>(
    policies: &'a PolicyRegistry,
    value: &Value,
) -> Result<Result<Vec<&'a Rule>, Rejection>, CommerceInputError> {
    let mut pins = Vec::new();
    for (index, entry) in list(value, "request.rule_versions")?.iter().enumerate() {
        let path = format!("request.rule_versions[{index}]");
        let pair = list(entry, &path)?;
        if pair.len() != 2 {
            return fail(&path, "expected a [rule_id, version] pair");
        }
        pins.push((
            text(&pair[0], &format!("{path}[0]"))?,
            integer(&pair[1], &format!("{path}[1]"))?,
        ));
    }
    match policies.resolve_versions(&pins) {
        Ok(rules) => Ok(Ok(rules)),
        Err(SnapshotError::DuplicateRuleId) => fail(
            "request.rule_versions",
            "a policy snapshot cannot pin the same rule id twice",
        ),
        Err(SnapshotError::Rejected(rejection)) => {
            let mut fields = Map::new();
            Ok(Err(match rejection {
                policy::PolicyRejection::RuleNotFound { rule_id } => {
                    fields.insert("rule_id".to_owned(), json!(rule_id));
                    Rejection {
                        code: "policy_rule_not_found",
                        fields,
                    }
                }
                policy::PolicyRejection::VersionNotFound { rule_id, version } => {
                    fields.insert("rule_id".to_owned(), json!(rule_id));
                    fields.insert("version".to_owned(), json!(version));
                    Rejection {
                        code: "policy_version_not_found",
                        fields,
                    }
                }
            }))
        }
    }
}

fn selector(catalog_id: &str, version: Option<i64>, as_of: Option<i64>) -> Map<String, Value> {
    let mut fields = Map::new();
    fields.insert("catalog_id".to_owned(), json!(catalog_id));
    if let Some(version) = version {
        fields.insert("version".to_owned(), json!(version));
    }
    if let Some(as_of) = as_of {
        fields.insert("as_of".to_owned(), json!(as_of));
    }
    fields
}

fn quote_payload(breakdown: &RateBreakdown) -> Value {
    json!({
        "carrier_id": breakdown.carrier_id,
        "service_id": breakdown.service_id,
        "tariff_version": breakdown.tariff_version,
        "zone": breakdown.zone,
        "actual_weight_g": breakdown.actual_weight_g,
        "dimensional_weight_g": breakdown.dimensional_weight_g,
        "billed_weight_g": breakdown.billed_weight_g,
        "base_charge_minor": breakdown.base_charge_minor,
        "minimum_charge_applied": breakdown.minimum_charge_applied,
        "fuel_surcharge_minor": breakdown.fuel_surcharge_minor,
        "accessorial_charges_minor": breakdown
            .accessorial_charges_minor
            .iter()
            .map(|(id, amount)| json!([id, amount]))
            .collect::<Vec<Value>>(),
        "total_minor": breakdown.total_minor,
    })
}

fn decision_payload(decision: &Decision) -> Value {
    json!({
        "scope": decision.scope.as_str(),
        "allowed": decision.allowed,
        "citation": decision.citation.as_ref().map(|citation| json!({
            "rule_id": citation.rule_id,
            "version": citation.version,
            "action": citation.action.as_str(),
            "priority": citation.priority,
            "reason": citation.reason,
        })),
    })
}

fn catalog_payload(catalog_id: &str, version: &catalog::Version, resolved_at: i64) -> Value {
    let snapshot = &version.snapshot;
    json!({
        "catalog_id": catalog_id,
        "version": version.number,
        "effective_at": version.effective_at,
        "published_at": version.published_at,
        "resolved_at": resolved_at,
        "rolled_back_from": version.rolled_back_from,
        "note": version.note,
        "entry_counts": {
            "items": snapshot.items.len(),
            "cartons": snapshot.cartons.len(),
            "pallets": snapshot.pallets.len(),
            "exclusions": snapshot.exclusions.len(),
            "overrides": snapshot.overrides.len(),
        },
        "item_ids": snapshot.item_ids(),
        "carton_ids": snapshot.carton_ids(),
        "pallet_ids": snapshot.pallet_ids(),
    })
}
