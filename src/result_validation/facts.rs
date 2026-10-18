//! The result's own claims about the search: counters, counted-work limits, start records and
//! the feasibility, termination and optimality facts.

use std::cmp::Ordering;

use super::json::{EMPTY_LIST, EMPTY_STR, Json, Res};
use super::pyfmt::{bool_repr, py_repr, py_str, str_repr};
use super::{Issue, MalformedResult};

const METRICS: [&str; 7] = [
    "candidate_points_considered",
    "orientations_considered",
    "feasible_candidates",
    "collision_checks",
    "support_checks",
    "space_partitions",
    "search_nodes_expanded",
];

fn algorithm(result: &Json) -> Res<&Json> {
    let algorithm = result.at("algorithm")?;
    if !algorithm.is_dict() {
        return Err(MalformedResult::contract(
            "algorithm must be an object".to_owned(),
        ));
    }
    Ok(algorithm)
}

pub(crate) fn check_algorithm_metrics(result: &Json, issues: &mut Vec<Issue>) -> Res<()> {
    let algorithm = algorithm(result)?;
    let metrics = algorithm.get_null("metrics")?;
    let counters_well_formed = metrics.is_dict()
        && METRICS.iter().all(|field| {
            matches!(metrics.get_null(field), Ok(value) if value.strict_int().is_some_and(|count| count >= 0))
        });
    if !counters_well_formed {
        issues.push(Issue::new(
            "solver_metrics_mismatch",
            "algorithm.metrics must contain every non-negative integer counter".to_owned(),
        ));
        return Ok(());
    }
    let legacy_candidates = algorithm.get_null("candidates_evaluated")?.strict_int();
    let legacy_attempts = algorithm.get_null("placements_attempted")?.strict_int();
    let mirrors = legacy_candidates.is_some()
        && legacy_attempts.is_some()
        && legacy_candidates == metrics.at("feasible_candidates")?.strict_int()
        && legacy_attempts == metrics.at("orientations_considered")?.strict_int();
    if !mirrors {
        issues.push(Issue::new(
            "solver_metrics_mismatch",
            "legacy candidate counters must mirror algorithm.metrics".to_owned(),
        ));
    }
    Ok(())
}

/// Counted-work limits are respected, and the stop flag is evidenced by one of them.
pub(crate) fn check_effort_budget(
    request: &Json,
    result: &Json,
    issues: &mut Vec<Issue>,
) -> Res<()> {
    let algorithm = result.at("algorithm")?;
    let effort_limited = match algorithm.get_null("effort_limit_reached")? {
        Json::Bool(flag) => *flag,
        _ => {
            return Err(MalformedResult::contract(
                "algorithm.effort_limit_reached must be a boolean".to_owned(),
            ));
        }
    };
    let empty = Json::Dict(Vec::new());
    let budget = request
        .get_or("configuration", &empty)?
        .get_null("effort_budget")?;
    if budget.is_null() {
        if effort_limited {
            issues.push(Issue::new(
                "effort_budget_mismatch",
                "effort_limit_reached is true but the request has no effort_budget".to_owned(),
            ));
        }
        return Ok(());
    }
    if !budget.is_dict() {
        return Err(MalformedResult::contract(
            "config.effort_budget must be an object".to_owned(),
        ));
    }
    let metrics = algorithm.get_null("metrics")?;
    if !metrics.is_dict() {
        return Ok(());
    }
    let mut reached = false;
    for (budget_name, metric_name) in [
        ("max_candidates_evaluated", "feasible_candidates"),
        ("max_placement_attempts", "orientations_considered"),
        ("max_search_nodes", "search_nodes_expanded"),
    ] {
        let limit = budget.get_null(budget_name)?;
        let Some(value) = metrics.get_null(metric_name)?.strict_int() else {
            continue;
        };
        if limit.is_null() {
            continue;
        }
        let order = Json::Int(value).py_cmp(limit)?;
        if order == Ordering::Greater {
            issues.push(Issue::new(
                "effort_budget_exceeded",
                format!(
                    "{metric_name} is {value}, above {budget_name}={}",
                    py_str(limit)
                ),
            ));
        }
        reached = reached || order != Ordering::Less;
    }
    let restart_limit = budget.get_null("max_restarts")?;
    let starts = result
        .get_or("termination", &empty)?
        .get_or("starts", &EMPTY_LIST)?;
    if let (Some(limit), Json::List(starts)) = (restart_limit.py_int_instance(), starts) {
        let started = starts
            .iter()
            .filter(|start| {
                start.is_dict() && matches!(start.get_null("started"), Ok(Json::Bool(true)))
            })
            .count() as i128;
        if started > limit {
            issues.push(Issue::new(
                "effort_budget_exceeded",
                format!(
                    "started {started} solver runs, above max_restarts={}",
                    py_str(restart_limit)
                ),
            ));
        }
        reached = reached || started >= limit;
    }
    if effort_limited && !reached {
        issues.push(Issue::new(
            "effort_budget_mismatch",
            "effort_limit_reached is true but no configured counted-work boundary was reached"
                .to_owned(),
        ));
    }
    Ok(())
}

