//! One packed container: its geometry, its loads, its accounting figures, and the box a
//! made-to-measure machine would have cut for it.

use super::geometry::{Cuboid, solids_overlap};
use super::json::{Json, Ordered, Res};
use super::num::{Ratio, abs, add, ceil_div, floor_div, mul, mul_div, py_mod, sub, sum};
use super::pyfmt::{int_tuple, list, py_repr, py_str, str_repr, tuple};
use super::spec::{ContainerSpec, ItemSpec, rotation_axes};
use super::stacking::{
    Entry, Envelopes, SQUARE_METRE_TICKS, check_crush, check_route_removal_order,
    check_stack_density, check_support_and_load, entries, occupied_volume, placed_hull,
    propagate_load, support_graph, valid_nesting,
};
use super::{Issue, Label, MalformedResult};

/// Ratios in the objective vector and the reserve are carried as parts per million.
pub(crate) const SCORE_SCALE: i128 = 1_000_000;

const SIDES: [&str; 3] = ["length", "width", "height"];

/// The physical box grown by the clearance margin. A placement without integer tick
/// coordinates is the reference's contract refusal, worded as it words it.
pub(crate) fn envelope(placement: &Json, clearance: i128) -> Res<Cuboid> {
    read_envelope(placement, clearance).map_err(|error| {
        MalformedResult::contract(format!(
            "placement is missing tick coordinates: {}",
            error.message
        ))
    })
}

fn read_envelope(placement: &Json, clearance: i128) -> Res<Cuboid> {
    let position = placement.at("position")?;
    let dimensions = placement.at("dimensions")?;
    let tick = |block: &Json, key: &str| -> Res<i128> {
        block.at(key)?.at("ticks")?.int("placement ticks")
    };
    let margin = mul(2, clearance)?;
    Cuboid::new(
        sub(tick(position, "x")?, clearance)?,
        sub(tick(position, "y")?, clearance)?,
        sub(tick(position, "z")?, clearance)?,
        add(tick(dimensions, "length")?, margin)?,
        add(tick(dimensions, "width")?, margin)?,
        add(tick(dimensions, "height")?, margin)?,
    )
}

/// The largest envelope x2, y2 and z2 over a packed container's placements.
pub(crate) fn achieved_extent(packed: &Json, clearance: i128) -> Res<[i128; 3]> {
    let mut extent: Option<[i128; 3]> = None;
    for placement in packed.at("placements")?.list("placements")? {
        let envelope = envelope(placement, clearance)?;
        extent = Some(match extent {
            None => [envelope.x2, envelope.y2, envelope.z2],
            Some([x, y, z]) => [x.max(envelope.x2), y.max(envelope.y2), z.max(envelope.z2)],
        });
    }
    Ok(extent.unwrap_or([0, 0, 0]))
}

/// `achieved_dimensions` is present exactly when the container declares a soft size, and
/// equals the tight extent; a flat bag's extent must pass the girth rule.
pub(crate) fn check_achieved_extent(
    packed: &Json,
    spec: &ContainerSpec,
    clearance: i128,
    issues: &mut Vec<Issue>,
) -> Res<()> {
    let reported = packed.get_null("achieved_dimensions")?;
    let packed_id = Label::of(packed)?;
    if !spec.reports_achieved_extent {
        if !reported.is_null() {
            issues.push(Issue::new(
                "achieved_dimensions_mismatch",
                format!("{packed_id}: reported for a container of fixed size"),
            ));
        }
        return packed_id.verify();
    }
    let [a, b, c] = achieved_extent(packed, clearance)?;
    let stated: Option<Vec<Json>> = if reported.is_dict() {
        let empty = Json::Dict(Vec::new());
        Some(
            SIDES
                .iter()
                .map(|side| Ok(reported.get_or(side, &empty)?.get_null("ticks")?.clone()))
                .collect::<Res<_>>()?,
        )
    } else {
        None
    };
    if !matches_ints(stated.as_deref(), &[a, b, c]) {
        issues.push(Issue::new(
            "achieved_dimensions_mismatch",
            format!(
                "{packed_id}: reported {}, computed {}",
                optional_tuple(stated.as_deref()),
                int_tuple(&[a, b, c])
            ),
        ));
    }
    if spec.flat_bag && (add(a, c)? > spec.inner[0] || add(b, c)? > spec.inner[1]) {
        issues.push(Issue::new(
            "bag_girth_exceeded",
            format!(
                "{packed_id}: a + c = {}, b + c = {}, flat size {} x {}",
                add(a, c)?,
                add(b, c)?,
                spec.inner[0],
                spec.inner[1],
            ),
        ));
    }
    packed_id.verify()
}

