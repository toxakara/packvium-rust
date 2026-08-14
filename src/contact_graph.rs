//! Direct vertical-contact graph with an XY broad phase.
//!
//! A plain pairwise scan costs `O(n^2)` even for a regular lattice where each box
//! touches only one box above and below.  Grouping by contact plane and hashing each
//! footprint into at most four cells makes construction `O(n log n + q + e)` with the
//! deterministic `BTreeMap`/`BTreeSet` implementation used here: `q` is broad-phase
//! candidates and `e` is real support edges.  The exact overlap test remains the
//! authority.  A physically dense graph can still have `e = O(n^2)`; no exact
//! representation can promise linear work in that case.

use crate::geometry::Aabb;
use crate::model::{Placement, valid_nesting};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ContactEdge {
    pub(crate) index: usize,
    pub(crate) area: i128,
}

#[derive(Clone, Debug)]
pub(crate) struct ContactGraph {
    supporters: Vec<Vec<ContactEdge>>,
    children: Vec<Vec<usize>>,
    #[cfg(test)]
    candidate_checks: usize,
}

type NestingColumnKey<'a> = (&'a str, Option<i64>, i64, i64, i64, i64);

fn nesting_column_key(placement: &Placement) -> NestingColumnKey<'_> {
    let box_ = placement.envelope_box();
    (
        placement.instance.item.id.as_str(),
        placement.instance.item.nesting_height.map(|depth| depth.0),
        box_.origin.x,
        box_.origin.y,
        box_.x2(),
        box_.y2(),
    )
}

impl ContactGraph {
    pub(crate) fn new(boxes: &[Aabb]) -> Self {
        let mut supporters = vec![Vec::new(); boxes.len()];
        let mut children = vec![Vec::new(); boxes.len()];
        let cell = boxes
            .iter()
            .map(|box_| box_.dimensions.length.0.max(box_.dimensions.width.0))
            .max()
            .unwrap_or(1)
            .max(1);

        let mut by_top = BTreeMap::<i64, Vec<usize>>::new();
        for (index, box_) in boxes.iter().enumerate() {
            by_top.entry(box_.z2()).or_default().push(index);
        }

        let mut levels = BTreeMap::<i64, BTreeMap<(i64, i64), Vec<usize>>>::new();
        for (top, indices) in &by_top {
            let level = levels.entry(*top).or_default();
            for index in indices {
                for key in cells(boxes[*index], cell) {
                    level.entry(key).or_default().push(*index);
                }
            }
        }

        #[cfg(test)]
        let mut candidate_checks = 0;
        for (upper_index, upper) in boxes.iter().enumerate() {
            let Some(level) = levels.get(&upper.origin.z) else {
                continue;
            };
            let mut nearby = BTreeSet::new();
            for key in cells(*upper, cell) {
                if let Some(indices) = level.get(&key) {
                    nearby.extend(indices.iter().copied());
                }
            }
            for lower_index in nearby {
                if lower_index == upper_index {
                    continue;
                }
                #[cfg(test)]
                {
                    candidate_checks += 1;
                }
                let area = boxes[lower_index].overlap_area_xy(*upper);
                if area > 0 {
                    supporters[upper_index].push(ContactEdge {
                        index: lower_index,
                        area,
                    });
                    children[lower_index].push(upper_index);
                }
            }
        }

        Self {
            supporters,
            children,
            #[cfg(test)]
            candidate_checks,
        }
    }

    /// Builds the ordinary face-contact graph, removes same-column face edges hidden
    /// behind a nearer nested item, and adds the direct bearing edge between adjacent
    /// members of every exact nesting column.
    ///
    /// A nesting column is keyed by item type and the complete XY footprint, then
    /// ordered by `(origin.z, placement_index)`. Each placement can acquire at most
    /// one nested supporter and one nested child, so grouping and ordering cost
    /// `O(n log n)` time and `O(n)` space on top of `new`'s output-sensitive bound.
    /// The shared `valid_nesting` predicate remains the final authority: grouping
    /// only supplies adjacent pairs that could plausibly be nested.
    pub(crate) fn from_placements(placements: &[Placement]) -> Self {
        let boxes = placements
            .iter()
            .map(Placement::envelope_box)
            .collect::<Vec<_>>();
        let mut graph = Self::new(&boxes);
        let mut columns = BTreeMap::<NestingColumnKey<'_>, Vec<usize>>::new();
        let mut modified = false;

        for (index, placement) in placements.iter().enumerate() {
            if placement.instance.item.nesting_height.is_none() {
                continue;
            }
            columns
                .entry(nesting_column_key(placement))
                .or_default()
                .push(index);
        }

        for indices in columns.values_mut() {
            indices.sort_by_key(|index| (boxes[*index].origin.z, *index));
            for adjacent in indices.windows(2) {
                let lower_index = adjacent[0];
                let upper_index = adjacent[1];
                if valid_nesting(&placements[lower_index], &placements[upper_index]) {
                    modified |= graph.remove_same_column_supporters(upper_index, placements);
                    modified |= graph.insert_edge(
                        lower_index,
                        upper_index,
                        boxes[lower_index].overlap_area_xy(boxes[upper_index]),
                    );
                }
            }
        }
        if modified {
            graph.rebuild_children();
        }

        graph
    }

