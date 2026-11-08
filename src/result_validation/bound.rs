//! The optimality bound and gap of docs/OPTIMALITY-GAP.md § 6.2, recomputed from the request
//!: `optimality.lower_bound` is the recomputed prefix, the gap agrees with it and the
//! score, no score falls below it, and `optimal` / `proven_optimal` are claimed only by
//! attainment.
//!
//! The reference's own copy of docs/OPTIMALITY-CERTIFICATES.md's formulas and its one refusal
//! predicate, sharing nothing with [`crate::bounds`], which is what it checks. The guarded sums
//! saturate rather than overflow, so a request past `i128` is refused exactly as the reference
//! refuses one past `10^30`; below the ceilings every product stays under `10^36`. O(t log t +
//! c log c) for `t` item types and `c` container types, O(t + c) memory, once per result.

use super::Issue;
use super::container::{SCORE_SCALE, ratio_ppm};
use super::json::{Json, Ordered, Res};
use super::num::{add, ceil_div, floor_div, mul, py_mod, sub};
use super::pyfmt::{int_list, py_repr};
use super::spec::{ContainerSpec, ItemSpec};

const MAX_BOUND_SUM: i128 = 1_000_000_000_000_000_000_000_000_000_000;
const MAX_BOUND_VALUE: i128 = 9_007_199_254_740_991;
const GAP_FIELDS: [&str; 3] = ["gap_key", "absolute_gap", "relative_gap"];

/// `(value, count)` pairs sorted by value: one per item type, however large its quantity.
pub(crate) type Runs = Vec<(i128, i128)>;

/// One container type as the bound reads it, `None` wherever the request sets no limit.
#[derive(Clone, Debug)]
pub(crate) struct BoundContainer {
    pub(crate) inner_volume: i128,
    pub(crate) base_area: i128,
    pub(crate) height: i128,
    pub(crate) usable: i128,
    pub(crate) cost_minor: i128,
    pub(crate) payload: Option<i128>,
    pub(crate) max_items: Option<i128>,
    pub(crate) quantity: Option<i128>,
}

/// One key of a stated bound: a position in `(L0, L1, L2, L3, L4)`, or the one key only that
/// objective ranks by.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Key {
    Bound(usize),
    ValueForgone,
    AchievedHeight,
    UnpackedVolumePpm,
}

/// Where each covered objective reads its bound in its own score order, with the objective's
/// name for the withholding reason. Any other objective has no bound.
fn order_of(objective: &Json) -> Option<(&str, [Key; 5])> {
    use Key::{AchievedHeight, Bound, UnpackedVolumePpm, ValueForgone};
    let Json::Str(name) = objective else {
        return None;
    };
    let order = match name.as_str() {
        "default" => [Bound(0), Bound(1), Bound(2), Bound(3), Bound(4)],
        "lowest_cost" => [Bound(0), Bound(2), Bound(1), Bound(3), Bound(4)],
        "maximum_value" => [Bound(0), ValueForgone, Bound(1), Bound(2), Bound(3)],
        "open_dimension_height" => [Bound(0), AchievedHeight, Bound(1), Bound(2), Bound(3)],
        "maximum_volume" => [UnpackedVolumePpm, Bound(0), Bound(1), Bound(2), Bound(3)],
        _ => return None,
    };
    Some((name, order))
}

fn run_count(runs: &[(i128, i128)]) -> i128 {
    runs.iter()
        .fold(0, |total, (_, count)| total.saturating_add(*count))
}

fn run_sum(runs: &[(i128, i128)]) -> i128 {
    runs.iter().fold(0, |total, (value, count)| {
        total.saturating_add(value.saturating_mul(*count))
    })
}

/// `Σ value × quantity`, or `None` when unbounded; a type that holds nothing adds 0. Saturates.
fn capacity_total(values: &[Option<i128>], quantities: &[Option<i128>]) -> Option<i128> {
    let mut total: i128 = 0;
    for (value, quantity) in values.iter().zip(quantities) {
        let value = (*value)?;
        match quantity {
            None if value > 0 => return None,
            None => {}
            Some(quantity) => total = total.saturating_add(value.saturating_mul(*quantity)),
        }
    }
    Some(total)
}

/// How many of the smallest values sum to at most `capacity`; all of them when unlimited.
fn fit(runs: &[(i128, i128)], capacity: Option<i128>) -> Res<i128> {
    let Some(capacity) = capacity else {
        return Ok(run_count(runs));
    };
    if capacity < 0 {
        return Ok(0);
    }
    let (mut taken, mut used) = (0, 0);
    for &(value, count) in runs {
        let room = if value == 0 {
            count
        } else {
            count.min(floor_div(sub(capacity, used)?, value)?)
        };
        taken = add(taken, room)?;
        used = add(used, mul(value, room)?)?;
        if room < count {
            break;
        }
    }
    Ok(taken)
}

/// The sum of the `count` smallest values of `runs`, walked in the order given.
fn smallest<'a, I: IntoIterator<Item = &'a (i128, i128)>>(runs: I, mut count: i128) -> Res<i128> {
    let mut total = 0;
    for &(value, available) in runs {
        if count <= 0 {
            break;
        }
        let taken = available.min(count);
        total = add(total, mul(value, taken)?)?;
        count -= taken;
    }
    Ok(total)
}

