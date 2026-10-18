//! The summary, the item accounting, and the proof behind every unpacked reason.

use std::cmp::Ordering;

use super::Issue;
use super::geometry::ROTATIONS;
use super::json::{EMPTY_STR, Json, Ordered, Res};
use super::num::{add, mul};
use super::pyfmt::{py_repr, py_str, str_repr};
use super::spec::{ContainerSpec, ItemSpec, rotation_axes};

pub(crate) fn check_structure(result: &Json, issues: &mut Vec<Issue>) -> Res<()> {
    let summary = result.at("summary")?;
    let containers = result.at("containers")?.list("result.containers")?;
    let mut packed = 0usize;
    for container in containers {
        packed += container.at("placements")?.list("placements")?.len();
    }
    let unpacked = result
        .at("unpacked_items")?
        .list("result.unpacked_items")?
        .len();
    for (field, actual) in [
        ("container_count", containers.len()),
        ("packed_item_count", packed),
        ("unpacked_item_count", unpacked),
    ] {
        let reported = summary.at(field)?;
        if !reported.eq_int(actual as i128) {
            issues.push(Issue::new(
                "summary_mismatch",
                format!("{field} {} != {actual}", py_str(reported)),
            ));
        }
    }
    let complete = result.at("complete")?;
    if !complete.eq_int(i128::from(unpacked == 0)) {
        issues.push(Issue::new(
            "complete_mismatch",
            format!("complete={} with {unpacked} unpacked", py_str(complete)),
        ));
    }
    Ok(())
}

/// Participating `require_container_tag` rules, resolved from the request alone -- a second
/// implementation of docs/POLICY-RULES.md, so a resolution bug shared with an engine cannot
/// hide behind agreement with it.
pub(crate) fn container_tag_rules(request: &Json) -> Res<Vec<Json>> {
    let policy = request.get_null("policy")?;
    if !policy.is_dict() {
        return Ok(Vec::new());
    }
    let declared = policy.get_null("rules")?;
    let Json::List(declared) = declared else {
        return Ok(Vec::new());
    };
    if declared.is_empty() {
        return Ok(Vec::new());
    }
    let Some(as_of) = policy.get_null("as_of")?.py_int_instance() else {
        return Ok(Vec::new());
    };
    let empty = Json::Dict(Vec::new());
    let shipment = policy.get_null("shipment")?;
    let shipment = if shipment.truthy() { shipment } else { &empty };
    let zero = Json::Int(0);
    let mut latest: Ordered<Json> = Ordered::default();
    for rule in declared {
        if !rule.is_dict() || !rule.has("require_container_tag") {
            continue;
        }
        let applies_to = rule.get_null("applies_to")?;
        let applies_to = if applies_to.truthy() {
            applies_to
        } else {
            &empty
        };
        let pairs = applies_to.as_dict().ok_or_else(|| {
            super::MalformedResult::shape("applies_to must be an object".to_owned())
        })?;
        let mut applies = true;
        for (fact, value) in pairs {
            if !shipment.get_null(fact)?.py_eq(value) {
                applies = false;
                break;
            }
        }
        if !applies {
            continue;
        }
        if rule
            .get_or("effective_at", &zero)?
            .py_cmp(&Json::Int(as_of))?
            == Ordering::Greater
        {
            continue;
        }
        let id = rule.get_null("id")?;
        let newer = match latest.get(id)? {
            None => true,
            Some(current) => {
                let mine = Json::List(vec![
                    rule.get_or("effective_at", &zero)?.clone(),
                    rule.get_or("version", &zero)?.clone(),
                ]);
                let theirs = Json::List(vec![
                    current.get_or("effective_at", &zero)?.clone(),
                    current.get_or("version", &zero)?.clone(),
                ]);
                mine.py_cmp(&theirs)? == Ordering::Greater
            }
        };
        if newer {
            latest.insert(id, rule.clone())?;
        }
    }
    Ok(latest.values().cloned().collect())
}

