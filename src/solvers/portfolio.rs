use super::extreme::{explain_unfit, score_solution};
use super::{
    pack_exact_one, pack_homogeneous_blocks, pack_layer_order, pack_maximal_order, pack_order,
    try_grid, unpriceable_container,
};
use crate::deadline::Deadline;
use crate::error::PackResult;
use crate::model::*;
use crate::solver::{CandidateScorer, ItemOrderStrategy, PlacementConstraint};
use std::cmp::Reverse;
use std::sync::Arc;

pub fn solve_portfolio(
    request: &PackingRequest,
    constraints: &[Arc<dyn PlacementConstraint>],
    scorers: &[Arc<dyn CandidateScorer>],
    custom_orders: &[Arc<dyn ItemOrderStrategy>],
) -> PackResult<PackingResult> {
    let deadline = Deadline::new(request.config.time_limit_ms.max(1));
    solve_portfolio_with_deadline(request, constraints, scorers, custom_orders, &deadline)
}

pub(crate) fn solve_portfolio_with_deadline(
    request: &PackingRequest,
    constraints: &[Arc<dyn PlacementConstraint>],
    scorers: &[Arc<dyn CandidateScorer>],
    custom_orders: &[Arc<dyn ItemOrderStrategy>],
    deadline: &Deadline,
) -> PackResult<PackingResult> {
    let enabled = |name: &str| {
        request.config.solvers.is_empty()
            || request.config.solvers.iter().any(|solver| solver == name)
    };
    let explicit_selection = !request.config.solvers.is_empty();
    let instances = request.instances();
    if request
        .config
        .solvers
        .iter()
        .any(|solver| solver == "exact_small")
        && instances.len() > request.config.exact_item_limit
    {
        return Err(crate::error::PackError::InvalidInput(
            "exact-small item limit exceeded".into(),
        ));
    }
    // The grid solver derives a lattice in closed form and has no candidate-level
    // extension seam. Letting it run with a registered hard constraint would make the
    // registry return a complete result before the constraint-aware solvers are reached
    //. The emptiness check is O(1) time/O(1) space; ordinary requests keep the
    // O(r + n) fast path, while extension-bearing requests deliberately use a solver
    // capable of evaluating every candidate.
    let grid_result = if enabled("grid") {
        let lattice = if constraints.is_empty() {
            try_grid(request, deadline)
        } else {
            None
        };
        lattice.or_else(|| {
            (explicit_selection
                && request.config.solvers.len() == 1
                && request.config.solvers[0] == "grid")
                .then(|| {
                    pack_order(
                        request,
                        &instances,
                        constraints,
                        scorers,
                        "grid:fallback",
                        deadline,
                    )
                })
        })
    } else {
        None
    };
    if !explicit_selection
        && let Some(grid) = grid_result.as_ref()
        && (grid.complete() || request.config.profile == SolverProfile::Fast)
    {
        return Ok(finalize_single_start(grid.clone(), deadline.expired()));
    }
    if enabled("exact_small")
        && request.config.profile == SolverProfile::ExactSmall
        && instances.len() <= request.config.exact_item_limit
        && let Some(result) = pack_exact_one(request, &instances, constraints, scorers, deadline)
        && result.complete()
    {
        return Ok(finalize_single_start(result, deadline.expired()));
    }

    let mut orders = Vec::new();
    if request.config.profile == SolverProfile::Quality {
        orders.push((
            "small_edge".to_owned(),
            sort_small_edge(instances.clone(), &request.config.objective),
        ));
        orders.push((
            "small_volume".to_owned(),
            sort_small_volume(instances.clone(), &request.config.objective),
        ));
    }
    orders.extend([
        (
            "volume".to_owned(),
            sort_volume(instances.clone(), &request.config.objective),
        ),
        (
            "base_area".to_owned(),
            sort_base(instances.clone(), &request.config.objective),
        ),
        (
            "longest_edge".to_owned(),
            sort_edge(instances.clone(), &request.config.objective),
        ),
        (
            "weight".to_owned(),
            sort_weight(instances.clone(), &request.config.objective),
        ),
        (
            "constrained".to_owned(),
            sort_constrained(instances.clone(), &request.config.objective),
        ),
    ]);
    for custom in custom_orders {
        let mut values = instances.clone();
        custom.order(&mut values);
        orders.push((custom.name().to_owned(), values));
    }
    let target = match request.config.profile {
        SolverProfile::Fast => 1,
        SolverProfile::Balanced => request.config.multi_start_orders.min(6),
        SolverProfile::Quality => request.config.multi_start_orders.max(12),
        SolverProfile::ExactSmall => request.config.multi_start_orders.min(4),
    };
    while orders.len() < target {
        let index = orders.len() as u64;
        orders.push((
            format!("shuffle-{index}"),
            deterministic_shuffle(
                instances.clone(),
                request.config.seed ^ index.wrapping_mul(0x9E37_79B9_7F4A_7C15),
            ),
        ));
    }
    let restart_limit = request
        .config
        .effort_budget
        .and_then(|budget| budget.max_restarts)
        .unwrap_or(usize::MAX);
    orders.truncate(target.min(restart_limit));
    let per_order = if request.config.profile == SolverProfile::Quality
        && instances.iter().all(|item| item.item.group.is_none())
    {
        2
    } else {
        1
    };
    let staged_plan_search = request.config.profile == SolverProfile::Quality
        && request.config.container_plan_beam_width > 1;
    let planned_start_count = orders.len() * per_order
        + if staged_plan_search { 2 } else { 0 }
        + usize::from(
            enabled("homogeneous_blocks")
                && (explicit_selection || request.config.profile == SolverProfile::Quality),
        )
        + usize::from(!matches!(request.config.profile, SolverProfile::Fast))
        + usize::from(instances.len() <= request.config.exact_item_limit);

    let block_result = if enabled("homogeneous_blocks")
        && (explicit_selection || request.config.profile == SolverProfile::Quality)
        && !deadline.expired()
    {
        pack_homogeneous_blocks(request, &instances, constraints, deadline).or_else(|| {
            (explicit_selection
                && request.config.solvers.len() == 1
                && request.config.solvers[0] == "homogeneous_blocks")
                .then(|| {
                    pack_order(
                        request,
                        &instances,
                        constraints,
                        scorers,
                        "homogeneous_blocks:fallback",
                        deadline,
                    )
                })
        })
    } else {
        None
    };
    let mut results = if enabled("extreme_points") || enabled("maximal_spaces") {
        if staged_plan_search {
            let mut greedy_request = request.clone();
            greedy_request.config.max_candidates_per_item = 1;
            greedy_request.config.container_plan_beam_width = 1;
            greedy_request.config.container_plan_node_limit = 1;
            let greedy_orders = orders
                .iter()
                .map(|(name, order)| (format!("{name}:greedy"), order.clone()))
                .collect();
            let mut staged = run_orders(
                &greedy_request,
                constraints,
                scorers,
                greedy_orders,
                deadline,
            );
            for (name, order) in orders.iter().take(2) {
                if deadline.expired() {
                    break;
                }
                staged.extend(run_one_order(
                    request,
                    constraints,
                    scorers,
                    format!("{name}:beam"),
                    order.clone(),
                    deadline,
                ));
            }
            staged
        } else {
            run_orders(request, constraints, scorers, orders, deadline)
        }
    } else {
        Vec::new()
    };
    if let Some(result) = block_result
        && results.len() < restart_limit
    {
        results.push(result);
    }
    if let Some(grid) = grid_result {
        results.push(grid);
    }
    if enabled("layer")
        && (explicit_selection || !matches!(request.config.profile, SolverProfile::Fast))
        && !deadline.expired()
        && results.len() < restart_limit
    {
        results.push(pack_layer_order(
            request,
            &instances,
            constraints,
            scorers,
            deadline,
        ));
    }
    if enabled("exact_small")
        && instances.len() <= request.config.exact_item_limit
        && !deadline.expired()
        && results.len() < restart_limit
        && let Some(result) = pack_exact_one(request, &instances, constraints, scorers, deadline)
    {
        results.push(result);
    }

    if explicit_selection {
        results.sort_by_key(|result| {
            let base = result
                .algorithm
                .solver
                .split(':')
                .next()
                .unwrap_or_default();
            request
                .config
                .solvers
                .iter()
                .position(|name| name == base)
                .unwrap_or(usize::MAX)
        });
    }

    if results.is_empty() {
        let timed_out = deadline.expired();
        let unpacked = instances
            .iter()
            .cloned()
            .map(|instance| {
                let structural_reason = explain_unfit(request, &instance);
                let reason = if timed_out
                    && !matches!(
                        structural_reason.as_str(),
                        "no_compatible_container_dimensions"
                            | "payload_exceeded"
                            | "rotation_restricted"
                    ) {
                    "time_limit".into()
                } else {
                    structural_reason
                };
                UnpackedItem::new(instance, reason, Vec::new())
            })
            .collect::<Vec<_>>();
        let all_rejections_proven = unpacked.iter().all(|item| item.proof.level == "proven");
        let status = if timed_out {
            PackingStatus::TimeLimit
        } else if all_rejections_proven {
            PackingStatus::Infeasible
        } else {
            PackingStatus::BestFound
        };
        let score = score_solution(&[], &unpacked, &request.config);
        results.push(PackingResult {
            status,
            containers: Vec::new(),
            unpacked,
            algorithm: AlgorithmReport {
                profile: request.config.profile.as_str().into(),
                solver: "portfolio".into(),
                time_limit_reached: timed_out,
                ..Default::default()
            },
            score,
            warnings: vec!["no solver produced a placement".into()],
            alternatives: Vec::new(),
            feasibility: None,
            termination: None,
            optimality: None,
            objective: request.config.objective.clone(),
            catalog_versions_used: Vec::new(),
        });
    }
    let global_deadline_reached = deadline.expired();
    let actual_signatures = results.iter().map(signature).collect::<Vec<_>>();
    let mut start_records = results
        .iter()
        .enumerate()
        .map(|(index, result)| StartRecord {
            id: format!("{}#{}", result.algorithm.solver, index + 1),
            started: true,
            completed: !result.algorithm.time_limit_reached
                && !result.algorithm.effort_limit_reached,
            truncated: result.algorithm.time_limit_reached || result.algorithm.effort_limit_reached,
            selected: false,
            global_deadline_reached,
        })
        .collect::<Vec<_>>();
    if global_deadline_reached {
        for index in start_records.len()..planned_start_count {
            start_records.push(StartRecord {
                id: format!("not_started_due_to_global_deadline#{}", index + 1),
                started: false,
                completed: false,
                truncated: false,
                selected: false,
                global_deadline_reached: true,
            });
        }
    }
    results.sort_by(|left, right| {
        left.score
            .cmp(&right.score)
            .then_with(|| left.algorithm.solver.cmp(&right.algorithm.solver))
    });
    results.dedup_by(|left, right| signature(left) == signature(right));
    apply_portfolio_termination(
        &mut results,
        &start_records,
        &actual_signatures,
        global_deadline_reached,
    );
    let mut best = results
        .first()
        .cloned()
        .expect("portfolio always contains at least one result");
    // The sentinel is a search device, never an answer -- alternatives included. A
    // runner-up whose tariff cannot price it is dropped before the slice, so the caller
    // still receives up to top_k-1 usable packings when priceable runners exist beyond
    // an unpriceable one ( review).
    best.alternatives = results
        .into_iter()
        .skip(1)
        .filter(|result| unpriceable_container(&result.containers, &request.config).is_none())
        .take(request.config.top_k.saturating_sub(1))
        .collect();
    Ok(best)
}

