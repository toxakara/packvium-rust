//! The request schema's rules, checked over the raw JSON before any model is built.
//!
//! A caller who sends a malformed request needs to know which value is wrong and why, in a
//! form a program can branch on, and the four engines must say it identically. So the first
//! violation is refused as a [`RequestError`] naming a reason from a closed set and the JSON
//! Pointer of the value, in the order every engine walks: units, configuration, items,
//! containers. The Python reference (`packvium/request_errors.py`) is the source of truth for
//! every rule, pointer and text here; a cross-engine suite holds this engine to it.

use crate::canonical_json::{MAX_EXACT_MAGNITUDE, json_integer, json_spelling};
use crate::error::RequestError;
use crate::fixed::unknown_keys;
use crate::units::{MeasureKind, measure_ticks};
use serde_json::{Map, Value};
use std::collections::BTreeSet;

type Checked<T = ()> = Result<T, RequestError>;

const SOLVER_PROFILES: [&str; 4] = ["fast", "balanced", "quality", "exact_small"];
const OBJECTIVES: [&str; 6] = [
    "default",
    "lowest_cost",
    "shipping_cost",
    "lowest_landed_cost",
    "open_dimension_height",
    "maximum_value",
];
const ACCESS_DIRECTIONS: [&str; 6] = ["+x", "-x", "+y", "-y", "+z", "-z"];
const CONFIGURATION_INTEGERS: [(&str, i64); 10] = [
    ("time_limit_ms", 1),
    ("alternatives", 1),
    ("max_containers", 1),
    ("exact_item_limit", 1),
    ("multi_start_orders", 1),
    ("max_candidates_per_item", 1),
    ("max_candidate_points", 16),
    ("container_plan_beam_width", 1),
    ("container_plan_node_limit", 1),
    ("dimensional_weight_divisor", 1),
];
const EFFORT_LIMITS: [&str; 4] = [
    "max_candidates_evaluated",
    "max_placement_attempts",
    "max_search_nodes",
    "max_restarts",
];
/// Every key the request schema's `configuration` declares; it sets `additionalProperties: false`.
const CONFIGURATION_FIELDS: [&str; 20] = [
    "alternatives",
    "clearance",
    "container_plan_beam_width",
    "container_plan_node_limit",
    "dimensional_weight_divisor",
    "dimensional_weight_length_unit",
    "dimensional_weight_weight_unit",
    "effort_budget",
    "exact_item_limit",
    "max_candidate_points",
    "max_candidates_per_item",
    "max_containers",
    "minimum_support_ratio",
    "multi_start_orders",
    "objective",
    "require_placement_coordinates",
    "seed",
    "solver_profile",
    "solvers",
    "time_limit_ms",
];
const SIDES: [&str; 3] = ["length", "width", "height"];
const AXES: [&str; 3] = ["x", "y", "z"];

/// Refuse the request with its first violation, or return the request's length unit.
///
/// O(n log n) in the size of the request: one pass over every value, plus the id sets.
pub(crate) fn check_request(data: &Value) -> Checked<&str> {
    let request = object(data, "")?;
    let unit = check_units(request.get("units"))?;
    check_configuration(request.get("configuration"), unit)?;
    let items = required_list(request, "items", "")?;
    for (index, raw) in items.iter().enumerate() {
        check_item(raw, &join("/items", &index.to_string()), unit)?;
    }
    require_unique_ids(items, "items")?;
    let containers = required_list(request, "containers", "")?;
    for (index, raw) in containers.iter().enumerate() {
        check_container(raw, &join("/containers", &index.to_string()), unit)?;
    }
    require_unique_ids(containers, "containers")?;
    Ok(unit)
}

/// `base` extended by one RFC 6901 reference token, escaping `~` and `/` in it.
pub(crate) fn join(base: &str, part: &str) -> String {
    format!("{base}/{}", part.replace('~', "~0").replace('/', "~1"))
}

// ------------------------------------------------------------------------------ the rules

fn check_units(raw: Option<&Value>) -> Checked<&str> {
    let Some(raw) = present(raw) else {
        return Ok("mm");
    };
    let Some(length) = present(object(raw, "/units")?.get("length")) else {
        return Ok("mm");
    };
    match length.as_str() {
        Some(unit) if MeasureKind::Length.knows_unit(unit) => Ok(unit),
        _ => Err(unknown_unit("/units/length", length)),
    }
}

