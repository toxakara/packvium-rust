//! The objective vector recomputed from the result alone, and the request-level rules that
//! only the whole result can answer: fixed placements, groups and solver selection.

use std::cmp::Ordering;
use std::collections::HashMap;

use super::container::{SCORE_SCALE, achieved_extent, unused_ppm};
use super::json::{EMPTY_LIST, Json, Ordered, Res, sort_py};
use super::num::{add, ceil_div, floor_div, mul, mul_div, sum};
use super::pyfmt::{int_list, int_tuple, py_repr, py_str};
use super::spec::{ContainerSpec, ItemSpec};
use super::units::{LENGTH, TICKS_PER_GRAM, WEIGHT, per_unit, to_ticks};
use super::{Issue, MalformedResult};

pub(crate) struct Inputs<'a> {
    pub(crate) containers: &'a Ordered<ContainerSpec>,
    pub(crate) items: &'a Ordered<ItemSpec>,
    pub(crate) fabricated: &'a Ordered<ContainerSpec>,
    pub(crate) clearance: i128,
}

/// `floor(volume * weight ticks per unit / (length ticks per unit ^ 3 * divisor))`.
fn dimensional_weight(
    size: [i128; 3],
    divisor: i128,
    length_unit: &str,
    weight_unit: &str,
) -> Res<i128> {
    let length = per_unit(LENGTH, length_unit)?;
    let weight = per_unit(WEIGHT, weight_unit)?;
    let volume = mul(mul(size[0], size[1])?, size[2])?;
    mul_div(
        volume,
        weight,
        mul(mul(mul(length, length)?, length)?, divisor)?,
    )
}

/// One container's carrier charge from the request's own rate card: billed weight up to whole
/// grams, the minimum charge, then the fuel surcharge rounded up.
fn landed_charge(spec: &ContainerSpec, billed_ticks: i128) -> Res<i128> {
    if spec.rate_table.is_null() {
        return Err(MalformedResult::contract(format!(
            "container {} has no rate_table, so lowest_landed_cost cannot price it",
            py_repr(&spec.id)
        )));
    }
    let table = &spec.rate_table;
    let brackets: Vec<i128> = table
        .at("weight_brackets_g")?
        .list("weight_brackets_g")?
        .iter()
        .map(|b| b.py_int("bracket"))
        .collect::<Res<_>>()?;
    let prices: Vec<i128> = table
        .at("prices_minor")?
        .list("prices_minor")?
        .iter()
        .map(|p| p.py_int("price"))
        .collect::<Res<_>>()?;
    let grams = ceil_div(billed_ticks, TICKS_PER_GRAM)?;
    let zero = Json::Int(0);
    for (bound, price) in brackets.iter().zip(&prices) {
        if grams <= *bound {
            let base = (*price).max(
                table
                    .get_or("minimum_charge_minor", &zero)?
                    .py_int("minimum_charge_minor")?,
            );
            let permille = table
                .get_or("fuel_surcharge_permille", &zero)?
                .py_int("fuel_surcharge_permille")?;
            return add(base, ceil_div(mul(base, permille)?, 1000)?);
        }
    }
    let last = brackets
        .last()
        .ok_or_else(|| MalformedResult::shape("an empty rate table".to_owned()))?;
    Err(MalformedResult::contract(format!(
        "container {} bills at {grams} g, above its rate table's last bracket ({last} g); the shipment has no published price",
        py_repr(&spec.id)
    )))
}

/// The dimensional-weight inputs as the request states them; read only when a container is
/// actually billed, as the reference reads them.
struct Billing<'a> {
    divisor: &'a Json,
    length_unit: &'a Json,
    weight_unit: &'a Json,
}

