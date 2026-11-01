use super::extreme::{
    ContainerState, PointSet, SCORE_SCALE, apply_candidate, billed_dimensional_weight,
    container_order_key, find_candidates, ranks_by_volume, score_solution, volume_share_ppm,
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
        // Its exhaustive claim is about one declared container; a box cut to measure is a
        // heuristic rung choice, so the extreme-point path with the rung chain answers
        // (D13).
        || request
            .containers
            .iter()
            .any(|container| container.made_to_measure.is_some())
    {
        return None;
    }

    let deadline = &deadline.for_start();
    let started = deadline.now_ns();
    let mut best_result: Option<PackingResult> = None;
    let mut searched_all_containers = true;
    let mut metrics = SolverMetrics::default();

    let mut type_ids = BTreeMap::<String, u32>::new();
    let mut equiv_ids = BTreeMap::<(String, i64, i64, i64, i64, Vec<u8>), u32>::new();
    let profiled = items
        .iter()
        .map(|item| {
            let next_type = type_ids.len() as u32;
            let type_id = *type_ids.entry(item.item.id.clone()).or_insert(next_type);
            let next_equiv = equiv_ids.len() as u32;
            let equiv_id = *equiv_ids
                .entry((
                    item.item.id.clone(),
                    item.item.dimensions.length.0,
                    item.item.dimensions.width.0,
                    item.item.dimensions.height.0,
                    item.item.weight.0,
                    item.item
                        .allowed_rotations
                        .iter()
                        .map(|r| *r as u8)
                        .collect(),
                ))
                .or_insert(next_equiv);
            (
                item.clone(),
                exact_item_profile(item, &request.config, type_id, equiv_id),
            )
        })
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
        if deadline.expired() {
            searched_all_containers = false;
            break;
        }
        if let Some(best) = best_result.as_ref()
            && (root_floor > best.score || (root_floor == best.score && position > best_position))
        {
            continue;
        }
        // Asked after the floor skip: a container proven unable to win is no work refused.
        if deadline.effort_stops(request, &metrics) {
            searched_all_containers = false;
            break;
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
        let initial_key = visited.keys.of(&initial);
        if !deadline.expired()
            && visited.seen.insert(initial_key.clone())
            && !deadline.effort_stops(request, &metrics)
        {
            search(
                can_fit,
                &cannot_fit,
                request,
                constraints,
                scorers,
                deadline,
                &initial,
                initial_key,
                &mut best_state,
                &mut best_state_score,
                &complete_lower_bound,
                &mut metrics,
                &mut visited,
            );
        }

        let placed = best_state
            .packed
            .placements
            .iter()
            .map(|placement| placement.instance.id())
            .collect::<BTreeSet<_>>();
        let timed_out = deadline.expired();
        let effort_limited = deadline.effort_stopped();
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
        let effort_limited = deadline.effort_stopped();
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
    /// Declared box volume: the currency of the `maximum_volume` share.
    volume: i128,
    /// Space the item takes once placed, which is what `used_volume` adds up and what the
    /// capacity, unused-volume and height floors must bound.
    occupied_volume: i128,
    minimum_height: i64,
    weight: i64,
    type_id: u32,
    equiv_id: u32,
}

fn exact_item_profile(
    item: &ItemInstance,
    config: &PackingConfig,
    type_id: u32,
    equiv_id: u32,
) -> ExactItemProfile {
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
        occupied_volume: occupied_item_volume(&item.item),
        minimum_height,
        weight: item.item.weight.0,
        type_id,
        equiv_id,
    }
}