fn flag(algorithm: &Json, name: &str) -> Res<bool> {
    match algorithm.get_null(name)? {
        Json::Bool(value) => Ok(*value),
        _ => Err(MalformedResult::contract(format!(
            "algorithm.{name} must be a boolean"
        ))),
    }
}

fn fact_code<'a>(result: &'a Json, name: &str) -> Res<&'a str> {
    let fact = result.at(name)?;
    if !fact.is_dict() {
        return Err(MalformedResult::contract(format!(
            "{name} must be an object"
        )));
    }
    match fact.get_null("code")? {
        Json::Str(code) if !code.is_empty() => Ok(code),
        _ => Err(MalformedResult::contract(format!(
            "{name}.code must be a non-empty string"
        ))),
    }
}

pub(crate) fn check_result_facts(result: &Json, issues: &mut Vec<Issue>) -> Res<()> {
    let algorithm = algorithm(result)?;
    let timed_out = flag(algorithm, "time_limit_reached")?;
    let effort_limited = flag(algorithm, "effort_limit_reached")?;
    if timed_out && effort_limited {
        issues.push(Issue::new(
            "result_fact_mismatch",
            "time_limit_reached and effort_limit_reached cannot both be true".to_owned(),
        ));
    }
    let termination = result.at("termination")?;
    if !termination.is_dict() {
        return Err(MalformedResult::contract(
            "termination must be an object".to_owned(),
        ));
    }
    check_starts(termination, algorithm, timed_out, effort_limited, issues)?;

    let status = result.at("status")?;
    let complete = result.at("complete")?.truthy();
    let invalid = status.is_str("invalid_result");
    let expected_feasibility = if status.is_str("infeasible") {
        "infeasible"
    } else if complete && !invalid {
        "feasible"
    } else {
        "unknown"
    };
    let expected_termination = if invalid {
        "error"
    } else if timed_out {
        "time_limit"
    } else if effort_limited {
        "effort_limit"
    } else {
        "complete"
    };
    let expected_optimality = if status.is_str("optimal") {
        "proven_optimal"
    } else if status.is_str("infeasible") {
        "proven_infeasible"
    } else if !complete && !invalid {
        "best_found"
    } else {
        "not_proven"
    };
    for (name, expected) in [
        ("feasibility", expected_feasibility),
        ("termination", expected_termination),
        ("optimality", expected_optimality),
    ] {
        let actual = fact_code(result, name)?;
        if actual != expected {
            issues.push(Issue::new(
                "result_fact_mismatch",
                format!(
                    "{name}.code is {}, expected {}",
                    str_repr(actual),
                    str_repr(expected)
                ),
            ));
        }
    }
    Ok(())
}

