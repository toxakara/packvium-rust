use crate::contact_graph::ContactGraph;
use crate::geometry::Point;
use crate::model::{PackedContainer, PackingRequest, PackingResult, Placement};
use crate::units::Weight;
use crate::validation::IndependentValidator;
use std::collections::BTreeSet;

/// One atomic relocation committed by [`rebalance_weight`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WeightMove {
    pub item_id: String,
    pub from_container_id: String,
    pub to_container_id: String,
}

/// The independently validated container set produced by a rebalance pass.
#[derive(Clone, Debug)]
pub struct RebalanceResult {
    pub containers: Vec<PackedContainer>,
    pub moves: Vec<WeightMove>,
}

impl RebalanceResult {
    pub fn improved(&self) -> bool {
        !self.moves.is_empty()
    }
}

fn refreshed_top_loads(mut placements: Vec<Placement>) -> Vec<Placement> {
    let graph = ContactGraph::from_placements(&placements);
    let mut loads = vec![0_i128; placements.len()];
    let mut order = (0..placements.len()).collect::<Vec<_>>();
    order.sort_by_key(|index| std::cmp::Reverse(placements[*index].envelope_box().z2()));
    for upper_index in order {
        let upper_box = placements[upper_index].envelope_box();
        if upper_box.origin.z == 0 {
            continue;
        }
        let supports = graph.supporters(upper_index);
        let total_area = supports.iter().map(|edge| edge.area).sum::<i128>();
        if total_area == 0 {
            continue;
        }
        let downward = placements[upper_index].instance.item.weight.0 as i128 + loads[upper_index];
        let mut distributed = 0_i128;
        for (position, edge) in supports.iter().enumerate() {
            let share = if position + 1 == supports.len() {
                downward.saturating_sub(distributed)
            } else {
                downward.saturating_mul(edge.area) / total_area
            };
            distributed = distributed.saturating_add(share);
            loads[edge.index] = loads[edge.index].saturating_add(share);
        }
    }
    for (placement, load) in placements.iter_mut().zip(loads) {
        placement.top_load = Weight(load.clamp(i64::MIN as i128, i64::MAX as i128) as i64);
    }
    placements
}

fn support_ratio(existing: &[Placement], placement: &Placement) -> f64 {
    let box_ = placement.envelope_box();
    if box_.origin.z == 0 {
        return 1.0;
    }
    if placement.instance.item.nesting_height.is_none() {
        let area = existing
            .iter()
            .filter(|other| other.envelope_box().z2() == box_.origin.z)
            .map(|other| other.envelope_box().overlap_area_xy(box_))
            .sum::<i128>();
        return (area as f64 / box_.dimensions.base_area() as f64).min(1.0);
    }
    let mut placements = existing.to_vec();
    placements.push(placement.clone());
    let index = placements.len() - 1;
    ContactGraph::from_placements(&placements).support_ratio(&placements, index)
}

fn candidate_points(container: &PackedContainer) -> Vec<Point> {
    let mut points = BTreeSet::from([(0_i64, 0_i64, 0_i64)]);
    for placement in &container.placements {
        let box_ = placement.envelope_box();
        points.insert((box_.x2(), box_.origin.y, box_.origin.z));
        points.insert((box_.origin.x, box_.y2(), box_.origin.z));
        points.insert((box_.origin.x, box_.origin.y, box_.z2()));
    }
    let mut points = points
        .into_iter()
        .map(|(x, y, z)| Point { x, y, z })
        .collect::<Vec<_>>();
    points.sort_by_key(|point| (point.z, point.y, point.x));
    points
}