pub(crate) struct Context<'a> {
    pub(crate) items: &'a Ordered<ItemSpec>,
    pub(crate) containers: &'a Ordered<ContainerSpec>,
    pub(crate) clearance: i128,
    pub(crate) rules: &'a [Json],
}

pub(crate) fn check_accounting(
    result: &Json,
    context: &Context<'_>,
    issues: &mut Vec<Issue>,
) -> Res<()> {
    let mut seen: Ordered<()> = Ordered::default();
    let mut per_type: Ordered<i128> = Ordered::default();
    let count = |per_type: &mut Ordered<i128>, item_type: &Json| -> Res<()> {
        let next = per_type.get(item_type)?.copied().unwrap_or(0) + 1;
        per_type.insert(item_type, next)
    };
    for container in result.at("containers")?.list("result.containers")? {
        for placement in container.at("placements")?.list("placements")? {
            let item_id = placement.at("item_id")?;
            if seen.contains(item_id)? {
                issues.push(Issue::new("duplicate_item", py_str(item_id)));
            }
            seen.insert(item_id, ())?;
            count(&mut per_type, placement.at("item_type")?)?;
        }
    }
    let algorithm = result.at("algorithm")?;
    let timed_out = algorithm.at("time_limit_reached")?.truthy();
    let effort_limited = algorithm.at("effort_limit_reached")?.truthy();
    for unpacked in result.at("unpacked_items")?.list("result.unpacked_items")? {
        let item_id = unpacked.at("item_id")?;
        if seen.contains(item_id)? {
            issues.push(Issue::new(
                "duplicate_item",
                format!("{} is both packed and unpacked", py_str(item_id)),
            ));
        }
        seen.insert(item_id, ())?;
        let item_type = unpacked.at("item_type")?;
        count(&mut per_type, item_type)?;
        if !unpacked.get_null("reason")?.truthy() {
            issues.push(Issue::new("missing_reason", py_str(item_id)));
        }
        let spec = context.items.get(unpacked.at("item_type")?)?;
        check_reason_proof(unpacked, spec, context, timed_out, effort_limited, issues)?;
    }

    for (item_type, count) in per_type.iter() {
        match context.items.get(item_type)? {
            None => issues.push(Issue::new("unknown_item", py_str(item_type))),
            Some(spec) if !spec.quantity.eq_int(*count) => issues.push(Issue::new(
                "quantity_mismatch",
                format!(
                    "{}: {count} accounted for, {} requested",
                    py_str(item_type),
                    py_str(&spec.quantity)
                ),
            )),
            Some(_) => {}
        }
    }
    for (item_type, spec) in context.items.iter() {
        if !per_type.contains(item_type)? {
            issues.push(Issue::new(
                "quantity_mismatch",
                format!(
                    "{}: 0 accounted for, {} requested",
                    py_str(item_type),
                    py_str(&spec.quantity)
                ),
            ));
        }
    }
    Ok(())
}

/// Whether one envelope of this size fits the empty container: its frame and, for a flat bag,
/// both girth sums with the envelope alone at the origin.
pub(crate) fn fits_empty(container: &ContainerSpec, size: [i128; 3]) -> Res<bool> {
    if (0..3).any(|axis| size[axis] > container.inner[axis]) {
        return Ok(false);
    }
    Ok(!container.flat_bag
        || (add(size[0], size[2])? <= container.inner[0]
            && add(size[1], size[2])? <= container.inner[1]))
}

