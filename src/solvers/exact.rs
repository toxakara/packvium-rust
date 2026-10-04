use super::extreme::{
    ContainerState, SCORE_SCALE, apply_candidate, container_order_key, dimensional_weight_ticks,
    find_candidates, score_solution,
};
use crate::deadline::Deadline;
use crate::geometry::ShapeType;
use crate::model::*;
use crate::solver::{CandidateScorer, PlacementConstraint};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

/// Exhaustively explores item orders, allowed rotations, and the engine's
/// discrete candidate-point set for each available container type. The search
/// is exact within that discrete model; the public status remains `feasible`
/// rather than claiming a global continuous-geometry proof.
pub fn pack_exact_one(
    request: &PackingRequest,
    items: &[ItemInstance],
    constraints: &[Arc<dyn PlacementConstraint>],
    scorers: &[Arc<dyn CandidateScorer>],
    deadline: &Deadline,
) -> Option<PackingResult> {
    // The depth-first search branches on individual items, so it cannot keep a group
    // together and would happily pack half of one. Stand aside and let the group-aware
    // extreme-point path answer instead, the same way `pack_maximal_order` does.
    // It also fills one container from empty, so it stands aside when fixed items have
    // already opened containers of their own (docs/PLAN-REVISIONS.md).
    if items.len() > request.config.exact_item_limit
        || request.containers.is_empty()
        || items.iter().any(|item| item.item.group.is_some())
        || !request.fixed_containers.is_empty()
    {
        return None;
    }

    let started = deadline.now_ns();
    let mut best_result: Option<PackingResult> = None;
    let mut searched_all_containers = true;
    let mut metrics = SolverMetrics::default();

    let profiled = items
        .iter()
        .map(|item| (item.clone(), exact_item_profile(item, &request.config)))
        .collect::<Vec<_>>();
    let mut containers = request.containers.clone();
    containers.sort_by_key(|container| container_order_key(container, &request.config));
    // Each container's root floor is an admissible bound on anything its search can
    // return, so containers are searched best-floor first and a container whose floor
    // cannot beat the incumbent is skipped as proven, never searched. `position` keeps
    // the ranked order as the tie-break -- among equal scores the earlier container
    // still wins, exactly as the plain in-order scan chose -- while the search no longer
    // spends its whole effort budget proving that a box which cannot take every item
    // is worse than one that already did.
    let mut planned = containers
        .into_iter()
        .enumerate()
        .filter(|(_, container)| container.quantity != Some(0))
        .map(|(position, container)| {
            let initial = ContainerState::new(container, 1);
            let (can_fit, cannot_fit): (Vec<_>, Vec<_>) =
                profiled.iter().cloned().partition(|(item, _)| {
                    item_can_fit_container(item, &initial.packed.container, &request.config)
                });
            let floor =
                optimistic_completion_score(&initial, &can_fit, &cannot_fit, &request.config);
            (floor, position, initial, can_fit, cannot_fit)
        })
        .collect::<Vec<_>>();
    planned.sort_by(|left, right| (&left.0, left.1).cmp(&(&right.0, right.1)));
    let mut best_position = usize::MAX;

    for (root_floor, position, initial, can_fit, cannot_fit) in planned {
        if deadline.expired() || effort_exhausted(request, &metrics) {
            searched_all_containers = false;
            break;
        }
        if let Some(best) = best_result.as_ref()
            && (root_floor > best.score || (root_floor == best.score && position > best_position))
        {
            continue;
        }

        let mut best_state = initial.clone();
        let mut best_state_score = score_state(&best_state, &can_fit, &cannot_fit, &request.config);
        let complete_lower_bound = root_floor;
        let mut visited = VisitedStates {
            keys: StateKeys::new(
                &profiled,
                constraints.is_empty()
                    && scorers.is_empty()
                    && initial.packed.container.max_stack_density.is_none()
                    && can_fit.iter().all(|(instance, _)| {
                        !instance.item.is_stack_sensitive()
                            && instance.item.shape_type != ShapeType::Compressible
                    }),
            ),
            seen: BTreeSet::new(),
        };
        search(
            can_fit,
            &cannot_fit,
            request,
            constraints,
            scorers,
            deadline,
            &initial,
            &mut best_state,
            &mut best_state_score,
            &complete_lower_bound,
            &mut metrics,
            &mut visited,
        );

        let placed = best_state
            .packed
            .placements
            .iter()
            .map(|placement| placement.instance.id())
            .collect::<BTreeSet<_>>();
        let timed_out = deadline.expired();
        let effort_limited = effort_exhausted(request, &metrics);
        let unpacked = items
            .iter()
            .filter(|item| !placed.contains(&item.id()))
            .cloned()
            .map(|instance| {
                let structural = super::extreme::explain_unfit(request, &instance);
                let reason = if matches!(
                    structural.as_str(),
                    "no_compatible_container_dimensions"
                        | "payload_exceeded"
                        | "rotation_restricted"
                        | "no_eligible_container"
                ) {
                    structural
                } else if timed_out {
                    "time_limit".into()
                } else if effort_limited {
                    "effort_limit".into()
                } else {
                    "exact_search_incomplete".into()
                };
                UnpackedItem::new(instance, reason, Vec::new())
            })
            .collect::<Vec<_>>();
        let packed_containers = if best_state.packed.placements.is_empty() {
            Vec::new()
        } else {
            vec![best_state.packed]
        };
        let score = score_solution(&packed_containers, &unpacked, &request.config);
        let result = PackingResult {
            status: if unpacked.is_empty() {
                PackingStatus::Feasible
            } else if timed_out {
                PackingStatus::TimeLimit
            } else {
                PackingStatus::BestFound
            },
            containers: packed_containers,
            unpacked,
            algorithm: AlgorithmReport {
                profile: request.config.profile.as_str().into(),
                // The part before the colon is the solver the caller pinned, which is what
                // `_check_solver_selection` in the shared validator compares against
                // `configuration.solvers`; everything after it is this engine's own
                // variant label. `exact_small_discrete` failed that check, and no fixture
                // had ever pinned a solver in a scene where the pin could matter
                // (found while adding one).
                solver: "exact_small:discrete".into(),
                duration_ms: deadline.elapsed_ms_since(started),
                seed: request.config.seed,
                time_limit_reached: timed_out,
                effort_limit_reached: effort_limited,
                candidates_evaluated: metrics.feasible_candidates,
                placements_attempted: metrics.orientations_considered,
                metrics: metrics.clone(),
            },
            score,
            warnings: vec![
                "exact_small proves exhaustiveness only for the generated discrete candidate points"
                    .into(),
            ],
            alternatives: Vec::new(),
            feasibility: None,
            termination: None,
            optimality: None,
            objective: request.config.objective.clone(),
            catalog_versions_used: Vec::new(),
        };

        let replace = best_result.as_ref().is_none_or(|best| {
            result.score < best.score || (result.score == best.score && position < best_position)
        });
        if replace {
            best_result = Some(result);
            best_position = position;
        }
        if timed_out || effort_limited {
            searched_all_containers = false;
            break;
        }
    }

    if let Some(result) = best_result.as_mut() {
        let timed_out = deadline.expired();
        let effort_limited = effort_exhausted(request, &metrics);
        result.algorithm.duration_ms = deadline.elapsed_ms_since(started);
        result.algorithm.time_limit_reached = timed_out;
        result.algorithm.effort_limit_reached = effort_limited;
        result.algorithm.candidates_evaluated = metrics.feasible_candidates;
        result.algorithm.placements_attempted = metrics.orientations_considered;
        result.algorithm.metrics = metrics;
        if !searched_all_containers {
            if timed_out && !result.complete() {
                result.status = PackingStatus::TimeLimit;
            }
            let limit_reason = if timed_out {
                Some("time_limit")
            } else if effort_limited {
                Some("effort_limit")
            } else {
                None
            };
            if let Some(limit_reason) = limit_reason {
                for unpacked in &mut result.unpacked {
                    if unpacked.reason == "exact_search_incomplete" {
                        unpacked.reason = limit_reason.into();
                        unpacked.proof =
                            ReasonProof::for_reason(&unpacked.reason, &unpacked.details);
                    }
                }
            }
        }
    }
    best_result
}