fn matches_ints(stated: Option<&[Json]>, expected: &[i128]) -> bool {
    stated.is_some_and(|values| {
        values.len() == expected.len() && values.iter().zip(expected).all(|(v, e)| v.eq_int(*e))
    })
}

fn optional_tuple(values: Option<&[Json]>) -> String {
    values.map_or_else(
        || "None".to_owned(),
        |values| tuple(values.iter().map(py_repr)),
    )
}

/// The smallest size on the machine grid `minimum + k * increment` that is at least `achieved`.
fn fabricated_side(achieved: i128, minimum: i128, increment: i128) -> Res<i128> {
    add(
        minimum,
        mul(
            ceil_div(sub(achieved.max(minimum), minimum)?, increment)?,
            increment,
        )?,
    )
}

/// `ceil(rate * 2(LW + LH + WH) / 1 m^2)`, the material and board rounding.
fn per_square_metre_ceiling(rate: i128, sides: [i128; 3]) -> Res<i128> {
    let [length, width, height] = sides;
    let area = mul(
        2,
        add(
            add(mul(length, width)?, mul(length, height)?)?,
            mul(width, height)?,
        )?,
    )?;
    ceil_div(mul(rate, area)?, SQUARE_METRE_TICKS)
}

/// `(value.get(key) or {}).get("ticks")` per key, or `None` when `value` is not an object.
fn reported_ticks(value: &Json) -> Res<Option<Vec<Json>>> {
    if !value.is_dict() {
        return Ok(None);
    }
    let mut ticks = Vec::with_capacity(3);
    for side in SIDES {
        let block = value.get_null(side)?;
        ticks.push(if block.truthy() {
            block.get_null("ticks")?.clone()
        } else {
            Json::Null
        });
    }
    Ok(Some(ticks))
}

/// `(block.get(key) or {}).get("ticks")`.
fn nested_ticks(block: &Json, key: &str) -> Res<Json> {
    let value = block.get_null(key)?;
    Ok(if value.truthy() {
        value.get_null("ticks")?.clone()
    } else {
        Json::Null
    })
}

/// Recompute a made-to-measure container's box from its placements alone and return the spec
/// every later check uses: the recomputed box, never the reported one.
pub(crate) fn check_made_to_measure(
    packed: &Json,
    compact: bool,
    spec: &ContainerSpec,
    items: &Ordered<ItemSpec>,
    clearance: i128,
    issues: &mut Vec<Issue>,
) -> Res<ContainerSpec> {
    let container_id = py_str(packed.get_or("id", &Json::str_of("?"))?);
    let reported_block = packed.get_null("made_to_measure")?;
    let Some(machine) = &spec.made_to_measure else {
        if !reported_block.is_null() {
            issues.push(Issue::new("made_to_measure_unexpected", container_id));
        }
        return Ok(spec.clone());
    };
    let empty = Json::Dict(Vec::new());
    let block = if reported_block.is_dict() {
        reported_block
    } else {
        issues.push(Issue::new("made_to_measure_missing", container_id.clone()));
        &empty
    };
    if compact {
        issues.push(Issue::new(
            "lattice_summary_on_made_to_measure",
            container_id.clone(),
        ));
    }
    let achieved = achieved_extent(packed, clearance)?;
    let mut fabricated = [0; 3];
    for axis in 0..3 {
        fabricated[axis] = fabricated_side(
            achieved[axis],
            machine.minimum[axis],
            machine.increment[axis],
        )?;
    }
    if (0..3).any(|axis| fabricated[axis] > spec.inner[axis]) {
        issues.push(Issue::new(
            "fabricated_dimensions_exceed_machine",
            format!(
                "{container_id}: {} beyond {}",
                int_tuple(&fabricated),
                int_tuple(&spec.inner)
            ),
        ));
    }
    let reported_inner = reported_ticks(packed.get_null("inner_dimensions")?)?;
    if !matches_ints(reported_inner.as_deref(), &fabricated) {
        issues.push(Issue::new(
            "fabricated_dimensions_mismatch",
            format!(
                "{container_id}: reported {}, computed {}",
                optional_tuple(reported_inner.as_deref()),
                int_tuple(&fabricated)
            ),
        ));
    }
    let outer = [
        add(fabricated[0], machine.allowance[0])?,
        add(fabricated[1], machine.allowance[1])?,
        add(fabricated[2], machine.allowance[2])?,
    ];
    let reported_outer = reported_ticks(packed.get_null("outer_dimensions")?)?;
    if !matches_ints(reported_outer.as_deref(), &outer) {
        issues.push(Issue::new(
            "outer_dimensions_mismatch",
            format!(
                "{container_id}: reported {}, computed {}",
                optional_tuple(reported_outer.as_deref()),
                int_tuple(&outer)
            ),
        ));
    }
    let cost = add(
        spec.cost_minor,
        per_square_metre_ceiling(machine.price_per_square_metre_minor, fabricated)?,
    )?;
    let reported_cost = block.get_null("cost_minor")?;
    if !reported_cost.eq_int(cost) {
        issues.push(Issue::new(
            "made_to_measure_cost_mismatch",
            format!(
                "{container_id}: reported {}, computed {cost}",
                py_str(reported_cost)
            ),
        ));
    }
    let tare = add(
        spec.tare_weight,
        per_square_metre_ceiling(machine.tare_per_square_metre, fabricated)?,
    )?;
    let reported_tare = nested_ticks(block, "tare_weight")?;
    if !reported_tare.eq_int(tare) {
        issues.push(Issue::new(
            "made_to_measure_tare_mismatch",
            format!(
                "{container_id}: reported {}, computed {tare}",
                py_str(&reported_tare)
            ),
        ));
    }
    let payload = payload_of(packed, items)?;
    let reported_gross = nested_ticks(packed, "gross_weight")?;
    let gross = add(tare, payload)?;
    if !reported_gross.eq_int(gross) {
        issues.push(Issue::new(
            "gross_weight_mismatch",
            format!(
                "{container_id}: reported {}, computed {gross}",
                py_str(&reported_gross)
            ),
        ));
    }
    Ok(ContainerSpec {
        inner: fabricated,
        outer: Some(outer),
        cost_minor: cost,
        tare_weight: tare,
        ..spec.clone()
    })
}

