//! The portable operational artifact.
//!
//! `docs/OPERATIONAL-ARTIFACTS.md` is the contract. An execution plan tells an operator what to
//! do first; it does not say how big the boxes are, what the load weighs or which request and
//! solver produced it. The artifact is the one document that can be drawn, printed and traced
//! offline, and it is built so that it adds nothing a solver decided.
//!
//! ```
//! let request = r#"{"items":[],"containers":[]}"#;
//! let result = r#"{"status":"feasible","objective":"default","score":[0],
//!   "containers":[],"unpacked_items":[]}"#;
//! let artifact = packvium_core::artifacts::build_artifact_json(request, result, "{}").unwrap();
//! assert!(artifact.contains(r#""format":"packvium-operational-artifact/v1""#));
//! ```
//!
//! Held to byte-identical output with `packvium.artifacts`. It reads the request, the result
//! and the optional loading orders -- the plan's own inputs -- and calls no solver, validator,
//! renderer or clock. The plan inside it is exactly what `execution::build_plan_json` emits for
//! the same inputs. Placements are found by the plan's placement reference, never by
//! `item_id`, so there is one address format to drift.

use std::collections::HashMap;

use serde_json::{Map, Value, json};

use crate::canonical_json::{
    self, CanonicalJsonError, CanonicalJsonErrorCode, MAX_EXACT_MAGNITUDE,
};
use crate::execution;

pub const FORMAT: &str = "packvium-operational-artifact/v1";

/// The suite version of this builder, the same string in all four engines of one release.
/// The engine's own name is deliberately not recorded: four correct builders naming
/// themselves would emit four different documents. `make version-set` moves the crate
/// version, and this follows it.
pub const SUITE_VERSION: &str = crate::VERSION;

/// The deterministic part of `result.algorithm`. `duration_ms` is wall-clock time and never
/// enters an artifact.
const SOLVER_FIELDS: [&str; 5] = [
    "profile",
    "solver",
    "seed",
    "time_limit_reached",
    "effort_limit_reached",
];
const SOLVER_FLAGS: [&str; 2] = ["time_limit_reached", "effort_limit_reached"];

pub(crate) const DIMENSION_AXES: [&str; 3] = ["length", "width", "height"];
pub(crate) const POSITION_AXES: [&str; 3] = ["x", "y", "z"];

/// The closed set of refusals, shared by all four engines.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ArtifactErrorCode {
    InvalidRequest,
    InvalidResult,
    InvalidPlanInput,
    MixedUnits,
    NumberOutOfRange,
    InvalidString,
    InvalidValue,
    UnknownFormat,
    InvalidJson,
}

impl ArtifactErrorCode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidRequest => "invalid_request",
            Self::InvalidResult => "invalid_result",
            Self::InvalidPlanInput => "invalid_plan_input",
            Self::MixedUnits => "mixed_units",
            Self::NumberOutOfRange => "number_out_of_range",
            Self::InvalidString => "invalid_string",
            Self::InvalidValue => "invalid_value",
            Self::UnknownFormat => "unknown_format",
            Self::InvalidJson => "invalid_json",
        }
    }
}

/// The builder or an export was handed something an artifact cannot carry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactError {
    kind: ArtifactErrorCode,
    message: String,
}

impl ArtifactError {
    pub(crate) fn new(kind: ArtifactErrorCode, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    pub fn kind(&self) -> ArtifactErrorCode {
        self.kind
    }

    /// The closed code, e.g. `mixed_units`: what a caller in any engine matches on.
    pub fn code(&self) -> &'static str {
        self.kind.as_str()
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

impl std::fmt::Display for ArtifactError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code(), self.message)
    }
}

impl std::error::Error for ArtifactError {}

impl From<CanonicalJsonError> for ArtifactError {
    fn from(error: CanonicalJsonError) -> Self {
        let kind = match error.code {
            CanonicalJsonErrorCode::NumberOutOfRange => ArtifactErrorCode::NumberOutOfRange,
            CanonicalJsonErrorCode::InvalidString => ArtifactErrorCode::InvalidString,
            CanonicalJsonErrorCode::InvalidJson => ArtifactErrorCode::InvalidJson,
        };
        Self::new(kind, error.message)
    }
}

type ArtifactResult<T> = Result<T, ArtifactError>;

fn invalid_result<T>(message: impl Into<String>) -> ArtifactResult<T> {
    Err(ArtifactError::new(
        ArtifactErrorCode::InvalidResult,
        message,
    ))
}