/// Per-instance constants the admissible bound below reads at every node. Computed
/// once per container search instead of re-deriving rotations inside the DFS.
#[derive(Clone, Copy)]
struct ExactItemProfile {
    volume: i128,
    minimum_height: i64,
    weight: i64,
}

fn exact_item_profile(item: &ItemInstance, config: &PackingConfig) -> ExactItemProfile {
    let minimum_height = item
        .item
        .dimensions
        .unique_rotations(&item.item.allowed_rotations)
        .into_iter()
        .map(|(_, physical)| {
            if config.clearance.0 > 0 {
                physical.expand(config.clearance).height.0
            } else {
                physical.height.0
            }
        })
        .min()
        .unwrap_or(0);
    ExactItemProfile {
        volume: item.item.dimensions.volume(),
        minimum_height,
        weight: item.item.weight.0,
    }
}

fn item_can_fit_container(
    item: &ItemInstance,
    container: &Container,
    config: &PackingConfig,
) -> bool {
    if container.max_items == Some(0) {
        return false;
    }
    if !item.item.eligible_container_tags.is_empty()
        && item
            .item
            .eligible_container_tags
            .is_disjoint(&container.tags)
    {
        return false;
    }
    if container
        .max_payload
        .is_some_and(|max| item.item.weight.0 > max.0)
    {
        return false;
    }
    item.item
        .dimensions
        .unique_rotations(&item.item.allowed_rotations)
        .iter()
        .any(|(_, dimensions)| {
            dimensions
                .expand(config.clearance)
                .fits_inside(container.inner_dimensions)
        })
}

