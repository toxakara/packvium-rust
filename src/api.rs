use crate::error::{PackError, PackResult};
use crate::geometry::{ALL_DIRECTIONS, Aabb, Dimensions, Point, Rotation, ShapeType};
use crate::hull::{self, Vertex};
use crate::model::*;
use crate::policy::{PolicyConstraint, PolicyRuleSet};
use crate::rebalance::rebalance_weight;
use crate::solver::SolverRegistry;
use crate::units::{Length, Weight};
use crate::validation::IndependentValidator;
use serde_json::{Map, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

pub fn pack_json(input: &str) -> PackResult<String> {
    let value: Value = serde_json::from_str(input)?;
    let request = parse_request(&value)?;
    let result = pack_request_with_policy(&request, &PolicyRuleSet::parse(value.get("policy"))?)?;
    Ok(serde_json::to_string(&result.to_json(
        &request.output_length_unit,
        &request.output_weight_unit,
        true,
    ))?)
}

/// Rebalance an already-produced canonical result without routing through a
/// language-level fallback.
pub fn rebalance_json(
    request_input: &str,
    result_input: &str,
    max_moves: usize,
) -> PackResult<String> {
    let request_value: Value = serde_json::from_str(request_input)?;
    let request = parse_request(&request_value)?;
    validate_request(&request)?;
    let result_value: Value = serde_json::from_str(result_input)?;
    let original = parse_rebalance_result(&request, &result_value)?;
    // A packing the tariff cannot price is refused here for the same reason
    // `pack_request_with_policy` refuses one on the way out: rebalancing it would hand
    // back a shipment with no published price under the caller's own objective (
    // review). Objective-gated inside the helper.
    if let Some((container_id, grams, bound)) =
        crate::solvers::unpriceable_container(&original.containers, &request.config)
    {
        return Err(PackError::InvalidInput(format!(
            "container {container_id:?} bills at {grams} g, above its rate table's last \
             bracket ({bound} g); the shipment has no published price"
        )));
    }
    let balanced = rebalance_weight(&request, &original, max_moves);
    let improved = balanced.improved();
    let mut serialized_result = original.clone();
    serialized_result.containers = balanced.containers;
    let serialized = serialized_result.to_json(
        &request.output_length_unit,
        &request.output_weight_unit,
        false,
    );
    Ok(serde_json::json!({
        "containers": serialized["containers"].clone(),
        "moves": balanced.moves.iter().map(|move_| serde_json::json!({
            "item_id": move_.item_id,
            "from_container_id": move_.from_container_id,
            "to_container_id": move_.to_container_id,
        })).collect::<Vec<_>>(),
        "improved": improved,
    })
    .to_string())
}

fn exact_ticks(value: &Value, name: &str) -> PackResult<i64> {
    let raw = value
        .as_object()
        .and_then(|object| object.get("ticks"))
        .unwrap_or(value);
    match raw {
        Value::Number(number) => number.as_i64().ok_or_else(|| {
            PackError::InvalidInput(format!("{name} must contain exact integer ticks"))
        }),
        Value::String(text) => text.parse::<i64>().map_err(|_| {
            PackError::InvalidInput(format!("{name} must contain exact integer ticks"))
        }),
        _ => Err(PackError::InvalidInput(format!(
            "{name} must contain exact integer ticks"
        ))),
    }
}

fn result_dimensions(value: &Value, name: &str) -> PackResult<Dimensions> {
    let object = value
        .as_object()
        .ok_or_else(|| PackError::InvalidInput(format!("{name} must be an object")))?;
    Ok(Dimensions {
        length: Length(exact_ticks(
            object
                .get("length")
                .ok_or_else(|| PackError::InvalidInput(format!("{name}.length is required")))?,
            &format!("{name}.length"),
        )?),
        width: Length(exact_ticks(
            object
                .get("width")
                .ok_or_else(|| PackError::InvalidInput(format!("{name}.width is required")))?,
            &format!("{name}.width"),
        )?),
        height: Length(exact_ticks(
            object
                .get("height")
                .ok_or_else(|| PackError::InvalidInput(format!("{name}.height is required")))?,
            &format!("{name}.height"),
        )?),
    })
}

fn result_point(value: &Value, name: &str) -> PackResult<Point> {
    let object = value
        .as_object()
        .ok_or_else(|| PackError::InvalidInput(format!("{name} must be an object")))?;
    Ok(Point {
        x: exact_ticks(
            object
                .get("x")
                .ok_or_else(|| PackError::InvalidInput(format!("{name}.x is required")))?,
            &format!("{name}.x"),
        )?,
        y: exact_ticks(
            object
                .get("y")
                .ok_or_else(|| PackError::InvalidInput(format!("{name}.y is required")))?,
            &format!("{name}.y"),
        )?,
        z: exact_ticks(
            object
                .get("z")
                .ok_or_else(|| PackError::InvalidInput(format!("{name}.z is required")))?,
            &format!("{name}.z"),
        )?,
    })
}

fn parse_status(value: Option<&Value>) -> PackingStatus {
    match value.and_then(Value::as_str) {
        Some("optimal") => PackingStatus::Optimal,
        Some("best_found") => PackingStatus::BestFound,
        Some("time_limit") => PackingStatus::TimeLimit,
        Some("infeasible") => PackingStatus::Infeasible,
        Some("invalid_result") => PackingStatus::InvalidResult,
        _ => PackingStatus::Feasible,
    }
}

fn parse_rebalance_result(request: &PackingRequest, value: &Value) -> PackResult<PackingResult> {
    let root = value
        .as_object()
        .ok_or_else(|| PackError::InvalidInput("result must be an object".into()))?;
    let mut instances = request
        .instances()
        .into_iter()
        .map(|instance| (instance.id(), instance))
        .collect::<BTreeMap<_, _>>();
    let clearance = request.config.clearance;
    let containers = root
        .get("containers")
        .and_then(Value::as_array)
        .ok_or_else(|| PackError::InvalidInput("result.containers must be an array".into()))?
        .iter()
        .enumerate()
        .map(|(container_index, raw)| {
            let object = raw.as_object().ok_or_else(|| {
                PackError::InvalidInput(format!(
                    "result.containers[{container_index}] must be an object"
                ))
            })?;
            if object.get("lattice_summary").is_some() {
                return Err(PackError::InvalidInput(
                    "rebalance requires materialized placement coordinates".into(),
                ));
            }
            let container_type = object
                .get("container_type")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    PackError::InvalidInput(format!(
                        "result.containers[{container_index}].container_type is required"
                    ))
                })?;
            let template = request
                .containers
                .iter()
                .find(|container| container.id == container_type)
                .cloned()
                .ok_or_else(|| {
                    PackError::InvalidInput(format!(
                        "unknown result container type {container_type:?}"
                    ))
                })?;
            let sequence = object
                .get("id")
                .and_then(Value::as_str)
                .and_then(|id| id.rsplit_once('#'))
                .and_then(|(_, suffix)| suffix.parse::<usize>().ok())
                .unwrap_or(container_index + 1);
            let placements = object
                .get("placements")
                .and_then(Value::as_array)
                .ok_or_else(|| {
                    PackError::InvalidInput(format!(
                        "result.containers[{container_index}].placements must be an array"
                    ))
                })?
                .iter()
                .enumerate()
                .map(|(placement_index, raw_placement)| {
                    let placement = raw_placement.as_object().ok_or_else(|| {
                        PackError::InvalidInput(format!(
                            "result.containers[{container_index}].placements[{placement_index}] must be an object"
                        ))
                    })?;
                    let item_id = placement
                        .get("item_id")
                        .and_then(Value::as_str)
                        .ok_or_else(|| PackError::InvalidInput("placement.item_id is required".into()))?;
                    let instance = instances.remove(item_id).ok_or_else(|| {
                        PackError::InvalidInput(format!(
                            "result duplicates or references unknown item instance {item_id:?}"
                        ))
                    })?;
                    let position = result_point(
                        placement
                            .get("position")
                            .ok_or_else(|| PackError::InvalidInput("placement.position is required".into()))?,
                        "placement.position",
                    )?;
                    let dimensions = result_dimensions(
                        placement
                            .get("dimensions")
                            .ok_or_else(|| PackError::InvalidInput("placement.dimensions is required".into()))?,
                        "placement.dimensions",
                    )?;
                    let rotation = placement
                        .get("orientation")
                        .and_then(Value::as_str)
                        .and_then(parse_rotation)
                        .ok_or_else(|| PackError::InvalidInput("placement.orientation is invalid".into()))?;
                    let support_ratio = placement
                        .get("support_ratio")
                        .and_then(|ratio| ratio.as_f64().or_else(|| ratio.as_str()?.parse().ok()))
                        .unwrap_or(0.0);
                    let top_load = placement
                        .get("top_load")
                        .map(|load| exact_ticks(load, "placement.top_load"))
                        .transpose()?
                        .unwrap_or(0);
                    Ok(Placement {
                        instance,
                        position,
                        rotation,
                        dimensions,
                        envelope_origin: Point {
                            x: position.x.saturating_sub(clearance.0),
                            y: position.y.saturating_sub(clearance.0),
                            z: position.z.saturating_sub(clearance.0),
                        },
                        envelope_dimensions: dimensions.expand(clearance),
                        support_ratio,
                        top_load: Weight(top_load),
                    })
                })
                .collect::<PackResult<Vec<_>>>()?;
            Ok(PackedContainer {
                container: template,
                sequence,
                placements,
                lattice_summary: None,
                lattice_items: Vec::new(),
            })
        })
        .collect::<PackResult<Vec<_>>>()?;
    let unpacked = root
        .get("unpacked_items")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|raw| {
            let object = raw
                .as_object()
                .ok_or_else(|| PackError::InvalidInput("unpacked item must be an object".into()))?;
            let item_id = object
                .get("item_id")
                .and_then(Value::as_str)
                .ok_or_else(|| PackError::InvalidInput("unpacked item_id is required".into()))?;
            let instance = instances.remove(item_id).ok_or_else(|| {
                PackError::InvalidInput(format!(
                    "result duplicates or references unknown item instance {item_id:?}"
                ))
            })?;
            let reason = object
                .get("reason")
                .and_then(Value::as_str)
                .unwrap_or("search_exhausted")
                .to_owned();
            let details = object
                .get("details")
                .and_then(Value::as_array)
                .map(|values| {
                    values
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default();
            Ok(UnpackedItem::new(instance, reason, details))
        })
        .collect::<PackResult<Vec<_>>>()?;
    if !instances.is_empty() {
        return Err(PackError::InvalidInput(
            "result does not account for every requested item instance".into(),
        ));
    }
    let algorithm = root.get("algorithm").and_then(Value::as_object);
    Ok(PackingResult {
        status: parse_status(root.get("status")),
        containers,
        unpacked,
        algorithm: AlgorithmReport {
            profile: algorithm
                .and_then(|value| value.get("profile"))
                .and_then(Value::as_str)
                .unwrap_or("balanced")
                .to_owned(),
            solver: algorithm
                .and_then(|value| value.get("solver"))
                .and_then(Value::as_str)
                .unwrap_or("external")
                .to_owned(),
            ..AlgorithmReport::default()
        },
        score: root
            .get("score")
            .and_then(Value::as_array)
            .map(|values| {
                values
                    .iter()
                    .filter_map(Value::as_i64)
                    .map(i128::from)
                    .collect()
            })
            .unwrap_or_default(),
        warnings: Vec::new(),
        alternatives: Vec::new(),
        feasibility: root.get("feasibility").and_then(ResultFact::from_json),
        termination: root.get("termination").and_then(ResultFact::from_json),
        optimality: root.get("optimality").and_then(ResultFact::from_json),
        objective: root
            .get("objective")
            .and_then(Value::as_str)
            .unwrap_or("default")
            .to_owned(),
        catalog_versions_used: root
            .get("catalog_versions_used")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default(),
    })
}

