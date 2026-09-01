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
use std::sync::OnceLock;

/// One contact plane's boxes, hashed into XY cells. Keyed by plane, then by cell.
type Levels = BTreeMap<i64, BTreeMap<(i64, i64), Vec<usize>>>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ContactEdge {
    pub(crate) index: usize,
    pub(crate) area: i128,
}

#[derive(Clone, Debug)]
pub(crate) struct ContactGraph {
    supporters: Vec<Vec<ContactEdge>>,
    children: Vec<Vec<usize>>,
    boxes: Vec<Aabb>,
    cell: i64,
    top_levels: Levels,
    /// Built on first use, never by `new`: only `with_box` queries downward-facing
    /// planes, and a graph that is built once and read once would otherwise pay for an
    /// index nothing looks at. Cached on the base so a run of candidates evaluated
    /// against one search state builds it once between them.
    bottom_levels: OnceLock<Levels>,
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
        Self::with_cell_hint(boxes, 1)
    }

    /// `cell_hint` is an upper bound on the footprint of any box that may later be
    /// appended with [`ContactGraph::with_box`].
    ///
    /// Without it the cell is sized from the boxes present now, and appending anything
    /// wider has to fall back to a full rebuild -- correct, but it defeats the point,
    /// because in a search the base is what is already placed and the candidate is a
    /// *new* item that may well be the widest in the request. A caller that knows the
    /// item set passes its widest footprint once and the delta path then always applies.
    /// Too large a hint only makes each bucket coarser; too small a one cannot give a
    /// wrong answer, because the fallback covers it.
    pub(crate) fn with_cell_hint(boxes: &[Aabb], cell_hint: i64) -> Self {
        let mut supporters = vec![Vec::new(); boxes.len()];
        let mut children = vec![Vec::new(); boxes.len()];
        let cell = boxes
            .iter()
            .map(|box_| box_.dimensions.length.0.max(box_.dimensions.width.0))
            .max()
            .unwrap_or(1)
            .max(cell_hint)
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
            boxes: boxes.to_vec(),
            cell,
            top_levels: levels,
            bottom_levels: OnceLock::new(),
            #[cfg(test)]
            candidate_checks,
        }
    }

    /// This graph plus one more box, appended at the next index.
    ///
    /// Adding a box cannot create or destroy contact between two boxes that were already
    /// here: contact is a pairwise geometric predicate over two boxes and nothing else.
    /// That is the whole reason a delta is sound, and it is why only the new box's own
    /// two planes are queried instead of every box being re-examined.
    ///
    /// The result is required to be identical to `ContactGraph::new(&[..boxes, box_])`,
    /// not merely equivalent: `calculate_top_loads` splits a conserved integer across the
    /// supporter slice and hands the rounding remainder to its last edge, so edge *order*
    /// is contract, not presentation. The new box takes the highest index, so appending
    /// it to an existing supporter list keeps that list ascending.
    pub(crate) fn with_box(&self, box_: Aabb) -> Self {
        let index = self.boxes.len();
        let footprint = box_.dimensions.length.0.max(box_.dimensions.width.0);
        if footprint > self.cell {
            // The broad phase is only correct while its cell is at least as large as
            // every box hashed into it or queried against it -- a larger box could step
            // over cells in the middle of its own footprint and miss a real overlap. So
            // this is a correctness fallback, not an optimisation choice.
            let mut boxes = self.boxes.clone();
            boxes.push(box_);
            return Self::with_cell_hint(&boxes, footprint);
        }

        let below = self.overlaps(&self.top_levels, box_.origin.z, box_);
        let above = self.overlaps(self.bottom_levels(), box_.z2(), box_);

        let mut graph = self.clone();
        graph.boxes.push(box_);
        graph.supporters.push(
            below
                .iter()
                .map(|(lower_index, area)| ContactEdge {
                    index: *lower_index,
                    area: *area,
                })
                .collect(),
        );
        graph
            .children
            .push(above.iter().map(|(upper, _)| *upper).collect());
        for (lower_index, _) in &below {
            graph.children[*lower_index].push(index);
        }
        for (upper_index, area) in &above {
            graph.supporters[*upper_index].push(ContactEdge { index, area: *area });
        }

        // One box joins exactly two planes, so only those two buckets change.
        for key in cells(box_, self.cell) {
            graph
                .top_levels
                .entry(box_.z2())
                .or_default()
                .entry(key)
                .or_default()
                .push(index);
        }
        // The clone above carries the base's downward index, already initialised because
        // `above` was just queried through it, so it is amended in place rather than
        // cloned a second time.
        if let Some(bottom) = graph.bottom_levels.get_mut() {
            for key in cells(box_, self.cell) {
                bottom
                    .entry(box_.origin.z)
                    .or_default()
                    .entry(key)
                    .or_default()
                    .push(index);
            }
        }
        graph
    }

    /// Every box whose named plane meets `box_`, with the overlap area, ascending by
    /// index -- `BTreeSet` supplies that order, which the remainder split depends on.
    fn overlaps(&self, levels: &Levels, plane: i64, box_: Aabb) -> Vec<(usize, i128)> {
        let Some(level) = levels.get(&plane) else {
            return Vec::new();
        };
        let mut nearby = BTreeSet::new();
        for key in cells(box_, self.cell) {
            if let Some(indices) = level.get(&key) {
                nearby.extend(indices.iter().copied());
            }
        }
        nearby
            .into_iter()
            .filter_map(|other_index| {
                let area = self.boxes[other_index].overlap_area_xy(box_);
                (area > 0).then_some((other_index, area))
            })
            .collect()
    }

    fn bottom_levels(&self) -> &Levels {
        self.bottom_levels.get_or_init(|| {
            let mut levels = Levels::new();
            for (index, box_) in self.boxes.iter().enumerate() {
                let level = levels.entry(box_.origin.z).or_default();
                for key in cells(*box_, self.cell) {
                    level.entry(key).or_default().push(index);
                }
            }
            levels
        })
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
                    shape_type: crate::geometry::ShapeType::RigidCuboid,
                    hull_vertices: None,
                    compression_ratio_ppm: None,
                    max_compression_pressure_kpa: None,
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

    /// A deterministic generator: the crate has no test RNG dependency, and a property
    /// test that cannot be replayed from its seed is not much of a property test.
    struct Lcg(u64);

    impl Lcg {
        fn next(&mut self, bound: u64) -> u64 {
            self.0 = self
                .0
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1);
            (self.0 >> 33) % bound
        }
    }

    /// A scene whose boxes actually touch each other.
    ///
    /// What is under test is that a delta reproduces edges, so a corpus of scenes that
    /// mostly have no edges at all would pass with the delta returning nothing. Snapping
    /// every coordinate and extent to one coarse lattice makes shared planes the norm.
    fn touching_scene(rng: &mut Lcg, count: usize) -> Vec<Aabb> {
        let extents = [10, 20, 30];
        (0..count)
            .map(|_| Aabb {
                origin: Point {
                    x: rng.next(6) as i64 * 10,
                    y: rng.next(6) as i64 * 10,
                    z: rng.next(4) as i64 * 10,
                },
                dimensions: Dimensions {
                    length: Length(extents[rng.next(3) as usize]),
                    width: Length(extents[rng.next(3) as usize]),
                    height: Length(extents[rng.next(3) as usize]),
                },
            })
            .collect()
    }

    fn widest_footprint(boxes: &[Aabb]) -> i64 {
        boxes
            .iter()
            .map(|box_| box_.dimensions.length.0.max(box_.dimensions.width.0))
            .max()
            .unwrap_or(1)
    }

    fn assert_same_edges(left: &ContactGraph, right: &ContactGraph, count: usize, note: &str) {
        for index in 0..count {
            // Compared as slices, never as sets: `calculate_top_loads` hands the integer
            // rounding remainder to whichever supporter is *last*, so two graphs holding
            // the same edges in a different order are two different answers.
            assert_eq!(left.supporters(index), right.supporters(index), "{note}");
            assert_eq!(left.children(index), right.children(index), "{note}");
        }
    }

    #[test]
    fn appending_a_box_matches_building_the_whole_scene_at_once() {
        // The base is built with the widest footprint in the scene as its hint, which is
        // what a solver knows before it starts placing: the candidate about to be
        // appended may be larger than anything already placed, and sizing the broad phase
        // from the placed boxes alone would send every append into the fallback.
        for seed in 0..40u64 {
            let mut rng = Lcg(2000 + seed);
            let count = 2 + rng.next(13) as usize;
            let boxes = touching_scene(&mut rng, count);
            let split = (boxes.len() / 2).max(1);
            let hint = widest_footprint(&boxes);
            let base = ContactGraph::with_cell_hint(&boxes[..split], hint);
            let mut graph = base.clone();
            for box_ in &boxes[split..] {
                graph = graph.with_box(*box_);
            }
            // A full rebuild recounts its broad-phase probes; the delta path clones the
            // base's count untouched. So an unchanged count is what says the delta ran --
            // without it, an assertion that the two graphs match is equally satisfied by
            // a `with_box` that quietly rebuilds everything, which is none of the point.
            assert_eq!(
                graph.candidate_checks, base.candidate_checks,
                "seed {seed}: an append fell back to a full rebuild"
            );
            assert_same_edges(
                &graph,
                &ContactGraph::new(&boxes),
                boxes.len(),
                &format!("seed {seed}"),
            );
        }
    }

    /// The delta matches a rebuild across scene shapes the property test above excludes.
    ///
    /// That test asserts *zero* fallbacks, because proving the delta ran rather than
    /// quietly rebuilding was what was at stake. The consequence is that nothing exercised
    /// a run where the fallback and the delta interleave, which is what a cell hint of one
    /// produces several times per scene.
    ///
    /// Three axes vary independently. Tight coordinates make shared planes and zero-area
    /// edge contacts the norm; coordinates at 10^9 push the broad phase's cell arithmetic
    /// somewhere a lattice never goes; a huge hint collapses every box into one cell, which
    /// is the degenerate case the hash exists to avoid and therefore the one most likely to
    /// be wrong.
    #[test]
    fn the_delta_matches_a_rebuild_across_scene_shapes() {
        const TIGHT: [i64; 5] = [0, 1, 2, 5, 10];
        const WIDE: [i64; 4] = [0, 10, 100, 1_000_000_000];
        const TINY: [i64; 3] = [1, 2, 3];
        const MIXED: [i64; 4] = [1, 5, 10, 40];

        let shapes: [(&[i64], &[i64], &str); 7] = [
            (&TIGHT, &TINY, "exact"),
            (&TIGHT, &TINY, "one"),
            (&TIGHT, &MIXED, "one"),
            (&TIGHT, &MIXED, "huge"),
            (&WIDE, &MIXED, "exact"),
            (&WIDE, &MIXED, "one"),
            (&WIDE, &TINY, "huge"),
        ];

        for (index, (coordinates, extents, hint_mode)) in shapes.iter().enumerate() {
            let mut rng = Lcg(7000 + index as u64);
            for _ in 0..60 {
                let count = 1 + rng.next(10) as usize;
                let boxes = (0..count)
                    .map(|_| Aabb {
                        origin: Point {
                            x: coordinates[rng.next(coordinates.len() as u64) as usize],
                            y: coordinates[rng.next(coordinates.len() as u64) as usize],
                            z: coordinates[rng.next(coordinates.len() as u64) as usize],
                        },
                        dimensions: Dimensions {
                            length: Length(extents[rng.next(extents.len() as u64) as usize]),
                            width: Length(extents[rng.next(extents.len() as u64) as usize]),
                            height: Length(extents[rng.next(extents.len() as u64) as usize]),
                        },
                    })
                    .collect::<Vec<_>>();
                let widest = widest_footprint(&boxes);
                let hint = match *hint_mode {
                    "exact" => widest,
                    "one" => 1,
                    _ => widest * 100,
                };
                let split = (boxes.len() / 2).max(1);
                let mut graph = ContactGraph::with_cell_hint(&boxes[..split], hint);
                for box_ in &boxes[split..] {
                    graph = graph.with_box(*box_);
                }
                assert_same_edges(
                    &graph,
                    &ContactGraph::new(&boxes),
                    boxes.len(),
                    &format!("shape {index} ({hint_mode})"),
                );
            }
        }
    }

    #[test]
    fn a_box_wider_than_the_hint_rebuilds_and_is_still_correct() {
        // The hint is an optimisation; being wrong about it may cost time, never an
        // answer. The broad phase is only sound while its cell is at least the largest
        // footprint it hashes or is queried with, so a box exceeding the cell has to be
        // met with a rebuild -- this asserts both halves: that the rebuild happens, and
        // that the result is the one the full build gives.
        let small = vec![box_at(0, 0, 0), box_at(10, 0, 0)];
        let wide = Aabb {
            origin: Point { x: 0, y: 0, z: 10 },
            dimensions: Dimensions {
                length: Length(40),
                width: Length(10),
                height: Length(10),
            },
        };
        let base = ContactGraph::new(&small);
        let graph = base.with_box(wide);
        assert_ne!(graph.candidate_checks, base.candidate_checks);
        assert_eq!(
            graph
                .supporters(2)
                .iter()
                .map(|edge| edge.index)
                .collect::<Vec<_>>(),
            vec![0, 1]
        );
        let whole = [small.as_slice(), &[wide]].concat();
        assert_same_edges(&graph, &ContactGraph::new(&whole), 3, "wide append");
    }

    #[test]
    fn an_appended_box_lands_last_in_the_lists_it_joins() {
        // The new box always takes the highest index, so appending it to an existing
        // supporter list keeps that list ascending -- but only because it is appended and
        // not inserted, which is the kind of detail a from-scratch comparison on random
        // scenes can miss when no scene happens to produce the collision.
        let spanning = Aabb {
            origin: Point { x: 0, y: 0, z: 10 },
            dimensions: Dimensions {
                length: Length(20),
                width: Length(10),
                height: Length(10),
            },
        };
        let scene = vec![box_at(0, 0, 0), spanning, box_at(10, 0, 0)];
        let graph = ContactGraph::with_cell_hint(&scene, 20).with_box(box_at(0, 0, 20));
        assert_eq!(
            graph
                .supporters(1)
                .iter()
                .map(|edge| edge.index)
                .collect::<Vec<_>>(),
            vec![0, 2]
        );
        assert_eq!(graph.children(1), &[3]);
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
