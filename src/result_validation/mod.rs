//! `validate_json`: language-neutral validation of a packing result.
//!
//! A port of the reference validator, `packvium.validate` (packvium-python's
//! `result_validation.py`), over the JSON documents rather than this crate's model. It shares no
//! code with the solvers or with [`crate::IndependentValidator`]: units, geometry, support and load
//! are all recomputed here from the request and the reported placements, because a validator
//! built on the engine's own code would agree with the engine's bugs. Every issue code and every
//! detail string is the reference's, Python spelling included, so one result gets the same issue
//! list from every package (docs/VALIDATION-CONTRACT.md).
//!
//! Where the reference computes on a value of the wrong type (a float tick, a string quantity)
//! this port refuses the result instead of following Python's arithmetic, and it refuses any
//! value too large to compute exactly in `i128`. Both are [`MalformedResult`]s.

mod accounting;
mod container;
mod facts;
mod geometry;
mod json;
mod lattice;
mod nonprintable;
mod num;
mod objective;
mod pyfmt;
mod spec;
mod stacking;
mod units;

use std::cell::Cell;
use std::fmt;

use serde::Serialize;

use json::{Json, Ordered, Res};
use units::{LENGTH, ratio_of, to_ticks};

/// One way a result fails to be a sound answer to its request.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ResultIssue {
    pub code: String,
    pub detail: String,
}

pub(crate) type Issue = ResultIssue;

impl ResultIssue {
    pub(crate) fn new(code: &str, detail: String) -> ResultIssue {
        ResultIssue {
            code: code.to_owned(),
            detail,
        }
    }
}

impl fmt::Display for ResultIssue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.code, self.detail)
    }
}

/// How the reference answers the same input.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RefusalKind {
    /// The reference raises `MalformedResult` with this message. Where the reference's message
    /// quotes a Python exception (a placement or lattice summary missing a field), the text
    /// after that quote is this port's own account of the same failure.
    Contract,
    /// The reference fails on the input some other way (a missing key, a value of the wrong
    /// type), or a value is beyond what this port computes exactly. The message is this port's.
    Shape,
}

/// The documents do not have the shape the contract requires: a contract break, not a packing
/// defect, so no issue list is returned.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct MalformedResult {
    pub kind: RefusalKind,
    pub message: String,
}

impl MalformedResult {
    pub(crate) fn contract(message: String) -> MalformedResult {
        MalformedResult {
            kind: RefusalKind::Contract,
            message,
        }
    }

    pub(crate) fn shape(message: String) -> MalformedResult {
        MalformedResult {
            kind: RefusalKind::Shape,
            message,
        }
    }
}

impl fmt::Display for MalformedResult {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for MalformedResult {}

/// A packed container's id as an issue prints it. The reference reads `packed["id"]` only
/// while formatting an issue, so a missing id fails only once an issue needs it; `verify`
/// reports that after the check, which aborts the run exactly as the reference's `KeyError` does.
pub(crate) struct Label {
    text: Option<String>,
    missing_used: Cell<bool>,
}

impl Label {
    pub(crate) fn of(packed: &Json) -> Res<Label> {
        Ok(Label {
            text: packed.get("id")?.map(pyfmt::py_str),
            missing_used: Cell::new(false),
        })
    }

    pub(crate) fn verify(&self) -> Res<()> {
        if self.missing_used.get() {
            return Err(MalformedResult::shape(
                "a packed container with an issue has no id".to_owned(),
            ));
        }
        Ok(())
    }
}

impl fmt::Display for Label {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.text {
            Some(text) => formatter.write_str(text),
            None => {
                self.missing_used.set(true);
                formatter.write_str("?")
            }
        }
    }
}

const REQUIRED_KEYS: [&str; 10] = [
    "status",
    "feasibility",
    "termination",
    "optimality",
    "complete",
    "algorithm",
    "summary",
    "containers",
    "unpacked_items",
    "score",
];

/// Every way `result` fails to be a sound answer to `request`, both given as JSON text. An
/// empty list means valid.
///
/// # Errors
///
/// [`MalformedResult`] when either document is not JSON or the result does not have the
/// documented shape at all.
pub fn validate_json(request: &str, result: &str) -> Result<Vec<ResultIssue>, MalformedResult> {
    let request = json::parse(request, "the request")?;
    let result = json::parse(result, "the result")?;
    validate(&request, &result)
}

/// [`validate_json`] as one JSON document, for a binding that can only hand text across:
/// `{"issues": [{"code": ..., "detail": ...}]}`, or `{"refusal": {"kind": ..., "message": ...}}`
/// when the documents cannot be judged. Never fails; a refusal is part of the answer.
pub fn validate_json_outcome(request: &str, result: &str) -> String {
    let outcome = match validate_json(request, result) {
        Ok(issues) => serde_json::json!({ "issues": issues }),
        Err(refusal) => serde_json::json!({ "refusal": refusal }),
    };
    outcome.to_string()
}

