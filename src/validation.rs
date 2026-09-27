use crate::contact_graph::ContactGraph;
use crate::geometry::{Aabb, Point};
use crate::model::*;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug)]
pub struct ValidationIssue {
    pub code: String,
    pub message: String,
}

#[derive(Clone, Debug)]
pub struct ValidationReport {
    pub valid: bool,
    pub issues: Vec<ValidationIssue>,
}

#[derive(Clone, Debug, Default)]
pub struct IndependentValidator;

impl IndependentValidator {
    pub fn validate(&self, request: &PackingRequest, result: &PackingResult) -> ValidationReport {
        let expected = request
            .instances()
            .iter()
            .map(ItemInstance::id)
            .collect::<BTreeSet<_>>();
        let (mut issues, mut seen) = self.check_containers(request, &result.containers);
        for unpacked in &result.unpacked {
            if !seen.insert(unpacked.instance.id()) {
                issue(&mut issues, "duplicate_accounting", &unpacked.instance.id());
            }
        }
        if seen != expected {
            issue(
                &mut issues,
                "item_accounting",
                "packed and unpacked items do not match request",
            );
        }
        ValidationReport {
            valid: issues.is_empty(),
            issues,
        }
    }

    /// Every rule that holds container by container, without item accounting: what a
    /// fixed set must satisfy on its own, before the free items are placed.
    pub fn validate_containers(
        &self,
        request: &PackingRequest,
        containers: &[PackedContainer],
    ) -> ValidationReport {
        let (issues, _) = self.check_containers(request, containers);
        ValidationReport {
            valid: issues.is_empty(),
            issues,
        }
    }