/// The summed weight of every placement whose type the request declares.
fn payload_of(packed: &Json, items: &Ordered<ItemSpec>) -> Res<i128> {
    let mut payload = 0;
    for placement in packed.at("placements")?.list("placements")? {
        if let Some(spec) = items.get(placement.at("item_type")?)? {
            payload = add(payload, spec.weight)?;
        }
    }
    Ok(payload)
}

pub(crate) fn check_container(
    packed: &Json,
    spec: &ContainerSpec,
    items: &Ordered<ItemSpec>,
    clearance: i128,
    global_support: Ratio,
    issues: &mut Vec<Issue>,
) -> Res<()> {
    let packed_id = Label::of(packed)?;
    let placements = packed.at("placements")?.list("placements")?;
    let boundary = Cuboid::new(0, 0, 0, spec.inner[0], spec.inner[1], spec.inner[2])?;
    let mut envelopes: Envelopes<'_> = Ordered::default();
    let mut payload = 0;

    for placement in placements {
        let item_id = placement.at("item_id")?;
        let item_spec = items.get(placement.at("item_type")?)?;
        let envelope = envelope(placement, clearance)?;
        envelopes.insert(
            item_id,
            Entry {
                envelope,
                placement,
                spec: item_spec,
            },
        )?;
        let item_id = py_str(item_id);

        if !boundary.contains(&envelope) {
            issues.push(Issue::new(
                "outside_container",
                format!("{item_id} in {packed_id}"),
            ));
        }
        for obstacle in &spec.obstacles {
            if envelope.intersects(obstacle)
                && solids_overlap(
                    &placed_hull(&envelope, placement, item_spec)?,
                    &obstacle.vertices(),
                )?
            {
                issues.push(Issue::new(
                    "obstacle_collision",
                    format!("{item_id} in {packed_id}"),
                ));
            }
        }
        let Some(item_spec) = item_spec else { continue };
        payload = add(payload, item_spec.weight)?;
        check_rotation(placement, item_spec, &item_id, issues)?;
        if !item_spec.eligible_container_tags.is_empty()
            && item_spec.eligible_container_tags.is_disjoint(&spec.tags)
        {
            issues.push(Issue::new(
                "container_ineligible",
                format!(
                    "{item_id}: {} has none of {}",
                    py_str(packed.at("container_type")?),
                    list(
                        item_spec
                            .eligible_container_tags
                            .iter()
                            .map(|tag| str_repr(tag))
                    ),
                ),
            ));
        }
        if item_spec.must_be_on_floor && envelope.z != 0 {
            issues.push(Issue::new(
                "floor_violation",
                format!("{item_id} at z={}", envelope.z),
            ));
        }
    }

    check_pairs(&envelopes, &packed_id, issues)?;
    let graph = support_graph(&envelopes)?;
    check_support_and_load(&envelopes, &graph, global_support, issues)?;
    let loads = propagate_load(&envelopes, &graph)?;
    if let Some(max_density) = spec.max_stack_density {
        check_stack_density(&packed_id, &envelopes, &loads, max_density, issues)?;
    }
    check_crush(&packed_id, &envelopes, &loads, issues)?;
    check_route_removal_order(
        &packed_id,
        &envelopes,
        &graph,
        spec.inner,
        &spec.access_directions,
        issues,
    )?;

    check_limits(packed, &packed_id, placements, spec, items, payload, issues)?;
    check_volumes(
        packed, &packed_id, placements, spec, items, &envelopes, &loads, issues,
    )?;
    let total_weight = payload_of(packed, items)?;
    check_centre_of_mass(
        packed,
        &packed_id,
        placements,
        spec,
        items,
        total_weight,
        issues,
    )?;
    check_axles(
        packed,
        &packed_id,
        placements,
        spec,
        items,
        total_weight,
        issues,
    )?;
    packed_id.verify()
}