    fn remove_same_column_supporters(
        &mut self,
        upper_index: usize,
        placements: &[Placement],
    ) -> bool {
        let column = nesting_column_key(&placements[upper_index]);
        let previous_len = self.supporters[upper_index].len();
        self.supporters[upper_index]
            .retain(|edge| nesting_column_key(&placements[edge.index]) != column);
        self.supporters[upper_index].len() != previous_len
    }

    fn insert_edge(&mut self, lower_index: usize, upper_index: usize, area: i128) -> bool {
        let Err(support_position) =
            self.supporters[upper_index].binary_search_by_key(&lower_index, |edge| edge.index)
        else {
            return false;
        };
        self.supporters[upper_index].insert(
            support_position,
            ContactEdge {
                index: lower_index,
                area,
            },
        );
        true
    }

    fn rebuild_children(&mut self) {
        for children in &mut self.children {
            children.clear();
        }
        for (upper_index, supporters) in self.supporters.iter().enumerate() {
            for edge in supporters {
                self.children[edge.index].push(upper_index);
            }
        }
    }

    pub(crate) fn supporters(&self, index: usize) -> &[ContactEdge] {
        &self.supporters[index]
    }

    pub(crate) fn support_area(&self, index: usize) -> i128 {
        self.supporters[index].iter().map(|edge| edge.area).sum()
    }

    pub(crate) fn support_ratio(&self, placements: &[Placement], index: usize) -> f64 {
        let upper = placements[index].envelope_box();
        if upper.origin.z == 0 {
            return 1.0;
        }
        (self.support_area(index) as f64 / upper.dimensions.base_area() as f64).min(1.0)
    }

    pub(crate) fn touches_all_corners(&self, placements: &[Placement], index: usize) -> bool {
        let upper = placements[index].envelope_box();
        let corners = [
            (upper.origin.x, upper.origin.y),
            (upper.x2(), upper.origin.y),
            (upper.origin.x, upper.y2()),
            (upper.x2(), upper.y2()),
        ];
        corners.iter().all(|(x, y)| {
            self.supporters[index].iter().any(|edge| {
                let surface = placements[edge.index].envelope_box();
                surface.origin.x <= *x
                    && *x <= surface.x2()
                    && surface.origin.y <= *y
                    && *y <= surface.y2()
            })
        })
    }

    pub(crate) fn children(&self, index: usize) -> &[usize] {
        &self.children[index]
    }
}

