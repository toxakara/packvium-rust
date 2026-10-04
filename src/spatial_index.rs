//! Uniform-grid broad-phase collision index.
//!
//! The index is deliberately only a filter.  A query returns placement indices whose
//! cells overlap the candidate's cells; the caller still performs the exact `Aabb`
//! intersection test.  Consequently an over-inclusive bucket only costs time, while
//! the tests below guard the correctness-critical property: an actually intersecting
//! box is never omitted.

use crate::geometry::{Aabb, Dimensions, Point};
use std::borrow::Cow;
use std::collections::BTreeMap;

type CellRange = (i64, i64);
type CellRanges = (CellRange, CellRange, CellRange);
type Cell = (i64, i64, i64);

/// Cells per axis inside the container; `cell_size` makes it at most 8.
const CELLS_PER_AXIS: i64 = 8;

#[derive(Clone, Debug)]
pub(crate) struct SpatialIndex {
    cell_x: i64,
    cell_y: i64,
    cell_z: i64,
    /// How many cells cover the container on each axis.
    grid: (i64, i64, i64),
    /// The container's cells, x-major: a bucket is one multiply-add away, where a tree
    /// lookup per cell was the query's dominant cost. At most 512 buckets.
    cells: Vec<Vec<usize>>,
    /// Cells outside the container. Nothing placed lies there on a valid request; the
    /// map keeps the index exact for any box it is given rather than assuming so.
    outside: BTreeMap<Cell, Vec<usize>>,
}

impl SpatialIndex {
    pub(crate) fn new(container: Dimensions) -> Self {
        let cell_x = cell_size(container.length.0);
        let cell_y = cell_size(container.width.0);
        let cell_z = cell_size(container.height.0);
        let grid = (
            ceil_div(container.length.0.max(1), cell_x),
            ceil_div(container.width.0.max(1), cell_y),
            ceil_div(container.height.0.max(1), cell_z),
        );
        Self {
            cell_x,
            cell_y,
            cell_z,
            grid,
            cells: Vec::new(),
            outside: BTreeMap::new(),
        }
    }

    pub(crate) fn add(&mut self, index: usize, box_: Aabb) {
        let ((x1, x2), (y1, y2), (z1, z2)) = self.cell_ranges(box_);
        if self.cells.is_empty() {
            // Allocated on first use, so an empty state -- and every clone of one -- costs
            // nothing for the buckets it has not filled.
            let (gx, gy, gz) = self.grid;
            self.cells = vec![Vec::new(); (gx * gy * gz) as usize];
        }
        for x in x1..x2 {
            for y in y1..y2 {
                for z in z1..z2 {
                    match self.slot((x, y, z)) {
                        Some(slot) => self.cells[slot].push(index),
                        None => self.outside.entry((x, y, z)).or_default().push(index),
                    }
                }
            }
        }
    }

    pub(crate) fn query(&self, box_: Aabb) -> Cow<'_, [usize]> {
        // Sorting restores the exact ascending index order the former BTreeSet returned,
        // so collision short-circuit order and deterministic metrics remain unchanged.
        // Multi-cell: O(c + q log q) time for c cells and q hits, O(q) space, sized once
        // up front instead of grown. A single cell borrows its bucket without allocating.
        let ((x1, x2), (y1, y2), (z1, z2)) = self.cell_ranges(box_);
        if x2 == x1 + 1 && y2 == y1 + 1 && z2 == z1 + 1 {
            return Cow::Borrowed(self.bucket((x1, y1, z1)));
        }
        let cells = || {
            (x1..x2).flat_map(move |x| (y1..y2).flat_map(move |y| (z1..z2).map(move |z| (x, y, z))))
        };
        let hits = cells().map(|cell| self.bucket(cell).len()).sum();
        let mut result = Vec::with_capacity(hits);
        for cell in cells() {
            result.extend_from_slice(self.bucket(cell));
        }
        if result.len() > 1 {
            result.sort_unstable();
            result.dedup();
        }
        Cow::Owned(result)
    }

    pub(crate) fn bucket_at(&self, point: Point) -> &[usize] {
        self.bucket((
            point.x.div_euclid(self.cell_x),
            point.y.div_euclid(self.cell_y),
            point.z.div_euclid(self.cell_z),
        ))
    }

    fn bucket(&self, cell: Cell) -> &[usize] {
        match self.slot(cell) {
            Some(slot) => self.cells.get(slot).map_or(&[], Vec::as_slice),
            None => self.outside.get(&cell).map_or(&[], Vec::as_slice),
        }
    }

    fn slot(&self, (x, y, z): Cell) -> Option<usize> {
        let (gx, gy, gz) = self.grid;
        let inside = (0..gx).contains(&x) && (0..gy).contains(&y) && (0..gz).contains(&z);
        inside.then(|| ((x * gy + y) * gz + z) as usize)
    }

    fn cell_ranges(&self, box_: Aabb) -> CellRanges {
        (
            cell_range(box_.origin.x, box_.x2(), self.cell_x),
            cell_range(box_.origin.y, box_.y2(), self.cell_y),
            cell_range(box_.origin.z, box_.z2(), self.cell_z),
        )
    }
}

