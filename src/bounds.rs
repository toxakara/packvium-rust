//! Lower bounds on the objective vector, and what a result states against them
//!.
//!
//! The mathematics is fixed by `docs/OPTIMALITY-CERTIFICATES.md`. This is an independent
//! implementation written from that document, not a transcription of the Python or PHP
//! ports, and `conformance/scene/objective-bounds.json` holds all of them to the same vectors
//! on the cases drawn from the golden corpus and on the edges of the one refusal predicate.
//!
//! A bound is a function of the *request*, so there is no room for a legitimately different
//! answer between engines, even though Rust and JavaScript are not held to placement
//! equality: this port is held to equality because equality is both achievable and strictly
//! stronger than soundness.
//!
//! ## Arithmetic
//!
//! `i128` throughout, and it is not close. A one-metre cube is 4.1e21 cubic ticks, a full
//! container load a few orders above that, and the widest intermediate here multiplies a
//! summed volume by 1e6 -- at most 1e36 once the declared ceiling has admitted the sums,
//! against an `i128` ceiling of 1.7e38. PHP needs decimal strings for the same quantities
//! and JavaScript needs `BigInt`; this port needs neither.
//!
//! ## Complexity
//!
//! Volumes and weights are carried as runs -- each distinct value with how many instances
//! carry it -- formed per item type, so a quantity of a million costs one entry rather than a
//! million cloned instances. `O(t log t + c log c)` time and `O(t + c)` space for `t` item
//! types and `c` container types: one sort of the volume runs, one of the weight runs, one of
//! the container types by cost. With every quantity 1 that is the document's
//! `O(n log n + c log c)`. No geometry is touched and no candidate position is generated,
//! which is why it is computed once per pack, before any solver runs.

use crate::geometry::ShapeType;
use crate::model::{Container, Item, ItemInstance, PackingRequest};
use serde_json::Value;
use std::collections::BTreeMap;

/// Parts per million, the scale keys 3 and 4 of the objective are carried at.
pub const PPM: i128 = 1_000_000;

/// Every sum in the bound path must stay below this.
///
/// Declared rather than inherited from the language. Python's integers are unbounded, PHP's
/// silently become doubles on overflow, JavaScript's `Number` stops being exact past `2^53`
/// and this port's `i128` wraps -- so if each engine refused at its own limit, the four would
/// disagree about which requests are answerable. Keys 3 and 4 multiply a summed volume by
/// `PPM`, so `10^30 * 10^6 = 10^36` sits about 170-fold inside `i128`.
pub const MAX_BOUND_SUM: i128 = 1_000_000_000_000_000_000_000_000_000_000;

/// Largest integer every binding can return without changing its value.
///
/// Intermediate arithmetic keeps the wider ceiling above. Results cross JSON and the
/// JavaScript `Number` boundary, so the five keys use the common exact range instead.
pub const MAX_BOUND_VALUE: i128 = 9_007_199_254_740_991;

/// A sum in the bound path exceeded the declared ceiling.
///
/// A structured refusal rather than a number, because the alternative is the failure
/// Baldacci et al. document for floating-point bin-packing solvers: a bound that is quietly
/// wrong and carries no signal that it is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BoundOverflow(pub String);

impl std::fmt::Display for BoundOverflow {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

fn wrapped(quantity: &str) -> BoundOverflow {
    BoundOverflow(format!("{quantity} overflows the exact range"))
}

fn guard(total: i128, quantity: &str) -> Result<i128, BoundOverflow> {
    if total > MAX_BOUND_SUM {
        return Err(BoundOverflow(format!(
            "{quantity} sums past the {MAX_BOUND_SUM} ceiling the bound path declares"
        )));
    }
    Ok(total)
}

fn guard_output(value: i128, quantity: &str) -> Result<i128, BoundOverflow> {
    if value > MAX_BOUND_VALUE {
        return Err(BoundOverflow(format!(
            "{quantity} bound is {value}, above the {MAX_BOUND_VALUE} exact portable result ceiling"
        )));
    }
    Ok(value)
}

/// The five lower bounds, in the default objective's key order.
///
/// Each is conditional on the keys before it: `container_count` is the least number of
/// containers *given* that `unpacked_count` items were left behind, and so on down. That is
/// what makes them comparable to a score vector key by key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Bounds {
    pub unpacked_count: i128,
    pub container_count: i128,
    pub total_cost_minor: i128,
    pub unused_volume_ppm: i128,
    pub stack_height_ppm: i128,
}

impl Bounds {
    pub fn as_vector(&self) -> [i128; 5] {
        [
            self.unpacked_count,
            self.container_count,
            self.total_cost_minor,
            self.unused_volume_ppm,
            self.stack_height_ppm,
        ]
    }
}