/// The objective vector of `result` under the objective `name`.
fn objective(
    result: &Json,
    inputs: &Inputs<'_>,
    name: &Json,
    billing: &Billing<'_>,
) -> Res<Vec<i128>> {
    let (mut cost, mut unused, mut height, mut billable, mut achieved_height, mut landed) =
        (0, 0, 0, 0, 0, 0);
    let billed_objective = name.is_str("shipping_cost") || name.is_str("lowest_landed_cost");
    let containers = result.at("containers")?.list("result.containers")?;
    for packed in containers {
        let fabricated = inputs.fabricated.get(packed.get_null("id")?)?;
        let spec = match fabricated {
            Some(spec) => spec,
            None => match inputs.containers.get(packed.at("container_type")?)? {
                Some(spec) => spec,
                None => continue,
            },
        };
        cost = add(cost, spec.cost_minor)?;
        let volume = spec.volume()?;
        if volume == 0 {
            continue;
        }
        let used = packed
            .at("used_volume_ticks3")?
            .py_int("used_volume_ticks3")?;
        unused = add(unused, unused_ppm(volume, used)?)?;
        let mut top: Option<i128> = None;
        for placement in packed.at("placements")?.list("placements")? {
            let z = placement
                .at("position")?
                .at("z")?
                .at("ticks")?
                .int("placement ticks")?;
            let extent = placement
                .at("dimensions")?
                .at("height")?
                .at("ticks")?
                .int("placement ticks")?;
            let reach = add(add(z, extent)?, inputs.clearance)?;
            top = Some(top.map_or(reach, |current| current.max(reach)));
        }
        let top = top.unwrap_or(0);
        height = add(height, floor_div(mul(top, SCORE_SCALE)?, spec.inner[2])?)?;
        achieved_height = add(achieved_height, top)?;
        if billed_objective && !billing.divisor.is_null() {
            let divisor = billing.divisor.int("dimensional_weight_divisor")?;
            let empty = Json::Dict(Vec::new());
            let gross = packed
                .get_or("gross_weight", &empty)?
                .get_or("ticks", &Json::Int(0))?
                .py_int("gross_weight")?;
            let billed_size = if spec.flat_bag {
                achieved_extent(packed, inputs.clearance)?
            } else {
                spec.outer.unwrap_or(spec.inner)
            };
            let dimensional = dimensional_weight(
                billed_size,
                divisor,
                billing.length_unit.text("dimensional_weight_length_unit")?,
                billing.weight_unit.text("dimensional_weight_weight_unit")?,
            )?;
            let billed = gross.max(dimensional);
            billable = add(billable, billed)?;
            if name.is_str("lowest_landed_cost") {
                landed = add(landed, landed_charge(spec, billed)?)?;
            }
        }
    }
    let unpacked_items = result.at("unpacked_items")?.list("result.unpacked_items")?;
    let unpacked_count = unpacked_items.len() as i128;
    let opened = containers.len() as i128;
    let requires_divisor = |objective: &str| {
        MalformedResult::contract(format!(
            "{objective} objective requires configuration.dimensional_weight_divisor"
        ))
    };
    let Json::Str(name_text) = name else {
        return Err(MalformedResult::contract(format!(
            "unknown requested objective {}",
            py_repr(name)
        )));
    };
    Ok(match name_text.as_str() {
        "default" => vec![unpacked_count, opened, cost, unused, height],
        "lowest_cost" => vec![unpacked_count, cost, opened, unused, height],
        "shipping_cost" => {
            if billing.divisor.is_null() {
                return Err(requires_divisor("shipping_cost"));
            }
            vec![unpacked_count, billable, opened, unused, height]
        }
        "lowest_landed_cost" => {
            if billing.divisor.is_null() {
                return Err(requires_divisor("lowest_landed_cost"));
            }
            vec![unpacked_count, landed, opened, unused, height]
        }
        "open_dimension_height" => vec![unpacked_count, achieved_height, opened, cost, unused],
        "maximum_value" => {
            let mut forgone = Vec::new();
            for unpacked in unpacked_items {
                if let Some(spec) = inputs.items.get(unpacked.at("item_type")?)? {
                    forgone.push(if spec.value.truthy() {
                        spec.value.int("value")?
                    } else {
                        0
                    });
                }
            }
            vec![unpacked_count, sum(forgone)?, opened, cost, unused]
        }
        "maximum_volume" => {
            let nominal = |spec: &ItemSpec| {
                mul(
                    mul(spec.dimensions[0], spec.dimensions[1])?,
                    spec.dimensions[2],
                )
            };
            let mut unpacked_volume = 0;
            for unpacked in unpacked_items {
                if let Some(spec) = inputs.items.get(unpacked.at("item_type")?)? {
                    unpacked_volume = add(unpacked_volume, nominal(spec)?)?;
                }
            }
            let mut requested = 0;
            for spec in inputs.items.values() {
                requested = add(
                    requested,
                    mul(nominal(spec)?, spec.quantity.int("quantity")?)?,
                )?;
            }
            let unpacked_ppm = if requested == 0 {
                0
            } else {
                ceil_div(mul(unpacked_volume, SCORE_SCALE)?, requested)?
            };
            vec![unpacked_ppm, unpacked_count, opened, cost, unused]
        }
        _ => {
            return Err(MalformedResult::contract(format!(
                "unknown requested objective {}",
                py_repr(name)
            )));
        }
    })
}