pub fn pack_request(request: &PackingRequest) -> PackResult<PackingResult> {
    pack_request_with_policy(request, &PolicyRuleSet::default())
}

/// `pack_request`, plus a rule set resolved from the request's `policy` block.
///
/// The rule set is a parameter rather than a field on `PackingRequest` deliberately.
/// Adding a required field to that struct would break nineteen literal initialisers
/// across the workspace, and giving it a `Default` to dodge that would turn every future
/// field into a silent zero instead of a compile error -- the same trade this codebase
/// already declined for `Container::rate_table`.
pub fn pack_request_with_policy(
    request: &PackingRequest,
    policy: &PolicyRuleSet,
) -> PackResult<PackingResult> {
    validate_request(request)?;
    // Registering the constraint is what makes an illegal candidate be rejected *during*
    // search rather than filtered out of a chosen answer -- and it is also what makes the
    // uniform-lattice and homogeneous-block fast paths stand down, since both gate on
    // `constraints.is_empty()`. This relies on the portfolio boundary's invariant that
    // no fast path may bypass a registered constraint rather than repeating that guard.
    let registry = if policy.is_empty() {
        SolverRegistry::default()
    } else {
        SolverRegistry::default()
            .with_constraint(Arc::new(PolicyConstraint::new(policy.rules.clone())))
    };
    let mut result = registry.solve(request)?;
    cite_policy_rejections(&mut result, request, policy);
    result.catalog_versions_used = request.catalog_versions_used.clone();
    // Independent validation re-derives every guarantee from per-item placements.
    // A container built by GridSolver's compact fast path carries a
    // `lattice_summary` instead; expand it into the identical placements the O(n)
    // path would have built just for this check -- `result`, and therefore what is
    // returned to the caller, stays compact.
    let report = if result
        .containers
        .iter()
        .any(|container| container.lattice_summary.is_some())
    {
        IndependentValidator.validate(request, &expand_for_validation(&result))
    } else {
        IndependentValidator.validate(request, &result)
    };
    if !report.valid {
        return Err(PackError::InvalidSolution(
            report
                .issues
                .iter()
                .map(|issue| format!("{}:{}", issue.code, issue.message))
                .collect::<Vec<_>>()
                .join("; "),
        ));
    }
    // The search ranks an unpriceable packing worst so that any priceable alternative
    // beats it; reaching here means no alternative existed and the sentinel is about to
    // be reported as a score. Refusing is the contract the request schema, the
    // conformance validator and the other two engines already state: a billed weight past
    // the last bracket has no published price, and quoting one anyway is the failure this
    // objective exists to prevent.
    if let Some((container_id, grams, bound)) =
        crate::solvers::unpriceable_container(&result.containers, &request.config)
    {
        return Err(PackError::InvalidInput(format!(
            "container {container_id:?} bills at {grams} g, above its rate table's last \
             bracket ({bound} g); the shipment has no published price"
        )));
    }
    // The same contract holds for the runner-up packings a result carries. The
    // portfolio filters its own alternatives, but a registry-provided solver may attach
    // ones that never passed through it, and an alternative quoting the sentinel is the
    // leak by another door.
    result.alternatives.retain(|alternative| {
        crate::solvers::unpriceable_container(&alternative.containers, &request.config).is_none()
    });
    Ok(result)
}

/// Names the rule that left an item behind, where one provably did.
///
/// The reason is a *diagnosis* over the request, not a decision taken during search --
/// `explain_unfit` already derives every other reason the same way, after the fact. Doing
/// it here rather than inside five solvers keeps one copy of the ranking rule and leaves
/// `explain_unfit`'s signature, and its six call sites, untouched.
///
/// Rank matters and matches Python and PHP: a policy never displaces a geometric or
/// eligibility proof. An item too big for every container is impossible whatever a policy
/// says, and naming the policy first would send a caller to fix the wrong thing.
fn cite_policy_rejections(
    result: &mut PackingResult,
    request: &PackingRequest,
    policy: &PolicyRuleSet,
) {
    if policy.is_empty() {
        return;
    }
    for unpacked in &mut result.unpacked {
        if matches!(
            unpacked.reason.as_str(),
            "no_compatible_container_dimensions"
                | "rotation_restricted"
                | "payload_exceeded"
                | "no_eligible_container"
        ) {
            continue;
        }
        if let Some(citation) = policy.proves_unplaceable(&unpacked.instance, &request.containers) {
            *unpacked = UnpackedItem::new(
                unpacked.instance.clone(),
                "policy_rule".to_string(),
                vec![citation],
            );
        }
    }
    // Alternatives too. The claim is about the request, so it holds for every solution in
    // the portfolio; leaving them alone would report one item as `policy_rule` in the
    // chosen answer and `search_exhausted` in the runner-up beside it, which reads as
    // though the two disagreed about why.
    for alternative in &mut result.alternatives {
        cite_policy_rejections(alternative, request, policy);
    }
}

fn expand_for_validation(result: &PackingResult) -> PackingResult {
    let mut expanded = result.clone();
    for container in &mut expanded.containers {
        if container.lattice_summary.is_some() {
            container.placements = container.expand_placements();
            container.lattice_summary = None;
            container.lattice_items = Vec::new();
        }
    }
    expanded
}

/// Public request fields this engine does not implement yet, by the scope they appear in.
///
/// Empty today, and that is the point: the lists exist so that adding a public field to
/// the schema before this engine implements it is a *rejection* rather than a silent
/// omission. Parsing reads the keys it knows and ignores the rest, so without this guard
/// an unimplemented field would produce a confident answer computed as though the caller
/// had never sent it -- indistinguishable, from the outside, from an engine that honoured
/// it. The JavaScript fallback has carried the same table since it was the only engine
/// behind on features; this is its counterpart, so staged rollout works the same way in
/// every implementation.
///
/// A name added here must also be recorded in `conformance/public-field-matrix.json` with
/// a `rejected:unsupported_feature` support level for this engine, which is what makes the
/// conformance corpus assert the rejection instead of merely tolerating it.
const UNSUPPORTED_REQUEST_FIELDS: &[&str] = &[];
const UNSUPPORTED_CONFIGURATION_FIELDS: &[&str] = &[];
// `hull_vertices`, `compression_ratio` and `max_compression_pressure_kpa` left this list in
//, when this engine gained both the solver behaviour and the independent validation
// the staged rollout requires. The JavaScript fallback still carries them.
const UNSUPPORTED_ITEM_FIELDS: &[&str] = &[];

/// `item.shape_type` values this engine does not implement.
///
/// Empty since: this engine implements every value the schema defines. The guard stays
/// because the next reserved value will need it, and because `reject_unsupported` takes its
/// lists as parameters precisely so it remains testable when they are empty.
const UNSUPPORTED_SHAPE_TYPES: &[&str] = &[];
// `pallet_overhang_limit` was reserved in the schema by at the 1.1.0 contract freeze
// and is refused everywhere until an engine implements it from a request: a field a caller
// can set and the solver ignores is worse than a refusal.
// `access_directions` left this list in, which wired the reserved field through to
// the stop-accessibility rule in all four engines at once.
const UNSUPPORTED_CONTAINER_FIELDS: &[&str] = &["pallet_overhang_limit"];

fn reject_unsupported(object: &Map<String, Value>) -> PackResult<()> {
    reject_listed_fields(
        object,
        UNSUPPORTED_REQUEST_FIELDS,
        UNSUPPORTED_CONFIGURATION_FIELDS,
        UNSUPPORTED_ITEM_FIELDS,
        UNSUPPORTED_CONTAINER_FIELDS,
        UNSUPPORTED_SHAPE_TYPES,
    )
}