/// Reuses the canonical finished-result objective at every visited exact-search node.
/// This adds `O(n^2)` scoring work per node because nesting-aware used volume checks
/// placement pairs, and no asymptotic live-space growth to the
/// already exponential/factorial DFS; `exact_item_limit` keeps `n` deliberately small.
fn score_state(
    state: &ContainerState,
    remaining: &[(ItemInstance, ExactItemProfile)],
    unfit: &[(ItemInstance, ExactItemProfile)],
    config: &PackingConfig,
) -> Vec<i128> {
    let unpacked = remaining
        .iter()
        .chain(unfit.iter())
        .map(|(instance, _)| {
            UnpackedItem::new(
                instance.clone(),
                "exact_search_candidate".into(),
                Vec::new(),
            )
        })
        .collect::<Vec<_>>();
    let containers = if state.packed.placements.is_empty() {
        &[][..]
    } else {
        std::slice::from_ref(&state.packed)
    };
    score_solution(containers, &unpacked, config)
}

/// An admissible objective floor for every descendant of `state` in this container.
///
/// Each term is a provable lexicographic lower bound on the score of any state the
/// DFS can still reach from here, so a subtree whose floor cannot strictly beat the
/// incumbent cannot change the result. The load-bearing term is the first one:
/// physical volumes are additive (nesting excepted, guarded below), so no descendant
/// can place more remaining instances than the greedy smallest-volume prefix that
/// still fits the container — descendants that leave more unpacked lose on the first
/// element, and descendants that exactly meet it can occupy at most the same count
/// of largest volumes (bounding unused), must lift the stack top to at least the
/// smallest-prefix volume over the footprint (bounding height), and pay at least the
/// smallest weights (bounding billable). This is the equal-count suffix-volume
/// pruning Python and PHP already apply, generalized to the full canonical objective
/// vector; at the root it doubles as the complete-packing floor the early-exit
/// equality compares against.
fn optimistic_completion_score(
    state: &ContainerState,
    remaining: &[(ItemInstance, ExactItemProfile)],
    unfit: &[(ItemInstance, ExactItemProfile)],
    config: &PackingConfig,
) -> Vec<i128> {
    if state.packed.placements.is_empty() && remaining.is_empty() && unfit.is_empty() {
        return score_solution(&[], &[], config);
    }

    let container = &state.packed.container;
    let volume = container.inner_dimensions.volume();
    let used_now = state.packed.used_volume();
    // Nesting lets two physical boxes share volume, which breaks the additive
    // capacity argument; fall back to "everything might still be placed" there.
    let nesting_involved = state
        .packed
        .placements
        .iter()
        .any(|placement| placement.instance.item.nesting_height.is_some())
        || remaining
            .iter()
            .any(|(instance, _)| instance.item.nesting_height.is_some());

    let mut volumes = remaining
        .iter()
        .map(|(_, profile)| profile.volume)
        .collect::<Vec<_>>();
    volumes.sort_unstable();
    let mut placeable = remaining.len();
    if !nesting_involved && volume > 0 {
        placeable = 0;
        let mut admitted_volume = 0_i128;
        for item_volume in &volumes {
            if used_now + admitted_volume + item_volume > volume {
                break;
            }
            admitted_volume += item_volume;
            placeable += 1;
        }
    }
    // Weight is additive under every rule, nesting included, so the same greedy
    // argument bounds the count from the payload side: no descendant can seat more
    // remaining instances than the lightest prefix the free payload still admits. The
    // item cap bounds it outright. Without these a weight-capped container searched
    // every ordering of items it could never take -- a million placement attempts to
    // prove three light boxes fit where the volume bound said all six might.
    if let Some(maximum) = container.max_payload {
        let free_payload = i128::from(maximum.0) - i128::from(state.payload);
        let mut weights = remaining
            .iter()
            .map(|(_, profile)| i128::from(profile.weight))
            .collect::<Vec<_>>();
        weights.sort_unstable();
        let mut admitted = 0;
        let mut lightest_sum = 0_i128;
        for weight in &weights {
            if lightest_sum + weight > free_payload {
                break;
            }
            lightest_sum += weight;
            admitted += 1;
        }
        placeable = placeable.min(admitted);
    }
    if let Some(maximum) = container.max_items {
        placeable = placeable.min(maximum.saturating_sub(state.packed.placements.len()));
    }
    let smallest_volume_sum = volumes.iter().take(placeable).sum::<i128>();
    let unpacked_floor = (remaining.len() - placeable) as i128 + unfit.len() as i128;

    if placeable == 0 && state.packed.placements.is_empty() {
        // No descendant can open this container at all: every one scores exactly
        // like this empty node, so the floor is that score with optimistic zeros.
        return vec![unpacked_floor, 0, 0, 0, 0];
    }

    let largest_volume_sum = volumes.iter().rev().take(placeable).sum::<i128>();
    let used_ceiling = (used_now + largest_volume_sum).min(volume.max(used_now));
    let unused = if volume > 0 {
        (volume - used_ceiling) * SCORE_SCALE / volume
    } else {
        0
    };
    let footprint = i128::from(container.inner_dimensions.length.0)
        * i128::from(container.inner_dimensions.width.0);
    let mut minimum_height = state.packed.max_z_ticks();
    if !nesting_involved && footprint > 0 {
        minimum_height =
            minimum_height.max((used_now + smallest_volume_sum + footprint - 1) / footprint);
    }
    if placeable == remaining.len() {
        // Only a true completion must still seat every remaining item's own
        // shortest envelope; a descendant allowed to skip items need not.
        minimum_height = minimum_height.max(
            remaining
                .iter()
                .map(|(_, profile)| i128::from(profile.minimum_height))
                .max()
                .unwrap_or(0),
        );
    }
    let inner_height = container.inner_dimensions.height.0 as i128;
    let height = if inner_height > 0 {
        minimum_height * SCORE_SCALE / inner_height
    } else {
        0
    };
    let cost = container.cost_minor as i128;
    let mut weights = remaining
        .iter()
        .map(|(_, profile)| profile.weight)
        .collect::<Vec<_>>();
    weights.sort_unstable();
    let gross_weight = weights
        .iter()
        .take(placeable)
        .fold(state.packed.gross_weight().0, |gross, weight| {
            gross.saturating_add(*weight)
        });
    let billable = if matches!(
        config.objective.as_str(),
        "shipping_cost" | "lowest_landed_cost"
    ) {
        i128::from(gross_weight).max(dimensional_weight_ticks(container, config))
    } else {
        0
    };
    // A tariff is deliberately allowed to dip at a promotional bracket. Charging the
    // lightest possible completion is therefore *not* a lower bound on descendants:
    // a heavier subset can cost less. All published charges are non-negative, so zero
    // is the tightest generally valid O(1) money floor. This weakens pruning only for
    // landed cost and keeps the branch-and-bound admissible (second review).
    let landed = 0;

    match config.objective.as_str() {
        "lowest_cost" => vec![unpacked_floor, cost, 1, unused, height],
        "shipping_cost" => vec![unpacked_floor, billable, 1, unused, height],
        "lowest_landed_cost" => vec![unpacked_floor, landed, 1, unused, height],
        "open_dimension_height" => vec![unpacked_floor, minimum_height, 1, cost, unused],
        "maximum_value" => vec![unpacked_floor, 0, 1, cost, unused],
        _ => vec![unpacked_floor, 1, cost, unused, height],
    }
}