/// Build the artifact for one validated result, as RFC 8785 canonical JSON.
///
/// `loading_orders_json` maps a container index, as a decimal string key, to the
/// engine-computed order its placements load in; it is passed straight to the plan builder.
/// All three are text for the reason `build_plan_json` takes text: it is the shape a
/// conformance probe can drive over a pipe.
///
/// O(R + P) time and space for a request of size R and P placements, plus O(k log k) key
/// comparisons per object of k keys when it is written. Each work-order line is found by a
/// hash lookup on its reference, never a scan.
pub fn build_artifact_json(
    request_json: &str,
    result_json: &str,
    loading_orders_json: &str,
) -> Result<String, ArtifactError> {
    let request = read_document(request_json)?;
    let result = read_document(result_json)?;
    let orders = read_document(loading_orders_json)?;
    let artifact = build_artifact(&request, &result, &orders)?;
    // Refused here rather than when someone serializes it: an artifact that exists must have
    // one spelling in every engine.
    canonical_artifact_json(&artifact)
}

/// JSON text, read exactly (see `canonical_json::parse`).
pub(crate) fn read_document(text: &str) -> ArtifactResult<Value> {
    Ok(canonical_json::parse(text)?)
}

/// The artifact's RFC 8785 canonical form, the bytes four engines are compared on.
pub(crate) fn canonical_artifact_json(artifact: &Value) -> ArtifactResult<String> {
    Ok(canonical_json::to_canonical_string(artifact)?)
}

/// The evaluation order below is the reference's, because it decides which code a result
/// with two faults is refused with: list entries, plan, units, provenance, geometry, work order.
fn build_artifact(request: &Value, result: &Value, orders: &Value) -> ArtifactResult<Value> {
    if !request.is_object() {
        return Err(ArtifactError::new(
            ArtifactErrorCode::InvalidRequest,
            "a request is a JSON object",
        ));
    }
    if !result.is_object() {
        return invalid_result("a result is a JSON object");
    }
    require_objects(result)?;
    if !(orders.is_object() || orders.is_null()) {
        return Err(ArtifactError::new(
            ArtifactErrorCode::InvalidPlanInput,
            "loading orders are a JSON object keyed by container index",
        ));
    }
    let plan = execution::build_plan(result, orders)
        .map_err(|error| ArtifactError::new(ArtifactErrorCode::InvalidPlanInput, error.0))?;

    let containers = list(result.get("containers"), "result.containers")?;
    let (length_unit, weight_unit) = units(containers)?;
    let provenance = provenance(request, result)?;
    let geometry = containers
        .iter()
        .enumerate()
        .map(|(index, container)| geometry(index, container))
        .collect::<ArtifactResult<Vec<Value>>>()?;
    let plan_containers = plan["containers"].as_array().map_or(&[][..], Vec::as_slice);
    let work_order = containers
        .iter()
        .zip(plan_containers)
        .enumerate()
        .map(|(index, (container, plan_container))| {
            work_order_container(index, container, plan_container, &length_unit, &weight_unit)
        })
        .collect::<ArtifactResult<Vec<Value>>>()?;

    Ok(json!({
        "format": FORMAT,
        "suite_version": SUITE_VERSION,
        "provenance": provenance,
        "plan": plan,
        "geometry": {"containers": geometry},
        "work_order": {
            "length_unit": length_unit,
            "weight_unit": weight_unit,
            "containers": work_order,
        },
    }))
}

// ------------------------------------------------------------------------------ provenance

fn provenance(request: &Value, result: &Value) -> ArtifactResult<Value> {
    let solver = solver(result.get("algorithm"))?;
    let catalogs = list(
        result.get("catalog_versions_used"),
        "result.catalog_versions_used",
    )?;
    for catalog in catalogs {
        catalog_reference(catalog)?;
    }
    let replay = replay(&solver);
    Ok(json!({
        // Embedded, not digested: only the request itself lets someone replay the artifact
        // without a lookup.
        "request": request,
        "catalog_versions_used": catalogs,
        "solver": solver,
        "replay": replay,
    }))
}

/// The result schema's closed catalog reference. The work order prints these fields, so a
/// mistyped one would print differently in every engine.
fn catalog_reference(catalog: &Value) -> ArtifactResult<()> {
    if !field(catalog, "catalog_id", "catalog_versions_used[]")?.is_string() {
        return invalid_result("catalog_versions_used[].catalog_id is not a string");
    }
    for name in ["version", "effective_at", "resolved_at"] {
        if !is_integer(field(catalog, name, "catalog_versions_used[]")?) {
            return invalid_result(format!("catalog_versions_used[].{name} is not an integer"));
        }
    }
    Ok(())
}