/// The lists are parameters rather than read from the constants directly so the guard
/// itself is testable. With every list empty -- the correct state whenever this engine is
/// caught up -- a test against `reject_unsupported` can only prove that nothing is
/// rejected, which is exactly as true of a guard that does nothing at all.
fn reject_listed_fields(
    object: &Map<String, Value>,
    request_fields: &[&str],
    configuration_fields: &[&str],
    item_fields: &[&str],
    container_fields: &[&str],
    shape_types: &[&str],
) -> PackResult<()> {
    let mut found: Vec<String> = Vec::new();
    // Top-level scope: a block such as `policy` is a property of the whole request
    // rather than of one item or container, so it has no per-entry loop to be caught by.
    for key in request_fields {
        if object.contains_key(*key) {
            found.push((*key).to_string());
        }
    }
    if let Some(configuration) = object.get("configuration").and_then(Value::as_object) {
        for key in configuration_fields {
            if configuration.contains_key(*key) {
                found.push(format!("configuration.{key}"));
            }
        }
    }
    for (scope, keys) in [("items", item_fields), ("containers", container_fields)] {
        let entries = object
            .get(scope)
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        for entry in entries {
            let Some(entry) = entry.as_object() else {
                continue;
            };
            for key in keys {
                if entry.contains_key(*key) {
                    // Singular, matching the JavaScript fallback's wording: the name
                    // identifies the field, not the position it was found in.
                    let singular = scope.trim_end_matches('s');
                    found.push(format!("{singular}.{key}"));
                }
            }
        }
    }
    for entry in object
        .get("items")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
    {
        let Some(shape) = entry
            .as_object()
            .and_then(|e| e.get("shape_type"))
            .and_then(Value::as_str)
        else {
            continue;
        };
        if shape_types.contains(&shape) {
            found.push(format!("item.shape_type={shape}"));
        }
    }
    if found.is_empty() {
        return Ok(());
    }
    found.sort();
    found.dedup();
    Err(PackError::UnsupportedFeature(format!(
        "the Rust core does not yet implement {}; the request was rejected instead of \
         silently ignoring public fields",
        found.join(", ")
    )))
}

fn parse_request(root: &Value) -> PackResult<PackingRequest> {
    let object = root
        .as_object()
        .ok_or_else(|| PackError::InvalidInput("request must be an object".into()))?;
    reject_unsupported(object)?;
    let length_unit = object
        .get("units")
        .and_then(Value::as_object)
        .and_then(|units| units.get("length"))
        .and_then(Value::as_str)
        .unwrap_or("mm");
    let config = parse_config(object.get("configuration"), length_unit)?;
    let output_length_unit = object
        .get("output")
        .and_then(Value::as_object)
        .and_then(|output| output.get("length_unit"))
        .and_then(Value::as_str)
        .unwrap_or(length_unit)
        .to_owned();
    let output_weight_unit = object
        .get("output")
        .and_then(Value::as_object)
        .and_then(|output| output.get("weight_unit"))
        .and_then(Value::as_str)
        .unwrap_or("g")
        .to_owned();
    let items = object
        .get("items")
        .and_then(Value::as_array)
        .ok_or_else(|| PackError::InvalidInput("items must be an array".into()))?
        .iter()
        .map(|value| parse_item(value, length_unit))
        .collect::<PackResult<Vec<_>>>()?;
    let containers = object
        .get("containers")
        .and_then(Value::as_array)
        .ok_or_else(|| PackError::InvalidInput("containers must be an array".into()))?
        .iter()
        .map(|value| parse_container(value, length_unit))
        .collect::<PackResult<Vec<_>>>()?;
    // Rating some containers and not others would rank a priced packing against an
    // unpriced one as though the unpriced were free, so a missing tariff is refused
    // before either solver path runs -- a static property of the request, unlike a
    // billed weight past the last bracket, which depends on how the search filled the
    // box and therefore loses a candidate instead. Python, PHP and the JavaScript
    // fallback all refuse here; Rust did not, and priced the container at the
    // unpriceable sentinel instead.
    if config.objective == "lowest_landed_cost"
        && let Some(unrated) = containers
            .iter()
            .find(|container| container.rate_table.is_none())
    {
        return Err(PackError::InvalidInput(format!(
            "the lowest_landed_cost objective requires a rate_table on every container; \
             {:?} has none",
            unrated.id
        )));
    }
    let catalog_versions_used = parse_catalog_versions(object.get("catalog_versions_used"))?;
    Ok(PackingRequest {
        items,
        containers,
        config,
        output_length_unit,
        output_weight_unit,
        catalog_versions_used,
    })
}

fn parse_catalog_versions(value: Option<&Value>) -> PackResult<Vec<Value>> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let references = value
        .as_array()
        .ok_or_else(|| PackError::InvalidInput("catalog_versions_used must be an array".into()))?;
    let required = ["catalog_id", "effective_at", "resolved_at", "version"];
    let mut seen = BTreeSet::new();
    let mut parsed = Vec::with_capacity(references.len());
    for (index, reference) in references.iter().enumerate() {
        let object = reference.as_object().ok_or_else(|| {
            PackError::InvalidInput(format!("catalog_versions_used[{index}] must be an object"))
        })?;
        let mut keys = object.keys().map(String::as_str).collect::<Vec<_>>();
        keys.sort_unstable();
        if keys != required {
            return Err(PackError::InvalidInput(format!(
                "catalog_versions_used[{index}] must contain exactly the canonical fields"
            )));
        }
        let catalog_id = object["catalog_id"]
            .as_str()
            .filter(|id| !id.is_empty())
            .ok_or_else(|| {
                PackError::InvalidInput(format!(
                    "catalog_versions_used[{index}].catalog_id must be non-empty"
                ))
            })?;
        if !seen.insert(catalog_id.to_owned()) {
            return Err(PackError::InvalidInput(format!(
                "catalog_versions_used contains ambiguous duplicate {catalog_id:?}"
            )));
        }
        for (field, minimum) in [("version", 1_u64), ("effective_at", 0), ("resolved_at", 0)] {
            if object[field].as_u64().is_none_or(|number| number < minimum) {
                return Err(PackError::InvalidInput(format!(
                    "catalog_versions_used[{index}].{field} must be >= {minimum}"
                )));
            }
        }
        parsed.push(reference.clone());
    }
    Ok(parsed)
}

fn parse_config(value: Option<&Value>, length_unit: &str) -> PackResult<PackingConfig> {
    let empty = Map::new();
    let map = value.and_then(Value::as_object).unwrap_or(&empty);
    let objective = match map.get("objective") {
        None => "default",
        Some(Value::String(value)) => value.as_str(),
        Some(_) => {
            return Err(PackError::InvalidInput(
                "configuration.objective must be a string".into(),
            ));
        }
    };
    if !matches!(
        objective,
        "default"
            | "lowest_cost"
            | "shipping_cost"
            | "lowest_landed_cost"
            | "open_dimension_height"
            | "maximum_value"
    ) {
        return Err(PackError::InvalidInput(format!(
            "unknown objective {objective:?}; expected default, lowest_cost, shipping_cost, lowest_landed_cost, open_dimension_height or maximum_value"
        )));
    }
    let dimensional_weight_divisor = match map.get("dimensional_weight_divisor") {
        None => None,
        Some(value) => value
            .as_u64()
            .filter(|value| *value > 0)
            .map(|value| value as i128)
            .map(Some)
            .ok_or_else(|| {
                PackError::InvalidInput(
                    "configuration.dimensional_weight_divisor must be a positive integer".into(),
                )
            })?,
    };
    // Landed cost is priced off billed weight, so it needs the same dimensional-weight
    // inputs; without them it would be pricing the wrong number rather than none at all.
    if matches!(objective, "shipping_cost" | "lowest_landed_cost")
        && dimensional_weight_divisor.is_none()
    {
        return Err(PackError::InvalidInput(format!(
            "the {objective} objective requires configuration.dimensional_weight_divisor"
        )));
    }
    let dimensional_weight_length_unit = config_enum(
        map,
        "dimensional_weight_length_unit",
        "in",
        &["mm", "cm", "m", "in", "ft"],
    )?;
    let dimensional_weight_weight_unit = config_enum(
        map,
        "dimensional_weight_weight_unit",
        "lb",
        &["mg", "g", "kg", "oz", "lb"],
    )?;
    let solvers = match map.get("solvers") {
        None => Vec::new(),
        Some(Value::Array(values)) => values
            .iter()
            .map(|value| {
                value.as_str().map(str::to_owned).ok_or_else(|| {
                    PackError::InvalidInput("configuration.solvers must contain strings".into())
                })
            })
            .collect::<PackResult<Vec<_>>>()?,
        Some(_) => {
            return Err(PackError::InvalidInput(
                "configuration.solvers must be an array".into(),
            ));
        }
    };
    const SOLVERS: &[&str] = &[
        "grid",
        "extreme_points",
        "homogeneous_blocks",
        "layer",
        "maximal_spaces",
        "exact_small",
    ];
    if let Some(unknown) = solvers
        .iter()
        .find(|name| !SOLVERS.contains(&name.as_str()))
    {
        return Err(PackError::InvalidInput(format!(
            "unknown solver {unknown:?}; expected one of {SOLVERS:?}"
        )));
    }
    let profile = match map
        .get("solver_profile")
        .and_then(Value::as_str)
        .unwrap_or("balanced")
    {
        "fast" => SolverProfile::Fast,
        "quality" => SolverProfile::Quality,
        "exact_small" | "exact-small" => SolverProfile::ExactSmall,
        _ => SolverProfile::Balanced,
    };
    Ok(PackingConfig {
        profile,
        time_limit_ms: u64_field(map, "time_limit_ms", 1_000),
        top_k: usize_field(map, "alternatives", 3).max(1),
        seed: u64_field(map, "seed", 42),
        max_containers: map
            .get("max_containers")
            .and_then(Value::as_u64)
            .map(|value| value as usize),
        clearance: parse_optional_length(map.get("clearance"), length_unit)?,
        minimum_support_ratio: map
            .get("minimum_support_ratio")
            .and_then(Value::as_f64)
            .unwrap_or(0.0),
        exact_item_limit: usize_field(map, "exact_item_limit", 7),
        multi_start_orders: usize_field(map, "multi_start_orders", 8).max(1),
        max_candidates_per_item: usize_field(
            map,
            "max_candidates_per_item",
            if profile == SolverProfile::Quality {
                16
            } else {
                1
            },
        )
        .max(1),
        max_candidate_points: usize_field(map, "max_candidate_points", 4_096).max(16),
        parallel: map.get("parallel").and_then(Value::as_bool).unwrap_or(true),
        effort_budget: parse_effort_budget(map.get("effort_budget"))?,
        solvers,
        objective: objective.into(),
        dimensional_weight_divisor,
        dimensional_weight_length_unit,
        dimensional_weight_weight_unit,
        require_placement_coordinates: map
            .get("require_placement_coordinates")
            .and_then(Value::as_bool)
            .unwrap_or(true),
        container_plan_beam_width: usize_field(
            map,
            "container_plan_beam_width",
            if profile == SolverProfile::Quality {
                16
            } else {
                1
            },
        )
        .max(1),
        container_plan_node_limit: usize_field(
            map,
            "container_plan_node_limit",
            if profile == SolverProfile::Quality {
                100_000
            } else {
                1
            },
        )
        .max(1),
        // Not read from the request: the schema has no access-directions field, so a
        // request cannot switch the stop-accessibility rule on. A library caller sets it
        // on the config directly, as in the Python and PHP engines.
        access_directions: Vec::new(),
    })
}

