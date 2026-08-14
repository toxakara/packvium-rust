use crate::contact_graph::ContactGraph;
use crate::deadline::Deadline;
use crate::geometry::{Aabb, Dimensions, Point, Rotation};
use crate::model::*;
use crate::solver::{CandidateScorer, PlacementConstraint, SolverContext};
use crate::spatial_index::SpatialIndex;
use crate::units::{Length, Weight};
use std::cmp::{Ordering, Reverse};
use std::collections::{BTreeMap, BTreeSet, BinaryHeap};
use std::sync::Arc;

#[derive(Clone, Debug)]
pub struct Candidate {
    pub envelope_origin: Point,
    pub position: Point,
    pub rotation: Rotation,
    pub dimensions: Dimensions,
    pub envelope_dimensions: Dimensions,
    pub support_ratio: f64,
    pub score: i128,
}

type CandidateOrderKey = (i128, i64, i64, i64, &'static str);

fn candidate_order_key(candidate: &Candidate) -> CandidateOrderKey {
    (
        candidate.score,
        candidate.envelope_origin.z,
        candidate.envelope_origin.y,
        candidate.envelope_origin.x,
        candidate.rotation.as_str(),
    )
}

struct RankedCandidate {
    key: CandidateOrderKey,
    ordinal: usize,
    candidate: Candidate,
}

impl PartialEq for RankedCandidate {
    fn eq(&self, other: &Self) -> bool {
        (self.key, self.ordinal) == (other.key, other.ordinal)
    }
}

impl Eq for RankedCandidate {}

impl PartialOrd for RankedCandidate {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for RankedCandidate {
    fn cmp(&self, other: &Self) -> Ordering {
        (self.key, self.ordinal).cmp(&(other.key, other.ordinal))
    }
}

enum CandidateAccumulator {
    All(Vec<Candidate>),
    Bounded {
        limit: usize,
        next_ordinal: usize,
        worst_first: BinaryHeap<RankedCandidate>,
    },
}

impl CandidateAccumulator {
    fn new(limit: usize) -> Self {
        if limit == usize::MAX {
            Self::All(Vec::new())
        } else {
            Self::Bounded {
                limit: limit.max(1),
                next_ordinal: 0,
                worst_first: BinaryHeap::new(),
            }
        }
    }

    fn push(&mut self, candidate: Candidate) {
        match self {
            Self::All(candidates) => candidates.push(candidate),
            Self::Bounded {
                limit,
                next_ordinal,
                worst_first,
            } => {
                let ranked = RankedCandidate {
                    key: candidate_order_key(&candidate),
                    ordinal: *next_ordinal,
                    candidate,
                };
                *next_ordinal = next_ordinal
                    .checked_add(1)
                    .expect("candidate ordinal cannot exceed addressable memory");
                if worst_first.len() < *limit {
                    worst_first.push(ranked);
                } else if worst_first
                    .peek()
                    .is_some_and(|worst| ranked.cmp(worst) == Ordering::Less)
                {
                    worst_first.pop();
                    worst_first.push(ranked);
                }
            }
        }
    }

    fn finish(self) -> Vec<Candidate> {
        match self {
            Self::All(mut candidates) => {
                candidates.sort_by_key(candidate_order_key);
                candidates
            }
            Self::Bounded { worst_first, .. } => {
                let mut ranked = worst_first.into_vec();
                ranked.sort();
                ranked
                    .into_iter()
                    .map(|candidate| candidate.candidate)
                    .collect()
            }
        }
    }
}

#[derive(Clone, Debug)]
pub struct ContainerState {
    pub packed: PackedContainer,
    pub payload: i64,
    used_volume: i128,
    stack_sensitive: bool,
    route_sensitive: bool,
    /// Candidate origins ordered by (z, y, x), matching the reference engines.
    points: BTreeSet<(i64, i64, i64)>,
    spatial_index: SpatialIndex,
}

struct ContainerTrial {
    state: ContainerState,
    remaining: Vec<ItemInstance>,
    /// Ranking terms in the same order `score_solution` ranks the finished result by,
    /// then the container id so the choice is total and deterministic.
    ordering_key: (Vec<i128>, String),
}

impl ContainerState {
    pub fn new(container: Container, sequence: usize) -> Self {
        let spatial_index = SpatialIndex::new(container.inner_dimensions);
        let obstacle_boxes = container
            .obstacles
            .iter()
            .flat_map(Obstacle::boxes)
            .collect::<Vec<_>>();
        let mut state = Self {
            packed: PackedContainer {
                container,
                sequence,
                placements: Vec::new(),
                lattice_summary: None,
                lattice_items: Vec::new(),
            },
            payload: 0,
            used_volume: 0,
            stack_sensitive: false,
            route_sensitive: false,
            points: BTreeSet::new(),
            spatial_index,
        };
        absorb_point(&mut state, Point::ZERO);
        for box_ in obstacle_boxes {
            for point in exposed_points(&state, box_) {
                absorb_point(&mut state, point);
            }
        }
        state
    }
}

pub fn candidate_points(state: &ContainerState, limit: usize) -> Vec<Point> {
    let limit = limit.max(1);
    // Under a tight point budget the Cartesian product spends most of the allowance
    // on arbitrary coordinate combinations. The sparse corner/projection set matches
    // Python/PHP and gives every retained point a geometric reason to exist. With a
    // generous budget, retain the Cartesian set's useful mixed-face origins: removing
    // those globally regresses dense heterogeneous scenes even though it is faster.
    const SPARSE_POINT_LIMIT: usize = 64;
    if limit <= SPARSE_POINT_LIMIT {
        return state
            .points
            .iter()
            .take(limit)
            .map(|(z, y, x)| Point {
                x: *x,
                y: *y,
                z: *z,
            })
            .collect();
    }
    legacy_candidate_points(state, limit)
}

fn legacy_candidate_points(state: &ContainerState, limit: usize) -> Vec<Point> {
    if limit == 0 {
        return Vec::new();
    }
    let mut xs = BTreeSet::from([0]);
    let mut ys = BTreeSet::from([0]);
    let mut zs = BTreeSet::from([0]);
    for box_ in solid_boxes(state) {
        xs.insert(box_.x2());
        ys.insert(box_.y2());
        zs.insert(box_.z2());
    }
    let boundary = state.packed.container.inner_dimensions;
    let mut points = Vec::new();
    'coordinates: for z in zs {
        for y in &ys {
            for x in &xs {
                if points.len() >= limit {
                    break 'coordinates;
                }
                if *x >= boundary.length.0 || *y >= boundary.width.0 || z >= boundary.height.0 {
                    continue;
                }
                let point = Point { x: *x, y: *y, z };
                if !point_inside_any_solid(state, point) {
                    points.push(point);
                }
            }
        }
    }
    points
}

// Each parameter is a distinct, independently-varying piece of search state (not
// several related values that would naturally bundle into one struct); grouping them
// only to satisfy the lint would add an abstraction with no caller that benefits from
// it.
#[allow(clippy::too_many_arguments)]
pub fn find_candidates(
    state: &ContainerState,
    item: &ItemInstance,
    request: &PackingRequest,
    constraints: &[Arc<dyn PlacementConstraint>],
    scorers: &[Arc<dyn CandidateScorer>],
    limit: usize,
    deadline: &Deadline,
    metrics: &mut SolverMetrics,
) -> Vec<Candidate> {
    let mut points = candidate_points(state, request.config.max_candidate_points);
    if item.item.nesting_height.is_some() {
        let nested = nesting_points(state, item);
        let merged = points
            .iter()
            .chain(nested.iter())
            .map(|point| (point.z, point.y, point.x))
            .collect::<BTreeSet<_>>();
        points = merged
            .iter()
            .take(request.config.max_candidate_points.max(1))
            .map(|(z, y, x)| Point {
                x: *x,
                y: *y,
                z: *z,
            })
            .collect();
    }
    if state.packed.container.axles.is_some() {
        points.extend(axle_balanced_points(state, item));
    }
    find_candidates_at_points(
        state,
        item,
        request,
        constraints,
        scorers,
        points,
        limit,
        deadline,
        metrics,
    )
}

fn nesting_points(state: &ContainerState, item: &ItemInstance) -> Vec<Point> {
    let Some(depth) = item.item.nesting_height else {
        return Vec::new();
    };
    state
        .packed
        .placements
        .iter()
        .filter(|placement| placement.instance.item.id == item.item.id)
        .map(|placement| {
            (
                placement.envelope_box().z2().saturating_sub(depth.0),
                placement.envelope_origin.y,
                placement.envelope_origin.x,
            )
        })
        .filter(|(z, _, _)| *z >= 0)
        .collect::<BTreeSet<(i64, i64, i64)>>()
        .into_iter()
        .map(|(z, y, x)| Point { x, y, z })
        .collect()
}

/// Extra floor-level candidates that seat this item on an axle's own limit.
///
/// Every other candidate point is derived from the container's own origin or a
/// placed box's far face, which is complete for plain volume packing but not once
/// axle limits are in play: the only feasible spot for an item can be floating in
/// open floor space, away from every wall and every other box, purely to keep that
/// item's own moment off one axle's limit (see `axle_balanced_origins`). Floor
/// level only (z=0), since that is the one place support needs no lateral contact.
fn axle_balanced_points(state: &ContainerState, item: &ItemInstance) -> Vec<Point> {
    let limit_x = state.packed.container.inner_dimensions.length.0;
    let floor_ys: BTreeSet<i64> = {
        let ys: BTreeSet<i64> = state
            .points
            .iter()
            .filter_map(|(z, y, _)| (*z == 0).then_some(*y))
            .collect();
        if ys.is_empty() {
            BTreeSet::from([0])
        } else {
            ys
        }
    };
    let mut points = Vec::new();
    for (_, physical) in item
        .item
        .dimensions
        .unique_rotations(&item.item.allowed_rotations)
    {
        let dx = physical.length.0;
        for x in axle_balanced_origins(
            &state.packed.container,
            &state.packed.placements,
            item.item.weight,
            dx,
        ) {
            if x < 0 || x > limit_x - dx {
                continue;
            }
            for &y in &floor_ys {
                points.push(Point { x, y, z: 0 });
            }
        }
    }
    points
}

// Same shape and same reasoning as `find_candidates` above -- `points` is simply
// supplied directly here instead of being derived from `state`/`request` internally,
// not a reason to bundle the rest into a struct no caller otherwise wants.
#[allow(clippy::too_many_arguments)]
pub fn find_candidates_at_points(
    state: &ContainerState,
    item: &ItemInstance,
    request: &PackingRequest,
    constraints: &[Arc<dyn PlacementConstraint>],
    scorers: &[Arc<dyn CandidateScorer>],
    points: Vec<Point>,
    limit: usize,
    deadline: &Deadline,
    metrics: &mut SolverMetrics,
) -> Vec<Candidate> {
    if !item.item.eligible_container_tags.is_empty()
        && item
            .item
            .eligible_container_tags
            .is_disjoint(&state.packed.container.tags)
    {
        return Vec::new();
    }
    if state
        .packed
        .container
        .tag_limits
        .iter()
        .any(|(tag, limit)| {
            item.item.tags.contains(tag)
                && state
                    .packed
                    .placements
                    .iter()
                    .filter(|placement| placement.instance.item.tags.contains(tag))
                    .count()
                    >= *limit
        })
    {
        return Vec::new();
    }
    if points.is_empty() {
        return Vec::new();
    }
    if deadline.expired() || effort_exhausted(request, metrics) {
        return Vec::new();
    }
    let boundary = Aabb {
        origin: Point::ZERO,
        dimensions: state.packed.container.inner_dimensions,
    };
    let rotations = item
        .item
        .dimensions
        .unique_rotations(&item.item.allowed_rotations)
        .into_iter()
        .map(|(rotation, physical)| {
            let envelope = if request.config.clearance.0 > 0 {
                physical.expand(request.config.clearance)
            } else {
                physical
            };
            (rotation, physical, envelope)
        })
        .collect::<Vec<_>>();
    let payload_exceeded = state
        .packed
        .container
        .max_payload
        .is_some_and(|maximum| state.payload.saturating_add(item.item.weight.0) > maximum.0);
    let maximum_items_reached = state
        .packed
        .container
        .max_items
        .is_some_and(|maximum| state.packed.placements.len() >= maximum);
    let item_incompatible = incompatible(state, item);
    let mut candidates = CandidateAccumulator::new(limit);
    for (point_index, point) in points.into_iter().enumerate() {
        if point_index > 0 && (deadline.expired() || effort_exhausted(request, metrics)) {
            break;
        }
        metrics.candidate_points_considered = metrics.candidate_points_considered.saturating_add(1);
        for &(rotation, physical, envelope) in &rotations {
            if effort_exhausted(request, metrics) {
                break;
            }
            metrics.orientations_considered = metrics.orientations_considered.saturating_add(1);
            let envelope_box = Aabb {
                origin: point,
                dimensions: envelope,
            };
            if !boundary.contains(envelope_box) {
                continue;
            }
            let clearance = request.config.clearance.0;
            let position = Point {
                x: point.x.saturating_add(clearance),
                y: point.y.saturating_add(clearance),
                z: point.z.saturating_add(clearance),
            };
            let tentative = item.item.nesting_height.map(|_| Placement {
                instance: item.clone(),
                position,
                rotation,
                dimensions: physical,
                envelope_origin: point,
                envelope_dimensions: envelope,
                support_ratio: 0.0,
                top_load: Weight(0),
            });
            let placement_collision =
                state
                    .spatial_index
                    .query(envelope_box)
                    .into_iter()
                    .any(|index| {
                        metrics.collision_checks = metrics.collision_checks.saturating_add(1);
                        let existing = &state.packed.placements[index];
                        envelope_box.intersects(existing.envelope_box())
                            && !tentative
                                .as_ref()
                                .is_some_and(|placement| valid_nesting(existing, placement))
                    });
            if placement_collision {
                continue;
            }
            let obstacle_collision = state
                .packed
                .container
                .obstacles
                .iter()
                .flat_map(Obstacle::boxes)
                .any(|box_| {
                    metrics.collision_checks = metrics.collision_checks.saturating_add(1);
                    envelope_box.intersects(box_)
                });
            if obstacle_collision {
                continue;
            }
            if payload_exceeded {
                continue;
            }
            if maximum_items_reached {
                continue;
            }
            if axle_overloaded(
                &state.packed.container,
                &state.packed.placements,
                Some((item.item.weight, envelope_box)),
            ) {
                continue;
            }
            if item.item.must_be_on_floor && point.z != 0 {
                continue;
            }
            if item_incompatible {
                continue;
            }
            metrics.support_checks = metrics.support_checks.saturating_add(1);
            let nested_support = tentative
                .as_ref()
                .map(|placement| placement_support_view(&state.packed.placements, placement));
            let support_ratio = nested_support
                .as_ref()
                .map(|support| support.ratio)
                .unwrap_or_else(|| support_ratio(state, envelope_box));
            let required_support = item
                .item
                .minimum_support_ratio
                .max(request.config.minimum_support_ratio);
            if support_ratio + 1e-12 < required_support {
                continue;
            }
            let ground_contact_allowed = nested_support.as_ref().map_or_else(
                || {
                    ground_contact_allowed(
                        item.item.ground_contact_rule.as_deref(),
                        envelope_box,
                        &state.packed.placements,
                    )
                },
                |support| {
                    ground_contact_allowed_from_view(
                        item.item.ground_contact_rule.as_deref(),
                        envelope_box.origin.z,
                        support,
                    )
                },
            );
            if !ground_contact_allowed {
                continue;
            }
            if (state.route_sensitive || item.item.stop_index.is_some())
                && !route_contact_allowed(
                    item.item.stop_index,
                    envelope_box,
                    &state.packed.placements,
                )
            {
                continue;
            }

            let candidate = Candidate {
                envelope_origin: point,
                position,
                rotation,
                dimensions: physical,
                envelope_dimensions: envelope,
                support_ratio,
                score: 0,
            };
            if !candidate_respects_loads(state, item, &candidate, nested_support.as_ref()) {
                continue;
            }
            if exceeds_void_fill_reserve(state, item, &candidate) {
                continue;
            }

            let context = SolverContext {
                request,
                container: &state.packed.container,
                placements: &state.packed.placements,
            };
            if constraints.iter().any(|constraint| {
                !constraint
                    .evaluate(&context, item, point, rotation, physical)
                    .allowed
            }) {
                continue;
            }

            metrics.feasible_candidates = metrics.feasible_candidates.saturating_add(1);
            let mut score = (point.z + envelope.height.0) as i128 * 1_000_000_000
                + (point.y + envelope.width.0) as i128 * 10_000
                + (point.x + envelope.length.0) as i128;
            for scorer in scorers {
                score = score.saturating_add(scorer.score(&context, item, &candidate));
            }
            candidates.push(Candidate { score, ..candidate });
        }
    }
    candidates.finish()
}

pub fn pack_order(
    request: &PackingRequest,
    ordered: &[ItemInstance],
    constraints: &[Arc<dyn PlacementConstraint>],
    scorers: &[Arc<dyn CandidateScorer>],
    solver_name: &str,
    deadline: &Deadline,
) -> PackingResult {
    let started = deadline.now_ns();
    let mut remaining = ordered.to_vec();
    let mut packed = Vec::new();
    let mut metrics = SolverMetrics::default();
    let mut container_sequence = 0;
    let mut inventory = inventory(request);

    if request.config.container_plan_beam_width > 1 {
        let (planned, left) = pack_container_plans(
            request,
            &remaining,
            constraints,
            scorers,
            deadline,
            &mut metrics,
        );
        packed = planned;
        remaining = left;
    } else {
        while !remaining.is_empty()
            && request
                .config
                .max_containers
                .map(|maximum| packed.len() < maximum)
                .unwrap_or(true)
            && !deadline.expired()
            && !effort_exhausted(request, &metrics)
        {
            // Evaluate every eligible container type against the same
            // `remaining` items and commit to whichever placed the most, rather than
            // committing up front to the cheapest/smallest eligible type regardless of
            // how few of the remaining items it can actually hold. Matches the
            // documented `O(c * b * single_solver_cost)` multi-container-selection
            // bound Python's `_across_containers`/PHP's `acrossContainers` already
            // implement (docs/ALGORITHMS-AND-COMPLEXITY.md) -- this was a Rust-specific
            // gap, not a new cost class.
            let mut best: Option<ContainerTrial> = None;
            for container in eligible_containers(request, &remaining, &inventory) {
                if deadline.expired() {
                    break;
                }
                let (state, next) = try_pack_into(
                    &container,
                    container_sequence + 1,
                    &remaining,
                    request,
                    constraints,
                    scorers,
                    deadline,
                    &mut metrics,
                );
                let placed = state.packed.placements.len();
                if placed == 0 {
                    continue;
                }
                let key =
                    container_selection_key(&state, next.len(), remaining.len(), &request.config);
                let replace = match &best {
                    Some(best) => key < best.ordering_key,
                    None => true,
                };
                if replace {
                    best = Some(ContainerTrial {
                        state,
                        remaining: next,
                        ordering_key: key,
                    });
                }
            }
            let Some(best) = best else {
                break;
            };
            let ContainerTrial {
                state,
                remaining: next,
                ..
            } = best;
            let container_id = state.packed.container.id.clone();
            container_sequence += 1;
            decrement(&mut inventory, &container_id);
            packed.push(state.packed);
            remaining = next;
        }
    }

    let timed_out = deadline.expired();
    let effort_limited = effort_exhausted(request, &metrics);
    let unpacked = remaining
        .into_iter()
        .map(|instance| {
            let structural = explain_unfit(request, &instance);
            let reason = if matches!(
                structural.as_str(),
                "no_compatible_container_dimensions" | "payload_exceeded" | "rotation_restricted"
            ) {
                structural
            } else if timed_out {
                "time_limit".into()
            } else if effort_limited {
                "effort_limit".into()
            } else {
                structural
            };
            UnpackedItem::new(instance, reason, Vec::new())
        })
        .collect::<Vec<_>>();
    let complete = unpacked.is_empty();
    let status = if complete {
        PackingStatus::Feasible
    } else if timed_out {
        PackingStatus::TimeLimit
    } else {
        PackingStatus::BestFound
    };
    let score = score_solution(&packed, &unpacked, &request.config);
    PackingResult {
        status,
        containers: packed,
        unpacked,
        algorithm: AlgorithmReport {
            profile: request.config.profile.as_str().into(),
            solver: solver_name.into(),
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

#[derive(Clone)]
struct MultiContainerPlan {
    packed: Vec<PackedContainer>,
    remaining: Vec<ItemInstance>,
    inventory: BTreeMap<String, Option<usize>>,
    sequence: usize,
}

fn multi_plan_score(
    plan: &MultiContainerPlan,
    remaining: &[ItemInstance],
    config: &PackingConfig,
) -> Vec<i128> {
    let unpacked = remaining
        .iter()
        .cloned()
        .map(|item| UnpackedItem::new(item, "search_exhausted".into(), Vec::new()))
        .collect::<Vec<_>>();
    score_solution(&plan.packed, &unpacked, config)
}

fn additional_container_lower_bound(plan: &MultiContainerPlan, request: &PackingRequest) -> usize {
    let available = request
        .containers
        .iter()
        .filter(|container| {
            plan.inventory
                .get(&container.id)
                .copied()
                .flatten()
                .map(|quantity| quantity > 0)
                .unwrap_or(true)
        })
        .collect::<Vec<_>>();
    if plan.remaining.is_empty() || available.is_empty() {
        return 0;
    }
    let mut lower = 0_usize;
    if plan
        .remaining
        .iter()
        .all(|item| item.item.nesting_height.is_none())
    {
        let capacity = available
            .iter()
            .map(|container| container.inner_dimensions.volume())
            .max()
            .unwrap_or(0);
        if capacity > 0 {
            let required = plan
                .remaining
                .iter()
                .map(|item| item.item.dimensions.volume())
                .sum::<i128>();
            lower = lower.max(((required + capacity - 1) / capacity) as usize);
        }
    }
    if available
        .iter()
        .all(|container| container.max_payload.is_some())
    {
        let capacity = available
            .iter()
            .filter_map(|container| container.max_payload)
            .map(|weight| i128::from(weight.0))
            .max()
            .unwrap_or(0);
        if capacity > 0 {
            let required = plan
                .remaining
                .iter()
                .map(|item| i128::from(item.item.weight.0))
                .sum::<i128>();
            lower = lower.max(((required + capacity - 1) / capacity) as usize);
        }
    }
    lower
}

fn multi_plan_bound(
    plan: &MultiContainerPlan,
    request: &PackingRequest,
) -> (Vec<i128>, Vec<String>) {
    let mut score = multi_plan_score(plan, &[], &request.config);
    let count_index = if request.config.objective == "default" {
        1
    } else {
        2
    };
    score[count_index] += additional_container_lower_bound(plan, request) as i128;
    (score, plan.remaining.iter().map(ItemInstance::id).collect())
}

#[allow(clippy::too_many_arguments)]
fn pack_container_plans(
    request: &PackingRequest,
    ordered: &[ItemInstance],
    constraints: &[Arc<dyn PlacementConstraint>],
    scorers: &[Arc<dyn CandidateScorer>],
    deadline: &Deadline,
    metrics: &mut SolverMetrics,
) -> (Vec<PackedContainer>, Vec<ItemInstance>) {
    let initial = MultiContainerPlan {
        packed: Vec::new(),
        remaining: ordered.to_vec(),
        inventory: inventory(request),
        sequence: 0,
    };
    let mut incumbent = initial.clone();
    let mut beam = vec![initial];
    let mut plan_nodes = 0_usize;
    let maximum = request.config.max_containers.unwrap_or(usize::MAX);

    while !beam.is_empty() && plan_nodes < request.config.container_plan_node_limit {
        let mut expansions = Vec::new();
        let mut exhausted = false;
        for plan in &beam {
            if plan.remaining.is_empty() || plan.packed.len() >= maximum {
                if multi_plan_score(plan, &plan.remaining, &request.config)
                    < multi_plan_score(&incumbent, &incumbent.remaining, &request.config)
                {
                    incumbent = plan.clone();
                }
                continue;
            }
            for container in eligible_containers(request, &plan.remaining, &plan.inventory) {
                if plan_nodes >= request.config.container_plan_node_limit {
                    break;
                }
                if deadline.expired() || effort_exhausted(request, metrics) {
                    exhausted = true;
                    break;
                }
                plan_nodes += 1;
                let (state, remaining) = try_pack_into(
                    &container,
                    plan.sequence + 1,
                    &plan.remaining,
                    request,
                    constraints,
                    scorers,
                    deadline,
                    metrics,
                );
                if state.packed.placements.is_empty() {
                    continue;
                }
                let mut child = plan.clone();
                child.sequence += 1;
                child.remaining = remaining;
                decrement(&mut child.inventory, &container.id);
                child.packed.push(state.packed);
                if multi_plan_score(&child, &child.remaining, &request.config)
                    < multi_plan_score(&incumbent, &incumbent.remaining, &request.config)
                {
                    incumbent = child.clone();
                }
                expansions.push(child);
            }
            if exhausted {
                break;
            }
        }
        if exhausted || expansions.is_empty() {
            break;
        }
        let mut dominant =
            BTreeMap::<(Vec<String>, Vec<(String, Option<usize>)>), MultiContainerPlan>::new();
        for plan in expansions {
            let signature = (
                plan.remaining.iter().map(ItemInstance::id).collect(),
                plan.inventory
                    .iter()
                    .map(|(key, value)| (key.clone(), *value))
                    .collect(),
            );
            let replace = dominant
                .get(&signature)
                .map(|previous| {
                    multi_plan_score(&plan, &[], &request.config)
                        < multi_plan_score(previous, &[], &request.config)
                })
                .unwrap_or(true);
            if replace {
                dominant.insert(signature, plan);
            }
        }
        beam = dominant.into_values().collect();
        beam.sort_by_key(|plan| multi_plan_bound(plan, request));
        beam.truncate(request.config.container_plan_beam_width);
    }
    (incumbent.packed, incumbent.remaining)
}

pub fn apply_candidate(state: &mut ContainerState, item: ItemInstance, candidate: &Candidate) {
    state.payload = state.payload.saturating_add(item.item.weight.0);
    let placement_index = state.packed.placements.len();
    state.spatial_index.add(
        placement_index,
        Aabb {
            origin: candidate.envelope_origin,
            dimensions: candidate.envelope_dimensions,
        },
    );
    let placement = Placement {
        instance: item,
        position: candidate.position,
        rotation: candidate.rotation,
        dimensions: candidate.dimensions,
        envelope_origin: candidate.envelope_origin,
        envelope_dimensions: candidate.envelope_dimensions,
        support_ratio: candidate.support_ratio,
        top_load: Weight(0),
    };
    state.used_volume = state
        .used_volume
        .saturating_add(used_volume_delta(&state.packed.placements, &placement));
    state.stack_sensitive |= placement_stack_sensitive(&placement.instance.item);
    state.route_sensitive |= placement.instance.item.stop_index.is_some();
    state.packed.placements.push(placement);
    let envelope_box = state.packed.placements[placement_index].envelope_box();
    let covered = state
        .points
        .iter()
        .filter(|(z, y, x)| {
            point_inside(
                Point {
                    x: *x,
                    y: *y,
                    z: *z,
                },
                envelope_box,
            )
        })
        .copied()
        .collect::<Vec<_>>();
    for key in covered {
        state.points.remove(&key);
    }
    for point in exposed_points(state, envelope_box) {
        absorb_point(state, point);
    }
    if let Some(loads) = calculate_top_loads(&state.packed.placements) {
        for (placement, load) in state.packed.placements.iter_mut().zip(loads) {
            placement.top_load = Weight(load.clamp(0, i64::MAX as i128) as i64);
        }
    }
}

fn point_inside(point: Point, box_: Aabb) -> bool {
    point.x >= box_.origin.x
        && point.x < box_.x2()
        && point.y >= box_.origin.y
        && point.y < box_.y2()
        && point.z >= box_.origin.z
        && point.z < box_.z2()
}

fn point_inside_any_solid(state: &ContainerState, point: Point) -> bool {
    state
        .packed
        .placements
        .iter()
        .map(Placement::envelope_box)
        .chain(
            state
                .packed
                .container
                .obstacles
                .iter()
                .flat_map(Obstacle::boxes),
        )
        .any(|box_| point_inside(point, box_))
}

fn absorb_point(state: &mut ContainerState, point: Point) {
    let boundary = state.packed.container.inner_dimensions;
    if point.x >= boundary.length.0
        || point.y >= boundary.width.0
        || point.z >= boundary.height.0
        || point_inside_any_solid(state, point)
    {
        return;
    }
    state.points.insert((point.z, point.y, point.x));
}

/// The six exposed corners plus their nearest projections onto surfaces below,
/// behind and to the left. At most twelve points are retained per placement, giving
/// the bounded hybrid scan a sparse, geometrically meaningful candidate source.
fn exposed_points(state: &ContainerState, box_: Aabb) -> [Point; 12] {
    let origin = box_.origin;
    let x2 = box_.x2();
    let y2 = box_.y2();
    let z2 = box_.z2();
    [
        Point {
            x: x2,
            y: origin.y,
            z: origin.z,
        },
        Point {
            x: origin.x,
            y: y2,
            z: origin.z,
        },
        Point {
            x: origin.x,
            y: origin.y,
            z: z2,
        },
        Point {
            x: x2,
            y: y2,
            z: origin.z,
        },
        Point {
            x: x2,
            y: origin.y,
            z: z2,
        },
        Point {
            x: origin.x,
            y: y2,
            z: z2,
        },
        Point {
            x: x2,
            y: origin.y,
            z: surface_z(state, x2, origin.y, origin.z),
        },
        Point {
            x: x2,
            y: surface_y(state, x2, origin.z, origin.y),
            z: origin.z,
        },
        Point {
            x: origin.x,
            y: y2,
            z: surface_z(state, origin.x, y2, origin.z),
        },
        Point {
            x: surface_x(state, y2, origin.z, origin.x),
            y: y2,
            z: origin.z,
        },
        Point {
            x: origin.x,
            y: surface_y(state, origin.x, z2, origin.y),
            z: z2,
        },
        Point {
            x: surface_x(state, origin.y, z2, origin.x),
            y: origin.y,
            z: z2,
        },
    ]
}

fn surface_z(state: &ContainerState, x: i64, y: i64, ceiling: i64) -> i64 {
    solid_boxes(state)
        .filter(|box_| {
            box_.z2() <= ceiling
                && box_.origin.x <= x
                && x < box_.x2()
                && box_.origin.y <= y
                && y < box_.y2()
        })
        .map(|box_| box_.z2())
        .max()
        .unwrap_or(0)
}

fn surface_y(state: &ContainerState, x: i64, z: i64, ceiling: i64) -> i64 {
    solid_boxes(state)
        .filter(|box_| {
            box_.y2() <= ceiling
                && box_.origin.x <= x
                && x < box_.x2()
                && box_.origin.z <= z
                && z < box_.z2()
        })
        .map(|box_| box_.y2())
        .max()
        .unwrap_or(0)
}

fn surface_x(state: &ContainerState, y: i64, z: i64, ceiling: i64) -> i64 {
    solid_boxes(state)
        .filter(|box_| {
            box_.x2() <= ceiling
                && box_.origin.y <= y
                && y < box_.y2()
                && box_.origin.z <= z
                && z < box_.z2()
        })
        .map(|box_| box_.x2())
        .max()
        .unwrap_or(0)
}

fn solid_boxes(state: &ContainerState) -> impl Iterator<Item = Aabb> + '_ {
    state
        .packed
        .placements
        .iter()
        .map(Placement::envelope_box)
        .chain(
            state
                .packed
                .container
                .obstacles
                .iter()
                .flat_map(Obstacle::boxes),
        )
}

fn support_ratio(state: &ContainerState, box_: Aabb) -> f64 {
    if box_.origin.z == 0 {
        return 1.0;
    }
    let area = state
        .packed
        .placements
        .iter()
        .filter(|placement| placement.envelope_box().z2() == box_.origin.z)
        .map(|placement| placement.envelope_box().overlap_area_xy(box_))
        .sum::<i128>();
    (area as f64 / box_.dimensions.base_area() as f64).min(1.0)
}

struct CandidateSupportView {
    ratio: f64,
    supporter_count: usize,
    covered: bool,
    placements: Vec<Placement>,
    graph: ContactGraph,
}

fn placement_support_view(existing: &[Placement], candidate: &Placement) -> CandidateSupportView {
    let mut placements = existing.to_vec();
    placements.push(candidate.clone());
    let candidate_index = placements.len() - 1;
    let graph = ContactGraph::from_placements(&placements);
    CandidateSupportView {
        ratio: graph.support_ratio(&placements, candidate_index),
        supporter_count: graph.supporters(candidate_index).len(),
        covered: graph.touches_all_corners(&placements, candidate_index),
        placements,
        graph,
    }
}

fn incompatible(state: &ContainerState, item: &ItemInstance) -> bool {
    !state
        .packed
        .container
        .tags
        .is_disjoint(&item.item.incompatible_tags)
        || state.packed.placements.iter().any(|placement| {
            !placement
                .instance
                .item
                .tags
                .is_disjoint(&item.item.incompatible_tags)
                || !item
                    .item
                    .tags
                    .is_disjoint(&placement.instance.item.incompatible_tags)
        })
}

fn candidate_respects_loads(
    state: &ContainerState,
    item: &ItemInstance,
    candidate: &Candidate,
    support_view: Option<&CandidateSupportView>,
) -> bool {
    if !state.stack_sensitive
        && !placement_stack_sensitive(&item.item)
        && state.packed.container.max_stack_density.is_none()
    {
        return true;
    }
    if let Some(support_view) = support_view {
        let Some(loads) =
            calculate_top_loads_with_graph(&support_view.placements, &support_view.graph)
        else {
            return false;
        };
        return stack_limits_valid(&support_view.placements, &support_view.graph)
            && stack_density_valid_with_loads(
                &support_view.placements,
                state.packed.container.max_stack_density,
                &loads,
            );
    }
    let mut placements = state.packed.placements.clone();
    placements.push(Placement {
        instance: item.clone(),
        position: candidate.position,
        rotation: candidate.rotation,
        dimensions: candidate.dimensions,
        envelope_origin: candidate.envelope_origin,
        envelope_dimensions: candidate.envelope_dimensions,
        support_ratio: candidate.support_ratio,
        top_load: Weight(0),
    });
    let graph = ContactGraph::from_placements(&placements);
    let Some(loads) = calculate_top_loads_with_graph(&placements, &graph) else {
        return false;
    };
    stack_limits_valid(&placements, &graph)
        && stack_density_valid_with_loads(
            &placements,
            state.packed.container.max_stack_density,
            &loads,
        )
}

fn ground_contact_allowed(rule: Option<&str>, candidate: Aabb, placements: &[Placement]) -> bool {
    if candidate.origin.z == 0 || matches!(rule, None | Some("free")) {
        return true;
    }
    let supporters = placements
        .iter()
        .map(Placement::envelope_box)
        .filter(|other| other.z2() == candidate.origin.z && other.overlap_area_xy(candidate) > 0)
        .collect::<Vec<_>>();
    match rule {
        Some("single") => supporters.len() == 1,
        Some("multiple") => supporters.len() >= 2,
        Some("covered") => {
            let corners = [
                (candidate.origin.x, candidate.origin.y),
                (candidate.x2(), candidate.origin.y),
                (candidate.origin.x, candidate.y2()),
                (candidate.x2(), candidate.y2()),
            ];
            corners.iter().all(|(x, y)| {
                supporters.iter().any(|surface| {
                    surface.origin.x <= *x
                        && *x <= surface.x2()
                        && surface.origin.y <= *y
                        && *y <= surface.y2()
                })
            })
        }
        _ => true,
    }
}

fn ground_contact_allowed_from_view(
    rule: Option<&str>,
    candidate_z: i64,
    support: &CandidateSupportView,
) -> bool {
    if candidate_z == 0 || matches!(rule, None | Some("free")) {
        return true;
    }
    match rule {
        Some("single") => support.supporter_count == 1,
        Some("multiple") => support.supporter_count >= 2,
        Some("covered") => support.covered,
        _ => true,
    }
}

/// A placement with no `stop_index` is never scheduled for removal and rides the whole
/// route, so nothing can be blocking it -- but it does block everything beneath it.
/// Ordering it after every real stop expresses both halves at once, and is what
/// docs/VALIDATION-CONTRACT.md and the shared validator already assume.
const RIDES_THE_WHOLE_ROUTE: usize = usize::MAX;

fn route_contact_allowed(
    candidate_stop: Option<usize>,
    candidate: Aabb,
    placements: &[Placement],
) -> bool {
    let candidate_stop = candidate_stop.unwrap_or(RIDES_THE_WHOLE_ROUTE);
    placements.iter().all(|other| {
        let other_stop = other
            .instance
            .item
            .stop_index
            .unwrap_or(RIDES_THE_WHOLE_ROUTE);
        if candidate_stop == RIDES_THE_WHOLE_ROUTE && other_stop == RIDES_THE_WHOLE_ROUTE {
            return true;
        }
        let other_box = other.envelope_box();
        if other_box.overlap_area_xy(candidate) == 0 {
            return true;
        }
        if other_box.z2() == candidate.origin.z {
            candidate_stop <= other_stop
        } else if candidate.z2() == other_box.origin.z {
            candidate_stop >= other_stop
        } else {
            true
        }
    })
}

fn exceeds_void_fill_reserve(
    state: &ContainerState,
    item: &ItemInstance,
    candidate: &Candidate,
) -> bool {
    let reserve = state.packed.container.inner_dimensions.volume()
        * state.packed.container.void_fill_reserve_ppm
        / 1_000_000;
    if reserve == 0 {
        return false;
    }
    let placement = Placement {
        instance: item.clone(),
        position: candidate.position,
        rotation: candidate.rotation,
        dimensions: candidate.dimensions,
        envelope_origin: candidate.envelope_origin,
        envelope_dimensions: candidate.envelope_dimensions,
        support_ratio: candidate.support_ratio,
        top_load: Weight(0),
    };
    state
        .used_volume
        .saturating_add(used_volume_delta(&state.packed.placements, &placement))
        .saturating_add(reserve)
        > state.packed.container.inner_dimensions.volume()
}

fn placement_stack_sensitive(item: &Item) -> bool {
    !item.stackable || item.max_top_load.is_some() || item.max_stacked_items.is_some()
}

/// Physical-volume change from appending one placement.
///
/// Existing overlap pairs cannot change. The common non-nesting case is O(1);
/// nesting-aware candidates scan the already placed items once, O(n) time and O(1)
/// space. This replaces cloning the whole container and recomputing O(n^2) nesting
/// volume for every candidate orientation.
fn used_volume_delta(placements: &[Placement], placement: &Placement) -> i128 {
    let mut delta = placement.dimensions.volume();
    let Some(depth) = placement.instance.item.nesting_height else {
        return delta;
    };
    let overlap = (depth.0 as i128).saturating_mul(placement.envelope_dimensions.base_area());
    for existing in placements {
        if valid_nesting(existing, placement) {
            delta = delta.saturating_sub(overlap);
        }
    }
    delta
}

fn stack_limits_valid(placements: &[Placement], graph: &ContactGraph) -> bool {
    for (root, placement) in placements.iter().enumerate() {
        let Some(maximum) = placement.instance.item.max_stacked_items else {
            continue;
        };
        let mut seen = BTreeSet::new();
        let mut pending = graph.children(root).to_vec();
        while let Some(index) = pending.pop() {
            if seen.insert(index) {
                pending.extend(graph.children(index).iter().copied());
            }
        }
        if seen.len() > maximum {
            return false;
        }
    }
    true
}

fn stack_density_valid_with_loads(
    placements: &[Placement],
    maximum: Option<Weight>,
    loads: &[i128],
) -> bool {
    let Some(maximum) = maximum else {
        return true;
    };
    const SQUARE_METRE_TICKS: i128 = 16_000_000_i128 * 16_000_000_i128;
    placements.iter().zip(loads).all(|(placement, load)| {
        let total = placement.instance.item.weight.0 as i128 + *load;
        total.saturating_mul(SQUARE_METRE_TICKS)
            <= (maximum.0 as i128).saturating_mul(placement.envelope_dimensions.base_area())
    })
}

pub fn calculate_top_loads(placements: &[Placement]) -> Option<Vec<i128>> {
    let graph = ContactGraph::from_placements(placements);
    calculate_top_loads_with_graph(placements, &graph)
}

fn calculate_top_loads_with_graph(
    placements: &[Placement],
    graph: &ContactGraph,
) -> Option<Vec<i128>> {
    let mut loads = vec![0_i128; placements.len()];
    let mut order = (0..placements.len()).collect::<Vec<_>>();
    order.sort_by_key(|index| {
        std::cmp::Reverse((
            placements[*index].envelope_box().z2(),
            placements[*index].envelope_origin.z,
        ))
    });

    for upper_index in order {
        let upper = placements[upper_index].envelope_box();
        if upper.origin.z == 0 {
            continue;
        }
        let supports = graph.supporters(upper_index);
        if supports.is_empty() {
            continue;
        }
        let total_area = supports.iter().map(|edge| edge.area).sum::<i128>();
        let downward = placements[upper_index].instance.item.weight.0 as i128 + loads[upper_index];
        let mut distributed = 0_i128;
        for (position, edge) in supports.iter().enumerate() {
            let share = if position + 1 == supports.len() {
                downward.saturating_sub(distributed)
            } else {
                downward.saturating_mul(edge.area) / total_area
            };
            distributed = distributed.saturating_add(share);
            let support = &placements[edge.index].instance.item;
            // `stackable: false` is geometry, not load: nothing may rest on the item at
            // all. Gating it on `share > 0` meant a weightless box resting on a
            // non-stackable one transferred nothing and so was waved through, and since
            // nothing here self-validates the answer came back `feasible` while the
            // shared validator called it `non_stackable`.
            if !support.stackable {
                return None;
            }
            loads[edge.index] = loads[edge.index].saturating_add(share);
            if let Some(maximum) = support.max_top_load
                && loads[edge.index] > maximum.0 as i128
            {
                return None;
            }
        }
    }
    Some(loads)
}

fn inventory(request: &PackingRequest) -> BTreeMap<String, Option<usize>> {
    request
        .containers
        .iter()
        .map(|container| (container.id.clone(), container.quantity))
        .collect()
}

fn decrement(inventory: &mut BTreeMap<String, Option<usize>>, id: &str) {
    if let Some(Some(value)) = inventory.get_mut(id) {
        *value = value.saturating_sub(1);
    }
}

fn eligible_containers(
    request: &PackingRequest,
    items: &[ItemInstance],
    inventory: &BTreeMap<String, Option<usize>>,
) -> Vec<Container> {
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
                    (item.item.eligible_container_tags.is_empty()
                        || !item
                            .item
                            .eligible_container_tags
                            .is_disjoint(&container.tags))
                        && item
                            .item
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
    containers
}

/// Trial-pack as many of `remaining` as this one container can hold, batching group
/// members together. Returns the resulting state (possibly empty, if
/// nothing fit) and whatever did not fit. Metrics accumulate into the caller's
/// shared counters even for a trial that is ultimately discarded, matching Python's
/// `_across_containers`/PHP's `acrossContainers`, which do the same per-template
/// work and pay for it in `SolverMetrics` regardless of which template wins.
#[allow(clippy::too_many_arguments)]
fn try_pack_into(
    container: &Container,
    sequence: usize,
    remaining: &[ItemInstance],
    request: &PackingRequest,
    constraints: &[Arc<dyn PlacementConstraint>],
    scorers: &[Arc<dyn CandidateScorer>],
    deadline: &Deadline,
    metrics: &mut SolverMetrics,
) -> (ContainerState, Vec<ItemInstance>) {
    if request.config.container_plan_beam_width > 1 {
        return try_pack_into_beam(
            container,
            sequence,
            remaining,
            request,
            constraints,
            scorers,
            deadline,
            metrics,
        );
    }
    try_pack_into_greedy(
        container,
        sequence,
        remaining,
        request,
        constraints,
        scorers,
        deadline,
        metrics,
    )
}

#[allow(clippy::too_many_arguments)]
fn try_pack_into_greedy(
    container: &Container,
    sequence: usize,
    remaining: &[ItemInstance],
    request: &PackingRequest,
    constraints: &[Arc<dyn PlacementConstraint>],
    scorers: &[Arc<dyn CandidateScorer>],
    deadline: &Deadline,
    metrics: &mut SolverMetrics,
) -> (ContainerState, Vec<ItemInstance>) {
    let mut state = ContainerState::new(container.clone(), sequence);
    let current = remaining;
    let mut next = Vec::new();
    let mut processed = vec![false; current.len()];

    for cursor in 0..current.len() {
        if effort_exhausted(request, metrics) {
            next.extend(
                current
                    .iter()
                    .enumerate()
                    .filter(|(index, _)| !processed[*index])
                    .map(|(_, item)| item.clone()),
            );
            break;
        }
        if processed[cursor] {
            continue;
        }
        metrics.search_nodes_expanded = metrics.search_nodes_expanded.saturating_add(1);
        let batch_indices = if let Some(group) = current[cursor].item.group.as_deref() {
            (cursor..current.len())
                .filter(|index| {
                    !processed[*index] && current[*index].item.group.as_deref() == Some(group)
                })
                .collect::<Vec<_>>()
        } else {
            vec![cursor]
        };
        for index in &batch_indices {
            processed[*index] = true;
        }
        let batch = batch_indices
            .iter()
            .map(|index| current[*index].clone())
            .collect::<Vec<_>>();
        // `apply_candidate` only runs when a candidate exists, so a single-item
        // batch that fails has not touched `state` and needs no rollback; only a
        // multi-item group can be left half-applied. The snapshot deep-copies
        // every placement, candidate point and spatial-index bucket accumulated
        // so far, so cloning it once per item on the default ungrouped path was
        // pure allocator load.
        let snapshot = (batch.len() > 1).then(|| state.clone());
        let mut accepted = true;
        for item in &batch {
            let candidates = find_candidates(
                &state,
                item,
                request,
                constraints,
                scorers,
                request.config.max_candidates_per_item,
                deadline,
                metrics,
            );
            if let Some(candidate) = candidates.first() {
                apply_candidate(&mut state, item.clone(), candidate);
            } else {
                accepted = false;
                break;
            }
        }
        if !accepted {
            if let Some(snapshot) = snapshot {
                state = snapshot;
            }
            next.extend(batch);
        }
    }
    (state, next)
}

#[derive(Clone)]
struct ContainerBeamNode {
    state: ContainerState,
    unplaced: Vec<ItemInstance>,
}

fn maximum_count_with_capacity(mut costs: Vec<i128>, capacity: i128) -> usize {
    costs.sort_unstable();
    let mut used = 0_i128;
    costs
        .into_iter()
        .take_while(|cost| {
            if used.saturating_add(*cost) > capacity {
                false
            } else {
                used = used.saturating_add(*cost);
                true
            }
        })
        .count()
}

fn unpacked_lower_bound(
    state: &ContainerState,
    unplaced: &[ItemInstance],
    future: &[ItemInstance],
) -> usize {
    if future.is_empty() {
        return unplaced.len();
    }
    let mut possible = future.len();
    if future.iter().all(|item| item.item.nesting_height.is_none()) {
        let free = (state.packed.container.inner_dimensions.volume() - state.used_volume).max(0);
        possible = possible.min(maximum_count_with_capacity(
            future
                .iter()
                .map(|item| item.item.dimensions.volume())
                .collect(),
            free,
        ));
    }
    if let Some(max_payload) = state.packed.container.max_payload {
        let free = (i128::from(max_payload.0) - i128::from(state.payload)).max(0);
        possible = possible.min(maximum_count_with_capacity(
            future
                .iter()
                .map(|item| i128::from(item.item.weight.0))
                .collect(),
            free,
        ));
    }
    unplaced.len() + future.len() - possible
}

fn container_beam_key(
    node: &ContainerBeamNode,
    future: &[ItemInstance],
) -> (usize, usize, Reverse<usize>, i128, Reverse<i128>, String) {
    let signature = node
        .state
        .packed
        .placements
        .iter()
        .map(|placement| {
            format!(
                "{}@{},{},{}",
                placement.instance.id(),
                placement.envelope_origin.x,
                placement.envelope_origin.y,
                placement.envelope_origin.z
            )
        })
        .collect::<Vec<_>>()
        .join("|");
    (
        unpacked_lower_bound(&node.state, &node.unplaced, future),
        node.unplaced.len(),
        Reverse(node.state.packed.placements.len()),
        node.state.packed.max_z_ticks(),
        Reverse(node.state.used_volume),
        signature,
    )
}

fn item_batches(items: &[ItemInstance]) -> Vec<Vec<ItemInstance>> {
    let mut batches = Vec::new();
    let mut consumed = BTreeSet::new();
    for item in items {
        if consumed.contains(&item.id()) {
            continue;
        }
        let batch = if let Some(group) = item.item.group.as_deref() {
            items
                .iter()
                .filter(|candidate| candidate.item.group.as_deref() == Some(group))
                .cloned()
                .collect::<Vec<_>>()
        } else {
            vec![item.clone()]
        };
        consumed.extend(batch.iter().map(ItemInstance::id));
        batches.push(batch);
    }
    batches
}

#[allow(clippy::too_many_arguments)]
fn try_pack_into_beam(
    container: &Container,
    sequence: usize,
    remaining: &[ItemInstance],
    request: &PackingRequest,
    constraints: &[Arc<dyn PlacementConstraint>],
    scorers: &[Arc<dyn CandidateScorer>],
    deadline: &Deadline,
    metrics: &mut SolverMetrics,
) -> (ContainerState, Vec<ItemInstance>) {
    let batches = item_batches(remaining);
    let (greedy_state, greedy_unplaced) = try_pack_into_greedy(
        container,
        sequence,
        remaining,
        request,
        constraints,
        scorers,
        deadline,
        metrics,
    );
    let initial = ContainerBeamNode {
        state: ContainerState::new(container.clone(), sequence),
        unplaced: Vec::new(),
    };
    let mut incumbent = ContainerBeamNode {
        state: greedy_state,
        unplaced: greedy_unplaced,
    };
    let mut beam = vec![initial];
    let mut nodes = 0_usize;

    for (position, batch) in batches.iter().enumerate() {
        let future = batches[position + 1..]
            .iter()
            .flatten()
            .cloned()
            .collect::<Vec<_>>();
        let mut expansions = Vec::new();
        let mut exhausted = false;
        for node in &beam {
            if nodes >= request.config.container_plan_node_limit
                || deadline.expired()
                || effort_exhausted(request, metrics)
            {
                exhausted = true;
                break;
            }
            nodes += 1;
            metrics.search_nodes_expanded = metrics.search_nodes_expanded.saturating_add(1);
            let mut children = Vec::new();
            if batch.len() == 1 {
                for candidate in find_candidates(
                    &node.state,
                    &batch[0],
                    request,
                    constraints,
                    scorers,
                    request.config.max_candidates_per_item,
                    deadline,
                    metrics,
                ) {
                    let mut state = node.state.clone();
                    apply_candidate(&mut state, batch[0].clone(), &candidate);
                    children.push(state);
                }
            } else {
                let mut state = node.state.clone();
                let mut accepted = true;
                for item in batch {
                    let candidates = find_candidates(
                        &state,
                        item,
                        request,
                        constraints,
                        scorers,
                        1,
                        deadline,
                        metrics,
                    );
                    if let Some(candidate) = candidates.first() {
                        apply_candidate(&mut state, item.clone(), candidate);
                    } else {
                        accepted = false;
                        break;
                    }
                }
                if accepted {
                    children.push(state);
                }
            }
            for state in children {
                expansions.push(ContainerBeamNode {
                    state,
                    unplaced: node.unplaced.clone(),
                });
            }
            let mut skipped = node.unplaced.clone();
            skipped.extend(batch.iter().cloned());
            expansions.push(ContainerBeamNode {
                state: node.state.clone(),
                unplaced: skipped,
            });
        }
        for node in &expansions {
            let mut complete = node.clone();
            complete.unplaced.extend(future.iter().cloned());
            if container_beam_key(&complete, &[]) < container_beam_key(&incumbent, &[]) {
                incumbent = complete;
            }
        }
        if expansions.is_empty() || exhausted {
            break;
        }
        expansions.sort_by_key(|node| container_beam_key(node, &future));
        expansions.truncate(request.config.container_plan_beam_width);
        beam = expansions;
    }
    if let Some(completed) = beam
        .into_iter()
        .min_by_key(|node| container_beam_key(node, &[]))
        && container_beam_key(&completed, &[]) < container_beam_key(&incumbent, &[])
    {
        incumbent = completed;
    }
    (incumbent.state, incumbent.unplaced)
}

/// Ratios in the objective vector are expressed as parts per million so the whole
/// vector stays exactly comparable across languages.
pub(crate) const SCORE_SCALE: i128 = 1_000_000;

pub(crate) fn dimensional_weight_ticks(container: &Container, config: &PackingConfig) -> i128 {
    let dimensions = container
        .outer_dimensions
        .unwrap_or(container.inner_dimensions);
    let length_ticks_per_unit = match config.dimensional_weight_length_unit.as_str() {
        "mm" => Length::TICKS_PER_MM as i128,
        "cm" => (Length::TICKS_PER_MM * 10) as i128,
        "m" => (Length::TICKS_PER_MM * 1_000) as i128,
        "in" => Length::TICKS_PER_INCH as i128,
        "ft" => (Length::TICKS_PER_INCH * 12) as i128,
        _ => unreachable!("configuration units are validated at admission"),
    };
    let weight_ticks_per_unit = match config.dimensional_weight_weight_unit.as_str() {
        "mg" => Weight::TICKS_PER_MG as i128,
        "g" => Weight::TICKS_PER_G as i128,
        "kg" => Weight::TICKS_PER_KG as i128,
        "oz" => Weight::TICKS_PER_OZ as i128,
        "lb" => Weight::TICKS_PER_LB as i128,
        _ => unreachable!("configuration units are validated at admission"),
    };
    dimensions.volume() * weight_ticks_per_unit
        / (length_ticks_per_unit.pow(3)
            * config
                .dimensional_weight_divisor
                .expect("shipping_cost divisor is validated at admission"))
}

/// How good opening this container type would be, ranked the way the finished result
/// will be ranked.
///
/// The previous key led with the number of items placed, so the requested objective was
/// only a tie-break: under `lowest_cost` a container that held one more item beat any
/// saving, however large, and Rust paid four times what Python paid on
/// `regression-smallest-sufficient-container` -- a fixture that exists to assert the
/// cheapest sufficient container is chosen. The terms below are the same ones
/// `score_solution` uses, assembled in the same order that objective puts them, so the
/// per-round choice and the final score cannot disagree about what "better" means.
///
/// `shipping_cost` deserves its own note: the old key ranked by the container's
/// *dimensional* weight while the score charges `max(gross, dimensional)`, the billable
/// weight. Selection optimised a figure nobody is charged whenever actual weight won.
fn container_selection_key(
    state: &ContainerState,
    remaining_after: usize,
    remaining_before: usize,
    config: &PackingConfig,
) -> (Vec<i128>, String) {
    let packed = &state.packed;
    let container = &packed.container;
    let inner = container.inner_dimensions;
    let volume = inner.volume();
    let placed = (remaining_before - remaining_after) as i128;

    // How many copies of this template would finish the remaining items, which is this
    // choice's contribution to `container_count`. Zero-placed trials never reach here.
    // Written out rather than via `div_ceil`, which is still unstable for `i128`. Both
    // operands are positive here, so the usual round-up form is exact.
    let per_container = placed.max(1);
    let containers_needed = (remaining_before as i128 + per_container - 1) / per_container;
    let cost = container.cost_minor as i128;
    let top = packed.max_z_ticks();
    let unused_ppm = if volume > 0 {
        (volume - packed.used_volume()) * SCORE_SCALE / volume
    } else {
        0
    };
    let inner_height = inner.height.0 as i128;
    let height_ppm = if inner_height > 0 {
        top * SCORE_SCALE / inner_height
    } else {
        0
    };
    let billable =
        if config.objective == "shipping_cost" || config.objective == "lowest_landed_cost" {
            i128::from(packed.gross_weight().0).max(dimensional_weight_ticks(container, config))
        } else {
            0
        };

    // `-placed` sits after the objective's *decisive* keys and before its two ratio
    // keys, and the position is deliberate. Container count and cost are what the
    // objective actually optimises, so they lead. Unused volume and stack height are its
    // two lowest-priority keys, and optimising them greedily one container at a time is
    // speculative -- filling this box tightest can leave an awkward remainder for the
    // next. Placing more is the sounder proxy for finishing in fewer containers, and
    // putting it after the ratio keys measurably regressed
    // `regression-cumulative-weight-never-exceeds-max-payload`.
    let key = match config.objective.as_str() {
        "lowest_cost" => vec![cost, containers_needed, -placed, unused_ppm, height_ppm],
        // Landed cost shares this key with shipping_cost on purpose: at the moment a
        // container is opened its final billed weight is not yet known, so the tariff
        // cannot be applied. Billable weight is the same monotone proxy for it that this
        // key already uses, and `score_solution` prices the finished answer exactly.
        "shipping_cost" | "lowest_landed_cost" => {
            vec![billable, containers_needed, -placed, unused_ppm, height_ppm]
        }
        "open_dimension_height" => vec![top, containers_needed, cost, -placed, unused_ppm],
        // `maximum_value` ranks by value forgone, which is a property of what is left
        // unpacked overall rather than of the container being opened, so a per-round
        // choice cannot see it. Item ordering is where that objective is served; this
        // falls back to the default order rather than pretending otherwise.
        _ => vec![containers_needed, cost, -placed, unused_ppm, height_ppm],
    };
    (key, container.id.clone())
}

pub fn container_order_key(
    container: &Container,
    config: &PackingConfig,
) -> (i128, i64, i128, String) {
    let primary = if config.objective == "shipping_cost" || config.objective == "lowest_landed_cost"
    {
        dimensional_weight_ticks(container, config)
    } else {
        container.cost_minor as i128
    };
    (
        primary,
        container.cost_minor,
        container.inner_dimensions.volume(),
        container.id.clone(),
    )
}

/// Canonical objective vector — see `docs/OBJECTIVE.md`.
///
/// Five lexicographic keys, ascending, lower is better. Every key is an exact integer:
/// the ratios are floored per container in `i128` rather than computed in floating
/// point, so Python, PHP, Rust and the JavaScript fallback all agree bit-for-bit.
pub fn score_solution(
    containers: &[PackedContainer],
    unpacked: &[UnpackedItem],
    config: &PackingConfig,
) -> Vec<i128> {
    let mut unused = 0_i128;
    let mut height = 0_i128;
    let mut billable = 0_i128;
    let mut landed = 0_i128;
    let mut achieved_height = 0_i128;

    for packed in containers {
        let inner = packed.container.inner_dimensions;

        let volume = inner.volume();
        if volume > 0 {
            unused += (volume - packed.used_volume()) * SCORE_SCALE / volume;
        }

        let top = packed.max_z_ticks();
        achieved_height += top;
        let inner_height = inner.height.0 as i128;
        if inner_height > 0 {
            height += top * SCORE_SCALE / inner_height;
        }

        if config.objective == "shipping_cost" || config.objective == "lowest_landed_cost" {
            let dimensional = dimensional_weight_ticks(&packed.container, config);
            let billed = i128::from(packed.gross_weight().0).max(dimensional);
            billable += billed;
            if config.objective == "lowest_landed_cost" {
                // A container with no table, or a weight past the last bracket, cannot be
                // priced. Ranking it as free would make the objective prefer exactly the
                // packing the caller cannot ship, so the run is scored as the worst
                // possible answer instead -- the same effect Python and PHP get by
                // refusing outright, reached without giving this function a fallible
                // signature every other objective would have to carry.
                landed = match packed
                    .container
                    .rate_table
                    .as_ref()
                    .and_then(|table| table.charge_minor(RateTable::grams(billed as i64)))
                {
                    Some(charge) => landed.saturating_add(i128::from(charge)),
                    None => i128::MAX,
                };
            }
        }
    }

    let default = vec![
        unpacked.len() as i128,
        containers.len() as i128,
        containers
            .iter()
            .map(|container| container.container.cost_minor as i128)
            .sum(),
        unused,
        height,
    ];
    if config.objective == "lowest_cost" {
        vec![default[0], default[2], default[1], default[3], default[4]]
    } else if config.objective == "shipping_cost" {
        vec![default[0], billable, default[1], default[3], default[4]]
    } else if config.objective == "lowest_landed_cost" {
        vec![default[0], landed, default[1], default[3], default[4]]
    } else if config.objective == "open_dimension_height" {
        vec![
            default[0],
            achieved_height,
            default[1],
            default[2],
            default[3],
        ]
    } else if config.objective == "maximum_value" {
        // Ranks by the total declared value of unpacked items ahead of
        // container count, cost and unused volume. `unpacked_count` still leads --
        // no selectable trade-off may prefer an incomplete answer over a complete
        // one -- this only tie-breaks among solutions that leave the same *number*
        // of items unpacked but not the same *value* forgone.
        let value_forgone: i128 = unpacked
            .iter()
            .map(|item| item.instance.item.value.unwrap_or(0) as i128)
            .sum();
        vec![
            default[0],
            value_forgone,
            default[1],
            default[2],
            default[3],
        ]
    } else {
        default
    }
}

pub(super) fn explain_unfit(request: &PackingRequest, item: &ItemInstance) -> String {
    let fits_with = |rotations: &[Rotation]| {
        request.containers.iter().any(|container| {
            item.item
                .dimensions
                .unique_rotations(rotations)
                .iter()
                .any(|(_, dimensions)| {
                    dimensions
                        .expand(request.config.clearance)
                        .fits_inside(container.inner_dimensions)
                })
        })
    };
    // Same geometric check with every physical orientation allowed, not only the
    // item's own restricted set: distinguishes "genuinely too big in any rotation"
    // from "this exact rotation restriction, and only it, rules every container out"
    //. Both are pure geometry, so both are provable without a complete
    // search.
    if !fits_with(&Rotation::ALL) {
        "no_compatible_container_dimensions".into()
    } else if !fits_with(&item.item.allowed_rotations) {
        "rotation_restricted".into()
    } else if request.containers.iter().all(|container| {
        container
            .max_payload
            .map(|maximum| item.item.weight.0 > maximum.0)
            .unwrap_or(false)
    }) {
        "payload_exceeded".into()
    } else if !item.item.eligible_container_tags.is_empty()
        && request.containers.iter().all(|container| {
            item.item
                .eligible_container_tags
                .is_disjoint(&container.tags)
        })
    {
        "no_eligible_container".into()
    } else {
        "search_exhausted".into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dimensions(length: i64, width: i64, height: i64) -> Dimensions {
        Dimensions {
            length: Length(length),
            width: Length(width),
            height: Length(height),
        }
    }

    fn candidate(score: i128, origin: Point, rotation: Rotation, support_ratio: f64) -> Candidate {
        let dimensions = dimensions(10, 20, 30);
        Candidate {
            envelope_origin: origin,
            position: origin,
            rotation,
            dimensions,
            envelope_dimensions: dimensions,
            support_ratio,
            score,
        }
    }

    fn candidate_fingerprint(
        candidate: &Candidate,
    ) -> (CandidateOrderKey, u64, Dimensions, Dimensions) {
        (
            candidate_order_key(candidate),
            candidate.support_ratio.to_bits(),
            candidate.dimensions,
            candidate.envelope_dimensions,
        )
    }

    fn reference_selection(mut candidates: Vec<Candidate>, limit: usize) -> Vec<Candidate> {
        candidates.sort_by_key(candidate_order_key);
        candidates.truncate(limit.max(1));
        candidates
    }

    fn bounded_selection(candidates: Vec<Candidate>, limit: usize) -> Vec<Candidate> {
        let mut selected = CandidateAccumulator::new(limit);
        for candidate in candidates {
            selected.push(candidate);
        }
        selected.finish()
    }

    #[test]
    fn bounded_candidate_selection_matches_the_full_stable_sort() {
        let candidates = vec![
            candidate(9, Point { x: 8, y: 0, z: 0 }, Rotation::Lwh, 0.1),
            candidate(2, Point { x: 4, y: 0, z: 0 }, Rotation::Wlh, 0.2),
            // Same public ordering key, deliberately different marker. The bounded
            // collector must preserve the stable first-seen order of the full sort.
            candidate(2, Point { x: 4, y: 0, z: 0 }, Rotation::Wlh, 0.3),
            candidate(1, Point { x: 9, y: 0, z: 0 }, Rotation::Lwh, 0.4),
            candidate(2, Point { x: 1, y: 0, z: 0 }, Rotation::Hwl, 0.5),
            candidate(7, Point { x: 0, y: 2, z: 0 }, Rotation::Lhw, 0.6),
        ];

        for limit in [0, 1, 3, 8, usize::MAX] {
            let expected = reference_selection(candidates.clone(), limit)
                .iter()
                .map(candidate_fingerprint)
                .collect::<Vec<_>>();
            let actual = bounded_selection(candidates.clone(), limit)
                .iter()
                .map(candidate_fingerprint)
                .collect::<Vec<_>>();
            assert_eq!(actual, expected, "limit={limit}");
        }
    }

    fn item(id: &str) -> Item {
        Item {
            id: id.into(),
            dimensions: dimensions(10, 20, 30),
            weight: Weight(0),
            quantity: 1,
            allowed_rotations: Rotation::ALL.to_vec(),
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
        }
    }

    fn container() -> Container {
        Container {
            id: "box".into(),
            inner_dimensions: dimensions(1_000, 1_000, 1_000),
            outer_dimensions: None,
            tare_weight: Weight(0),
            max_payload: None,
            cost_minor: 0,
            quantity: Some(1),
            obstacles: Vec::new(),
            tags: BTreeSet::new(),
            max_items: None,
            metadata: BTreeMap::new(),
            axles: None,
            void_fill_reserve_ppm: 0,
            tag_limits: BTreeMap::new(),
            max_stack_density: None,
            rate_table: None,
        }
    }

    fn request(
        item: &Item,
        container: &Container,
        effort_budget: Option<EffortBudget>,
    ) -> PackingRequest {
        let config = PackingConfig {
            time_limit_ms: 60_000,
            effort_budget,
            ..Default::default()
        };
        PackingRequest {
            items: vec![item.clone()],
            containers: vec![container.clone()],
            config,
            output_length_unit: "ticks".into(),
            output_weight_unit: "ticks".into(),
            catalog_versions_used: Vec::new(),
        }
    }

    fn candidates_with_limit(limit: usize) -> (Vec<Candidate>, SolverMetrics) {
        let item = item("varied");
        let container = container();
        let request = request(&item, &container, None);
        let state = ContainerState::new(container, 1);
        let instance = ItemInstance { item, sequence: 1 };
        let mut metrics = SolverMetrics::default();
        let candidates = find_candidates_at_points(
            &state,
            &instance,
            &request,
            &[],
            &[],
            vec![Point::ZERO, Point { x: 200, y: 0, z: 0 }],
            limit,
            &Deadline::new(60_000),
            &mut metrics,
        );
        (candidates, metrics)
    }

    #[test]
    fn bounded_selection_does_not_shorten_candidate_effort_metrics() {
        let (one, one_metrics) = candidates_with_limit(1);
        let (three, three_metrics) = candidates_with_limit(3);

        assert_eq!(one.len(), 1);
        assert_eq!(three.len(), 3);
        assert_eq!(one_metrics.candidate_points_considered, 2);
        assert_eq!(one_metrics.orientations_considered, 12);
        assert_eq!(one_metrics.feasible_candidates, 12);
        assert_eq!(one_metrics.support_checks, 12);
        assert_eq!(
            one_metrics.to_json(),
            three_metrics.to_json(),
            "retained width must not change the counted search prefix"
        );
    }

    #[test]
    fn invariant_incompatibility_keeps_the_existing_effort_prefix() {
        let container = container();
        let mut state = ContainerState::new(container.clone(), 1);
        let mut base = item("base");
        base.tags.insert("hazard".into());
        let base_candidate = candidate(0, Point::ZERO, Rotation::Lwh, 1.0);
        apply_candidate(
            &mut state,
            ItemInstance {
                item: base,
                sequence: 1,
            },
            &base_candidate,
        );

        let mut incoming = item("incoming");
        incoming.incompatible_tags.insert("hazard".into());
        let effort_budget = EffortBudget {
            max_placement_attempts: Some(5),
            ..Default::default()
        };
        let request = request(&incoming, &container, Some(effort_budget));
        let mut metrics = SolverMetrics::default();
        let candidates = find_candidates_at_points(
            &state,
            &ItemInstance {
                item: incoming,
                sequence: 1,
            },
            &request,
            &[],
            &[],
            vec![Point { x: 200, y: 0, z: 0 }, Point { x: 400, y: 0, z: 0 }],
            1,
            &Deadline::new(60_000),
            &mut metrics,
        );

        assert!(candidates.is_empty());
        assert_eq!(metrics.candidate_points_considered, 1);
        assert_eq!(metrics.orientations_considered, 5);
        assert_eq!(metrics.feasible_candidates, 0);
        assert_eq!(metrics.support_checks, 0);
    }

    #[test]
    fn candidate_load_check_counts_nested_support_edges() {
        let mut nested = item("nested");
        nested.dimensions = dimensions(10, 10, 10);
        nested.allowed_rotations = vec![Rotation::Lwh];
        nested.weight = Weight(Weight::TICKS_PER_KG);
        nested.max_top_load = Some(Weight(Weight::TICKS_PER_KG * 3 / 2));
        nested.nesting_height = Some(Length(5));
        let mut state = ContainerState::new(container(), 1);
        let candidate_at = |z| Candidate {
            envelope_origin: Point { x: 0, y: 0, z },
            position: Point { x: 0, y: 0, z },
            rotation: Rotation::Lwh,
            dimensions: dimensions(10, 10, 10),
            envelope_dimensions: dimensions(10, 10, 10),
            support_ratio: 1.0,
            score: 0,
        };

        apply_candidate(
            &mut state,
            ItemInstance {
                item: nested.clone(),
                sequence: 1,
            },
            &candidate_at(0),
        );
        let second = ItemInstance {
            item: nested.clone(),
            sequence: 2,
        };
        assert!(candidate_respects_loads(
            &state,
            &second,
            &candidate_at(5),
            None
        ));
        apply_candidate(&mut state, second, &candidate_at(5));

        let third = ItemInstance {
            item: nested,
            sequence: 3,
        };
        assert!(!candidate_respects_loads(
            &state,
            &third,
            &candidate_at(10),
            None
        ));
    }

    #[test]
    fn nested_top_loads_follow_only_the_adjacent_column_edges() {
        let mut nested = item("nested-loads");
        nested.dimensions = dimensions(10, 10, 10);
        nested.allowed_rotations = vec![Rotation::Lwh];
        nested.weight = Weight(100);
        nested.nesting_height = Some(Length(5));
        let candidate_at = |z| Candidate {
            envelope_origin: Point { x: 0, y: 0, z },
            position: Point { x: 0, y: 0, z },
            rotation: Rotation::Lwh,
            dimensions: dimensions(10, 10, 10),
            envelope_dimensions: dimensions(10, 10, 10),
            support_ratio: 1.0,
            score: 0,
        };
        let mut state = ContainerState::new(container(), 1);
        for (sequence, z) in [0, 5, 10].into_iter().enumerate() {
            apply_candidate(
                &mut state,
                ItemInstance {
                    item: nested.clone(),
                    sequence: sequence + 1,
                },
                &candidate_at(z),
            );
        }

        assert_eq!(
            calculate_top_loads(&state.packed.placements),
            Some(vec![200, 100, 0])
        );
        state.packed.placements[1].instance.item.max_top_load = Some(Weight(75));
        assert_eq!(calculate_top_loads(&state.packed.placements), None);
    }

    #[test]
    fn nested_candidate_uses_its_direct_predecessor_for_support_and_ground_rules() {
        for rule in ["single", "covered"] {
            let mut nested = item("nested-support");
            nested.dimensions = dimensions(10, 10, 10);
            nested.allowed_rotations = vec![Rotation::Lwh];
            nested.nesting_height = Some(Length(5));
            nested.minimum_support_ratio = 1.0;
            nested.ground_contact_rule = Some(rule.into());
            let container = container();
            let request = request(&nested, &container, None);
            let base_candidate = Candidate {
                envelope_origin: Point::ZERO,
                position: Point::ZERO,
                rotation: Rotation::Lwh,
                dimensions: dimensions(10, 10, 10),
                envelope_dimensions: dimensions(10, 10, 10),
                support_ratio: 1.0,
                score: 0,
            };
            let mut state = ContainerState::new(container, 1);
            apply_candidate(
                &mut state,
                ItemInstance {
                    item: nested.clone(),
                    sequence: 1,
                },
                &base_candidate,
            );
            let mut metrics = SolverMetrics::default();
            let candidates = find_candidates_at_points(
                &state,
                &ItemInstance {
                    item: nested,
                    sequence: 2,
                },
                &request,
                &[],
                &[],
                vec![Point { x: 0, y: 0, z: 5 }],
                1,
                &Deadline::new(60_000),
                &mut metrics,
            );

            assert_eq!(candidates.len(), 1, "rule={rule}");
            assert_eq!(candidates[0].support_ratio, 1.0, "rule={rule}");
            apply_candidate(
                &mut state,
                ItemInstance {
                    item: request.items[0].clone(),
                    sequence: 2,
                },
                &candidates[0],
            );
            assert_eq!(state.packed.placements[1].support_ratio, 1.0);
            assert_eq!(metrics.support_checks, 1);
            assert_eq!(metrics.feasible_candidates, 1);
        }

        let mut nested = item("nested-support");
        nested.dimensions = dimensions(10, 10, 10);
        nested.allowed_rotations = vec![Rotation::Lwh];
        nested.nesting_height = Some(Length(5));
        nested.minimum_support_ratio = 1.0;
        nested.ground_contact_rule = Some("multiple".into());
        let container = container();
        let request = request(&nested, &container, None);
        let mut state = ContainerState::new(container, 1);
        apply_candidate(
            &mut state,
            ItemInstance {
                item: nested.clone(),
                sequence: 1,
            },
            &Candidate {
                envelope_origin: Point::ZERO,
                position: Point::ZERO,
                rotation: Rotation::Lwh,
                dimensions: dimensions(10, 10, 10),
                envelope_dimensions: dimensions(10, 10, 10),
                support_ratio: 1.0,
                score: 0,
            },
        );
        let mut metrics = SolverMetrics::default();
        let candidates = find_candidates_at_points(
            &state,
            &ItemInstance {
                item: nested,
                sequence: 2,
            },
            &request,
            &[],
            &[],
            vec![Point { x: 0, y: 0, z: 5 }],
            1,
            &Deadline::new(60_000),
            &mut metrics,
        );

        assert!(candidates.is_empty());
        assert_eq!(metrics.support_checks, 1);
        assert_eq!(metrics.feasible_candidates, 0);
    }
}