fn solver(algorithm: Option<&Value>) -> ArtifactResult<Value> {
    let Some(algorithm) = algorithm.filter(|value| !value.is_null()) else {
        return Ok(Value::Null);
    };
    let Some(record) = algorithm.as_object() else {
        return invalid_result("result.algorithm is not an object");
    };
    let missing: Vec<&str> = SOLVER_FIELDS
        .into_iter()
        .filter(|field| !record.contains_key(*field))
        .collect();
    if !missing.is_empty() {
        return invalid_result(format!("result.algorithm has no {}", missing.join(", ")));
    }
    for flag in SOLVER_FLAGS {
        if !record[flag].is_boolean() {
            return invalid_result(format!("result.algorithm.{flag} is not a boolean"));
        }
    }
    Ok(Value::Object(
        SOLVER_FIELDS
            .into_iter()
            .map(|field| (field.to_string(), record[field].clone()))
            .collect(),
    ))
}

/// `exact` only when a replay must reproduce the result. A search stopped by wall-clock time
/// cannot be reproduced, and a result that does not say how it was solved cannot be promised
/// to; claiming otherwise would be softening a proof level by another name.
fn replay(solver: &Value) -> Value {
    if solver.is_null() {
        return json!({"level": "not_guaranteed", "because": "provenance.solver"});
    }
    if solver["time_limit_reached"] == true {
        return json!({
            "level": "not_guaranteed",
            "because": "provenance.solver.time_limit_reached",
        });
    }
    json!({"level": "exact", "because": null})
}

// -------------------------------------------------------------------------------- geometry

fn geometry(index: usize, container: &Value) -> ArtifactResult<Value> {
    let inner_dimensions = tick_dimensions(field(container, "inner_dimensions", "containers[]")?)?;
    let placements = placements_of(container)?;
    let mut drawn = Vec::with_capacity(placements.len());
    for placement in placements {
        let reference = reference(index, placement)?;
        let dimensions =
            tick_dimensions(field(placement, "dimensions", "containers[].placements[]")?)?;
        drawn.push(json!({"placement": reference, "dimensions": dimensions}));
    }
    Ok(json!({
        "container_index": index,
        "inner_dimensions": inner_dimensions,
        "placements": drawn,
    }))
}

/// Lengths as decimal strings of ticks: the scene contract's spelling, and one no engine has
/// to hold as a native number.
fn tick_dimensions(dimensions: &Value) -> ArtifactResult<Value> {
    let mut ticks = Map::new();
    for axis in DIMENSION_AXES {
        let spelled = tick_string(field(dimensions, axis, "dimensions")?)?;
        ticks.insert(axis.to_string(), Value::String(spelled));
    }
    Ok(Value::Object(ticks))
}

fn tick_string(scalar: &Value) -> ArtifactResult<String> {
    let ticks = field(scalar, "ticks", "exact scalar")?;
    if !is_integer(ticks) {
        return invalid_result(format!("ticks {ticks} is not an integer"));
    }
    // Written as a string, so the canonical writer never sees it as a number; the range is
    // checked here instead, because JavaScript has already rounded such a value while parsing.
    let magnitude = ticks
        .as_i64()
        .map(i64::unsigned_abs)
        .or_else(|| ticks.as_u64())
        .unwrap_or(u64::MAX);
    if magnitude > MAX_EXACT_MAGNITUDE {
        return Err(ArtifactError::new(
            ArtifactErrorCode::NumberOutOfRange,
            format!("ticks {ticks} is beyond what every engine holds exactly"),
        ));
    }
    Ok(ticks.to_string())
}

// ------------------------------------------------------------------------------ work order

/// The display units, read from the result rather than re-derived from request defaults: the
/// result already rendered every value in them.
fn units(containers: &[Value]) -> ArtifactResult<(Value, Value)> {
    let Some(first) = containers.first() else {
        return Ok((Value::Null, Value::Null));
    };
    let inner = field(first, "inner_dimensions", "containers[0]")?;
    let length = field(
        field(inner, "length", "inner_dimensions")?,
        "unit",
        "length",
    )?;
    let payload = field(first, "payload_weight", "containers[0]")?;
    let weight = field(payload, "unit", "payload_weight")?;
    Ok((length.clone(), weight.clone()))
}

fn work_order_container(
    index: usize,
    container: &Value,
    plan_container: &Value,
    length_unit: &Value,
    weight_unit: &Value,
) -> ArtifactResult<Value> {
    let by_reference = placements_by_reference(index, container)?;
    let steps = plan_container["steps"]
        .as_array()
        .map_or(&[][..], Vec::as_slice);
    // One line per plan step, in plan step order: the order is the plan's, looked up, never
    // derived a second time.
    let lines = steps
        .iter()
        .map(|step| work_order_line(step, &by_reference, length_unit))
        .collect::<ArtifactResult<Vec<Value>>>()?;
    let payload_weight = rendered(
        field(container, "payload_weight", "containers[]")?,
        weight_unit,
    )?;
    let gross_weight = rendered(
        field(container, "gross_weight", "containers[]")?,
        weight_unit,
    )?;
    Ok(json!({
        "container_index": index,
        "container_type": container.get("container_type").unwrap_or(&Value::Null),
        "payload_weight": payload_weight,
        "gross_weight": gross_weight,
        "lines": lines,
    }))
}