/// One instance reduced to what the bounds actually consume.
#[derive(Clone, Copy, Debug)]
pub struct Instance {
    pub volume: i128,
    pub weight: i128,
    /// Whether this instance can occupy less room than its declared dimensions.
    pub shrinks: bool,
}

/// One container type, with `None` wherever the request declares no limit.
#[derive(Clone, Copy, Debug)]
pub struct ContainerType {
    pub usable: i128,
    pub inner: i128,
    pub base_area: i128,
    pub height: i128,
    pub payload: Option<i128>,
    pub max_items: Option<i128>,
    pub quantity: Option<i128>,
    pub cost_minor: i128,
}

impl ContainerType {
    fn of(container: &Container) -> Self {
        let inner = container.inner_dimensions.volume();
        Self {
            usable: inner - inner * container.void_fill_reserve_ppm / PPM,
            inner,
            base_area: container.inner_dimensions.base_area(),
            height: i128::from(container.inner_dimensions.height.0),
            payload: container.max_payload.map(|weight| i128::from(weight.0)),
            max_items: container.max_items.map(|count| count as i128),
            quantity: container.quantity.map(|count| count as i128),
            cost_minor: i128::from(container.cost_minor),
        }
    }
}

/// Values sorted ascending, each with how many instances carry it.
type Runs = Vec<(i128, i128)>;

fn runs(pairs: impl Iterator<Item = (i128, i128)>) -> Runs {
    let mut runs: Runs = pairs.filter(|&(_, count)| count > 0).collect();
    runs.sort_unstable();
    runs
}

/// Can this item take up less room than its declared dimensions?
///
/// Three ways, and each breaks the same argument -- that nominal volumes sum to something a
/// solution must carry. A nested item sinks into the one below it; a `convex_hull` occupies
/// its hull and leaves the rest of its bounding box free; a `compressible` item gives up
/// height under load.
///
/// The design document named only the first until found the omission with a
/// soundness test over the corpus. Asking the question once, here, is what stops a future
/// fourth shape from reintroducing the same unsoundness silently.
pub fn occupies_less_than_its_box(item: &Item) -> bool {
    item.nesting_height.is_some()
        || matches!(
            item.shape_type,
            ShapeType::ConvexHull | ShapeType::Compressible
        )
}

/// Where each covered objective reads the five bounds: the position in the default-order
/// vector of each of the objective's own score keys. Any other objective has no bound.
fn objective_order(objective: &str) -> Option<[usize; 5]> {
    match objective {
        "default" => Some([0, 1, 2, 3, 4]),
        "lowest_cost" => Some([0, 2, 1, 3, 4]),
        _ => None,
    }
}

/// The request's bound in its objective's own score order, or `None` where none is stated.
///
/// Over every instance the request declares, fixed placements included (they are part of
/// their item's quantity), and every container type. `None` for an objective with no bound,
/// for a request with a `made_to_measure` container (D13: the unused-volume key would
/// be taken from the machine maximum while the score uses the fabricated box), and for a
/// request past a declared ceiling, which withholds the bound and nothing else
/// (OPTIMALITY-GAP.md § 1.5). Computed from the request alone, independently of the solver
/// and deadline; emission also checks the result's score and the portable gap ceiling.
pub fn for_request(request: &PackingRequest) -> Option<Vec<i128>> {
    let order = objective_order(&request.config.objective)?;
    if request
        .containers
        .iter()
        .any(|container| container.made_to_measure.is_some())
    {
        return None;
    }
    let vector = from_items(&request.items, &request.containers)
        .ok()?
        .as_vector();
    Some(order.iter().map(|&position| vector[position]).collect())
}

/// Every bound for one request, from its item types and container types.
pub fn from_items(items: &[Item], containers: &[Container]) -> Result<Bounds, BoundOverflow> {
    let counted = || {
        items
            .iter()
            .map(|item| (item, item.quantity as i128))
            .filter(|&(_, count)| count > 0)
    };
    let volumes = runs(counted().map(|(item, count)| (item.dimensions.volume(), count)));
    let weights = runs(counted().map(|(item, count)| (i128::from(item.weight.0), count)));
    let shrinks = counted().any(|(item, _)| occupies_less_than_its_box(item));
    let types: Vec<ContainerType> = containers.iter().map(ContainerType::of).collect();
    from_runs(&volumes, &weights, shrinks, &types)
}

/// Every bound for one request, from the engine's own instances.
pub fn compute(
    instances: &[ItemInstance],
    containers: &[Container],
) -> Result<Bounds, BoundOverflow> {
    let reduced: Vec<Instance> = instances
        .iter()
        .map(|instance| Instance {
            volume: instance.item.dimensions.volume(),
            weight: i128::from(instance.item.weight.0),
            shrinks: occupies_less_than_its_box(&instance.item),
        })
        .collect();
    let types: Vec<ContainerType> = containers.iter().map(ContainerType::of).collect();
    from_reduced(&reduced, &types)
}