fn finalize_single_start(
    mut result: PackingResult,
    global_deadline_reached: bool,
) -> PackingResult {
    let truncated = result.algorithm.time_limit_reached || result.algorithm.effort_limit_reached;
    let starts = [StartRecord {
        id: result.algorithm.solver.clone(),
        started: true,
        completed: !truncated,
        truncated,
        selected: true,
        global_deadline_reached,
    }];
    result.algorithm.time_limit_reached =
        result.algorithm.time_limit_reached || global_deadline_reached;
    if result.algorithm.time_limit_reached {
        result.algorithm.effort_limit_reached = false;
    }
    result.termination = Some(aggregate_termination(
        &starts,
        result.status == PackingStatus::InvalidResult,
    ));
    result
}

fn apply_portfolio_termination(
    results: &mut [PackingResult],
    records: &[StartRecord],
    actual_signatures: &[String],
    global_deadline_reached: bool,
) {
    for result in results.iter_mut() {
        let result_signature = signature(result);
        let selected_index = actual_signatures
            .iter()
            .enumerate()
            .find_map(|(index, candidate)| {
                (candidate == &result_signature
                    && records[index]
                        .id
                        .starts_with(&format!("{}#", result.algorithm.solver)))
                .then_some(index)
            })
            .or_else(|| {
                actual_signatures
                    .iter()
                    .position(|candidate| candidate == &result_signature)
            })
            .expect("every retained result came from a recorded start");
        let mut selected_records = records.to_vec();
        selected_records[selected_index].selected = true;
        result.algorithm.time_limit_reached =
            result.algorithm.time_limit_reached || global_deadline_reached;
        if result.algorithm.time_limit_reached {
            result.algorithm.effort_limit_reached = false;
        }
        result.termination = Some(aggregate_termination(
            &selected_records,
            result.status == PackingStatus::InvalidResult,
        ));
    }
}

