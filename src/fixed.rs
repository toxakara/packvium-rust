//! Admission of a request's fixed placements (docs/PLAN-REVISIONS.md).
//!
//! A fixed placement is an item already in a known place: loaded, or locked there by an
//! operator. It enters the solve as a real `Placement` -- weight, support, top load and all --
//! seeded into the one container instance it names, so every rule the engine already
//! enforces holds for it with no rule of its own.
//!
//! What this module adds is the refusal. A fixed set that is not a valid packing on its own
//! is refused before any search, with `invalid_fixed_placement`, rather than exempted: an
//! exempt item would need a second validator, and four engines would have to agree on which
//! rules it skips. The check is the ordinary `IndependentValidator` run over the fixed set
//! alone, without item accounting, because the free items have not been placed yet.

use crate::canonical_json::{json_integer, json_spelling, spell_names};
use crate::contact_graph::ContactGraph;
use crate::error::{PackError, PackResult, RequestError};
use crate::geometry::Point;
use crate::model::*;
use crate::solvers::calculate_top_loads;
use crate::units::{MeasureKind, Weight, measure_ticks};
use crate::validation::IndependentValidator;
use serde_json::{Map, Value};
use std::collections::BTreeMap;

const ORIENTATIONS: [&str; 6] = ["LWH", "LHW", "WLH", "WHL", "HLW", "HWL"];
const REQUIRED: [&str; 3] = ["item_type", "container_type", "orientation"];
const FIELDS: [&str; 5] = [
    "item_type",
    "container_type",
    "orientation",
    "container_instance",
    "position",
];
const AXES: [&str; 3] = ["x", "y", "z"];

/// The request's `fixed_placements` as JSON gave them, or a refusal naming the first entry
/// that is not the schema's shape. Nothing is coerced: `"1"` and `true` are not instances,
/// and a list is not a position. An absent or null field is no fixed placements. A position's
/// coordinates are parsed in the request's length `unit`, so a negative or unparsable one is
/// refused here, by its pointer, rather than as an anonymous parse failure later.
pub(crate) fn require_fixed_placement_shapes<'a>(
    raw: Option<&'a Value>,
    unit: &str,
) -> PackResult<&'a [Value]> {
    let Some(raw) = raw.filter(|raw| !raw.is_null()) else {
        return Ok(&[]);
    };
    let Some(entries) = raw.as_array() else {
        return Err(malformed(Fault {
            detail: "fixed_placements is a list".into(),
            field: "/fixed_placements".into(),
        }));
    };
    for (index, entry) in entries.iter().enumerate() {
        let place = format!("fixed_placements[{index}]");
        let field = format!("/fixed_placements/{index}");
        if let Some(fault) = entry_shape_fault(entry, &place, &field, unit) {
            return Err(malformed(fault));
        }
    }
    Ok(entries)
}

/// Why a value is refused: the message, and the JSON Pointer of the value at fault.
struct Fault {
    detail: String,
    field: String,
}

fn fault(detail: String, field: &str) -> Option<Fault> {
    Some(Fault {
        detail,
        field: field.to_owned(),
    })
}

fn entry_shape_fault(entry: &Value, place: &str, field: &str, unit: &str) -> Option<Fault> {
    let Some(map) = entry.as_object() else {
        return fault(format!("{place} is an object"), field);
    };
    let unknown = unknown_keys(map, &FIELDS);
    if !unknown.is_empty() {
        return fault(
            format!("{place} does not carry {}", spell_names(&unknown)),
            field,
        );
    }
    let missing = REQUIRED
        .into_iter()
        .filter(|name| !map.contains_key(*name))
        .collect::<Vec<_>>();
    if !missing.is_empty() {
        return fault(format!("{place} needs {}", spell_names(&missing)), field);
    }
    if let Some(name) = ["item_type", "container_type"]
        .into_iter()
        .find(|name| map[*name].as_str().is_none_or(str::is_empty))
    {
        return fault(
            format!("{place}.{name} is a non-empty string"),
            &format!("{field}/{name}"),
        );
    }
    if !map["orientation"]
        .as_str()
        .is_some_and(|orientation| ORIENTATIONS.contains(&orientation))
    {
        return fault(
            format!("{place}.orientation is one of the six codes"),
            &format!("{field}/orientation"),
        );
    }
    if map
        .get("container_instance")
        .is_some_and(|instance| json_integer(instance).is_none_or(|instance| instance < 1))
    {
        return fault(
            format!("{place}.container_instance counts from 1"),
            &format!("{field}/container_instance"),
        );
    }
    let position = map.get("position")?;
    let place = format!("{place}.position");
    let field = format!("{field}/position");
    point_shape_fault(position, &place, &field)
        .or_else(|| coordinate_fault(position, &place, &field, unit))
}