fn config_enum(
    map: &Map<String, Value>,
    field: &str,
    default: &str,
    allowed: &[&str],
) -> PackResult<String> {
    let value = match map.get(field) {
        None => default,
        Some(Value::String(value)) => value,
        Some(_) => {
            return Err(PackError::InvalidInput(format!(
                "configuration.{field} must be a string"
            )));
        }
    };
    if !allowed.contains(&value) {
        return Err(PackError::InvalidInput(format!(
            "configuration.{field} must be one of {allowed:?}"
        )));
    }
    Ok(value.to_owned())
}

fn parse_effort_budget(value: Option<&Value>) -> PackResult<Option<EffortBudget>> {
    let Some(value) = value else {
        return Ok(None);
    };
    let map = value.as_object().ok_or_else(|| {
        PackError::InvalidInput("configuration.effort_budget must be object".into())
    })?;
    let field = |name: &str| -> PackResult<Option<u64>> {
        match map.get(name) {
            None => Ok(None),
            Some(value) => value
                .as_u64()
                .filter(|value| *value > 0)
                .map(Some)
                .ok_or_else(|| {
                    PackError::InvalidInput(format!("effort_budget.{name} must be positive"))
                }),
        }
    };
    Ok(Some(EffortBudget {
        max_candidates_evaluated: field("max_candidates_evaluated")?,
        max_placement_attempts: field("max_placement_attempts")?,
        max_search_nodes: field("max_search_nodes")?,
        max_restarts: field("max_restarts")?.map(|value| value as usize),
    }))
}

fn parse_item(value: &Value, unit: &str) -> PackResult<Item> {
    let map = value
        .as_object()
        .ok_or_else(|| PackError::InvalidInput("item must be object".into()))?;
    let id = str_field(map, "id")?;
    let dimensions = parse_dimensions(map.get("dimensions"), unit)?;
    let mut rotations = map
        .get("allowed_rotations")
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(|value| value.as_str().and_then(parse_rotation))
                .collect::<Vec<_>>()
        })
        .unwrap_or_else(|| Rotation::ALL.to_vec());
    if map
        .get("keep_upright")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        rotations.retain(|rotation| Rotation::UPRIGHT.contains(rotation));
    }
    if rotations.is_empty() {
        return Err(PackError::InvalidInput(format!(
            "item {id} has no allowed rotations"
        )));
    }
    let shape_type: ShapeType = match map.get("shape_type").and_then(Value::as_str) {
        None | Some("rigid_cuboid") => ShapeType::RigidCuboid,
        Some("convex_hull") => ShapeType::ConvexHull,
        Some("compressible") => ShapeType::Compressible,
        Some(other) => {
            return Err(PackError::InvalidInput(format!(
                "item.shape_type {other} is not a known shape"
            )));
        }
    };
    let nesting_height = map
        .get("nesting_height")
        .map(|value| Length::parse(value, unit))
        .transpose()?;
    let compression_ratio_ppm = match map.get("compression_ratio").and_then(Value::as_f64) {
        None => None,
        Some(ratio) => Some(crate::compression::ratio_to_ppm(ratio).ok_or_else(|| {
            PackError::InvalidInput("compression_ratio must be between zero and one".into())
        })?),
    };
    let max_compression_pressure_kpa = map
        .get("max_compression_pressure_kpa")
        .and_then(Value::as_i64);
    let hull_vertices = admit_shape(
        shape_type,
        parse_hull_vertices(map.get("hull_vertices"), unit)?,
        compression_ratio_ppm,
        max_compression_pressure_kpa,
        dimensions,
        nesting_height,
    )?;
    Ok(Item {
        id,
        dimensions,
        weight: parse_optional_weight(map.get("weight"), "g")?,
        quantity: usize_field(map, "quantity", 1),
        allowed_rotations: rotations,
        stackable: map
            .get("stackable")
            .and_then(Value::as_bool)
            .unwrap_or(true),
        must_be_on_floor: map
            .get("must_be_on_floor")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        max_top_load: map
            .get("max_top_load")
            .map(|value| Weight::parse(value, "g"))
            .transpose()?,
        minimum_support_ratio: map
            .get("minimum_support_ratio")
            .and_then(Value::as_f64)
            .unwrap_or(0.0),
        group: map.get("group").and_then(Value::as_str).map(str::to_owned),
        tags: string_set(map.get("tags")),
        incompatible_tags: string_set(map.get("incompatible_tags")),
        priority: map.get("priority").and_then(Value::as_i64).unwrap_or(0) as i32,
        metadata: object_map(map.get("metadata")),
        nesting_height,
        max_stacked_items: optional_usize(map, "max_stacked_items", "item")?,
        ground_contact_rule: optional_string(map, "ground_contact_rule", "item")?,
        stop_index: optional_stop_index(map)?,
        eligible_container_tags: optional_string_set(
            map.get("eligible_container_tags"),
            "item.eligible_container_tags",
        )?,
        value: optional_usize(map, "value", "item")?,
        shape_type,
        hull_vertices,
        compression_ratio_ppm,
        max_compression_pressure_kpa,
    })
}

/// Parse `hull_vertices` into the integer tick frame before any geometry runs.
///
/// Coordinates go through `Length`, which refuses a negative value, so a hull crossing the
/// wire is authored as non-negative offsets from the corner of its own bounding box. A library
/// caller may still centre a hull wherever it likes -- `hull::rotate` normalises either way --
/// but the wire keeps one convention so four engines cannot disagree about where an item's
/// frame starts.
fn parse_hull_vertices(value: Option<&Value>, unit: &str) -> PackResult<Option<Vec<Vertex>>> {
    let Some(raw) = value else {
        return Ok(None);
    };
    let entries = raw
        .as_array()
        .ok_or_else(|| PackError::InvalidInput("item.hull_vertices must be an array".into()))?;
    let mut vertices = Vec::with_capacity(entries.len());
    for entry in entries {
        let point = entry.as_object().ok_or_else(|| {
            PackError::InvalidInput("item.hull_vertices entries must be objects".into())
        })?;
        let mut coordinates = [0i64; 3];
        for (index, axis) in ["x", "y", "z"].into_iter().enumerate() {
            let measure = point.get(axis).ok_or_else(|| {
                PackError::InvalidInput(format!("item.hull_vertices entry needs {axis}"))
            })?;
            coordinates[index] = Length::parse(measure, unit)?.0;
        }
        vertices.push(coordinates);
    }
    Ok(Some(vertices))
}

/// Admit an item's shape, or refuse it with the reason.
///
/// The one rule here spanning four fields at once: which are required, which are forbidden,
/// and what the survivors must agree with. Mirrors the other engines exactly -- all four must
/// refuse the same requests.
fn admit_shape(
    shape_type: ShapeType,
    hull_vertices: Option<Vec<Vertex>>,
    ratio_ppm: Option<i64>,
    limit_kpa: Option<i64>,
    dimensions: Dimensions,
    nesting_height: Option<Length>,
) -> PackResult<Option<Vec<Vertex>>> {
    let foreign: &[(&str, bool)] = match shape_type {
        ShapeType::ConvexHull => &[
            ("compression_ratio", ratio_ppm.is_some()),
            ("max_compression_pressure_kpa", limit_kpa.is_some()),
        ],
        ShapeType::Compressible => &[("hull_vertices", hull_vertices.is_some())],
        ShapeType::RigidCuboid => &[
            ("hull_vertices", hull_vertices.is_some()),
            ("compression_ratio", ratio_ppm.is_some()),
            ("max_compression_pressure_kpa", limit_kpa.is_some()),
        ],
    };
    let label = shape_label(shape_type);
    for (name, present) in foreign {
        if *present {
            return Err(PackError::InvalidInput(format!(
                "{name} is not part of a {label} item"
            )));
        }
    }
    // Both rewrite occupied height. Choosing an order silently would give four engines four
    // contracts, so the interaction is refused until a task defines it.
    if nesting_height.is_some() && shape_type != ShapeType::RigidCuboid {
        return Err(PackError::InvalidInput(format!(
            "nesting_height with shape_type {label} is not supported yet"
        )));
    }
    match shape_type {
        ShapeType::ConvexHull => {
            let vertices = hull_vertices.ok_or_else(|| {
                PackError::InvalidInput("a convex_hull item requires hull_vertices".into())
            })?;
            let canonical =
                hull::validate(&vertices).map_err(|error| PackError::InvalidInput(error.0))?;
            let (low, high) = hull::bounding_extent(&canonical);
            let declared = [dimensions.length.0, dimensions.width.0, dimensions.height.0];
            for axis in 0..3 {
                // `dimensions` stays the broad phase and the candidate-generation envelope, so
                // a hull poking out of it would be collision-tested against space the solver
                // never reserved.
                if high[axis] - low[axis] > declared[axis] {
                    return Err(PackError::InvalidInput(
                        "hull_vertices span does not fit inside dimensions".into(),
                    ));
                }
            }
            Ok(Some(canonical))
        }
        ShapeType::Compressible => {
            let (Some(ratio), Some(limit)) = (ratio_ppm, limit_kpa) else {
                return Err(PackError::InvalidInput(
                    "a compressible item requires both compression_ratio and \
                     max_compression_pressure_kpa"
                        .into(),
                ));
            };
            if !(0..=crate::compression::PPM as i64).contains(&ratio) {
                return Err(PackError::InvalidInput(
                    "compression_ratio must be between zero and one".into(),
                ));
            }
            if limit < 0 {
                return Err(PackError::InvalidInput(
                    "max_compression_pressure_kpa cannot be negative".into(),
                ));
            }
            Ok(hull_vertices)
        }
        ShapeType::RigidCuboid => Ok(hull_vertices),
    }
}