/// The same bounds, from the numbers the formulas consume.
///
/// This is the shape `conformance/scene/objective-bounds.json` records, so this port can be
/// held to Python's vectors without first reimplementing a request parser. It takes
/// `shrinks` as given rather than deriving it; whether this engine decides that flag
/// correctly is asserted separately, against engine objects, so that a port cannot pass the
/// corpus check by consuming a flag it never computes.
pub fn from_reduced(
    instances: &[Instance],
    containers: &[ContainerType],
) -> Result<Bounds, BoundOverflow> {
    let volumes = runs(instances.iter().map(|instance| (instance.volume, 1)));
    let weights = runs(instances.iter().map(|instance| (instance.weight, 1)));
    let shrinks = instances.iter().any(|instance| instance.shrinks);
    from_runs(&volumes, &weights, shrinks, containers)
}

/// The one refusal predicate of docs/OPTIMALITY-CERTIFICATES.md, then the five formulas.
///
/// Refused exactly where a sum the formulas form passes `MAX_BOUND_SUM` -- the instance
/// volumes, the instance weights, and each *finite* capacity total -- or a bound passes
/// `MAX_BOUND_VALUE`. An unbounded capacity total is never formed, so it is never guarded,
/// and nothing the formulas never compute (a single type's capacity, a cost no selection
/// reaches) is refused (finding F4). Every later product is bounded by the guarded totals
/// times `PPM`, which is what makes the rest of the arithmetic safe by derivation.
fn from_runs(
    volumes: &[(i128, i128)],
    weights: &[(i128, i128)],
    shrinks: bool,
    containers: &[ContainerType],
) -> Result<Bounds, BoundOverflow> {
    guard(run_total(volumes, "instance volume")?, "instance volume")?;
    guard(run_total(weights, "instance weight")?, "instance weight")?;
    let volume_capacity = capacity_total(containers, |c| Some(c.usable))?;
    let payload_capacity = capacity_total(containers, |c| c.payload)?;
    let slot_capacity = capacity_total(containers, |c| c.max_items)?;
    let count: i128 = volumes.iter().map(|&(_, run)| run).sum();

    let mut placeable = count;
    if !shrinks {
        placeable = placeable.min(fit(volumes, volume_capacity));
    }
    placeable = placeable.min(fit(weights, payload_capacity));
    if let Some(slots) = slot_capacity {
        placeable = placeable.min(slots);
    }
    let unpacked = count - placeable;
    let opened = container_bound(volumes, weights, shrinks, placeable, containers);
    let cost = cost_bound(containers, opened)?;
    let unused = unused_volume_bound(volumes, shrinks, placeable, containers, opened);
    let height = stack_height_bound(volumes, shrinks, placeable, containers, opened);
    Ok(Bounds {
        unpacked_count: guard_output(unpacked, "unpacked count")?,
        container_count: guard_output(opened, "container count")?,
        total_cost_minor: guard_output(cost, "opening cost")?,
        unused_volume_ppm: guard_output(unused, "unused volume")?,
        stack_height_ppm: guard_output(height, "stack height")?,
    })
}

/// `Σ value · count` over the runs, refusing a product or sum that would wrap `i128`; a sum
/// that large is far past the ceiling anyway, so both roads lead to the same refusal.
fn run_total(runs: &[(i128, i128)], quantity: &str) -> Result<i128, BoundOverflow> {
    runs.iter().try_fold(0i128, |total, &(value, count)| {
        value
            .checked_mul(count)
            .and_then(|product| total.checked_add(product))
            .ok_or_else(|| wrapped(quantity))
    })
}

/// `Σc limit · quantity`, guarded, or `None` when the total is unbounded: some type declares
/// no limit, or an unlimited quantity of a type whose limit is positive. A type that holds
/// nothing adds nothing however many of it there are. Unboundedness is decided before
/// anything is summed, so whether a request is refused never depends on type order.
fn capacity_total(
    containers: &[ContainerType],
    limit: fn(&ContainerType) -> Option<i128>,
) -> Result<Option<i128>, BoundOverflow> {
    let unbounded = containers.iter().any(|container| match limit(container) {
        None => true,
        Some(value) => container.quantity.is_none() && value > 0,
    });
    if unbounded {
        return Ok(None);
    }
    let mut total: i128 = 0;
    for container in containers {
        let (Some(value), Some(quantity)) = (limit(container), container.quantity) else {
            continue;
        };
        total = value
            .checked_mul(quantity)
            .and_then(|product| total.checked_add(product))
            .ok_or_else(|| wrapped("container capacity"))?;
    }
    guard(total, "container capacity").map(Some)
}

