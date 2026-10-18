//! What rests on what, and everything that follows from it: support, top load, stacking
//! limits, load propagation, crush, stack density and route unloading order.

use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap, HashSet};

use super::accounting::holds;
use super::container::{SCORE_SCALE, ratio_ppm};
use super::geometry::{Cuboid, Vertex, hull_volume, rotate_hull};
use super::json::{Json, Ordered, Res, sort_py};
use super::num::{Ratio, add, ceil_div, floor_div, mul, mul_div, sub};
use super::pyfmt::{fixed6, list, py_repr, py_str};
use super::spec::{EXIT_DIRECTIONS, ItemSpec, rotation_axes};
use super::units::{TICKS_PER_KG, TICKS_PER_MM};
use super::{Issue, Label};

/// One square metre in length ticks squared.
pub(crate) const SQUARE_METRE_TICKS: i128 = (TICKS_PER_MM * 1000) * (TICKS_PER_MM * 1000);

/// One placement as the container checks see it, keyed by its item id.
pub(crate) struct Entry<'a> {
    pub(crate) envelope: Cuboid,
    pub(crate) placement: &'a Json,
    pub(crate) spec: Option<&'a ItemSpec>,
}

pub(crate) type Envelopes<'a> = Ordered<Entry<'a>>;

/// A nested column: the item type, the exact footprint and the nesting height.
type ColumnKey = (String, [i128; 5]);

/// Index-based views of the envelopes, in their dict order.
pub(crate) struct Graph {
    /// Each item's direct supporters with the contact area, in deterministic order.
    pub(crate) supporters: Vec<Vec<(usize, i128)>>,
    /// The items resting directly on each item.
    pub(crate) direct_above: Vec<HashSet<usize>>,
}

pub(crate) fn entries<'e, 'a>(envelopes: &'e Envelopes<'a>) -> Vec<(&'e Json, &'e Entry<'a>)> {
    envelopes.iter().collect()
}

/// Exact direct-support edges. Ordinary face contacts keep placement order; nested instances
/// of one column are linked only to their adjacent predecessor, which replaces any shadowed
/// face contact from the same column.
pub(crate) fn support_graph(envelopes: &Envelopes<'_>) -> Res<Graph> {
    let list = entries(envelopes);
    let mut by_top: BTreeMap<i128, Vec<usize>> = BTreeMap::new();
    for (index, (_, entry)) in list.iter().enumerate() {
        by_top.entry(entry.envelope.z2).or_default().push(index);
    }
    let mut supporters: Vec<Vec<(usize, i128)>> = vec![Vec::new(); list.len()];
    for (upper, (_, entry)) in list.iter().enumerate() {
        for &lower in by_top
            .get(&entry.envelope.z)
            .map(Vec::as_slice)
            .unwrap_or(&[])
        {
            if lower == upper {
                continue;
            }
            let area = list[lower].1.envelope.overlap_area_xy(&entry.envelope);
            if area > 0 {
                supporters[upper].push((lower, area));
            }
        }
    }

    let mut group_keys: HashMap<usize, ColumnKey> = HashMap::new();
    let mut groups: Vec<(ColumnKey, Vec<usize>)> = Vec::new();
    let mut group_index: HashMap<ColumnKey, usize> = HashMap::new();
    for (index, (_, entry)) in list.iter().enumerate() {
        let Some(nesting) = entry.spec.and_then(|spec| spec.nesting_height) else {
            continue;
        };
        let envelope = &entry.envelope;
        let key = (
            entry.placement.at("item_type")?.hash_key()?,
            [envelope.x, envelope.y, envelope.x2, envelope.y2, nesting],
        );
        group_keys.insert(index, key.clone());
        match group_index.get(&key) {
            Some(&position) => groups[position].1.push(index),
            None => {
                group_index.insert(key.clone(), groups.len());
                groups.push((key, vec![index]));
            }
        }
    }
    let mut predecessors: Vec<(usize, usize)> = Vec::new();
    for (_, group) in &mut groups {
        group.sort_by_key(|index| (list[*index].1.envelope.z, *index));
        for pair in group.windows(2) {
            let (lower, upper) = (pair[0], pair[1]);
            let nesting = list[lower]
                .1
                .spec
                .and_then(|spec| spec.nesting_height)
                .unwrap_or(0);
            if sub(list[lower].1.envelope.z2, list[upper].1.envelope.z)? == nesting {
                predecessors.push((upper, lower));
            }
        }
    }
    for (upper, lower) in predecessors {
        let key = &group_keys[&upper];
        let mut edges: Vec<(usize, i128)> = supporters[upper]
            .iter()
            .copied()
            .filter(|(candidate, _)| group_keys.get(candidate) != Some(key))
            .collect();
        let position = edges
            .iter()
            .position(|(candidate, _)| *candidate > lower)
            .unwrap_or(edges.len());
        edges.insert(position, (lower, list[upper].1.envelope.base_area));
        supporters[upper] = edges;
    }

    let mut direct_above: Vec<HashSet<usize>> = vec![HashSet::new(); list.len()];
    for (upper, edges) in supporters.iter().enumerate() {
        for (lower, _) in edges {
            direct_above[*lower].insert(upper);
        }
    }
    Ok(Graph {
        supporters,
        direct_above,
    })
}