pub(crate) fn check_objective(
    request: &Json,
    result: &Json,
    inputs: &Inputs<'_>,
    issues: &mut Vec<Issue>,
) -> Res<()> {
    let empty = Json::Dict(Vec::new());
    let configuration = request.get_or("configuration", &empty)?;
    let default_objective = Json::str_of("default");
    let selected = configuration.get_or("objective", &default_objective)?;
    let reported_name = result.get_null("objective")?;
    if (configuration.has("objective") || !reported_name.is_null())
        && !reported_name.py_eq(selected)
    {
        issues.push(Issue::new(
            "objective_mismatch",
            format!(
                "reported {}, requested {}",
                py_repr(reported_name),
                py_repr(selected)
            ),
        ));
    }
    let reported: Vec<i128> = result
        .at("score")?
        .list("score")?
        .iter()
        .map(|component| component.py_int("score"))
        .collect::<Res<_>>()?;
    let length_unit = Json::str_of("in");
    let weight_unit = Json::str_of("lb");
    let billing = Billing {
        divisor: configuration.get_null("dimensional_weight_divisor")?,
        length_unit: configuration.get_or("dimensional_weight_length_unit", &length_unit)?,
        weight_unit: configuration.get_or("dimensional_weight_weight_unit", &weight_unit)?,
    };
    let recomputed = objective(result, inputs, selected, &billing)?;
    if reported != recomputed {
        issues.push(Issue::new(
            "score_mismatch",
            format!(
                "reported {}, recomputed {}",
                int_list(&reported),
                int_list(&recomputed)
            ),
        ));
    }
    Ok(())
}

pub(crate) fn check_solver_selection(
    request: &Json,
    result: &Json,
    issues: &mut Vec<Issue>,
) -> Res<()> {
    let empty = Json::Dict(Vec::new());
    let selected = request
        .get_or("configuration", &empty)?
        .get_or("solvers", &EMPTY_LIST)?;
    if !selected.truthy() {
        return Ok(());
    }
    let selected_list = selected.list("configuration.solvers")?;
    let reported = result.at("algorithm")?.get_null("solver")?;
    let base = match reported {
        Json::Str(name) => {
            Json::str_of(name.split_once(':').map_or(name.as_str(), |(head, _)| head))
        }
        other => other.clone(),
    };
    if !selected_list.iter().any(|solver| solver.py_eq(&base)) {
        issues.push(Issue::new(
            "solver_selection_mismatch",
            format!(
                "reported {}, requested one of {}",
                py_repr(reported),
                py_repr(selected)
            ),
        ));
    }
    Ok(())
}

/// A group travels together or not at all: one container, and never half-packed.
pub(crate) fn check_groups(request: &Json, result: &Json, issues: &mut Vec<Issue>) -> Res<()> {
    let raw_items = request.at("items")?.list("request.items")?;
    let mut groups: Ordered<Json> = Ordered::default();
    for raw in raw_items {
        let group = raw.get_null("group")?;
        if !group.is_null() {
            groups.insert(raw.at("id")?, raw.at("group")?.clone())?;
        }
    }
    if groups.is_empty() {
        return Ok(());
    }
    let mut located: Ordered<Ordered<()>> = Ordered::default();
    let mut packed: Ordered<i128> = Ordered::default();
    for container in result.at("containers")?.list("result.containers")? {
        for placement in container.at("placements")?.list("placements")? {
            let Some(group) = groups.get(placement.at("item_type")?)? else {
                continue;
            };
            let container_id = container.at("id")?;
            match located.get_mut(group)? {
                Some(places) => places.insert(container_id, ())?,
                None => {
                    let mut places = Ordered::default();
                    places.insert(container_id, ())?;
                    located.insert(group, places)?;
                }
            }
            let count = packed.get(group)?.copied().unwrap_or(0);
            packed.insert(group, count + 1)?;
        }
    }
    let mut expected: Ordered<i128> = Ordered::default();
    for raw in raw_items {
        let group = raw.get_null("group")?;
        if group.is_null() {
            continue;
        }
        let total = expected.get(group)?.copied().unwrap_or(0);
        expected.insert(
            group,
            add(
                total,
                raw.get_or("quantity", &Json::Int(1))?.int("quantity")?,
            )?,
        )?;
    }
    let mut names: Vec<&Json> = located.iter().map(|(group, _)| group).collect();
    sort_py(&mut names)?;
    for group in names {
        let places = located.get(group)?.expect("a located group");
        if places.len() > 1 {
            let mut where_: Vec<&str> = places
                .iter()
                .map(|(place, _)| place.text("a container id"))
                .collect::<Res<_>>()?;
            where_.sort_unstable();
            issues.push(Issue::new(
                "group_split",
                format!("{}: {}", py_str(group), where_.join(", ")),
            ));
        }
        let placed = packed.get(group)?.copied().unwrap_or(0);
        let wanted = expected.get(group)?.copied().unwrap_or(0);
        if placed != wanted {
            issues.push(Issue::new(
                "group_partially_packed",
                format!("{}: {placed} of {wanted} placed", py_str(group)),
            ));
        }
    }
    Ok(())
}