pub(crate) fn shape_label(shape_type: ShapeType) -> &'static str {
    match shape_type {
        ShapeType::RigidCuboid => "rigid_cuboid",
        ShapeType::ConvexHull => "convex_hull",
        ShapeType::Compressible => "compressible",
    }
}

fn parse_container(value: &Value, unit: &str) -> PackResult<Container> {
    let map = value
        .as_object()
        .ok_or_else(|| PackError::InvalidInput("container must be object".into()))?;
    let obstacles = map
        .get("obstacles")
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .map(|value| parse_obstacle(value, unit))
                .collect::<PackResult<Vec<_>>>()
        })
        .transpose()?
        .unwrap_or_default();
    Ok(Container {
        id: str_field(map, "id")?,
        inner_dimensions: parse_dimensions(map.get("inner_dimensions"), unit)?,
        outer_dimensions: map
            .get("outer_dimensions")
            .map(|value| parse_dimensions(Some(value), unit))
            .transpose()?,
        tare_weight: parse_optional_weight(map.get("tare_weight"), "g")?,
        max_payload: map
            .get("max_payload")
            .map(|value| Weight::parse(value, "g"))
            .transpose()?,
        cost_minor: map.get("cost_minor").and_then(Value::as_i64).unwrap_or(0),
        quantity: map
            .get("quantity")
            .and_then(Value::as_u64)
            .map(|value| value as usize),
        obstacles,
        tags: string_set(map.get("tags")),
        max_items: map
            .get("max_items")
            .and_then(Value::as_u64)
            .map(|value| value as usize),
        metadata: object_map(map.get("metadata")),
        axles: parse_axles(map.get("axles"), unit)?,
        void_fill_reserve_ppm: parse_ratio_ppm(map.get("void_fill_reserve_ratio"))?,
        tag_limits: match map.get("tag_limits") {
            None => BTreeMap::new(),
            Some(Value::Object(limits)) => limits
                .iter()
                .map(|(tag, value)| {
                    value
                        .as_u64()
                        .filter(|limit| *limit > 0)
                        .map(|limit| (tag.clone(), limit as usize))
                        .ok_or_else(|| {
                            PackError::InvalidInput(format!(
                                "container.tag_limits.{tag} must be positive"
                            ))
                        })
                })
                .collect::<PackResult<BTreeMap<_, _>>>()?,
            Some(_) => {
                return Err(PackError::InvalidInput(
                    "container.tag_limits must be an object".into(),
                ));
            }
        },
        max_stack_density: map
            .get("max_stack_density")
            .map(|value| Weight::parse(value, "g"))
            .transpose()?,
        rate_table: parse_rate_table(map.get("rate_table"))?,
        access_directions: parse_access_directions(map.get("access_directions"))?,
    })
}

/// The container walls an item may be unloaded through.
///
/// Canonicalised into `ALL_DIRECTIONS` order and deduplicated rather than kept as given:
/// two callers naming the same doors in a different order must search identically, and this
/// is the one place a request reaches the field. The schema already constrains the values,
/// so the check here is the engine refusing to trust a schema it does not run.
fn parse_access_directions(value: Option<&Value>) -> PackResult<Vec<String>> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let Some(list) = value.as_array() else {
        return Err(PackError::InvalidInput(
            "container.access_directions must be an array".into(),
        ));
    };
    let mut given = Vec::with_capacity(list.len());
    for entry in list {
        let Some(direction) = entry.as_str() else {
            return Err(PackError::InvalidInput(
                "container.access_directions entries must be strings".into(),
            ));
        };
        if !ALL_DIRECTIONS.contains(&direction) {
            return Err(PackError::InvalidInput(format!(
                "unknown movement direction {direction}"
            )));
        }
        given.push(direction);
    }
    Ok(ALL_DIRECTIONS
        .iter()
        .filter(|direction| given.contains(*direction))
        .map(|direction| (*direction).to_string())
        .collect())
}

fn parse_rate_table(value: Option<&Value>) -> PackResult<Option<RateTable>> {
    let Some(value) = value else {
        return Ok(None);
    };
    let map = value
        .as_object()
        .ok_or_else(|| PackError::InvalidInput("container.rate_table must be an object".into()))?;
    let integers = |key: &str| -> PackResult<Vec<i64>> {
        map.get(key)
            .and_then(Value::as_array)
            .ok_or_else(|| {
                PackError::InvalidInput(format!("container.rate_table.{key} must be an array"))
            })?
            .iter()
            .map(|entry| {
                entry.as_i64().ok_or_else(|| {
                    PackError::InvalidInput(format!(
                        "container.rate_table.{key} must hold integers"
                    ))
                })
            })
            .collect()
    };
    let optional = |key: &str| -> i64 { map.get(key).and_then(Value::as_i64).unwrap_or(0) };
    let table = RateTable {
        weight_brackets_g: integers("weight_brackets_g")?,
        prices_minor: integers("prices_minor")?,
        minimum_charge_minor: optional("minimum_charge_minor"),
        fuel_surcharge_permille: optional("fuel_surcharge_permille"),
    };
    if table.weight_brackets_g.is_empty() {
        return Err(PackError::InvalidInput(
            "container.rate_table requires at least one weight bracket".into(),
        ));
    }
    if table.weight_brackets_g.len() != table.prices_minor.len() {
        return Err(PackError::InvalidInput(
            "container.rate_table weight_brackets_g and prices_minor must be the same length"
                .into(),
        ));
    }
    if table
        .weight_brackets_g
        .windows(2)
        .any(|pair| pair[1] <= pair[0])
        || table
            .weight_brackets_g
            .first()
            .is_some_and(|first| *first <= 0)
    {
        return Err(PackError::InvalidInput(
            "container.rate_table weight brackets must be strictly ascending and positive".into(),
        ));
    }
    Ok(Some(table))
}

fn parse_axles(value: Option<&Value>, unit: &str) -> PackResult<Option<[Axle; 2]>> {
    let Some(values) = value else {
        return Ok(None);
    };
    let values = values
        .as_array()
        .ok_or_else(|| PackError::InvalidInput("container.axles must be an array".into()))?;
    if values.len() != 2 {
        return Err(PackError::InvalidInput(
            "container.axles must contain exactly front and rear".into(),
        ));
    }
    let parse = |value: &Value| -> PackResult<Axle> {
        let map = value
            .as_object()
            .ok_or_else(|| PackError::InvalidInput("axle must be object".into()))?;
        Ok(Axle {
            position: Length::parse(
                map.get("position")
                    .ok_or_else(|| PackError::InvalidInput("axle.position is required".into()))?,
                unit,
            )?,
            max_load: map
                .get("max_load")
                .map(|value| Weight::parse(value, "g"))
                .transpose()?,
        })
    };
    Ok(Some([parse(&values[0])?, parse(&values[1])?]))
}

fn parse_obstacle(value: &Value, unit: &str) -> PackResult<Obstacle> {
    let map = value
        .as_object()
        .ok_or_else(|| PackError::InvalidInput("obstacle must be object".into()))?;
    let additional_boxes = map
        .get("additional_boxes")
        .and_then(Value::as_array)
        .map(|boxes| {
            boxes
                .iter()
                .map(|value| {
                    value
                        .as_object()
                        .ok_or_else(|| {
                            PackError::InvalidInput("obstacle additional box must be object".into())
                        })
                        .and_then(|box_| parse_aabb(box_, unit))
                })
                .collect::<PackResult<Vec<_>>>()
        })
        .transpose()?
        .unwrap_or_default();
    Ok(Obstacle {
        id: str_field(map, "id")?,
        box_: parse_aabb(map, unit)?,
        additional_boxes,
    })
}

fn parse_aabb(map: &Map<String, Value>, unit: &str) -> PackResult<Aabb> {
    let origin = map.get("origin").and_then(Value::as_object);
    Ok(Aabb {
        origin: Point {
            x: parse_optional_length(origin.and_then(|value| value.get("x")), unit)?.0,
            y: parse_optional_length(origin.and_then(|value| value.get("y")), unit)?.0,
            z: parse_optional_length(origin.and_then(|value| value.get("z")), unit)?.0,
        },
        dimensions: parse_dimensions(map.get("dimensions"), unit)?,
    })
}

fn parse_dimensions(value: Option<&Value>, unit: &str) -> PackResult<Dimensions> {
    let map = value
        .and_then(Value::as_object)
        .ok_or_else(|| PackError::InvalidInput("dimensions must be object".into()))?;
    let dimensions = Dimensions {
        length: Length::parse(
            map.get("length")
                .ok_or_else(|| PackError::InvalidInput("missing length".into()))?,
            unit,
        )?,
        width: Length::parse(
            map.get("width")
                .ok_or_else(|| PackError::InvalidInput("missing width".into()))?,
            unit,
        )?,
        height: Length::parse(
            map.get("height")
                .ok_or_else(|| PackError::InvalidInput("missing height".into()))?,
            unit,
        )?,
    };
    if dimensions.length.0 <= 0 || dimensions.width.0 <= 0 || dimensions.height.0 <= 0 {
        return Err(PackError::InvalidInput(
            "dimensions must be positive".into(),
        ));
    }
    Ok(dimensions)
}