fn run_orders(
    request: &PackingRequest,
    constraints: &[Arc<dyn PlacementConstraint>],
    scorers: &[Arc<dyn CandidateScorer>],
    orders: Vec<(String, Vec<ItemInstance>)>,
    deadline: &Deadline,
) -> Vec<PackingResult> {
    #[cfg(not(target_arch = "wasm32"))]
    if request.config.parallel && orders.len() > 1 {
        return std::thread::scope(|scope| {
            let handles = orders
                .into_iter()
                .map(|(name, order)| {
                    scope.spawn(move || {
                        run_one_order(request, constraints, scorers, name, order, deadline)
                    })
                })
                .collect::<Vec<_>>();
            handles
                .into_iter()
                .filter_map(|handle| handle.join().ok())
                .flatten()
                .collect()
        });
    }

    orders
        .into_iter()
        .take_while(|_| !deadline.expired())
        .flat_map(|(name, order)| {
            run_one_order(request, constraints, scorers, name, order, deadline)
        })
        .collect()
}

fn run_one_order(
    request: &PackingRequest,
    constraints: &[Arc<dyn PlacementConstraint>],
    scorers: &[Arc<dyn CandidateScorer>],
    name: String,
    order: Vec<ItemInstance>,
    deadline: &Deadline,
) -> Vec<PackingResult> {
    if deadline.expired() {
        return Vec::new();
    }
    let enabled = |solver: &str| {
        request.config.solvers.is_empty()
            || request.config.solvers.iter().any(|name| name == solver)
    };
    let mut results = Vec::new();
    if enabled("extreme_points") {
        results.push(pack_order(
            request,
            &order,
            constraints,
            scorers,
            &format!("extreme_points:{name}"),
            deadline,
        ));
    }
    if enabled("maximal_spaces")
        && (request.config.profile == SolverProfile::Quality || !request.config.solvers.is_empty())
        && !deadline.expired()
        && order.iter().all(|item| item.item.group.is_none())
        // The maximal-space walk opens the first container in a static order and never
        // consults a tariff, so under `lowest_landed_cost` it can commit to the one
        // container the caller cannot buy -- and its unpriceable packings were exactly
        // what leaked the sentinel through `alternatives`. Like the lattice (
        // precedent), it stands down for this objective; an explicit pin falls back to
        // the money-ranked greedy below, the way `grid:fallback` does ( review).
        && request.config.objective != "lowest_landed_cost"
    {
        results.push(pack_maximal_order(
            request,
            &order,
            constraints,
            scorers,
            deadline,
        ));
    }
    // An explicit `maximal_spaces` pin under `lowest_landed_cost` still deserves an
    // answer: the money-ranked greedy stands in, so the pin refuses nothing a priceable
    // container could ship. Duplicates of an `extreme_points` run dedup by signature.
    if enabled("maximal_spaces")
        && !request.config.solvers.is_empty()
        && request.config.objective == "lowest_landed_cost"
        && !deadline.expired()
    {
        results.push(pack_order(
            request,
            &order,
            constraints,
            scorers,
            &format!("maximal_spaces:fallback:{name}"),
            deadline,
        ));
    }
    results
}