    fn check_containers(
        &self,
        request: &PackingRequest,
        containers: &[PackedContainer],
    ) -> (Vec<ValidationIssue>, BTreeSet<String>) {
        let mut issues = Vec::new();
        let mut seen = BTreeSet::new();
        let mut inventory = BTreeMap::<String, usize>::new();
        let mut groups = BTreeMap::<String, String>::new();

        for packed in containers {
            *inventory.entry(packed.container.id.clone()).or_default() += 1;
            if let Some(maximum) = packed.container.quantity
                && inventory[&packed.container.id] > maximum
            {
                issue(
                    &mut issues,
                    "container_inventory",
                    "container quantity exceeded",
                );
            }
            let boundary = Aabb {
                origin: Point::ZERO,
                dimensions: packed.container.inner_dimensions,
            };
            let support_graph = ContactGraph::from_placements(&packed.placements);
            let mut payload = 0_i64;
            let mut tag_counts = BTreeMap::<String, usize>::new();
            // Without a tag in the container no pair can be incompatible, so the common case
            // skips the pairwise scan and stays one pass.
            let compatibility_sensitive = packed.placements.iter().any(|placement| {
                !placement.instance.item.tags.is_empty()
                    || !placement.instance.item.incompatible_tags.is_empty()
            });
            for (index, placement) in packed.placements.iter().enumerate() {
                let id = placement.instance.id();
                if !seen.insert(id.clone()) {
                    issue(&mut issues, "duplicate_item", &id);
                }
                if let Some(group) = &placement.instance.item.group
                    && let Some(existing) = groups.insert(group.clone(), packed.id())
                    && existing != packed.id()
                {
                    issue(&mut issues, "group_split", group);
                }
                payload = payload.saturating_add(placement.instance.item.weight.0);
                if !placement.instance.item.eligible_container_tags.is_empty()
                    && placement
                        .instance
                        .item
                        .eligible_container_tags
                        .is_disjoint(&packed.container.tags)
                {
                    issue(&mut issues, "container_ineligible", &id);
                }
                for tag in &placement.instance.item.tags {
                    *tag_counts.entry(tag.clone()).or_default() += 1;
                }
                if !boundary.contains(placement.envelope_box()) {
                    issue(&mut issues, "outside_container", &id);
                }
                if !placement
                    .instance
                    .item
                    .allowed_rotations
                    .contains(&placement.rotation)
                {
                    issue(&mut issues, "rotation_forbidden", &id);
                }
                if packed
                    .container
                    .obstacles
                    .iter()
                    .flat_map(Obstacle::boxes)
                    .any(|box_| placement_hits_box(placement, box_))
                {
                    issue(&mut issues, "obstacle_collision", &id);
                }
                for other in packed.placements.iter().skip(index + 1) {
                    if placements_collide(placement, other) && !valid_nesting(placement, other) {
                        issue(
                            &mut issues,
                            "overlap",
                            &format!("{} and {}", id, other.instance.id()),
                        );
                    }
                }
                if compatibility_sensitive
                    && let Some(other) = first_incompatible(&packed.placements, index)
                {
                    issue(
                        &mut issues,
                        "incompatible_items",
                        &format!(
                            "{id}: {} is incompatible with {}",
                            placement.instance.item.id, other.id
                        ),
                    );
                }
                if placement.instance.item.must_be_on_floor && placement.envelope_origin.z != 0 {
                    issue(&mut issues, "floor_required", &id);
                }
                let required = placement
                    .instance
                    .item
                    .minimum_support_ratio
                    .max(request.config.minimum_support_ratio);
                if !support_area_sufficient(
                    placement.envelope_origin.z,
                    support_graph.support_area(index),
                    placement.envelope_dimensions.base_area(),
                    required,
                ) {
                    issue(&mut issues, "support", &id);
                }
                if !ground_contact_valid_with_graph(packed, index, &support_graph) {
                    issue(&mut issues, "ground_contact_violation", &id);
                }
            }
            if let Some(maximum) = packed.container.max_payload
                && payload > maximum.0
            {
                issue(&mut issues, "payload", "payload exceeded");
            }
            if let Some(maximum) = packed.container.max_items
                && packed.placements.len() > maximum
            {
                issue(&mut issues, "max_items", "item count exceeded");
            }
            if axle_overloaded(&packed.container, &packed.placements, None) {
                issue(&mut issues, "axle_overloaded", "gross axle limit exceeded");
            }
            for (tag, maximum) in &packed.container.tag_limits {
                if tag_counts.get(tag).copied().unwrap_or_default() > *maximum {
                    issue(&mut issues, "tag_count_exceeded", tag);
                }
            }
            let reserve = packed.container.inner_dimensions.volume()
                * packed.container.void_fill_reserve_ppm
                / 1_000_000;
            if packed.used_volume().saturating_add(reserve)
                > packed.container.inner_dimensions.volume()
            {
                issue(
                    &mut issues,
                    "void_fill_reserve_exceeded",
                    "used volume plus reserve exceeds inner volume",
                );
            }
            validate_top_loads_with_graph(packed, &support_graph, &mut issues);
            validate_stack_counts_with_graph(packed, &support_graph, &mut issues);
            validate_route_order_with_graph(packed, &support_graph, &mut issues);
        }
        validate_fixed_placements(request, containers, &mut issues);
        (issues, seen)
    }
}

/// The first other item in the container, in placement order, that the item at `index`
/// may not share it with: either one's `incompatible_tags` names a tag the other carries.
fn first_incompatible(placements: &[Placement], index: usize) -> Option<&Item> {
    let item = &placements[index].instance.item;
    if item.tags.is_empty() && item.incompatible_tags.is_empty() {
        return None;
    }
    placements
        .iter()
        .enumerate()
        .filter(|(position, _)| *position != index)
        .map(|(_, other)| &other.instance.item)
        .find(|other| {
            !item.incompatible_tags.is_disjoint(&other.tags)
                || !other.incompatible_tags.is_disjoint(&item.tags)
        })
}