fn check_rotation(
    placement: &Json,
    spec: &ItemSpec,
    item_id: &str,
    issues: &mut Vec<Issue>,
) -> Res<()> {
    let orientation = placement.at("orientation")?;
    if !super::spec::is_rotation(orientation)? {
        issues.push(Issue::new(
            "unknown_orientation",
            format!("{item_id}: {}", py_str(orientation)),
        ));
        return Ok(());
    }
    let name = orientation.text("orientation")?;
    if !spec.allows(orientation) {
        issues.push(Issue::new(
            "forbidden_rotation",
            format!("{item_id}: {name}"),
        ));
    }
    let axes = rotation_axes(name)?;
    let expected = [
        spec.dimensions[axes[0]],
        spec.dimensions[axes[1]],
        spec.dimensions[axes[2]],
    ];
    let dimensions = placement.at("dimensions")?;
    let actual: Vec<Json> = SIDES
        .iter()
        .map(|side| Ok(dimensions.at(side)?.at("ticks")?.clone()))
        .collect::<Res<_>>()?;
    if !matches_ints(Some(&actual), &expected) {
        issues.push(Issue::new(
            "dimension_mismatch",
            format!(
                "{item_id}: {name} of {} is {}, got {}",
                int_tuple(&spec.dimensions),
                int_tuple(&expected),
                tuple(actual.iter().map(py_repr)),
            ),
        ));
    }
    Ok(())
}

fn check_pairs(envelopes: &Envelopes<'_>, packed_id: &Label, issues: &mut Vec<Issue>) -> Res<()> {
    let list = entries(envelopes);
    for (first, (left_id, left)) in list.iter().enumerate() {
        for (right_id, right) in &list[first + 1..] {
            if left.envelope.intersects(&right.envelope)
                && solids_overlap(
                    &placed_hull(&left.envelope, left.placement, left.spec)?,
                    &placed_hull(&right.envelope, right.placement, right.spec)?,
                )?
                && !valid_nesting(left, right)?
            {
                issues.push(Issue::new(
                    "collision",
                    format!(
                        "{} with {} in {packed_id}",
                        py_str(left_id),
                        py_str(right_id)
                    ),
                ));
            }
            if let (Some(left_spec), Some(right_spec)) = (left.spec, right.spec)
                && (!left_spec.incompatible_tags.is_disjoint(&right_spec.tags)
                    || !right_spec.incompatible_tags.is_disjoint(&left_spec.tags))
            {
                issues.push(Issue::new(
                    "incompatible_items",
                    format!(
                        "{} is incompatible with {} in {packed_id}",
                        py_str(left_id),
                        py_str(right_id)
                    ),
                ));
            }
        }
    }
    Ok(())
}