fn validate(request: &Json, result: &Json) -> Res<Vec<Issue>> {
    for key in REQUIRED_KEYS {
        if !result.has(key) {
            return Err(MalformedResult::contract(format!(
                "result is missing {}",
                pyfmt::str_repr(key)
            )));
        }
    }
    let empty = Json::Dict(Vec::new());
    let unit = request
        .get_or("units", &empty)?
        .get_or("length", &Json::str_of("mm"))?
        .text("units.length")?
        .to_owned();
    let configuration = request.get_or("configuration", &empty)?;
    let clearance = to_ticks(
        configuration.get_or("clearance", &Json::Int(0))?,
        &unit,
        LENGTH,
    )?;
    let global_support = ratio_of(configuration.get_or("minimum_support_ratio", &Json::Int(0))?)?;
    let items = spec::parse_items(request, &unit)?;
    let containers = spec::parse_containers(request, &unit)?;

    let mut issues = Vec::new();
    let validated =
        lattice::materialize(request, result, &items, &containers, clearance, &mut issues)?;
    accounting::check_structure(&validated, &mut issues)?;
    facts::check_algorithm_metrics(&validated, &mut issues)?;
    facts::check_effort_budget(request, &validated, &mut issues)?;
    facts::check_result_facts(&validated, &mut issues)?;
    let rules = accounting::container_tag_rules(request)?;
    let context = accounting::Context {
        items: &items,
        containers: &containers,
        clearance,
        rules: &rules,
    };
    accounting::check_accounting(&validated, &context, &mut issues)?;
    check_infeasible_proof(&validated, &mut issues)?;

    let fabricated = check_containers(
        &validated,
        result,
        &items,
        &containers,
        clearance,
        global_support,
        &mut issues,
    )?;
    check_container_counts(&validated, configuration, &containers, &mut issues)?;

    objective::check_fixed_placements(request, &validated, &unit, &mut issues)?;
    objective::check_groups(request, &validated, &mut issues)?;
    objective::check_solver_selection(request, &validated, &mut issues)?;
    let inputs = objective::Inputs {
        containers: &containers,
        items: &items,
        fabricated: &fabricated,
        clearance,
    };
    objective::check_objective(request, &validated, &inputs, &mut issues)?;
    Ok(issues)
}

fn check_infeasible_proof(result: &Json, issues: &mut Vec<Issue>) -> Res<()> {
    if !result.at("status")?.is_str("infeasible") {
        return Ok(());
    }
    let empty = Json::Dict(Vec::new());
    for unpacked in result.at("unpacked_items")?.list("result.unpacked_items")? {
        if !unpacked
            .get_or("proof", &empty)?
            .get_null("level")?
            .is_str("proven")
        {
            issues.push(Issue::new(
                "unproven_infeasible",
                "infeasible status requires a proven rejection for every unpacked item".to_owned(),
            ));
            break;
        }
    }
    Ok(())
}

/// Each packed container against its type, recomputing a made-to-measure box first; returns
/// those recomputed boxes by container id, for the objective.
fn check_containers(
    validated: &Json,
    reported: &Json,
    items: &Ordered<spec::ItemSpec>,
    containers: &Ordered<spec::ContainerSpec>,
    clearance: i128,
    global_support: num::Ratio,
    issues: &mut Vec<Issue>,
) -> Res<Ordered<spec::ContainerSpec>> {
    let mut fabricated = Ordered::default();
    let reported = reported.at("containers")?.list("result.containers")?;
    for (packed, original) in validated
        .at("containers")?
        .list("result.containers")?
        .iter()
        .zip(reported)
    {
        let container_type = packed.at("container_type")?;
        let Some(spec) = containers.get(container_type)? else {
            issues.push(Issue::new(
                "unknown_container_type",
                pyfmt::py_str(container_type),
            ));
            continue;
        };
        let spec = container::check_made_to_measure(
            packed,
            original.has("lattice_summary"),
            spec,
            items,
            clearance,
            issues,
        )?;
        if spec.made_to_measure.is_some() {
            fabricated.insert(packed.at("id")?, spec.clone())?;
        }
        container::check_container(packed, &spec, items, clearance, global_support, issues)?;
        container::check_achieved_extent(packed, &spec, clearance, issues)?;
    }
    Ok(fabricated)
}

fn check_container_counts(
    validated: &Json,
    configuration: &Json,
    containers: &Ordered<spec::ContainerSpec>,
    issues: &mut Vec<Issue>,
) -> Res<()> {
    let opened = validated.at("containers")?.list("result.containers")?;
    let mut inventory: Ordered<i128> = Ordered::default();
    for packed in opened {
        let container_type = packed.at("container_type")?;
        if containers.contains(container_type)? {
            let count = inventory.get(container_type)?.copied().unwrap_or(0);
            inventory.insert(container_type, count + 1)?;
        }
    }
    for (container_type, count) in inventory.iter() {
        let spec = containers
            .get(container_type)?
            .expect("an inventoried type is declared");
        if let Some(quantity) = spec.quantity
            && *count > quantity
        {
            issues.push(Issue::new(
                "container_inventory_exceeded",
                format!(
                    "{}: used {count}, available {quantity}",
                    pyfmt::py_str(container_type)
                ),
            ));
        }
    }
    if let Some(limit) = configuration
        .get_null("max_containers")?
        .opt_int("max_containers")?
        && opened.len() as i128 > limit
    {
        issues.push(Issue::new(
            "max_containers_exceeded",
            format!("{} containers opened, limit {limit}", opened.len()),
        ));
    }
    Ok(())
}