/// The keys every built-in item ordering starts with.
///
/// Priority is a preference, not a guarantee: it leads so a caller can bias the search,
/// but ties (the default, priority 0 for all items) fall through to the strategy's own
/// key unchanged. Under `maximum_value` the objective's second key is the value left
/// behind (docs/OBJECTIVE.md), so the most valuable item takes first refusal on the
/// space -- behind an explicit priority bias, ahead of the strategy's own key. An
/// undeclared value is zero, so a request that never sets `value` orders exactly as it
/// did before this key existed.
pub(crate) fn ordering_lead(
    item: &ItemInstance,
    objective: &str,
) -> (Reverse<i32>, Reverse<usize>) {
    let value = if objective == "maximum_value" {
        item.item.value.unwrap_or(0)
    } else {
        0
    };
    (Reverse(item.item.priority), Reverse(value))
}

fn sort_volume(mut values: Vec<ItemInstance>, objective: &str) -> Vec<ItemInstance> {
    values.sort_by_key(|item| {
        (
            ordering_lead(item, objective),
            Reverse(item.item.dimensions.volume()),
            item.id(),
        )
    });
    values
}

fn sort_small_edge(mut values: Vec<ItemInstance>, objective: &str) -> Vec<ItemInstance> {
    values.sort_by_key(|item| {
        (
            ordering_lead(item, objective),
            item.item.dimensions.longest_edge(),
            item.item.dimensions.volume(),
            item.id(),
        )
    });
    values
}