/// `any(_fits_empty(container, rotated envelope) for container ... for rotation ...)`,
/// stopping at the first fit as Python's generator does, so an unknown rotation is only an
/// error when the search reaches it.
fn fits_some(spec: &ItemSpec, context: &Context<'_>, rotations: &[Json]) -> Res<bool> {
    let margin = mul(2, context.clearance)?;
    for container in context.containers.values() {
        for rotation in rotations {
            let axes = rotation_axes(rotation.text("a rotation")?)?;
            let size = [
                add(spec.dimensions[axes[0]], margin)?,
                add(spec.dimensions[axes[1]], margin)?,
                add(spec.dimensions[axes[2]], margin)?,
            ];
            if fits_empty(container, size)? {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

const PROVEN: [&str; 5] = [
    "no_compatible_container_dimensions",
    "payload_exceeded",
    "no_eligible_container",
    "rotation_restricted",
    "policy_rule",
];
const LIMITED: [&str; 2] = ["time_limit", "effort_limit"];
const OBSERVED: [&str; 4] = [
    "no_feasible_placement",
    "search_exhausted",
    "exact_search_incomplete",
    "insufficient_support",
];

fn check_reason_proof(
    unpacked: &Json,
    spec: Option<&ItemSpec>,
    context: &Context<'_>,
    timed_out: bool,
    effort_limited: bool,
    issues: &mut Vec<Issue>,
) -> Res<()> {
    let item_id = py_str(unpacked.get_or("item_id", &Json::str_of("?"))?);
    let reason = unpacked.get_or("reason", &EMPTY_STR)?;
    let proof = unpacked.get_null("proof")?;
    if !proof.is_dict() {
        issues.push(Issue::new("missing_reason_proof", item_id));
        return Ok(());
    }
    reason.hash_key()?;
    let is = |names: &[&str]| names.iter().any(|name| reason.is_str(name));
    let expected = if is(&PROVEN) {
        "proven"
    } else if is(&LIMITED) {
        "unknown_due_to_limit"
    } else if is(&OBSERVED) {
        "observed"
    } else {
        "inferred"
    };
    let level = proof.get_null("level")?;
    if !level.is_str(expected) {
        issues.push(Issue::new(
            "reason_proof_mismatch",
            format!(
                "{item_id}: {} has proof {}, expected {}",
                py_repr(reason),
                py_repr(level),
                str_repr(expected)
            ),
        ));
    }
    if reason.is_str("time_limit") && !timed_out {
        issues.push(Issue::new(
            "reason_limit_mismatch",
            format!("{item_id}: time_limit reason without a reached time limit"),
        ));
    }
    if reason.is_str("effort_limit") && !effort_limited {
        issues.push(Issue::new(
            "reason_limit_mismatch",
            format!("{item_id}: effort_limit reason without a reached effort limit"),
        ));
    }
    if let Some(spec) = spec {
        check_proven_reason(unpacked, reason, spec, context, &item_id, issues)?;
    }
    check_observations(proof, reason, &item_id, issues)
}

fn check_proven_reason(
    unpacked: &Json,
    reason: &Json,
    spec: &ItemSpec,
    context: &Context<'_>,
    item_id: &str,
    issues: &mut Vec<Issue>,
) -> Res<()> {
    let own = &spec.allowed_rotations;
    let every: Vec<Json> = ROTATIONS
        .iter()
        .map(|(code, _)| Json::str_of(code))
        .collect();
    let every = &every;
    let invalid = |detail: String| Issue::new("invalid_proven_reason", detail);

    if reason.is_str("no_compatible_container_dimensions") {
        if fits_some(spec, context, own)? {
            issues.push(invalid(format!(
                "{item_id}: at least one allowed orientation fits a container"
            )));
        }
        if fits_some(spec, context, every)? {
            issues.push(invalid(format!(
                "{item_id}: fits at least one container once every rotation is tried, not only its own allowed set -- should be rotation_restricted"
            )));
        }
    }
    if reason.is_str("rotation_restricted") {
        if fits_some(spec, context, own)? {
            issues.push(invalid(format!(
                "{item_id}: already fits a container within its own allowed rotations"
            )));
        }
        if !fits_some(spec, context, every)? {
            issues.push(invalid(format!(
                "{item_id}: does not fit any container even with every rotation tried -- should be no_compatible_container_dimensions"
            )));
        }
    }
    if reason.is_str("payload_exceeded")
        && context.containers.values().any(|container| {
            container
                .max_payload
                .is_none_or(|limit| spec.weight <= limit)
        })
    {
        issues.push(invalid(format!(
            "{item_id}: its weight is within at least one container payload limit"
        )));
    }
    if reason.is_str("no_eligible_container")
        && context.containers.values().any(|container| {
            spec.eligible_container_tags.is_empty()
                || !spec.eligible_container_tags.is_disjoint(&container.tags)
        })
    {
        issues.push(invalid(format!(
            "{item_id}: at least one container satisfies its eligible tags"
        )));
    }
    if reason.is_str("policy_rule") {
        check_policy_citation(unpacked, spec, context, item_id, issues)?;
    }
    Ok(())
}

/// Two claims, both checked: that some rule rules this item out of every container, and that
/// the rule the result *names* is that rule.
fn check_policy_citation(
    unpacked: &Json,
    spec: &ItemSpec,
    context: &Context<'_>,
    item_id: &str,
    issues: &mut Vec<Issue>,
) -> Res<()> {
    let mut blocking = Vec::new();
    for rule in context.rules {
        let requirement = rule.at("require_container_tag")?;
        let item_tag = requirement.get_null("item_tag")?;
        if !holds(&spec.tags, item_tag)? {
            continue;
        }
        let mut admitted = false;
        for container in context.containers.values() {
            if holds(
                &container.tags,
                rule.at("require_container_tag")?
                    .get_null("container_tag")?,
            )? {
                admitted = true;
                break;
            }
        }
        if !admitted {
            blocking.push(rule);
        }
    }
    if blocking.is_empty() {
        issues.push(Issue::new(
            "invalid_proven_reason",
            format!("{item_id}: no participating policy rule rules it out of every container"),
        ));
        return Ok(());
    }
    let details = unpacked.get_null("details")?;
    let cited = if details.truthy() {
        details
            .list("details")?
            .iter()
            .map(|detail| detail.text("details").map(str::to_owned))
            .collect::<Res<Vec<_>>>()?
            .join(" ")
    } else {
        String::new()
    };
    let mut named = false;
    for rule in blocking {
        if cited.contains(&format!(
            "{}@{}",
            py_str(rule.at("id")?),
            py_str(rule.at("version")?)
        )) {
            named = true;
            break;
        }
    }
    if !named {
        issues.push(Issue::new(
            "invalid_proven_reason",
            format!(
                "{item_id}: cites {}, which names no rule that actually blocks it",
                str_repr(&cited)
            ),
        ));
    }
    Ok(())
}

/// `value in tags` for a frozenset of strings; an unhashable value is Python's `TypeError`.
pub(crate) fn holds(tags: &std::collections::BTreeSet<String>, value: &Json) -> Res<bool> {
    value.hash_key()?;
    Ok(matches!(value, Json::Str(tag) if tags.contains(tag)))
}

fn check_observations(
    proof: &Json,
    reason: &Json,
    item_id: &str,
    issues: &mut Vec<Issue>,
) -> Res<()> {
    let observations = match proof.get_null("observations")? {
        Json::List(observations) if !observations.is_empty() => observations,
        _ => {
            issues.push(Issue::new(
                "missing_reason_observations",
                item_id.to_owned(),
            ));
            return Ok(());
        }
    };
    let mut observed_reason = false;
    for observation in observations {
        if !observation.is_dict() {
            issues.push(Issue::new(
                "malformed_reason_observation",
                item_id.to_owned(),
            ));
            continue;
        }
        let code = observation.get_null("code")?;
        let well_formed = matches!(code, Json::Str(text) if !text.is_empty())
            && observation
                .get_null("count")?
                .py_int_instance()
                .is_some_and(|count| count >= 1)
            && matches!(observation.get_null("details")?, Json::List(_));
        if !well_formed {
            issues.push(Issue::new(
                "malformed_reason_observation",
                item_id.to_owned(),
            ));
            continue;
        }
        if code.py_eq(reason) {
            observed_reason = true;
        }
    }
    if !observed_reason {
        issues.push(Issue::new(
            "reason_observation_mismatch",
            format!("{item_id}: {}", py_str(reason)),
        ));
    }
    Ok(())
}