/// Why `point` is not an object of `x`, `y` and `z` measures, if it is not. Units, and what
/// makes a measure's text valid, are the length parser's.
pub(crate) fn point_shape_problem(point: &Value, place: &str) -> Option<String> {
    point_shape_fault(point, place, "").map(|fault| fault.detail)
}

fn point_shape_fault(point: &Value, place: &str, field: &str) -> Option<Fault> {
    let Some(map) = point.as_object() else {
        return fault(format!("{place} is a point object"), field);
    };
    let unknown = unknown_keys(map, &AXES);
    if !unknown.is_empty() {
        return fault(
            format!("{place} does not carry {}", spell_names(&unknown)),
            field,
        );
    }
    AXES.into_iter()
        .find(|axis| {
            map.get(*axis).is_some_and(|value| {
                matches!(value, Value::Null | Value::Bool(_) | Value::Array(_))
            })
        })
        .and_then(|axis| {
            fault(
                format!("{place}.{axis} is a measure"),
                &format!("{field}/{axis}"),
            )
        })
}

/// The first present coordinate, in axis order, that is negative or not a length at all.
/// Only called on a point whose shape is already admitted.
fn coordinate_fault(point: &Value, place: &str, field: &str, unit: &str) -> Option<Fault> {
    AXES.into_iter().find_map(|axis| {
        let value = point.get(axis)?;
        let problem = match coordinate_ticks(value, unit) {
            Some(ticks) if ticks < 0 => "cannot be negative",
            Some(_) => return None,
            None => "is a measure",
        };
        fault(
            format!("{place}.{axis} {problem}"),
            &format!("{field}/{axis}"),
        )
    })
}

/// A coordinate's ticks, or `None` when it is not a length: a fraction in binary floating
/// point is not the decimal the caller wrote, and a unit must be stated as text.
fn coordinate_ticks(value: &Value, unit: &str) -> Option<i128> {
    let refused = match value {
        Value::Number(number) => number.as_f64().is_some_and(|float| float.fract() != 0.0),
        Value::Object(map) => map.get("unit").is_some_and(|unit| !unit.is_string()),
        _ => false,
    };
    if refused {
        return None;
    }
    measure_ticks(value, unit, MeasureKind::Length).ok()
}

fn malformed(fault: Fault) -> PackError {
    RequestError::fixed_placement("malformed", fault.field, fault.detail).into()
}

/// The keys of `map` outside `allowed`, sorted by code point as every engine sorts them.
pub(crate) fn unknown_keys<'a>(map: &'a Map<String, Value>, allowed: &[&str]) -> Vec<&'a str> {
    let mut unknown = map
        .keys()
        .map(String::as_str)
        .filter(|key| !allowed.contains(key))
        .collect::<Vec<_>>();
    // `str` orders by UTF-8 bytes, which is code-point order; the explicit sort keeps that
    // true even if a feature ever switches `Map` to insertion order.
    unknown.sort_unstable();
    unknown
}

/// Resolve `request.fixed_placements` into the containers they name, in opening order
/// (request container order, then instance), each holding only its fixed items.
///
/// O(f log f) to resolve f entries, plus one validator pass over them, which is O(f^2) in
/// the worst case of its contact graph. It runs once, before search.
pub(crate) fn admit(request: &PackingRequest) -> PackResult<Vec<PackedContainer>> {
    if request.fixed_placements.is_empty() {
        return Ok(Vec::new());
    }
    for entry in &request.fixed_placements {
        require_known(request, entry)?;
    }
    let instances = assign_instances(request)?;
    let grouped = group_by_container(request, &instances)?;
    require_contiguous_instances(request, &grouped)?;
    let packed = grouped
        .into_iter()
        .map(|((position, sequence), placements)| PackedContainer {
            container: request.containers[position].clone(),
            sequence,
            placements: with_support_and_loads(placements),
            lattice_summary: None,
            lattice_items: Vec::new(),
        })
        .collect::<Vec<_>>();
    let report = IndependentValidator.validate_containers(request, &packed);
    if let Some(issue) = report.issues.first() {
        return Err(refusal(format!("{}: {}", issue.code, issue.message)));
    }
    Ok(packed)
}

/// A fixed set that is well formed but cannot hold as a packing on its own.
fn refusal(detail: String) -> PackError {
    RequestError::fixed_placement("cannot_hold", "/fixed_placements", detail).into()
}

