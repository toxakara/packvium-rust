use super::extreme::{
    ContainerState, apply_candidate, container_order_key, find_candidates_at_points, score_solution,
};
use crate::deadline::Deadline;
use crate::geometry::{Aabb, Dimensions, Point};
use crate::model::*;
use crate::solver::{CandidateScorer, PlacementConstraint};
use crate::units::Length;
use std::collections::BTreeMap;
use std::sync::Arc;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Space(pub(crate) Aabb);

/// Same deterministic hard bound used by the Python and PHP maximal-space solvers.
/// Once dominance filtering is exhausted, the largest residual spaces are the most
/// likely to fit a later item; retaining only those keeps the carve, the `O(new * s)`
/// dominance scan and retained memory bounded on adversarial scenes.
const MAX_MAXIMAL_SPACES: usize = 256;

pub fn pack_maximal_order(
    request: &PackingRequest,
    items: &[ItemInstance],
    constraints: &[Arc<dyn PlacementConstraint>],
    scorers: &[Arc<dyn CandidateScorer>],
    deadline: &Deadline,
) -> PackingResult {
    let started = deadline.now_ns();
    let mut remaining = items.to_vec();
    let mut packed = Vec::new();
    let mut metrics = SolverMetrics::default();
    let mut container_sequence = 0;
    let mut inventory = request
        .containers
        .iter()
        .map(|container| (container.id.clone(), container.quantity))
        .collect::<BTreeMap<_, _>>();

    while !remaining.is_empty()
        && request
            .config
            .max_containers
            .map(|maximum| packed.len() < maximum)
            .unwrap_or(true)
        && !deadline.expired()
    {
        let Some(container) = choose_container(request, &remaining, &inventory) else {
            break;
        };
        container_sequence += 1;
        let mut state = ContainerState::new(container.clone(), container_sequence);
        let mut spaces = vec![Space(Aabb {
            origin: Point::ZERO,
            dimensions: container.inner_dimensions,
        })];
        for obstacle in &container.obstacles {
            for box_ in obstacle.boxes() {
                spaces = subtract_all(spaces, box_, &mut metrics);
            }
        }
        let mut next = Vec::new();
        // Sorted origins for the current `spaces` snapshot. `spaces` is mutated only by
        // `subtract_all` after a committed placement (see the `if let Some(candidate)` arm
        // below); every other statement between here and the next commit only reads it.
        // Caching the sort and the derived points across those reads is therefore a no-op
        // transform: re-sorting an already-sorted `Vec` with a stable comparator reproduces
        // the same order, so the cached order is bit-for-bit what a fresh sort would give.
        let mut sorted_points: Option<Vec<Point>> = None;

        for item in remaining {
            if deadline.expired() || effort_exhausted(request, &metrics) {
                next.push(item);
                continue;
            }
            metrics.search_nodes_expanded = metrics.search_nodes_expanded.saturating_add(1);
            let points = sorted_points
                .get_or_insert_with(|| {
                    spaces.sort_by_key(|space| {
                        (
                            space.0.origin.z,
                            space.0.dimensions.volume(),
                            space.0.origin.y,
                            space.0.origin.x,
                        )
                    });
                    spaces.iter().map(|space| space.0.origin).collect()
                })
                .clone();
            let candidates = find_candidates_at_points(
                &state,
                &item,
                request,
                constraints,
                scorers,
                points,
                usize::MAX,
                deadline,
                &mut metrics,
            );
            let selected = candidates.into_iter().find(|candidate| {
                let box_ = Aabb {
                    origin: candidate.envelope_origin,
                    dimensions: candidate.envelope_dimensions,
                };
                spaces.iter().any(|space| space.0.contains(box_))
            });
            if let Some(candidate) = selected {
                let occupied = Aabb {
                    origin: candidate.envelope_origin,
                    dimensions: candidate.envelope_dimensions,
                };
                apply_candidate(&mut state, item, &candidate);
                spaces = subtract_all(spaces, occupied, &mut metrics);
                sorted_points = None;
            } else {
                next.push(item);
            }
        }

        if state.packed.placements.is_empty() {
            inventory.insert(container.id.clone(), Some(0));
            remaining = next;
            continue;
        }
        if let Some(Some(value)) = inventory.get_mut(&container.id) {
            *value = value.saturating_sub(1);
        }
        packed.push(state.packed);
        remaining = next;
    }

    let timed_out = deadline.expired();
    let unpacked = remaining
        .into_iter()
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
            } else {
                "search_exhausted".into()
            };
            UnpackedItem::new(instance, reason, Vec::new())
        })
        .collect::<Vec<_>>();
    let score = score_solution(&packed, &unpacked, &request.config);
    let effort_limited = effort_exhausted(request, &metrics);
    PackingResult {
        status: if unpacked.is_empty() {
            PackingStatus::Feasible
        } else if timed_out {
            PackingStatus::TimeLimit
        } else {
            PackingStatus::BestFound
        },
        containers: packed,
        unpacked,
        algorithm: AlgorithmReport {
            profile: request.config.profile.as_str().into(),
            solver: "maximal_spaces".into(),
            duration_ms: deadline.elapsed_ms_since(started),
            seed: request.config.seed,
            time_limit_reached: timed_out,
            effort_limit_reached: effort_limited,
            candidates_evaluated: metrics.feasible_candidates,
            placements_attempted: metrics.orientations_considered,
            metrics,
        },
        score,
        warnings: Vec::new(),
        alternatives: Vec::new(),
        feasibility: None,
        termination: None,
        optimality: None,
        objective: request.config.objective.clone(),
        catalog_versions_used: Vec::new(),
    }
}