/// Exact `floor(base_area * ppm / 10^6)`, the least supported area the ratio allows.
fn required_support_area(base_area: i128, ppm: i128) -> Res<i128> {
    mul_div(base_area, ppm, SCORE_SCALE)
}

pub(crate) fn check_support_and_load(
    envelopes: &Envelopes<'_>,
    graph: &Graph,
    global_support: Ratio,
    issues: &mut Vec<Issue>,
) -> Res<()> {
    let list = entries(envelopes);
    for (index, (item_id, entry)) in list.iter().enumerate() {
        let item_id = py_str(item_id);
        let Some(spec) = entry.spec else { continue };
        let envelope = &entry.envelope;
        if envelope.z == 0 {
            continue;
        }
        let supporting = &graph.supporters[index];
        let required = if global_support.compare(spec.minimum_support_ratio)? == Ordering::Less {
            spec.minimum_support_ratio
        } else {
            global_support
        };
        check_ground_contact(
            &item_id,
            envelope,
            supporting,
            &list,
            &spec.ground_contact_rule,
            issues,
        );
        if required.compare_int(0)? == Ordering::Greater {
            let area: i128 = supporting.iter().map(|(_, area)| *area).try_fold(0, add)?;
            if area < required_support_area(envelope.base_area, ratio_ppm(required))? {
                let ratio = Ratio::new(area, envelope.base_area)?;
                issues.push(Issue::new(
                    "insufficient_support",
                    format!(
                        "{item_id}: {} < {}",
                        fixed6(ratio.to_f64()),
                        fixed6(required.to_f64())
                    ),
                ));
            }
        }
        for (lower, _) in supporting {
            let (lower_id, lower_entry) = list[*lower];
            let Some(lower_spec) = lower_entry.spec else {
                continue;
            };
            let lower_id = py_str(lower_id);
            if !lower_spec.stackable {
                issues.push(Issue::new(
                    "non_stackable",
                    format!("{item_id} rests on {lower_id}"),
                ));
            } else if !lower_spec.accepts_stacking_groups.is_empty()
                && !holds(&lower_spec.accepts_stacking_groups, &spec.stacking_group)?
            {
                issues.push(Issue::new(
                    "stacking_group_incompatible",
                    format!("{item_id} rests on {lower_id}"),
                ));
            }
            if let Some(limit) = lower_spec.max_top_load
                && spec.weight > limit
            {
                issues.push(Issue::new(
                    "top_load_exceeded",
                    format!(
                        "{item_id} ({} ticks) on {lower_id} (limit {limit})",
                        spec.weight
                    ),
                ));
            }
        }
    }

    for (index, (item_id, entry)) in list.iter().enumerate() {
        let Some(limit) = entry.spec.and_then(|spec| spec.max_stacked_items) else {
            continue;
        };
        let mut descendants: HashSet<usize> = HashSet::new();
        let mut pending: Vec<usize> = graph.direct_above[index].iter().copied().collect();
        while let Some(above) = pending.pop() {
            if descendants.insert(above) {
                pending.extend(graph.direct_above[above].iter().copied());
            }
        }
        if descendants.len() as i128 > limit {
            issues.push(Issue::new(
                "max_stacked_items_exceeded",
                format!(
                    "{}: {} above > limit {limit}",
                    py_str(item_id),
                    descendants.len()
                ),
            ));
        }
    }
    Ok(())
}