fn cell_size(axis: i64) -> i64 {
    ceil_div(axis.max(1), CELLS_PER_AXIS).max(1)
}

fn cell_range(start: i64, end: i64, size: i64) -> (i64, i64) {
    let first = start.div_euclid(size);
    let exclusive_end = ceil_div(end.max(start.saturating_add(1)), size);
    (first, exclusive_end)
}

fn ceil_div(value: i64, divisor: i64) -> i64 {
    value.div_euclid(divisor) + i64::from(value.rem_euclid(divisor) != 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::Point;
    use crate::units::Length;
    use std::collections::BTreeSet;

    fn box_at(x: i64, y: i64, z: i64, length: i64, width: i64, height: i64) -> Aabb {
        Aabb {
            origin: Point { x, y, z },
            dimensions: Dimensions {
                length: Length(length),
                width: Length(width),
                height: Length(height),
            },
        }
    }

    #[test]
    fn single_cell_queries_borrow_the_bucket_without_allocating() {
        let mut index = SpatialIndex::new(box_at(0, 0, 0, 800, 800, 800).dimensions);
        index.add(0, box_at(0, 0, 0, 50, 50, 50));
        let query = index.query(box_at(0, 0, 0, 60, 60, 60));
        assert_eq!(&query[..], &[0]);
        assert_eq!(
            query.as_ptr(),
            index.bucket_at(Point { x: 0, y: 0, z: 0 }).as_ptr()
        );
    }

    #[test]
    fn every_exact_collision_is_present_in_the_broad_phase_result() {
        let container = box_at(0, 0, 0, 800, 800, 800).dimensions;
        let boxes = (0..120)
            .map(|index| {
                let x = (index * 67) % 720;
                let y = (index * 101) % 720;
                let z = (index * 149) % 720;
                box_at(x, y, z, 80, 80, 80)
            })
            .collect::<Vec<_>>();
        let mut index = SpatialIndex::new(container);
        for (position, box_) in boxes.iter().copied().enumerate() {
            index.add(position, box_);
        }

        for query_number in 0..80 {
            let query = box_at(
                (query_number * 83) % 740,
                (query_number * 127) % 740,
                (query_number * 173) % 740,
                60,
                60,
                60,
            );
            let broad = index.query(query).iter().copied().collect::<BTreeSet<_>>();
            let exact = boxes
                .iter()
                .enumerate()
                .filter_map(|(position, box_)| query.intersects(*box_).then_some(position))
                .collect::<BTreeSet<_>>();
            assert!(
                exact.is_subset(&broad),
                "query {query_number} lost a collision"
            );
        }
    }

    #[test]
    fn a_box_outside_the_container_is_still_found() {
        let mut index = SpatialIndex::new(box_at(0, 0, 0, 800, 800, 800).dimensions);
        index.add(0, box_at(-150, 0, 0, 100, 100, 100));
        index.add(1, box_at(780, 0, 0, 100, 100, 100));
        index.add(2, box_at(0, 0, 0, 50, 50, 50));

        assert_eq!(&index.query(box_at(-140, 10, 10, 20, 20, 20))[..], &[0]);
        assert_eq!(
            &index.query(box_at(-150, 0, 0, 1000, 60, 60))[..],
            &[0, 1, 2]
        );
        assert_eq!(index.bucket_at(Point { x: 850, y: 0, z: 0 }), &[1]);
    }

    #[test]
    fn an_empty_index_answers_every_query_with_nothing() {
        let index = SpatialIndex::new(box_at(0, 0, 0, 800, 800, 800).dimensions);
        assert!(index.query(box_at(0, 0, 0, 800, 800, 800)).is_empty());
        assert!(index.bucket_at(Point { x: 0, y: 0, z: 0 }).is_empty());
    }

    #[test]
    fn a_local_query_does_not_degenerate_to_the_whole_container() {
        let container = box_at(0, 0, 0, 800, 800, 800).dimensions;
        let mut index = SpatialIndex::new(container);
        for position in 0..64 {
            let x = (position % 4) as i64 * 200;
            let y = ((position / 4) % 4) as i64 * 200;
            let z = (position / 16) as i64 * 200;
            index.add(position, box_at(x, y, z, 50, 50, 50));
        }

        let candidates = index.query(box_at(0, 0, 0, 60, 60, 60));
        assert!(candidates.len() < 64);
        assert!(candidates.contains(&0));
    }
}