fn attempt_move(
    request: &PackingRequest,
    original: &PackingResult,
    containers: &[PackedContainer],
    source_index: usize,
    destination_index: usize,
    placement_index: usize,
) -> Option<Vec<PackedContainer>> {
    let source = &containers[source_index];
    let destination = &containers[destination_index];
    let moving = &source.placements[placement_index];
    let offset = Point {
        x: moving.position.x - moving.envelope_origin.x,
        y: moving.position.y - moving.envelope_origin.y,
        z: moving.position.z - moving.envelope_origin.z,
    };

    for point in candidate_points(destination) {
        let mut relocated = moving.clone();
        relocated.envelope_origin = point;
        relocated.position = Point {
            x: point.x + offset.x,
            y: point.y + offset.y,
            z: point.z + offset.z,
        };
        relocated.support_ratio = support_ratio(&destination.placements, &relocated);
        relocated.top_load = Weight(0);

        let mut trial = containers.to_vec();
        let mut source_placements = source.placements.clone();
        source_placements.remove(placement_index);
        trial[source_index].placements = refreshed_top_loads(source_placements);
        let mut destination_placements = destination.placements.clone();
        destination_placements.push(relocated);
        trial[destination_index].placements = refreshed_top_loads(destination_placements);

        let mut trial_result = original.clone();
        trial_result.containers = trial.clone();
        if IndependentValidator.validate(request, &trial_result).valid {
            return Some(trial);
        }
    }
    None
}