/// The volume `model::occupied_volume` will count for this item in any orientation.
///
/// A hull occupies its authored solid, not its bounding box, and a proper rotation keeps that
/// volume, so one orientation answers for all six. Bounding the search with the box volume
/// left the unused-volume floor short of every reachable score, so the early exit on a
/// matched floor never fired and a five-item order with one jar searched its whole
/// permutation tree; it also overstated the height and capacity floors for hulls
/// whose boxes interlock. A compressible item keeps its declared volume here, as before.
pub(super) fn occupied_item_volume(item: &Item) -> i128 {
    if item.shape_type == ShapeType::ConvexHull
        && let Some(vertices) = &item.hull_vertices
        && let Some(shape) = crate::hull::shape_for(
            vertices,
            crate::hull::source_axes(crate::geometry::Rotation::Lwh),
        )
    {
        return shape.volume;
    }
    item.dimensions.volume()
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
        .any(|(_, dimensions)| container.fits_alone(dimensions.expand(config.clearance)))
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
        .map(|(_, profile)| profile.occupied_volume)
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
    let mut weights = remaining
        .iter()
        .map(|(_, profile)| profile.weight)
        .collect::<Vec<_>>();
    weights.sort_unstable();
    if let Some(maximum) = container.max_payload {
        let free_payload = i128::from(maximum.0) - i128::from(state.payload);
        let mut admitted = 0;
        let mut lightest_sum = 0_i128;
        for weight in &weights {
            let weight = i128::from(*weight);
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

    let largest_volume_sum = volumes.iter().rev().take(placeable).sum::<i128>();
    let volume_floor = if ranks_by_volume(config) {
        unpacked_volume_floor_ppm(state, remaining, unfit, largest_volume_sum)
    } else {
        0
    };

    if placeable == 0 && state.packed.placements.is_empty() {
        // No descendant can open this container at all: every one scores exactly
        // like this empty node, so the floor is that score with optimistic zeros.
        return if ranks_by_volume(config) {
            vec![volume_floor, unpacked_floor, 0, 0, 0]
        } else {
            vec![unpacked_floor, 0, 0, 0, 0]
        };
    }

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
        i128::from(gross_weight).max(billed_dimensional_weight(&state.packed, config))
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
        "maximum_volume" => vec![volume_floor, unpacked_floor, 1, cost, unused],
        _ => vec![unpacked_floor, 1, cost, unused, height],
    }
}

/// The least share of the requested volume any descendant can leave unpacked, in parts
/// per million. What cannot fit this container stays behind whatever is placed, and no
/// descendant loads more of the rest than the free space holds or than `largest_placeable`,
/// the volume of the most instances the count bound admits, taken largest first.
///
/// Both caps need declared volumes to add up to occupied space. Nesting, a hull and a
/// compressible item each occupy less than they declare, so with any of them in the
/// scene the floor assumes everything remaining may still be loaded. O(n).
fn unpacked_volume_floor_ppm(
    state: &ContainerState,
    remaining: &[(ItemInstance, ExactItemProfile)],
    unfit: &[(ItemInstance, ExactItemProfile)],
    largest_placeable: i128,
) -> i128 {
    let placed = &state.packed.placements;
    let placed_volume = placed
        .iter()
        .map(|placement| placement.instance.item.dimensions.volume())
        .sum::<i128>();
    let remaining_volume = total_volume(remaining);
    let unfit_volume = total_volume(unfit);
    let additive = placed
        .iter()
        .map(|placement| &placement.instance)
        .chain(remaining.iter().map(|(instance, _)| instance))
        .all(|instance| occupies_its_declared_volume(&instance.item));
    let loadable = if additive {
        let container_volume = state.packed.container.inner_dimensions.volume();
        largest_placeable.min((container_volume - placed_volume).max(0))
    } else {
        remaining_volume
    };
    volume_share_ppm(
        unfit_volume + remaining_volume - loadable,
        placed_volume + remaining_volume + unfit_volume,
    )
}

fn total_volume(items: &[(ItemInstance, ExactItemProfile)]) -> i128 {
    items.iter().map(|(_, profile)| profile.volume).sum()
}

fn occupies_its_declared_volume(item: &Item) -> bool {
    item.nesting_height.is_none()
        && item.shape_type != ShapeType::ConvexHull
        && item.max_compression_pressure_kpa.is_none()
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

    fn extend(
        &self,
        parent: &StateKey,
        type_id: u32,
        candidate: &super::extreme::Candidate,
    ) -> StateKey {
        let entry = (
            type_id,
            candidate.envelope_origin.x,
            candidate.envelope_origin.y,
            candidate.envelope_origin.z,
            candidate.rotation as u8,
        );
        let mut key = Vec::with_capacity(parent.len() + 1);
        if self.canonicalize_order {
            let pos = parent.partition_point(|existing| *existing <= entry);
            key.extend_from_slice(&parent[..pos]);
            key.push(entry);
            key.extend_from_slice(&parent[pos..]);
        } else {
            key.extend_from_slice(parent);
            key.push(entry);
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
    state_key: StateKey,
    best: &mut ContainerState,
    best_score: &mut Vec<i128>,
    complete_lower_bound: &[i128],
    metrics: &mut SolverMetrics,
    visited: &mut VisitedStates,
) {
    metrics.search_nodes_expanded = metrics.search_nodes_expanded.saturating_add(1);
    // This node's own score leads with `remaining.len() + unfit.len()` unpacked, so it can only
    // beat the incumbent when that count does not already lose the first element;
    // skipping the `O(n^2)` scoring otherwise changes no incumbent decision. A
    // volume-first score leads with a share of volume instead, which a count says
    // nothing about, so every node is scored there.
    let by_volume = ranks_by_volume(&request.config);
    let total_unpacked = (remaining.len() + unfit.len()) as i128;
    if by_volume || total_unpacked <= best_score[0] {
        let score = score_state(state, &remaining, unfit, &request.config);
        if score < *best_score {
            *best = state.clone();
            *best_score = score;
        }
    }
    if best_score.as_slice() == complete_lower_bound {
        return;
    }
    // Fewer pieces can still be more volume, so the piece-count cut stands down when the
    // objective fills volume first; the admissible floor below prunes there instead.
    if remaining.is_empty()
        || (!by_volume
            && state.packed.placements.len() + remaining.len() < best.packed.placements.len())
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

    // Keep configurable large exact searches logarithmic in overflow classes.
    let mut seen_equiv_mask = 0_u64;
    let mut seen_equiv_overflow = BTreeSet::<u32>::new();
    for index in 0..remaining.len() {
        let (item, profile) = &remaining[index];
        let equiv_id = profile.equiv_id;
        if equiv_id < 64 {
            let bit = 1_u64 << equiv_id;
            if (seen_equiv_mask & bit) != 0 {
                continue;
            }
            seen_equiv_mask |= bit;
        } else if !seen_equiv_overflow.insert(equiv_id) {
            continue;
        }
        let candidates = find_candidates(
            state,
            item,
            request,
            constraints,
            scorers,
            usize::MAX,
            PointSet::Cartesian,
            deadline,
            metrics,
        );
        for candidate in candidates {
            if deadline.expired() {
                continue;
            }
            let next_key = visited.keys.extend(&state_key, profile.type_id, &candidate);
            if visited.seen.contains(&next_key) {
                continue;
            }
            visited.seen.insert(next_key.clone());
            if deadline.effort_stops(request, metrics) {
                continue;
            }
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
                next_key,
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
    use super::*;
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
        // is spent, and the remaining container must not erase that result. Its floor
        // cannot beat the incumbent, so it is skipped as proven: the budget refused no
        // work and the search finished on exactly its budget, not stopped by it.
        let output = pack_json(&open_height_request(5)).expect("budgeted exact-small request");
        let result: serde_json::Value = serde_json::from_str(&output).unwrap();

        assert_eq!(result["complete"], true);
        assert_eq!(result["containers"][0]["container_type"], "b-flat");
        assert_eq!(result["algorithm"]["metrics"]["search_nodes_expanded"], 5);
        assert_eq!(result["algorithm"]["effort_limit_reached"], false);
        assert_eq!(result["termination"]["code"], "complete");

        // One node short, `b-flat` is cut before its last placement and the stop is reported.
        let output = pack_json(&open_height_request(4)).expect("budgeted exact-small request");
        let result: serde_json::Value = serde_json::from_str(&output).unwrap();
        assert_eq!(result["algorithm"]["metrics"]["search_nodes_expanded"], 4);
        assert_eq!(result["algorithm"]["effort_limit_reached"], true);
        assert_eq!(result["termination"]["code"], "effort_limit");
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

    /// An octagonal-prism jar 80 x 80 x 100 mm, in the item's own frame.
    fn octagonal_jar() -> serde_json::Value {
        let ring = [
            (23, 0),
            (57, 0),
            (80, 23),
            (80, 57),
            (57, 80),
            (23, 80),
            (0, 57),
            (0, 23),
        ];
        let vertices = [0, 100]
            .iter()
            .flat_map(|z| {
                ring.iter().map(move |(x, y)| {
                    serde_json::json!({"x": x.to_string(), "y": y.to_string(), "z": z.to_string()})
                })
            })
            .collect::<Vec<_>>();
        serde_json::json!({
            "id": "jar3", "quantity": 1,
            "dimensions": {"length": "80", "width": "80", "height": "100"},
            "shape_type": "convex_hull", "hull_vertices": vertices
        })
    }

    fn mailer_with_a_jar(configuration: serde_json::Value) -> String {
        let cuboid = |id: &str, length: &str, width: &str, height: &str| {
            serde_json::json!({"id": id, "quantity": 1,
                "dimensions": {"length": length, "width": width, "height": height}})
        };
        serde_json::json!({
            "units": {"length": "mm"},
            "configuration": configuration,
            "items": [
                cuboid("phonecase0", "180", "100", "25"),
                cuboid("socks1", "200", "100", "40"),
                cuboid("tshirt2", "280", "200", "30"),
                octagonal_jar(),
                cuboid("mugbox4", "120", "100", "110"),
            ],
            "containers": [{"id": "frame", "quantity": 1,
                "inner_dimensions": {"length": "610", "width": "480", "height": "300"}}]
        })
        .to_string()
    }

    #[test]
    fn exact_small_reaches_its_floor_on_a_scene_with_a_hull() {
        // The floors added up declared box volumes while the score counts the jar's
        // own solid, so the unused-volume floor sat below every reachable score, the early
        // exit never fired, and this order searched 3.68 million nodes (the whole default
        // second, or 60 s when given it). The first complete branch already meets the floor.
        let output = pack_json(&mailer_with_a_jar(serde_json::json!({
            "solvers": ["exact_small"],
            "solver_profile": "exact_small",
            "time_limit_ms": 300000,
            "effort_budget": {
                "max_candidates_evaluated": 1000000,
                "max_placement_attempts": 1000000,
                "max_search_nodes": 1000
            }
        })))
        .expect("a mailer order with a jar");
        let result: serde_json::Value = serde_json::from_str(&output).unwrap();

        assert_eq!(result["complete"], true);
        assert_eq!(result["algorithm"]["effort_limit_reached"], false);
        assert_eq!(result["algorithm"]["metrics"]["search_nodes_expanded"], 6);
        assert_eq!(
            result["score"],
            serde_json::json!([0, 1, 0, 945535, 333333])
        );
    }

    #[test]
    fn the_fast_profile_completes_a_jar_order_inside_a_counted_budget() {
        // The `fast` portfolio runs exact-small as a start for orders of up to seven items;
        // before that start alone exhausted any budget, so every start must finish.
        let output = pack_json(&mailer_with_a_jar(serde_json::json!({
            "solver_profile": "fast",
            "time_limit_ms": 300000,
            "effort_budget": {
                "max_candidates_evaluated": 100000,
                "max_placement_attempts": 100000,
                "max_search_nodes": 1000
            }
        })))
        .expect("a mailer order with a jar");
        let result: serde_json::Value = serde_json::from_str(&output).unwrap();

        assert_eq!(result["termination"]["code"], "complete");
        assert_eq!(result["termination"]["any_start_truncated"], false);
        assert_eq!(
            result["score"],
            serde_json::json!([0, 1, 0, 945535, 333333])
        );
    }

    #[test]
    fn exact_small_seats_two_wedges_whose_boxes_each_fill_the_crate() {
        // Counted by box volume, each wedge claimed the whole crate, so the capacity floor
        // said only one could ever be placed and the search pruned the packing that holds
        // both. Exact-small alone answered one wedge unpacked.
        let wedge = |id: &str, corners: [[u8; 3]; 6]| {
            let vertices = corners
                .iter()
                .map(|[x, y, z]| {
                    serde_json::json!({"x": (u32::from(*x) * 100).to_string(),
                        "y": (u32::from(*y) * 100).to_string(),
                        "z": (u32::from(*z) * 100).to_string()})
                })
                .collect::<Vec<_>>();
            serde_json::json!({"id": id, "quantity": 1,
                "dimensions": {"length": "100", "width": "100", "height": "100"},
                "shape_type": "convex_hull", "hull_vertices": vertices})
        };
        let request = serde_json::json!({
            "units": {"length": "mm"},
            "configuration": {"solvers": ["exact_small"], "time_limit_ms": 300000},
            "items": [
                wedge("wedge-lower", [[0, 0, 0], [1, 0, 0], [0, 1, 0], [0, 0, 1], [1, 0, 1], [0, 1, 1]]),
                wedge("wedge-upper", [[1, 1, 0], [1, 0, 0], [0, 1, 0], [1, 1, 1], [1, 0, 1], [0, 1, 1]]),
            ],
            "containers": [{"id": "crate",
                "inner_dimensions": {"length": "100", "width": "100", "height": "100"}}]
        });

        let output = pack_json(&request.to_string()).expect("two complementary wedges");
        let result: serde_json::Value = serde_json::from_str(&output).unwrap();

        assert_eq!(result["complete"], true);
        assert_eq!(result["score"], serde_json::json!([0, 1, 0, 0, 1000000]));
    }

    fn profiled_instances(items: serde_json::Value) -> Vec<(ItemInstance, ExactItemProfile)> {
        let request = crate::api::request_from_json(&serde_json::json!({
            "units": {"length": "mm"},
            "items": items,
            "containers": [{"id": "crate",
                "inner_dimensions": {"length": "100", "width": "100", "height": "100"}}]
        }))
        .expect("request");
        let config = request.config.clone();
        request
            .items
            .iter()
            .flat_map(|item| (0..item.quantity).map(move |_| item.clone()))
            .enumerate()
            .map(|(sequence, item)| {
                let instance = ItemInstance { item, sequence };
                let profile = exact_item_profile(&instance, &config, 0, 0);
                (instance, profile)
            })
            .collect()
    }

    #[test]
    fn a_child_key_extended_from_its_parent_equals_the_key_rebuilt_from_the_child() {
        // The search extends each child's visited key from its parent's instead
        // of rebuilding it from every placement; the fold is only sound if the two agree,
        // in insertion order and canonicalized, whatever order the placements arrive in.
        let profiled = profiled_instances(serde_json::json!([
            {"id": "b", "quantity": 2, "dimensions": {"length": "10", "width": "10", "height": "10"}},
            {"id": "a", "quantity": 1, "dimensions": {"length": "10", "width": "20", "height": "10"}},
        ]));
        let request = crate::api::request_from_json(&serde_json::json!({
            "units": {"length": "mm"},
            "items": [{"id": "a", "quantity": 1, "dimensions": {"length": "1", "width": "1", "height": "1"}}],
            "containers": [{"id": "crate",
                "inner_dimensions": {"length": "100", "width": "100", "height": "100"}}]
        }))
        .expect("request");
        let origins = [(40, 0, 0), (0, 0, 0), (0, 30, 0), (0, 0, 10)];
        for canonicalize_order in [false, true] {
            let keys = StateKeys::new(&profiled, canonicalize_order);
            let mut state = ContainerState::new(request.containers[0].clone(), 1);
            let mut key = keys.of(&state);
            for (index, (x, y, z)) in origins.iter().copied().enumerate() {
                let (instance, _) = &profiled[index % profiled.len()];
                let origin = crate::geometry::Point { x, y, z };
                let candidate = super::super::extreme::Candidate {
                    envelope_origin: origin,
                    position: origin,
                    rotation: crate::geometry::Rotation::Lwh,
                    dimensions: instance.item.dimensions,
                    envelope_dimensions: instance.item.dimensions,
                    support_ratio: 1.0,
                    score: 0,
                };
                key = keys.extend(&key, keys.types[&instance.item.id], &candidate);
                apply_candidate(&mut state, instance.clone(), &candidate);
                assert_eq!(
                    key,
                    keys.of(&state),
                    "canonicalize_order={canonicalize_order}, step {index}"
                );
            }
        }
    }

    #[test]
    fn equivalent_items_past_the_sixty_fourth_class_still_branch_once() {
        // Equivalence classes below 64 are folded through a bitmask and the rest
        // through an ordered set. A duplicate in the 65th class must still add no branch: the root
        // considers exactly the orientations it would with that duplicate absent.
        let items = |copies: u32| {
            let mut items = (0..64)
                .map(|index| {
                    serde_json::json!({"id": format!("t{index:02}"), "quantity": 1,
                    "allowed_rotations": ["LWH"],
                    "dimensions": {"length": "1", "width": "1", "height": (index + 1).to_string()}})
                })
                .collect::<Vec<_>>();
            items.push(
                serde_json::json!({"id": "t64", "quantity": copies, "allowed_rotations": ["LWH"],
                "dimensions": {"length": "1", "width": "1", "height": "99"}}),
            );
            items
        };
        let root_work = |copies: u32| {
            let request = serde_json::json!({
                "units": {"length": "mm"},
                "configuration": {"solvers": ["exact_small"], "solver_profile": "exact_small",
                    "exact_item_limit": 80, "max_containers": 1, "time_limit_ms": 300000,
                    "effort_budget": {"max_candidates_evaluated": 1000000,
                        "max_placement_attempts": 1000000, "max_search_nodes": 1}},
                "items": items(copies),
                "containers": [{"id": "crate", "quantity": 1,
                    "inner_dimensions": {"length": "100", "width": "100", "height": "100"}}]
            });
            let output = pack_json(&request.to_string()).expect("a 65-class exact request");
            let result: serde_json::Value = serde_json::from_str(&output).unwrap();
            assert_eq!(result["algorithm"]["metrics"]["search_nodes_expanded"], 1);
            result["algorithm"]["metrics"]["orientations_considered"].clone()
        };
        assert_eq!(root_work(2), root_work(1));
    }

    /// One millisecond per reading, so a test can say on which check the deadline passes.
    struct SteppingClock(std::sync::atomic::AtomicU64);

    impl crate::deadline::Clock for SteppingClock {
        fn now_ns(&self) -> u64 {
            self.0
                .fetch_add(1_000_000, std::sync::atomic::Ordering::SeqCst)
        }
    }

    fn stepping(limit_ms: u64) -> crate::deadline::Deadline {
        crate::deadline::Deadline::with_clock(
            limit_ms,
            std::sync::Arc::new(SteppingClock(std::sync::atomic::AtomicU64::new(0))),
        )
    }

    fn cubes(quantity: usize, containers: serde_json::Value) -> crate::model::PackingRequest {
        crate::api::request_from_json(&serde_json::json!({
            "units": {"length": "mm"},
            "configuration": {"solvers": ["exact_small"], "solver_profile": "exact_small",
                              "time_limit_ms": 300000},
            "items": [{"id": "cube", "quantity": quantity, "allowed_rotations": ["LWH"],
                       "dimensions": {"length": "10", "width": "10", "height": "10"}}],
            "containers": containers
        }))
        .expect("a valid request")
    }

    fn exact(
        request: &crate::model::PackingRequest,
        deadline: &crate::deadline::Deadline,
    ) -> Option<crate::model::PackingResult> {
        super::pack_exact_one(request, &request.instances(), &[], &[], deadline)
    }

    fn crate_of(id: &str, length: &str) -> serde_json::Value {
        serde_json::json!({"id": id, "quantity": 1,
            "inner_dimensions": {"length": length, "width": "10", "height": "10"}})
    }

    #[test]
    fn a_deadline_passed_before_the_first_container_answers_nothing() {
        let request = cubes(2, serde_json::json!([crate_of("box", "20")]));
        assert!(exact(&request, &stepping(1)).is_none());
    }

    #[test]
    fn a_deadline_passed_mid_search_keeps_the_incumbent_and_names_the_limit() {
        // Two containers, neither of which holds all four cubes: the first search runs out
        // of clock partway, so the answer is the partial incumbent, flagged as cut off.
        let request = cubes(
            4,
            serde_json::json!([crate_of("short", "20"), crate_of("long", "30")]),
        );
        let result = exact(&request, &stepping(12)).expect("an incumbent");
        assert!(result.algorithm.time_limit_reached);
        assert_eq!(result.status, crate::model::PackingStatus::TimeLimit);
        assert!(!result.unpacked.is_empty());
        assert!(
            result
                .unpacked
                .iter()
                .all(|item| item.reason == "time_limit")
        );
    }

    #[test]
    fn a_container_that_takes_no_item_leaves_every_item_unpacked() {
        let mut request = cubes(2, serde_json::json!([crate_of("box", "20")]));
        request.containers[0].max_items = Some(0);
        let result = exact(&request, &stepping(60_000)).expect("an answer");
        assert!(result.containers.is_empty());
        assert_eq!(result.unpacked.len(), 2);
    }

    #[test]
    fn a_container_item_limit_caps_the_floor_and_the_answer() {
        let mut request = cubes(3, serde_json::json!([crate_of("box", "30")]));
        request.containers[0].max_items = Some(1);
        let result = exact(&request, &stepping(60_000)).expect("an answer");
        assert_eq!(result.containers[0].placements.len(), 1);
        assert_eq!(result.unpacked.len(), 2);
    }
}