// Precondition: `spaces` is containment-free -- every call site passes either a
// single whole space or this function's own output. A survivor untouched by the
// carve can therefore neither contain nor be contained by another survivor, and a
// new slab cannot contain one either (the slab's own parent could not), so only
// new slabs need the dominance check below: O(new * s) instead of O(s^2).
pub(crate) fn subtract_all(
    spaces: Vec<Space>,
    occupied: Aabb,
    metrics: &mut SolverMetrics,
) -> Vec<Space> {
    metrics.space_partitions = metrics.space_partitions.saturating_add(spaces.len() as u64);
    let mut pieces: Vec<(Space, bool)> = Vec::with_capacity(spaces.len());
    for space in spaces {
        if space.0.intersects(occupied) {
            pieces.extend(
                subtract(space, occupied)
                    .into_iter()
                    .map(|part| (part, true)),
            );
        } else {
            pieces.push((space, false));
        }
    }
    pieces.retain(|(space, _)| {
        space.0.dimensions.length.0 > 0
            && space.0.dimensions.width.0 > 0
            && space.0.dimensions.height.0 > 0
    });
    pieces.sort_by_key(|(space, _)| {
        (
            space.0.origin.x,
            space.0.origin.y,
            space.0.origin.z,
            space.0.dimensions.volume(),
        )
    });
    pieces.dedup_by(|a, b| a.0 == b.0);
    let extents = pieces
        .iter()
        .map(|(space, _)| {
            [
                space.0.origin.x,
                space.0.origin.y,
                space.0.origin.z,
                space.0.x2(),
                space.0.y2(),
                space.0.z2(),
            ]
        })
        .collect::<Vec<_>>();
    let mut result = Vec::with_capacity(pieces.len());
    for (index, (space, is_new)) in pieces.iter().enumerate() {
        if *is_new {
            let candidate = extents[index];
            let dominated = extents.iter().enumerate().any(|(other_index, other)| {
                other_index != index
                    && other[0] <= candidate[0]
                    && other[1] <= candidate[1]
                    && other[2] <= candidate[2]
                    && candidate[3] <= other[3]
                    && candidate[4] <= other[4]
                    && candidate[5] <= other[5]
            });
            if dominated {
                continue;
            }
        }
        result.push(*space);
    }
    if result.len() > MAX_MAXIMAL_SPACES {
        result.sort_by_key(|space| {
            (
                std::cmp::Reverse(space.0.dimensions.volume()),
                space.0.origin.z,
                space.0.origin.y,
                space.0.origin.x,
            )
        });
        result.truncate(MAX_MAXIMAL_SPACES);
        result.sort_by_key(|space| (space.0.origin.z, space.0.origin.y, space.0.origin.x));
    } else {
        result.sort_by_key(|space| {
            (
                space.0.origin.z,
                space.0.origin.y,
                space.0.origin.x,
                std::cmp::Reverse(space.0.dimensions.volume()),
            )
        });
    }
    result
}