/// Greedily reduce payload spread without ever committing an unvalidated move.
///
/// This is an explicit post-processing pass over an existing result. Every
/// candidate uses the placement's exact envelope/orientation, and the complete
/// trial result must pass [`IndependentValidator`] before it becomes observable.
/// Therefore a rejected move cannot drop/duplicate an item or strand a supported
/// placement in the source container.
pub fn rebalance_weight(
    request: &PackingRequest,
    result: &PackingResult,
    max_moves: usize,
) -> RebalanceResult {
    let mut working = result.containers.clone();
    let mut moves = Vec::new();

    for _ in 0..max_moves {
        if working.len() < 2 {
            break;
        }
        let weights = working
            .iter()
            .map(|container| container.payload_weight().0)
            .collect::<Vec<_>>();
        let spread = weights.iter().max().unwrap() - weights.iter().min().unwrap();
        if spread <= 0 {
            break;
        }
        let source_index = (0..working.len())
            .max_by_key(|index| weights[*index])
            .unwrap();
        let mut placements = (0..working[source_index].placements.len()).collect::<Vec<_>>();
        placements.sort_by_key(|index| {
            std::cmp::Reverse(
                working[source_index].placements[*index]
                    .instance
                    .item
                    .weight
                    .0,
            )
        });
        let mut destinations = (0..working.len())
            .filter(|index| *index != source_index)
            .collect::<Vec<_>>();
        destinations.sort_by_key(|index| weights[*index]);

        let mut committed = None;
        'search: for placement_index in placements {
            if working[source_index].placements[placement_index].fixed {
                continue;
            }
            let weight = working[source_index].placements[placement_index]
                .instance
                .item
                .weight
                .0;
            if weight <= 0 {
                continue;
            }
            for destination_index in destinations.iter().copied() {
                let mut projected = weights.clone();
                projected[source_index] -= weight;
                projected[destination_index] += weight;
                if projected.iter().max().unwrap() - projected.iter().min().unwrap() >= spread {
                    continue;
                }
                if let Some(trial) = attempt_move(
                    request,
                    result,
                    &working,
                    source_index,
                    destination_index,
                    placement_index,
                ) {
                    // A move that prices the destination past its tariff is not an
                    // improvement: the sentinel must never ride out through a
                    // rebalanced packing any more than through a packed one (
                    // review). Objective-gated inside the helper, so every other
                    // objective is untouched.
                    if crate::solvers::unpriceable_container(&trial, &request.config).is_some() {
                        continue;
                    }
                    committed = Some((
                        trial,
                        WeightMove {
                            item_id: working[source_index].placements[placement_index]
                                .instance
                                .id(),
                            from_container_id: working[source_index].id(),
                            to_container_id: working[destination_index].id(),
                        },
                    ));
                    break 'search;
                }
            }
        }
        let Some((trial, move_)) = committed else {
            break;
        };
        working = trial;
        moves.push(move_);
    }
    RebalanceResult {
        containers: working,
        moves,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        AlgorithmReport, Container, Dimensions, Item, ItemInstance, PackingConfig, PackingStatus,
        Placement, Rotation,
    };
    use crate::{Length, Weight};
    use std::collections::{BTreeMap, BTreeSet};

    fn item(id: &str, weight: i64, quantity: usize) -> Item {
        Item {
            id: id.into(),
            dimensions: Dimensions {
                length: Length(10),
                width: Length(10),
                height: Length(10),
            },
            weight: Weight(weight),
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
        }
    }

    fn container() -> Container {
        Container {
            id: "box".into(),
            inner_dimensions: Dimensions {
                length: Length(30),
                width: Length(20),
                height: Length(20),
            },
            outer_dimensions: None,
            tare_weight: Weight(0),
            max_payload: None,
            cost_minor: 0,
            quantity: Some(2),
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
            preloaded: Vec::new(),
        }
    }

    fn placement(item: &Item, sequence: usize, x: i64, z: i64) -> Placement {
        Placement {
            instance: ItemInstance {
                item: item.clone(),
                sequence,
            },
            position: Point { x, y: 0, z },
            rotation: Rotation::Lwh,
            dimensions: item.dimensions,
            envelope_origin: Point { x, y: 0, z },
            envelope_dimensions: item.dimensions,
            support_ratio: 1.0,
            top_load: Weight(0),
            fixed: false,
        }
    }

    fn result(containers: Vec<PackedContainer>) -> PackingResult {
        PackingResult {
            status: PackingStatus::Feasible,
            containers,
            unpacked: Vec::new(),
            algorithm: AlgorithmReport::default(),
            score: Vec::new(),
            warnings: Vec::new(),
            alternatives: Vec::new(),
            feasibility: None,
            termination: None,
            optimality: None,
            objective: "default".into(),
            catalog_versions_used: Vec::new(),
        }
    }

    #[test]
    fn moves_a_light_item_without_changing_exact_accounting() {
        let heavy = item("heavy", 500, 1);
        let light = item("light", 100, 1);
        let alone = item("alone", 100, 1);
        let request = PackingRequest {
            items: vec![heavy.clone(), light.clone(), alone.clone()],
            containers: vec![container()],
            config: PackingConfig::default(),
            output_length_unit: "ticks".into(),
            output_weight_unit: "ticks".into(),
            catalog_versions_used: Vec::new(),
            fixed_placements: Vec::new(),
            fixed_containers: Vec::new(),
        };
        let original = result(vec![
            PackedContainer {
                container: container(),
                sequence: 1,
                placements: vec![placement(&heavy, 1, 0, 0), placement(&light, 1, 10, 0)],
                lattice_summary: None,
                lattice_items: Vec::new(),
            },
            PackedContainer {
                container: container(),
                sequence: 2,
                placements: vec![placement(&alone, 1, 0, 0)],
                lattice_summary: None,
                lattice_items: Vec::new(),
            },
        ]);

        let balanced = rebalance_weight(&request, &original, 64);

        // A cross-language fixture kept one level above this crate; a published copy
        // does not carry it, and everything above this line has already been checked.
        let shared = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../../../conformance/scene/rebalance-fixtures.json");
        let Ok(fixture_text) = std::fs::read_to_string(&shared) else {
            eprintln!(
                "skipping: the shared cross-language scene fixture is not part of this package"
            );
            return;
        };
        let fixture: serde_json::Value = serde_json::from_str(&fixture_text).unwrap();
        let expected = &fixture["cases"][0];
        assert_eq!(
            balanced.moves,
            vec![WeightMove {
                item_id: expected["expected_move"]["item_id"]
                    .as_str()
                    .unwrap()
                    .into(),
                from_container_id: expected["expected_move"]["from_container_id"]
                    .as_str()
                    .unwrap()
                    .into(),
                to_container_id: expected["expected_move"]["to_container_id"]
                    .as_str()
                    .unwrap()
                    .into(),
            }]
        );
        assert_eq!(
            balanced
                .containers
                .iter()
                .map(|packed| packed.payload_weight().0)
                .collect::<Vec<_>>(),
            vec![500, 200]
        );
        assert_eq!(
            balanced
                .containers
                .iter()
                .map(|container| {
                    container
                        .placements
                        .iter()
                        .map(|placement| placement.instance.id())
                        .collect::<Vec<_>>()
                })
                .collect::<Vec<_>>(),
            serde_json::from_value::<Vec<Vec<String>>>(
                expected["expected_container_item_ids"].clone()
            )
            .unwrap()
        );
        let mut trial = original.clone();
        trial.containers = balanced.containers;
        assert!(IndependentValidator.validate(&request, &trial).valid);
    }

    #[test]
    fn never_strands_an_item_that_was_resting_on_the_candidate() {
        let base = item("base", 500, 1);
        let mut top = item("top", 100, 1);
        top.minimum_support_ratio = 1.0;
        let request = PackingRequest {
            items: vec![base.clone(), top.clone()],
            containers: vec![container()],
            config: PackingConfig::default(),
            output_length_unit: "ticks".into(),
            output_weight_unit: "ticks".into(),
            catalog_versions_used: Vec::new(),
            fixed_placements: Vec::new(),
            fixed_containers: Vec::new(),
        };
        let original = result(vec![
            PackedContainer {
                container: container(),
                sequence: 1,
                placements: vec![placement(&base, 1, 0, 0), placement(&top, 1, 0, 10)],
                lattice_summary: None,
                lattice_items: Vec::new(),
            },
            PackedContainer {
                container: container(),
                sequence: 2,
                placements: Vec::new(),
                lattice_summary: None,
                lattice_items: Vec::new(),
            },
        ]);

        let balanced = rebalance_weight(&request, &original, 1);
        assert_eq!(balanced.moves.len(), 1);
        assert_eq!(balanced.moves[0].item_id, "top#1");
        assert!(
            balanced.containers[0]
                .placements
                .iter()
                .any(|placement| placement.instance.id() == "base#1")
        );
    }

    #[test]
    fn rebalance_helpers_preserve_nested_support_and_cumulative_loads() {
        let mut nested = item("nested", 100, 3);
        nested.nesting_height = Some(Length(5));
        let placements = vec![
            placement(&nested, 1, 0, 0),
            placement(&nested, 2, 0, 5),
            placement(&nested, 3, 0, 10),
        ];

        assert_eq!(support_ratio(&placements[..1], &placements[1]), 1.0);
        assert_eq!(
            refreshed_top_loads(placements)
                .iter()
                .map(|placement| placement.top_load.0)
                .collect::<Vec<_>>(),
            vec![200, 100, 0]
        );
    }

    #[test]
    fn randomized_weight_cases_preserve_accounting_and_never_widen_the_spread() {
        let mut seed = 0x5eed_u64;
        for _ in 0..32 {
            seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
            let light_weight = 1 + (seed % 100) as i64;
            seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
            let alone_weight = 1 + (seed % 100) as i64;
            let heavy = item("heavy", 300 + light_weight + alone_weight, 1);
            let light = item("light", light_weight, 1);
            let alone = item("alone", alone_weight, 1);
            let request = PackingRequest {
                items: vec![heavy.clone(), light.clone(), alone.clone()],
                containers: vec![container()],
                config: PackingConfig::default(),
                output_length_unit: "ticks".into(),
                output_weight_unit: "ticks".into(),
                catalog_versions_used: Vec::new(),
                fixed_placements: Vec::new(),
                fixed_containers: Vec::new(),
            };
            let original = result(vec![
                PackedContainer {
                    container: container(),
                    sequence: 1,
                    placements: vec![placement(&heavy, 1, 0, 0), placement(&light, 1, 10, 0)],
                    lattice_summary: None,
                    lattice_items: Vec::new(),
                },
                PackedContainer {
                    container: container(),
                    sequence: 2,
                    placements: vec![placement(&alone, 1, 0, 0)],
                    lattice_summary: None,
                    lattice_items: Vec::new(),
                },
            ]);
            let before = original
                .containers
                .iter()
                .map(|packed| packed.payload_weight().0)
                .collect::<Vec<_>>();
            let balanced = rebalance_weight(&request, &original, 8);
            let after = balanced
                .containers
                .iter()
                .map(|packed| packed.payload_weight().0)
                .collect::<Vec<_>>();
            assert!(
                after.iter().max().unwrap() - after.iter().min().unwrap()
                    <= before.iter().max().unwrap() - before.iter().min().unwrap()
            );
            let ids = balanced
                .containers
                .iter()
                .flat_map(|packed| packed.placements.iter())
                .map(|placement| placement.instance.id())
                .collect::<BTreeSet<_>>();
            assert_eq!(
                ids,
                BTreeSet::from([
                    "heavy#1".to_owned(),
                    "light#1".to_owned(),
                    "alone#1".to_owned()
                ])
            );
            let mut trial = original.clone();
            trial.containers = balanced.containers;
            assert!(IndependentValidator.validate(&request, &trial).valid);
        }
    }
}