fn sort_small_volume(mut values: Vec<ItemInstance>, objective: &str) -> Vec<ItemInstance> {
    values.sort_by_key(|item| {
        (
            ordering_lead(item, objective),
            item.item.dimensions.volume(),
            item.item.dimensions.longest_edge(),
            item.id(),
        )
    });
    values
}

fn sort_base(mut values: Vec<ItemInstance>, objective: &str) -> Vec<ItemInstance> {
    values.sort_by_key(|item| {
        (
            ordering_lead(item, objective),
            Reverse(item.item.dimensions.base_area()),
            Reverse(item.item.dimensions.volume()),
            item.id(),
        )
    });
    values
}

fn sort_edge(mut values: Vec<ItemInstance>, objective: &str) -> Vec<ItemInstance> {
    values.sort_by_key(|item| {
        (
            ordering_lead(item, objective),
            Reverse(item.item.dimensions.longest_edge()),
            Reverse(item.item.dimensions.volume()),
            item.id(),
        )
    });
    values
}

fn sort_weight(mut values: Vec<ItemInstance>, objective: &str) -> Vec<ItemInstance> {
    values.sort_by_key(|item| {
        (
            ordering_lead(item, objective),
            Reverse(item.item.weight.0),
            Reverse(item.item.dimensions.volume()),
            item.id(),
        )
    });
    values
}

