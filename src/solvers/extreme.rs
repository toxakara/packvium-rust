use std::rc::Rc;

use crate::contact_graph::{ContactEdge, ContactGraph};
use crate::deadline::Deadline;
use crate::geometry::{self, Aabb, Dimensions, Point, Rotation, ShapeType};
use crate::hull::{self, HullShape};
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
    compression_sensitive: bool,
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
            compression_sensitive: false,
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
    // A hull is not the same solid under two rotations that happen to give the same box, so
    // `unique_rotations` -- which keys on the box -- would silently drop orientations that
    // differ. Cuboids keep the deduplication they have always had.
    let is_hull = item.item.shape_type == ShapeType::ConvexHull;
    let exact_hull = item
        .item
        .hull_collision_is_exact(request.config.clearance.0 == 0);
    let rotation_forms: Vec<(Rotation, Dimensions)> = if is_hull {
        item.item
            .allowed_rotations
            .iter()
            .map(|rotation| (*rotation, item.item.dimensions.rotated(*rotation)))
            .collect()
    } else {
        item.item
            .dimensions
            .unique_rotations(&item.item.allowed_rotations)
    };
    let rotations = rotation_forms
        .into_iter()
        .map(|(rotation, physical)| {
            let envelope = if request.config.clearance.0 > 0 {
                physical.expand(request.config.clearance)
            } else {
                physical
            };
            let shape = exact_hull
                .then_some(item.item.hull_vertices.as_ref())
                .flatten()
                .and_then(|vertices| hull::shape_for(vertices, hull::source_axes(rotation)));
            (rotation, physical, envelope, shape)
        })
        .collect::<Vec<_>>();
    // Built once for the whole sweep: the placed scene does not move, and a hull costs
    // `O(v^4)` to build, so rebuilding one per candidate would dominate the scan.
    let placed_hulls: Vec<Option<Rc<HullShape>>> = state
        .packed
        .placements
        .iter()
        .map(Placement::hull_shape)
        .collect();
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
    // The placed boxes do not move for the whole of this item's candidate sweep,
    // so their contact graph is built once here and every candidate is appended to it
    // instead of each rebuilding the graph from nothing.
    //
    // Two exclusions. The first mirrors `candidate_respects_loads`' own short circuit --
    // no load rule is active, so no graph is ever asked for and building one would be
    // pure cost. The second is nesting: a nesting predecessor *replaces* the face edges
    // of everything in its column, so one new placement can rewrite edges arbitrarily far
    // from itself and the delta is no longer local. Nesting keeps the from-scratch path.
    let load_rules_inactive = !state.stack_sensitive
        && !placement_stack_sensitive(&item.item)
        && state.packed.container.max_stack_density.is_none();
    let nesting_present = item.item.nesting_height.is_some()
        || state
            .packed
            .placements
            .iter()
            .any(|placement| placement.instance.item.nesting_height.is_some());
    let load_sweep = (!load_rules_inactive && !nesting_present).then(|| {
        // The cell must cover every box hashed into the broad phase or queried against
        // it, and the candidate is a new item that may be wider than anything placed --
        // so the hint comes from this item's own rotations, which are known here.
        let widest = rotations
            .iter()
            .map(|(_, _, envelope, _)| envelope.length.0.max(envelope.width.0))
            .max()
            .unwrap_or(1);
        LoadSweep::new(state, widest)
    });
    // The placed scene is immutable for this whole candidate sweep. Building its open
    // corridors once changes the hot-path accessibility check from O(m² * |D|) for every
    // candidate to O(m * |D|), while preserving the exact same blocker predicate.
    // The container's own doors win; the configured list is what a container that states
    // none inherits. `access_directions` became a request field at the 1.1.0
    // freeze and had to be a per-container one -- two doors on one trailer and none on
    // another is the case that makes the rule worth having -- while `PackingConfig` stays
    // as the default for the library callers who set the doors in code.
    let doors: &[String] = if state.packed.container.access_directions.is_empty() {
        &request.config.access_directions
    } else {
        &state.packed.container.access_directions
    };
    let stop_accessibility_base = ((state.route_sensitive || item.item.stop_index.is_some())
        && !doors.is_empty())
    .then(|| {
        StopAccessibilityBase::new(
            item.item.stop_index,
            &state.packed.placements,
            state.packed.container.inner_dimensions,
            doors,
        )
    });
    let mut candidates = CandidateAccumulator::new(limit);
    for (point_index, point) in points.into_iter().enumerate() {
        if point_index > 0 && (deadline.expired() || effort_exhausted(request, metrics)) {
            break;
        }
        metrics.candidate_points_considered = metrics.candidate_points_considered.saturating_add(1);
        for (rotation, physical, envelope, shape) in &rotations {
            let (rotation, physical, envelope) = (*rotation, *physical, *envelope);
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
                        if !envelope_box.intersects(existing.envelope_box()) {
                            return false;
                        }
                        if tentative
                            .as_ref()
                            .is_some_and(|placement| valid_nesting(existing, placement))
                        {
                            return false;
                        }
                        let blocker = placed_hulls[index].as_deref();
                        // The axis-aligned test is the broad phase and stays mandatory. Only
                        // when a hull is one of the two solids does the exact test get to
                        // overrule it, so a request of ordinary boxes never reaches here.
                        if shape.is_none() && blocker.is_none() {
                            return true;
                        }
                        solids_collide(
                            shape.as_deref(),
                            envelope_box,
                            blocker,
                            existing.envelope_box(),
                        )
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
                        && (shape.is_none()
                            || solids_collide(shape.as_deref(), envelope_box, None, box_))
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
            let (support_area, support_ratio) = match nested_support.as_ref() {
                Some(support) => (support.area, support.ratio),
                None => {
                    let area = supporting_area(state, envelope_box);
                    (area, support_ratio_of(area, envelope_box))
                }
            };
            let required_support = item
                .item
                .minimum_support_ratio
                .max(request.config.minimum_support_ratio);
            if !support_area_sufficient(
                point.z,
                support_area,
                envelope.base_area(),
                required_support,
            ) {
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
            if stop_accessibility_base.as_ref().is_some_and(|base| {
                !base.allows(
                    envelope_box,
                    &state.packed.placements,
                    state.packed.container.inner_dimensions,
                    doors,
                )
            }) {
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
            if !candidate_respects_loads(
                state,
                item,
                &candidate,
                nested_support.as_ref(),
                load_sweep.as_ref(),
            ) {
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
    let compression_sensitive =
        state.compression_sensitive || item.item.shape_type == ShapeType::Compressible;
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
    if !compression_sensitive {
        state.used_volume = state
            .used_volume
            .saturating_add(used_volume_delta(&state.packed.placements, &placement));
    }
    state.stack_sensitive |= placement_stack_sensitive(&placement.instance.item);
    state.route_sensitive |= placement.instance.item.stop_index.is_some();
    state.compression_sensitive = compression_sensitive;
    state.packed.placements.push(placement);
    let envelope_box = state.packed.placements[placement_index].envelope_box();
    // Retiring a point because it falls inside a solid's box assumes the box *is* the solid.
    // For a hull it is not: a placement origin is a corner of a bounding box, and a hull
    // leaves most of that box -- including, for a wedge, the origin itself -- available to the
    // next item. Pruning them first would mean the engine could describe an interlocking pack
    // it could never propose, and the exact collision test would be correct and never
    // consulted.
    if state.packed.placements[placement_index]
        .hull_shape()
        .is_none()
    {
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
    }
    for point in exposed_points(state, envelope_box) {
        absorb_point(state, point);
    }
    if let Some(loads) = calculate_top_loads(&state.packed.placements) {
        for (placement, load) in state.packed.placements.iter_mut().zip(loads) {
            placement.top_load = Weight(load.clamp(0, i64::MAX as i128) as i64);
        }
    }
    if compression_sensitive {
        state.used_volume = state.packed.used_volume();
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
        // A hull leaves most of its bounding box free, including -- for a wedge -- the origin
        // itself, so treating that box as solid would drop exactly the points an interlocking
        // pack needs. The same rule the retirement path applies.
        .filter(|placement| placement.hull_shape().is_none())
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

/// Square ticks of placed top faces directly under `box_`'s base plane.
fn supporting_area(state: &ContainerState, box_: Aabb) -> i128 {
    state
        .packed
        .placements
        .iter()
        .filter(|placement| placement.envelope_box().z2() == box_.origin.z)
        .map(|placement| placement.envelope_box().overlap_area_xy(box_))
        .sum::<i128>()
}

/// The reported fraction, for the placement record only: the feasibility decision reads
/// the integer area through `support_area_sufficient`.
fn support_ratio_of(area: i128, box_: Aabb) -> f64 {
    if box_.origin.z == 0 {
        return 1.0;
    }
    (area as f64 / box_.dimensions.base_area() as f64).min(1.0)
}

struct CandidateSupportView {
    area: i128,
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
        area: graph.support_area(candidate_index),
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
    sweep: Option<&LoadSweep>,
) -> bool {
    if !state.stack_sensitive
        && !placement_stack_sensitive(&item.item)
        && state.packed.container.max_stack_density.is_none()
    {
        return true;
    }
    let max_density = state.packed.container.max_stack_density;
    if let Some(support_view) = support_view {
        return load_rules_hold(&support_view.placements, &support_view.graph, max_density);
    }
    let candidate_box = Aabb {
        origin: candidate.envelope_origin,
        dimensions: candidate.envelope_dimensions,
    };
    // With no nesting anywhere in this container, the placement graph *is* the face
    // graph, so the sweep prepared for this item answers from the placed scene's settled
    // loads. With nesting present, the column-aware build stays authoritative, and it
    // needs the placements themselves -- the two are required to agree exactly, which is
    // what `contact_graph`'s append property test holds them to.
    if let Some(sweep) = sweep {
        return sweep.allows(LoadBody::of(&item.item, candidate_box));
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
    load_rules_hold(&placements, &graph, max_density)
}

/// Every bearing rule at once, the way `apply_candidate`'s predecessor checked them.
fn load_rules_hold<T: LoadSubject>(
    bodies: &[T],
    graph: &ContactGraph,
    max_density: Option<Weight>,
) -> bool {
    let Some(loads) = calculate_top_loads_with_graph(bodies, graph) else {
        return false;
    };
    stack_limits_valid(bodies, graph)
        && crush_free(bodies, &loads)
        && stack_density_valid_with_loads(bodies, max_density, &loads)
}

/// The item facts the bearing rules read about one box, and nothing else. `Placement`
/// answers from its item; `LoadBody` is the same six facts copied out once per sweep, so
/// a candidate check never clones an `Item` -- its id, tags and metadata -- to read them.
pub(crate) trait LoadSubject {
    fn envelope(&self) -> Aabb;
    fn weight_ticks(&self) -> i64;
    fn stackable(&self) -> bool;
    fn max_top_load_ticks(&self) -> Option<i64>;
    fn max_stacked_items(&self) -> Option<usize>;
    fn max_compression_pressure_kpa(&self) -> Option<i64>;
}

impl LoadSubject for Placement {
    fn envelope(&self) -> Aabb {
        self.envelope_box()
    }
    fn weight_ticks(&self) -> i64 {
        self.instance.item.weight.0
    }
    fn stackable(&self) -> bool {
        self.instance.item.stackable
    }
    fn max_top_load_ticks(&self) -> Option<i64> {
        self.instance.item.max_top_load.map(|weight| weight.0)
    }
    fn max_stacked_items(&self) -> Option<usize> {
        self.instance.item.max_stacked_items
    }
    fn max_compression_pressure_kpa(&self) -> Option<i64> {
        self.instance.item.max_compression_pressure_kpa
    }
}

#[derive(Clone, Copy, Debug)]
struct LoadBody {
    envelope: Aabb,
    weight: i64,
    stackable: bool,
    max_top_load: Option<i64>,
    max_stacked_items: Option<usize>,
    max_compression_pressure_kpa: Option<i64>,
}

impl LoadBody {
    fn of(item: &Item, envelope: Aabb) -> Self {
        Self {
            envelope,
            weight: item.weight.0,
            stackable: item.stackable,
            max_top_load: item.max_top_load.map(|weight| weight.0),
            max_stacked_items: item.max_stacked_items,
            max_compression_pressure_kpa: item.max_compression_pressure_kpa,
        }
    }
}

impl LoadSubject for LoadBody {
    fn envelope(&self) -> Aabb {
        self.envelope
    }
    fn weight_ticks(&self) -> i64 {
        self.weight
    }
    fn stackable(&self) -> bool {
        self.stackable
    }
    fn max_top_load_ticks(&self) -> Option<i64> {
        self.max_top_load
    }
    fn max_stacked_items(&self) -> Option<usize> {
        self.max_stacked_items
    }
    fn max_compression_pressure_kpa(&self) -> Option<i64> {
        self.max_compression_pressure_kpa
    }
}

/// The placed scene's bearing state, settled once per candidate sweep.
///
/// built the contact graph once per sweep and appended each candidate to a
/// clone of it; the loads were still recomputed from nothing, over a cloned
/// `Vec<Placement>`, for every candidate. Here the loads are settled once too, and a
/// candidate is judged by recomputing only the boxes whose load it can change: everything
/// reachable downward from it, and from whatever already rests on its top face. The
/// recomputation applies the exact distribution rule `calculate_top_loads_with_graph`
/// applies -- same supporter order, same floor-and-remainder split -- so the verdict is
/// the one a from-scratch rebuild gives; the differential test below holds it to that.
/// Cost per candidate is `O(|affected| * degree)` instead of `O(n log n + e)` plus a
/// deep clone of the scene.
struct LoadSweep {
    graph: ContactGraph,
    bodies: Vec<LoadBody>,
    /// `None` when the placed scene breaks a bearing edge on its own.
    base_loads: Option<Vec<i128>>,
    /// Whether the placed scene passes every bearing rule by itself. When it does not,
    /// only a rebuild can say whether a candidate repairs it: one placed under an
    /// overhang joins that box's supporters and shrinks the shares its other supporters
    /// carry, and the remainder split is not monotone in the load above it either.
    base_holds: bool,
    max_density: Option<Weight>,
}

impl LoadSweep {
    fn new(state: &ContainerState, widest_footprint: i64) -> Self {
        let bodies = state
            .packed
            .placements
            .iter()
            .map(|placement| LoadBody::of(&placement.instance.item, placement.envelope_box()))
            .collect::<Vec<_>>();
        let boxes = bodies.iter().map(|body| body.envelope).collect::<Vec<_>>();
        let graph = ContactGraph::with_cell_hint(&boxes, widest_footprint);
        let max_density = state.packed.container.max_stack_density;
        let base_loads = calculate_top_loads_with_graph(&bodies, &graph);
        let base_holds = base_loads.as_ref().is_some_and(|loads| {
            stack_limits_valid(&bodies, &graph)
                && crush_free(&bodies, loads)
                && stack_density_valid_with_loads(&bodies, max_density, loads)
        });
        Self {
            graph,
            bodies,
            base_loads,
            base_holds,
            max_density,
        }
    }

    fn allows(&self, candidate: LoadBody) -> bool {
        let footprint = candidate
            .envelope
            .dimensions
            .length
            .0
            .max(candidate.envelope.dimensions.width.0);
        if footprint > self.graph.cell() || !self.base_holds {
            return self.rebuilt_with(candidate);
        }
        let Some(base_loads) = self.base_loads.as_ref() else {
            return self.rebuilt_with(candidate);
        };
        let below = self.graph.contacts_below(candidate.envelope);
        let above = self.graph.contacts_above(candidate.envelope);
        // The only new edges: a non-stackable supporter refuses the candidate, and a
        // non-stackable candidate refuses whatever already rests on its top face.
        if below.iter().any(|edge| !self.bodies[edge.index].stackable) {
            return false;
        }
        if !above.is_empty() && !candidate.stackable {
            return false;
        }
        let scene = CandidateScene {
            sweep: self,
            candidate,
            below: &below,
            above: &above,
        };
        let candidate_index = self.bodies.len();
        let mut affected = BTreeSet::new();
        let mut pending = above.iter().map(|edge| edge.index).collect::<Vec<_>>();
        pending.push(candidate_index);
        while let Some(index) = pending.pop() {
            if affected.insert(index) {
                pending.extend(scene.supporters(index).iter().map(|edge| edge.index));
            }
        }
        // The same order `calculate_top_loads_with_graph` settles loads in: top faces
        // descending, then bases descending, then index -- `affected` is ascending and
        // the sort is stable, so ties keep index order.
        let mut order = affected.iter().copied().collect::<Vec<_>>();
        order.sort_by_key(|index| {
            let envelope = scene.body(*index).envelope;
            Reverse((envelope.z2(), envelope.origin.z))
        });
        let mut loads = BTreeMap::new();
        for index in order {
            let mut children = scene.children(index);
            children.sort_by_key(|child| {
                let envelope = scene.body(*child).envelope;
                (Reverse((envelope.z2(), envelope.origin.z)), *child)
            });
            let mut load = 0_i128;
            for child in children {
                let downward = i128::from(scene.body(child).weight).saturating_add(
                    loads
                        .get(&child)
                        .copied()
                        .unwrap_or_else(|| base_loads[child]),
                );
                load = load.saturating_add(scene.share(child, index, downward));
            }
            if let Some(maximum) = scene.body(index).max_top_load
                && load > i128::from(maximum)
            {
                return false;
            }
            loads.insert(index, load);
        }
        for (index, load) in &loads {
            let body = scene.body(*index);
            let footprint_area = body.envelope.dimensions.base_area();
            if let Some(limit) = body.max_compression_pressure_kpa
                && crush_exceeded(limit, *load, footprint_area)
            {
                return false;
            }
            if let Some(maximum) = self.max_density
                && !density_within(body.weight, *load, maximum, footprint_area)
            {
                return false;
            }
        }
        scene.stack_counts_hold(&affected)
    }

    /// The from-scratch verdict, for the cases the delta cannot answer.
    fn rebuilt_with(&self, candidate: LoadBody) -> bool {
        let mut bodies = self.bodies.clone();
        bodies.push(candidate);
        let boxes = bodies.iter().map(|body| body.envelope).collect::<Vec<_>>();
        let footprint = candidate
            .envelope
            .dimensions
            .length
            .0
            .max(candidate.envelope.dimensions.width.0);
        let graph = ContactGraph::with_cell_hint(&boxes, footprint.max(self.graph.cell()));
        load_rules_hold(&bodies, &graph, self.max_density)
    }
}

/// The placed scene plus one candidate, read as the graph `with_box` would have built:
/// the candidate takes the next index, rests on `below`, and joins the supporter list of
/// every box in `above` at the end.
struct CandidateScene<'a> {
    sweep: &'a LoadSweep,
    candidate: LoadBody,
    below: &'a [ContactEdge],
    above: &'a [ContactEdge],
}

impl CandidateScene<'_> {
    fn candidate_index(&self) -> usize {
        self.sweep.bodies.len()
    }

    fn body(&self, index: usize) -> LoadBody {
        if index == self.candidate_index() {
            self.candidate
        } else {
            self.sweep.bodies[index]
        }
    }

    fn supporters(&self, index: usize) -> Vec<ContactEdge> {
        if index == self.candidate_index() {
            return self.below.to_vec();
        }
        let mut supporters = self.sweep.graph.supporters(index).to_vec();
        if let Some(edge) = self.above.iter().find(|edge| edge.index == index) {
            supporters.push(ContactEdge {
                index: self.candidate_index(),
                area: edge.area,
            });
        }
        supporters
    }

    fn children(&self, index: usize) -> Vec<usize> {
        if index == self.candidate_index() {
            return self.above.iter().map(|edge| edge.index).collect();
        }
        let mut children = self.sweep.graph.children(index).to_vec();
        if self.below.iter().any(|edge| edge.index == index) {
            children.push(self.candidate_index());
        }
        children
    }

    /// `upper`'s share of `downward` onto `lower`: proportional by contact area, with
    /// the last supporter taking the rounding remainder, exactly as the settled loads.
    fn share(&self, upper: usize, lower: usize, downward: i128) -> i128 {
        let supporters = self.supporters(upper);
        let total_area = supporters.iter().map(|edge| edge.area).sum::<i128>();
        let mut distributed = 0_i128;
        for (position, edge) in supporters.iter().enumerate() {
            let share = if position + 1 == supporters.len() {
                downward.saturating_sub(distributed)
            } else {
                downward.saturating_mul(edge.area) / total_area
            };
            if edge.index == lower {
                return share;
            }
            distributed = distributed.saturating_add(share);
        }
        0
    }

    /// `max_stacked_items` over the boxes whose stack the candidate joins: those below
    /// it gain the candidate and everything already resting on it; nothing else changes.
    fn stack_counts_hold(&self, affected: &BTreeSet<usize>) -> bool {
        let candidate_index = self.candidate_index();
        let mut carried_by_candidate = BTreeSet::new();
        let mut pending = self.above.iter().map(|edge| edge.index).collect::<Vec<_>>();
        while let Some(index) = pending.pop() {
            if carried_by_candidate.insert(index) {
                pending.extend(self.sweep.graph.children(index).iter().copied());
            }
        }
        if let Some(maximum) = self.candidate.max_stacked_items
            && carried_by_candidate.len() > maximum
        {
            return false;
        }
        let mut under_candidate = BTreeSet::new();
        pending = self.below.iter().map(|edge| edge.index).collect();
        while let Some(index) = pending.pop() {
            if under_candidate.insert(index) {
                pending.extend(
                    self.sweep
                        .graph
                        .supporters(index)
                        .iter()
                        .map(|edge| edge.index),
                );
            }
        }
        debug_assert!(under_candidate.iter().all(|index| affected.contains(index)));
        for root in under_candidate {
            let Some(maximum) = self.sweep.bodies[root].max_stacked_items else {
                continue;
            };
            let mut stacked = carried_by_candidate.clone();
            stacked.insert(candidate_index);
            pending = self.sweep.graph.children(root).to_vec();
            while let Some(index) = pending.pop() {
                if stacked.insert(index) {
                    pending.extend(self.sweep.graph.children(index).iter().copied());
                }
            }
            if stacked.len() > maximum {
                return false;
            }
        }
        true
    }
}

fn density_within(weight: i64, load: i128, maximum: Weight, footprint_area: i128) -> bool {
    const SQUARE_METRE_TICKS: i128 = 16_000_000_i128 * 16_000_000_i128;
    let total = i128::from(weight).saturating_add(load);
    total.saturating_mul(SQUARE_METRE_TICKS) <= i128::from(maximum.0).saturating_mul(footprint_area)
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

/// Corridors that are open in one immutable placement state.
///
/// Construction is `O(m² * |D|)` time and `O(m * |D|)` space. Every candidate then costs
/// `O(m * |D|)` rather than rebuilding the same placed-vs-placed intersections.
struct StopAccessibilityBase {
    candidate_stop: usize,
    stops: Vec<usize>,
    clear_sweeps: Vec<Vec<geometry::SweptRegion>>,
    inert: bool,
}

impl StopAccessibilityBase {
    fn new(
        candidate_stop: Option<usize>,
        placements: &[Placement],
        container: Dimensions,
        directions: &[String],
    ) -> Self {
        let candidate_stop = candidate_stop.unwrap_or(RIDES_THE_WHOLE_ROUTE);
        let stops = placements
            .iter()
            .map(|placement| {
                placement
                    .instance
                    .item
                    .stop_index
                    .unwrap_or(RIDES_THE_WHOLE_ROUTE)
            })
            .collect::<Vec<_>>();
        let inert = directions.is_empty() || stops.iter().all(|stop| *stop == candidate_stop);
        if inert {
            return Self {
                candidate_stop,
                stops,
                clear_sweeps: Vec::new(),
                inert,
            };
        }

        let boxes = placements
            .iter()
            .map(Placement::envelope_box)
            .collect::<Vec<_>>();
        let clear_sweeps = boxes
            .iter()
            .enumerate()
            .map(|(index, box_)| {
                if stops[index] == RIDES_THE_WHOLE_ROUTE {
                    return Vec::new();
                }
                directions
                    .iter()
                    .filter_map(|direction| geometry::swept_volume(*box_, container, direction))
                    .filter(|sweep| {
                        !boxes.iter().enumerate().any(|(other, other_box)| {
                            other != index
                                && stops[other] > stops[index]
                                && geometry::sweep_intersects(*sweep, *other_box)
                        })
                    })
                    .collect()
            })
            .collect();
        Self {
            candidate_stop,
            stops,
            clear_sweeps,
            inert,
        }
    }

    fn allows(
        &self,
        candidate: Aabb,
        placements: &[Placement],
        container: Dimensions,
        directions: &[String],
    ) -> bool {
        if self.inert {
            return true;
        }

        for (index, sweeps) in self.clear_sweeps.iter().enumerate() {
            if self.candidate_stop <= self.stops[index] {
                continue;
            }
            if !sweeps
                .iter()
                .any(|sweep| !geometry::sweep_intersects(*sweep, candidate))
            {
                return false;
            }
        }

        if self.candidate_stop == RIDES_THE_WHOLE_ROUTE {
            return true;
        }
        directions.iter().any(|direction| {
            geometry::swept_volume(candidate, container, direction).is_some_and(|sweep| {
                !placements.iter().zip(&self.stops).any(|(placement, stop)| {
                    *stop > self.candidate_stop
                        && geometry::sweep_intersects(sweep, placement.envelope_box())
                })
            })
        })
    }
}

/// The horizontal half of route order: nothing due later may stand between an earlier item
/// and a door.
///
/// `route_contact_allowed` above enforces the vertical half -- nothing due later may rest
/// *above* something due earlier. Both are necessary and neither implies the other;
/// docs/STOP-ACCESSIBILITY.md derives the rule and the post-validator's whole-scene replay
/// remains the sufficient check.
///
/// Inert unless the caller supplied exit directions, because the request schema has no
/// field for them: assuming all six walls open would enforce a rule true of no real
/// vehicle and nearly vacuous besides, since a box is almost always free through *some*
/// face.
///
/// The blocker set is `{q : s(q) > s(p)}` -- strictly later. Items due at the *same* stop
/// are excluded because the order within a stop is free: whichever is in the way comes off
/// first. Using `>=` would refuse two same-stop pallets standing one behind the other.
#[cfg(test)]
fn stop_accessible(
    candidate_stop: Option<usize>,
    candidate: Aabb,
    placements: &[Placement],
    container: Dimensions,
    directions: &[String],
) -> bool {
    if directions.is_empty() {
        return true;
    }
    let candidate_stop = candidate_stop.unwrap_or(RIDES_THE_WHOLE_ROUTE);
    let stop_of = |placement: &Placement| {
        placement
            .instance
            .item
            .stop_index
            .unwrap_or(RIDES_THE_WHOLE_ROUTE)
    };
    if placements
        .iter()
        .all(|placement| stop_of(placement) == candidate_stop)
    {
        return true;
    }
    for (index, placement) in placements.iter().enumerate() {
        let stop = stop_of(placement);
        if candidate_stop <= stop {
            continue;
        }
        let box_ = placement.envelope_box();
        let open = directions.iter().any(|direction| {
            geometry::swept_volume(box_, container, direction).is_some_and(|sweep| {
                !geometry::sweep_intersects(sweep, candidate)
                    && !placements.iter().enumerate().any(|(other, blocker)| {
                        other != index
                            && stop_of(blocker) > stop
                            && geometry::sweep_intersects(sweep, blocker.envelope_box())
                    })
            })
        });
        if !open {
            return false;
        }
    }
    if candidate_stop == RIDES_THE_WHOLE_ROUTE {
        return true;
    }
    directions.iter().any(|direction| {
        geometry::swept_volume(candidate, container, direction).is_some_and(|sweep| {
            !placements.iter().any(|blocker| {
                stop_of(blocker) > candidate_stop
                    && geometry::sweep_intersects(sweep, blocker.envelope_box())
            })
        })
    })
}

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
    let projected = if state.compression_sensitive
        || item.item.shape_type == ShapeType::Compressible
    {
        // A zero-load candidate is at its largest, while appending it can only shrink
        // existing compressible supports. When this upper bound fits, an exact graph
        // refresh cannot turn it into a reserve violation. Only candidates close to the
        // boundary pay the non-local calculation.
        let upper_bound = state
            .used_volume
            .saturating_add(occupied_volume(&placement));
        if upper_bound.saturating_add(reserve) <= state.packed.container.inner_dimensions.volume() {
            upper_bound
        } else {
            used_volume_with_current_loads(&state.packed, placement)
        }
    } else {
        state
            .used_volume
            .saturating_add(used_volume_delta(&state.packed.placements, &placement))
    };
    projected.saturating_add(reserve) > state.packed.container.inner_dimensions.volume()
}

/// Physical volume after an appended placement changes the support loads in the scene.
///
/// Compression makes the delta non-local: an upper item can shrink existing supports.
/// The ordinary rigid path remains O(1). One composite refresh is
/// O(n log n + q + e) time and O(n + e) graph space, where q is broad-phase work and e
/// is the contact-edge count; both are O(n^2) in a physically dense worst case. The
/// whole-solve sum over candidates and search nodes is documented separately.
fn used_volume_with_current_loads(packed: &PackedContainer, placement: Placement) -> i128 {
    let mut projected = packed.clone();
    projected.placements.push(placement);
    if let Some(loads) = calculate_top_loads(&projected.placements) {
        for (placement, load) in projected.placements.iter_mut().zip(loads) {
            placement.top_load = Weight(load.clamp(0, i64::MAX as i128) as i64);
        }
    }
    projected.used_volume()
}

fn placement_stack_sensitive(item: &Item) -> bool {
    item.is_stack_sensitive()
}

/// Physical-volume change from appending one placement.
///
/// Existing overlap pairs cannot change. The common non-nesting case is O(1);
/// nesting-aware candidates scan the already placed items once, O(n) time and O(1)
/// space. This replaces cloning the whole container and recomputing O(n^2) nesting
/// volume for every candidate orientation.
fn used_volume_delta(placements: &[Placement], placement: &Placement) -> i128 {
    let mut delta = occupied_volume(placement);
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

fn stack_limits_valid<T: LoadSubject>(bodies: &[T], graph: &ContactGraph) -> bool {
    for (root, body) in bodies.iter().enumerate() {
        let Some(maximum) = body.max_stacked_items() else {
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

fn stack_density_valid_with_loads<T: LoadSubject>(
    bodies: &[T],
    maximum: Option<Weight>,
    loads: &[i128],
) -> bool {
    let Some(maximum) = maximum else {
        return true;
    };
    bodies.iter().zip(loads).all(|(body, load)| {
        density_within(
            body.weight_ticks(),
            *load,
            maximum,
            body.envelope().dimensions.base_area(),
        )
    })
}

/// `crushed` without the offender's name, over any bearing subject.
fn crush_free<T: LoadSubject>(bodies: &[T], loads: &[i128]) -> bool {
    bodies.iter().zip(loads).all(|(body, load)| {
        body.max_compression_pressure_kpa().is_none_or(|limit| {
            !crush_exceeded(limit, *load, body.envelope().dimensions.base_area())
        })
    })
}

pub fn calculate_top_loads(placements: &[Placement]) -> Option<Vec<i128>> {
    let graph = ContactGraph::from_placements(placements);
    calculate_top_loads_with_graph(placements, &graph)
}

fn calculate_top_loads_with_graph<T: LoadSubject>(
    bodies: &[T],
    graph: &ContactGraph,
) -> Option<Vec<i128>> {
    let mut loads = vec![0_i128; bodies.len()];
    let mut order = (0..bodies.len()).collect::<Vec<_>>();
    order.sort_by_key(|index| {
        let envelope = bodies[*index].envelope();
        Reverse((envelope.z2(), envelope.origin.z))
    });

    for upper_index in order {
        let upper = bodies[upper_index].envelope();
        if upper.origin.z == 0 {
            continue;
        }
        let supports = graph.supporters(upper_index);
        if supports.is_empty() {
            continue;
        }
        let total_area = supports.iter().map(|edge| edge.area).sum::<i128>();
        let downward = i128::from(bodies[upper_index].weight_ticks()) + loads[upper_index];
        let mut distributed = 0_i128;
        for (position, edge) in supports.iter().enumerate() {
            let share = if position + 1 == supports.len() {
                downward.saturating_sub(distributed)
            } else {
                downward.saturating_mul(edge.area) / total_area
            };
            distributed = distributed.saturating_add(share);
            let support = &bodies[edge.index];
            // `stackable: false` is geometry, not load: nothing may rest on the item at
            // all. Gating it on `share > 0` meant a weightless box resting on a
            // non-stackable one transferred nothing and so was waved through, and since
            // nothing here self-validates the answer came back `feasible` while the
            // shared validator called it `non_stackable`.
            if !support.stackable() {
                return None;
            }
            loads[edge.index] = loads[edge.index].saturating_add(share);
            if let Some(maximum) = support.max_top_load_ticks()
                && loads[edge.index] > i128::from(maximum)
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
    let mut groups = group_member_indices(current);

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
            groups
                .remove(group)
                .expect("first unprocessed group member")
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

fn group_member_indices(items: &[ItemInstance]) -> BTreeMap<&str, Vec<usize>> {
    let mut groups: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    for (index, item) in items.iter().enumerate() {
        if let Some(group) = item.item.group.as_deref() {
            groups.entry(group).or_default().push(index);
        }
    }
    groups
}

fn item_batches(items: &[ItemInstance]) -> Vec<Vec<ItemInstance>> {
    let mut batches: Vec<Vec<ItemInstance>> = Vec::new();
    let mut positions = BTreeMap::new();
    for item in items {
        if let Some(group) = item.item.group.as_deref() {
            let position = *positions.entry(group).or_insert_with(|| {
                batches.push(Vec::new());
                batches.len() - 1
            });
            batches[position].push(item.clone());
        } else {
            batches.push(vec![item.clone()]);
        }
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
    //: Which batch the loop stopped before, when it stopped early. The surviving beam
    //: nodes have consumed batches `0..pending_from` and nothing after, so completing one
    //: of them means adding `batches[pending_from..]` to its unplaced list -- see the
    //: final selection below for why leaving that out lost items outright.
    let mut incumbent_key = container_beam_key(&incumbent, &[]);
    let mut pending_from: Option<usize> = None;

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
            let complete_key = container_beam_key(&complete, &[]);
            if complete_key < incumbent_key {
                incumbent = complete;
                incumbent_key = complete_key;
            }
        }
        if expansions.is_empty() || exhausted {
            pending_from = Some(position);
            break;
        }
        expansions.sort_by_cached_key(|node| container_beam_key(node, &future));
        expansions.truncate(request.config.container_plan_beam_width);
        beam = expansions;
    }
    // Every node still in the beam has to account for the batches the loop never reached
    // before it can be compared with the incumbent, or chosen over it.
    //
    // On a loop that ran to the end this changes nothing: the beam then holds expansions
    // of the final batch, each of which was already compared above with an empty future,
    // so `pending` is empty and the block is the no-op it always was. On a loop that
    // stopped early -- deadline, `container_plan_node_limit`, or an exhausted effort
    // budget, all three reachable through public configuration -- the beam holds nodes
    // from *before* the current batch, and taking one without its tail returned a result
    // that neither placed those items nor reported them unpacked. They simply
    // disappeared: 112 requested, 43 placed, 0 unpacked, on a 3-type BR instance at
    // `container_plan_node_limit: 4`. The engine's own independent validator caught the
    // accounting hole and refused the whole request, so no wrong answer ever escaped --
    // but a legal request failed outright, which is why this is a fix and not a tidy-up.
    let pending: Vec<ItemInstance> = pending_from
        .map(|from| batches[from..].iter().flatten().cloned().collect())
        .unwrap_or_default();
    if let Some(completed) = beam
        .into_iter()
        .map(|mut node| {
            node.unplaced.extend(pending.iter().cloned());
            node
        })
        .min_by_key(|node| container_beam_key(node, &[]))
        && container_beam_key(&completed, &[]) < incumbent_key
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

/// The first container in a finished answer whose own rate table cannot price it, as
/// `(container id, billed grams, last bracket)`.
///
/// Ranking an unpriceable candidate worst is what makes a priceable alternative win the
/// round; it is a search device, not an answer. This is the guard that keeps the sentinel
/// from ever surfacing: a returned packing the tariff cannot price would quote a number
/// the carrier never published, which is the one outcome `charge_minor` refuses to
/// invent. Callers turn a hit into the same refusal Python and PHP raise.
pub(crate) fn unpriceable_container(
    containers: &[PackedContainer],
    config: &PackingConfig,
) -> Option<(String, i64, i64)> {
    if config.objective != "lowest_landed_cost" {
        return None;
    }
    containers.iter().find_map(|packed| {
        let dimensional = dimensional_weight_ticks(&packed.container, config);
        let billed = i128::from(packed.gross_weight().0).max(dimensional);
        let grams = RateTable::grams(billed as i64);
        // A container with no table at all is refused at admission, so `parse_request`
        // callers never reach this arm -- but `pack_request_with_policy` is public, and a
        // Rust consumer assembling a `PackingRequest` by hand bypasses that check. Report
        // it rather than skipping it: an untabled container is the one case where "cannot
        // price" is certain. Python and PHP report the same `0` bracket here.
        let Some(table) = packed.container.rate_table.as_ref() else {
            return Some((packed.container.id.clone(), grams, 0));
        };
        if table.charge_minor(grams).is_some() {
            return None;
        }
        let last = table.weight_brackets_g.last().copied().unwrap_or(0);
        Some((packed.container.id.clone(), grams, last))
    })
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
    // The greedy loop commits this trial verbatim -- nothing is added to the container
    // after it wins the round -- so its billed weight is final here and the tariff can be
    // read now. `charge_minor` is a bracket walk over a table with a handful of entries,
    // so pricing every trial costs nothing the round did not already spend packing it.
    let landed = if config.objective == "lowest_landed_cost" {
        container
            .rate_table
            .as_ref()
            .and_then(|table| table.charge_minor(RateTable::grams(billable as i64)))
            .map_or(i128::MAX, i128::from)
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
        "shipping_cost" => vec![billable, containers_needed, -placed, unused_ppm, height_ppm],
        // Landed cost ranks by the money `score_solution` will actually charge for this
        // container, not by the billed weight it is derived from. The two order
        // candidates identically only while price rises smoothly with weight; a bracket
        // step or a minimum charge makes the cheaper shipment the heavier one, and an
        // unpriceable trial is not merely expensive but unshippable, so it sorts behind
        // every priceable alternative rather than winning on being light.
        "lowest_landed_cost" => vec![landed, containers_needed, -placed, unused_ppm, height_ppm],
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
            shape_type: crate::geometry::ShapeType::RigidCuboid,
            hull_vertices: None,
            compression_ratio_ppm: None,
            max_compression_pressure_kpa: None,
        }
    }

    #[test]
    fn group_batches_preserve_first_appearance_and_member_order() {
        let groups = [
            Some("00"),
            None,
            Some("0"),
            Some("00"),
            Some("1"),
            Some("0"),
            None,
            Some("1"),
        ];
        let items = groups
            .iter()
            .enumerate()
            .map(|(index, group)| {
                let mut value = item(&index.to_string());
                value.group = group.map(str::to_owned);
                ItemInstance {
                    item: value,
                    sequence: 1,
                }
            })
            .collect::<Vec<_>>();
        let batches = item_batches(&items)
            .iter()
            .map(|batch| {
                batch
                    .iter()
                    .map(|instance| instance.item.id.clone())
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        assert_eq!(
            batches,
            vec![
                vec!["0", "3"],
                vec!["1"],
                vec!["2", "5"],
                vec!["4", "7"],
                vec!["6"]
            ]
        );
        let indices = group_member_indices(&items);
        assert_eq!(indices["00"], vec![0, 3]);
        assert_eq!(indices["0"], vec![2, 5]);
        assert_eq!(indices["1"], vec![4, 7]);
        assert_eq!(indices.len(), 3);
        assert!(item_batches(&[]).is_empty());
        assert!(group_member_indices(&[]).is_empty());
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
            access_directions: Vec::new(),
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

    /// Every instance the beam was handed comes back either placed or unplaced.
    ///
    /// The invariant is the whole contract of `try_pack_into_beam`'s return value, and it
    /// held only while the loop ran to the end. On an early stop -- a node limit here, a
    /// deadline or an exhausted effort budget in the field -- the surviving beam nodes
    /// still had every unvisited batch missing from their unplaced list, and choosing one
    /// of them dropped those items on the floor. It is asserted with a *node limit*
    /// rather than a short deadline on purpose: this is a counted, clock-free reproduction
    /// that cannot go quiet on a fast host, and `container_plan_node_limit` is public
    /// configuration, so the hole was reachable without any timing at all.
    /// The same accounting invariant, across every shape that reaches the early stop.
    ///
    /// The regression test below pins one scene, because one scene is what it took to
    /// reproduce the defect. That leaves the invariant asserted on a single arrangement of
    /// item size, beam width and node limit -- and the defect it was written for had
    /// survived three releases precisely because nobody had varied those.
    ///
    /// A node limit of zero and a beam width of one are the two configurations most likely
    /// to be wrong and least likely to be typed: zero stops the loop before any expansion
    /// exists, and one leaves the beam holding only its initial empty node.
    #[test]
    fn the_beam_accounts_for_every_instance_across_widths_and_limits() {
        for (extent, width, limit, count) in [
            (600_i64, 1_usize, 0_usize, 5_usize), // stops before the first expansion
            (600, 1, 1, 5),
            (600, 4, 0, 12),
            (600, 4, 3, 12),
            (600, 16, 100, 12), // never stops early at all
            (10, 4, 3, 12),     // everything fits; the incumbent is complete
            (600, 4, 3, 1),     // one instance, one batch
            (600, 4, 3, 2),
        ] {
            let mut item_type = item("beamed");
            item_type.dimensions = dimensions(extent, extent, extent);
            let instances = (1..=count as u32)
                .map(|sequence| ItemInstance {
                    item: item_type.clone(),
                    sequence: sequence as usize,
                })
                .collect::<Vec<_>>();
            let mut request = request(&item_type, &container(), None);
            request.config.container_plan_beam_width = width;
            request.config.container_plan_node_limit = limit;

            let mut metrics = SolverMetrics::default();
            let (state, unplaced) = try_pack_into_beam(
                &container(),
                1,
                &instances,
                &request,
                &[],
                &[],
                &Deadline::new(request.config.time_limit_ms),
                &mut metrics,
            );

            let mut seen = state
                .packed
                .placements
                .iter()
                .map(|placement| placement.instance.id())
                .collect::<BTreeSet<_>>();
            seen.extend(unplaced.iter().map(ItemInstance::id));
            assert_eq!(
                seen,
                instances
                    .iter()
                    .map(ItemInstance::id)
                    .collect::<BTreeSet<_>>(),
                "extent {extent}, width {width}, limit {limit}, {count} instances: an \
                 instance was lost, reported twice, or invented",
            );
            assert_eq!(
                state.packed.placements.len() + unplaced.len(),
                instances.len(),
                "extent {extent}, width {width}, limit {limit}, {count} instances",
            );
        }
    }

    #[test]
    fn the_container_beam_accounts_for_every_instance_when_it_stops_early() {
        // Deliberately too large to share the container: only one instance fits, so the
        // greedy incumbent carries eleven unplaced items and any beam node not yet offered
        // a batch it must skip looks perfect beside it. That is the arrangement in which a
        // truncated node wins the comparison -- in a scene where everything fits the
        // incumbent is unplaced-free too, the truncated node never wins, and the hole stays
        // hidden behind an assertion that passes for the wrong reason.
        let mut narrow = item("beamed");
        narrow.dimensions = dimensions(600, 600, 600);
        let instances = (1..=12)
            .map(|sequence| ItemInstance {
                item: narrow.clone(),
                sequence,
            })
            .collect::<Vec<_>>();
        let mut request = request(&narrow, &container(), None);
        request.config.container_plan_beam_width = 4;
        request.config.container_plan_node_limit = 3;

        let mut metrics = SolverMetrics::default();
        let (state, unplaced) = try_pack_into_beam(
            &container(),
            1,
            &instances,
            &request,
            &[],
            &[],
            &Deadline::new(request.config.time_limit_ms),
            &mut metrics,
        );

        assert_eq!(
            state.packed.placements.len() + unplaced.len(),
            instances.len(),
            "the beam stopped early and lost {} instance(s)",
            instances.len() - state.packed.placements.len() - unplaced.len(),
        );
        let mut seen = state
            .packed
            .placements
            .iter()
            .map(|placement| placement.instance.id())
            .collect::<BTreeSet<_>>();
        seen.extend(unplaced.iter().map(ItemInstance::id));
        assert_eq!(
            seen,
            instances
                .iter()
                .map(ItemInstance::id)
                .collect::<BTreeSet<_>>(),
            "an instance was reported twice or invented",
        );
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
            None,
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
            None,
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

    // ------------------------------------------- stop accessibility
    //
    // The worked examples in docs/STOP-ACCESSIBILITY.md live in
    // `conformance/scene/stop-accessibility-fixtures.json` and are read from there, so this
    // suite and the other three assert one table rather than four transcriptions of it.

    /// Ticks, read straight rather than scaled: all four engines assert the same integers
    /// instead of each applying its own conversion.
    fn fixture_dimensions(raw: &serde_json::Value) -> Dimensions {
        dimensions(
            raw["length"].as_i64().unwrap(),
            raw["width"].as_i64().unwrap(),
            raw["height"].as_i64().unwrap(),
        )
    }

    fn fixture_origin(raw: &serde_json::Value) -> Point {
        Point {
            x: raw["x"].as_i64().unwrap(),
            y: raw["y"].as_i64().unwrap(),
            z: raw["z"].as_i64().unwrap(),
        }
    }

    /// An absent stop is `null` in the corpus and `None` here, which both engines then read
    /// as the latest possible stop.
    fn fixture_stop(raw: &serde_json::Value) -> Option<usize> {
        raw["stop_index"].as_u64().map(|stop| stop as usize)
    }

    fn fixture_placement(raw: &serde_json::Value) -> Placement {
        let mut placed = item(raw["id"].as_str().unwrap());
        placed.dimensions = fixture_dimensions(&raw["dimensions"]);
        placed.stop_index = fixture_stop(raw);
        let origin = fixture_origin(&raw["origin"]);
        let dims = placed.dimensions;
        Placement {
            instance: ItemInstance {
                item: placed,
                sequence: 1,
            },
            position: origin,
            rotation: Rotation::Lwh,
            dimensions: dims,
            envelope_origin: origin,
            envelope_dimensions: dims,
            support_ratio: 1.0,
            top_load: Weight(0),
        }
    }

    /// Every scene in the shared corpus, with the verdict every engine must reach.
    ///
    /// `accessible` is asserted by all four engines and `route_order_allowed` here, in
    /// Python and in PHP. `code` is not: this engine answers with a verdict rather than a
    /// reason, and the corpus records which columns each engine can check.
    #[test]
    fn shared_four_language_stop_accessibility_scenes() {
        // A cross-language corpus kept one level above this crate; a published copy does
        // not carry it.
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../../../../conformance/scene/stop-accessibility-fixtures.json");
        let Ok(payload_text) = std::fs::read_to_string(&path) else {
            eprintln!(
                "skipping: the shared cross-language scene corpus is not part of this package"
            );
            return;
        };
        let payload: serde_json::Value = serde_json::from_str(&payload_text).unwrap();
        let scenes = payload["scenes"].as_array().unwrap();
        assert!(
            !scenes.is_empty(),
            "an empty corpus would pass this loop without asserting anything",
        );

        for scene in scenes {
            let id = scene["id"].as_str().unwrap();
            let placements: Vec<Placement> = scene["placements"]
                .as_array()
                .unwrap()
                .iter()
                .map(fixture_placement)
                .collect();
            let raw = &scene["candidate"];
            let candidate = Aabb {
                origin: fixture_origin(&raw["origin"]),
                dimensions: fixture_dimensions(&raw["dimensions"]),
            };
            let doors: Vec<String> = scene["directions"]
                .as_array()
                .unwrap()
                .iter()
                .map(|door| door.as_str().unwrap().to_owned())
                .collect();
            let container = fixture_dimensions(&scene["container"]);
            let base =
                StopAccessibilityBase::new(fixture_stop(raw), &placements, container, &doors);

            assert_eq!(
                base.allows(candidate, &placements, container, &doors),
                scene["accessible"].as_bool().unwrap(),
                "{id}",
            );
            assert_eq!(
                stop_accessible(fixture_stop(raw), candidate, &placements, container, &doors),
                scene["accessible"].as_bool().unwrap(),
                "from-scratch and candidate-sweep paths diverged for {id}",
            );
            if let Some(expected) = scene["route_order_allowed"].as_bool() {
                assert_eq!(
                    route_contact_allowed(fixture_stop(raw), candidate, &placements),
                    expected,
                    "{id}",
                );
            }
        }
    }

    /// A small deterministic generator: the property below wants many scenes, not a
    /// chosen few, and a fixed seed keeps every run identical.
    struct Lcg(u64);

    impl Lcg {
        fn next(&mut self, bound: u64) -> u64 {
            self.0 = self
                .0
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (self.0 >> 33) % bound
        }
    }

    fn load_body(lcg: &mut Lcg, origin: Point) -> LoadBody {
        let side = 10 + lcg.next(3) as i64 * 10;
        LoadBody {
            envelope: Aabb {
                origin,
                dimensions: dimensions(side, side, 10 + lcg.next(2) as i64 * 10),
            },
            weight: lcg.next(5) as i64 * 1_000,
            stackable: lcg.next(6) != 0,
            max_top_load: (lcg.next(3) == 0).then(|| lcg.next(4) as i64 * 1_500),
            max_stacked_items: (lcg.next(4) == 0).then(|| lcg.next(3) as usize),
            max_compression_pressure_kpa: (lcg.next(4) == 0).then(|| 1 + lcg.next(3) as i64),
        }
    }

    fn body_item(body: &LoadBody, sequence: usize) -> ItemInstance {
        let mut item = item("body");
        item.dimensions = body.envelope.dimensions;
        item.weight = Weight(body.weight);
        item.stackable = body.stackable;
        item.max_top_load = body.max_top_load.map(Weight);
        item.max_stacked_items = body.max_stacked_items;
        item.max_compression_pressure_kpa = body.max_compression_pressure_kpa;
        ItemInstance { item, sequence }
    }

    /// The from-scratch verdict `candidate_respects_loads` gave before the sweep existed:
    /// every placement plus the candidate, one graph, every rule over all of it.
    fn rebuilt_verdict(
        bodies: &[LoadBody],
        candidate: LoadBody,
        max_density: Option<Weight>,
    ) -> bool {
        let mut all = bodies.to_vec();
        all.push(candidate);
        let boxes = all.iter().map(|body| body.envelope).collect::<Vec<_>>();
        let widest = boxes
            .iter()
            .map(|box_| box_.dimensions.length.0.max(box_.dimensions.width.0))
            .max()
            .unwrap_or(1);
        load_rules_hold(
            &all,
            &ContactGraph::with_cell_hint(&boxes, widest),
            max_density,
        )
    }

    #[test]
    fn the_incremental_load_sweep_matches_the_from_scratch_verdict() {
        // Stacks are built on a coarse lattice so faces meet, overhang and float; candidates
        // land under existing boxes as often as on top of them, which is the case where a
        // supporter list changes and the settled shares move.
        let mut lcg = Lcg(20_260_902);
        let mut agreements = (0_usize, 0_usize);
        for scene in 0..400 {
            let mut container = container();
            container.max_stack_density =
                (scene % 3 == 0).then(|| Weight(lcg.next(4) as i64 * 5_000_000_000));
            let mut state = ContainerState::new(container.clone(), 1);
            let mut bodies = Vec::new();
            for sequence in 0..(1 + lcg.next(7) as usize) {
                let origin = Point {
                    x: lcg.next(4) as i64 * 10,
                    y: lcg.next(4) as i64 * 10,
                    z: lcg.next(4) as i64 * 10,
                };
                let body = load_body(&mut lcg, origin);
                if bodies
                    .iter()
                    .any(|placed: &LoadBody| placed.envelope.intersects(body.envelope))
                {
                    continue;
                }
                let candidate = Candidate {
                    envelope_origin: origin,
                    position: origin,
                    rotation: Rotation::Lwh,
                    dimensions: body.envelope.dimensions,
                    envelope_dimensions: body.envelope.dimensions,
                    support_ratio: 1.0,
                    score: 0,
                };
                apply_candidate(&mut state, body_item(&body, sequence), &candidate);
                bodies.push(body);
            }
            let sweep = LoadSweep::new(&state, 30);
            for _ in 0..12 {
                let origin = Point {
                    x: lcg.next(5) as i64 * 10,
                    y: lcg.next(5) as i64 * 10,
                    z: lcg.next(5) as i64 * 10,
                };
                let candidate = load_body(&mut lcg, origin);
                if bodies
                    .iter()
                    .any(|placed| placed.envelope.intersects(candidate.envelope))
                {
                    continue;
                }
                let expected = rebuilt_verdict(&bodies, candidate, container.max_stack_density);
                assert_eq!(
                    sweep.allows(candidate),
                    expected,
                    "scene {scene}: candidate {candidate:?} over {bodies:?}"
                );
                if expected {
                    agreements.0 += 1;
                } else {
                    agreements.1 += 1;
                }
            }
        }
        // The property is only worth something if both verdicts actually occur.
        assert!(agreements.0 > 100 && agreements.1 > 100, "{agreements:?}");
    }
}