/// `L1`, given `L0`.
fn container_bound(
    volumes: &[(i128, i128)],
    weights: &[(i128, i128)],
    shrinks: bool,
    placed: i128,
    containers: &[BoundContainer],
) -> Res<i128> {
    if placed <= 0 || containers.is_empty() {
        return Ok(0);
    }
    let mut bound = 1;
    let largest_volume = containers.iter().map(|c| c.usable).max().unwrap_or(0);
    if !shrinks && largest_volume > 0 {
        bound = bound.max(ceil_div(smallest(volumes, placed)?, largest_volume)?);
    }
    let payloads: Option<Vec<i128>> = containers.iter().map(|c| c.payload).collect();
    if let Some(largest_payload) = payloads.and_then(|values| values.into_iter().max())
        && largest_payload > 0
    {
        bound = bound.max(ceil_div(smallest(weights, placed)?, largest_payload)?);
    }
    let slots: Option<Vec<i128>> = containers.iter().map(|c| c.max_items).collect();
    if let Some(largest_slots) = slots.and_then(|values| values.into_iter().max())
        && largest_slots > 0
    {
        bound = bound.max(ceil_div(placed, largest_slots)?);
    }
    Ok(bound)
}

/// `L2`: the `opened` cheapest containers the inventory holds, greedily by cost. Saturates, so
/// a total past `i128` is past the result ceiling, as the reference finds it.
fn cost_bound(containers: &[BoundContainer], mut opened: i128) -> i128 {
    let mut by_cost: Vec<&BoundContainer> = containers.iter().collect();
    by_cost.sort_by_key(|container| container.cost_minor);
    let mut total: i128 = 0;
    for container in by_cost {
        if opened <= 0 {
            break;
        }
        let taken = container
            .quantity
            .map_or(opened, |quantity| quantity.min(opened));
        total = total.saturating_add(container.cost_minor.saturating_mul(taken));
        opened -= taken;
    }
    total
}

/// `L3`.
fn unused_bound(
    volumes: &[(i128, i128)],
    shrinks: bool,
    placed: i128,
    containers: &[BoundContainer],
    opened: i128,
) -> Res<i128> {
    let smallest_inner = containers
        .iter()
        .map(|c| c.inner_volume)
        .filter(|volume| *volume > 0)
        .min();
    let Some(smallest_inner) = smallest_inner else {
        return Ok(0);
    };
    if opened <= 0 || shrinks {
        return Ok(0);
    }
    let largest = smallest(volumes.iter().rev(), placed)?;
    let filled = ceil_div(mul(largest, SCORE_SCALE)?, smallest_inner)?;
    Ok(sub(sub(mul(opened, SCORE_SCALE)?, filled)?, opened - 1)?.max(0))
}

/// `L4`.
fn height_bound(
    volumes: &[(i128, i128)],
    shrinks: bool,
    placed: i128,
    containers: &[BoundContainer],
    opened: i128,
) -> Res<i128> {
    let widest = containers
        .iter()
        .map(|c| c.base_area)
        .filter(|a| *a > 0)
        .max();
    let tallest = containers.iter().map(|c| c.height).filter(|h| *h > 0).max();
    let (Some(widest), Some(tallest)) = (widest, tallest) else {
        return Ok(0);
    };
    if opened <= 0 || shrinks {
        return Ok(0);
    }
    let required = ceil_div(smallest(volumes, placed)?, widest)?;
    Ok(sub(floor_div(mul(required, SCORE_SCALE)?, tallest)?, opened - 1)?.max(0))
}

/// `[L0, L1, L2, L3, L4]` of docs/OPTIMALITY-CERTIFICATES.md, or `None` where its one refusal
/// predicate withholds the bound. `volumes` and `weights` are runs sorted by value.
pub(crate) fn lower_bound(
    volumes: &[(i128, i128)],
    weights: &[(i128, i128)],
    shrinks: bool,
    containers: &[BoundContainer],
) -> Res<Option<[i128; 5]>> {
    let quantities: Vec<Option<i128>> = containers.iter().map(|c| c.quantity).collect();
    let column = |read: fn(&BoundContainer) -> Option<i128>| -> Vec<Option<i128>> {
        containers.iter().map(read).collect()
    };
    let usable = capacity_total(&column(|c| Some(c.usable)), &quantities);
    let payload = capacity_total(&column(|c| c.payload), &quantities);
    let slots = capacity_total(&column(|c| c.max_items), &quantities);
    let guarded = [
        Some(run_sum(volumes)),
        Some(run_sum(weights)),
        usable,
        payload,
        slots,
    ];
    if guarded.iter().flatten().any(|total| *total > MAX_BOUND_SUM) {
        return Ok(None);
    }
    let count = run_count(volumes);
    let mut placeable = 0;
    if !containers.is_empty() {
        placeable = count;
        if !shrinks {
            placeable = placeable.min(fit(volumes, usable)?);
        }
        placeable = placeable.min(fit(weights, payload)?);
        if let Some(slots) = slots {
            placeable = placeable.min(slots);
        }
    }
    let opened = container_bound(volumes, weights, shrinks, placeable, containers)?;
    let bound = [
        count - placeable,
        opened,
        cost_bound(containers, opened),
        unused_bound(volumes, shrinks, placeable, containers, opened)?,
        height_bound(volumes, shrinks, placeable, containers, opened)?,
    ];
    Ok(if bound.iter().any(|value| *value > MAX_BOUND_VALUE) {
        None
    } else {
        Some(bound)
    })
}