fn work_order_line(
    step: &Value,
    by_reference: &HashMap<String, &Value>,
    length_unit: &Value,
) -> ArtifactResult<Value> {
    let reference = &step["placement"];
    // The plan built every step's reference from these same placements, so this cannot miss.
    let Some(placement) = by_reference.get(&reference.to_string()) else {
        return invalid_result("a plan step names no placement");
    };
    let mut line = Map::new();
    if let Some(sequence) = step.get("sequence") {
        line.insert("sequence".to_string(), sequence.clone());
    }
    let position = field(placement, "position", "placement")?;
    let dimensions = field(placement, "dimensions", "placement")?;
    line.insert("placement".to_string(), reference.clone());
    line.insert(
        "position".to_string(),
        rendered_axes(position, POSITION_AXES, "position", length_unit)?,
    );
    line.insert(
        "dimensions".to_string(),
        rendered_axes(dimensions, DIMENSION_AXES, "dimensions", length_unit)?,
    );
    Ok(Value::Object(line))
}

/// Placements keyed by the compact JSON of their reference. Only looked up, never iterated,
/// so the hash order cannot reach the output.
fn placements_by_reference(
    index: usize,
    container: &Value,
) -> ArtifactResult<HashMap<String, &Value>> {
    let placements = placements_of(container)?;
    let mut found = HashMap::with_capacity(placements.len());
    for placement in placements {
        found.insert(reference(index, placement)?.to_string(), placement);
    }
    if found.len() != placements.len() {
        return invalid_result(format!(
            "two placements in container {index} share an origin, type and orientation"
        ));
    }
    Ok(found)
}

fn rendered_axes(
    values: &Value,
    axes: [&str; 3],
    context: &str,
    unit: &Value,
) -> ArtifactResult<Value> {
    let mut spelled = Map::new();
    for axis in axes {
        spelled.insert(
            axis.to_string(),
            rendered(field(values, axis, context)?, unit)?,
        );
    }
    Ok(Value::Object(spelled))
}

/// The result's rendered value, copied and never re-rendered.
fn rendered(scalar: &Value, unit: &Value) -> ArtifactResult<Value> {
    let found = field(scalar, "unit", "exact scalar")?;
    if found != unit {
        return Err(ArtifactError::new(
            ArtifactErrorCode::MixedUnits,
            format!("a value in {found} where the result uses {unit}"),
        ));
    }
    match field(scalar, "value", "exact scalar")? {
        Value::String(value) => Ok(Value::String(value.clone())),
        other => invalid_result(format!("value {other} is not a string")),
    }
}

// --------------------------------------------------------------------------------- helpers

/// Every list entry the artifact and its plan read is an object, checked before either reads
/// one, so a malformed result is refused by name in every engine instead of failing wherever
/// each language first touches it. O(C + P + U + A + K) over the lists it walks.
fn require_objects(result: &Value) -> ArtifactResult<()> {
    for name in [
        "containers",
        "unpacked_items",
        "alternatives",
        "catalog_versions_used",
    ] {
        if list(result.get(name), name)?
            .iter()
            .any(|entry| !entry.is_object())
        {
            return invalid_result(format!("result.{name} holds a non-object entry"));
        }
    }
    for (index, container) in list(result.get("containers"), "containers")?
        .iter()
        .enumerate()
    {
        if placements_of(container)?
            .iter()
            .any(|placement| !placement.is_object())
        {
            return invalid_result(format!(
                "containers[{index}].placements holds a non-object entry"
            ));
        }
    }
    Ok(())
}

/// An integer as the reference means it: a JSON number without a fraction or exponent that
/// the reader kept as one. Booleans are not integers here.
fn is_integer(value: &Value) -> bool {
    value
        .as_number()
        .is_some_and(|number| number.is_i64() || number.is_u64())
}

fn reference(index: usize, placement: &Value) -> ArtifactResult<Value> {
    execution::placement_reference(index, placement)
        .map_err(|error| ArtifactError::new(ArtifactErrorCode::InvalidPlanInput, error.0))
}

fn placements_of(container: &Value) -> ArtifactResult<&[Value]> {
    list(container.get("placements"), "containers[].placements")
}