/// Every fixed placement is where the request put it, and nothing else claims to be.
///
/// Physics needs no rule: a fixed item is an ordinary placement, so every check above
/// already applied to it. Only whether it moved is new (docs/PLAN-REVISIONS.md).
fn validate_fixed_placements(
    request: &PackingRequest,
    containers: &[PackedContainer],
    issues: &mut Vec<ValidationIssue>,
) {
    type FixedKey = (String, String, i64, i64, i64, &'static str);
    let requested = request
        .fixed_placements
        .iter()
        .map(|entry| {
            (
                entry.packed_container_id(),
                entry.item_id.clone(),
                entry.position.x,
                entry.position.y,
                entry.position.z,
                entry.rotation.as_str(),
            )
        })
        .collect::<BTreeSet<FixedKey>>();
    let reported = containers
        .iter()
        .flat_map(|packed| {
            packed
                .placements
                .iter()
                .filter(|placement| placement.fixed)
                .map(move |placement| {
                    (
                        packed.id(),
                        placement.instance.item.id.clone(),
                        placement.position.x,
                        placement.position.y,
                        placement.position.z,
                        placement.rotation.as_str(),
                    )
                })
        })
        .collect::<BTreeSet<FixedKey>>();
    let present = containers
        .iter()
        .map(PackedContainer::id)
        .collect::<BTreeSet<_>>();
    let detail = |key: &FixedKey| {
        format!(
            "{} in {} at ({}, {}, {}) {}",
            key.1, key.0, key.2, key.3, key.4, key.5
        )
    };
    for key in requested.difference(&reported) {
        let code = if present.contains(&key.0) {
            "fixed_placement_moved"
        } else {
            "fixed_container_missing"
        };
        issue(issues, code, &detail(key));
    }
    for key in reported.difference(&requested) {
        issue(issues, "unexpected_fixed_placement", &detail(key));
    }
}

// `pub(crate)`, not private: `sequence.rs`'s `verify_loading_prefix_business_rules`
// reuses these exact calculations against every loading prefix, not only
// the finished scene -- crate-visible so that reuse doesn't need to duplicate the
// arithmetic, without widening this module's public API surface.
pub(crate) fn validate_top_loads(container: &PackedContainer, issues: &mut Vec<ValidationIssue>) {
    let graph = ContactGraph::from_placements(&container.placements);
    validate_top_loads_with_graph(container, &graph, issues);
}

fn validate_top_loads_with_graph(
    container: &PackedContainer,
    graph: &ContactGraph,
    issues: &mut Vec<ValidationIssue>,
) {
    let mut loads = vec![0_i128; container.placements.len()];
    let mut order = (0..container.placements.len()).collect::<Vec<_>>();
    order.sort_by_key(|index| std::cmp::Reverse(container.placements[*index].envelope_box().z2()));
    for upper_index in order {
        let upper_box = container.placements[upper_index].envelope_box();
        if upper_box.origin.z == 0 {
            continue;
        }
        let supports = graph.supporters(upper_index);
        if supports.is_empty() {
            continue;
        }
        let total_area = supports.iter().map(|edge| edge.area).sum::<i128>();
        let downward =
            container.placements[upper_index].instance.item.weight.0 as i128 + loads[upper_index];
        let mut distributed = 0_i128;
        for (position, edge) in supports.iter().enumerate() {
            let share = if position + 1 == supports.len() {
                downward.saturating_sub(distributed)
            } else {
                downward.saturating_mul(edge.area) / total_area
            };
            distributed = distributed.saturating_add(share);
            let support = &container.placements[edge.index].instance.item;
            if !support.stackable {
                issue(issues, "non_stackable", &support.id);
            }
            loads[edge.index] = loads[edge.index].saturating_add(share);
            if let Some(maximum) = support.max_top_load
                && loads[edge.index] > maximum.0 as i128
            {
                issue(issues, "top_load", &support.id);
            }
        }
    }
    // The same propagated loads in the other currency: a pressure the item itself must
    // survive, rather than a mass the box below must bear.
    if let Some((code, detail)) = crushed(&container.placements, &loads) {
        issue(issues, &code, &detail);
    }
    if let Some(maximum) = container.container.max_stack_density {
        const SQUARE_METRE_TICKS: i128 = 16_000_000_i128 * 16_000_000_i128;
        for (placement, load) in container.placements.iter().zip(loads) {
            let total = placement.instance.item.weight.0 as i128 + load;
            if total.saturating_mul(SQUARE_METRE_TICKS)
                > (maximum.0 as i128).saturating_mul(placement.envelope_dimensions.base_area())
            {
                issue(issues, "stack_density_exceeded", &placement.instance.id());
            }
        }
    }
}

pub(crate) fn ground_contact_valid(container: &PackedContainer, index: usize) -> bool {
    let graph = ContactGraph::from_placements(&container.placements);
    ground_contact_valid_with_graph(container, index, &graph)
}

fn ground_contact_valid_with_graph(
    container: &PackedContainer,
    index: usize,
    graph: &ContactGraph,
) -> bool {
    let placement = &container.placements[index];
    let candidate = placement.envelope_box();
    if candidate.origin.z == 0
        || matches!(
            placement.instance.item.ground_contact_rule.as_deref(),
            None | Some("free")
        )
    {
        return true;
    }
    match placement.instance.item.ground_contact_rule.as_deref() {
        Some("single") => graph.supporters(index).len() == 1,
        Some("multiple") => graph.supporters(index).len() >= 2,
        Some("covered") => graph.touches_all_corners(&container.placements, index),
        _ => true,
    }
}

pub(crate) fn validate_stack_counts(
    container: &PackedContainer,
    issues: &mut Vec<ValidationIssue>,
) {
    let graph = ContactGraph::from_placements(&container.placements);
    validate_stack_counts_with_graph(container, &graph, issues);
}

fn validate_stack_counts_with_graph(
    container: &PackedContainer,
    graph: &ContactGraph,
    issues: &mut Vec<ValidationIssue>,
) {
    for (root, placement) in container.placements.iter().enumerate() {
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
            issue(
                issues,
                "max_stacked_items_exceeded",
                &placement.instance.id(),
            );
        }
    }
}