/// Every item type's `value`, 0 when unset, as runs sorted by value.
fn value_runs(items: &Ordered<ItemSpec>) -> Res<Runs> {
    let mut values = Vec::new();
    for spec in items.values() {
        let value = if spec.value.truthy() {
            spec.value.int("value")?
        } else {
            0
        };
        values.push((value, spec.quantity.int("quantity")?));
    }
    values.sort_by_key(|run| run.0);
    Ok(values)
}

/// `maximum_value`'s second key: the `unpacked` least valuable instances. Saturates, so a sum
/// past `i128` is past the result ceiling, as the reference finds it.
fn value_forgone(values: &[(i128, i128)], mut unpacked: i128) -> i128 {
    let mut total: i128 = 0;
    for &(value, available) in values {
        if unpacked <= 0 {
            break;
        }
        let taken = available.min(unpacked);
        total = total.saturating_add(value.saturating_mul(taken));
        unpacked -= taken;
    }
    total
}

/// The key only one objective ranks by, each conditional on `L0` like the five: the
/// least value forgone and the least summed top of a packing leaving `unpacked` behind, and
/// the least unpacked share of requested volume any packing scores. Volumes and capacity are
/// inside the ceiling `lower_bound` admitted. O(t log t + c).
fn objective_term(
    key: Key,
    values: &[(i128, i128)],
    volumes: &[(i128, i128)],
    shrinks: bool,
    containers: &[BoundContainer],
    unpacked: i128,
) -> Res<i128> {
    if matches!(key, Key::ValueForgone) {
        return Ok(value_forgone(values, unpacked));
    }
    if shrinks {
        return Ok(0);
    }
    if matches!(key, Key::AchievedHeight) {
        let placed = run_count(volumes) - unpacked;
        let widest = containers
            .iter()
            .map(|c| c.base_area)
            .filter(|a| *a > 0)
            .max();
        return match widest {
            Some(widest) if placed > 0 => ceil_div(smallest(volumes, placed)?, widest),
            _ => Ok(0),
        };
    }
    let requested = run_sum(volumes);
    let quantities: Vec<Option<i128>> = containers.iter().map(|c| c.quantity).collect();
    let usable: Vec<Option<i128>> = containers.iter().map(|c| Some(c.usable)).collect();
    let Some(capacity) = capacity_total(&usable, &quantities) else {
        return Ok(0);
    };
    if requested == 0 {
        return Ok(0);
    }
    ceil_div(
        mul(sub(requested, capacity)?.max(0), SCORE_SCALE)?,
        requested,
    )
}

/// The bound's inputs: every instance, fixed ones included, and every declared container type.
type Inputs = (Runs, Runs, bool, Vec<BoundContainer>);

fn inputs(items: &Ordered<ItemSpec>, containers: &Ordered<ContainerSpec>) -> Res<Inputs> {
    let mut volumes = Vec::new();
    let mut weights = Vec::new();
    let mut shrinks = false;
    for spec in items.values() {
        let quantity = spec.quantity.int("quantity")?;
        let [length, width, height] = spec.dimensions;
        volumes.push((mul(mul(length, width)?, height)?, quantity));
        weights.push((spec.weight, quantity));
        shrinks = shrinks
            || (quantity > 0
                && (spec.nesting_height.is_some()
                    || spec.shape_type.is_str("convex_hull")
                    || spec.shape_type.is_str("compressible")));
    }
    volumes.sort_by_key(|run| run.0);
    weights.sort_by_key(|run| run.0);
    let mut types = Vec::new();
    for spec in containers.values() {
        let volume = spec.volume()?;
        let scaled = ratio_ppm(spec.void_fill_reserve_ratio);
        let reserved = add(
            mul(floor_div(volume, SCORE_SCALE)?, scaled)?,
            floor_div(mul(py_mod(volume, SCORE_SCALE)?, scaled)?, SCORE_SCALE)?,
        )?;
        types.push(BoundContainer {
            inner_volume: volume,
            base_area: mul(spec.inner[0], spec.inner[1])?,
            height: spec.inner[2],
            usable: sub(volume, reserved)?,
            cost_minor: spec.cost_minor,
            payload: spec.max_payload,
            max_items: spec.max_items,
            quantity: spec.quantity,
        });
    }
    Ok((volumes, weights, shrinks, types))
}