fn check_ground_contact(
    item_id: &str,
    envelope: &Cuboid,
    supporting: &[(usize, i128)],
    list: &[(&Json, &Entry<'_>)],
    rule: &Json,
    issues: &mut Vec<Issue>,
) {
    if rule.is_null() || rule.is_str("free") {
        return;
    }
    if rule.is_str("covered") {
        let corners = [
            (envelope.x, envelope.y),
            (envelope.x2, envelope.y),
            (envelope.x, envelope.y2),
            (envelope.x2, envelope.y2),
        ];
        let covered = corners.iter().all(|(x, y)| {
            supporting.iter().any(|(lower, _)| {
                let surface = &list[*lower].1.envelope;
                surface.x <= *x && *x <= surface.x2 && surface.y <= *y && *y <= surface.y2
            })
        });
        if !covered {
            issues.push(Issue::new(
                "ground_contact_violation",
                format!("{item_id}: covered requires all four base corners"),
            ));
        }
    } else if rule.is_str("single") && supporting.len() != 1 {
        issues.push(Issue::new(
            "ground_contact_violation",
            format!("{item_id}: single has {} supporters", supporting.len()),
        ));
    } else if rule.is_str("multiple") && supporting.len() < 2 {
        issues.push(Issue::new(
            "ground_contact_violation",
            format!("{item_id}: multiple has {} supporters", supporting.len()),
        ));
    }
}

/// Cumulative weight each item bears from everything resting on it, split across supporters by
/// contact area and conserved exactly (the remainder goes to the last supporter), top-down.
pub(crate) fn propagate_load(envelopes: &Envelopes<'_>, graph: &Graph) -> Res<Vec<i128>> {
    let list = entries(envelopes);
    let mut order: Vec<usize> = (0..list.len()).collect();
    let mut failure = None;
    order.sort_by(|&left, &right| {
        let (a, b) = (&list[left].1.envelope, &list[right].1.envelope);
        b.z2.cmp(&a.z2).then(b.z.cmp(&a.z)).then_with(|| {
            list[left].0.py_cmp(list[right].0).unwrap_or_else(|error| {
                failure = Some(error);
                Ordering::Equal
            })
        })
    });
    if let Some(error) = failure {
        return Err(error);
    }
    let mut loads = vec![0i128; list.len()];
    for upper in order {
        let Some(spec) = list[upper].1.spec else {
            continue;
        };
        let supports = &graph.supporters[upper];
        let total_area = supports.iter().map(|(_, area)| *area).try_fold(0, add)?;
        if total_area == 0 {
            continue;
        }
        let downward = add(spec.weight, loads[upper])?;
        let mut assigned = 0;
        for (position, (lower, area)) in supports.iter().enumerate() {
            let share = if position == supports.len() - 1 {
                sub(downward, assigned)?
            } else {
                floor_div(mul(downward, *area)?, total_area)?
            };
            assigned = add(assigned, share)?;
            loads[*lower] = add(loads[*lower], share)?;
        }
    }
    Ok(loads)
}

/// The pressure an item's footprint carries, in kPa, exactly:
/// `load * g * (ticks per metre)^2 / (ticks per kg * g denominator * Pa per kPa * area)`.
fn pressure(load: i128, base_area: i128) -> Res<Ratio> {
    let constant = Ratio::new(980_665 * SQUARE_METRE_TICKS, TICKS_PER_KG * 100_000 * 1_000)?;
    Ratio::from_int(load)
        .mul(constant)?
        .mul(Ratio::new(1, base_area)?)
}

/// No `compressible` item may carry more pressure than it declared.
pub(crate) fn check_crush(
    packed_id: &Label,
    envelopes: &Envelopes<'_>,
    loads: &[i128],
    issues: &mut Vec<Issue>,
) -> Res<()> {
    for (index, (item_id, entry)) in entries(envelopes).into_iter().enumerate() {
        let Some(limit) = entry
            .spec
            .and_then(|spec| spec.max_compression_pressure_kpa)
        else {
            continue;
        };
        let pressure = pressure(loads[index], entry.envelope.base_area)?;
        if pressure.compare_int(limit)? == Ordering::Greater {
            issues.push(Issue::new(
                "crush_violation",
                format!(
                    "{} in {packed_id}: {} kPa exceeds {limit} kPa",
                    py_str(item_id),
                    fixed6(pressure.to_f64())
                ),
            ));
        }
    }
    Ok(())
}

/// Occupied height under load, rounded up, never below one tick. A crushed item keeps its
/// height: `check_crush` already reports it.
pub(crate) fn compressed_height(envelope: &Cuboid, spec: &ItemSpec, load: i128) -> Res<i128> {
    let Some(limit) = spec.max_compression_pressure_kpa else {
        return Ok(envelope.height);
    };
    // The reference asserts the pair: a pressure limit without a ratio is not a request it reads.
    let ratio_ppm = spec.compression_ratio_ppm.ok_or_else(|| {
        super::MalformedResult::shape(format!(
            "{}: max_compression_pressure_kpa without compression_ratio",
            spec.id
        ))
    })?;
    let pressure = pressure(load, envelope.base_area)?;
    if pressure.compare_int(limit)? == Ordering::Greater || limit == 0 {
        return Ok(envelope.height);
    }
    let divisor = mul(mul(limit, 1_000_000)?, pressure.denominator)?;
    let retained = sub(divisor, mul(ratio_ppm, pressure.numerator)?)?;
    Ok(ceil_div(mul(envelope.height, retained)?, divisor)?.max(1))
}

/// Weight per square metre of an item's own base, at every level of the stack.
pub(crate) fn check_stack_density(
    packed_id: &Label,
    envelopes: &Envelopes<'_>,
    loads: &[i128],
    max_density: i128,
    issues: &mut Vec<Issue>,
) -> Res<()> {
    for (index, (item_id, entry)) in entries(envelopes).into_iter().enumerate() {
        let Some(spec) = entry.spec else { continue };
        let total = add(spec.weight, loads[index])?;
        if mul(total, SQUARE_METRE_TICKS)? > mul(max_density, entry.envelope.base_area)? {
            issues.push(Issue::new(
                "stack_density_exceeded",
                format!(
                    "{} in {packed_id}: {total} ticks over {} ticks^2 exceeds {max_density} ticks/m^2",
                    py_str(item_id),
                    entry.envelope.base_area,
                ),
            ));
        }
    }
    Ok(())
}

fn swept_region(envelope: &Cuboid, container: [i128; 3], direction: &str) -> Res<Cuboid> {
    let (mut x1, mut y1, mut z1) = (envelope.x, envelope.y, envelope.z);
    let (mut x2, mut y2, mut z2) = (envelope.x2, envelope.y2, envelope.z2);
    match direction {
        "+x" => (x1, x2) = (x2, container[0]),
        "-x" => (x1, x2) = (0, x1),
        "+y" => (y1, y2) = (y2, container[1]),
        "-y" => (y1, y2) = (0, y1),
        "+z" => (z1, z2) = (z2, container[2]),
        _ => (z1, z2) = (0, z1),
    }
    Cuboid::new(x1, y1, z1, sub(x2, x1)?, sub(y2, y1)?, sub(z2, z1)?)
}

fn has_clear_exit(
    item: usize,
    list: &[(&Json, &Entry<'_>)],
    present: &HashSet<usize>,
    container: [i128; 3],
    directions: &[&str],
) -> Res<bool> {
    for direction in directions {
        let region = swept_region(&list[item].1.envelope, container, direction)?;
        if !present
            .iter()
            .any(|other| *other != item && region.intersects(&list[*other].1.envelope))
        {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Items due at an earlier stop must all be removable -- unblocked
/// from above and slidable out a door (all six walls when the container names none) -- before
/// the route moves on.
pub(crate) fn check_route_removal_order(
    packed_id: &Label,
    envelopes: &Envelopes<'_>,
    graph: &Graph,
    container: [i128; 3],
    doors: &[&'static str],
    issues: &mut Vec<Issue>,
) -> Res<()> {
    let list = entries(envelopes);
    let stops: Vec<(usize, i128)> = list
        .iter()
        .enumerate()
        .filter_map(|(index, (_, entry))| {
            entry
                .spec
                .and_then(|spec| spec.stop_index)
                .map(|stop| (index, stop))
        })
        .collect();
    if stops.is_empty() {
        return Ok(());
    }
    let directions: &[&str] = if doors.is_empty() {
        &EXIT_DIRECTIONS
    } else {
        doors
    };
    let mut present: HashSet<usize> = (0..list.len()).collect();
    let mut distinct: Vec<i128> = stops.iter().map(|(_, stop)| *stop).collect();
    distinct.sort_unstable();
    distinct.dedup();
    for stop in distinct {
        let mut due: Vec<usize> = stops
            .iter()
            .filter(|(index, value)| *value == stop && present.contains(index))
            .map(|(index, _)| *index)
            .collect();
        while !due.is_empty() {
            let mut removable = Vec::new();
            for &item in &due {
                if graph.direct_above[item]
                    .iter()
                    .any(|above| present.contains(above))
                {
                    continue;
                }
                if has_clear_exit(item, &list, &present, container, directions)? {
                    removable.push(item);
                }
            }
            let Some(chosen) = minimum_by_id(&removable, &list)? else {
                let mut names: Vec<&Json> = due.iter().map(|index| list[*index].0).collect();
                sort_py(&mut names)?;
                issues.push(Issue::new(
                    "unloading_order_violation",
                    format!(
                        "{packed_id}: stop {stop} cannot fully unload {} before the route moves on",
                        list_of(&names),
                    ),
                ));
                return Ok(());
            };
            present.remove(&chosen);
            due.retain(|item| *item != chosen);
        }
    }
    Ok(())
}

fn list_of(names: &[&Json]) -> String {
    list(names.iter().map(|name| py_repr(name)))
}

fn minimum_by_id(candidates: &[usize], list: &[(&Json, &Entry<'_>)]) -> Res<Option<usize>> {
    let mut best: Option<usize> = None;
    for &candidate in candidates {
        best = match best {
            Some(current) if list[current].0.py_cmp(list[candidate].0)? != Ordering::Greater => {
                Some(current)
            }
            _ => Some(candidate),
        };
    }
    Ok(best)
}

/// Whether two stacked instances form a valid nested column: same type and footprint, and
/// the upper one sunk into the lower one by exactly the nesting height.
pub(crate) fn valid_nesting(left: &Entry<'_>, right: &Entry<'_>) -> Res<bool> {
    let (Some(left_spec), Some(right_spec)) = (left.spec, right.spec) else {
        return Ok(false);
    };
    let Some(nesting) = left_spec.nesting_height else {
        return Ok(false);
    };
    if left_spec.id != right_spec.id
        || !left
            .placement
            .at("item_type")?
            .py_eq(right.placement.at("item_type")?)
        || right_spec.nesting_height != Some(nesting)
    {
        return Ok(false);
    }
    let (a, b) = (&left.envelope, &right.envelope);
    if (a.x, a.y, a.x2, a.y2) != (b.x, b.y, b.x2, b.y2) {
        return Ok(false);
    }
    let (lower, upper) = if a.z <= b.z { (a, b) } else { (b, a) };
    Ok(lower.z != upper.z && sub(lower.z2, upper.z)? == nesting)
}

/// Space a placement actually takes: its compressed box, its hull, or its box.
pub(crate) fn occupied_volume(
    entry_envelope: &Cuboid,
    placement: &Json,
    spec: Option<&ItemSpec>,
    load: i128,
) -> Res<i128> {
    if let Some(spec) = spec {
        if spec.max_compression_pressure_kpa.is_some() {
            return mul(
                entry_envelope.base_area,
                compressed_height(entry_envelope, spec, load)?,
            );
        }
        if spec.is_convex_hull()
            && let Some(vertices) = &spec.hull_vertices
        {
            let axes = rotation_axes(placement.at("orientation")?.text("orientation")?)?;
            return hull_volume(&rotate_hull(vertices, axes)?);
        }
    }
    let solid = placed_hull(entry_envelope, placement, spec)?;
    if solid == entry_envelope.vertices() {
        let dimensions = placement.at("dimensions")?;
        let side = |name: &str| -> Res<i128> {
            dimensions
                .at(name)?
                .at("ticks")?
                .int("a placement dimension")
        };
        return mul(mul(side("length")?, side("width")?)?, side("height")?);
    }
    hull_volume(&solid)
}

/// The solid a placement occupies, as vertices in container coordinates: the exact hull when
/// the request allows it and the reported box matches, otherwise the envelope's corners.
pub(crate) fn placed_hull(
    envelope: &Cuboid,
    placement: &Json,
    spec: Option<&ItemSpec>,
) -> Res<Vec<Vertex>> {
    let Some(spec) = spec else {
        return Ok(envelope.vertices());
    };
    let Some(vertices) = &spec.hull_vertices else {
        return Ok(envelope.vertices());
    };
    if !spec.is_convex_hull() || spec.stop_index.is_some() {
        return Ok(envelope.vertices());
    }
    let axes = rotation_axes(placement.at("orientation")?.text("orientation")?)?;
    let rotated = [
        spec.dimensions[axes[0]],
        spec.dimensions[axes[1]],
        spec.dimensions[axes[2]],
    ];
    if [envelope.length, envelope.width, envelope.height] != rotated {
        return Ok(envelope.vertices());
    }
    rotate_hull(vertices, axes)?
        .into_iter()
        .map(|[x, y, z]| {
            Ok([
                add(envelope.x, x)?,
                add(envelope.y, y)?,
                add(envelope.z, z)?,
            ])
        })
        .collect()
}