fn validate_route_order_with_graph(
    container: &PackedContainer,
    graph: &ContactGraph,
    issues: &mut Vec<ValidationIssue>,
) {
    if container
        .placements
        .iter()
        .all(|placement| placement.instance.item.stop_index.is_none())
    {
        return;
    }
    for lower in 0..container.placements.len() {
        let upper_indices = graph.children(lower);
        let lower_stop = container.placements[lower].instance.item.stop_index;
        for upper in upper_indices {
            let upper_stop = container.placements[*upper].instance.item.stop_index;
            if let (Some(lower_stop), Some(upper_stop)) = (lower_stop, upper_stop)
                && lower_stop < upper_stop
            {
                issue(
                    issues,
                    "unloading_order_violation",
                    &container.placements[lower].instance.id(),
                );
            }
        }
    }
}

fn issue(issues: &mut Vec<ValidationIssue>, code: &str, message: &str) {
    issues.push(ValidationIssue {
        code: code.into(),
        message: message.into(),
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::{Dimensions, Rotation};
    use crate::units::{Length, Weight};

    fn nested_container(
        count: usize,
        max_top_load: Option<Weight>,
        max_stacked: Option<usize>,
    ) -> PackedContainer {
        let dimensions = Dimensions {
            length: Length(10),
            width: Length(10),
            height: Length(10),
        };
        let item = Item {
            id: "tote".into(),
            dimensions,
            weight: Weight(Weight::TICKS_PER_KG),
            quantity: count,
            allowed_rotations: vec![Rotation::Lwh],
            stackable: true,
            must_be_on_floor: false,
            max_top_load,
            minimum_support_ratio: 0.0,
            group: None,
            tags: BTreeSet::new(),
            incompatible_tags: BTreeSet::new(),
            priority: 0,
            metadata: BTreeMap::new(),
            nesting_height: Some(Length(5)),
            max_stacked_items: max_stacked,
            ground_contact_rule: None,
            stop_index: None,
            eligible_container_tags: BTreeSet::new(),
            value: None,
            shape_type: crate::geometry::ShapeType::RigidCuboid,
            hull_vertices: None,
            compression_ratio_ppm: None,
            max_compression_pressure_kpa: None,
        };
        let placements = (0..count)
            .map(|index| Placement {
                instance: ItemInstance {
                    item: item.clone(),
                    sequence: index + 1,
                },
                position: Point {
                    x: 0,
                    y: 0,
                    z: index as i64 * 5,
                },
                rotation: Rotation::Lwh,
                dimensions,
                envelope_origin: Point {
                    x: 0,
                    y: 0,
                    z: index as i64 * 5,
                },
                envelope_dimensions: dimensions,
                support_ratio: 1.0,
                top_load: Weight(0),
                fixed: false,
            })
            .collect();
        PackedContainer {
            container: Container {
                id: "column".into(),
                inner_dimensions: Dimensions {
                    length: Length(10),
                    width: Length(10),
                    height: Length(100),
                },
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
                preloaded: Vec::new(),
            },
            sequence: 1,
            placements,
            lattice_summary: None,
            lattice_items: Vec::new(),
        }
    }

    fn validate_nested(packed: PackedContainer) -> ValidationReport {
        let request = PackingRequest {
            items: vec![packed.placements[0].instance.item.clone()],
            containers: vec![packed.container.clone()],
            config: PackingConfig::default(),
            output_length_unit: "ticks".into(),
            output_weight_unit: "ticks".into(),
            catalog_versions_used: Vec::new(),
            fixed_placements: Vec::new(),
            fixed_containers: Vec::new(),
        };
        let result = PackingResult {
            status: PackingStatus::Feasible,
            containers: vec![packed],
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
        };
        IndependentValidator.validate(&request, &result)
    }

    fn incompatibilities(packed: PackedContainer) -> Vec<String> {
        validate_nested(packed)
            .issues
            .into_iter()
            .filter(|issue| issue.code == "incompatible_items")
            .map(|issue| issue.message)
            .collect()
    }

    #[test]
    fn validator_refuses_incompatible_items_sharing_a_container() {
        let mut packed = nested_container(3, None, None);
        packed.placements[0].instance.item.id = "acid".into();
        packed.placements[0]
            .instance
            .item
            .tags
            .insert("acid".into());
        packed.placements[2].instance.item.id = "base".into();
        packed.placements[2]
            .instance
            .item
            .incompatible_tags
            .insert("acid".into());
        assert_eq!(
            incompatibilities(packed.clone()),
            [
                "acid#1: acid is incompatible with base",
                "base#3: base is incompatible with acid"
            ]
        );
        packed.placements[2].instance.item.incompatible_tags = ["alkali".into()].into();
        assert!(incompatibilities(packed).is_empty());
        assert!(incompatibilities(nested_container(3, None, None)).is_empty());
    }

    #[test]
    fn validator_applies_top_load_limits_across_a_nested_edge() {
        let mut packed = nested_container(3, None, None);
        packed.placements[1].instance.item.max_top_load =
            Some(Weight(Weight::TICKS_PER_KG * 3 / 4));
        let mut issues = Vec::new();

        validate_top_loads(&packed, &mut issues);

        assert_eq!(
            issues
                .iter()
                .filter(|issue| issue.code == "top_load")
                .count(),
            1
        );
    }

    #[test]
    fn validator_counts_nested_descendants_for_stack_limits() {
        let packed = nested_container(3, None, Some(1));
        let mut issues = Vec::new();

        validate_stack_counts(&packed, &mut issues);

        assert!(
            issues
                .iter()
                .any(|issue| issue.code == "max_stacked_items_exceeded")
        );
    }

    #[test]
    fn validator_treats_a_nested_predecessor_as_full_single_covered_support() {
        for rule in ["single", "covered"] {
            let mut packed = nested_container(3, None, None);
            for placement in &mut packed.placements {
                placement.instance.item.minimum_support_ratio = 1.0;
                placement.instance.item.ground_contact_rule = Some(rule.into());
            }

            let report = validate_nested(packed);

            assert!(report.valid, "rule={rule}: {:?}", report.issues);
        }
    }

    #[test]
    fn validator_rejects_multiple_ground_contact_for_one_nested_predecessor() {
        let mut packed = nested_container(3, None, None);
        for placement in &mut packed.placements {
            placement.instance.item.minimum_support_ratio = 1.0;
            placement.instance.item.ground_contact_rule = Some("multiple".into());
        }

        let report = validate_nested(packed);

        assert!(!report.valid);
        assert!(
            report
                .issues
                .iter()
                .any(|issue| issue.code == "ground_contact_violation")
        );
        assert!(report.issues.iter().all(|issue| issue.code != "support"));
    }

    #[test]
    fn validator_rejects_weightless_nested_contact_on_a_non_stackable_predecessor() {
        let mut packed = nested_container(2, None, None);
        for placement in &mut packed.placements {
            placement.instance.item.weight = Weight(0);
            placement.instance.item.stackable = false;
        }

        let report = validate_nested(packed);

        assert!(
            report
                .issues
                .iter()
                .any(|issue| issue.code == "non_stackable")
        );
    }
}