/// `bound` read in `order`, each objective-only key computed from the same inputs.
fn stated_vector(
    order: &[Key; 5],
    bound: &[i128; 5],
    values: &[(i128, i128)],
    volumes: &[(i128, i128)],
    shrinks: bool,
    containers: &[BoundContainer],
) -> Res<Vec<i128>> {
    order
        .iter()
        .map(|&key| match key {
            Key::Bound(position) => Ok(bound[position]),
            _ => objective_term(key, values, volumes, shrinks, containers, bound[0]),
        })
        .collect()
}

/// The bound in the objective's score order, or `None` with the reason none is stated (§ 1.5).
fn stated(
    request: &Json,
    status: &Json,
    items: &Ordered<ItemSpec>,
    containers: &Ordered<ContainerSpec>,
) -> Res<(Option<Vec<i128>>, String)> {
    let empty = Json::Dict(Vec::new());
    let default = Json::str_of("default");
    let name = request
        .get_or("configuration", &empty)?
        .get_or("objective", &default)?;
    let Some((objective, order)) = order_of(name) else {
        return Ok((None, format!("objective {} has no bound", py_repr(name))));
    };
    if containers
        .values()
        .any(|spec| spec.made_to_measure.is_some())
    {
        return Ok((None, "a container is made to measure".to_owned()));
    }
    for withheld in ["infeasible", "invalid_result"] {
        if status.is_str(withheld) {
            return Ok((None, format!("status is '{withheld}'")));
        }
    }
    let (volumes, weights, shrinks, types) = inputs(items, containers)?;
    let Some(bound) = lower_bound(&volumes, &weights, shrinks, &types)? else {
        return Ok((None, "the request is past a declared ceiling".to_owned()));
    };
    // Read only where ranked, so a malformed `value` never touches another objective's bound.
    let values = if order.contains(&Key::ValueForgone) {
        value_runs(items)?
    } else {
        Vec::new()
    };
    let vector = stated_vector(&order, &bound, &values, &volumes, shrinks, &types)?;
    if vector.iter().any(|value| *value > MAX_BOUND_VALUE) {
        return Ok((
            None,
            format!("the {objective} key is past the portable ceiling"),
        ));
    }
    Ok((Some(vector), String::new()))
}

/// The first position where `score` leaves `bound`, and whether it is above it.
fn departure(score: &[i128], bound: &[i128]) -> Option<(usize, bool)> {
    score
        .iter()
        .zip(bound)
        .position(|(achieved, floor)| achieved != floor)
        .map(|index| (index, score[index] > bound[index]))
}

/// `lower_bound` as counts when it is a list of 1 to 5 non-negative integers.
fn well_formed(emitted: &Json) -> Option<Vec<i128>> {
    let Json::List(values) = emitted else {
        return None;
    };
    if values.is_empty() || values.len() > 5 {
        return None;
    }
    values
        .iter()
        .map(|value| value.strict_int().filter(|count| *count >= 0))
        .collect()
}

/// The three gap fields a correct result carries (§ 1.1), with the prefix length.
struct ExpectedGap {
    length: usize,
    gap_key: Option<i128>,
    absolute_gap: Option<i128>,
    relative_gap: Option<(i128, i128)>,
}

fn expected_gap(score: &[i128], bound: &[i128], departure: Option<(usize, bool)>) -> ExpectedGap {
    let Some((key, _)) = departure else {
        return ExpectedGap {
            length: bound.len(),
            gap_key: None,
            absolute_gap: None,
            relative_gap: None,
        };
    };
    let absolute = score[key] - bound[key];
    ExpectedGap {
        length: key + 1,
        gap_key: Some(key as i128),
        absolute_gap: Some(absolute),
        relative_gap: (bound[key] != 0).then_some((absolute, bound[key])),
    }
}

fn int_field_matches(actual: &Json, expected: Option<i128>) -> bool {
    match expected {
        None => actual.is_null(),
        Some(value) => actual.strict_int() == Some(value),
    }
}

fn relative_matches(actual: &Json, expected: Option<(i128, i128)>) -> Res<bool> {
    let Some((numerator, denominator)) = expected else {
        return Ok(actual.is_null());
    };
    let Some(pairs) = actual.as_dict() else {
        return Ok(false);
    };
    Ok(pairs.len() == 2
        && actual.get_null("numerator")?.strict_int() == Some(numerator)
        && actual.get_null("denominator")?.strict_int() == Some(denominator))
}

fn gap_text(expected: Option<i128>) -> String {
    expected.map_or_else(|| "absent".to_owned(), |value| value.to_string())
}

