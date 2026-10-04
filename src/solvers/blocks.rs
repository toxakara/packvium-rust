use super::extreme::{dimensional_weight_ticks, score_solution};
use super::maximal::{Space, subtract_all};
use crate::deadline::Deadline;
use crate::geometry::{Aabb, Dimensions, Point, Rotation};
use crate::model::*;
use crate::solver::PlacementConstraint;
use crate::units::{Length, Weight};
use std::collections::BTreeMap;
use std::sync::Arc;

#[derive(Clone)]
struct Block {
    space: Space,
    item_id: String,
    rotation: Rotation,
    physical: Dimensions,
    envelope: Dimensions,
    nx: i64,
    ny: i64,
    nz: i64,
    count: usize,
    key: (
        i128,
        i128,
        i128,
        i64,
        i64,
        i64,
        String,
        &'static str,
        i64,
        i64,
        i64,
    ),
}

/// Deterministic solid single-type block search for unrestricted carton loading.
///
/// Candidate enumeration is O(B*S*T*R*X*Y) time and O(S+n) space. B is committed
/// blocks, S the capped maximal-space count, T item types, R rotations, and X/Y
/// grid extents. Deadline, effort budget and `container_plan_node_limit` bound the
/// product explicitly.
pub(crate) fn pack_homogeneous_blocks(
    request: &PackingRequest,
    items: &[ItemInstance],
    constraints: &[Arc<dyn PlacementConstraint>],
    deadline: &Deadline,
) -> Option<PackingResult> {
    if !supports(request, constraints) {
        return None;
    }
    let started = deadline.now_ns();
    let mut remaining = items.to_vec();
    let mut packed = Vec::new();
    let mut metrics = SolverMetrics::default();
    let mut inventory = request
        .containers
        .iter()
        .map(|container| (container.id.clone(), container.quantity))
        .collect::<BTreeMap<_, _>>();
    let maximum = request.config.max_containers.unwrap_or_else(|| {
        request
            .containers
            .iter()
            .map(|container| container.quantity.unwrap_or(items.len()))
            .sum()
    });

    while !remaining.is_empty() && packed.len() < maximum && !deadline.expired() {
        let mut trials = Vec::new();
        let mut ordered = request.containers.iter().collect::<Vec<_>>();
        ordered.sort_by_key(|container| {
            (
                container.cost_minor,
                container.inner_dimensions.volume(),
                container.id.clone(),
            )
        });
        for container in ordered {
            if inventory.get(&container.id).copied().flatten() == Some(0) {
                continue;
            }
            let (candidate, next, reached) = pack_one(
                container,
                packed.len() + 1,
                &remaining,
                request,
                deadline,
                &mut metrics,
            );
            if candidate.placements.is_empty() {
                continue;
            }
            let unused = container.inner_dimensions.volume() - candidate.used_volume();
            // Under `lowest_landed_cost` the round ranks in the money the finished
            // score will charge: the block loader commits this trial verbatim,
            // so its billed weight is final here and the tariff can simply be read. An
            // unpriceable trial sorts behind every priceable alternative instead of
            // winning on progress. Zero for every other objective, leaving the
            // progress-first key below unchanged.
            let landed = if request.config.objective == "lowest_landed_cost" {
                let dimensional = dimensional_weight_ticks(container, &request.config);
                let billed = i128::from(candidate.gross_weight().0).max(dimensional);
                container
                    .rate_table
                    .as_ref()
                    .and_then(|table| table.charge_minor(RateTable::grams(billed as i64)))
                    .map_or(i128::MAX, i128::from)
            } else {
                0
            };
            trials.push((
                landed,
                next.len(),
                container.cost_minor,
                unused,
                container.id.clone(),
                candidate,
                next,
                reached,
            ));
            if deadline.expired() {
                break;
            }
        }
        let Some((_, _, _, _, id, candidate, next, _)) = trials
            .into_iter()
            .min_by_key(|trial| (trial.0, trial.1, trial.2, trial.3, trial.4.clone()))
        else {
            break;
        };
        if let Some(Some(quantity)) = inventory.get_mut(&id) {
            *quantity = quantity.saturating_sub(1);
        }
        packed.push(candidate);
        remaining = next;
    }

    let timed_out = deadline.expired();
    let unpacked = remaining
        .into_iter()
        .map(|instance| {
            UnpackedItem::new(
                instance,
                if timed_out {
                    "time_limit".into()
                } else {
                    "search_exhausted".into()
                },
                Vec::new(),
            )
        })
        .collect::<Vec<_>>();
    let score = score_solution(&packed, &unpacked, &request.config);
    let effort_limited = effort_exhausted(request, &metrics);
    Some(PackingResult {
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
            solver: "homogeneous_blocks".into(),
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

fn supports(request: &PackingRequest, constraints: &[Arc<dyn PlacementConstraint>]) -> bool {
    constraints.is_empty()
        // A block set on a smaller one overhangs it, so a request that asks for support
        // is left to the per-item search.
        && request.config.minimum_support_ratio == 0.0
        && request.fixed_containers.is_empty()
        && request.containers.iter().all(|container| {
            container.obstacles.is_empty()
                && container.axles.is_none()
                && container.tag_limits.is_empty()
                && container.max_stack_density.is_none()
                && container.void_fill_reserve_ppm == 0
        })
        && request.items.iter().all(|item| {
            item.group.is_none()
                && item.tags.is_empty()
                && item.incompatible_tags.is_empty()
                && item.eligible_container_tags.is_empty()
                && item.stackable
                && !item.must_be_on_floor
                && item.max_top_load.is_none()
                && item.max_stacked_items.is_none()
                && item.minimum_support_ratio == 0.0
                && matches!(item.ground_contact_rule.as_deref(), None | Some("free"))
                && item.nesting_height.is_none()
                && item.stop_index.is_none()
        })
}

/// The share of a member's base that rests on the floor or on a box beneath it.
fn member_support(placements: &[Placement], origin: Point, envelope: Dimensions) -> f64 {
    if origin.z == 0 {
        return 1.0;
    }
    let base = Aabb {
        origin,
        dimensions: envelope,
    };
    let area = placements
        .iter()
        .map(Placement::envelope_box)
        .filter(|below| below.z2() == origin.z)
        .map(|below| below.overlap_area_xy(base))
        .sum::<i128>();
    (area as f64 / envelope.base_area() as f64).min(1.0)
}

fn pack_one(
    container: &Container,
    sequence: usize,
    items: &[ItemInstance],
    request: &PackingRequest,
    deadline: &Deadline,
    metrics: &mut SolverMetrics,
) -> (PackedContainer, Vec<ItemInstance>, bool) {
    let mut best = None;
    let mut reached = false;
    for volume_first in [false, true] {
        if deadline.expired() {
            reached = true;
            break;
        }
        let (candidate, remaining, limited) = pack_mode(
            container,
            sequence,
            items,
            request,
            deadline,
            metrics,
            volume_first,
        );
        reached |= limited;
        let signature = candidate
            .placements
            .iter()
            .map(|placement| {
                (
                    placement.instance.id(),
                    placement.envelope_origin.z,
                    placement.envelope_origin.y,
                    placement.envelope_origin.x,
                )
            })
            .collect::<Vec<_>>();
        let max_z = candidate
            .placements
            .iter()
            .map(|placement| placement.envelope_box().z2())
            .max()
            .unwrap_or(0);
        let key = (remaining.len(), -candidate.used_volume(), max_z, signature);
        if best.as_ref().is_none_or(|(best_key, _, _)| key < *best_key) {
            best = Some((key, candidate, remaining));
        }
    }
    let (_, packed, remaining) = best.unwrap_or_else(|| {
        let empty = PackedContainer {
            container: container.clone(),
            sequence,
            placements: Vec::new(),
            lattice_summary: None,
            lattice_items: Vec::new(),
        };
        ((items.len(), 0, 0, Vec::new()), empty, items.to_vec())
    });
    (packed, remaining, reached)
}

fn pack_mode(
    container: &Container,
    sequence: usize,
    items: &[ItemInstance],
    request: &PackingRequest,
    deadline: &Deadline,
    metrics: &mut SolverMetrics,
    volume_first: bool,
) -> (PackedContainer, Vec<ItemInstance>, bool) {
    let mut packed = PackedContainer {
        container: container.clone(),
        sequence,
        placements: Vec::new(),
        lattice_summary: None,
        lattice_items: Vec::new(),
    };
    let mut spaces = vec![Space(Aabb {
        origin: Point::ZERO,
        dimensions: container.inner_dimensions,
    })];
    let mut remaining = BTreeMap::<String, Vec<ItemInstance>>::new();
    for item in items {
        remaining
            .entry(item.item.id.clone())
            .or_default()
            .push(item.clone());
    }
    let mut nodes = 0_usize;
    let mut reached = false;
    while !spaces.is_empty() && remaining.values().any(|values| !values.is_empty()) {
        let mut best: Option<Block> = None;
        'spaces: for space in &spaces {
            let space_volume = space.0.dimensions.volume();
            for (item_id, available) in &remaining {
                let Some(prototype) = available.first() else {
                    continue;
                };
                let mut capacity = available.len();
                if let Some(max_items) = container.max_items {
                    capacity = capacity.min(max_items.saturating_sub(packed.placements.len()));
                }
                if let Some(max_payload) = container.max_payload
                    && prototype.item.weight.0 > 0
                {
                    let payload = packed
                        .placements
                        .iter()
                        .map(|p| p.instance.item.weight.0)
                        .sum::<i64>();
                    capacity = capacity.min(
                        max_payload.0.saturating_sub(payload).max(0) as usize
                            / prototype.item.weight.0 as usize,
                    );
                }
                if capacity == 0 {
                    continue;
                }
                for (rotation, physical) in prototype
                    .item
                    .dimensions
                    .unique_rotations(&prototype.item.allowed_rotations)
                {
                    let envelope = physical.expand(request.config.clearance);
                    let maximum_x = space.0.dimensions.length.0 / envelope.length.0;
                    let maximum_y = space.0.dimensions.width.0 / envelope.width.0;
                    let maximum_z = space.0.dimensions.height.0 / envelope.height.0;
                    for nx in 1..=maximum_x.min(capacity as i64) {
                        let maximum_ny = maximum_y.min(capacity as i64 / nx);
                        for ny in 1..=maximum_ny {
                            if nodes >= request.config.container_plan_node_limit
                                || deadline.expired()
                                || effort_exhausted(request, metrics)
                            {
                                reached = true;
                                break 'spaces;
                            }
                            nodes += 1;
                            metrics.search_nodes_expanded =
                                metrics.search_nodes_expanded.saturating_add(1);
                            let nz = maximum_z.min(capacity as i64 / (nx * ny));
                            if nz <= 0 {
                                continue;
                            }
                            let count = (nx * ny * nz) as usize;
                            let used = count as i128 * physical.volume();
                            let fill = used * 1_000_000 / space_volume;
                            let lead = if volume_first {
                                (-used, -fill, -(count as i128))
                            } else {
                                (-(count as i128), -fill, -used)
                            };
                            let key = (
                                lead.0,
                                lead.1,
                                lead.2,
                                space.0.origin.z,
                                space.0.origin.y,
                                space.0.origin.x,
                                item_id.clone(),
                                rotation.as_str(),
                                nx,
                                ny,
                                nz,
                            );
                            let candidate = Block {
                                space: *space,
                                item_id: item_id.clone(),
                                rotation,
                                physical,
                                envelope,
                                nx,
                                ny,
                                nz,
                                count,
                                key,
                            };
                            if best
                                .as_ref()
                                .is_none_or(|current| candidate.key < current.key)
                            {
                                best = Some(candidate);
                            }
                        }
                    }
                }
            }
        }
        let Some(block) = best else { break };
        if reached {
            break;
        }
        let available = remaining
            .get_mut(&block.item_id)
            .expect("candidate item exists");
        let chosen = available.drain(..block.count).collect::<Vec<_>>();
        let clearance = request.config.clearance.0;
        let mut index = 0;
        for z in 0..block.nz {
            for y in 0..block.ny {
                for x in 0..block.nx {
                    let origin = Point {
                        x: block.space.0.origin.x + x * block.envelope.length.0,
                        y: block.space.0.origin.y + y * block.envelope.width.0,
                        z: block.space.0.origin.z + z * block.envelope.height.0,
                    };
                    packed.placements.push(Placement {
                        instance: chosen[index].clone(),
                        position: Point {
                            x: origin.x + clearance,
                            y: origin.y + clearance,
                            z: origin.z + clearance,
                        },
                        rotation: block.rotation,
                        dimensions: block.physical,
                        envelope_origin: origin,
                        envelope_dimensions: block.envelope,
                        // Above its bottom layer a member rests on an identical
                        // footprint; the bottom layer rests on whatever is there.
                        support_ratio: if z == 0 {
                            member_support(&packed.placements, origin, block.envelope)
                        } else {
                            1.0
                        },
                        top_load: Weight(0),
                        fixed: false,
                    });
                    index += 1;
                    metrics.feasible_candidates = metrics.feasible_candidates.saturating_add(1);
                    metrics.orientations_considered =
                        metrics.orientations_considered.saturating_add(1);
                }
            }
        }
        let occupied = Aabb {
            origin: block.space.0.origin,
            dimensions: Dimensions {
                length: Length(block.nx * block.envelope.length.0),
                width: Length(block.ny * block.envelope.width.0),
                height: Length(block.nz * block.envelope.height.0),
            },
        };
        spaces = subtract_all(spaces, occupied, metrics);
    }
    let remaining = remaining.into_values().flatten().collect();
    (packed, remaining, reached || deadline.expired())
}

#[cfg(test)]
mod tests {
    use crate::pack_json;

    fn request(node_limit: usize, stackable: bool) -> String {
        serde_json::json!({
            "units": {"length": "mm"},
            "configuration": {
                "solver_profile": "quality",
                "time_limit_ms": 5_000,
                "solvers": ["homogeneous_blocks"],
                "container_plan_beam_width": 16,
                "container_plan_node_limit": node_limit
            },
            "items": [
                {"id":"large","quantity":8,"dimensions":{"length":"100","width":"100","height":"100"},"weight":{"value":"1","unit":"g"},"stackable":stackable},
                {"id":"small","quantity":20,"dimensions":{"length":"60","width":"50","height":"40"},"weight":{"value":"1","unit":"g"}}
            ],
            "containers": [{"id":"box","quantity":1,"inner_dimensions":{"length":"300","width":"200","height":"200"},"max_payload":{"value":"1000","unit":"g"}}]
        }).to_string()
    }

    /// One 60x60x40 base and three 40x60x50 tops in a 100 mm crate: the base ends on two
    /// tops and overhangs them.
    fn a_base_and_three_tops(minimum_support_ratio: f64) -> serde_json::Value {
        let request = serde_json::json!({
            "units": {"length": "mm"},
            "configuration": {
                "solver_profile": "quality",
                "solvers": ["homogeneous_blocks"],
                "time_limit_ms": 60_000,
                "effort_budget": {"max_search_nodes": 100_000},
                "minimum_support_ratio": minimum_support_ratio
            },
            "items": [
                {"id":"base","quantity":1,"dimensions":{"length":"60","width":"60","height":"40"}},
                {"id":"top","quantity":3,"dimensions":{"length":"40","width":"60","height":"50"}}
            ],
            "containers": [{"id":"crate","quantity":1,"inner_dimensions":{"length":"100","width":"100","height":"100"}}]
        })
        .to_string();
        serde_json::from_str(&pack_json(&request).expect("solve")).unwrap()
    }

    /// Each placement's reported support next to the share of its base that rests on the
    /// floor or on a top face, from geometry alone.
    fn reported_and_resting_support(result: &serde_json::Value) -> Vec<(f64, f64)> {
        let ticks = |value: &serde_json::Value| value["ticks"].as_i64().unwrap();
        let boxes = result["containers"][0]["placements"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| {
                let (x, y, z) = (
                    ticks(&p["position"]["x"]),
                    ticks(&p["position"]["y"]),
                    ticks(&p["position"]["z"]),
                );
                let reported = p["support_ratio"].as_str().unwrap().parse::<f64>().unwrap();
                (
                    [x, y, z],
                    [
                        x + ticks(&p["dimensions"]["length"]),
                        y + ticks(&p["dimensions"]["width"]),
                        z + ticks(&p["dimensions"]["height"]),
                    ],
                    reported,
                )
            })
            .collect::<Vec<_>>();
        boxes
            .iter()
            .map(|(low, high, reported)| {
                if low[2] == 0 {
                    return (*reported, 1.0);
                }
                let resting = boxes
                    .iter()
                    .filter(|(_, below, _)| below[2] == low[2])
                    .map(|(other_low, other_high, _)| {
                        (high[0].min(other_high[0]) - low[0].max(other_low[0])).max(0)
                            * (high[1].min(other_high[1]) - low[1].max(other_low[1])).max(0)
                    })
                    .sum::<i64>();
                let area = (high[0] - low[0]) * (high[1] - low[1]);
                (*reported, resting as f64 / area as f64)
            })
            .collect()
    }

    #[test]
    fn a_block_set_on_a_smaller_one_reports_the_support_it_really_has() {
        let result = a_base_and_three_tops(0.0);
        let support = reported_and_resting_support(&result);
        assert_eq!(
            support.iter().filter(|(_, resting)| *resting < 1.0).count(),
            1
        );
        assert!(
            support
                .iter()
                .any(|(reported, resting)| (*reported - 5.0 / 6.0).abs() < 1e-6
                    && (*resting - 5.0 / 6.0).abs() < 1e-6)
        );
        for (reported, resting) in support {
            assert!(
                (reported - resting).abs() < 1e-6,
                "{reported} reported, {resting} resting"
            );
        }
    }

    #[test]
    fn the_block_solver_leaves_a_request_that_asks_for_support_to_the_per_item_search() {
        let result = a_base_and_three_tops(1.0);
        let support = reported_and_resting_support(&result);
        assert!(!support.is_empty());
        assert!(support.iter().all(|(_, resting)| *resting == 1.0));
    }

    #[test]
    fn more_counted_block_effort_cannot_worsen_the_objective() {
        let low: serde_json::Value =
            serde_json::from_str(&pack_json(&request(1, true)).expect("low effort solve")).unwrap();
        let high: serde_json::Value =
            serde_json::from_str(&pack_json(&request(100_000, true)).expect("high effort solve"))
                .unwrap();
        let score = |value: &serde_json::Value| {
            value["score"]
                .as_array()
                .unwrap()
                .iter()
                .map(|part| part.as_i64().unwrap())
                .collect::<Vec<_>>()
        };
        assert!(score(&high) <= score(&low));
    }

    #[test]
    fn an_explicit_block_solver_falls_back_for_distinguishing_rules() {
        let value: serde_json::Value =
            serde_json::from_str(&pack_json(&request(100_000, false)).expect("fallback solve"))
                .unwrap();
        assert!(value["summary"]["packed_item_count"].as_u64().unwrap() > 0);
        assert!(
            value["algorithm"]["solver"]
                .as_str()
                .unwrap()
                .starts_with("homogeneous_blocks:fallback")
        );
    }

    #[test]
    fn landed_cost_block_round_chooses_a_priceable_container() {
        let request = serde_json::json!({
            "units": {"length": "mm"},
            "configuration": {
                "objective": "lowest_landed_cost",
                "dimensional_weight_divisor": 5000,
                "dimensional_weight_length_unit": "cm",
                "dimensional_weight_weight_unit": "kg",
                "time_limit_ms": 5_000,
                "solvers": ["homogeneous_blocks"],
                "container_plan_beam_width": 16,
                "container_plan_node_limit": 1_000_000
            },
            "items": [{
                "id": "box",
                "quantity": 8,
                "weight": "500 g",
                "dimensions": {"length": "100", "width": "100", "height": "100"}
            }],
            "containers": [
                {
                    "id": "alpha_unpriceable",
                    "inner_dimensions": {"length": "300", "width": "300", "height": "300"},
                    "rate_table": {"weight_brackets_g": [2_000], "prices_minor": [900]}
                },
                {
                    "id": "beta_priceable",
                    "inner_dimensions": {"length": "400", "width": "400", "height": "400"},
                    "rate_table": {"weight_brackets_g": [20_000], "prices_minor": [1_500]}
                }
            ]
        });
        let value: serde_json::Value = serde_json::from_str(
            &pack_json(&request.to_string()).expect("landed-cost block solve"),
        )
        .unwrap();

        assert_eq!(value["containers"][0]["container_type"], "beta_priceable");
        assert_eq!(value["score"][1], 1_500);
    }
}