fn validate_request(request: &PackingRequest) -> PackResult<()> {
    if request.items.is_empty() || request.containers.is_empty() {
        return Err(PackError::InvalidInput(
            "items and containers are required".into(),
        ));
    }
    if !(0.0..=1.0).contains(&request.config.minimum_support_ratio) {
        return Err(PackError::InvalidInput(
            "minimum_support_ratio must be between 0 and 1".into(),
        ));
    }
    let mut ids = BTreeSet::new();
    for item in &request.items {
        if item.quantity == 0 || !ids.insert(item.id.clone()) {
            return Err(PackError::InvalidInput(format!(
                "invalid or duplicate item id {}",
                item.id
            )));
        }
        if !(0.0..=1.0).contains(&item.minimum_support_ratio) {
            return Err(PackError::InvalidInput(format!(
                "item {} has invalid minimum_support_ratio",
                item.id
            )));
        }
        if let Some(depth) = item.nesting_height
            && (depth.0 < 0 || depth.0 >= item.dimensions.height.0)
        {
            return Err(PackError::InvalidInput(format!(
                "item {} has invalid nesting_height",
                item.id
            )));
        }
        if item.max_stacked_items == Some(0) {
            return Err(PackError::InvalidInput(format!(
                "item {} max_stacked_items must be at least 1",
                item.id
            )));
        }
        if let Some(rule) = item.ground_contact_rule.as_deref()
            && !matches!(rule, "free" | "covered" | "single" | "multiple")
        {
            return Err(PackError::InvalidInput(format!(
                "item {} has invalid ground_contact_rule",
                item.id
            )));
        }
    }
    ids.clear();
    for container in &request.containers {
        if !ids.insert(container.id.clone()) {
            return Err(PackError::InvalidInput(format!(
                "duplicate container id {}",
                container.id
            )));
        }
        if !(0..=1_000_000).contains(&container.void_fill_reserve_ppm) {
            return Err(PackError::InvalidInput(format!(
                "container {} has invalid void_fill_reserve_ratio",
                container.id
            )));
        }
        let boundary = Aabb {
            origin: Point::ZERO,
            dimensions: container.inner_dimensions,
        };
        if let Some([front, rear]) = container.axles
            && (front.position.0 >= rear.position.0
                || front.position.0 < 0
                || rear.position.0 > container.inner_dimensions.length.0)
        {
            return Err(PackError::InvalidInput(format!(
                "container {} has invalid axle positions",
                container.id
            )));
        }
        for obstacle in &container.obstacles {
            if obstacle.boxes().any(|box_| !boundary.contains(box_)) {
                return Err(PackError::InvalidInput(format!(
                    "obstacle {} outside {}",
                    obstacle.id, container.id
                )));
            }
        }
    }
    Ok(())
}

fn parse_optional_length(value: Option<&Value>, unit: &str) -> PackResult<Length> {
    let zero = Value::String("0".to_owned());
    Length::parse(value.unwrap_or(&zero), unit)
}

fn parse_optional_weight(value: Option<&Value>, unit: &str) -> PackResult<Weight> {
    let zero = Value::String("0".to_owned());
    Weight::parse(value.unwrap_or(&zero), unit)
}

fn parse_ratio_ppm(value: Option<&Value>) -> PackResult<i128> {
    let Some(value) = value else {
        return Ok(0);
    };
    let ratio = value.as_f64().ok_or_else(|| {
        PackError::InvalidInput("void_fill_reserve_ratio must be a number".into())
    })?;
    if !(0.0..=1.0).contains(&ratio) {
        return Err(PackError::InvalidInput(
            "void_fill_reserve_ratio must be between 0 and 1".into(),
        ));
    }
    Ok((ratio * 1_000_000.0 + 0.5) as i128)
}

fn str_field(map: &Map<String, Value>, key: &str) -> PackResult<String> {
    map.get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| PackError::InvalidInput(format!("missing {key}")))
}

/// The largest `stop_index` every engine can carry identically.
///
/// Route order is decided by comparing stop indices, and JavaScript holds numbers as
/// doubles: `JSON.parse` collapses 2**53 + 1 to 2**53 before any constraint sees it, so
/// two consecutive stops above this bound become one number there while Rust's integers
/// keep them apart. The JavaScript engine already refuses anything outside the safe
/// range; this makes the others agree rather than accept a value they would order
/// differently.
const MAX_EXACT_STOP_INDEX: u64 = (1u64 << 53) - 1;

fn optional_stop_index(map: &Map<String, Value>) -> PackResult<Option<usize>> {
    match map.get("stop_index") {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_u64()
            .filter(|number| *number <= MAX_EXACT_STOP_INDEX)
            .and_then(|number| usize::try_from(number).ok())
            .map(Some)
            .ok_or_else(|| {
                PackError::InvalidInput(
                    "stop_index must be a non-negative safe integer".to_string(),
                )
            }),
    }
}

fn optional_usize(map: &Map<String, Value>, key: &str, scope: &str) -> PackResult<Option<usize>> {
    match map.get(key) {
        None => Ok(None),
        Some(value) => value
            .as_u64()
            .and_then(|number| usize::try_from(number).ok())
            .map(Some)
            .ok_or_else(|| {
                PackError::InvalidInput(format!("{scope}.{key} must be a non-negative integer"))
            }),
    }
}

fn optional_string(map: &Map<String, Value>, key: &str, scope: &str) -> PackResult<Option<String>> {
    match map.get(key) {
        None => Ok(None),
        Some(Value::String(value)) => Ok(Some(value.clone())),
        Some(_) => Err(PackError::InvalidInput(format!(
            "{scope}.{key} must be a string"
        ))),
    }
}

fn optional_string_set(value: Option<&Value>, path: &str) -> PackResult<BTreeSet<String>> {
    match value {
        None => Ok(BTreeSet::new()),
        Some(Value::Array(values)) => values
            .iter()
            .map(|value| {
                value
                    .as_str()
                    .map(str::to_owned)
                    .ok_or_else(|| PackError::InvalidInput(format!("{path} must contain strings")))
            })
            .collect(),
        Some(_) => Err(PackError::InvalidInput(format!("{path} must be an array"))),
    }
}

fn usize_field(map: &Map<String, Value>, key: &str, default: usize) -> usize {
    map.get(key)
        .and_then(Value::as_u64)
        .map(|value| value as usize)
        .unwrap_or(default)
}

fn u64_field(map: &Map<String, Value>, key: &str, default: u64) -> u64 {
    map.get(key).and_then(Value::as_u64).unwrap_or(default)
}