/// A search state up to the labelling of interchangeable instances. Item types are
/// numbered once per search so a key is five machine words per placement, not a string,
/// and the set stays small: at most one entry per node the effort budget admits.
///
/// Sorting is sound only when future decisions are independent of insertion order. Load
/// settling hands an indivisible integer remainder to the last supporter, while extension
/// constraints/scorers may deliberately inspect placement order. Those scenes retain the
/// insertion sequence in the key. Ordinary box scenes canonicalise it and fold the full
/// item-order permutation tree; identical instances remain interchangeable in both modes.
type StateKey = Vec<(u32, i64, i64, i64, u8)>;

struct VisitedStates {
    keys: StateKeys,
    seen: BTreeSet<StateKey>,
}

struct StateKeys {
    types: BTreeMap<String, u32>,
    canonicalize_order: bool,
}

impl StateKeys {
    fn new(items: &[(ItemInstance, ExactItemProfile)], canonicalize_order: bool) -> Self {
        let mut types = BTreeMap::new();
        for (instance, _) in items {
            let next = types.len() as u32;
            types.entry(instance.item.id.clone()).or_insert(next);
        }
        Self {
            types,
            canonicalize_order,
        }
    }

    fn of(&self, state: &ContainerState) -> StateKey {
        let mut key = state
            .packed
            .placements
            .iter()
            .map(|placement| {
                (
                    self.types[&placement.instance.item.id],
                    placement.envelope_origin.x,
                    placement.envelope_origin.y,
                    placement.envelope_origin.z,
                    placement.rotation as u8,
                )
            })
            .collect::<Vec<_>>();
        if self.canonicalize_order {
            key.sort_unstable();
        }
        key
    }
}