/// A fixed placement's identity: container id, item type, x, y, z, orientation.
type FixedKey = Vec<Json>;

fn key_hash(key: &FixedKey) -> Res<String> {
    key.iter()
        .map(Json::hash_key)
        .collect::<Res<Vec<_>>>()
        .map(|parts| parts.join("\u{0}"))
}

/// Every fixed placement is reported where the request put it, and nothing else claims to be.
pub(crate) fn check_fixed_placements(
    request: &Json,
    result: &Json,
    unit: &str,
    issues: &mut Vec<Issue>,
) -> Res<()> {
    let mut requested: HashMap<String, FixedKey> = HashMap::new();
    let empty_list = Json::List(Vec::new());
    for entry in request
        .get_or("fixed_placements", &empty_list)?
        .list("fixed_placements")?
    {
        let container_id = format!(
            "{}#{}",
            py_str(entry.at("container_type")?),
            py_str(entry.get_or("container_instance", &Json::Int(1))?),
        );
        let empty = Json::Dict(Vec::new());
        let position = entry.get_or("position", &empty)?;
        let zero = Json::Int(0);
        let mut key = vec![Json::Str(container_id), entry.at("item_type")?.clone()];
        for axis in ["x", "y", "z"] {
            key.push(Json::Int(to_ticks(
                position.get_or(axis, &zero)?,
                unit,
                LENGTH,
            )?));
        }
        key.push(entry.at("orientation")?.clone());
        requested.insert(key_hash(&key)?, key);
    }
    let mut reported: HashMap<String, FixedKey> = HashMap::new();
    let mut container_types: Ordered<Json> = Ordered::default();
    let blank = Json::str_of("");
    for container in result.at("containers")?.list("result.containers")? {
        let container_id = container.get_or("id", &blank)?;
        container_types.insert(container_id, container.at("container_type")?.clone())?;
        for placement in container.at("placements")?.list("placements")? {
            if !matches!(placement.get_null("fixed")?, Json::Bool(true)) {
                continue;
            }
            let position = placement.at("position")?;
            let mut key = vec![container_id.clone(), placement.at("item_type")?.clone()];
            for axis in ["x", "y", "z"] {
                key.push(position.at(axis)?.at("ticks")?.clone());
            }
            key.push(placement.at("orientation")?.clone());
            reported.insert(key_hash(&key)?, key);
        }
    }
    for key in sorted_keys(requested.values().collect())? {
        let container_id = key[0].text("container id")?;
        let declared_type = container_id
            .rsplit_once('#')
            .map_or(container_id, |(head, _)| head);
        let matches_type = container_types
            .get(&key[0])?
            .is_some_and(|reported_type| reported_type.is_str(declared_type));
        if !matches_type {
            issues.push(Issue::new(
                "fixed_container_missing",
                container_id.to_owned(),
            ));
        } else if !reported.contains_key(&key_hash(key)?) {
            issues.push(Issue::new(
                "fixed_placement_moved",
                format!(
                    "{} fixed in {container_id} at {} {}",
                    py_str(&key[1]),
                    position_tuple(key),
                    py_str(&key[5])
                ),
            ));
        }
    }
    let unexpected: Vec<&FixedKey> = reported
        .iter()
        .filter(|(hash, _)| !requested.contains_key(*hash))
        .map(|(_, key)| key)
        .collect();
    for key in sorted_keys(unexpected)? {
        issues.push(Issue::new(
            "unexpected_fixed_placement",
            format!(
                "{} in {} at {} {}",
                py_str(&key[1]),
                py_str(&key[0]),
                position_tuple(key),
                py_str(&key[5])
            ),
        ));
    }
    Ok(())
}

fn position_tuple(key: &FixedKey) -> String {
    match (
        key[2].strict_int(),
        key[3].strict_int(),
        key[4].strict_int(),
    ) {
        (Some(x), Some(y), Some(z)) => int_tuple(&[x, y, z]),
        _ => super::pyfmt::tuple(key[2..5].iter().map(py_repr)),
    }
}

fn sorted_keys(mut keys: Vec<&FixedKey>) -> Res<Vec<&FixedKey>> {
    let mut failure = None;
    keys.sort_by(|left, right| {
        Json::List(left.to_vec())
            .py_cmp(&Json::List(right.to_vec()))
            .unwrap_or_else(|error| {
                failure = Some(error);
                Ordering::Equal
            })
    });
    failure.map_or(Ok(keys), Err)
}