/// A list field, where an absent or null field is an empty list.
fn list<'a>(value: Option<&'a Value>, context: &str) -> ArtifactResult<&'a [Value]> {
    match value {
        None | Some(Value::Null) => Ok(&[]),
        Some(Value::Array(elements)) => Ok(elements),
        Some(_) => invalid_result(format!("{context} is not a list")),
    }
}

fn field<'a>(mapping: &'a Value, name: &str, context: &str) -> ArtifactResult<&'a Value> {
    match mapping.as_object().and_then(|object| object.get(name)) {
        Some(value) => Ok(value),
        None => invalid_result(format!("{context} has no {name}")),
    }
}

#[cfg(test)]
pub(crate) mod fixtures {
    use serde_json::{Value, json};

    use super::{ArtifactError, build_artifact_json};

    const TICKS_PER_MM: i64 = 16000;

    fn scalar(ticks: i64, unit: &str) -> Value {
        let per_unit = if unit == "mm" { TICKS_PER_MM } else { 8000 };
        json!({"ticks": ticks, "unit": unit, "value": (ticks / per_unit).to_string()})
    }

    pub(crate) fn placement(item_type: &str, x_mm: i64, length_mm: i64) -> Value {
        let mm = |millimetres: i64| scalar(millimetres * TICKS_PER_MM, "mm");
        json!({
            "item_id": format!("{item_type}#{x_mm}"),
            "item_type": item_type,
            "orientation": "LWH",
            "position": {"x": mm(x_mm), "y": mm(0), "z": mm(0)},
            "dimensions": {"length": mm(length_mm), "width": mm(10), "height": mm(10)},
            "support_ratio": "1.000000",
            "top_load": scalar(0, "g"),
        })
    }

    pub(crate) fn result_with(placements: Vec<Value>) -> Value {
        let mm = |millimetres: i64| scalar(millimetres * TICKS_PER_MM, "mm");
        json!({
            "status": "feasible",
            "objective": "default",
            "score": [1, 0, 250],
            "feasibility": {"code": "all_items_packed"},
            "optimality": null,
            "containers": [{
                "id": "crate#1",
                "container_type": "crate",
                "inner_dimensions": {"length": mm(100), "width": mm(100), "height": mm(100)},
                "payload_weight": scalar(8000, "g"),
                "gross_weight": scalar(16000, "g"),
                "volume_utilization": "0.002000",
                "placements": placements,
            }],
            "unpacked_items": [],
            "catalog_versions_used": [
                {"catalog_id": "cartons", "version": 3, "effective_at": 10, "resolved_at": 11}
            ],
            "algorithm": {
                "profile": "balanced", "solver": "extreme_point", "seed": 7, "duration_ms": 41,
                "time_limit_reached": false, "effort_limit_reached": false,
            },
        })
    }

    pub(crate) fn result() -> Value {
        result_with(vec![placement("box", 0, 10), placement("tin", 10, 5)])
    }

    pub(crate) fn request() -> Value {
        json!({
            "items": [{"id": "box", "dimensions": {"length": 10, "width": 10, "height": 10},
                       "minimum_support_ratio": 0.25}],
            "containers": [{"id": "crate",
                            "inner_dimensions": {"length": 100, "width": 100, "height": 100}}],
        })
    }

    pub(crate) fn artifact_text(result: &Value, orders: &str) -> Result<String, ArtifactError> {
        build_artifact_json(&request().to_string(), &result.to_string(), orders)
    }

    pub(crate) fn artifact(result: &Value, orders: &str) -> Value {
        serde_json::from_str(&artifact_text(result, orders).unwrap()).unwrap()
    }
}

#[cfg(test)]
mod tests {
    use super::fixtures::*;
    use super::*;