/// `L1`: grant every container the largest capacity available, which can only understate how
/// many are needed.
fn container_bound(
    volumes: &[(i128, i128)],
    weights: &[(i128, i128)],
    shrinks: bool,
    placed: i128,
    containers: &[ContainerType],
) -> i128 {
    if placed <= 0 || containers.is_empty() {
        return 0;
    }
    let mut bound: i128 = 1;
    if !shrinks {
        let largest = containers.iter().map(|c| c.usable).max().unwrap_or(0);
        if largest > 0 {
            bound = bound.max(ceil_div(smallest(volumes, placed), largest));
        }
    }
    if let Some(largest) = finite_max(containers, |c| c.payload)
        && largest > 0
    {
        bound = bound.max(ceil_div(smallest(weights, placed), largest));
    }
    if let Some(largest) = finite_max(containers, |c| c.max_items)
        && largest > 0
    {
        bound = bound.max(ceil_div(placed, largest));
    }
    bound
}

/// `L2`: at least `L1` containers open, each costing at least the cheapest the inventory
/// still holds.
///
/// The types are sorted by cost once and taken greedily, `min(quantity, still needed)` of
/// each, which is the sum of the `L1` cheapest entries of the per-unit multiset without
/// building it: `O(c log c)` rather than the `O(c · L1 log(c · L1))` of materialising up to
/// `L1` copies of every type and sorting those (finding F6).
fn cost_bound(containers: &[ContainerType], opened: i128) -> Result<i128, BoundOverflow> {
    let mut by_cost: Vec<&ContainerType> = containers.iter().collect();
    by_cost.sort_unstable_by_key(|container| container.cost_minor);
    let mut needed = opened;
    let mut total: i128 = 0;
    for container in by_cost {
        if needed <= 0 {
            break;
        }
        let taken = container
            .quantity
            .map_or(needed, |quantity| quantity.min(needed));
        total = container
            .cost_minor
            .checked_mul(taken)
            .and_then(|product| total.checked_add(product))
            .ok_or_else(|| wrapped("opening cost"))?;
        needed -= taken;
    }
    guard(total, "opening cost")
}

/// `L3`: the fill is largest when every container is the smallest available and holds the
/// greatest volume that could be placed at all.
///
/// Key 3 sums a *per-container* ratio, so a tight bound would have to know which items went
/// where -- that is the packing problem, not a relaxation of it. The `L1 - 1` term is the
/// exact worst case by which summing `k` ceilings can exceed the ceiling of the sum.
fn unused_volume_bound(
    volumes: &[(i128, i128)],
    shrinks: bool,
    placed: i128,
    containers: &[ContainerType],
    opened: i128,
) -> i128 {
    if shrinks || opened <= 0 || containers.is_empty() {
        return 0;
    }
    let smallest_inner = containers.iter().map(|c| c.inner).min().unwrap_or(0);
    if smallest_inner <= 0 {
        return 0;
    }
    let filled = ceil_div(largest(volumes, placed) * PPM, smallest_inner);
    (opened * PPM - filled - (opened - 1)).max(0)
}

/// `L4`: the volume that must be placed has to stand at least as tall as itself spread across
/// the widest floor available, in the tallest container available.
///
/// The `L1 - 1` correction is the exact worst-case difference between a sum of floors and the
/// floor of a sum, not a cautionary fudge: the objective floors each container's ratio before
/// summing.
fn stack_height_bound(
    volumes: &[(i128, i128)],
    shrinks: bool,
    placed: i128,
    containers: &[ContainerType],
    opened: i128,
) -> i128 {
    if shrinks || opened <= 0 || containers.is_empty() {
        return 0;
    }
    let widest = containers.iter().map(|c| c.base_area).max().unwrap_or(0);
    let tallest = containers.iter().map(|c| c.height).max().unwrap_or(0);
    if widest <= 0 || tallest <= 0 {
        return 0;
    }
    let required = ceil_div(smallest(volumes, placed), widest);
    (required * PPM / tallest - (opened - 1)).max(0)
}

/// The largest declared limit, or `None` if any type declares none.
///
/// One unlimited type makes the maximum unbounded and every term conditioned on it vacuous,
/// which is why this collapses to `None` rather than ignoring the gap.
fn finite_max(
    containers: &[ContainerType],
    limit: fn(&ContainerType) -> Option<i128>,
) -> Option<i128> {
    let mut best: Option<i128> = None;
    for container in containers {
        let value = limit(container)?;
        best = Some(best.map_or(value, |current: i128| current.max(value)));
    }
    best
}

/// The largest `n` such that the `n` smallest values sum to at most `capacity`; all of them
/// when the capacity is unbounded.
///
/// Smallest first, and that is the whole soundness argument: taking the cheapest units
/// maximises how many fit under one capacity, so this over-estimates what any real packing
/// achieves. Geometry, support ratios, stacking rules, incompatible tags and route order can
/// each make the real answer worse and none of them can make it better.
fn fit(ascending: &[(i128, i128)], capacity: Option<i128>) -> i128 {
    let total: i128 = ascending.iter().map(|&(_, count)| count).sum();
    let Some(capacity) = capacity else {
        return total;
    };
    let (mut taken, mut used) = (0i128, 0i128);
    for &(value, count) in ascending {
        let room = if value == 0 {
            count
        } else {
            count.min((capacity - used).max(0) / value)
        };
        taken += room;
        used += value * room;
        if room < count {
            break;
        }
    }
    taken
}