// Each parameter is independently-varying recursive DFS state (the remaining work,
// the read-only request/constraints/scorers/deadline threaded unchanged through every
// call, and the mutable state/best/metrics accumulators) -- bundling them into a
// struct only to satisfy the lint would add an abstraction no other caller needs
// (same reasoning as `find_candidates`).
#[allow(clippy::too_many_arguments)]
fn search(
    remaining: Vec<(ItemInstance, ExactItemProfile)>,
    unfit: &[(ItemInstance, ExactItemProfile)],
    request: &PackingRequest,
    constraints: &[Arc<dyn PlacementConstraint>],
    scorers: &[Arc<dyn CandidateScorer>],
    deadline: &Deadline,
    state: &ContainerState,
    best: &mut ContainerState,
    best_score: &mut Vec<i128>,
    complete_lower_bound: &[i128],
    metrics: &mut SolverMetrics,
    visited: &mut VisitedStates,
) {
    if deadline.expired() || effort_exhausted(request, metrics) {
        return;
    }
    // In an order-insensitive scene, two item orders that seat the same boxes at the same
    // places reach the same state and the first visit already explored its subtree. In a
    // load-sensitive or extension-bearing scene StateKeys preserves insertion order, so
    // the same check only folds states whose order-dependent future is also the same.
    if !visited.seen.insert(visited.keys.of(state)) {
        return;
    }
    metrics.search_nodes_expanded = metrics.search_nodes_expanded.saturating_add(1);
    // This node's own score leads with `remaining.len() + unfit.len()` unpacked, so it can only
    // beat the incumbent when that count does not already lose the first element;
    // skipping the `O(n^2)` scoring otherwise changes no incumbent decision.
    let total_unpacked = (remaining.len() + unfit.len()) as i128;
    if total_unpacked <= best_score[0] {
        let score = score_state(state, &remaining, unfit, &request.config);
        if score < *best_score {
            *best = state.clone();
            *best_score = score;
        }
    }
    if best_score.as_slice() == complete_lower_bound {
        return;
    }
    if remaining.is_empty()
        || state.packed.placements.len() + remaining.len() < best.packed.placements.len()
    {
        return;
    }
    // Admissible branch-and-bound cut: the floor lexicographically lower-bounds every
    // state reachable from here, and replacing the incumbent requires a strictly
    // smaller score, so a subtree whose floor cannot strictly beat `best_score`
    // cannot change the result. Without this the removal of the first-complete
    // shortcut left the DFS exploring the entire permutation tree whenever the
    // complete floor is geometrically unreachable.
    if optimistic_completion_score(state, &remaining, unfit, &request.config) >= *best_score {
        return;
    }

    let mut equivalent = BTreeSet::new();
    for index in 0..remaining.len() {
        let item = &remaining[index].0;
        let signature = format!(
            "{}:{}:{}:{}:{}:{:?}",
            item.item.id,
            item.item.dimensions.length.0,
            item.item.dimensions.width.0,
            item.item.dimensions.height.0,
            item.item.weight.0,
            item.item.allowed_rotations,
        );
        if !equivalent.insert(signature) {
            continue;
        }
        let candidates = find_candidates(
            state,
            item,
            request,
            constraints,
            scorers,
            usize::MAX,
            deadline,
            metrics,
        );
        for candidate in candidates {
            let mut next_state = state.clone();
            apply_candidate(&mut next_state, item.clone(), &candidate);
            let mut next_remaining = remaining.clone();
            next_remaining.remove(index);
            search(
                next_remaining,
                unfit,
                request,
                constraints,
                scorers,
                deadline,
                &next_state,
                best,
                best_score,
                complete_lower_bound,
                metrics,
                visited,
            );
            if best_score.as_slice() == complete_lower_bound {
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::api::pack_json;

    fn open_height_request(max_search_nodes: u64) -> String {
        format!(
            r#"{{
                "units":{{"length":"mm"}},
                "configuration":{{
                    "solvers":["exact_small"],
                    "solver_profile":"exact_small",
                    "objective":"open_dimension_height",
                    "max_containers":1,
                    "time_limit_ms":300000,
                    "effort_budget":{{
                        "max_candidates_evaluated":1000000,
                        "max_placement_attempts":1000000,
                        "max_search_nodes":{max_search_nodes}
                    }}
                }},
                "items":[{{
                    "id":"cube",
                    "quantity":4,
                    "allowed_rotations":["LWH"],
                    "dimensions":{{"length":"10","width":"10","height":"10"}}
                }}],
                "containers":[
                    {{
                        "id":"a-tall",
                        "cost_minor":0,
                        "quantity":1,
                        "inner_dimensions":{{"length":"10","width":"10","height":"40"}}
                    }},
                    {{
                        "id":"b-flat",
                        "cost_minor":100,
                        "quantity":1,
                        "inner_dimensions":{{"length":"20","width":"20","height":"10"}}
                    }}
                ]
            }}"#
        )
    }

    #[test]
    fn exact_small_evaluates_later_containers_for_the_requested_objective() {
        let output = pack_json(&open_height_request(1_000_000)).expect("exact-small request");
        let result: serde_json::Value = serde_json::from_str(&output).unwrap();

        assert_eq!(result["containers"][0]["container_type"], "b-flat");
        assert_eq!(result["score"], serde_json::json!([0, 160000, 1, 100, 0]));
        assert_eq!(result["algorithm"]["effort_limit_reached"], false);
    }

    #[test]
    fn exact_small_keeps_a_complete_incumbent_when_effort_expires() {
        // Root plus four placements exactly consumes this counted prefix. Containers are
        // searched best root floor first, so `b-flat` -- the one the objective prefers --
        // has already produced a complete incumbent when the global exact search budget
        // binds, and the remaining container must not erase that result.
        let output = pack_json(&open_height_request(5)).expect("budgeted exact-small request");
        let result: serde_json::Value = serde_json::from_str(&output).unwrap();

        assert_eq!(result["complete"], true);
        assert_eq!(result["containers"][0]["container_type"], "b-flat");
        assert_eq!(result["algorithm"]["metrics"]["search_nodes_expanded"], 5);
        assert_eq!(result["algorithm"]["effort_limit_reached"], true);
    }

    #[test]
    fn exact_small_optimizes_the_objective_inside_each_container_search() {
        // In a-twenty the first complete DFS branch puts the 4mm and 2mm items on
        // the floor and the 6mm item above the shorter one: 8mm high. A later item
        // order reaches 6mm. b-thirty reaches 6mm immediately but costs more, so it
        // only wins if exact-small incorrectly treats the first complete branch in
        // a-twenty as an objective proof.
        let request = r#"{
            "units":{"length":"mm"},
            "configuration":{
                "solvers":["exact_small"],
                "solver_profile":"exact_small",
                "objective":"open_dimension_height",
                "max_containers":1,
                "time_limit_ms":300000,
                "effort_budget":{
                    "max_candidates_evaluated":1000000,
                    "max_placement_attempts":1000000,
                    "max_search_nodes":1000000
                }
            },
            "items":[
                {"id":"height-four","dimensions":{"length":"10","width":"10","height":"4"},"allowed_rotations":["LWH"]},
                {"id":"height-two","dimensions":{"length":"10","width":"10","height":"2"},"allowed_rotations":["LWH"]},
                {"id":"height-six","dimensions":{"length":"10","width":"10","height":"6"},"allowed_rotations":["LWH"]}
            ],
            "containers":[
                {"id":"a-twenty","cost_minor":0,"quantity":1,"inner_dimensions":{"length":"20","width":"10","height":"10"}},
                {"id":"b-thirty","cost_minor":100,"quantity":1,"inner_dimensions":{"length":"30","width":"10","height":"10"}}
            ]
        }"#;

        let output = pack_json(request).expect("within-container objective request");
        let result: serde_json::Value = serde_json::from_str(&output).unwrap();

        assert_eq!(result["containers"][0]["container_type"], "a-twenty");
        assert_eq!(result["score"], serde_json::json!([0, 96000, 1, 0, 400000]));
    }

    #[test]
    fn exact_small_labels_an_effort_truncated_partial_incumbent() {
        let output = pack_json(&open_height_request(2)).expect("effort-limited exact request");
        let result: serde_json::Value = serde_json::from_str(&output).unwrap();
        let unpacked = result["unpacked_items"].as_array().unwrap();

        assert_eq!(result["complete"], false);
        assert_eq!(result["algorithm"]["effort_limit_reached"], true);
        assert!(!unpacked.is_empty());
        assert!(unpacked.iter().all(|item| item["reason"] == "effort_limit"));
        assert!(
            unpacked
                .iter()
                .all(|item| item["proof"]["level"] == "unknown_due_to_limit")
        );
    }

    #[test]
    fn exact_small_stops_early_when_an_item_is_proven_not_to_fit() {
        // 's scene: without the pallet jack the search finishes in about a hundred
        // candidates. Before the jack alone spent the whole budget (106 861
        // candidates at 1M, `effort_limit`); this budget is small enough to show that fast.
        let request = r#"{
            "units":{"length":"mm"},
            "configuration":{
                "time_limit_ms":300000,
                "effort_budget":{
                    "max_candidates_evaluated":20000,
                    "max_placement_attempts":20000,
                    "max_search_nodes":20000
                }
            },
            "items":[
                {"id":"printer","quantity":1,"weight":"9 kg","keep_upright":true,
                 "dimensions":{"length":"420","width":"340","height":"260"}},
                {"id":"toner","quantity":4,"weight":"900 g",
                 "dimensions":{"length":"180","width":"120","height":"100"}},
                {"id":"pallet-jack","quantity":1,"weight":"80 kg",
                 "dimensions":{"length":"1200","width":"550","height":"1200"}}
            ],
            "containers":[
                {"id":"crate","quantity":1,"max_payload":"30 kg",
                 "inner_dimensions":{"length":"600","width":"400","height":"400"}}
            ]
        }"#;

        let output = pack_json(request).expect("a request with an unfittable item");
        let result: serde_json::Value = serde_json::from_str(&output).unwrap();
        assert_eq!(result["algorithm"]["effort_limit_reached"], false);
        assert_eq!(result["algorithm"]["time_limit_reached"], false);
        let unpacked = result["unpacked_items"].as_array().unwrap();
        assert_eq!(unpacked.len(), 1);
        assert_eq!(unpacked[0]["item_id"], "pallet-jack#1");
        assert_eq!(unpacked[0]["proof"]["level"], "proven");
    }
}