fn check_starts(
    termination: &Json,
    algorithm: &Json,
    timed_out: bool,
    effort_limited: bool,
    issues: &mut Vec<Issue>,
) -> Res<()> {
    let starts = match termination.get_null("starts")? {
        Json::List(starts) if !starts.is_empty() => starts,
        _ => {
            issues.push(Issue::new(
                "termination_aggregate_mismatch",
                "termination.starts must contain at least one start".to_owned(),
            ));
            return Ok(());
        }
    };
    let mut malformed = false;
    for start in starts {
        if !start.is_dict() || !matches!(start.get_null("id")?, Json::Str(_)) {
            malformed = true;
            continue;
        }
        let mut flags = [false; 5];
        let mut booleans = true;
        for (slot, name) in [
            "started",
            "completed",
            "truncated",
            "selected",
            "global_deadline_reached",
        ]
        .iter()
        .enumerate()
        {
            match start.get_null(name)? {
                Json::Bool(value) => flags[slot] = *value,
                _ => booleans = false,
            }
        }
        if !booleans {
            malformed = true;
            continue;
        }
        let [started, completed, truncated, _, _] = flags;
        if (completed && (!started || truncated))
            || (truncated && (!started || completed))
            || (!started && (completed || truncated))
        {
            issues.push(Issue::new(
                "start_record_mismatch",
                format!("{}: inconsistent lifecycle flags", py_str(start.at("id")?)),
            ));
        }
    }
    if malformed {
        issues.push(Issue::new(
            "start_record_mismatch",
            "a start record is missing its id or boolean lifecycle fields".to_owned(),
        ));
    }
    let is_true = |start: &Json, name: &str| {
        start.is_dict() && matches!(start.get_null(name), Ok(Json::Bool(true)))
    };
    let selected: Vec<&Json> = starts
        .iter()
        .filter(|start| is_true(start, "selected"))
        .collect();
    if selected.len() != 1 {
        issues.push(Issue::new(
            "termination_aggregate_mismatch",
            format!("expected one selected start, found {}", selected.len()),
        ));
        return Ok(());
    }
    if !starts.iter().all(Json::is_dict) {
        return Ok(());
    }
    let winner = selected[0];
    let aggregate = [
        (
            "any_start_truncated",
            starts.iter().any(|start| is_true(start, "truncated")),
        ),
        (
            "all_required_starts_completed",
            starts.iter().all(|start| is_true(start, "completed")),
        ),
        ("winning_start_truncated", is_true(winner, "truncated")),
        (
            "global_deadline_reached",
            starts
                .iter()
                .any(|start| is_true(start, "global_deadline_reached")),
        ),
    ];
    for (name, expected) in aggregate {
        let reported = termination.get_null(name)?;
        if !matches!(reported, Json::Bool(value) if *value == expected) {
            issues.push(Issue::new(
                "termination_aggregate_mismatch",
                format!(
                    "{name} is {}, expected {}",
                    py_repr(reported),
                    bool_repr(expected)
                ),
            ));
        }
    }
    let solver = algorithm.get_null("solver")?;
    let selected_id = winner.get_or("id", &EMPTY_STR)?;
    if let Json::Str(solver) = solver
        && !(selected_id.is_str(solver)
            || selected_id
                .text("a start id")?
                .starts_with(&format!("{solver}#")))
    {
        issues.push(Issue::new(
            "winning_start_mismatch",
            format!(
                "selected start {} does not identify solver {}",
                py_repr(selected_id),
                str_repr(solver)
            ),
        ));
    }
    let (_, global_deadline) = aggregate[3];
    let (_, winning_truncated) = aggregate[2];
    if global_deadline && !timed_out {
        issues.push(Issue::new(
            "legacy_time_limit_mismatch",
            "global_deadline_reached requires time_limit_reached".to_owned(),
        ));
    }
    if (global_deadline || winning_truncated) != (timed_out || effort_limited) {
        issues.push(Issue::new(
            "termination_limit_mismatch",
            "selected-start truncation must identify exactly one reached limit".to_owned(),
        ));
    }
    Ok(())
}