/// The sum of the `count` smallest values.
fn smallest(ascending: &[(i128, i128)], count: i128) -> i128 {
    take(ascending.iter(), count)
}

/// The sum of the `count` largest values.
fn largest(ascending: &[(i128, i128)], count: i128) -> i128 {
    take(ascending.iter().rev(), count)
}

fn take<'a>(runs: impl Iterator<Item = &'a (i128, i128)>, mut count: i128) -> i128 {
    let mut total = 0;
    for &(value, available) in runs {
        if count <= 0 {
            break;
        }
        let taken = available.min(count);
        total += value * taken;
        count -= taken;
    }
    total
}

fn ceil_div(numerator: i128, denominator: i128) -> i128 {
    (numerator + denominator - 1) / denominator
}

/// What one result states against its request's bound (OPTIMALITY-GAP.md § 1.1).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Statement {
    /// The bound through the first missed position, or all of it when nothing is missed.
    pub lower_bound: Vec<i128>,
    /// The first missed position and `score[k] - bound[k]` there; `None` when attained.
    pub gap: Option<(usize, i128)>,
}

impl Statement {
    /// Every position attained: with nothing unpacked, a proof of optimality.
    pub fn attained(&self) -> bool {
        self.gap.is_none()
    }

    /// The result's `optimality` attributes, beside its `code`. `relative_gap` is unreduced,
    /// so the pair reads as `(score - bound, bound)` by eye, and absent when the bound on the
    /// missed position is zero.
    pub fn attributes(&self) -> BTreeMap<String, Value> {
        let mut attributes = BTreeMap::new();
        attributes.insert(
            "lower_bound".into(),
            Value::Array(self.lower_bound.iter().map(|&value| exact(value)).collect()),
        );
        if let Some((key, absolute)) = self.gap {
            attributes.insert("gap_key".into(), Value::from(key));
            attributes.insert("absolute_gap".into(), exact(absolute));
            let least = self.lower_bound[key];
            if least > 0 {
                attributes.insert(
                    "relative_gap".into(),
                    serde_json::json!({"numerator": exact(absolute), "denominator": exact(least)}),
                );
            }
        }
        attributes
    }
}

/// A bound value as a JSON integer. Every one is inside `MAX_BOUND_VALUE` by the output
/// guard, and every gap is at most its score component.
fn exact(value: i128) -> Value {
    Value::from(i64::try_from(value).unwrap_or(i64::MAX))
}

