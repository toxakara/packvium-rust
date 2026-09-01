//! Lower bounds on the objective vector.
//!
//! The mathematics is fixed by `docs/OPTIMALITY-CERTIFICATES.md`. This is an independent
//! implementation written from that document, not a transcription of the Python or PHP
//! ports, and `conformance/scene/objective-bounds.json` holds all three to the same vectors
//! on 380 cases drawn from the golden corpus.
//!
//!  asks only for soundness here -- the bound must never exceed the achieved
//! objective -- because Rust and JavaScript are not held to placement equality. That
//! distinction does not apply to a bound: it is a function of the *request*, so there is no
//! room for a legitimately different answer, and this port is held to equality because
//! equality is both achievable and strictly stronger.
//!
//! ## Arithmetic
//!
//! `i128` throughout, and it is not close. A one-metre cube is 4.1e21 cubic ticks, a full
//! container load a few orders above that, and the widest intermediate here multiplies a
//! summed volume by 1e6 -- call it 1e28 against an `i128` ceiling of 1.7e38. PHP needs
//! decimal strings for the same quantities and JavaScript needs `BigInt`; this port needs
//! neither, so there is no second arithmetic path to keep in step.
//!
//! ## Complexity
//!
//! `O(n log n + c log c)` time and `O(n + c)` space for `n` quantity-expanded instances and
//! `c` container types: one sort of the volumes, one of the weights, one of the per-unit
//! costs. No geometry is touched and no candidate position is generated, which is why this
//! can be computed at the root of a search rather than inside it.

use crate::geometry::ShapeType;
use crate::model::{Container, Item, ItemInstance};

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

/// Sum a slice, refusing past the ceiling instead of carrying it further.
///
/// `checked_add` as well as the ceiling: a sum large enough to wrap `i128` is far past the
/// ceiling anyway, so both roads lead to the same refusal, and neither leads to a wrong
/// number.
fn checked_sum(values: &[i128], quantity: &str) -> Result<i128, BoundOverflow> {
    let mut total: i128 = 0;
    for value in values {
        total = total
            .checked_add(*value)
            .ok_or_else(|| BoundOverflow(format!("{quantity} overflows the exact range")))?;
        if total > MAX_BOUND_SUM {
            return Err(BoundOverflow(format!(
                "{quantity} sums past the {MAX_BOUND_SUM} ceiling the bound path declares"
            )));
        }
    }
    Ok(total)
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

/// Can this item take up less room than its declared dimensions?
///
/// Three ways, and each breaks the same argument -- that nominal volumes sum to something a
/// solution must carry. A nested item sinks into the one below it; a `convex_hull` occupies
/// its hull and leaves the rest of its bounding box free; a `compressible` item gives up
/// height under load.
///
/// The design document named only the first until  found the omission with a
/// soundness test over the corpus. Asking the question once, here, is what stops a future
/// fourth shape from reintroducing the same unsoundness silently.
pub fn occupies_less_than_its_box(item: &Item) -> bool {
    item.nesting_height.is_some()
        || matches!(
            item.shape_type,
            ShapeType::ConvexHull | ShapeType::Compressible
        )
}

/// Every bound for one request, from the engine's own objects.
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
    let types: Vec<ContainerType> = containers
        .iter()
        .map(|container| {
            let inner = container.inner_dimensions.volume();
            ContainerType {
                usable: inner - inner * container.void_fill_reserve_ppm / PPM,
                inner,
                base_area: container.inner_dimensions.base_area(),
                height: i128::from(container.inner_dimensions.height.0),
                payload: container.max_payload.map(|weight| i128::from(weight.0)),
                max_items: container.max_items.map(|count| count as i128),
                quantity: container.quantity.map(|count| count as i128),
                cost_minor: i128::from(container.cost_minor),
            }
        })
        .collect();
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
    let mut volumes: Vec<i128> = instances.iter().map(|i| i.volume).collect();
    let mut weights: Vec<i128> = instances.iter().map(|i| i.weight).collect();
    volumes.sort_unstable();
    weights.sort_unstable();
    let shrinks = instances.iter().any(|i| i.shrinks);
    let count = instances.len() as i128;

    // The a-priori check, once, on the way in. Every later product is bounded by these totals
    // times `PPM`, so guarding them here is what makes the rest of the arithmetic safe by
    // derivation rather than by hoping each step stays small.
    checked_sum(&volumes, "instance volume")?;
    checked_sum(&weights, "instance weight")?;
    for container in containers {
        guard(container.usable, "container capacity")?;
        guard(container.cost_minor, "opening cost")?;
    }

    let unpacked = unpacked_bound(&volumes, &weights, shrinks, count, containers)?;
    let placed = count - unpacked;
    let opened = container_bound(&volumes, &weights, shrinks, placed, containers);
    let cost = cost_bound(containers, opened)?;
    let unused = unused_volume_bound(&volumes, shrinks, placed, containers, opened);
    let height = stack_height_bound(&volumes, shrinks, placed, containers, opened);
    guard(unused, "unused volume")?;
    guard(height, "stack height")?;
    Ok(Bounds {
        unpacked_count: guard_output(unpacked, "unpacked count")?,
        container_count: guard_output(opened, "container count")?,
        total_cost_minor: guard_output(cost, "opening cost")?,
        unused_volume_ppm: guard_output(unused, "unused volume")?,
        stack_height_ppm: guard_output(height, "stack height")?,
    })
}