fn check_reported(
    emitted: &[i128],
    score: &[i128],
    bound: &[i128],
    departure: Option<(usize, bool)>,
    fact: &Json,
    issues: &mut Vec<Issue>,
) -> Res<()> {
    let prefix = &bound[..emitted.len().min(bound.len())];
    if emitted != prefix {
        issues.push(Issue::new(
            "optimality_bound_mismatch",
            format!(
                "reported {}, recomputed {}",
                int_list(emitted),
                int_list(prefix)
            ),
        ));
    }
    let expected = expected_gap(score, bound, departure);
    if emitted.len() != expected.length {
        issues.push(Issue::new(
            "optimality_gap_mismatch",
            format!(
                "lower_bound has {} entries, expected {}",
                emitted.len(),
                expected.length
            ),
        ));
    }
    let mut mismatch = |name: &str, text: String| {
        issues.push(Issue::new(
            "optimality_gap_mismatch",
            format!("{name} should be {text}"),
        ));
    };
    if !int_field_matches(fact.get_null("gap_key")?, expected.gap_key) {
        mismatch("gap_key", gap_text(expected.gap_key));
    }
    if !int_field_matches(fact.get_null("absolute_gap")?, expected.absolute_gap) {
        mismatch("absolute_gap", gap_text(expected.absolute_gap));
    }
    if !relative_matches(fact.get_null("relative_gap")?, expected.relative_gap)? {
        let text = expected.relative_gap.map_or_else(
            || "absent".to_owned(),
            |(numerator, denominator)| format!("{numerator}/{denominator}"),
        );
        mismatch("relative_gap", text);
    }
    Ok(())
}

fn check_claims(
    result: &Json,
    fact: &Json,
    attained: bool,
    claims_attainment: bool,
    issues: &mut Vec<Issue>,
) -> Res<()> {
    let status = result.at("status")?;
    let complete = result.at("complete")?.truthy();
    if status.is_str("optimal") && !(complete && attained) {
        issues.push(Issue::new(
            "result_fact_mismatch",
            "status 'optimal' requires a complete result that attains the recomputed lower bound"
                .to_owned(),
        ));
    }
    if status.is_str("feasible") && complete && claims_attainment {
        issues.push(Issue::new(
            "result_fact_mismatch",
            "status is 'feasible', but the result attains its lower_bound; expected 'optimal'"
                .to_owned(),
        ));
    }
    if fact.get_null("code")?.is_str("proven_optimal") && !claims_attainment {
        issues.push(Issue::new(
            "unproven_optimality_claim",
            "optimality.code is 'proven_optimal' without an attained lower_bound".to_owned(),
        ));
    }
    Ok(())
}

