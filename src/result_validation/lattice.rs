//! Compact lattice results, expanded independently before ordinary validation.
//!
//! The wire summary is an untrusted, reconstructible representation: its lattice parameters
//! must agree with the request before they are used to synthesize placements, and every
//! synthesized placement then passes through the same checks as an explicit one.

use super::json::{Json, Ordered, Res};
use super::num::{add, ceil_div, floor_div, mul};
use super::pyfmt::{py_repr, py_str, tuple};
use super::spec::{ContainerSpec, ItemSpec, is_rotation};
use super::{Issue, MalformedResult};

const SIDES: [&str; 3] = ["length", "width", "height"];

pub(crate) fn materialize(
    coordinates_required: bool,
    result: &Json,
    items: &Ordered<ItemSpec>,
    containers: &Ordered<ContainerSpec>,
    clearance: i128,
    issues: &mut Vec<Issue>,
) -> Res<Json> {
    let packed_containers = result.at("containers")?.list("result.containers")?;
    if !packed_containers
        .iter()
        .any(|packed| packed.has("lattice_summary"))
    {
        return Ok(result.clone());
    }
    let mut expanded = result.clone();
    let mut rewritten = Vec::with_capacity(packed_containers.len());
    for packed in packed_containers {
        let mut packed = packed.clone();
        if let Some(placements) = expand(
            &packed,
            coordinates_required,
            items,
            containers,
            clearance,
            issues,
        )? {
            packed.set("placements", Json::List(placements))?;
        }
        rewritten.push(packed);
    }
    expanded.set("containers", Json::List(rewritten))?;
    Ok(expanded)
}

/// The placements to write into one container: `None` leaves it untouched.
fn expand(
    packed: &Json,
    coordinates_required: bool,
    items: &Ordered<ItemSpec>,
    containers: &Ordered<ContainerSpec>,
    clearance: i128,
    issues: &mut Vec<Issue>,
) -> Res<Option<Vec<Json>>> {
    let summary = match packed.get("lattice_summary")? {
        None | Some(Json::Null) => return Ok(None),
        Some(summary) => summary,
    };
    let container_id = py_str(packed.get_or("id", &Json::str_of("?"))?);
    if coordinates_required {
        issues.push(Issue::new(
            "unexpected_lattice_summary",
            format!("{container_id}: request requires explicit placement coordinates"),
        ));
    }
    if packed.get_null("placements")?.truthy() {
        issues.push(Issue::new(
            "lattice_placements_conflict",
            format!(
                "{container_id}: compact summary and explicit placements are mutually exclusive"
            ),
        ));
    }
    let item_type = summary.get_null("item_type")?;
    let item = items.get(item_type)?;
    let container = containers.get(packed.get_null("container_type")?)?;
    let (Some(item), Some(container)) = (item, container) else {
        // The ordinary accounting and container checks report the unknown reference.
        return Ok(None);
    };

    let fields = Fields::read(summary, &container_id)?;
    let problems = problems(&fields, item, item_type, container, clearance)?;
    if !problems.is_empty() {
        issues.extend(problems.into_iter().map(|problem| {
            Issue::new(
                "lattice_summary_mismatch",
                format!("{container_id}: {problem}"),
            )
        }));
        return Ok(Some(Vec::new()));
    }
    synthesize(&fields, summary, item_type, &container_id, clearance).map(Some)
}

struct Fields {
    orientation: Json,
    physical: [Json; 3],
    envelope: [Json; 3],
    nx: Json,
    ny: Json,
    layers_used: Json,
    layer_step: Json,
    count: Json,
}

impl Fields {
    fn read(summary: &Json, container_id: &str) -> Res<Fields> {
        let read = || -> Res<Fields> {
            let sides = |key: &str| -> Res<[Json; 3]> {
                let block = summary.at(key)?;
                Ok([
                    block.at(SIDES[0])?.at("ticks")?.clone(),
                    block.at(SIDES[1])?.at("ticks")?.clone(),
                    block.at(SIDES[2])?.at("ticks")?.clone(),
                ])
            };
            Ok(Fields {
                orientation: summary.at("orientation")?.clone(),
                physical: sides("physical_dimensions")?,
                envelope: sides("envelope_dimensions")?,
                nx: summary.at("nx")?.clone(),
                ny: summary.at("ny")?.clone(),
                layers_used: summary.at("layers_used")?.clone(),
                layer_step: summary.at("layer_step")?.at("ticks")?.clone(),
                count: summary.at("count")?.clone(),
            })
        };
        // The reference reports a missing field as a contract break, naming the container.
        read().map_err(|error| {
            MalformedResult::contract(format!(
                "{container_id}: lattice_summary is missing a canonical field: {}",
                error.message
            ))
        })
    }
}

fn json_tuple(values: &[Json]) -> String {
    tuple(values.iter().map(py_repr))
}