fn check_configuration(raw: Option<&Value>, unit: &str) -> Checked {
    let Some(raw) = present(raw) else {
        return Ok(());
    };
    let configuration = object(raw, "/configuration")?;
    let at = "/configuration";
    known_fields(configuration, at, &CONFIGURATION_FIELDS)?;
    optional(configuration, "solver_profile", at, |value, field| {
        one_of(value, field, &SOLVER_PROFILES)
    })?;
    optional(configuration, "objective", at, |value, field| {
        one_of(value, field, &OBJECTIVES)
    })?;
    for (name, minimum) in CONFIGURATION_INTEGERS {
        optional(configuration, name, at, |value, field| {
            integer(value, field, minimum)
        })?;
    }
    optional(
        configuration,
        "minimum_support_ratio",
        at,
        |value, field| ratio(value, field, Some(1)),
    )?;
    optional(configuration, "clearance", at, |value, field| {
        measure(value, field, MeasureKind::Length, unit)
    })?;
    let Some(effort) = present(configuration.get("effort_budget")) else {
        return Ok(());
    };
    let at = "/configuration/effort_budget";
    let budget = object(effort, at)?;
    known_fields(budget, at, &EFFORT_LIMITS)?;
    for name in EFFORT_LIMITS {
        optional(budget, name, at, |value, field| integer(value, field, 1))?;
    }
    Ok(())
}

fn check_item(raw: &Value, at: &str, unit: &str) -> Checked {
    let item = object(raw, at)?;
    required_string(item, "id", at)?;
    optional(item, "quantity", at, |value, field| {
        integer(value, field, 1)
    })?;
    dimensions(
        required(item, "dimensions", at)?,
        &join(at, "dimensions"),
        unit,
    )?;
    for name in ["weight", "max_top_load"] {
        optional(item, name, at, |value, field| {
            measure(value, field, MeasureKind::Weight, "g")
        })?;
    }
    optional(item, "nesting_height", at, |value, field| {
        measure(value, field, MeasureKind::Length, unit)
    })?;
    for (name, minimum) in [
        ("max_stacked_items", 1),
        ("stop_index", 0),
        ("value", 0),
        ("max_compression_pressure_kpa", 0),
    ] {
        optional(item, name, at, |value, field| {
            integer(value, field, minimum)
        })?;
    }
    optional(item, "minimum_support_ratio", at, |value, field| {
        ratio(value, field, Some(1))
    })?;
    optional(item, "compression_ratio", at, |value, field| {
        ratio(value, field, None)
    })
}

fn check_container(raw: &Value, at: &str, unit: &str) -> Checked {
    let container = object(raw, at)?;
    required_string(container, "id", at)?;
    optional(container, "quantity", at, |value, field| {
        integer(value, field, 1)
    })?;
    let inner = required(container, "inner_dimensions", at)?;
    dimensions(inner, &join(at, "inner_dimensions"), unit)?;
    optional(container, "outer_dimensions", at, |value, field| {
        dimensions(value, field, unit)
    })?;
    for name in ["tare_weight", "max_payload", "max_stack_density"] {
        optional(container, name, at, |value, field| {
            measure(value, field, MeasureKind::Weight, "g")
        })?;
    }
    optional(container, "max_items", at, |value, field| {
        integer(value, field, 1)
    })?;
    optional(container, "cost_minor", at, |value, field| {
        integer(value, field, 0)
    })?;
    optional(container, "void_fill_reserve_ratio", at, |value, field| {
        ratio(value, field, Some(1))
    })?;
    optional(container, "access_directions", at, access_directions)?;
    optional(container, "tag_limits", at, tag_limits)?;
    optional(container, "rate_table", at, rate_table)?;
    optional(container, "obstacles", at, |value, field| {
        obstacles(value, field, unit)
    })
}

fn dimensions(raw: &Value, at: &str, unit: &str) -> Checked {
    let sides = object(raw, at)?;
    for side in SIDES {
        measure(
            required(sides, side, at)?,
            &join(at, side),
            MeasureKind::Length,
            unit,
        )?;
    }
    Ok(())
}