fn check_limits(
    packed: &Json,
    packed_id: &Label,
    placements: &[Json],
    spec: &ContainerSpec,
    items: &Ordered<ItemSpec>,
    payload: i128,
    issues: &mut Vec<Issue>,
) -> Res<()> {
    if let Some(limit) = spec.max_payload
        && payload > limit
    {
        issues.push(Issue::new(
            "payload_exceeded",
            format!("{packed_id}: {payload} > {limit} ticks"),
        ));
    }
    let empty = Json::Dict(Vec::new());
    let reported = packed.get_or("payload_weight", &empty)?.get_null("ticks")?;
    if !reported.is_null() && !reported.eq_int(payload) {
        issues.push(Issue::new(
            "payload_weight_mismatch",
            format!(
                "{packed_id}: reported {}, computed {payload}",
                py_str(reported)
            ),
        ));
    }
    if let Some(limit) = spec.max_items
        && placements.len() as i128 > limit
    {
        issues.push(Issue::new(
            "max_items_exceeded",
            format!("{packed_id}: {} > {limit}", placements.len()),
        ));
    }
    for (tag, limit) in &spec.tag_limits {
        let mut count = 0;
        for placement in placements {
            if items
                .get(placement.at("item_type")?)?
                .is_some_and(|item| item.tags.contains(tag))
            {
                count += 1;
            }
        }
        if count > *limit {
            issues.push(Issue::new(
                "tag_count_exceeded",
                format!("{packed_id}: {tag} count {count} > limit {limit}"),
            ));
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn check_volumes(
    packed: &Json,
    packed_id: &Label,
    placements: &[Json],
    spec: &ContainerSpec,
    items: &Ordered<ItemSpec>,
    envelopes: &Envelopes<'_>,
    loads: &[i128],
    issues: &mut Vec<Issue>,
) -> Res<()> {
    let list = entries(envelopes);
    // Every placement counts, through the envelope its item id resolves to -- a repeated id
    // resolves to its last placement, as the reference's dict does.
    let mut used = 0;
    for placement in placements {
        let item_id = placement.at("item_id")?;
        let index = envelopes.position(item_id)?.ok_or_else(|| {
            super::MalformedResult::shape("placement without an envelope".to_owned())
        })?;
        let spec = items.get(placement.at("item_type")?)?;
        used = add(
            used,
            occupied_volume(&list[index].1.envelope, placement, spec, loads[index])?,
        )?;
    }
    for (first, (_, left)) in list.iter().enumerate() {
        for (_, right) in &list[first + 1..] {
            if valid_nesting(left, right)? {
                let nesting = left.spec.and_then(|spec| spec.nesting_height).unwrap_or(0);
                used = sub(
                    used,
                    mul(mul(nesting, left.envelope.length)?, left.envelope.width)?,
                )?;
            }
        }
    }
    let reported_volume = packed.get_null("used_volume_ticks3")?;
    if !reported_volume.is_null() && reported_volume.py_int("used_volume_ticks3")? != used {
        issues.push(Issue::new(
            "used_volume_mismatch",
            format!(
                "{packed_id}: reported {}, computed {used}",
                py_str(reported_volume)
            ),
        ));
    }
    let volume = spec.volume()?;
    let scaled_reserve = ratio_ppm(spec.void_fill_reserve_ratio);
    let reserved = add(
        mul(floor_div(volume, SCORE_SCALE)?, scaled_reserve)?,
        floor_div(
            mul(py_mod(volume, SCORE_SCALE)?, scaled_reserve)?,
            SCORE_SCALE,
        )?,
    )?;
    let reported_reserve = packed.get_null("void_fill_reserve_ticks3")?;
    if !reported_reserve.is_null()
        && reported_reserve.py_int("void_fill_reserve_ticks3")? != reserved
    {
        issues.push(Issue::new(
            "void_fill_reserve_mismatch",
            format!(
                "{packed_id}: reported {}, computed {reserved}",
                py_str(reported_reserve)
            ),
        ));
    } else if reserved != 0 && reported_reserve.is_null() {
        issues.push(Issue::new(
            "void_fill_reserve_mismatch",
            format!("{packed_id}: non-zero reserve {reserved} is not reported"),
        ));
    }
    let usable = sub(volume, reserved)?;
    if used > usable {
        issues.push(Issue::new(
            "void_fill_reserve_exceeded",
            format!("{packed_id}: used {used} > usable {usable}"),
        ));
    }
    Ok(())
}

/// A support, void-fill reserve or compression ratio in parts per million exactly as every solver converts
/// it: the IEEE double `floor(ratio * 10^6 + 0.5)` of docs/UNITS-AND-NUMERICS.md. Rounding the
/// exact rational instead disagrees on about one in a hundred seven-decimal halves and would
/// refuse placements, and reported reserves, every solver produces.
pub(crate) fn ratio_ppm(ratio: Ratio) -> i128 {
    (ratio.to_f64() * SCORE_SCALE as f64 + 0.5) as i128
}

/// `sum(weight * (2 * position + extent))` along one axis, over declared item types.
fn doubled_moment(
    placements: &[Json],
    items: &Ordered<ItemSpec>,
    axis: &str,
    side: &str,
) -> Res<i128> {
    let mut moments = Vec::new();
    for placement in placements {
        let Some(spec) = items.get(placement.at("item_type")?)? else {
            continue;
        };
        let position = placement
            .at("position")?
            .at(axis)?
            .at("ticks")?
            .int("placement ticks")?;
        let extent = placement
            .at("dimensions")?
            .at(side)?
            .at("ticks")?
            .int("placement ticks")?;
        moments.push(mul(spec.weight, add(mul(2, position)?, extent)?)?);
    }
    sum(moments)
}

fn check_centre_of_mass(
    packed: &Json,
    packed_id: &Label,
    placements: &[Json],
    spec: &ContainerSpec,
    items: &Ordered<ItemSpec>,
    total_weight: i128,
    issues: &mut Vec<Issue>,
) -> Res<()> {
    let expected = if total_weight == 0 {
        0
    } else {
        let offset = |doubled: i128, side: i128| -> Res<i128> {
            let centred = mul(total_weight, side)?;
            floor_div(mul(abs(sub(doubled, centred)?)?, SCORE_SCALE)?, centred)
        };
        let x_ppm = offset(
            doubled_moment(placements, items, "x", "length")?,
            spec.inner[0],
        )?;
        let y_ppm = offset(
            doubled_moment(placements, items, "y", "width")?,
            spec.inner[1],
        )?;
        x_ppm.max(y_ppm)
    };
    let reported = packed.get_null("centre_of_mass_offset_ppm")?;
    if !reported.eq_int(expected) {
        issues.push(Issue::new(
            "centre_of_mass_mismatch",
            format!(
                "{packed_id}: reported {}, computed {expected}",
                py_str(reported)
            ),
        ));
    }
    Ok(())
}

fn check_axles(
    packed: &Json,
    packed_id: &Label,
    placements: &[Json],
    spec: &ContainerSpec,
    items: &Ordered<ItemSpec>,
    total_weight: i128,
    issues: &mut Vec<Issue>,
) -> Res<()> {
    let Some(axles) = &spec.axles else {
        if packed.has("axle_reactions") {
            issues.push(Issue::new(
                "axle_reaction_mismatch",
                format!("{packed_id}: reactions reported without configured axles"),
            ));
        }
        return Ok(());
    };
    let [front, rear] = axles.as_slice() else {
        return Err(super::MalformedResult::shape(format!(
            "{packed_id}: axles must name exactly a front and a rear axle"
        )));
    };
    let denominator = mul(2, sub(rear.position, front.position)?)?;
    let gross = add(spec.tare_weight, total_weight)?;
    let doubled_weighted_x = add(
        mul(spec.tare_weight, spec.inner[0])?,
        doubled_moment(placements, items, "x", "length")?,
    )?;
    let front_numerator = sub(mul(mul(2, gross)?, rear.position)?, doubled_weighted_x)?;
    let rear_numerator = sub(doubled_weighted_x, mul(mul(2, gross)?, front.position)?)?;
    let expected = Json::Dict(vec![
        ("basis".to_owned(), Json::str_of("gross")),
        ("denominator".to_owned(), Json::Str(denominator.to_string())),
        (
            "front_numerator".to_owned(),
            Json::Str(front_numerator.to_string()),
        ),
        (
            "rear_numerator".to_owned(),
            Json::Str(rear_numerator.to_string()),
        ),
    ]);
    let reported = packed.get_null("axle_reactions")?;
    if !reported.py_eq(&expected) {
        issues.push(Issue::new(
            "axle_reaction_mismatch",
            format!(
                "{packed_id}: reported {}, computed {}",
                py_str(reported),
                py_repr(&expected)
            ),
        ));
    }
    let over = |limit: Option<i128>, numerator: i128| -> Res<bool> {
        Ok(match limit {
            Some(limit) => numerator > mul(limit, denominator)?,
            None => false,
        })
    };
    if over(front.max_load, front_numerator)? || over(rear.max_load, rear_numerator)? {
        issues.push(Issue::new("axle_overloaded", packed_id.to_string()));
    }
    Ok(())
}

/// `mul_div`, used by the objective for the products that outgrow `i128`.
pub(crate) fn unused_ppm(volume: i128, used: i128) -> Res<i128> {
    mul_div(sub(volume, used)?, SCORE_SCALE, volume)
}