fn problems(
    fields: &Fields,
    item: &ItemSpec,
    item_type: &Json,
    container: &ContainerSpec,
    clearance: i128,
) -> Res<Vec<String>> {
    let mut problems = Vec::new();
    if !is_rotation(&fields.orientation)? {
        problems.push(format!(
            "unknown orientation {}",
            py_repr(&fields.orientation)
        ));
    } else {
        let orientation = fields.orientation.text("orientation")?;
        let expected = item.rotated(orientation)?;
        if !fields
            .physical
            .iter()
            .zip(expected)
            .all(|(reported, value)| reported.eq_int(value))
        {
            problems.push(format!(
                "physical dimensions {} != rotated item {}",
                json_tuple(&fields.physical),
                super::pyfmt::int_tuple(&expected),
            ));
        }
        if !item.allows(&fields.orientation) {
            problems.push(format!(
                "orientation {orientation} is forbidden for {}",
                py_str(item_type)
            ));
        }
    }
    let mut expected_envelope = Vec::with_capacity(3);
    for side in &fields.physical {
        expected_envelope.push(add(side.int("physical dimension")?, mul(2, clearance)?)?);
    }
    if !fields
        .envelope
        .iter()
        .zip(&expected_envelope)
        .all(|(reported, value)| reported.eq_int(*value))
    {
        problems.push(format!(
            "envelope dimensions {} != physical+clearance {}",
            json_tuple(&fields.envelope),
            super::pyfmt::int_tuple(&expected_envelope),
        ));
    }
    if item.nesting_height.is_some() {
        problems.push("compact lattice form is not valid for nesting items".to_owned());
    }
    if !fields.layer_step.py_eq(&fields.envelope[2]) {
        problems.push(format!(
            "layer_step {} != envelope height {}",
            py_str(&fields.layer_step),
            py_str(&fields.envelope[2]),
        ));
    }
    for (name, value) in [
        ("nx", &fields.nx),
        ("ny", &fields.ny),
        ("layers_used", &fields.layers_used),
        ("layer_step", &fields.layer_step),
        ("count", &fields.count),
    ] {
        if !value.strict_int().is_some_and(|number| number >= 1) {
            problems.push(format!(
                "{name} must be a positive integer, got {}",
                py_repr(value)
            ));
        }
    }
    if problems.is_empty() {
        independent_counts(fields, item, container, &mut problems)?;
    }
    Ok(problems)
}

/// Only reached once every field is a positive integer and the envelope is the expected one.
fn independent_counts(
    fields: &Fields,
    item: &ItemSpec,
    container: &ContainerSpec,
    problems: &mut Vec<String>,
) -> Res<()> {
    let int = |value: &Json| value.int("lattice field");
    let (nx, ny, layers_used, layer_step, count) = (
        int(&fields.nx)?,
        int(&fields.ny)?,
        int(&fields.layers_used)?,
        int(&fields.layer_step)?,
        int(&fields.count)?,
    );
    let expected_nx = floor_div(container.inner[0], int(&fields.envelope[0])?)?;
    let expected_ny = floor_div(container.inner[1], int(&fields.envelope[1])?)?;
    let expected_layers = ceil_div(count, mul(nx, ny)?)?;
    if nx != expected_nx {
        problems.push(format!("nx {nx} != independently computed {expected_nx}"));
    }
    if ny != expected_ny {
        problems.push(format!("ny {ny} != independently computed {expected_ny}"));
    }
    if layers_used != expected_layers {
        problems.push(format!(
            "layers_used {layers_used} != independently computed {expected_layers}"
        ));
    }
    let quantity = item.quantity.int("quantity")?;
    if count > quantity {
        problems.push(format!(
            "count {count} exceeds requested quantity {}",
            py_str(&item.quantity)
        ));
    }
    let vertical_capacity = floor_div(container.inner[2], layer_step)?;
    if layers_used > vertical_capacity {
        problems.push(format!(
            "layers_used {layers_used} exceeds vertical capacity {vertical_capacity}"
        ));
    }
    Ok(())
}

fn synthesize(
    fields: &Fields,
    summary: &Json,
    item_type: &Json,
    container_id: &str,
    clearance: i128,
) -> Res<Vec<Json>> {
    let int = |value: &Json| value.int("lattice field");
    let (nx, ny, count, layer_step) = (
        int(&fields.nx)?,
        int(&fields.ny)?,
        int(&fields.count)?,
        int(&fields.layer_step)?,
    );
    let envelope = [int(&fields.envelope[0])?, int(&fields.envelope[1])?];
    let per_layer = mul(nx, ny)?;
    let physical = summary.at("physical_dimensions")?.clone();
    let mut placements = Vec::new();
    for index in 0..count {
        let tick = |value: i128| Json::Dict(vec![("ticks".to_owned(), Json::Int(value))]);
        let x = add(mul(index % nx, envelope[0])?, clearance)?;
        let y = add(mul((index / nx) % ny, envelope[1])?, clearance)?;
        let z = add(mul(index / per_layer, layer_step)?, clearance)?;
        placements.push(Json::Dict(vec![
            (
                "item_id".to_owned(),
                Json::Str(format!(
                    "{}#lattice:{container_id}:{}",
                    py_str(item_type),
                    index + 1
                )),
            ),
            ("item_type".to_owned(), item_type.clone()),
            ("orientation".to_owned(), fields.orientation.clone()),
            (
                "position".to_owned(),
                Json::Dict(vec![
                    ("x".to_owned(), tick(x)),
                    ("y".to_owned(), tick(y)),
                    ("z".to_owned(), tick(z)),
                ]),
            ),
            ("dimensions".to_owned(), physical.clone()),
        ]));
    }
    Ok(placements)
}