/// The statement one score makes against `bound`, both in the result's own score order, or
/// `None` where a score below its bound shows the bound wrong: an engine must not publish a
/// claim it has just refuted (OPTIMALITY-GAP.md § 1.5). At most five comparisons, `O(1)`.
pub fn statement(score: &[i128], bound: &[i128]) -> Option<Statement> {
    for (index, (&achieved, &least)) in score.iter().zip(bound).enumerate() {
        if achieved < least {
            return None;
        }
        if achieved > least {
            if achieved - least > MAX_BOUND_VALUE {
                return None;
            }
            return Some(Statement {
                lower_bound: bound[..=index].to_vec(),
                gap: Some((index, achieved - least)),
            });
        }
    }
    Some(Statement {
        lower_bound: bound.to_vec(),
        gap: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every default-objective fixture in the golden corpus, as Python computed it.
    ///
    /// asks only that this engine's bound never exceed the achieved objective. It is
    /// held to the stronger claim because the stronger claim is true: a bound is a function
    /// of the request, so Rust and Python disagreeing about one would be a defect in one of
    /// them rather than the permitted freedom in how these two engines place items.
    #[test]
    fn every_corpus_case_matches_python_exactly() {
        // A cross-language fixture kept one level above this crate; a published copy
        // does not carry it.
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../../../../conformance/scene/objective-bounds.json");
        let Ok(text) = std::fs::read_to_string(&path) else {
            eprintln!(
                "skipping: the shared cross-language scene fixture is not part of this package"
            );
            return;
        };
        let document: serde_json::Value = serde_json::from_str(&text).expect("valid scene");
        assert_eq!(document["format"], "packvium-objective-bounds/v1");
        let cases = document["cases"].as_array().expect("scene cases");
        assert!(
            cases.len() > 300,
            "a scene that quietly emptied itself would make every case below vacuous"
        );

        for case in cases {
            let (instances, containers) = scene_inputs(case);
            let expected: Vec<i128> = case["bounds"]
                .as_array()
                .expect("bounds")
                .iter()
                .map(|value| i128::from(value.as_i64().expect("integer bound")))
                .collect();
            let computed = from_reduced(&instances, &containers)
                .expect("a corpus case stays inside the ceiling")
                .as_vector();
            assert_eq!(
                computed.to_vec(),
                expected,
                "bounds diverge from Python on {}",
                case["fixture"]
            );
        }

        // The edges are written from the oracle: the fixed-placement rule and every edge of
        // the one refusal predicate, where `null` means this port must refuse too (F4).
        let edges = document["edges"].as_array().expect("scene edges");
        assert!(
            !edges.is_empty(),
            "the refusal predicate's edges are not optional"
        );
        for edge in edges {
            let (instances, containers) = scene_inputs(edge);
            let computed = from_reduced(&instances, &containers).map(|b| b.as_vector().to_vec());
            let expected = edge["bounds"].as_array().map(|values| {
                values
                    .iter()
                    .map(|value| i128::from(value.as_i64().expect("integer bound")))
                    .collect::<Vec<_>>()
            });
            assert_eq!(computed.ok(), expected, "edge {:?}", edge["name"]);
        }
    }

    fn decimal(value: &serde_json::Value) -> i128 {
        value
            .as_str()
            .expect("a decimal string")
            .parse()
            .expect("an integer")
    }

    fn scene_inputs(case: &serde_json::Value) -> (Vec<Instance>, Vec<ContainerType>) {
        let instances = case["instances"]
            .as_array()
            .expect("instances")
            .iter()
            .map(|raw| Instance {
                volume: decimal(&raw["volume"]),
                weight: raw["weight"].as_i64().expect("weight ticks").into(),
                shrinks: raw["shrinks"].as_bool().expect("shrink flag"),
            })
            .collect();
        let containers = case["containers"]
            .as_array()
            .expect("containers")
            .iter()
            .map(|raw| ContainerType {
                usable: decimal(&raw["usable"]),
                inner: decimal(&raw["inner"]),
                base_area: raw["base_area"].as_i64().expect("base area").into(),
                height: raw["height"].as_i64().expect("height").into(),
                payload: raw["payload"].as_i64().map(i128::from),
                max_items: raw["max_items"].as_i64().map(i128::from),
                quantity: raw["quantity"].as_i64().map(i128::from),
                cost_minor: raw["cost_minor"].as_i64().expect("cost").into(),
            })
            .collect();
        (instances, containers)
    }

    /// The statement a result makes: a prefix through the first miss, an unreduced relative
    /// gap that is absent on a zero bound, and nothing at all for a score below its bound.
    #[test]
    fn a_statement_stops_at_the_first_missed_position() {
        let bound = [0, 2, 10, 500, 7];
        let attained = statement(&bound, &bound).expect("attained");
        assert!(attained.attained());
        assert_eq!(
            attained.attributes().keys().collect::<Vec<_>>(),
            ["lower_bound"]
        );

        let missed = statement(&[0, 2, 14, 0, 0], &bound).expect("missed on key 2");
        assert_eq!(missed.lower_bound, vec![0, 2, 10]);
        assert_eq!(missed.gap, Some((2, 4)));
        assert_eq!(
            missed.attributes()["relative_gap"],
            serde_json::json!({"numerator": 4, "denominator": 10})
        );

        let from_zero = statement(&[1, 0, 0, 0, 0], &bound).expect("missed on key 0");
        assert_eq!(from_zero.gap, Some((0, 1)));
        assert!(!from_zero.attributes().contains_key("relative_gap"));

        assert_eq!(statement(&[0, 1, 99, 0, 0], &bound), None);
    }

    /// The request-level entry point: `lowest_cost` reads the permuted vector, an uncovered
    /// objective and a made-to-measure container read none.
    #[test]
    fn a_request_states_its_bound_in_its_objective_order() {
        use crate::geometry::Dimensions;
        use crate::units::Length;
        let item = Item::new(
            "a",
            Dimensions {
                length: Length(10),
                width: Length(10),
                height: Length(5),
            },
            crate::units::Weight(0),
            1,
        );
        let mut request = PackingRequest {
            items: vec![item],
            containers: vec![Container::new(
                "box",
                Dimensions {
                    length: Length(10),
                    width: Length(10),
                    height: Length(10),
                },
            )],
            config: Default::default(),
            output_length_unit: "mm".into(),
            output_weight_unit: "g".into(),
            catalog_versions_used: Vec::new(),
            fixed_placements: Vec::new(),
            fixed_containers: Vec::new(),
        };
        request.containers[0].cost_minor = 3;
        assert_eq!(for_request(&request), Some(vec![0, 1, 3, 500_000, 500_000]));
        request.config.objective = "lowest_cost".into();
        assert_eq!(for_request(&request), Some(vec![0, 3, 1, 500_000, 500_000]));
        request.config.objective = "maximum_value".into();
        assert_eq!(for_request(&request), None);
    }

    /// The five cases the document works by hand.
    ///
    /// Carried here as well as in the corpus scene because they are the ones a reader can
    /// check without running anything, and they pin boundaries the corpus happens not to
    /// contain: a perfect fill, an exhausted cheap inventory, and no container at all.
    #[test]
    fn the_worked_examples_from_the_document() {
        let instance = |volume: i128, weight: i128, shrinks: bool| Instance {
            volume,
            weight,
            shrinks,
        };
        let container = |inner: i128,
                         base_area: i128,
                         height: i128,
                         cost_minor: i128,
                         payload: Option<i128>,
                         quantity: Option<i128>| ContainerType {
            usable: inner,
            inner,
            base_area,
            height,
            payload,
            max_items: None,
            quantity,
            cost_minor,
        };

        assert_eq!(
            from_reduced(
                &[instance(125, 1, false); 8],
                &[container(1000, 100, 10, 7, None, Some(4))]
            )
            .expect("inside the declared ceiling")
            .as_vector(),
            [0, 1, 7, 0, 1_000_000]
        );

        // Volume says all ten fit; the payload ceiling says four do.
        assert_eq!(
            from_reduced(
                &[instance(1, 30, false); 10],
                &[container(1000, 100, 10, 0, Some(120), Some(1))]
            )
            .expect("inside the declared ceiling")
            .as_vector(),
            [6, 1, 0, 996_000, 100_000]
        );

        // Two containers are unavoidable and the cheap one is out of stock after the first.
        assert_eq!(
            from_reduced(
                &[instance(600, 1, false); 2],
                &[
                    container(1000, 100, 10, 5, None, Some(1)),
                    container(1000, 100, 10, 9, None, Some(3)),
                ]
            )
            .expect("inside the declared ceiling")
            .as_vector(),
            [0, 2, 14, 799_999, 1_199_999]
        );

        // An instance that occupies less than its box drops the volume argument entirely.
        assert_eq!(
            from_reduced(
                &[instance(900, 1, true); 5],
                &[container(1000, 100, 10, 0, None, Some(1))]
            )
            .expect("inside the declared ceiling")
            .as_vector(),
            [0, 1, 0, 0, 0]
        );

        // One item, no containers: no division by a capacity that does not exist.
        assert_eq!(
            from_reduced(&[instance(10, 1, false)], &[])
                .expect("inside the declared ceiling")
                .as_vector(),
            [1, 0, 0, 0, 0]
        );
    }

    /// A request past the declared ceiling is refused rather than answered.
    ///
    /// The ceiling is declared, not inherited: this port could carry a good deal more in
    /// `i128` than PHP can in a native integer or JavaScript can in a `Number`. Refusing at
    /// the same declared point is what keeps the four engines agreeing about which requests
    /// are answerable at all -- the alternative is a caller getting a number from one engine
    /// and a refusal from another for the same input.
    #[test]
    fn a_sum_past_the_declared_ceiling_is_refused_rather_than_answered() {
        let container = ContainerType {
            usable: 1000,
            inner: 1000,
            base_area: 100,
            height: 10,
            payload: None,
            max_items: None,
            quantity: Some(1),
            cost_minor: 0,
        };
        let inside = Instance {
            volume: MAX_BOUND_SUM / 4,
            weight: 1,
            shrinks: false,
        };
        assert!(
            from_reduced(&[inside; 3], &[container]).is_ok(),
            "three quarters of the ceiling is inside it"
        );

        let over = Instance {
            volume: MAX_BOUND_SUM / 2,
            weight: 1,
            shrinks: false,
        };
        let refusal = from_reduced(&[over; 3], &[container])
            .expect_err("one and a half times the ceiling is past it");
        assert!(refusal.0.contains("instance volume"), "{}", refusal.0);

        // And a sum wide enough to wrap `i128` refuses by the same road rather than silently
        // returning a negative bound.
        let enormous = Instance {
            volume: i128::MAX / 2,
            weight: 1,
            shrinks: false,
        };
        assert!(from_reduced(&[enormous; 4], &[container]).is_err());
    }

    /// Unlimited inventory must not hide a selected-cost overflow from the precheck.
    #[test]
    fn a_bound_that_cannot_cross_every_binding_exactly_is_refused() {
        let instances = [
            Instance {
                volume: 1,
                weight: 1,
                shrinks: false,
            },
            Instance {
                volume: 1,
                weight: 1,
                shrinks: false,
            },
        ];
        let container = ContainerType {
            usable: 1,
            inner: 1,
            base_area: 1,
            height: 1,
            payload: None,
            max_items: Some(1),
            quantity: None,
            cost_minor: MAX_BOUND_VALUE,
        };

        let refusal = from_reduced(&instances, &[container])
            .expect_err("two selected prices cannot cross every binding exactly");
        assert!(
            refusal.0.contains("exact portable result ceiling"),
            "{}",
            refusal.0
        );
    }

    /// The degenerate geometry each key guards against, as the Python and PHP suites assert.
    ///
    /// Every branch here returns a *bound*, and a bound that is wrong on a degenerate request
    /// is wrong in the direction that matters -- it claims a score is unreachable that a
    /// solver then reaches. The reduced form lets a container be described inconsistently on
    /// purpose, which is the only way to reach a guard whose job is never to be reached.
    #[test]
    fn a_container_with_no_room_bounds_nothing_rather_than_dividing_by_it() {
        let item = Instance {
            volume: 1,
            weight: 1,
            shrinks: false,
        };
        let of = |inner: i128, base_area: i128, height: i128| ContainerType {
            usable: 1000,
            inner,
            base_area,
            height,
            payload: None,
            max_items: None,
            quantity: Some(1),
            cost_minor: 0,
        };
        // Usable volume admits the item; the inner volume key 3 divides by is zero.
        assert_eq!(
            from_reduced(&[item], &[of(0, 100, 10)])
                .unwrap()
                .unused_volume_ppm,
            0
        );
        // No floor to stand on, and no height to stand in: key 4 abstains for both.
        assert_eq!(
            from_reduced(&[item], &[of(1000, 0, 10)])
                .unwrap()
                .stack_height_ppm,
            0
        );
        assert_eq!(
            from_reduced(&[item], &[of(1000, 100, 0)])
                .unwrap()
                .stack_height_ppm,
            0
        );
    }

    /// An unlimited supply of nothing is still nothing.
    ///
    /// `None` inventory means unbounded, and one unlimited type that carries anything makes
    /// the whole capacity unbounded. A type carrying *zero* is the exception on both the
    /// volume and the numeric paths: however many exist they add nothing, so the term is
    /// skipped rather than turning the total infinite.
    #[test]
    fn an_unlimited_supply_of_zero_capacity_adds_nothing() {
        let item = Instance {
            volume: 1,
            weight: 1,
            shrinks: false,
        };
        let empty = ContainerType {
            usable: 0,
            inner: 1000,
            base_area: 100,
            height: 10,
            payload: Some(0),
            max_items: None,
            quantity: None,
            cost_minor: 0,
        };
        let bound = from_reduced(&[item], &[empty]).unwrap();
        assert_eq!(bound.unpacked_count, 1);
        assert_eq!(bound.container_count, 0);
    }

    /// The shape rule, asserted against engine objects rather than the scene's flag.
    ///
    /// The scene supplies `shrinks` ready-made so the corpus check is about arithmetic
    /// alone. That leaves one thing it cannot catch, and it is the exact omission
    /// found in Python: a port that checks only `nesting_height` is unsound for
    /// `convex_hull` and `compressible`, both of which occupy less than their bounding box.
    #[test]
    fn each_shape_that_occupies_less_than_its_box_is_recognised() {
        use crate::geometry::{Dimensions, Rotation};
        use crate::units::{Length, Weight};
        use std::collections::{BTreeMap, BTreeSet};

        let mut item = Item {
            id: "a".into(),
            dimensions: Dimensions {
                length: Length(10),
                width: Length(10),
                height: Length(10),
            },
            weight: Weight(0),
            quantity: 1,
            allowed_rotations: vec![Rotation::Lwh],
            stackable: true,
            must_be_on_floor: false,
            max_top_load: None,
            minimum_support_ratio: 0.0,
            group: None,
            tags: BTreeSet::new(),
            incompatible_tags: BTreeSet::new(),
            priority: 0,
            metadata: BTreeMap::new(),
            nesting_height: None,
            max_stacked_items: None,
            ground_contact_rule: None,
            stop_index: None,
            eligible_container_tags: BTreeSet::new(),
            value: None,
            shape_type: ShapeType::RigidCuboid,
            hull_vertices: None,
            compression_ratio_ppm: None,
            max_compression_pressure_kpa: None,
            stacking_group: None,
            accepts_stacking_groups: Default::default(),
        };
        assert!(!occupies_less_than_its_box(&item));

        item.shape_type = ShapeType::ConvexHull;
        assert!(occupies_less_than_its_box(&item));

        item.shape_type = ShapeType::Compressible;
        assert!(occupies_less_than_its_box(&item));

        item.shape_type = ShapeType::RigidCuboid;
        item.nesting_height = Some(Length(1));
        assert!(occupies_less_than_its_box(&item));
    }
    #[test]
    fn gap_ceiling_is_inclusive() {
        for least in [0, 1] {
            let bound = [0, 1, least, 0, 0];
            let statement = statement(&[0, 1, least + MAX_BOUND_VALUE, 0, 0], &bound).unwrap();
            assert_eq!(statement.gap, Some((2, MAX_BOUND_VALUE)));
            assert!(super::statement(&[0, 1, least + MAX_BOUND_VALUE + 1, 0, 0], &bound).is_none());
        }
    }
}