/// Tags in code-point order, the one order all four engines can share: JavaScript lists
/// integer-like keys first, so insertion order is not it. `serde_json::Map` is a `BTreeMap`
/// (the `preserve_order` feature is off), and UTF-8 byte order is code-point order.
fn tag_limits(raw: &Value, at: &str) -> Checked {
    for (tag, limit) in object(raw, at)? {
        integer(limit, &join(at, tag), 1)?;
    }
    Ok(())
}

fn rate_table(raw: &Value, at: &str) -> Checked {
    let table = object(raw, at)?;
    for (name, minimum) in [("weight_brackets_g", 1), ("prices_minor", 0)] {
        let Some(values) = present(table.get(name)) else {
            continue;
        };
        let listed = join(at, name);
        for (index, value) in list(values, &listed)?.iter().enumerate() {
            integer(value, &join(&listed, &index.to_string()), minimum)?;
        }
    }
    for name in ["minimum_charge_minor", "fuel_surcharge_permille"] {
        optional(table, name, at, |value, field| integer(value, field, 0))?;
    }
    Ok(())
}

fn obstacles(raw: &Value, at: &str, unit: &str) -> Checked {
    for (index, entry) in list(raw, at)?.iter().enumerate() {
        let here = join(at, &index.to_string());
        let obstacle = object(entry, &here)?;
        if let Some(origin) = present(obstacle.get("origin")) {
            let origin_at = join(&here, "origin");
            let point = object(origin, &origin_at)?;
            for axis in AXES {
                optional(point, axis, &origin_at, |value, field| {
                    measure(value, field, MeasureKind::Length, unit)
                })?;
            }
        }
        let sides = required(obstacle, "dimensions", &here)?;
        dimensions(sides, &join(&here, "dimensions"), unit)?;
    }
    Ok(())
}

/// Checked once every entry is well formed, so the later of two equal ids is the one named.
fn require_unique_ids(entries: &[Value], key: &str) -> Checked {
    let mut seen = BTreeSet::new();
    for (index, entry) in entries.iter().enumerate() {
        let id = entry["id"].as_str().unwrap_or_default();
        if !seen.insert(id) {
            return Err(RequestError::new(
                "duplicate_id",
                format!("/{key}/{index}/id"),
                format!("repeats the id {}", json_spelling(&entry["id"])),
            ));
        }
    }
    Ok(())
}

// ------------------------------------------------------------------------------ primitives

fn integer(value: &Value, field: &str, minimum: i64) -> Checked {
    match json_integer(value) {
        Some(number) if number < minimum => Err(below_minimum(field, minimum)),
        Some(_) => Ok(()),
        None => Err(non_integer(value, field, minimum)),
    }
}

/// A whole number past 2^53 - 1 is out of range, not mistyped: say which way it is out.
fn non_integer(value: &Value, field: &str, minimum: i64) -> RequestError {
    let Value::Number(number) = value else {
        return wrong_type(field, "must be an integer");
    };
    let below = if let Some(integer) = number.as_i64() {
        integer < minimum
    } else if number.is_u64() {
        false
    } else {
        let float = number.as_f64().unwrap_or(f64::NAN);
        if !float.is_finite() || float.fract() != 0.0 {
            return wrong_type(field, "must be an integer");
        }
        float < minimum as f64
    };
    if below {
        below_minimum(field, minimum)
    } else {
        RequestError::new(
            "above_maximum",
            field,
            format!("must be at most {MAX_EXACT_MAGNITUDE}"),
        )
    }
}

fn ratio(value: &Value, field: &str, maximum: Option<i64>) -> Checked {
    let Some(number) = value.as_f64() else {
        return Err(wrong_type(field, "must be a number"));
    };
    if number < 0.0 {
        return Err(below_minimum(field, 0));
    }
    match maximum {
        Some(maximum) if number > maximum as f64 => Err(RequestError::new(
            "above_maximum",
            field,
            format!("must be at most {maximum}"),
        )),
        _ => Ok(()),
    }
}

/// The schema closes this object: a key it does not name is refused, never ignored. The first
/// unknown key in code-point order is named, the order every engine can share.
fn known_fields(map: &Map<String, Value>, at: &str, known: &[&str]) -> Checked {
    match unknown_keys(map, known).first() {
        Some(key) => Err(RequestError::new(
            "not_allowed",
            join(at, key),
            "is not a known field",
        )),
        None => Ok(()),
    }
}

