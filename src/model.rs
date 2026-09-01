use std::rc::Rc;

use crate::compression;
use crate::geometry::{Aabb, Dimensions, Point, Rotation, ShapeType};
use crate::hull::{self, HullShape, Vertex};
use crate::units::{Length, Weight};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SolverProfile {
    Fast,
    #[default]
    Balanced,
    Quality,
    ExactSmall,
}

impl SolverProfile {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Fast => "fast",
            Self::Balanced => "balanced",
            Self::Quality => "quality",
            Self::ExactSmall => "exact_small",
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct EffortBudget {
    pub max_candidates_evaluated: Option<u64>,
    pub max_placement_attempts: Option<u64>,
    pub max_search_nodes: Option<u64>,
    pub max_restarts: Option<usize>,
}

impl EffortBudget {
    pub fn exceeded(self, metrics: &SolverMetrics) -> bool {
        self.max_candidates_evaluated
            .map(|limit| metrics.feasible_candidates >= limit)
            .unwrap_or(false)
            || self
                .max_placement_attempts
                .map(|limit| metrics.orientations_considered >= limit)
                .unwrap_or(false)
            || self
                .max_search_nodes
                .map(|limit| metrics.search_nodes_expanded >= limit)
                .unwrap_or(false)
    }
}

#[derive(Clone, Debug)]
pub struct PackingConfig {
    pub profile: SolverProfile,
    pub time_limit_ms: u64,
    pub top_k: usize,
    pub seed: u64,
    pub max_containers: Option<usize>,
    pub clearance: Length,
    pub minimum_support_ratio: f64,
    pub exact_item_limit: usize,
    pub multi_start_orders: usize,
    pub max_candidates_per_item: usize,
    pub max_candidate_points: usize,
    pub parallel: bool,
    pub effort_budget: Option<EffortBudget>,
    /// Explicit built-in solver allow-list. Empty means the profile portfolio.
    pub solvers: Vec<String>,
    /// Name of the public lexicographic objective.
    pub objective: String,
    /// Carrier-published divisor and unit convention used by `shipping_cost`.
    pub dimensional_weight_divisor: Option<i128>,
    pub dimensional_weight_length_unit: String,
    pub dimensional_weight_weight_unit: String,
    /// `false` lets `GridSolver`'s regular-lattice fast path skip
    /// materializing one `Placement` per instance when a caller only needs to know
    /// the packing fits, not each item's own coordinates. Default `true` reproduces
    /// every existing result byte-for-byte.
    pub require_placement_coordinates: bool,
    /// Deterministic width of the beam over partial container plans.
    pub container_plan_beam_width: usize,
    /// Counted-work ceiling for partial container-plan nodes.
    pub container_plan_node_limit: usize,
    /// The container walls an item may be unloaded through, for the
    /// stop-accessibility rule. Empty (the default) disables the check entirely and
    /// reproduces every existing result byte-for-byte.
    ///
    /// Programmatic only, and deliberately so: the request schema has no
    /// access-directions field yet, and defaulting to all six walls would enforce a rule
    /// true of no real vehicle. A caller who wants the check states the doors in code, the
    /// same way the Python and PHP engines take them.
    pub access_directions: Vec<String>,
}

impl Default for PackingConfig {
    fn default() -> Self {
        Self {
            profile: SolverProfile::Balanced,
            time_limit_ms: 1_000,
            top_k: 3,
            seed: 42,
            max_containers: None,
            clearance: Length(0),
            minimum_support_ratio: 0.0,
            exact_item_limit: 7,
            multi_start_orders: 8,
            max_candidates_per_item: 1,
            max_candidate_points: 4_096,
            parallel: true,
            effort_budget: None,
            solvers: Vec::new(),
            objective: "default".into(),
            dimensional_weight_divisor: None,
            dimensional_weight_length_unit: "in".into(),
            dimensional_weight_weight_unit: "lb".into(),
            require_placement_coordinates: true,
            container_plan_beam_width: 1,
            container_plan_node_limit: 1,
            access_directions: Vec::new(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Item {
    pub id: String,
    pub dimensions: Dimensions,
    pub weight: Weight,
    pub quantity: usize,
    pub allowed_rotations: Vec<Rotation>,
    pub stackable: bool,
    pub must_be_on_floor: bool,
    pub max_top_load: Option<Weight>,
    pub minimum_support_ratio: f64,
    pub group: Option<String>,
    pub tags: BTreeSet<String>,
    pub incompatible_tags: BTreeSet<String>,
    pub priority: i32,
    pub metadata: BTreeMap<String, Value>,
    pub nesting_height: Option<Length>,
    pub max_stacked_items: Option<usize>,
    pub ground_contact_rule: Option<String>,
    pub stop_index: Option<usize>,
    pub eligible_container_tags: BTreeSet<String>,
    /// Exact, unit-less economic worth for the `maximum_value` objective,
    /// which ranks by the total value of unpacked items rather than treating every
    /// item as equally worth leaving behind. `None` never affects placement or
    /// scoring under any objective, `maximum_value` included.
    pub value: Option<usize>,
    /// How much of `dimensions` this item actually occupies. The default is the
    /// contract as it stood before this epic -- the item is its box -- and the three fields
    /// below are the data the two narrower shapes need. Each belongs to exactly one shape;
    /// setting one against the wrong shape is refused rather than ignored, because a
    /// `compression_ratio` silently dropped on a `convex_hull` reads as an item packed to
    /// limits it never had.
    pub shape_type: ShapeType,
    pub hull_vertices: Option<Vec<Vertex>>,
    pub compression_ratio_ppm: Option<i64>,
    pub max_compression_pressure_kpa: Option<i64>,
}

impl Placement {
    /// This placement's rotated hull, or `None` when its box is the honest answer.
    ///
    /// `None` for every `rigid_cuboid` and for the cases `Item::hull_collision_is_exact`
    /// names. Shared rather than owned: building a hull is `O(v^4)`, `hull::shape_for`
    /// memoises it per item and rotation, and handing back a clone would give a good part of
    /// that saving straight back to the collision predicate that asks for it.
    pub fn hull_shape(&self) -> Option<Rc<HullShape>> {
        let item = &self.instance.item;
        let same_envelope = self.envelope_dimensions == self.dimensions;
        if !item.hull_collision_is_exact(same_envelope) {
            return None;
        }
        let vertices = item.hull_vertices.as_ref()?;
        hull::shape_for(vertices, hull::source_axes(self.rotation))
    }
}

/// Do two placed items actually overlap?
///
/// The axis-aligned envelope test is the broad phase and stays mandatory; this refines its
/// answer only when a hull is one of the two solids. One definition, so the solver, the
/// sequence check and the validator cannot disagree about what "collides" means.
/// Space one placement actually takes, which is its box only if it is one.
///
/// A `convex_hull` item occupies its hull: counting the bounding box is not a conservative
/// approximation of utilisation but a wrong number, putting two interlocking wedges at 200% of
/// a crate. A `compressible` item occupies the height left after the load it reports, which is
/// what makes `compression_ratio` observable at all.
pub fn occupied_volume(placement: &Placement) -> i128 {
    let item = &placement.instance.item;
    if item.shape_type == ShapeType::ConvexHull
        && let Some(vertices) = &item.hull_vertices
        && let Some(shape) = hull::shape_for(vertices, hull::source_axes(placement.rotation))
    {
        // A route or clearance may make collision use the conservative envelope. Physical
        // volume remains the authored hull and must not inherit that collision fallback.
        return shape.volume;
    }
    let Some(limit) = item.max_compression_pressure_kpa else {
        return placement.dimensions.volume();
    };
    let footprint = placement.dimensions.base_area();
    let Ok(pressure) = compression::applied_pressure(placement.top_load.0, footprint) else {
        return placement.dimensions.volume();
    };
    // A crushed item has no meaningful occupied volume, and the arrangement is already invalid
    // -- the crush check refuses it and the validator reports it. Reporting the uncompressed
    // figure keeps that one reported issue rather than a second, quieter one.
    if pressure.exceeds(limit).unwrap_or(true) {
        return placement.dimensions.volume();
    }
    match compression::effective_height(
        placement.dimensions.height.0,
        item.compression_ratio_ppm.unwrap_or(0),
        limit,
        pressure,
    ) {
        Ok(height) => footprint.saturating_mul(i128::from(height)),
        Err(_) => placement.dimensions.volume(),
    }
}

/// First compressible unit carrying more pressure than it declared it can take.
///
/// Deliberately shaped like the bearing check and reading the same propagated loads: the two
/// answer one question in two currencies -- a mass the box below must bear, against a pressure
/// the item itself must survive. An item can pass one and fail the other, so both are asked.
pub fn crushed(placements: &[Placement], loads: &[i128]) -> Option<(String, String)> {
    for (placement, load) in placements.iter().zip(loads) {
        let item = &placement.instance.item;
        let Some(limit) = item.max_compression_pressure_kpa else {
            continue;
        };
        let footprint = placement.envelope_dimensions.base_area();
        let carried = (*load).clamp(0, i128::from(i64::MAX)) as i64;
        let exceeded = compression::applied_pressure(carried, footprint)
            .and_then(|pressure| pressure.exceeds(limit))
            .unwrap_or(true);
        if exceeded {
            return Some(("crush_violation".into(), placement.instance.id()));
        }
    }
    None
}

pub fn placements_collide(left: &Placement, right: &Placement) -> bool {
    let left_box = left.envelope_box();
    let right_box = right.envelope_box();
    if !left_box.intersects(right_box) {
        return false;
    }
    let (left_shape, right_shape) = (left.hull_shape(), right.hull_shape());
    if left_shape.is_none() && right_shape.is_none() {
        return true;
    }
    solids_collide(
        left_shape.as_deref(),
        left_box,
        right_shape.as_deref(),
        right_box,
    )
}

/// Whether a placed item overlaps a plain box -- an obstacle, or any other fixed solid.
pub fn placement_hits_box(placement: &Placement, box_: Aabb) -> bool {
    let envelope = placement.envelope_box();
    if !envelope.intersects(box_) {
        return false;
    }
    match placement.hull_shape() {
        None => true,
        Some(shape) => solids_collide(Some(&shape), envelope, None, box_),
    }
}

/// Exact overlap between two solids of which at least one is a hull.
///
/// Reached only after the axis-aligned test has said their envelopes overlap, so the cost is
/// paid on the small set of pairs where a box answer would have been wrong.
pub fn solids_collide(
    left: Option<&HullShape>,
    left_box: Aabb,
    right: Option<&HullShape>,
    right_box: Aabb,
) -> bool {
    let left_owned;
    let left_shape = match left {
        Some(shape) => shape,
        None => {
            left_owned = box_shape_of(left_box);
            &left_owned
        }
    };
    let right_owned;
    let right_shape = match right {
        Some(shape) => shape,
        None => {
            right_owned = box_shape_of(right_box);
            &right_owned
        }
    };
    hull::collide(
        left_shape,
        [left_box.origin.x, left_box.origin.y, left_box.origin.z],
        right_shape,
        [right_box.origin.x, right_box.origin.y, right_box.origin.z],
    )
}

fn box_shape_of(box_: Aabb) -> HullShape {
    HullShape::box_shape(
        box_.dimensions.length.0,
        box_.dimensions.width.0,
        box_.dimensions.height.0,
    )
}

impl Item {
    /// Whether what rests on this item can change a verdict.
    ///
    /// The three original reasons are about the item refusing load. The fourth is about the
    /// item *yielding* to it: a compressible item needs the cumulative mass above it before
    /// its occupied height -- or its crush limit -- means anything.
    pub fn is_stack_sensitive(&self) -> bool {
        !self.stackable
            || self.max_top_load.is_some()
            || self.max_stacked_items.is_some()
            || self.max_compression_pressure_kpa.is_some()
    }

    /// Whether this item's collisions may be decided by its hull rather than by its box.
    ///
    /// Three conditions, each falling back to the box for its own reason: it is not a
    /// `convex_hull`; a clearance has inflated the envelope past the physical box, and a
    /// margin around a hull is not a hull; or the item is on a route, where the sequence
    /// replay reasons with box sweeps only and packing tighter than it can verify would
    /// produce arrangements the engine then reports as unloadable. Every fallback
    /// over-reserves space, the only safe direction to be wrong in.
    pub fn hull_collision_is_exact(&self, envelope_matches_physical: bool) -> bool {
        self.shape_type == ShapeType::ConvexHull
            && envelope_matches_physical
            && self.stop_index.is_none()
    }
}

#[derive(Clone, Debug)]
pub struct ItemInstance {
    pub item: Item,
    pub sequence: usize,
}

impl ItemInstance {
    pub fn id(&self) -> String {
        format!("{}#{}", self.item.id, self.sequence)
    }
}

#[derive(Clone, Debug)]
pub struct Obstacle {
    pub id: String,
    pub box_: Aabb,
    pub additional_boxes: Vec<Aabb>,
}

impl Obstacle {
    pub fn boxes(&self) -> impl Iterator<Item = Aabb> + '_ {
        std::iter::once(self.box_).chain(self.additional_boxes.iter().copied())
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Axle {
    pub position: Length,
    pub max_load: Option<Weight>,
}

/// A carrier rate card carried by the request rather than by a code plugin.
///
/// `weight_brackets_g` is strictly ascending and the same length as `prices_minor`; the
/// price charged is the one at the first bracket at or above the billed weight, which is
/// how a carrier's own published table reads. Everything is an exact integer -- grams,
/// minor currency units, parts per thousand -- because a landed cost that participates in
/// the objective must be reproducible bit for bit across four engines, and a float is not.
///
/// Prices are *not* required to be non-decreasing. A promotional band that dips is a real
/// rate card, and the table is read by bracket rather than by comparing prices.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RateTable {
    pub weight_brackets_g: Vec<i64>,
    pub prices_minor: Vec<i64>,
    pub minimum_charge_minor: i64,
    pub fuel_surcharge_permille: i64,
}

impl RateTable {
    /// The exact landed cost of one shipment at this billed weight, in minor units.
    ///
    /// `None` above the last bracket rather than a clamp to the top price: clamping would
    /// quietly under-price every oversize shipment and, worse, make the objective prefer
    /// a packing the caller cannot actually ship at that price.
    pub fn charge_minor(&self, billed_weight_g: i64) -> Option<i64> {
        for (bound, price) in self.weight_brackets_g.iter().zip(&self.prices_minor) {
            if billed_weight_g <= *bound {
                let base = (*price).max(self.minimum_charge_minor);
                // Ceiling division, never a float: the surcharge is a share of the base,
                // and rounding down would let a fractional unit of revenue vanish.
                let surcharge =
                    ceil_div(base as i128 * self.fuel_surcharge_permille as i128, 1000) as i64;
                return Some(base + surcharge);
            }
        }
        None
    }

    /// Billed weight in whole grams, rounded up -- a carrier reads a scale that way, and
    /// rounding down would price a shipment fractionally over a bracket in the one below.
    pub fn grams(weight_ticks: i64) -> i64 {
        ceil_div(weight_ticks as i128, Weight::TICKS_PER_G as i128) as i64
    }
}

#[derive(Clone, Debug)]
pub struct Container {
    pub id: String,
    pub inner_dimensions: Dimensions,
    pub outer_dimensions: Option<Dimensions>,
    pub tare_weight: Weight,
    pub max_payload: Option<Weight>,
    pub cost_minor: i64,
    pub quantity: Option<usize>,
    pub obstacles: Vec<Obstacle>,
    pub tags: BTreeSet<String>,
    pub max_items: Option<usize>,
    pub metadata: BTreeMap<String, Value>,
    pub axles: Option<[Axle; 2]>,
    /// Parts per million of inner volume reserved for packing material.
    pub void_fill_reserve_ppm: i128,
    pub tag_limits: BTreeMap<String, usize>,
    /// Weight ticks permitted per square metre of supporting footprint.
    pub max_stack_density: Option<Weight>,
    pub rate_table: Option<RateTable>,
}

#[derive(Clone, Debug)]
pub struct PackingRequest {
    pub items: Vec<Item>,
    pub containers: Vec<Container>,
    pub config: PackingConfig,
    pub output_length_unit: String,
    pub output_weight_unit: String,
    pub catalog_versions_used: Vec<Value>,
}

impl PackingRequest {
    pub fn instances(&self) -> Vec<ItemInstance> {
        self.items
            .iter()
            .flat_map(|item| {
                (1..=item.quantity).map(move |sequence| ItemInstance {
                    item: item.clone(),
                    sequence,
                })
            })
            .collect()
    }
}

#[derive(Clone, Debug)]
pub struct Placement {
    pub instance: ItemInstance,
    pub position: Point,
    pub rotation: Rotation,
    pub dimensions: Dimensions,
    pub envelope_origin: Point,
    pub envelope_dimensions: Dimensions,
    pub support_ratio: f64,
    pub top_load: Weight,
}

impl Placement {
    pub fn box_(&self) -> Aabb {
        Aabb {
            origin: self.position,
            dimensions: self.dimensions,
        }
    }

    pub fn envelope_box(&self) -> Aabb {
        Aabb {
            origin: self.envelope_origin,
            dimensions: self.envelope_dimensions,
        }
    }
}

/// Compact, `O(1)`-to-build description of one `GridSolver` regular-lattice run.
///
/// `GridSolver` already computes every field here in `O(r)` (at most six rotations)
/// before it ever places a single item -- rotation, physical/envelope dimensions,
/// per-axis capacity, layer step, clearance, and how many instances actually fit.
/// Building the summary costs nothing beyond what the solver already pays; what it
/// *replaces* is the `O(n)` loop that used to construct one `Placement` per item
/// purely to fill in per-item coordinates.
///
/// `expand` reconstructs the exact `Placement` list that loop would have built --
/// same order, same coordinates, same rotation -- so opting into the compact form
/// never loses information, only defers materializing it until a caller actually
/// asks for per-item coordinates.
///
/// Restricted to the case `Item::nesting_height` is unset: a nested column's used
/// volume and overlap bookkeeping depends on which adjacent pair of layers actually
/// touch, which is exact but not worth the closed-form derivation risk for a still
/// uncommon feature. `GridSolver` only builds a `LatticeSummary` when the prototype
/// item has no `nesting_height`; nesting keeps using the `O(n)` materializing loop
/// unchanged, regardless of the config flag.
#[derive(Clone, Debug)]
pub struct LatticeSummary {
    pub item_type: String,
    pub rotation: Rotation,
    pub physical: Dimensions,
    pub envelope: Dimensions,
    pub nx: i64,
    pub ny: i64,
    pub layer_step: i64,
    pub clearance_ticks: i64,
    pub count: usize,
    pub weight_ticks: i64,
}

impl LatticeSummary {
    fn per_layer(&self) -> i128 {
        self.nx as i128 * self.ny as i128
    }

    pub fn full_layers(&self) -> i128 {
        self.count as i128 / self.per_layer()
    }

    pub fn remainder(&self) -> i128 {
        self.count as i128 % self.per_layer()
    }

    pub fn layers_used(&self) -> i128 {
        self.full_layers() + i128::from(self.remainder() > 0)
    }

    pub fn total_weight_ticks(&self) -> i128 {
        self.count as i128 * self.weight_ticks as i128
    }

    /// Physical volume occupied. No nesting overlap term: `GridSolver` only ever
    /// builds a summary when the prototype has no `nesting_height` (see struct
    /// docs), so this reduces to a plain per-item volume sum.
    pub fn used_volume_ticks(&self) -> i128 {
        self.count as i128 * self.physical.volume()
    }

    /// Highest envelope `z2`, matching `Aabb::z2` of the topmost placed item's
    /// envelope box in the original per-item loop.
    pub fn max_z_ticks(&self) -> i64 {
        let top_layer_index = (self.layers_used() - 1) as i64;
        top_layer_index
            .saturating_mul(self.layer_step)
            .saturating_add(self.envelope.height.0)
    }

    fn triangular(n: i128) -> i128 {
        n * (n - 1) / 2
    }

    /// Closed-form equivalent of `PackedContainer::centre_of_mass_offset_ppm` for a
    /// uniform lattice of identical items, without expanding a single `Placement`.
    ///
    /// The per-item formula there is
    /// `doubled_weighted_axis = sum(weight * (2*position + dimension))`; because
    /// every instance shares the same weight and physical dimensions in a lattice,
    /// this reduces to `weight * (2*envelope_step*sum(index) + count*(2*clearance +
    /// dimension))`, where `sum(index)` is the sum of the x (or y) grid index across
    /// every placed item -- itself a closed form over full layers plus one partial
    /// layer, since the grid fills `x` fastest, then `y`, then `z` (see `GridSolver`).
    pub fn centre_of_mass_offset_ppm(
        &self,
        inner_length_ticks: i64,
        inner_width_ticks: i64,
    ) -> i128 {
        if self.weight_ticks == 0 || self.count == 0 {
            return 0;
        }
        let nx = self.nx as i128;
        let ny = self.ny as i128;
        let full_layers = self.full_layers();
        let remainder = self.remainder();
        let rows_in_partial = remainder / nx;
        let extra_cols = remainder % nx;
        let sum_x = full_layers * ny * Self::triangular(nx)
            + rows_in_partial * Self::triangular(nx)
            + Self::triangular(extra_cols);
        let sum_y = full_layers * nx * Self::triangular(ny)
            + nx * Self::triangular(rows_in_partial)
            + extra_cols * rows_in_partial;
        let clearance = self.clearance_ticks as i128;
        let count = self.count as i128;
        let weight = self.weight_ticks as i128;
        let doubled_weighted_x = weight.saturating_mul(
            (2_i128 * self.envelope.length.0 as i128 * sum_x).saturating_add(
                count.saturating_mul(2 * clearance + self.physical.length.0 as i128),
            ),
        );
        let doubled_weighted_y = weight.saturating_mul(
            (2_i128 * self.envelope.width.0 as i128 * sum_y).saturating_add(
                count.saturating_mul(2 * clearance + self.physical.width.0 as i128),
            ),
        );
        let total_weight = self.total_weight_ticks();
        let numerator_x = doubled_weighted_x
            .saturating_sub(total_weight.saturating_mul(inner_length_ticks as i128));
        let numerator_y = doubled_weighted_y
            .saturating_sub(total_weight.saturating_mul(inner_width_ticks as i128));
        let offset_x_ppm = numerator_x.abs().saturating_mul(1_000_000)
            / total_weight.saturating_mul(inner_length_ticks as i128);
        let offset_y_ppm = numerator_y.abs().saturating_mul(1_000_000)
            / total_weight.saturating_mul(inner_width_ticks as i128);
        offset_x_ppm.max(offset_y_ppm)
    }

    /// Rebuild the exact `Placement` list the original per-item loop would have
    /// produced, in the same order, from `items[0..count]`.
    pub fn expand(&self, items: &[ItemInstance]) -> Vec<Placement> {
        let mut out = Vec::with_capacity(self.count);
        for (index, item) in items.iter().take(self.count).enumerate() {
            let index = index as i64;
            let x = index % self.nx;
            let y = (index / self.nx) % self.ny;
            let z = index / (self.nx * self.ny);
            let origin = Point {
                x: x * self.envelope.length.0,
                y: y * self.envelope.width.0,
                z: z * self.layer_step,
            };
            out.push(Placement {
                instance: item.clone(),
                position: Point {
                    x: origin.x + self.clearance_ticks,
                    y: origin.y + self.clearance_ticks,
                    z: origin.z + self.clearance_ticks,
                },
                rotation: self.rotation,
                dimensions: self.physical,
                envelope_origin: origin,
                envelope_dimensions: self.envelope,
                support_ratio: 1.0,
                top_load: Weight(0),
            });
        }
        out
    }
}

#[derive(Clone, Debug)]
pub struct PackedContainer {
    pub container: Container,
    pub sequence: usize,
    pub placements: Vec<Placement>,
    /// Populated instead of `placements` (which stays empty) when `GridSolver` took
    /// the quantity-compression fast path
    /// (`configuration.require_placement_coordinates = false`): the lattice
    /// parameters plus how many instances were placed, in `O(1)`/`O(r)` additional
    /// space rather than one `Placement` per instance.
    pub lattice_summary: Option<LatticeSummary>,
    /// The specific instances the lattice fast path consumed, in placement order --
    /// needed to reconstruct identical `Placement` objects via `expand_placements`.
    /// This list itself is still `O(n)`; only the geometry it would otherwise
    /// require per item is what the fast path avoids computing.
    pub lattice_items: Vec<ItemInstance>,
}

impl PackedContainer {
    pub fn id(&self) -> String {
        format!("{}#{}", self.container.id, self.sequence)
    }

    pub fn placement_count(&self) -> usize {
        self.lattice_summary
            .as_ref()
            .map(|summary| summary.count)
            .unwrap_or(self.placements.len())
    }

    pub fn payload_weight(&self) -> Weight {
        if let Some(summary) = &self.lattice_summary {
            return Weight(summary.total_weight_ticks().clamp(0, i64::MAX as i128) as i64);
        }
        Weight(self.placements.iter().fold(0_i64, |sum, placement| {
            sum.saturating_add(placement.instance.item.weight.0)
        }))
    }

    pub fn gross_weight(&self) -> Weight {
        Weight(
            self.container
                .tare_weight
                .0
                .saturating_add(self.payload_weight().0),
        )
    }

    pub fn used_volume(&self) -> i128 {
        if let Some(summary) = &self.lattice_summary {
            return summary.used_volume_ticks();
        }
        let total = self.placements.iter().map(occupied_volume).sum::<i128>();
        let mut overlap = 0_i128;
        for (index, placement) in self.placements.iter().enumerate() {
            for other in self.placements.iter().skip(index + 1) {
                if valid_nesting(placement, other) {
                    overlap = overlap.saturating_add(
                        placement.instance.item.nesting_height.unwrap_or_default().0 as i128
                            * placement.envelope_dimensions.base_area(),
                    );
                }
            }
        }
        total.saturating_sub(overlap)
    }

    /// Highest envelope `z2` across every placed item, used by the objective's
    /// height component. Reduces to the closed-form `LatticeSummary::max_z_ticks`
    /// when the compact fast path applies, since that path never expands
    /// `placements`.
    pub fn max_z_ticks(&self) -> i128 {
        if let Some(summary) = &self.lattice_summary {
            return summary.max_z_ticks() as i128;
        }
        self.placements
            .iter()
            .map(|placement| placement.envelope_box().z2() as i128)
            .max()
            .unwrap_or(0)
    }

    pub fn utilization(&self) -> f64 {
        self.used_volume() as f64 / self.container.inner_dimensions.volume() as f64
    }

    pub fn centre_of_mass_offset_ppm(&self) -> i128 {
        if let Some(summary) = &self.lattice_summary {
            return summary.centre_of_mass_offset_ppm(
                self.container.inner_dimensions.length.0,
                self.container.inner_dimensions.width.0,
            );
        }
        let total_weight = self.placements.iter().fold(0_i128, |sum, placement| {
            sum.saturating_add(placement.instance.item.weight.0 as i128)
        });
        if total_weight == 0 {
            return 0;
        }
        let length = self.container.inner_dimensions.length.0 as i128;
        let width = self.container.inner_dimensions.width.0 as i128;
        let doubled_weighted_x = self.placements.iter().fold(0_i128, |sum, placement| {
            let centre = 2_i128
                .saturating_mul(placement.position.x as i128)
                .saturating_add(placement.dimensions.length.0 as i128);
            sum.saturating_add((placement.instance.item.weight.0 as i128).saturating_mul(centre))
        });
        let doubled_weighted_y = self.placements.iter().fold(0_i128, |sum, placement| {
            let centre = 2_i128
                .saturating_mul(placement.position.y as i128)
                .saturating_add(placement.dimensions.width.0 as i128);
            sum.saturating_add((placement.instance.item.weight.0 as i128).saturating_mul(centre))
        });
        let x = doubled_weighted_x
            .saturating_sub(total_weight.saturating_mul(length))
            .abs()
            .saturating_mul(1_000_000)
            / total_weight.saturating_mul(length);
        let y = doubled_weighted_y
            .saturating_sub(total_weight.saturating_mul(width))
            .abs()
            .saturating_mul(1_000_000)
            / total_weight.saturating_mul(width);
        x.max(y)
    }

    /// The full per-item `Placement` list, reconstructed from `lattice_summary` if
    /// the compact fast path was taken, otherwise `placements` unchanged. Used only
    /// by independent validation, which needs real geometry to re-derive every
    /// guarantee; the returned `PackingResult` itself stays compact.
    pub fn expand_placements(&self) -> Vec<Placement> {
        match &self.lattice_summary {
            Some(summary) => summary.expand(&self.lattice_items),
            None => self.placements.clone(),
        }
    }

    pub fn axle_reactions(&self) -> Option<(i128, i128, i128)> {
        axle_reactions(&self.container, &self.placements, None)
    }
}

pub fn axle_reactions(
    container: &Container,
    placements: &[Placement],
    extra: Option<(Weight, Aabb)>,
) -> Option<(i128, i128, i128)> {
    let [front, rear] = container.axles?;
    let mut total_weight = container.tare_weight.0 as i128;
    // Tare is part of the gross axle basis and acts at the container's exact
    // geometric longitudinal centre. Using the doubled coordinate preserves an
    // odd half-tick without rounding.
    let mut doubled_weighted_x = (container.tare_weight.0 as i128)
        .saturating_mul(container.inner_dimensions.length.0 as i128);
    for placement in placements {
        let weight = placement.instance.item.weight.0 as i128;
        total_weight = total_weight.saturating_add(weight);
        let centre = 2_i128
            .saturating_mul(placement.position.x as i128)
            .saturating_add(placement.dimensions.length.0 as i128);
        doubled_weighted_x = doubled_weighted_x.saturating_add(weight.saturating_mul(centre));
    }
    if let Some((weight, box_)) = extra {
        let weight = weight.0 as i128;
        total_weight = total_weight.saturating_add(weight);
        let centre = 2_i128
            .saturating_mul(box_.origin.x as i128)
            .saturating_add(box_.dimensions.length.0 as i128);
        doubled_weighted_x = doubled_weighted_x.saturating_add(weight.saturating_mul(centre));
    }
    let denominator = 2_i128.saturating_mul((rear.position.0 - front.position.0) as i128);
    let front_numerator = 2_i128
        .saturating_mul(total_weight)
        .saturating_mul(rear.position.0 as i128)
        .saturating_sub(doubled_weighted_x);
    let rear_numerator = doubled_weighted_x.saturating_sub(
        2_i128
            .saturating_mul(total_weight)
            .saturating_mul(front.position.0 as i128),
    );
    Some((denominator, front_numerator, rear_numerator))
}

pub fn axle_overloaded(
    container: &Container,
    placements: &[Placement],
    extra: Option<(Weight, Aabb)>,
) -> bool {
    let Some((denominator, front_numerator, rear_numerator)) =
        axle_reactions(container, placements, extra)
    else {
        return false;
    };
    let [front, rear] = container.axles.expect("reactions require axles");
    front
        .max_load
        .map(|limit| front_numerator > (limit.0 as i128).saturating_mul(denominator))
        .unwrap_or(false)
        || rear
            .max_load
            .map(|limit| rear_numerator > (limit.0 as i128).saturating_mul(denominator))
            .unwrap_or(false)
}

/// The tightest x-origins that seat this item exactly on either axle's limit.
///
/// `candidate_points` only ever proposes positions derived from the container's own
/// origin or a placed box's far face -- correct for plain volume packing, where
/// nothing is ever gained by leaving a gap, but incomplete once axle limits are in
/// play: sometimes the only feasible spot for an item is floating away from every
/// wall and every other box, specifically to keep this item's own moment from
/// tipping one axle over its limit. A floor-level item needs no lateral contact for
/// support, so that position is otherwise unreachable by any point this module
/// already generates.
///
/// Solving `axle_reactions`'s own boundary equation for this item's centre -- "where
/// would the front/rear reaction land exactly on its limit if this item's centre
/// were here" -- turns the two axle limits into two extra x-origins worth trying, on
/// top of the ordinary candidate points. Both are exact integer ticks, biased toward
/// the safe side of their own limit (never past it) since a placement that lands
/// exactly on the boundary is still allowed by `axle_overloaded`'s own strict `>`
/// comparison; the caller runs every candidate through the same collision and
/// constraint checks regardless, so a boundary that turns out unreachable (blocked,
/// out of bounds, or infeasible for the other axle) is simply rejected same as any
/// other candidate.
pub fn axle_balanced_origins(
    container: &Container,
    placements: &[Placement],
    item_weight: Weight,
    item_length: i64,
) -> Vec<i64> {
    let Some([front, rear]) = container.axles else {
        return Vec::new();
    };
    if item_weight.0 <= 0 {
        return Vec::new();
    }
    let item_weight = item_weight.0 as i128;
    let mut other_total = container.tare_weight.0 as i128;
    let mut other_doubled_x = (container.tare_weight.0 as i128)
        .saturating_mul(container.inner_dimensions.length.0 as i128);
    for placement in placements {
        let weight = placement.instance.item.weight.0 as i128;
        other_total = other_total.saturating_add(weight);
        let centre = 2_i128
            .saturating_mul(placement.position.x as i128)
            .saturating_add(placement.dimensions.length.0 as i128);
        other_doubled_x = other_doubled_x.saturating_add(weight.saturating_mul(centre));
    }
    let total = other_total.saturating_add(item_weight);
    let denominator = 2_i128.saturating_mul((rear.position.0 - front.position.0) as i128);
    let mut origins = Vec::new();
    if let Some(max_load) = front.max_load {
        // Smallest doubled centre for which front_numerator <= front limit * denominator.
        let required = 2_i128
            .saturating_mul(total)
            .saturating_mul(rear.position.0 as i128)
            .saturating_sub((max_load.0 as i128).saturating_mul(denominator))
            .saturating_sub(other_doubled_x);
        let doubled_centre = ceil_div(required, item_weight); // never understate what front needs
        origins.push(ceil_div(doubled_centre - item_length as i128, 2) as i64); // stay on the safe side
    }
    if let Some(max_load) = rear.max_load {
        // Largest doubled centre for which rear_numerator <= rear limit * denominator.
        let allowed = (max_load.0 as i128)
            .saturating_mul(denominator)
            .saturating_add(
                2_i128
                    .saturating_mul(total)
                    .saturating_mul(front.position.0 as i128),
            )
            .saturating_sub(other_doubled_x);
        let doubled_centre = allowed.div_euclid(item_weight); // never overstate what rear allows
        origins.push((doubled_centre - item_length as i128).div_euclid(2) as i64); // stay on the safe side
    }
    origins
}

fn ceil_div(a: i128, b: i128) -> i128 {
    -(-a).div_euclid(b)
}

pub fn valid_nesting(a: &Placement, b: &Placement) -> bool {
    if a.instance.item.id != b.instance.item.id {
        return false;
    }
    let (Some(depth), Some(other_depth)) = (
        a.instance.item.nesting_height,
        b.instance.item.nesting_height,
    ) else {
        return false;
    };
    if depth != other_depth {
        return false;
    }
    let a_box = a.envelope_box();
    let b_box = b.envelope_box();
    if (a_box.origin.x, a_box.origin.y, a_box.x2(), a_box.y2())
        != (b_box.origin.x, b_box.origin.y, b_box.x2(), b_box.y2())
    {
        return false;
    }
    let (lower, upper) = if a_box.origin.z <= b_box.origin.z {
        (a_box, b_box)
    } else {
        (b_box, a_box)
    };
    lower.origin.z != upper.origin.z && lower.z2() - upper.origin.z == depth.0
}

#[derive(Clone, Debug)]
pub struct RejectionObservation {
    pub code: String,
    pub count: u64,
    pub details: Vec<String>,
}

impl RejectionObservation {
    pub fn to_json(&self) -> Value {
        serde_json::json!({
            "code": self.code,
            "count": self.count,
            "details": self.details,
        })
    }
}

#[derive(Clone, Debug)]
pub struct ReasonProof {
    pub level: String,
    pub observations: Vec<RejectionObservation>,
}

impl ReasonProof {
    pub fn for_reason(reason: &str, details: &[String]) -> Self {
        // `policy_rule` is proven, not inferred: it is only ever reported when a rule
        // rules the item out of *every* offered container, which is a statement about the
        // request that no search outcome can change.
        let level = if matches!(
            reason,
            "no_compatible_container_dimensions"
                | "payload_exceeded"
                | "rotation_restricted"
                | "policy_rule"
        ) {
            "proven"
        } else if matches!(reason, "time_limit" | "effort_limit") {
            "unknown_due_to_limit"
        } else if matches!(
            reason,
            "no_feasible_placement" | "search_exhausted" | "exact_search_incomplete"
        ) {
            "observed"
        } else {
            "inferred"
        };
        Self {
            level: level.into(),
            observations: vec![RejectionObservation {
                code: reason.into(),
                count: 1,
                details: details.to_vec(),
            }],
        }
    }

    pub fn to_json(&self) -> Value {
        serde_json::json!({
            "level": self.level,
            "observations": self
                .observations
                .iter()
                .map(RejectionObservation::to_json)
                .collect::<Vec<_>>(),
        })
    }
}

#[derive(Clone, Debug)]
pub struct UnpackedItem {
    pub instance: ItemInstance,
    pub reason: String,
    pub details: Vec<String>,
    pub proof: ReasonProof,
}

impl UnpackedItem {
    pub fn new(instance: ItemInstance, reason: String, details: Vec<String>) -> Self {
        let proof = ReasonProof::for_reason(&reason, &details);
        Self {
            instance,
            reason,
            details,
            proof,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PackingStatus {
    Optimal,
    Feasible,
    BestFound,
    TimeLimit,
    Infeasible,
    InvalidResult,
}

impl PackingStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Optimal => "optimal",
            Self::Feasible => "feasible",
            Self::BestFound => "best_found",
            Self::TimeLimit => "time_limit",
            Self::Infeasible => "infeasible",
            Self::InvalidResult => "invalid_result",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResultFact {
    pub code: String,
    pub attributes: BTreeMap<String, Value>,
}

impl ResultFact {
    pub fn new(code: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            attributes: BTreeMap::new(),
        }
    }

    pub fn from_json(value: &Value) -> Option<Self> {
        let object = value.as_object()?;
        let code = object.get("code")?.as_str()?;
        if code.is_empty() {
            return None;
        }
        let attributes = object
            .iter()
            .filter(|(key, _)| key.as_str() != "code")
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect();
        Some(Self {
            code: code.to_owned(),
            attributes,
        })
    }

    pub fn to_json(&self) -> Value {
        let mut object = serde_json::Map::new();
        object.insert("code".into(), Value::String(self.code.clone()));
        object.extend(
            self.attributes
                .iter()
                .map(|(key, value)| (key.clone(), value.clone())),
        );
        Value::Object(object)
    }
}

pub fn derive_result_facts(
    status: PackingStatus,
    complete: bool,
    timed_out: bool,
) -> (ResultFact, ResultFact, ResultFact) {
    let feasibility = if status == PackingStatus::Infeasible {
        "infeasible"
    } else if complete && status != PackingStatus::InvalidResult {
        "feasible"
    } else {
        "unknown"
    };
    let termination = if status == PackingStatus::InvalidResult {
        "error"
    } else if timed_out {
        "time_limit"
    } else {
        "complete"
    };
    let optimality = if status == PackingStatus::Optimal {
        "proven_optimal"
    } else if status == PackingStatus::Infeasible {
        "proven_infeasible"
    } else if !complete && status != PackingStatus::InvalidResult {
        "best_found"
    } else {
        "not_proven"
    };
    (
        ResultFact::new(feasibility),
        ResultFact::new(termination),
        ResultFact::new(optimality),
    )
}

#[derive(Clone, Debug, Default)]
pub struct SolverMetrics {
    pub candidate_points_considered: u64,
    pub orientations_considered: u64,
    pub feasible_candidates: u64,
    pub collision_checks: u64,
    pub support_checks: u64,
    pub space_partitions: u64,
    pub search_nodes_expanded: u64,
}

impl SolverMetrics {
    pub fn to_json(&self) -> Value {
        serde_json::json!({
            "candidate_points_considered": self.candidate_points_considered,
            "orientations_considered": self.orientations_considered,
            "feasible_candidates": self.feasible_candidates,
            "collision_checks": self.collision_checks,
            "support_checks": self.support_checks,
            "space_partitions": self.space_partitions,
            "search_nodes_expanded": self.search_nodes_expanded,
        })
    }
}

pub fn effort_exhausted(request: &PackingRequest, metrics: &SolverMetrics) -> bool {
    request
        .config
        .effort_budget
        .map(|budget| budget.exceeded(metrics))
        .unwrap_or(false)
}

#[derive(Clone, Debug, Default)]
pub struct AlgorithmReport {
    pub profile: String,
    pub solver: String,
    pub duration_ms: u64,
    pub seed: u64,
    pub time_limit_reached: bool,
    pub effort_limit_reached: bool,
    pub candidates_evaluated: u64,
    pub placements_attempted: u64,
    pub metrics: SolverMetrics,
}

#[derive(Clone, Debug)]
pub struct StartRecord {
    pub id: String,
    pub started: bool,
    pub completed: bool,
    pub truncated: bool,
    pub selected: bool,
    pub global_deadline_reached: bool,
}

impl StartRecord {
    pub fn to_json(&self) -> Value {
        serde_json::json!({
            "id": self.id,
            "started": self.started,
            "completed": self.completed,
            "truncated": self.truncated,
            "selected": self.selected,
            "global_deadline_reached": self.global_deadline_reached,
        })
    }
}

pub fn aggregate_termination(starts: &[StartRecord], error: bool) -> ResultFact {
    assert!(
        !starts.is_empty(),
        "termination aggregation requires at least one start record"
    );
    let selected = starts
        .iter()
        .filter(|record| record.selected)
        .collect::<Vec<_>>();
    assert_eq!(
        selected.len(),
        1,
        "termination aggregation requires exactly one selected start"
    );
    let any_truncated = starts.iter().any(|record| record.truncated);
    let all_completed = starts.iter().all(|record| record.completed);
    let winning_truncated = selected[0].truncated;
    let global_deadline = starts.iter().any(|record| record.global_deadline_reached);
    let code = if error {
        "error"
    } else if winning_truncated || global_deadline {
        "time_limit"
    } else {
        "complete"
    };
    let mut attributes = BTreeMap::new();
    attributes.insert("any_start_truncated".into(), Value::Bool(any_truncated));
    attributes.insert(
        "all_required_starts_completed".into(),
        Value::Bool(all_completed),
    );
    attributes.insert(
        "winning_start_truncated".into(),
        Value::Bool(winning_truncated),
    );
    attributes.insert(
        "global_deadline_reached".into(),
        Value::Bool(global_deadline),
    );
    attributes.insert(
        "starts".into(),
        Value::Array(starts.iter().map(StartRecord::to_json).collect()),
    );
    ResultFact {
        code: code.into(),
        attributes,
    }
}

#[derive(Clone, Debug)]
pub struct PackingResult {
    pub status: PackingStatus,
    pub containers: Vec<PackedContainer>,
    pub unpacked: Vec<UnpackedItem>,
    pub algorithm: AlgorithmReport,
    pub score: Vec<i128>,
    pub warnings: Vec<String>,
    pub alternatives: Vec<PackingResult>,
    pub feasibility: Option<ResultFact>,
    pub termination: Option<ResultFact>,
    pub optimality: Option<ResultFact>,
    pub objective: String,
    pub catalog_versions_used: Vec<Value>,
}

impl PackingResult {
    pub fn complete(&self) -> bool {
        self.unpacked.is_empty()
    }

    pub fn packed_item_count(&self) -> usize {
        self.containers
            .iter()
            .map(PackedContainer::placement_count)
            .sum()
    }

    pub fn to_json(
        &self,
        length_unit: &str,
        weight_unit: &str,
        include_alternatives: bool,
    ) -> Value {
        let derived = derive_result_facts(
            self.status,
            self.complete(),
            self.algorithm.time_limit_reached,
        );
        let feasibility = self.feasibility.as_ref().unwrap_or(&derived.0);
        let default_termination = aggregate_termination(
            &[StartRecord {
                id: if self.algorithm.solver.is_empty() {
                    "unknown".into()
                } else {
                    self.algorithm.solver.clone()
                },
                started: true,
                completed: !self.algorithm.time_limit_reached
                    && !self.algorithm.effort_limit_reached,
                truncated: self.algorithm.time_limit_reached || self.algorithm.effort_limit_reached,
                selected: true,
                global_deadline_reached: self.algorithm.time_limit_reached,
            }],
            self.status == PackingStatus::InvalidResult,
        );
        let mut termination = self
            .termination
            .as_ref()
            .unwrap_or(&default_termination)
            .clone();
        if self.algorithm.effort_limit_reached {
            termination.code = "effort_limit".into();
        }
        let optimality = self.optimality.as_ref().unwrap_or(&derived.2);
        let containers = self
            .containers
            .iter()
            .map(|packed| {
                let placements = packed
                    .placements
                    .iter()
                    .map(|placement| {
                        serde_json::json!({
                            "item_id": placement.instance.id(),
                            "item_type": placement.instance.item.id.clone(),
                            "position": placement.position.to_json(length_unit),
                            "dimensions": placement.dimensions.to_json(length_unit),
                            "orientation": placement.rotation.as_str(),
                            "support_ratio": format!("{:.6}", placement.support_ratio),
                            "top_load": placement.top_load.to_json(weight_unit),
                        })
                    })
                    .collect::<Vec<_>>();

                let mut serialized = serde_json::json!({
                    "id": packed.id(),
                    "container_type": packed.container.id.clone(),
                    "inner_dimensions": packed.container.inner_dimensions.to_json(length_unit),
                    "outer_dimensions": packed
                        .container
                        .outer_dimensions
                        .unwrap_or(packed.container.inner_dimensions)
                        .to_json(length_unit),
                    "payload_weight": packed.payload_weight().to_json(weight_unit),
                    "gross_weight": packed.gross_weight().to_json(weight_unit),
                    "used_volume_ticks3": packed.used_volume().to_string(),
                    "volume_utilization": format!("{:.6}", packed.utilization()),
                    "centre_of_mass_offset_ppm": packed.centre_of_mass_offset_ppm(),
                    "void_fill_reserve_ticks3":
                        (packed.container.inner_dimensions.volume()
                            * packed.container.void_fill_reserve_ppm / 1_000_000).to_string(),
                    "placements": placements,
                });
                if let Some((denominator, front, rear)) = packed.axle_reactions() {
                    serialized["axle_reactions"] = serde_json::json!({
                        "basis": "gross",
                        "denominator": denominator.to_string(),
                        "front_numerator": front.to_string(),
                        "rear_numerator": rear.to_string(),
                    });
                }
                if let Some(summary) = &packed.lattice_summary {
                    serialized["lattice_summary"] = serde_json::json!({
                        "item_type": summary.item_type,
                        "orientation": summary.rotation.as_str(),
                        "physical_dimensions": summary.physical.to_json(length_unit),
                        "envelope_dimensions": summary.envelope.to_json(length_unit),
                        "nx": summary.nx,
                        "ny": summary.ny,
                        "layers_used": summary.layers_used() as i64,
                        "layer_step": Length(summary.layer_step).to_json(length_unit),
                        "count": summary.count,
                    });
                }
                serialized
            })
            .collect::<Vec<_>>();

        let alternatives = if include_alternatives {
            self.alternatives
                .iter()
                .map(|alternative| alternative.to_json(length_unit, weight_unit, false))
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };

        // Every key of the canonical objective vector is bounded by the number of
        // containers times the parts-per-million scale, so all of them fit a JSON
        // number exactly -- with one exception the unchecked cast used to get wrong.
        // `lowest_landed_cost` ranks a shipment the tariff cannot price at `i128::MAX`,
        // and `i128::MAX as i64` wraps to -1: the sentinel meaning "worst possible"
        // arrived in the result as the *best* possible score, which is precisely the
        // "never rank an unpriceable packing as free" rule it exists to enforce.
        // Saturating keeps the ordering the search used. See docs/OBJECTIVE.md.
        let score = self
            .score
            .iter()
            .map(|component| {
                Value::from(i64::try_from(*component).unwrap_or(if *component < 0 {
                    i64::MIN
                } else {
                    i64::MAX
                }))
            })
            .collect::<Vec<_>>();

        serde_json::json!({
            "status": self.status.as_str(),
            "feasibility": feasibility.to_json(),
            "termination": termination.to_json(),
            "optimality": optimality.to_json(),
            "complete": self.complete(),
            "objective": self.objective.clone(),
            "algorithm": {
                "profile": self.algorithm.profile.clone(),
                "solver": self.algorithm.solver.clone(),
                "duration_ms": self.algorithm.duration_ms,
                "seed": self.algorithm.seed,
                "time_limit_reached": self.algorithm.time_limit_reached,
                "effort_limit_reached": self.algorithm.effort_limit_reached,
                "candidates_evaluated": self.algorithm.candidates_evaluated,
                "placements_attempted": self.algorithm.placements_attempted,
                "metrics": self.algorithm.metrics.to_json(),
            },
            "summary": {
                "container_count": self.containers.len(),
                "packed_item_count": self.packed_item_count(),
                "unpacked_item_count": self.unpacked.len(),
            },
            "score": score,
            "containers": containers,
            "unpacked_items": self
                .unpacked
                .iter()
                .map(|unpacked| serde_json::json!({
                    "item_id": unpacked.instance.id(),
                    "item_type": unpacked.instance.item.id.clone(),
                    "reason": unpacked.reason.clone(),
                    "details": unpacked.details.clone(),
                    "proof": unpacked.proof.to_json(),
                }))
                .collect::<Vec<_>>(),
            "catalog_versions_used": self.catalog_versions_used.clone(),
            "warnings": self.warnings.clone(),
            "alternatives": alternatives,
        })
    }
}

#[cfg(test)]
mod tests {
    //! The hull-versus-box collision path, and what a placement is counted as occupying.
    //!
    //! The shared request corpus compares this engine's answers against the other three.
    //! What whole-request comparison cannot show is a branch: it exercises
    //! whole requests, so `solids_collide`'s box fallbacks and `occupied_volume`'s refusal
    //! paths were reachable from a packing and unreachable from `cargo test`. That is how
    //! `model.rs` came to sit a point below its coverage baseline while every answer-level
    //! check was green.

    use std::collections::{BTreeMap, BTreeSet};

    use super::*;
    use crate::geometry::{Dimensions, Point, Rotation, ShapeType};
    use crate::units::{Length, Weight};

    const SIDE: i64 = 100;

    /// The half-cube that made hulls worth having: it leaves the far top corner of its own
    /// bounding box empty, which is exactly where a box test and a hull test disagree.
    fn wedge() -> Vec<Vertex> {
        vec![
            [0, 0, 0],
            [SIDE, 0, 0],
            [0, SIDE, 0],
            [0, 0, SIDE],
            [SIDE, 0, SIDE],
            [0, SIDE, SIDE],
        ]
    }

    fn item(id: &str, shape: ShapeType, vertices: Option<Vec<Vertex>>) -> Item {
        Item {
            id: id.into(),
            dimensions: Dimensions {
                length: Length(SIDE),
                width: Length(SIDE),
                height: Length(SIDE),
            },
            weight: Weight(0),
            quantity: 1,
            allowed_rotations: vec![Rotation::Lwh],
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
            value: None,
            shape_type: shape,
            hull_vertices: vertices,
            compression_ratio_ppm: None,
            max_compression_pressure_kpa: None,
            eligible_container_tags: BTreeSet::new(),
        }
    }

    fn placed(item: Item, at: (i64, i64, i64)) -> Placement {
        let position = Point {
            x: at.0,
            y: at.1,
            z: at.2,
        };
        Placement {
            instance: ItemInstance {
                item: item.clone(),
                sequence: 1,
            },
            position,
            rotation: Rotation::Lwh,
            dimensions: item.dimensions,
            envelope_origin: position,
            envelope_dimensions: item.dimensions,
            support_ratio: 1.0,
            top_load: Weight(0),
        }
    }

    fn cube_at(at: (i64, i64, i64), side: i64) -> Aabb {
        Aabb {
            origin: Point {
                x: at.0,
                y: at.1,
                z: at.2,
            },
            dimensions: Dimensions {
                length: Length(side),
                width: Length(side),
                height: Length(side),
            },
        }
    }

    /// The corner a bounding box claims and a wedge does not.
    #[test]
    fn a_hull_does_not_hit_a_box_in_the_corner_it_leaves_empty() {
        let hull = placed(item("w", ShapeType::ConvexHull, Some(wedge())), (0, 0, 0));
        let empty_corner = cube_at((60, 60, 60), 30);
        assert!(
            hull.envelope_box().intersects(empty_corner),
            "the envelopes must overlap, or the broad phase would answer and prove nothing"
        );
        assert!(!placement_hits_box(&hull, empty_corner));
    }

    #[test]
    fn a_hull_hits_a_box_inside_its_own_solid() {
        let hull = placed(item("w", ShapeType::ConvexHull, Some(wedge())), (0, 0, 0));
        assert!(placement_hits_box(&hull, cube_at((5, 5, 5), 20)));
    }

    /// A `rigid_cuboid` never reaches the exact test: its box *is* its solid, so the broad
    /// phase is the whole answer.
    #[test]
    fn a_cuboid_hits_any_box_its_envelope_overlaps() {
        let cuboid = placed(item("c", ShapeType::RigidCuboid, None), (0, 0, 0));
        assert!(placement_hits_box(&cuboid, cube_at((60, 60, 60), 30)));
        assert!(!placement_hits_box(&cuboid, cube_at((200, 0, 0), 10)));
    }

    /// One hull against one cuboid: the cuboid is turned into a shape so the two can be
    /// compared on the same axes, which is the fallback a box-only request never reaches.
    #[test]
    fn a_hull_and_a_cuboid_collide_by_the_hull_rather_than_by_the_boxes() {
        let hull = placed(item("w", ShapeType::ConvexHull, Some(wedge())), (0, 0, 0));
        let clear = placed(item("c", ShapeType::RigidCuboid, None), (60, 60, 60));
        assert!(
            hull.envelope_box().intersects(clear.envelope_box()),
            "boxes overlap; only the hull test can tell them apart"
        );
        assert!(!placements_collide(&hull, &clear));
        assert!(!placements_collide(&clear, &hull), "and symmetrically");

        let overlapping = placed(item("c", ShapeType::RigidCuboid, None), (10, 10, 10));
        assert!(placements_collide(&hull, &overlapping));
    }

    #[test]
    fn two_cuboids_never_leave_the_broad_phase() {
        let left = placed(item("a", ShapeType::RigidCuboid, None), (0, 0, 0));
        let right = placed(item("b", ShapeType::RigidCuboid, None), (50, 0, 0));
        assert!(placements_collide(&left, &right));
        let apart = placed(item("b", ShapeType::RigidCuboid, None), (SIDE, 0, 0));
        assert!(
            !placements_collide(&left, &apart),
            "touching is contact, not collision"
        );
    }

    /// Utilisation is the one number a wrong occupancy rule corrupts silently: two
    /// interlocking wedges counted by their boxes fill a crate twice over.
    #[test]
    fn a_hull_occupies_its_hull_and_a_cuboid_its_box() {
        let hull = placed(item("w", ShapeType::ConvexHull, Some(wedge())), (0, 0, 0));
        assert_eq!(
            occupied_volume(&hull),
            i128::from(SIDE) * i128::from(SIDE) * i128::from(SIDE) / 2
        );
        let cuboid = placed(item("c", ShapeType::RigidCuboid, None), (0, 0, 0));
        assert_eq!(occupied_volume(&cuboid), cuboid.dimensions.volume());
    }

    /// A hundred-millimetre cube, in ticks.
    ///
    /// Named rather than reusing `SIDE`: the geometry tests above only need a consistent
    /// scale, but a pressure is a load over a *real* area, and a hundred-tick footprint is six
    /// micrometres across. Every compressible item would crush under any load at all, and the
    /// test would run through the refusal path while claiming to measure compression.
    fn millimetres(count: i64) -> Length {
        Length(count * Length::TICKS_PER_MM)
    }

    fn compressible_cube(ratio_ppm: i64, limit_kpa: i64) -> Item {
        let mut compressible = item("s", ShapeType::Compressible, None);
        compressible.dimensions = Dimensions {
            length: millimetres(100),
            width: millimetres(100),
            height: millimetres(100),
        };
        compressible.compression_ratio_ppm = Some(ratio_ppm);
        compressible.max_compression_pressure_kpa = Some(limit_kpa);
        compressible
    }

    #[test]
    fn a_compressible_item_occupies_the_height_left_under_its_load() {
        let mut placement = placed(compressible_cube(250_000, 100), (0, 0, 0));

        // Unloaded, it is simply its box.
        assert_eq!(occupied_volume(&placement), placement.dimensions.volume());

        // Under a load inside its limit it gives up height, and only height.
        placement.top_load = Weight(4 * Weight::TICKS_PER_KG);
        let loaded = occupied_volume(&placement);
        assert!(
            loaded < placement.dimensions.volume(),
            "a compressible item under load must occupy less than its uncompressed box"
        );
        assert_eq!(
            loaded % placement.dimensions.base_area(),
            0,
            "footprint is unchanged"
        );
    }

    /// A crushed item has no meaningful occupied volume, and the arrangement is already
    /// invalid -- the crush check refuses it. Reporting the uncompressed figure keeps that one
    /// reported issue rather than adding a second, quieter one.
    #[test]
    fn a_crushed_item_reports_its_uncompressed_volume() {
        let mut placement = placed(compressible_cube(500_000, 1), (0, 0, 0));
        placement.top_load = Weight(10_000 * Weight::TICKS_PER_KG);
        assert_eq!(occupied_volume(&placement), placement.dimensions.volume());
    }

    /// `hull_collision_is_exact` is false for a route item, so the box stands even though the
    /// item is a hull -- the sequence replay reasons with box sweeps only, and packing tighter
    /// than it can verify would produce arrangements the engine then calls unloadable.
    #[test]
    fn a_hull_on_a_route_falls_back_to_its_box() {
        let mut routed = item("w", ShapeType::ConvexHull, Some(wedge()));
        routed.stop_index = Some(2);
        let placement = placed(routed, (0, 0, 0));
        assert!(placement.hull_shape().is_none());
        assert!(placement_hits_box(&placement, cube_at((60, 60, 60), 30)));
        assert_eq!(
            occupied_volume(&placement),
            SIDE as i128 * SIDE as i128 * SIDE as i128 / 2
        );
    }
}