fn subtract(space: Space, occupied: Aabb) -> Vec<Space> {
    let source = space.0;
    if !source.intersects(occupied) {
        return vec![space];
    }
    let mut out = Vec::new();
    push_space(
        &mut out,
        source.origin.x,
        source.origin.y,
        source.origin.z,
        occupied.origin.x.min(source.x2()) - source.origin.x,
        source.dimensions.width.0,
        source.dimensions.height.0,
    );
    push_space(
        &mut out,
        occupied.x2().max(source.origin.x),
        source.origin.y,
        source.origin.z,
        source.x2() - occupied.x2().max(source.origin.x),
        source.dimensions.width.0,
        source.dimensions.height.0,
    );
    push_space(
        &mut out,
        source.origin.x,
        source.origin.y,
        source.origin.z,
        source.dimensions.length.0,
        occupied.origin.y.min(source.y2()) - source.origin.y,
        source.dimensions.height.0,
    );
    push_space(
        &mut out,
        source.origin.x,
        occupied.y2().max(source.origin.y),
        source.origin.z,
        source.dimensions.length.0,
        source.y2() - occupied.y2().max(source.origin.y),
        source.dimensions.height.0,
    );
    push_space(
        &mut out,
        source.origin.x,
        source.origin.y,
        source.origin.z,
        source.dimensions.length.0,
        source.dimensions.width.0,
        occupied.origin.z.min(source.z2()) - source.origin.z,
    );
    push_space(
        &mut out,
        source.origin.x,
        source.origin.y,
        occupied.z2().max(source.origin.z),
        source.dimensions.length.0,
        source.dimensions.width.0,
        source.z2() - occupied.z2().max(source.origin.z),
    );
    out
}

// The six scalars are an axis-aligned region's origin and extent, computed piecewise
// at each call site from different source boxes -- wrapping them in an Aabb here
// would just repack what the caller would otherwise have to construct solely to
// satisfy the lint, with no other consumer of that type.
#[allow(clippy::too_many_arguments)]
fn push_space(
    spaces: &mut Vec<Space>,
    x: i64,
    y: i64,
    z: i64,
    length: i64,
    width: i64,
    height: i64,
) {
    if length > 0 && width > 0 && height > 0 {
        spaces.push(Space(Aabb {
            origin: Point { x, y, z },
            dimensions: Dimensions {
                length: Length(length),
                width: Length(width),
                height: Length(height),
            },
        }));
    }
}

fn choose_container(
    request: &PackingRequest,
    items: &[ItemInstance],
    inventory: &BTreeMap<String, Option<usize>>,
) -> Option<Container> {
    let mut containers = request
        .containers
        .iter()
        .filter(|container| {
            inventory
                .get(&container.id)
                .copied()
                .flatten()
                .map(|value| value > 0)
                .unwrap_or(true)
                && items.iter().any(|item| {
                    item.item
                        .dimensions
                        .unique_rotations(&item.item.allowed_rotations)
                        .iter()
                        .any(|(_, dimensions)| {
                            dimensions
                                .expand(request.config.clearance)
                                .fits_inside(container.inner_dimensions)
                        })
                })
        })
        .cloned()
        .collect::<Vec<_>>();
    containers.sort_by_key(|container| container_order_key(container, &request.config));
    containers.into_iter().next()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn free_space_partitions_are_counted() {
        let mut metrics = SolverMetrics::default();
        let source = Space(Aabb {
            origin: Point::ZERO,
            dimensions: Dimensions {
                length: Length(100),
                width: Length(100),
                height: Length(100),
            },
        });
        let occupied = Aabb {
            origin: Point {
                x: 20,
                y: 20,
                z: 20,
            },
            dimensions: Dimensions {
                length: Length(30),
                width: Length(30),
                height: Length(30),
            },
        };

        let remaining = subtract_all(vec![source], occupied, &mut metrics);

        assert!(!remaining.is_empty());
        assert_eq!(metrics.space_partitions, 1);
    }

    #[test]
    fn adversarial_carving_never_exceeds_the_space_budget() {
        let mut metrics = SolverMetrics::default();
        let mut spaces = vec![Space(Aabb {
            origin: Point::ZERO,
            dimensions: Dimensions {
                length: Length(3000),
                width: Length(3000),
                height: Length(3000),
            },
        })];
        for k in 0..120 {
            let size = 20 + (k * 7) % 60;
            let occupied = Aabb {
                origin: Point {
                    x: (k * 131) % 2900,
                    y: (k * 277) % 2900,
                    z: (k * 419) % 2900,
                },
                dimensions: Dimensions {
                    length: Length(size),
                    width: Length(size),
                    height: Length(size),
                },
            };
            spaces = subtract_all(spaces, occupied, &mut metrics);
            assert!(spaces.len() <= MAX_MAXIMAL_SPACES);
        }
    }
}