fn string_set(value: Option<&Value>) -> BTreeSet<String> {
    value
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

fn object_map(value: Option<&Value>) -> BTreeMap<String, Value> {
    value
        .and_then(Value::as_object)
        .map(|map| {
            map.iter()
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect()
        })
        .unwrap_or_default()
}

fn parse_rotation(value: &str) -> Option<Rotation> {
    match value {
        "LWH" => Some(Rotation::Lwh),
        "LHW" => Some(Rotation::Lhw),
        "WLH" => Some(Rotation::Wlh),
        "WHL" => Some(Rotation::Whl),
        "HLW" => Some(Rotation::Hlw),
        "HWL" => Some(Rotation::Hwl),
        _ => None,
    }
}

#[cfg(test)]
mod unsupported_field_tests {
    use super::{
        UNSUPPORTED_CONFIGURATION_FIELDS, UNSUPPORTED_CONTAINER_FIELDS, UNSUPPORTED_ITEM_FIELDS,
        UNSUPPORTED_REQUEST_FIELDS, UNSUPPORTED_SHAPE_TYPES, reject_listed_fields,
    };
    use serde_json::{Map, Value, json};
    use std::collections::BTreeSet;

    fn object(value: Value) -> Map<String, Value> {
        value.as_object().expect("a request object").clone()
    }

    #[test]
    fn a_listed_field_is_rejected_wherever_it_appears() {
        let request = object(json!({
            "policy": {"rules": []},
            "configuration": {"tariff": {}},
            "items": [{"id": "a"}, {"id": "b", "hazmat_class": "3"}],
            "containers": [{"id": "c", "rate_table": {}}],
        }));

        let error = reject_listed_fields(
            &request,
            &["policy"],
            &["tariff"],
            &["hazmat_class"],
            &["rate_table"],
            &[],
        )
        .expect_err("every listed field should be refused");
        let message = error.to_string();

        assert!(message.starts_with("unsupported_feature:"), "{message}");
        for expected in [
            "policy",
            "configuration.tariff",
            "item.hazmat_class",
            "container.rate_table",
        ] {
            assert!(
                message.contains(expected),
                "{expected} missing from {message}"
            );
        }
    }

    #[test]
    fn one_field_on_several_entries_is_named_once() {
        // The name identifies the field, not each position it was found in: a request
        // with fifty containers should not produce fifty copies of the same complaint.
        let request = object(json!({
            "containers": [{"id": "a", "rate_table": {}}, {"id": "b", "rate_table": {}}],
        }));

        let message = reject_listed_fields(&request, &[], &[], &[], &["rate_table"], &[])
            .expect_err("the field is listed")
            .to_string();

        assert_eq!(
            message.matches("container.rate_table").count(),
            1,
            "{message}"
        );
    }

    #[test]
    fn a_request_that_touches_nothing_listed_is_accepted() {
        let request = object(json!({
            "items": [{"id": "a", "weight": "1"}],
            "containers": [{"id": "c", "cost_minor": 100}],
        }));

        assert!(
            reject_listed_fields(
                &request,
                &["policy"],
                &["tariff"],
                &["hazmat_class"],
                &["rate_table"],
                &[]
            )
            .is_ok()
        );
    }

    #[test]
    fn the_unsupported_lists_match_what_the_field_matrix_records() {
        // Every refusal this engine makes is recorded in the matrix, and the reverse. The
        // assertion used to be that all four lists are empty, which was the same thing
        // while they were -- and stopped being the same thing the moment
        // populated one. What the coupling is actually for is that the corpus *asserts*
        // each rejection instead of merely tolerating it, so read the matrix and compare
        // both directions.
        // The matrix is vendored one level above this crate, in the binding workspace; a
        // published copy of the crate alone does not carry it.
        let matrix_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/public-field-matrix.json");
        let Ok(matrix_text) = std::fs::read_to_string(&matrix_path) else {
            eprintln!("skipping: the shared public field matrix is not part of this package");
            return;
        };
        let matrix: Value = serde_json::from_str(&matrix_text).expect("matrix JSON");
        // An engine refuses a field by name; the matrix is keyed on the schema's leaves,
        // so one refused field is several rows. The matrix's own `rejection_name` -- the
        // name the conformance harness demands in the diagnostic -- ties the rows to the
        // field, so the comparison is made on that and never inferred from the spelling
        // of a path. A value-keyed template such as `item.shape_type={value}` names the
        // field before `=`.
        fn field_of(rejection_name: &str) -> String {
            rejection_name
                .split('=')
                .next()
                .unwrap_or(rejection_name)
                .to_owned()
        }
        let support_sets = &matrix["support_sets"];
        let rows = matrix["fields"]
            .as_object()
            .expect("fields")
            .iter()
            .map(|(path, row)| {
                let support =
                    support_sets[row["support"].as_str().expect("a support set name")]["rust"]
                        .as_str()
                        .expect("a support value")
                        .to_owned();
                let name = row["rejection_name"].as_str().map(field_of);
                (path.clone(), name, support)
            })
            .collect::<Vec<_>>();
        let rejected_by_matrix = rows
            .iter()
            .filter(|(_, _, support)| support == "rejected:unsupported_feature")
            .map(|(path, name, _)| {
                name.clone()
                    .unwrap_or_else(|| panic!("{path}: no rejection_name"))
            })
            .collect::<BTreeSet<_>>();

        let mut declared = BTreeSet::new();
        declared.extend(
            UNSUPPORTED_REQUEST_FIELDS
                .iter()
                .map(|name| (*name).to_owned()),
        );
        declared.extend(
            UNSUPPORTED_CONFIGURATION_FIELDS
                .iter()
                .map(|name| format!("configuration.{name}")),
        );
        declared.extend(
            UNSUPPORTED_ITEM_FIELDS
                .iter()
                .map(|name| format!("item.{name}")),
        );
        declared.extend(
            UNSUPPORTED_CONTAINER_FIELDS
                .iter()
                .map(|name| format!("container.{name}")),
        );
        if !UNSUPPORTED_SHAPE_TYPES.is_empty() {
            declared.insert("item.shape_type".to_owned());
        }

        assert_eq!(
            declared, rejected_by_matrix,
            "the engine and the matrix disagree about what Rust refuses"
        );
        // A field refused by name is refused on every one of its leaves: a row that names
        // a refused field while recording this engine as implementing it is a matrix error.
        let half_recorded = rows
            .iter()
            .filter(|(_, name, support)| {
                name.as_deref().is_some_and(|name| declared.contains(name))
                    && support != "rejected:unsupported_feature"
            })
            .map(|(path, _, _)| path.clone())
            .collect::<Vec<_>>();
        assert!(
            half_recorded.is_empty(),
            "rows recorded as implemented for a field Rust refuses: {half_recorded:?}"
        );
    }

    #[test]
    fn the_default_shape_type_is_served_rather_than_refused() {
        // `rigid_cuboid` is implemented, so spelling the default out must not be a
        // rejection. This is why `shape_type` is not in the presence-keyed table: that
        // table means "this engine does not implement the field at all", and a value-keyed
        // refusal is a different claim. A caller who writes the default explicitly is
        // asking for what they already get.
        let request = object(json!({
            "units": {"length": "mm"},
            "items": [{
                "id": "a",
                "shape_type": "rigid_cuboid",
                "dimensions": {"length": "100", "width": "100", "height": "100"},
            }],
            "containers": [{
                "id": "c",
                "inner_dimensions": {"length": "200", "width": "200", "height": "200"},
            }],
        }));

        assert!(
            reject_listed_fields(&request, &[], &[], &[], &[], UNSUPPORTED_SHAPE_TYPES).is_ok()
        );
    }

    #[test]
    fn an_unimplemented_shape_type_names_the_value_it_refused() {
        let request = object(json!({"items": [{"id": "a", "shape_type": "convex_hull"}]}));
        let message = reject_listed_fields(&request, &[], &[], &[], &[], &["convex_hull"])
            .expect_err("the value is listed")
            .to_string();
        assert!(message.contains("item.shape_type=convex_hull"), "{message}");
    }

    /// What a caller gets wrong about a shape, and what they are told.
    ///
    /// These refusals are the request contract, not internal validation: each one is a message
    /// a caller reads and acts on. The shared rejection corpus checks that all four engines
    /// agree; these unit tests additionally assert what this engine says.
    #[test]
    fn a_shape_refuses_the_data_it_cannot_use() {
        use super::pack_json;

        let pack = |items: serde_json::Value| {
            let request = json!({
                "units": {"length": "mm"},
                "items": items,
                "containers": [{
                    "id": "c",
                    "inner_dimensions": {"length": "200", "width": "200", "height": "200"},
                }],
            });
            pack_json(&request.to_string())
                .expect_err("the request is not admissible")
                .to_string()
        };

        let dimensions = json!({"length": "100", "width": "100", "height": "100"});

        let message = pack(json!([{
            "id": "a", "shape_type": "convex_hull", "dimensions": dimensions,
        }]));
        assert!(
            message.contains("a convex_hull item requires hull_vertices"),
            "{message}"
        );

        // A vertex is three coordinates, and a missing one is named rather than defaulted to
        // zero: a hull silently flattened onto a plane encloses no volume and would pass
        // through everything it meets.
        let message = pack(json!([{
            "id": "a", "shape_type": "convex_hull", "dimensions": dimensions,
            "hull_vertices": [
                {"x": "0", "y": "0", "z": "0"},
                {"x": "100", "y": "0"},
                {"x": "0", "y": "100", "z": "0"},
                {"x": "0", "y": "0", "z": "100"},
            ],
        }]));
        assert!(
            message.contains("item.hull_vertices entry needs z"),
            "{message}"
        );

        // Four coplanar vertices enclose nothing. Refused rather than repaired, for the same
        // reason: a zero-volume solid is separated from everything on its own normal.
        let message = pack(json!([{
            "id": "a", "shape_type": "convex_hull", "dimensions": dimensions,
            "hull_vertices": [
                {"x": "0", "y": "0", "z": "0"},
                {"x": "100", "y": "0", "z": "0"},
                {"x": "0", "y": "100", "z": "0"},
                {"x": "100", "y": "100", "z": "0"},
            ],
        }]));
        assert!(message.contains("coplanar"), "{message}");

        // And the two shapes do not share their data: a `compression_ratio` quietly dropped on
        // a hull reads back as an item packed to limits it never had.
        let message = pack(json!([{
            "id": "a", "dimensions": dimensions, "compression_ratio": 0.25,
        }]));
        assert!(message.contains("compression_ratio"), "{message}");
    }
}

#[cfg(test)]
mod public_feature_tests {
    use super::{pack_json, rebalance_json};

    fn request_with(fragment: &str) -> String {
        format!(
            r#"{{"configuration":{{{fragment}}},"items":[{{"id":"a","dimensions":{{"length":"10","width":"10","height":"10"}}}}],"containers":[{{"id":"c","inner_dimensions":{{"length":"20","width":"20","height":"20"}}}}]}}"#
        )
    }

    #[test]
    fn explicit_objective_is_applied_and_reported() {
        let output = pack_json(&request_with(r#""objective":"lowest_cost""#)).unwrap();
        let result: serde_json::Value = serde_json::from_str(&output).unwrap();
        assert_eq!(result["objective"], "lowest_cost");
        assert_eq!(
            result["score"],
            serde_json::json!([0, 0, 1, 875000, 500000])
        );
    }

    #[test]
    fn catalog_versions_are_pinned_and_ambiguous_duplicates_are_rejected() {
        let request = r#"{"catalog_versions_used":[{"catalog_id":"items","version":7,"effective_at":10,"resolved_at":20},{"catalog_id":"cartons","version":3,"effective_at":11,"resolved_at":20}],"items":[{"id":"a","dimensions":{"length":"10","width":"10","height":"10"}}],"containers":[{"id":"c","inner_dimensions":{"length":"20","width":"20","height":"20"}}]}"#;
        let result: serde_json::Value =
            serde_json::from_str(&pack_json(request).expect("catalog request")).unwrap();
        assert_eq!(
            result["catalog_versions_used"],
            serde_json::json!([
                {"catalog_id":"items","version":7,"effective_at":10,"resolved_at":20},
                {"catalog_id":"cartons","version":3,"effective_at":11,"resolved_at":20}
            ])
        );

        let duplicate = r#"{"catalog_versions_used":[{"catalog_id":"items","version":7,"effective_at":10,"resolved_at":20},{"catalog_id":"items","version":8,"effective_at":11,"resolved_at":20}],"items":[{"id":"a","dimensions":{"length":"10","width":"10","height":"10"}}],"containers":[{"id":"c","inner_dimensions":{"length":"20","width":"20","height":"20"}}]}"#;
        let error = pack_json(duplicate).unwrap_err().to_string();
        assert!(error.contains("ambiguous duplicate"), "{error}");
    }

    #[test]
    fn explicit_solver_order_is_preserved_in_start_records() {
        // The assertion is about explicit portfolio order, not host speed. Counted work
        // remains the deterministic bound; wall time is only a remote hang ceiling on
        // cold linux/amd64 emulation.
        let request = r#"{"configuration":{"solvers":["grid","layer","extreme_points"],"time_limit_ms":300000,"effort_budget":{"max_candidates_evaluated":1000000,"max_placement_attempts":1000000,"max_search_nodes":1000000}},"items":[{"id":"a","dimensions":{"length":"10","width":"10","height":"10"}}],"containers":[{"id":"c","inner_dimensions":{"length":"20","width":"20","height":"20"}}]}"#;
        let result: serde_json::Value =
            serde_json::from_str(&pack_json(request).expect("explicit solver portfolio")).unwrap();
        let starts = result["termination"]["starts"].as_array().unwrap();
        let names = starts
            .iter()
            .map(|start| {
                start["id"]
                    .as_str()
                    .unwrap()
                    .split([':', '#'])
                    .next()
                    .unwrap()
            })
            .collect::<Vec<_>>();
        assert_eq!(names[0], "grid");
        assert_eq!(names[1], "layer");
        assert!(names[2..].iter().all(|name| *name == "extreme_points"));
    }

    #[test]
    fn pinned_exact_small_rejects_an_instance_over_its_configured_limit() {
        let request = r#"{"configuration":{"solvers":["exact_small"],"exact_item_limit":7},"items":[{"id":"a","quantity":8,"dimensions":{"length":"10","width":"10","height":"10"}}],"containers":[{"id":"c","inner_dimensions":{"length":"100","width":"100","height":"100"}}]}"#;
        let error = pack_json(request).unwrap_err().to_string();
        assert!(error.contains("exact-small item limit exceeded"), "{error}");
    }

    #[test]
    fn lowest_cost_does_not_keep_the_first_more_expensive_container() {
        let request = r#"{"configuration":{"solver_profile":"fast","objective":"lowest_cost"},"items":[{"id":"a","dimensions":{"length":"10","width":"10","height":"10"}}],"containers":[{"id":"expensive","cost_minor":100,"inner_dimensions":{"length":"20","width":"20","height":"20"}},{"id":"cheap","cost_minor":1,"inner_dimensions":{"length":"20","width":"20","height":"20"}}]}"#;
        let result: serde_json::Value =
            serde_json::from_str(&pack_json(request).expect("lowest-cost request")).unwrap();
        assert_eq!(result["containers"][0]["container_type"], "cheap");
        assert_eq!(
            result["score"],
            serde_json::json!([0, 1, 1, 875000, 500000])
        );
    }

    #[test]
    fn container_eligibility_is_enforced() {
        let request = r#"{"configuration":{"time_limit_ms":60000},"items":[{"id":"a","dimensions":{"length":"10","width":"10","height":"10"},"eligible_container_tags":["cold"]}],"containers":[{"id":"c","inner_dimensions":{"length":"20","width":"20","height":"20"},"tags":["ambient"]}]}"#;
        let output = pack_json(request).unwrap();
        let result: serde_json::Value = serde_json::from_str(&output).unwrap();
        assert_eq!(result["complete"], false);
        assert_eq!(
            result["unpacked_items"][0]["reason"],
            "no_eligible_container"
        );
    }

    #[test]
    fn malformed_public_fields_fail_admission_instead_of_being_ignored() {
        assert!(
            pack_json(&request_with(r#""objective":17"#))
                .unwrap_err()
                .to_string()
                .contains("configuration.objective")
        );
        let item = r#"{"items":[{"id":"a","dimensions":{"length":"10","width":"10","height":"10"},"max_stacked_items":"1"}],"containers":[{"id":"c","inner_dimensions":{"length":"20","width":"20","height":"20"}}]}"#;
        assert!(
            pack_json(item)
                .unwrap_err()
                .to_string()
                .contains("item.max_stacked_items")
        );
        let container = r#"{"items":[{"id":"a","dimensions":{"length":"10","width":"10","height":"10"}}],"containers":[{"id":"c","inner_dimensions":{"length":"20","width":"20","height":"20"},"tag_limits":[]}]}"#;
        assert!(
            pack_json(container)
                .unwrap_err()
                .to_string()
                .contains("container.tag_limits")
        );
    }

    #[test]
    fn nesting_height_fits_the_same_extra_layer_as_python_and_php() {
        let request = r#"{"items":[{"id":"a","dimensions":{"length":"10","width":"10","height":"10"},"quantity":3,"nesting_height":"2","allowed_rotations":["LWH"]}],"containers":[{"id":"c","inner_dimensions":{"length":"10","width":"10","height":"26"}}]}"#;
        let result: serde_json::Value =
            serde_json::from_str(&pack_json(request).expect("nesting request")).unwrap();
        assert_eq!(result["summary"]["packed_item_count"], 3);
        assert_eq!(
            result["containers"][0]["used_volume_ticks3"],
            "10649600000000000"
        );
    }

    #[test]
    fn nesting_height_uses_the_same_closed_open_validation_boundary() {
        let zero = r#"{"items":[{"id":"a","dimensions":{"length":"10","width":"10","height":"10"},"nesting_height":"0"}],"containers":[{"id":"c","inner_dimensions":{"length":"10","width":"10","height":"10"}}]}"#;
        assert!(pack_json(zero).is_ok());
        for invalid in ["-1", "10"] {
            let request = format!(
                r#"{{"items":[{{"id":"a","dimensions":{{"length":"10","width":"10","height":"10"}},"nesting_height":"{invalid}"}}],"containers":[{{"id":"c","inner_dimensions":{{"length":"10","width":"10","height":"10"}}}}]}}"#
            );
            assert!(pack_json(&request).is_err(), "{invalid} must be rejected");
        }
    }

    #[test]
    fn axle_reactions_are_reported_on_the_gross_basis() {
        let request = r#"{"configuration":{"time_limit_ms":60000},"items":[{"id":"a","dimensions":{"length":"10","width":"10","height":"10"}}],"containers":[{"id":"c","inner_dimensions":{"length":"20","width":"20","height":"20"},"tare_weight":"100","axles":[{"position":"5","max_load":"50"},{"position":"15","max_load":"50"}]}]}"#;
        let result: serde_json::Value =
            serde_json::from_str(&pack_json(request).expect("axle request")).unwrap();
        let reaction = &result["containers"][0]["axle_reactions"];
        assert_eq!(reaction["basis"], "gross");
        assert_eq!(reaction["front_numerator"], reaction["rear_numerator"]);
    }

    #[test]
    fn every_box_of_a_compound_obstacle_is_enforced() {
        let request = r#"{"configuration":{"time_limit_ms":60000},"items":[{"id":"a","dimensions":{"length":"10","width":"20","height":"20"},"allowed_rotations":["LWH"]}],"containers":[{"id":"c","inner_dimensions":{"length":"20","width":"20","height":"20"},"obstacles":[{"id":"o","dimensions":{"length":"1","width":"20","height":"20"},"additional_boxes":[{"origin":{"x":"11"},"dimensions":{"length":"9","width":"20","height":"20"}}]}]}]}"#;
        let result: serde_json::Value =
            serde_json::from_str(&pack_json(request).expect("compound obstacle request")).unwrap();
        assert_eq!(
            result["containers"][0]["placements"][0]["position"]["x"]["value"],
            "1"
        );
    }

    #[test]
    fn effort_budget_stops_at_the_exact_counted_boundary() {
        let request = r#"{"configuration":{"solver_profile":"fast","time_limit_ms":60000,"effort_budget":{"max_search_nodes":5}},"items":[{"id":"a","quantity":20,"dimensions":{"length":"10","width":"10","height":"10"}}],"containers":[{"id":"c","inner_dimensions":{"length":"100","width":"100","height":"100"}}]}"#;
        let result: serde_json::Value =
            serde_json::from_str(&pack_json(request).expect("effort request")).unwrap();
        assert_eq!(result["summary"]["packed_item_count"], 5);
        assert_eq!(result["algorithm"]["metrics"]["search_nodes_expanded"], 5);
        assert_eq!(result["termination"]["code"], "effort_limit");
        assert_eq!(result["algorithm"]["time_limit_reached"], false);
    }

    #[test]
    fn public_rebalance_json_uses_the_native_core_without_losing_accounting() {
        let request = r#"{
            "units":{"length":"mm"},
            "items":[
                {"id":"heavy","priority":3,"dimensions":{"length":"10","width":"10","height":"10"},"weight":"500"},
                {"id":"light","priority":2,"dimensions":{"length":"10","width":"10","height":"10"},"weight":"100"},
                {"id":"alone","priority":1,"dimensions":{"length":"10","width":"10","height":"10"},"weight":"100"}
            ],
            "containers":[{
                "id":"box","quantity":2,"max_items":2,"max_payload":"600",
                "inner_dimensions":{"length":"30","width":"20","height":"20"}
            }],
            "configuration":{"solvers":["extreme_points"],"time_limit_ms":5000}
        }"#;
        let packed = pack_json(request).expect("initial native packing");
        let balanced = rebalance_json(request, &packed, 64).expect("native rebalancing");
        let value: serde_json::Value = serde_json::from_str(&balanced).unwrap();
        assert_eq!(value["improved"], true);
        assert_eq!(value["moves"][0]["item_id"], "light#1");
        let ids = value["containers"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|container| container["placements"].as_array().unwrap())
            .map(|placement| placement["item_id"].as_str().unwrap())
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(
            ids,
            std::collections::BTreeSet::from(["alone#1", "heavy#1", "light#1"])
        );
    }
}

#[cfg(test)]
mod result_contract_tests {
    use super::pack_json;

    /// docs/OBJECTIVE.md and docs/SERIALIZATION.md promise `objective` (result-level)
    /// and `void_fill_reserve_ticks3` (per-container) on every result, in every
    /// implementation. Both were silently missing from this core until a differential
    /// harness compared its output against Python/PHP directly and caught it -- comparing
    /// Python and PHP results against each other never could, since the omission was
    /// specific to this implementation.
    #[test]
    fn the_default_objective_and_zero_void_fill_reserve_are_present() {
        let request = r#"{"items":[{"id":"a","dimensions":{"length":"10","width":"10","height":"10"}}],"containers":[{"id":"c","inner_dimensions":{"length":"20","width":"20","height":"20"}}]}"#;
        let output = pack_json(request).unwrap();
        let value: serde_json::Value = serde_json::from_str(&output).unwrap();
        assert_eq!(value["objective"], "default");
        assert_eq!(value["containers"][0]["void_fill_reserve_ticks3"], "0");
    }
}