fn sort_constrained(mut values: Vec<ItemInstance>, objective: &str) -> Vec<ItemInstance> {
    values.sort_by_key(|item| {
        (
            ordering_lead(item, objective),
            item.item.allowed_rotations.len(),
            Reverse(item.item.must_be_on_floor),
            Reverse(item.item.minimum_support_ratio.to_bits()),
            item.id(),
        )
    });
    values
}

fn deterministic_shuffle(mut values: Vec<ItemInstance>, mut state: u64) -> Vec<ItemInstance> {
    for index in (1..values.len()).rev() {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        values.swap(index, swap_index(state, index));
    }
    values
}

/// Partner index for position `index` in [`deterministic_shuffle`]'s Fisher-Yates pass.
///
/// The modulo must be taken in `u64` and only then narrowed. Reducing the 64-bit
/// xorshift state to `usize` first discards its top 32 bits on a 32-bit target, so
/// wasm32 drew a different permutation from every 64-bit target and packed the same
/// request differently -- one source, two answers. The result is always below
/// `index + 1`, so narrowing it afterwards is lossless on every target.
fn swap_index(state: u64, index: usize) -> usize {
    (state % (index as u64 + 1)) as usize
}

fn signature(result: &PackingResult) -> String {
    result
        .containers
        .iter()
        .map(|container| {
            format!(
                "{}:{}",
                container.container.id,
                container
                    .placements
                    .iter()
                    .map(|placement| format!(
                        "{}@{},{},{}:{}",
                        placement.instance.id(),
                        placement.position.x,
                        placement.position.y,
                        placement.position.z,
                        placement.rotation.as_str()
                    ))
                    .collect::<Vec<_>>()
                    .join("|")
            )
        })
        .collect::<Vec<_>>()
        .join(";")
}

#[cfg(test)]
mod tests {
    use super::swap_index;
    use crate::api::pack_json;

    /// Regression, : the shuffle partner is a property of the seed alone, never of
    /// the pointer width of the target the core happens to be compiled for. The second
    /// assertion is what gives the first its teeth -- it shows this very state answers
    /// differently once the top 32 bits are dropped, so a target-width-dependent
    /// implementation cannot pass both.
    #[test]
    fn the_shuffle_partner_does_not_depend_on_pointer_width() {
        let state = 0x1234_5678_9abc_def0_u64;

        for index in 1..64_usize {
            assert_eq!(swap_index(state, index) as u64, state % (index as u64 + 1));
        }

        let truncated = u64::from(state as u32);
        assert_ne!(state % 7, truncated % 7);
    }

    #[test]
    fn an_explicit_grid_solver_falls_back_without_mixing_nested_item_types() {
        let request = r#"{
            "units":{"length":"mm"},
            "configuration":{
                "solver_profile":"fast",
                "solvers":["grid"],
                "time_limit_ms":300000
            },
            "items":[
                {
                    "id":"a",
                    "dimensions":{"length":"100","width":"100","height":"100"},
                    "allowed_rotations":["LWH"],
                    "nesting_height":"40"
                },
                {
                    "id":"b",
                    "dimensions":{"length":"100","width":"100","height":"100"},
                    "allowed_rotations":["LWH"],
                    "nesting_height":"40"
                }
            ],
            "containers":[{
                "id":"cell",
                "quantity":2,
                "inner_dimensions":{"length":"100","width":"100","height":"160"}
            }]
        }"#;

        let output = pack_json(request).expect("explicit grid fallback request");
        let result: serde_json::Value = serde_json::from_str(&output).unwrap();

        assert_eq!(result["complete"], true);
        assert_eq!(result["algorithm"]["solver"], "grid:fallback");
        assert_eq!(result["summary"]["container_count"], 2);
        assert!(
            result["containers"]
                .as_array()
                .unwrap()
                .iter()
                .all(|container| {
                    container["placements"].as_array().unwrap().len() == 1
                        && container["placements"][0]["top_load"]["ticks"] == 0
                })
        );
    }
}