fn require_known(request: &PackingRequest, entry: &FixedPlacement) -> PackResult<()> {
    let Some(item) = request.items.iter().find(|item| item.id == entry.item_id) else {
        return Err(refusal(format!(
            "unknown item type {}",
            json_spelling(&Value::String(entry.item_id.clone()))
        )));
    };
    if !request
        .containers
        .iter()
        .any(|container| container.id == entry.container_id)
    {
        return Err(refusal(format!(
            "unknown container type {}",
            json_spelling(&Value::String(entry.container_id.clone()))
        )));
    }
    if !item.allowed_rotations.contains(&entry.rotation) {
        return Err(refusal(format!(
            "{} may not be placed in orientation {}",
            entry.item_id,
            entry.rotation.as_str()
        )));
    }
    Ok(())
}

/// Fixed items take the first instances of their type, in the order they are listed.
fn assign_instances(request: &PackingRequest) -> PackResult<Vec<ItemInstance>> {
    let mut taken = BTreeMap::<&str, usize>::new();
    request
        .fixed_placements
        .iter()
        .map(|entry| {
            let item = request
                .items
                .iter()
                .find(|item| item.id == entry.item_id)
                .expect("admission checked every item type first");
            let count = taken.entry(item.id.as_str()).or_default();
            *count += 1;
            if *count > item.quantity {
                return Err(refusal(format!(
                    "{} {} fixed, {} requested",
                    count, item.id, item.quantity
                )));
            }
            Ok(ItemInstance {
                item: item.clone(),
                sequence: *count,
            })
        })
        .collect()
}

/// Placements grouped by (request container position, instance), which is opening order.
fn group_by_container(
    request: &PackingRequest,
    instances: &[ItemInstance],
) -> PackResult<BTreeMap<(usize, usize), Vec<Placement>>> {
    let clearance = request.config.clearance;
    let mut grouped = BTreeMap::<(usize, usize), Vec<Placement>>::new();
    for (entry, instance) in request.fixed_placements.iter().zip(instances) {
        let position = request
            .containers
            .iter()
            .position(|container| container.id == entry.container_id)
            .expect("admission checked every container type first");
        let origin = entry.position;
        // The clearance envelope must be inside the container, as it must for any
        // placement; an item flush against a wall has an envelope that starts before it.
        if origin.x.min(origin.y).min(origin.z) < clearance.0 {
            return Err(refusal(format!("outside_container: {}", instance.id())));
        }
        let dimensions = instance.item.dimensions.rotated(entry.rotation);
        grouped
            .entry((position, entry.container_instance))
            .or_default()
            .push(Placement {
                instance: instance.clone(),
                position: origin,
                rotation: entry.rotation,
                dimensions,
                envelope_origin: Point {
                    x: origin.x - clearance.0,
                    y: origin.y - clearance.0,
                    z: origin.z - clearance.0,
                },
                envelope_dimensions: dimensions.expand(clearance),
                support_ratio: 1.0,
                top_load: Weight(0),
                fixed: true,
            });
    }
    Ok(grouped)
}

fn require_contiguous_instances(
    request: &PackingRequest,
    grouped: &BTreeMap<(usize, usize), Vec<Placement>>,
) -> PackResult<()> {
    let mut named = BTreeMap::<usize, Vec<usize>>::new();
    for (position, sequence) in grouped.keys() {
        named.entry(*position).or_default().push(*sequence);
    }
    for (position, sequences) in named {
        let container = &request.containers[position];
        let count = sequences.len();
        if sequences != (1..=count).collect::<Vec<_>>() {
            let listed = sequences
                .iter()
                .map(usize::to_string)
                .collect::<Vec<_>>()
                .join(",");
            return Err(refusal(format!(
                "{} instances [{listed}] are not numbered 1..{count}",
                container.id
            )));
        }
        if let Some(quantity) = container.quantity
            && count > quantity
        {
            return Err(refusal(format!(
                "{count} {} named, {quantity} available",
                container.id
            )));
        }
    }
    if let Some(maximum) = request.config.max_containers
        && grouped.len() > maximum
    {
        return Err(refusal(format!(
            "{} containers hold fixed items, max_containers is {maximum}",
            grouped.len()
        )));
    }
    Ok(())
}

/// Each fixed item's support ratio from the fixed items under it, and its top load.
///
/// Support is measured against every other fixed item in the container, not only the ones
/// listed before it: the set is one arrangement, and listing order is not physics.
fn with_support_and_loads(mut placements: Vec<Placement>) -> Vec<Placement> {
    let graph = ContactGraph::from_placements(&placements);
    let ratios = (0..placements.len())
        .map(|index| graph.support_ratio(&placements, index))
        .collect::<Vec<_>>();
    for (placement, ratio) in placements.iter_mut().zip(ratios) {
        placement.support_ratio = ratio;
    }
    if let Some(loads) = calculate_top_loads(&placements) {
        for (placement, load) in placements.iter_mut().zip(loads) {
            placement.top_load = Weight(load.clamp(0, i64::MAX as i128) as i64);
        }
    }
    placements
}