fn one_of(value: &Value, field: &str, allowed: &[&str]) -> Checked {
    if value.as_str().is_some_and(|item| allowed.contains(&item)) {
        return Ok(());
    }
    let allowed_values = allowed
        .iter()
        .map(|item| Value::String((*item).to_owned()))
        .collect();
    Err(RequestError::new(
        "not_allowed",
        field,
        format!(
            "must be one of {}",
            json_spelling(&Value::Array(allowed_values))
        ),
    ))
}

fn access_directions(raw: &Value, at: &str) -> Checked {
    let directions = list(raw, at)?;
    for (index, direction) in directions.iter().enumerate() {
        one_of(direction, &join(at, &index.to_string()), &ACCESS_DIRECTIONS)?;
    }
    Ok(())
}

/// A measure is an integer, a string or `{value, unit}`, in a known unit, and never negative.
fn measure(value: &Value, field: &str, kind: MeasureKind, unit: &str) -> Checked {
    match value {
        // A fraction in binary floating point is not the decimal the caller wrote.
        Value::Number(number) if number.is_f64() && json_integer(value).is_none() => {
            return Err(wrong_type(field, "must be a measure"));
        }
        Value::Number(_) | Value::String(_) => {}
        Value::Object(map) => require_measure_unit(map, field, kind, unit)?,
        _ => return Err(wrong_type(field, "must be a measure")),
    }
    match measure_ticks(value, unit, kind) {
        Ok(ticks) if ticks < 0 => Err(RequestError::new(
            "negative_measure",
            field,
            "cannot be negative",
        )),
        Ok(_) => Ok(()),
        Err(_) => Err(wrong_type(field, "must be a measure")),
    }
}

fn require_measure_unit(
    map: &Map<String, Value>,
    field: &str,
    kind: MeasureKind,
    unit: &str,
) -> Checked {
    if !map.contains_key("value") {
        return Err(wrong_type(field, "must be a measure"));
    }
    let default = Value::String(unit.to_owned());
    // Present-but-null is a stated unit, and not one anybody knows.
    let stated = map.get("unit").unwrap_or(&default);
    match stated.as_str() {
        Some(stated) if kind.knows_unit(stated) => Ok(()),
        _ => Err(unknown_unit(field, stated)),
    }
}

// ------------------------------------------------------------------------------ plumbing

/// Absent and JSON `null` are the same: the field's default.
fn present(value: Option<&Value>) -> Option<&Value> {
    value.filter(|value| !value.is_null())
}

fn optional(
    container: &Map<String, Value>,
    name: &str,
    at: &str,
    check: impl FnOnce(&Value, &str) -> Checked,
) -> Checked {
    match present(container.get(name)) {
        Some(value) => check(value, &join(at, name)),
        None => Ok(()),
    }
}

fn required<'a>(container: &'a Map<String, Value>, name: &str, at: &str) -> Checked<&'a Value> {
    present(container.get(name))
        .ok_or_else(|| RequestError::new("missing_field", join(at, name), "is required"))
}

fn required_list<'a>(
    container: &'a Map<String, Value>,
    name: &str,
    at: &str,
) -> Checked<&'a [Value]> {
    list(required(container, name, at)?, &join(at, name))
}

fn required_string(container: &Map<String, Value>, name: &str, at: &str) -> Checked {
    if required(container, name, at)?.is_string() {
        return Ok(());
    }
    Err(wrong_type(&join(at, name), "must be a string"))
}

fn object<'a>(value: &'a Value, field: &str) -> Checked<&'a Map<String, Value>> {
    value
        .as_object()
        .ok_or_else(|| wrong_type(field, "must be an object"))
}

fn list<'a>(value: &'a Value, field: &str) -> Checked<&'a [Value]> {
    value
        .as_array()
        .map(Vec::as_slice)
        .ok_or_else(|| wrong_type(field, "must be a list"))
}

fn wrong_type(field: &str, detail: &str) -> RequestError {
    RequestError::new("wrong_type", field, detail)
}

fn below_minimum(field: &str, minimum: i64) -> RequestError {
    RequestError::new(
        "below_minimum",
        field,
        format!("must be at least {minimum}"),
    )
}

fn unknown_unit(field: &str, stated: &Value) -> RequestError {
    RequestError::new(
        "invalid_unit",
        field,
        format!("has an unknown unit {}", json_spelling(stated)),
    )
}