fn cells(box_: Aabb, cell: i64) -> BTreeSet<(i64, i64)> {
    let x1 = box_.origin.x.div_euclid(cell);
    let y1 = box_.origin.y.div_euclid(cell);
    let x2 = box_.x2().saturating_sub(1).div_euclid(cell);
    let y2 = box_.y2().saturating_sub(1).div_euclid(cell);
    BTreeSet::from([(x1, y1), (x2, y1), (x1, y2), (x2, y2)])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::{Dimensions, Point, Rotation};
    use crate::model::{Item, ItemInstance};
    use crate::units::{Length, Weight};
    use std::collections::{BTreeMap, BTreeSet};

    fn box_at(x: i64, y: i64, z: i64) -> Aabb {
        Aabb {
            origin: Point { x, y, z },
            dimensions: Dimensions {
                length: Length(10),
                width: Length(10),
                height: Length(10),
            },
        }
    }

    fn placement(sequence: usize, z: i64, nesting_height: Option<i64>) -> Placement {
        let dimensions = Dimensions {
            length: Length(10),
            width: Length(10),
            height: Length(10),
        };
        Placement {
            instance: ItemInstance {
                item: Item {
                    id: "tote".into(),
                    dimensions,
                    weight: Weight(0),
                    quantity: 2,
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
                    nesting_height: nesting_height.map(Length),
                    max_stacked_items: None,
                    ground_contact_rule: None,
                    stop_index: None,
                    eligible_container_tags: BTreeSet::new(),
                    value: None,
                },
                sequence,
            },
            position: Point { x: 0, y: 0, z },
            rotation: Rotation::Lwh,
            dimensions,
            envelope_origin: Point { x: 0, y: 0, z },
            envelope_dimensions: dimensions,
            support_ratio: 1.0,
            top_load: Weight(0),
        }
    }

    #[test]
    fn a_regular_lattice_does_not_fall_back_to_all_pairs() {
        let mut boxes = Vec::new();
        for layer in 0..2 {
            for y in 0..32 {
                for x in 0..32 {
                    boxes.push(box_at(x * 10, y * 10, layer * 10));
                }
            }
        }
        let graph = ContactGraph::new(&boxes);
        assert_eq!(graph.candidate_checks, 1024);
        assert_eq!(graph.supporters(1024).len(), 1);
        assert_eq!(graph.children(0), &[1024]);
    }

    #[test]
    fn broad_phase_edges_match_an_exact_pairwise_scan() {
        let boxes = vec![
            box_at(0, 0, 0),
            box_at(10, 0, 0),
            box_at(5, 0, 10),
            box_at(30, 0, 10),
        ];
        let graph = ContactGraph::new(&boxes);
        for (upper_index, upper) in boxes.iter().enumerate() {
            let expected = boxes
                .iter()
                .enumerate()
                .filter_map(|(lower_index, lower)| {
                    let area = lower.overlap_area_xy(*upper);
                    (lower_index != upper_index && lower.z2() == upper.origin.z && area > 0)
                        .then_some(ContactEdge {
                            index: lower_index,
                            area,
                        })
                })
                .collect::<Vec<_>>();
            assert_eq!(graph.supporters(upper_index), expected);
        }
    }

    #[test]
    fn placement_graph_adds_exact_adjacent_nesting_support() {
        let placements = vec![
            placement(1, 0, Some(5)),
            placement(2, 5, Some(5)),
            placement(3, 10, Some(5)),
        ];
        let boxes = placements
            .iter()
            .map(Placement::envelope_box)
            .collect::<Vec<_>>();

        assert!(ContactGraph::new(&boxes).supporters(1).is_empty());
        assert_eq!(
            ContactGraph::new(&boxes).supporters(2),
            &[ContactEdge {
                index: 0,
                area: 100,
            }]
        );
        let graph = ContactGraph::from_placements(&placements);
        assert_eq!(
            graph.supporters(1),
            &[ContactEdge {
                index: 0,
                area: 100,
            }]
        );
        assert_eq!(
            graph.supporters(2),
            &[ContactEdge {
                index: 1,
                area: 100,
            }]
        );
        assert_eq!(graph.children(0), &[1]);
        assert_eq!(graph.children(1), &[2]);
        assert_eq!(graph.support_area(1), 100);
        assert_eq!(graph.support_ratio(&placements, 1), 1.0);
        assert!(graph.touches_all_corners(&placements, 1));
        assert_eq!(graph.support_area(2), 100);
        assert_eq!(graph.support_ratio(&placements, 2), 1.0);
        assert!(graph.touches_all_corners(&placements, 2));
    }

    #[test]
    fn placement_graph_does_not_mix_same_id_with_different_nesting_depths() {
        let lower = placement(1, 0, Some(5));
        let upper = placement(2, 5, Some(6));
        assert!(!valid_nesting(&lower, &upper));
        assert!(!valid_nesting(&upper, &lower));

        let graph = ContactGraph::from_placements(&[lower, upper]);
        assert!(graph.supporters(1).is_empty());
    }

    #[test]
    fn placement_graph_preserves_the_non_nesting_graph_and_check_count() {
        let placements = vec![placement(1, 0, None), placement(2, 10, None)];
        let boxes = placements
            .iter()
            .map(Placement::envelope_box)
            .collect::<Vec<_>>();
        let expected = ContactGraph::new(&boxes);
        let actual = ContactGraph::from_placements(&placements);

        assert_eq!(actual.candidate_checks, expected.candidate_checks);
        for index in 0..placements.len() {
            assert_eq!(actual.supporters(index), expected.supporters(index));
            assert_eq!(actual.children(index), expected.children(index));
        }
    }
}