    fn code(result: &Value, orders: &str) -> &'static str {
        artifact_text(result, orders).unwrap_err().code()
    }

    // --------------------------------------------------------------------- the document

    #[test]
    fn the_plan_inside_is_exactly_the_plan_builder_output() {
        let artifact = artifact(&result(), r#"{"0":[1,0]}"#);
        assert_eq!(artifact["format"], FORMAT);
        assert_eq!(artifact["suite_version"], SUITE_VERSION);
        let plan = execution::build_plan_json(&result().to_string(), r#"{"0":[1,0]}"#).unwrap();
        assert_eq!(
            canonical_json::to_canonical_string(&artifact["plan"]).unwrap(),
            plan
        );
    }

    #[test]
    fn the_suite_version_is_the_crate_version() {
        assert_eq!(SUITE_VERSION, env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn the_request_is_embedded_as_given_with_its_fractional_numbers() {
        let text = artifact_text(&result(), "{}").unwrap();
        let artifact: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(artifact["provenance"]["request"], request());
        assert!(text.contains(r#""minimum_support_ratio":0.25"#), "{text}");
    }

    #[test]
    fn geometry_is_in_result_order_in_tick_strings_addressed_by_placement_reference() {
        let artifact = artifact(&result(), "{}");
        let container = &artifact["geometry"]["containers"][0];
        assert_eq!(
            container["inner_dimensions"],
            json!({"length": "1600000", "width": "1600000", "height": "1600000"})
        );
        let types: Vec<&Value> = container["placements"]
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| &entry["placement"]["item_type"])
            .collect();
        assert_eq!(types, [&json!("box"), &json!("tin")]);
        assert_eq!(container["placements"][1]["dimensions"]["length"], "80000");
        assert!(!container.to_string().contains("item_id"));
    }

    #[test]
    fn work_order_lines_follow_the_plan_steps_and_copy_rendered_values() {
        let artifact = artifact(&result(), r#"{"0":[1,0]}"#);
        let work_order = &artifact["work_order"];
        let container = &work_order["containers"][0];
        assert_eq!(
            (&work_order["length_unit"], &work_order["weight_unit"]),
            (&json!("mm"), &json!("g"))
        );
        assert_eq!(
            (&container["payload_weight"], &container["gross_weight"]),
            (&json!("1"), &json!("2"))
        );
        let lines = container["lines"].as_array().unwrap();
        let ordered: Vec<(Value, Value)> = lines
            .iter()
            .map(|line| {
                (
                    line["sequence"].clone(),
                    line["placement"]["item_type"].clone(),
                )
            })
            .collect();
        assert_eq!(
            ordered,
            [(json!(1), json!("tin")), (json!(2), json!("box"))]
        );
        assert_eq!(lines[0]["position"], json!({"x": "10", "y": "0", "z": "0"}));
        let steps = artifact["plan"]["containers"][0]["steps"]
            .as_array()
            .unwrap();
        for (step, line) in steps.iter().zip(lines) {
            assert_eq!(step["placement"], line["placement"]);
        }
    }

    #[test]
    fn without_a_loading_order_no_line_is_numbered() {
        let artifact = artifact(&result(), "{}");
        let lines = artifact["work_order"]["containers"][0]["lines"]
            .as_array()
            .unwrap();
        assert!(lines.iter().all(|line| line.get("sequence").is_none()));
    }

    #[test]
    fn a_result_without_containers_has_no_units_and_no_lines() {
        let mut result = result();
        result["containers"] = json!([]);
        assert_eq!(
            artifact(&result, "{}")["work_order"],
            json!({"length_unit": null, "weight_unit": null, "containers": []})
        );
    }

    // -------------------------------------------------------------------- provenance

    #[test]
    fn the_solver_is_the_deterministic_part_of_the_algorithm_and_wall_clock_never_enters() {
        let text = artifact_text(&result(), "{}").unwrap();
        let artifact: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(
            artifact["provenance"]["solver"],
            json!({"profile": "balanced", "solver": "extreme_point", "seed": 7,
                   "time_limit_reached": false, "effort_limit_reached": false})
        );
        assert_eq!(
            artifact["provenance"]["replay"],
            json!({"level": "exact", "because": null})
        );
        assert_eq!(
            artifact["provenance"]["catalog_versions_used"][0]["catalog_id"],
            "cartons"
        );
        assert!(!text.contains("duration_ms"));
    }

    #[test]
    fn a_search_stopped_by_the_clock_is_not_promised_an_exact_replay() {
        let mut result = result();
        result["algorithm"]["time_limit_reached"] = json!(true);
        assert_eq!(
            artifact(&result, "{}")["provenance"]["replay"],
            json!({"level": "not_guaranteed", "because": "provenance.solver.time_limit_reached"})
        );
    }

    #[test]
    fn a_result_that_does_not_say_how_it_was_solved_is_not_promised_one_either() {
        for algorithm in [None, Some(Value::Null)] {
            let mut result = result();
            match algorithm {
                None => {
                    result.as_object_mut().unwrap().remove("algorithm");
                }
                Some(value) => result["algorithm"] = value,
            }
            let provenance = &artifact(&result, "{}")["provenance"];
            assert!(provenance["solver"].is_null());
            assert_eq!(
                provenance["replay"],
                json!({"level": "not_guaranteed", "because": "provenance.solver"})
            );
        }
    }

    // ---------------------------------------------------------------------- refusals

    #[test]
    fn a_request_that_is_not_an_object_is_refused() {
        let error = build_artifact_json("[]", &result().to_string(), "{}").unwrap_err();
        assert_eq!(error.code(), "invalid_request");
    }

    #[test]
    fn text_that_does_not_parse_is_refused_as_invalid_json() {
        for (request, result) in [("{", "{}"), ("{}", "{\"status\":")] {
            let error = build_artifact_json(request, result, "{}").unwrap_err();
            assert_eq!(error.code(), "invalid_json");
        }
    }

    #[test]
    fn a_loading_order_that_is_not_a_permutation_is_refused_by_the_plan() {
        assert_eq!(code(&result(), r#"{"0":[0,0]}"#), "invalid_plan_input");
        assert_eq!(code(&result(), r#"{"0":[0,"1"]}"#), "invalid_plan_input");
        assert_eq!(code(&result(), "[]"), "invalid_plan_input");
    }

    #[test]
    fn values_in_two_units_are_refused_rather_than_printed_as_one() {
        let mut result = result();
        result["containers"][0]["placements"][0]["position"]["x"]["unit"] = json!("cm");
        assert_eq!(code(&result, "{}"), "mixed_units");
    }

    #[test]
    fn two_placements_with_one_reference_are_refused() {
        let result = result_with(vec![placement("box", 0, 10), placement("box", 0, 10)]);
        assert_eq!(code(&result, "{}"), "invalid_result");
    }

    #[test]
    fn a_container_without_inner_dimensions_is_refused() {
        let mut result = result();
        result["containers"][0]
            .as_object_mut()
            .unwrap()
            .remove("inner_dimensions");
        assert_eq!(code(&result, "{}"), "invalid_result");
    }

    #[test]
    fn an_algorithm_record_that_is_incomplete_or_mistyped_is_refused() {
        let mut mistyped = result()["algorithm"].clone();
        mistyped["time_limit_reached"] = json!("no");
        for algorithm in [json!({"profile": "fast"}), mistyped, json!("fast")] {
            let mut result = result();
            result["algorithm"] = algorithm;
            assert_eq!(code(&result, "{}"), "invalid_result");
        }
    }

    #[test]
    fn ticks_that_are_not_an_integer_are_refused() {
        for ticks in [json!(160000.0), json!(true), json!("160000")] {
            let mut result = result();
            result["containers"][0]["placements"][0]["dimensions"]["width"]["ticks"] =
                ticks.clone();
            assert_eq!(code(&result, "{}"), "invalid_result", "{ticks}");
        }
    }

    #[test]
    fn a_result_that_is_not_an_object_is_refused() {
        for result in ["[]", "\"feasible\"", "null"] {
            let error = build_artifact_json(&request().to_string(), result, "{}").unwrap_err();
            assert_eq!(error.code(), "invalid_result", "{result}");
        }
    }

    #[test]
    fn a_rendered_value_that_is_not_a_string_is_refused() {
        for value in [json!(10), Value::Null, json!(10.5)] {
            let mut result = result();
            result["containers"][0]["placements"][0]["position"]["x"]["value"] = value.clone();
            assert_eq!(code(&result, "{}"), "invalid_result", "{value}");
        }
    }

    #[test]
    fn the_display_units_are_read_from_the_first_container_or_refused() {
        let mut no_length_unit = result();
        no_length_unit["containers"][0]["inner_dimensions"]["length"]
            .as_object_mut()
            .unwrap()
            .remove("unit");
        assert_eq!(code(&no_length_unit, "{}"), "invalid_result");
        let mut no_weight_unit = result();
        no_weight_unit["containers"][0]["payload_weight"]
            .as_object_mut()
            .unwrap()
            .remove("unit");
        assert_eq!(code(&no_weight_unit, "{}"), "invalid_result");
    }

    #[test]
    fn every_container_carries_its_payload_and_gross_weight() {
        // The units come from container 0, so a later container's missing weight is only
        // noticed when its own work order is written.
        let mut later = result();
        let mut empty = later["containers"][0].clone();
        empty["placements"] = json!([]);
        empty.as_object_mut().unwrap().remove("payload_weight");
        later["containers"].as_array_mut().unwrap().push(empty);
        assert_eq!(code(&later, "{}"), "invalid_result");

        let mut gross = result();
        gross["containers"][0]
            .as_object_mut()
            .unwrap()
            .remove("gross_weight");
        assert_eq!(code(&gross, "{}"), "invalid_result");

        // A weight that is present is copied only as the result rendered it, in one unit.
        let mut unrendered_payload = result();
        unrendered_payload["containers"][0]["payload_weight"]["value"] = json!(1);
        assert_eq!(code(&unrendered_payload, "{}"), "invalid_result");
        let mut gross_in_kilograms = result();
        gross_in_kilograms["containers"][0]["gross_weight"]["unit"] = json!("kg");
        assert_eq!(code(&gross_in_kilograms, "{}"), "mixed_units");
    }

    #[test]
    fn an_error_carries_its_closed_code_its_kind_and_a_message() {
        let request = r#"{"items":[],"containers":[],"metadata":{"bad":"\ud800"}}"#;
        let error = build_artifact_json(request, &result().to_string(), "{}").unwrap_err();
        assert_eq!(error.kind(), ArtifactErrorCode::InvalidString);
        assert_eq!(error.code(), "invalid_string");
        assert!(!error.message().is_empty());
        assert_eq!(
            error.to_string(),
            format!("invalid_string: {}", error.message())
        );
    }

    #[test]
    fn a_list_entry_that_is_not_an_object_is_refused_by_name_before_anything_reads_it() {
        for name in [
            "containers",
            "unpacked_items",
            "alternatives",
            "catalog_versions_used",
        ] {
            let mut result = result();
            result[name] = json!(["not an object"]);
            assert_eq!(code(&result, "{}"), "invalid_result", "{name}");
        }
        let mut placement = result();
        placement["containers"][0]["placements"] = json!([7]);
        assert_eq!(code(&placement, "{}"), "invalid_result");
        for (name, value) in [
            ("containers", json!({})),
            ("unpacked_items", json!({})),
            ("catalog_versions_used", json!("cartons")),
            ("alternatives", json!(3)),
        ] {
            let mut not_a_list = result();
            not_a_list[name] = value;
            assert_eq!(code(&not_a_list, "{}"), "invalid_result", "{name}");
        }
    }

    #[test]
    fn geometry_ticks_javascript_cannot_hold_are_refused() {
        // Geometry writes ticks as strings, so the canonical writer never sees them as numbers.
        for ticks in [
            "9007199254740992",
            "-9007199254740992",
            "18446744073709551616",
        ] {
            let text = result().to_string().replacen(
                "\"ticks\":160000,",
                &format!("\"ticks\":{ticks},"),
                1,
            );
            let error = build_artifact_json(&request().to_string(), &text, "{}").unwrap_err();
            assert_eq!(error.code(), "number_out_of_range", "{ticks}");
        }
    }

    #[test]
    fn a_catalog_reference_of_the_wrong_type_is_refused() {
        for (name, value) in [
            ("version", json!(1.5)),
            ("effective_at", json!(true)),
            ("resolved_at", Value::Null),
            ("catalog_id", json!(3)),
        ] {
            let mut result = result();
            result["catalog_versions_used"][0][name] = value;
            assert_eq!(code(&result, "{}"), "invalid_result", "{name}");
        }
        let mut missing = result();
        missing["catalog_versions_used"][0]
            .as_object_mut()
            .unwrap()
            .remove("resolved_at");
        assert_eq!(code(&missing, "{}"), "invalid_result");
    }

    #[test]
    fn a_request_number_javascript_cannot_hold_is_refused() {
        let request = r#"{"items":[],"containers":[],"metadata":{"order":9007199254740992}}"#;
        let error = build_artifact_json(request, &result().to_string(), "{}").unwrap_err();
        assert_eq!(error.code(), "number_out_of_range");
    }

    #[test]
    fn a_number_outside_the_artifact_does_not_refuse_it() {
        let text = result()
            .to_string()
            .replacen("\"duration_ms\":41", "\"duration_ms\":1e400", 1);
        assert!(build_artifact_json(&request().to_string(), &text, "{}").is_ok());
    }

    #[test]
    fn the_builder_imports_no_solver_validator_renderer_or_clock() {
        let allowed = ["artifacts", "canonical_json", "execution", "value_text"];
        // The plan it wraps is held to the same rule.
        let sources = [
            ("execution.rs", include_str!("execution.rs")),
            ("artifacts.rs", include_str!("artifacts.rs")),
            ("artifact_exports.rs", include_str!("artifact_exports.rs")),
            ("canonical_json.rs", include_str!("canonical_json.rs")),
            ("value_text.rs", include_str!("value_text.rs")),
        ];
        for (name, source) in sources {
            for line in source.lines().map(str::trim) {
                if let Some(path) = line.strip_prefix("use crate::") {
                    // `use crate::{a, b};` names several modules; `use crate::a::{B, C};` one.
                    let modules: Vec<&str> = match path.strip_prefix('{') {
                        Some(group) => group.trim_end_matches([';', '}']).split(',').collect(),
                        None => vec![path],
                    };
                    for module in modules {
                        let root = module.trim().trim_end_matches(';').split("::").next();
                        let root = root.unwrap_or("");
                        assert!(allowed.contains(&root), "{name} imports crate::{root}");
                    }
                } else if let Some(path) = line.strip_prefix("use ") {
                    let root = path.split("::").next().unwrap_or("");
                    assert!(
                        ["std", "serde_json", "super"].contains(&root),
                        "{name} imports {root}"
                    );
                }
            }
        }
    }
}