/// `L0`: remove the geometry entirely and ask what the declared resources alone forbid.
///
/// With two resources the minimum over each separately is a genuine relaxation and can fall
/// below the true two-resource optimum; it remains a bound, which is all that is claimed.
fn unpacked_bound(
    volumes: &[i128],
    weights: &[i128],
    shrinks: bool,
    count: i128,
    containers: &[ContainerType],
) -> Result<i128, BoundOverflow> {
    let mut placeable = count;
    if !shrinks {
        let capacity = volume_capacity(containers)?;
        placeable = placeable.min(fit(volumes, capacity));
    }
    let payload = native_capacity(containers, |c| c.payload)?;
    placeable = placeable.min(fit(weights, payload));
    if let Some(slots) = native_capacity(containers, |c| c.max_items)? {
        placeable = placeable.min(slots);
    }
    Ok(count - placeable)
}

/// `L1`: grant every container the largest capacity available, which can only understate how
/// many are needed.
fn container_bound(
    volumes: &[i128],
    weights: &[i128],
    shrinks: bool,
    placed: i128,
    containers: &[ContainerType],
) -> i128 {
    if placed <= 0 || containers.is_empty() {
        return 0;
    }
    let taken = placed as usize;
    let mut bound: i128 = 1;
    if !shrinks {
        let largest = containers.iter().map(|c| c.usable).max().unwrap_or(0);
        if largest > 0 {
            bound = bound.max(ceil_div(volumes[..taken].iter().sum::<i128>(), largest));
        }
    }
    if let Some(largest) = finite_max(containers, |c| c.payload)
        && largest > 0
    {
        bound = bound.max(ceil_div(weights[..taken].iter().sum::<i128>(), largest));
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
/// Inventory is respected rather than assumed unlimited. Charging the cheapest type `L1`
/// times would also be a bound, and a weaker one whenever that type is nearly exhausted; the
/// difference costs one sort.
fn cost_bound(containers: &[ContainerType], opened: i128) -> Result<i128, BoundOverflow> {
    if opened <= 0 {
        return Ok(0);
    }
    let mut available: Vec<i128> = Vec::new();
    for container in containers {
        let repeat = container.quantity.unwrap_or(opened).min(opened);
        for _ in 0..repeat.max(0) {
            available.push(container.cost_minor);
        }
    }
    available.sort_unstable();
    checked_sum(
        &available[..(opened as usize).min(available.len())],
        "opening cost",
    )
}

/// `L3`: the fill is largest when every container is the smallest available and holds the
/// greatest volume that could be placed at all.
///
/// Key 3 sums a *per-container* ratio, so a tight bound would have to know which items went
/// where -- that is the packing problem, not a relaxation of it. The `L1 - 1` term is the
/// exact worst case by which summing `k` ceilings can exceed the ceiling of the sum.
fn unused_volume_bound(
    volumes: &[i128],
    shrinks: bool,
    placed: i128,
    containers: &[ContainerType],
    opened: i128,
) -> i128 {
    if shrinks || opened <= 0 || containers.is_empty() {
        return 0;
    }
    let smallest = containers.iter().map(|c| c.inner).min().unwrap_or(0);
    if smallest <= 0 {
        return 0;
    }
    let largest_placed = if placed > 0 {
        volumes[volumes.len() - placed as usize..]
            .iter()
            .sum::<i128>()
    } else {
        0
    };
    (opened * PPM - ceil_div(largest_placed * PPM, smallest) - (opened - 1)).max(0)
}

/// `L4`: the volume that must be placed has to stand at least as tall as itself spread across
/// the widest floor available, in the tallest container available.
///
/// The `L1 - 1` correction is the exact worst-case difference between a sum of floors and the
/// floor of a sum, not a cautionary fudge: the objective floors each container's ratio before
/// summing.
fn stack_height_bound(
    volumes: &[i128],
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
    let required = if placed > 0 {
        ceil_div(volumes[..placed as usize].iter().sum::<i128>(), widest)
    } else {
        0
    };
    (required * PPM / tallest - (opened - 1)).max(0)
}

/// `Sum of usable * quantity`, or `None` for an unbounded total.
///
/// A container with no usable volume adds nothing however many of it there are, which is why
/// an unlimited quantity only unbounds the total when the type actually holds something.
fn volume_capacity(containers: &[ContainerType]) -> Result<Option<i128>, BoundOverflow> {
    let mut total = 0;
    for container in containers {
        match container.quantity {
            None => {
                if container.usable > 0 {
                    return Ok(None);
                }
            }
            Some(quantity) => {
                let contribution = container.usable.checked_mul(quantity).ok_or_else(|| {
                    BoundOverflow("container capacity overflows the exact range".into())
                })?;
                total = checked_sum(&[total, contribution], "container capacity")?;
            }
        }
    }
    Ok(Some(total))
}

/// `Sum of limit * quantity`, or `None` when any limit or inventory is undeclared.
fn native_capacity(
    containers: &[ContainerType],
    limit: fn(&ContainerType) -> Option<i128>,
) -> Result<Option<i128>, BoundOverflow> {
    let mut total = 0;
    for container in containers {
        let Some(value) = limit(container) else {
            return Ok(None);
        };
        match container.quantity {
            None => {
                if value > 0 {
                    return Ok(None);
                }
            }
            Some(quantity) => {
                let contribution = value.checked_mul(quantity).ok_or_else(|| {
                    BoundOverflow("container capacity overflows the exact range".into())
                })?;
                total = checked_sum(&[total, contribution], "container capacity")?;
            }
        }
    }
    Ok(Some(total))
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

/// The largest `n` such that the `n` smallest values sum to at most `capacity`.
///
/// Smallest first, and that is the whole soundness argument: taking the cheapest units
/// maximises how many fit under one capacity, so this over-estimates what any real packing
/// achieves. Geometry, support ratios, stacking rules, incompatible tags and route order can
/// each make the real answer worse and none of them can make it better.
///
/// The caller passes an already-ascending slice; sorting again here would double the only
/// superlinear work this module does.
fn fit(ascending: &[i128], capacity: Option<i128>) -> i128 {
    let Some(capacity) = capacity else {
        return ascending.len() as i128;
    };
    let mut used = 0;
    for (taken, value) in ascending.iter().enumerate() {
        used += value;
        if used > capacity {
            return taken as i128;
        }
    }
    ascending.len() as i128
}

fn ceil_div(numerator: i128, denominator: i128) -> i128 {
    (numerator + denominator - 1) / denominator
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every default-objective fixture in the golden corpus, as Python computed it.
    ///
    ///  asks only that this engine's bound never exceed the achieved objective. It is
    /// held to the stronger claim because the stronger claim is true: a bound is a function
    /// of the request, so Rust and Python disagreeing about one would be a defect in one of
    /// them rather than the permitted freedom in how these two engines place items.
    #[test]
    fn every_corpus_case_matches_python_exactly() {
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../../../../conformance/scene/objective-bounds.json");
        let text = std::fs::read_to_string(path).expect("the shared bounds scene");
        let document: serde_json::Value = serde_json::from_str(&text).expect("valid scene");
        assert_eq!(document["format"], "packvium-objective-bounds/v1");
        let cases = document["cases"].as_array().expect("scene cases");
        assert!(
            cases.len() > 300,
            "a scene that quietly emptied itself would make every case below vacuous"
        );

        for case in cases {
            let instances: Vec<Instance> = case["instances"]
                .as_array()
                .expect("instances")
                .iter()
                .map(|raw| Instance {
                    volume: raw["volume"]
                        .as_str()
                        .expect("decimal volume")
                        .parse()
                        .unwrap(),
                    weight: raw["weight"].as_i64().expect("weight ticks").into(),
                    shrinks: raw["shrinks"].as_bool().expect("shrink flag"),
                })
                .collect();
            let containers: Vec<ContainerType> = case["containers"]
                .as_array()
                .expect("containers")
                .iter()
                .map(|raw| ContainerType {
                    usable: raw["usable"]
                        .as_str()
                        .expect("decimal usable")
                        .parse()
                        .unwrap(),
                    inner: raw["inner"]
                        .as_str()
                        .expect("decimal inner")
                        .parse()
                        .unwrap(),
                    base_area: raw["base_area"].as_i64().expect("base area").into(),
                    height: raw["height"].as_i64().expect("height").into(),
                    payload: raw["payload"].as_i64().map(i128::from),
                    max_items: raw["max_items"].as_i64().map(i128::from),
                    quantity: raw["quantity"].as_i64().map(i128::from),
                    cost_minor: raw["cost_minor"].as_i64().expect("cost").into(),
                })
                .collect();
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
}