/// The reference's `_check_optimality_bound`. Absence of the field is never an issue.
pub(crate) fn check_optimality_bound(
    request: &Json,
    result: &Json,
    items: &Ordered<ItemSpec>,
    containers: &Ordered<ContainerSpec>,
    score: &[i128],
    issues: &mut Vec<Issue>,
) -> Res<()> {
    let fact = result.at("optimality")?;
    let (bound, withheld) = stated(request, result.at("status")?, items, containers)?;
    let departure = bound.as_deref().and_then(|bound| departure(score, bound));
    let below = matches!(departure, Some((_, false)));
    if let (Some(bound), Some((key, false))) = (&bound, departure) {
        issues.push(Issue::new(
            "score_below_bound",
            format!(
                "score[{key}] is {}, below its lower bound {}",
                score[key], bound[key]
            ),
        ));
    }
    let emitted = fact.get_null("lower_bound")?;
    if !emitted.is_null()
        && let (Some(bound), Some((key, _))) = (&bound, departure)
        && score[key] - bound[key] > MAX_BOUND_VALUE
    {
        issues.push(Issue::new(
            "optimality_gap_mismatch",
            "the gap exceeds the portable result ceiling".to_owned(),
        ));
    }
    let counts = well_formed(emitted);
    if emitted.is_null() {
        let mut reports_gap = false;
        for name in GAP_FIELDS {
            reports_gap = reports_gap || !fact.get_null(name)?.is_null();
        }
        if reports_gap {
            issues.push(Issue::new(
                "optimality_gap_mismatch",
                "a gap is reported without lower_bound".to_owned(),
            ));
        }
    } else if let Some(counts) = &counts {
        match &bound {
            None => issues.push(Issue::new(
                "optimality_bound_mismatch",
                format!("lower_bound is reported where no bound is stated: {withheld}"),
            )),
            Some(_) if below => issues.push(Issue::new(
                "optimality_bound_mismatch",
                "lower_bound is reported for a score below the recomputed bound".to_owned(),
            )),
            Some(bound) => check_reported(counts, score, bound, departure, fact, issues)?,
        }
    } else {
        issues.push(Issue::new(
            "optimality_bound_mismatch",
            "lower_bound must be a list of 1 to 5 non-negative integers".to_owned(),
        ));
    }
    let attained = bound.is_some() && departure.is_none();
    let claims_attainment = counts.is_some_and(|counts| counts.as_slice() == score);
    check_claims(result, fact, attained, claims_attainment, issues)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The cross-language scene kept one level above this crate; a published copy does not
    /// carry it.
    fn scene() -> Option<serde_json::Value> {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../../../conformance/scene/objective-bounds.json"
        );
        let Ok(text) = std::fs::read_to_string(path) else {
            eprintln!(
                "skipping: the shared cross-language scene fixture is not part of this package"
            );
            return None;
        };
        Some(serde_json::from_str(&text).expect("the scene is JSON"))
    }

    fn integer(value: &serde_json::Value) -> i128 {
        match value {
            serde_json::Value::String(text) => text.parse().expect("a decimal string"),
            other => i128::from(other.as_i64().expect("an integer")),
        }
    }

    fn optional(value: &serde_json::Value) -> Option<i128> {
        (!value.is_null()).then(|| integer(value))
    }

    fn runs(instances: &[serde_json::Value], field: &str) -> Runs {
        let mut runs: Runs = instances.iter().map(|i| (integer(&i[field]), 1)).collect();
        runs.sort_by_key(|run| run.0);
        runs
    }

    /// The validator's copy agrees with the oracle on every case and edge of the shared scene,
    /// refusals included.
    #[test]
    fn the_validator_bound_matches_the_scene() {
        let Some(scene) = scene() else {
            return;
        };
        let mut compared = 0;
        for case in ["cases", "edges"]
            .iter()
            .flat_map(|key| scene[*key].as_array().expect("an array"))
        {
            let instances = case["instances"].as_array().expect("instances");
            let containers: Vec<BoundContainer> = case["containers"]
                .as_array()
                .expect("containers")
                .iter()
                .map(|c| BoundContainer {
                    inner_volume: integer(&c["inner"]),
                    base_area: integer(&c["base_area"]),
                    height: integer(&c["height"]),
                    usable: integer(&c["usable"]),
                    cost_minor: integer(&c["cost_minor"]),
                    payload: optional(&c["payload"]),
                    max_items: optional(&c["max_items"]),
                    quantity: optional(&c["quantity"]),
                })
                .collect();
            let shrinks = instances.iter().any(|i| i["shrinks"] == true);
            let computed = lower_bound(
                &runs(instances, "volume"),
                &runs(instances, "weight"),
                shrinks,
                &containers,
            )
            .expect("no overflow");
            let expected: Option<Vec<i128>> = case["bounds"]
                .as_array()
                .map(|values| values.iter().map(integer).collect());
            assert_eq!(computed.map(|bound| bound.to_vec()), expected, "{case}");
            let objectives = case["objectives"].as_object().expect("objectives");
            assert_eq!(objectives.len(), 5, "every covered objective is recorded");
            for (objective, vector) in objectives {
                let (_, order) = order_of(&Json::str_of(objective)).expect("a covered objective");
                let stated = computed.and_then(|bound| {
                    let vector = stated_vector(
                        &order,
                        &bound,
                        &runs(instances, "value"),
                        &runs(instances, "volume"),
                        shrinks,
                        &containers,
                    )
                    .expect("no overflow");
                    (!vector.iter().any(|value| *value > MAX_BOUND_VALUE)).then_some(vector)
                });
                let expected: Option<Vec<i128>> = vector
                    .as_array()
                    .map(|values| values.iter().map(integer).collect());
                assert_eq!(stated, expected, "{objective} on {case}");
            }
            compared += 1;
        }
        assert!(compared > 400);
    }

    #[test]
    fn a_saturated_capacity_is_past_the_ceiling() {
        let container = BoundContainer {
            inner_volume: 1,
            base_area: 1,
            height: 1,
            usable: i128::MAX,
            cost_minor: 0,
            payload: None,
            max_items: None,
            quantity: Some(2),
        };
        assert_eq!(
            lower_bound(&[(1, 1)], &[(0, 1)], false, &[container]).unwrap(),
            None
        );
    }

    #[test]
    fn a_negative_or_unlimited_capacity_and_no_containers_are_read_as_the_reference_reads_them() {
        let mut container = BoundContainer {
            inner_volume: 1000,
            base_area: 100,
            height: 10,
            usable: -1,
            cost_minor: 3,
            payload: Some(5),
            max_items: None,
            quantity: Some(1),
        };
        let bound = |containers: &[BoundContainer]| {
            lower_bound(&[(0, 2)], &[(1, 2)], false, containers).unwrap()
        };
        assert_eq!(
            bound(std::slice::from_ref(&container)),
            Some([2, 0, 0, 0, 0])
        );
        container.quantity = None;
        assert_eq!(bound(&[container]), Some([0, 1, 3, 1_000_000, 0]));
        assert_eq!(
            lower_bound(&[(1, 1)], &[(0, 1)], false, &[]).unwrap(),
            Some([1, 0, 0, 0, 0])
        );
    }

    /// One cube in a box for two: every position but the height is attained.
    const REQUEST: &str = r#"{
      "items": [{"id": "cube", "quantity": 1, "dimensions": {"length": "10", "width": "10", "height": "10"}}],
      "containers": [{"id": "box", "inner_dimensions": {"length": "20", "width": "10", "height": "10"}}]
    }"#;

    fn gap() -> serde_json::Value {
        serde_json::json!({
            "code": "not_proven",
            "lower_bound": [0, 1, 0, 500000, 500000],
            "gap_key": 4,
            "absolute_gap": 500000,
            "relative_gap": {"numerator": 500000, "denominator": 500000},
        })
    }

    fn packed(request: &str) -> serde_json::Value {
        serde_json::from_str(&crate::pack_json(request).expect("packs")).expect("JSON")
    }

    fn issues(request: &str, result: &serde_json::Value) -> Vec<Issue> {
        crate::validate_json(request, &result.to_string()).expect("judged")
    }

    fn codes_after(
        optimality: serde_json::Value,
        change: impl FnOnce(&mut serde_json::Value),
    ) -> Vec<String> {
        let mut result = packed(REQUEST);
        result["optimality"] = optimality;
        change(&mut result);
        issues(REQUEST, &result)
            .into_iter()
            .map(|issue| issue.code)
            .collect()
    }

    fn codes(optimality: serde_json::Value) -> Vec<String> {
        codes_after(optimality, |_| {})
    }

    fn details(request: &str, optimality: serde_json::Value) -> Vec<String> {
        let mut result = packed(REQUEST);
        result["optimality"] = optimality;
        issues(request, &result)
            .into_iter()
            .map(|issue| issue.detail)
            .collect()
    }

    #[test]
    fn a_stated_gap_and_its_absence_are_both_valid() {
        assert_eq!(codes(gap()), Vec::<String>::new());
        assert_eq!(
            codes(serde_json::json!({"code": "not_proven"})),
            Vec::<String>::new()
        );
    }

    #[test]
    fn a_bound_where_none_is_stated_or_of_the_wrong_shape_is_a_mismatch() {
        assert_eq!(
            details(
                REQUEST,
                serde_json::json!({"code": "not_proven", "lower_bound": []})
            ),
            ["lower_bound must be a list of 1 to 5 non-negative integers"]
        );

        // Payload capacity 8e24 weight ticks times a million boxes passes the 10^30 ceiling;
        // the same packing answers it, only the inventory changed.
        let mut huge: serde_json::Value = serde_json::from_str(REQUEST).expect("JSON");
        huge["containers"][0]["max_payload"] =
            serde_json::json!({"value": "1000000000000000", "unit": "kg"});
        huge["containers"][0]["quantity"] = serde_json::json!(1_000_000);
        assert_eq!(
            details(
                &huge.to_string(),
                serde_json::json!({"code": "not_proven", "lower_bound": [0]})
            ),
            [
                "lower_bound is reported where no bound is stated: the request is past a declared ceiling"
            ]
        );

        let mut invalid = packed(REQUEST);
        invalid["status"] = serde_json::json!("invalid_result");
        invalid["optimality"] = serde_json::json!({"code": "not_proven", "lower_bound": [0]});
        let invalid: Vec<String> = issues(REQUEST, &invalid)
            .into_iter()
            .map(|issue| issue.detail)
            .collect();
        assert!(
            invalid.contains(
                &"lower_bound is reported where no bound is stated: status is 'invalid_result'"
                    .to_owned()
            )
        );
    }

    #[test]
    fn each_check_fires_on_its_own_mutation() {
        let mut wrong_bound = gap();
        wrong_bound["lower_bound"][1] = serde_json::json!(2);
        assert_eq!(codes(wrong_bound), ["optimality_bound_mismatch"]);

        let mut wrong_gap = gap();
        wrong_gap["absolute_gap"] = serde_json::json!(1);
        assert_eq!(
            details(REQUEST, wrong_gap),
            ["absolute_gap should be 500000"]
        );

        let mut wrong_relative = gap();
        wrong_relative["relative_gap"] = serde_json::json!(1);
        assert_eq!(
            details(REQUEST, wrong_relative),
            ["relative_gap should be 500000/500000"]
        );

        assert_eq!(
            codes_after(gap(), |result| result["score"][3] = serde_json::json!(0)),
            [
                "score_mismatch",
                "score_below_bound",
                "optimality_bound_mismatch"
            ]
        );

        let mut claimed = gap();
        claimed["code"] = serde_json::json!("proven_optimal");
        assert_eq!(
            codes_after(claimed, |result| result["status"] =
                serde_json::json!("optimal")),
            ["result_fact_mismatch", "unproven_optimality_claim"]
        );
    }

    /// Two cubes fill the box, so every position is attained and no gap may be stated.
    #[test]
    fn an_attained_bound_is_valid_and_states_no_gap() {
        let attained = r#"{
          "items": [{"id": "cube", "quantity": 2, "dimensions": {"length": "10", "width": "10", "height": "10"}}],
          "containers": [{"id": "box", "inner_dimensions": {"length": "20", "width": "10", "height": "10"}}]
        }"#;
        let mut result = packed(attained);
        result["status"] = serde_json::json!("optimal");
        result["optimality"] = serde_json::json!({
            "code": "proven_optimal",
            "lower_bound": [0, 1, 0, 0, 1000000],
        });
        assert_eq!(issues(attained, &result), Vec::<Issue>::new());

        result["optimality"]["gap_key"] = serde_json::json!(0);
        result["optimality"]["absolute_gap"] = serde_json::json!(0);
        result["optimality"]["relative_gap"] =
            serde_json::json!({"numerator": 0, "denominator": 1});
        let details: Vec<String> = issues(attained, &result)
            .into_iter()
            .map(|issue| issue.detail)
            .collect();
        assert_eq!(
            details,
            [
                "gap_key should be absent",
                "absolute_gap should be absent",
                "relative_gap should be absent"
            ]
        );
    }
    /// Three cubes, one box: two are left behind whatever the objective.
    fn one_of_three(objective: &str, values: [i64; 3]) -> String {
        let items: Vec<serde_json::Value> = values
            .iter()
            .enumerate()
            .map(|(index, value)| {
                serde_json::json!({"id": format!("cube{index}"), "quantity": 1, "value": value,
                    "dimensions": {"length": "10", "width": "10", "height": "10"}})
            })
            .collect();
        serde_json::json!({
            "items": items,
            "containers": [{"id": "box", "quantity": 1,
                "inner_dimensions": {"length": "10", "width": "10", "height": "10"}}],
            "configuration": {"objective": objective},
        })
        .to_string()
    }

    /// The engine's own statement for each objective with a key of its own is the
    /// one the validator recomputes, and forging that key is a mismatch.
    #[test]
    fn each_objective_with_its_own_key_is_recomputed() {
        for (objective, own_key) in [
            ("maximum_value", 1),
            ("open_dimension_height", 1),
            ("maximum_volume", 0),
        ] {
            // A zero value is read as an unset one.
            let request = one_of_three(objective, [5, 0, 9]);
            let mut result = packed(&request);
            assert!(
                !result["optimality"]["lower_bound"].is_null(),
                "{objective}"
            );
            assert_eq!(
                issues(&request, &result),
                Vec::<Issue>::new(),
                "{objective}"
            );
            let forged = &mut result["optimality"]["lower_bound"][own_key];
            *forged = serde_json::json!(forged.as_i64().expect("an integer") + 1);
            assert!(
                issues(&request, &result)
                    .iter()
                    .any(|issue| issue.code == "optimality_bound_mismatch"),
                "{objective}"
            );
        }
    }

    /// A value sum past the portable ceiling withholds `maximum_value`'s bound, and nothing
    /// else: the default objective over the same request still states one.
    #[test]
    fn a_value_past_the_portable_ceiling_withholds_only_maximum_value() {
        let past = [9_007_199_254_740_991, 1, 9_007_199_254_740_991];
        let request = one_of_three("maximum_value", past);
        let mut result = packed(&request);
        assert!(result["optimality"]["lower_bound"].is_null());
        assert_eq!(issues(&request, &result), Vec::<Issue>::new());
        result["optimality"]["lower_bound"] = serde_json::json!([2]);
        let details: Vec<String> = issues(&request, &result)
            .into_iter()
            .map(|issue| issue.detail)
            .collect();
        assert_eq!(
            details,
            [
                "lower_bound is reported where no bound is stated: the maximum_value key is past the portable ceiling"
            ]
        );
        let default = one_of_three("default", past);
        assert!(!packed(&default)["optimality"]["lower_bound"].is_null());
    }

    /// Branches the scene does not reach: no floor, nothing placed, nothing requested.
    #[test]
    fn an_objective_term_abstains_without_a_floor_a_placement_or_a_volume() {
        let container = BoundContainer {
            inner_volume: 1000,
            base_area: 0,
            height: 10,
            usable: 1000,
            cost_minor: 0,
            payload: None,
            max_items: None,
            quantity: Some(1),
        };
        let term = |key: Key, volumes: &[(i128, i128)], unpacked: i128| {
            objective_term(
                key,
                &[],
                volumes,
                false,
                std::slice::from_ref(&container),
                unpacked,
            )
            .unwrap()
        };
        assert_eq!(term(Key::AchievedHeight, &[(600, 1)], 0), 0);
        assert_eq!(term(Key::AchievedHeight, &[(600, 1)], 1), 0);
        assert_eq!(term(Key::UnpackedVolumePpm, &[(0, 1)], 0), 0);
        assert_eq!(term(Key::UnpackedVolumePpm, &[(1600, 1)], 1), 375_000);
        assert_eq!(value_forgone(&[(1, 2), (i128::MAX, 2)], 3), i128::MAX);
        assert!(order_of(&Json::Null).is_none());
    }

    #[test]
    fn unrepresentable_gaps_are_withheld_and_rejected_when_forged() {
        for cheap_cost in [0i64, 1] {
            let request = serde_json::json!({
                "items": [{"id": "i", "quantity": 2, "dimensions": {"length":"10", "width":"10", "height":"10"}}],
                "containers": [
                    {"id":"cheap", "inner_dimensions":{"length":"1", "width":"1", "height":"1"}, "cost_minor":cheap_cost, "max_items":1},
                    {"id":"big", "inner_dimensions":{"length":"10", "width":"10", "height":"10"}, "cost_minor":9007199254740991i64, "max_items":1}
                ]
            }).to_string();
            let mut result = packed(&request);
            assert_eq!(
                result["optimality"],
                serde_json::json!({"code":"not_proven"})
            );
            assert!(issues(&request, &result).is_empty());
            let absolute = 18014398509481982i64 - 2 * cheap_cost;
            result["optimality"] = serde_json::json!({"code":"not_proven", "lower_bound":[0,2,2*cheap_cost], "gap_key":2, "absolute_gap":absolute});
            if cheap_cost != 0 {
                result["optimality"]["relative_gap"] =
                    serde_json::json!({"numerator":absolute,"denominator":2*cheap_cost});
            }
            assert!(
                issues(&request, &result)
                    .iter()
                    .any(|issue| issue.code == "optimality_gap_mismatch")
            );
        }
    }
}
