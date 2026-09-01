use super::extreme::{
    SCORE_SCALE, calculate_top_loads, container_order_key, dimensional_weight_ticks, score_solution,
};
use crate::deadline::Deadline;
use crate::geometry::{Dimensions, Point, Rotation};
use crate::model::*;
use crate::units::Weight;
use std::cmp::Reverse;

/// Ranking key for one candidate orientation: most *useful* units first --
/// capacity beyond what is being placed buys nothing -- then the stack top, then the
/// smallest envelope, then rotation name so ties resolve deterministically.
///
/// The stack top matters and the layer count does not: sixteen crates can fit as one
/// layer standing tall or two layers lying flat, and the objective's fifth key measures
/// how high the load sits. Ranking by capacity alone left that to the rotation *name*,
/// where `HLW` sorts before `LWH` and stood the item on its long edge.
type GridKey = (Reverse<usize>, i64, i128, &'static str);

/// The best orientation found so far: key, rotation, physical and envelope
/// dimensions, the per-axis counts, and the resulting capacity.
type GridChoice = (
    GridKey,
    Rotation,
    Dimensions,
    Dimensions,
    i64,
    i64,
    i64,
    usize,
);

/// How good opening this container type would be, in the objective's own key order
///. Mirrors `extreme::container_selection_key`; the lattice knows its own
/// geometry in closed form, so every term here is arithmetic rather than a trial pack.
fn grid_selection_key(
    container: &Container,
    best: &GridChoice,
    remaining: usize,
    item: &Item,
    nesting: i64,
    config: &PackingConfig,
) -> Vec<i128> {
    let (_, _, physical, envelope, nx, ny, _, capacity) = best;
    let count = (*capacity).min(remaining);
    let inner = container.inner_dimensions;
    let volume = inner.volume();
    let used = physical.volume() * count as i128;

    let containers_needed = remaining.div_ceil((*capacity).max(1)) as i128;
    let cost = container.cost_minor as i128;
    let per_layer = ((*nx as i128) * (*ny as i128)).max(1) as usize;
    let layers = count.div_ceil(per_layer);
    let top = if layers == 0 {
        0
    } else {
        envelope.height.0 + (layers as i64 - 1) * (envelope.height.0 - nesting)
    };
    let unused_ppm = if volume > 0 {
        (volume - used) * SCORE_SCALE / volume
    } else {
        0
    };
    let height_ppm = if inner.height.0 > 0 {
        top as i128 * SCORE_SCALE / inner.height.0 as i128
    } else {
        0
    };
    let billable =
        if config.objective == "shipping_cost" || config.objective == "lowest_landed_cost" {
            let payload = i128::from(item.weight.0) * count as i128;
            (payload + i128::from(container.tare_weight.0))
                .max(dimensional_weight_ticks(container, config))
        } else {
            0
        };

    let mut key = match config.objective.as_str() {
        "lowest_cost" => vec![cost, containers_needed, unused_ppm, height_ppm],
        // `lowest_landed_cost` never reaches this key: `try_grid` stands down for it
        // (see below), because a closed-form solver commits to one container with no
        // alternative to correct a mispriced proxy, and billed weight is exactly such
        // a proxy -- a bracket step or a minimum charge makes the cheaper shipment the
        // heavier one. Keeping an arm for it here would invite dropping the
        // exclusion on the strength of a comment; the exclusion is the fix.
        "shipping_cost" => vec![billable, containers_needed, unused_ppm, height_ppm],
        "open_dimension_height" => vec![top as i128, containers_needed, cost, unused_ppm],
        _ => vec![containers_needed, cost, unused_ppm, height_ppm],
    };
    // Only when nothing the objective ranks by separates the candidates: packing more
    // now cannot leave a worse remainder.
    key.push(-(count as i128));
    key
}

pub fn try_grid(request: &PackingRequest, deadline: &Deadline) -> Option<PackingResult> {
    if request.items.len() != 1
        // A rate table prices by weight bracket, and a bracket step or minimum charge
        // means cost is not monotone in the billed-weight proxy this closed-form solver
        // would otherwise rank by. Unlike the general portfolio, this solver
        // commits to one container type with no alternative to compare against, so a
        // proxy that occasionally disagrees with the real price has nothing to correct
        // it -- it can settle on an unpriceable container over a priced one before
        // pricing ever entered the decision. Falling through to the general search lets
        // multiple starts run, each priced exactly by `score_solution`.
        || request.config.objective == "lowest_landed_cost"
        || request.items[0].group.is_some()
        || !request.items[0].tags.is_empty()
        || !request.items[0].eligible_container_tags.is_empty()
        || request.items[0].max_stacked_items.is_some()
        // The lattice is closed-form over boxes: it counts cells from envelope extents and
        // caps a column from `max_top_load` arithmetic alone. Neither step can see a hull --
        // it would tile bounding boxes and call the result exact -- and neither can see
        // pressure, so a compressible column would be sized without ever asking whether its
        // bottom item survives. The general solver checks both per candidate.
        || request.items[0].shape_type != crate::geometry::ShapeType::RigidCuboid
        || !matches!(
            request.items[0].ground_contact_rule.as_deref(),
            None | Some("free")
        )
        || request.containers.iter().any(|container| {
            !container.obstacles.is_empty()
                || container.axles.is_some()
                || container.void_fill_reserve_ppm > 0
                || !container.tag_limits.is_empty()
                || container.max_stack_density.is_some()
        })
    {
        return None;
    }

    let started = deadline.now_ns();
    let item = &request.items[0];
    let mut remaining = item.quantity;
    let mut containers = Vec::new();
    let mut metrics = SolverMetrics::default();
    let mut sequence = 0;
    let nesting = item.nesting_height.unwrap_or_default().0;

    // Per-unit capacity depends only on (item, container spec), both fixed for the
    // whole solve, so it is computed once per container type rather than
    // recomputed on every unit opened. Every container type's capacity is
    // known up front so each round can pick whichever holds the most of what is
    // left, instead of exhausting one container type's entire inventory before a
    // roomier type is ever tried -- ten single-item containers of the cheapest
    // type when one larger type would have held them all.
    let mut container_order = request.containers.iter().collect::<Vec<_>>();
    container_order.sort_by_key(|container| container_order_key(container, &request.config));
    let mut capacities: Vec<(&Container, GridChoice, usize)> = container_order
        .into_iter()
        .filter_map(|container| {
            let mut best: Option<GridChoice> = None;
            for (rotation, physical) in item.dimensions.unique_rotations(&item.allowed_rotations) {
                let envelope = if request.config.clearance.0 > 0 {
                    physical.expand(request.config.clearance)
                } else {
                    physical
                };
                let nx = container.inner_dimensions.length.0 / envelope.length.0;
                let ny = container.inner_dimensions.width.0 / envelope.width.0;
                let layer_step = envelope.height.0 - nesting;
                let mut nz = if container.inner_dimensions.height.0 < envelope.height.0 {
                    0
                } else {
                    (container.inner_dimensions.height.0 - envelope.height.0) / layer_step + 1
                };
                if item.must_be_on_floor || !item.stackable {
                    nz = nz.min(1);
                }
                if let Some(maximum) = item.max_top_load
                    && item.weight.0 > 0
                {
                    nz = nz.min((maximum.0 / item.weight.0).saturating_add(1));
                }
                let mut capacity =
                    (nx as i128 * ny as i128 * nz as i128).min(usize::MAX as i128) as usize;
                if let Some(maximum) = container.max_items {
                    capacity = capacity.min(maximum);
                }
                if let Some(maximum) = container.max_payload
                    && item.weight.0 > 0
                {
                    capacity = capacity.min((maximum.0 / item.weight.0) as usize);
                }
                let useful = capacity.min(item.quantity);
                let per_layer = ((nx as i128) * (ny as i128)).max(1) as usize;
                let layers = useful.div_ceil(per_layer);
                let top = if layers == 0 {
                    0
                } else {
                    envelope.height.0 + (layers as i64 - 1) * layer_step
                };
                let key = (Reverse(useful), top, envelope.volume(), rotation.as_str());
                let replace = match &best {
                    Some((existing, ..)) => key < *existing,
                    None => true,
                };
                if replace {
                    best = Some((key, rotation, physical, envelope, nx, ny, nz, capacity));
                }
            }
            let best = best?;
            if best.7 == 0 {
                // No rotation of the item fits this container at all; this holds
                // for every unit of this container type, so it can never be
                // selected below instead of wasting the whole deadline retrying it
                //.
                return None;
            }
            let available = container.quantity.unwrap_or(usize::MAX);
            Some((container, best, available))
        })
        .collect();

    while remaining > 0 && !deadline.expired() && !effort_exhausted(request, &metrics) {
        // Rank by what the objective rewards, not by raw capacity. Leading
        // with capacity reads as "the biggest box wins" -- it minimises container count
        // and then stops caring, so under `lowest_cost` a box holding one more item beat
        // any saving and Rust paid four times what Python paid on
        // `regression-smallest-sufficient-container`, a fixture that exists to assert the
        // cheapest sufficient container is chosen. The terms below are the ones
        // `score_solution` uses, in the order that objective puts them, so the per-round
        // choice and the final score cannot disagree about "better".
        let choice = capacities
            .iter_mut()
            .filter(|(_, _, available)| *available > 0)
            .min_by_key(|(container, best, _)| {
                (
                    grid_selection_key(container, best, remaining, item, nesting, &request.config),
                    container.id.clone(),
                )
            });
        let Some((container, (_, rotation, physical, envelope, nx, ny, nz, capacity), available)) =
            choice
        else {
            break;
        };
        let (container, rotation, physical, envelope, nx, ny, nz, capacity) = (
            *container, *rotation, *physical, *envelope, *nx, *ny, *nz, *capacity,
        );
        *available -= 1;
        {
            sequence += 1;
            let clearance = request.config.clearance.0;
            let total = capacity.min(remaining);
            if total > 0 && !request.config.require_placement_coordinates && nesting == 0 {
                // Quantity-compression fast path: the lattice parameters
                // above already fully determine every coordinate in `O(r)`; skip the
                // `O(n)` loop that would otherwise build one `Placement` per instance
                // purely to fill them in. `nesting == 0` keeps this to the case
                // `used_volume`/`centre_of_mass` reduce to a plain per-item sum (see
                // `LatticeSummary`).
                let start_sequence = item.quantity - remaining + 1;
                let lattice_items = (0..total)
                    .map(|offset| ItemInstance {
                        item: item.clone(),
                        sequence: start_sequence + offset,
                    })
                    .collect::<Vec<_>>();
                let summary = LatticeSummary {
                    item_type: item.id.clone(),
                    rotation,
                    physical,
                    envelope,
                    nx,
                    ny,
                    layer_step: envelope.height.0 - nesting,
                    clearance_ticks: clearance,
                    count: total,
                    weight_ticks: item.weight.0,
                };
                remaining -= total;
                metrics.search_nodes_expanded = metrics.search_nodes_expanded.saturating_add(1);
                metrics.candidate_points_considered = metrics
                    .candidate_points_considered
                    .saturating_add(total as u64);
                metrics.orientations_considered =
                    metrics.orientations_considered.saturating_add(total as u64);
                metrics.feasible_candidates =
                    metrics.feasible_candidates.saturating_add(total as u64);
                containers.push(PackedContainer {
                    container: container.clone(),
                    sequence,
                    placements: Vec::new(),
                    lattice_summary: Some(summary),
                    lattice_items,
                });
                if request
                    .config
                    .max_containers
                    .map(|maximum| containers.len() >= maximum)
                    .unwrap_or(false)
                {
                    break;
                }
                continue;
            }
            let mut placements = Vec::new();
            let mut placed = 0;
            'outer: for z in 0..nz {
                for y in 0..ny {
                    for x in 0..nx {
                        if placed >= capacity
                            || remaining == 0
                            || deadline.expired()
                            || effort_exhausted(request, &metrics)
                        {
                            break 'outer;
                        }
                        metrics.search_nodes_expanded =
                            metrics.search_nodes_expanded.saturating_add(1);
                        metrics.candidate_points_considered =
                            metrics.candidate_points_considered.saturating_add(1);
                        metrics.orientations_considered =
                            metrics.orientations_considered.saturating_add(1);
                        metrics.feasible_candidates = metrics.feasible_candidates.saturating_add(1);
                        placed += 1;
                        let instance = ItemInstance {
                            item: item.clone(),
                            sequence: item.quantity - remaining + 1,
                        };
                        remaining -= 1;
                        let origin = Point {
                            x: x * envelope.length.0,
                            y: y * envelope.width.0,
                            z: z * (envelope.height.0 - nesting),
                        };
                        placements.push(Placement {
                            instance,
                            position: Point {
                                x: origin.x + clearance,
                                y: origin.y + clearance,
                                z: origin.z + clearance,
                            },
                            rotation,
                            dimensions: physical,
                            envelope_origin: origin,
                            envelope_dimensions: envelope,
                            support_ratio: 1.0,
                            top_load: Weight(0),
                        });
                    }
                }
            }
            if let Some(loads) = calculate_top_loads(&placements) {
                for (placement, load) in placements.iter_mut().zip(loads) {
                    placement.top_load = Weight(load.clamp(0, i64::MAX as i128) as i64);
                }
            }
            containers.push(PackedContainer {
                container: container.clone(),
                sequence,
                placements,
                lattice_summary: None,
                lattice_items: Vec::new(),
            });
            if request
                .config
                .max_containers
                .map(|maximum| containers.len() >= maximum)
                .unwrap_or(false)
            {
                break;
            }
        }
    }

    let timed_out = deadline.expired();
    let effort_limited = effort_exhausted(request, &metrics);
    let unpacked = (0..remaining)
        .map(|offset| {
            let instance = ItemInstance {
                item: item.clone(),
                sequence: item.quantity - remaining + offset + 1,
            };
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
                "container_inventory_exhausted".into()
            };
            UnpackedItem::new(instance, reason, Vec::new())
        })
        .collect::<Vec<_>>();
    let complete = unpacked.is_empty();
    let score = score_solution(&containers, &unpacked, &request.config);
    Some(PackingResult {
        status: if complete {
            PackingStatus::Feasible
        } else if timed_out {
            PackingStatus::TimeLimit
        } else {
            PackingStatus::BestFound
        },
        containers,
        unpacked,
        algorithm: AlgorithmReport {
            profile: request.config.profile.as_str().into(),
            solver: "grid".into(),
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
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::deadline::Clock;
    use crate::units::Length;
    use std::collections::{BTreeMap, BTreeSet};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};

    #[derive(Debug)]
    struct CheckBudgetClock(AtomicU64);

    impl Clock for CheckBudgetClock {
        fn now_ns(&self) -> u64 {
            self.0.fetch_add(1_000_000, Ordering::SeqCst)
        }
    }

    fn request(quantity: usize) -> PackingRequest {
        PackingRequest {
            items: vec![Item {
                id: "cube".into(),
                dimensions: Dimensions {
                    length: Length(10),
                    width: Length(10),
                    height: Length(10),
                },
                weight: Weight(0),
                quantity,
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
                shape_type: crate::geometry::ShapeType::RigidCuboid,
                hull_vertices: None,
                compression_ratio_ppm: None,
                max_compression_pressure_kpa: None,
                eligible_container_tags: BTreeSet::new(),
            }],
            containers: vec![Container {
                id: "container".into(),
                inner_dimensions: Dimensions {
                    length: Length(100),
                    width: Length(100),
                    height: Length(100),
                },
                outer_dimensions: None,
                tare_weight: Weight(0),
                max_payload: None,
                cost_minor: 0,
                quantity: None,
                obstacles: Vec::new(),
                tags: BTreeSet::new(),
                max_items: None,
                metadata: BTreeMap::new(),
                axles: None,
                void_fill_reserve_ppm: 0,
                tag_limits: BTreeMap::new(),
                max_stack_density: None,
                rate_table: None,
            }],
            config: PackingConfig {
                profile: SolverProfile::Fast,
                ..PackingConfig::default()
            },
            output_length_unit: "mm".into(),
            output_weight_unit: "g".into(),
            catalog_versions_used: Vec::new(),
        }
    }

    fn fake_deadline(limit_ms: u64) -> Deadline {
        Deadline::with_clock(limit_ms, Arc::new(CheckBudgetClock(AtomicU64::new(0))))
    }

    #[test]
    fn an_injected_clock_can_expire_before_the_first_placement() {
        let result = try_grid(&request(8), &fake_deadline(1)).expect("grid should apply");
        assert!(result.algorithm.time_limit_reached);
        assert!(result.containers.is_empty());
        assert_eq!(result.unpacked.len(), 8);
    }

    #[test]
    fn an_injected_clock_can_expire_mid_search() {
        let result = try_grid(&request(8), &fake_deadline(6)).expect("grid should apply");
        let packed = result
            .containers
            .iter()
            .map(|container| container.placements.len())
            .sum::<usize>();
        assert!(result.algorithm.time_limit_reached);
        assert!(packed > 0 && packed < 8);
        assert_eq!(packed + result.unpacked.len(), 8);
    }

    fn no_deadline() -> Deadline {
        Deadline::with_clock(60_000, Arc::new(CheckBudgetClock(AtomicU64::new(0))))
    }

    /// A container type that cannot fit the item at all (here, a 1-unit
    /// container half the item's own edge length) must be skipped in `O(1)`, not
    /// retried `available` times until the deadline expires with nothing packed.
    #[test]
    fn an_infeasible_container_type_is_skipped_instead_of_spinning_until_the_deadline() {
        let mut req = request(8);
        req.containers.insert(
            0,
            Container {
                id: "too-small".into(),
                inner_dimensions: Dimensions {
                    length: Length(5),
                    width: Length(5),
                    height: Length(5),
                },
                outer_dimensions: None,
                tare_weight: Weight(0),
                max_payload: None,
                cost_minor: 0,
                quantity: None,
                obstacles: Vec::new(),
                tags: BTreeSet::new(),
                max_items: None,
                metadata: BTreeMap::new(),
                axles: None,
                void_fill_reserve_ppm: 0,
                tag_limits: BTreeMap::new(),
                max_stack_density: None,
                rate_table: None,
            },
        );
        let result = try_grid(&req, &no_deadline()).expect("grid should apply");
        assert!(!result.algorithm.time_limit_reached);
        assert!(result.unpacked.is_empty());
        assert_eq!(
            result
                .containers
                .iter()
                .map(PackedContainer::placement_count)
                .sum::<usize>(),
            8
        );
        assert!(
            result
                .containers
                .iter()
                .all(|c| c.container.id != "too-small"),
            "the infeasible container type must never be opened"
        );
    }

    /// A small, cheap container that can only hold one item must not be
    /// preferred, one unit at a time, over a larger available container that could
    /// hold every remaining item in a single opening.
    #[test]
    fn the_container_holding_the_most_remaining_items_is_preferred_over_the_cheapest() {
        let mut req = request(10);
        req.containers.insert(
            0,
            Container {
                id: "small".into(),
                inner_dimensions: Dimensions {
                    length: Length(10),
                    width: Length(10),
                    height: Length(11),
                },
                outer_dimensions: None,
                tare_weight: Weight(0),
                max_payload: None,
                cost_minor: 0,
                quantity: None,
                obstacles: Vec::new(),
                tags: BTreeSet::new(),
                max_items: None,
                metadata: BTreeMap::new(),
                axles: None,
                void_fill_reserve_ppm: 0,
                tag_limits: BTreeMap::new(),
                max_stack_density: None,
                rate_table: None,
            },
        );
        let result = try_grid(&req, &no_deadline()).expect("grid should apply");
        assert!(result.unpacked.is_empty());
        assert_eq!(
            result.containers.len(),
            1,
            "all ten items must share one container"
        );
        assert_eq!(result.containers[0].container.id, "container");
    }

    /// (a) Default behavior (`require_placement_coordinates` unset, i.e. `true` via
    /// `PackingConfig::default()`) must still materialize one `Placement` per item
    /// and carry no `lattice_summary` -- strict backward compatibility.
    #[test]
    fn default_config_still_materializes_every_placement() {
        let result = try_grid(&request(8), &no_deadline()).expect("grid should apply");
        assert_eq!(result.containers.len(), 1);
        let container = &result.containers[0];
        assert!(container.lattice_summary.is_none());
        assert_eq!(container.placements.len(), 8);
        assert_eq!(container.placement_count(), 8);
    }

    /// (b) The compact fast path must not materialize a per-item `Placement` for a
    /// large quantity: `placements` stays empty and a `lattice_summary` carries the
    /// same information in `O(1)`/`O(r)` instead of `O(n)`.
    #[test]
    fn compact_fast_path_builds_no_per_item_placements_for_a_large_quantity() {
        let mut compact_request = request(10_000);
        compact_request.config.require_placement_coordinates = false;
        let result = try_grid(&compact_request, &no_deadline()).expect("grid should apply");
        assert!(result.complete());
        let total_count = result
            .containers
            .iter()
            .map(PackedContainer::placement_count)
            .sum::<usize>();
        assert_eq!(total_count, 10_000);
        for container in &result.containers {
            assert!(
                container.placements.is_empty(),
                "compact path must not materialize per-item placements"
            );
            assert!(container.lattice_summary.is_some());
        }
        // Sanity bound: the in-memory placement vectors collectively hold nothing,
        // regardless of how many of the 10,000 items were requested -- the compact
        // representation is bounded by the number of containers used (a handful of
        // `LatticeSummary`s), not by item count.
        let summaries = result.containers.len();
        assert!(summaries < 10_000);
    }

    /// (c) Cross-check: expanding a `LatticeSummary` (the same reconstruction
    /// `IndependentValidator` relies on) must reproduce the exact coordinates a full
    /// non-compressed run of the identical request would have produced.
    #[test]
    fn expanding_the_compact_form_matches_full_materialization() {
        let quantity = 37; // spans more than one partial layer for this container size
        let full = try_grid(&request(quantity), &no_deadline()).expect("grid should apply");
        let mut compact_request = request(quantity);
        compact_request.config.require_placement_coordinates = false;
        let compact = try_grid(&compact_request, &no_deadline()).expect("grid should apply");

        assert_eq!(full.containers.len(), compact.containers.len());
        for (full_container, compact_container) in full.containers.iter().zip(&compact.containers) {
            let summary = compact_container
                .lattice_summary
                .as_ref()
                .expect("compact run should have produced a lattice summary");
            let expanded = summary.expand(&compact_container.lattice_items);
            assert_eq!(expanded.len(), full_container.placements.len());
            for (expected, actual) in full_container.placements.iter().zip(&expanded) {
                assert_eq!(expected.position, actual.position);
                assert_eq!(expected.rotation, actual.rotation);
                assert_eq!(expected.dimensions, actual.dimensions);
                assert_eq!(expected.envelope_origin, actual.envelope_origin);
                assert_eq!(expected.envelope_dimensions, actual.envelope_dimensions);
            }
            assert_eq!(
                full_container.used_volume(),
                compact_container.used_volume()
            );
            assert_eq!(
                full_container.max_z_ticks(),
                compact_container.max_z_ticks()
            );
            assert_eq!(
                full_container.centre_of_mass_offset_ppm(),
                compact_container.centre_of_mass_offset_ppm()
            );
            assert_eq!(
                full_container.payload_weight(),
                compact_container.payload_weight()
            );
        }
    }

    /// The fast path must not engage when the item declares `nesting_height`: nesting
    /// overlap bookkeeping is not part of the closed-form aggregates, so the
    /// ordinary materializing loop must still run regardless of the config flag.
    #[test]
    fn compact_fast_path_is_skipped_when_nesting_height_is_set() {
        let mut compact_request = request(6);
        compact_request.config.require_placement_coordinates = false;
        compact_request.items[0].nesting_height = Some(Length(2));
        let result = try_grid(&compact_request, &no_deadline()).expect("grid should apply");
        for container in &result.containers {
            assert!(container.lattice_summary.is_none());
            assert!(!container.placements.is_empty());
        }
    }

    #[test]
    fn nested_grid_caps_cumulative_top_load_and_reports_the_direct_load() {
        let mut req = request(3);
        req.items[0].dimensions = Dimensions {
            length: Length(50),
            width: Length(50),
            height: Length(50),
        };
        req.items[0].weight = Weight(Weight::TICKS_PER_KG);
        req.items[0].max_top_load = Some(Weight(Weight::TICKS_PER_KG * 3 / 2));
        req.items[0].nesting_height = Some(Length(25));
        req.containers[0].inner_dimensions = Dimensions {
            length: Length(50),
            width: Length(50),
            height: Length(150),
        };
        req.containers[0].quantity = Some(1);
        req.config.max_containers = Some(1);

        let result = try_grid(&req, &no_deadline()).expect("nested grid should apply");
        assert_eq!(result.containers.len(), 1);
        assert_eq!(result.containers[0].placements.len(), 2);
        assert_eq!(result.unpacked.len(), 1);
        assert_eq!(
            result.containers[0]
                .placements
                .iter()
                .map(|placement| placement.envelope_origin.z)
                .collect::<Vec<_>>(),
            vec![0, 25]
        );
        assert_eq!(
            result.containers[0]
                .placements
                .iter()
                .map(|placement| placement.top_load)
                .collect::<Vec<_>>(),
            vec![Weight(Weight::TICKS_PER_KG), Weight(0)]
        );
    }
}
