//! Uniform-grid broad-phase collision index.
//!
//! The index is deliberately only a filter.  A query returns placement indices whose
//! cells overlap the candidate's cells; the caller still performs the exact `Aabb`
//! intersection test.  Consequently an over-inclusive bucket only costs time, while
//! the tests below guard the correctness-critical property: an actually intersecting
//! box is never omitted.

use crate::geometry::{Aabb, Dimensions};
use std::collections::BTreeMap;

#[derive(Clone, Debug)]
pub(crate) struct SpatialIndex {
    cell_x: i64,
    cell_y: i64,
    cell_z: i64,
    cells: BTreeMap<(i64, i64, i64), Vec<usize>>,
}

impl SpatialIndex {
    pub(crate) fn new(container: Dimensions) -> Self {
        Self {
            cell_x: cell_size(container.length.0),
            cell_y: cell_size(container.width.0),
            cell_z: cell_size(container.height.0),
            cells: BTreeMap::new(),
        }
    }

    pub(crate) fn add(&mut self, index: usize, box_: Aabb) {
        for cell in self.cells_for(box_) {
            self.cells.entry(cell).or_default().push(index);
        }
    }

    pub(crate) fn query(&self, box_: Aabb) -> Vec<usize> {
        // A contiguous buffer is materially cheaper in this hot path than allocating
        // one tree node per unique placement. Sorting restores the exact ascending
        // index order the former BTreeSet returned, so collision short-circuit order
        // and deterministic metrics remain unchanged. O(q log q) time, O(q) space.
        let mut result = Vec::new();
        for cell in self.cells_for(box_) {
            if let Some(indices) = self.cells.get(&cell) {
                result.extend_from_slice(indices);
            }
        }
        result.sort_unstable();
        result.dedup();
        result
    }

    fn cells_for(&self, box_: Aabb) -> Vec<(i64, i64, i64)> {
        let (x1, x2) = cell_range(box_.origin.x, box_.x2(), self.cell_x);
        let (y1, y2) = cell_range(box_.origin.y, box_.y2(), self.cell_y);
        let (z1, z2) = cell_range(box_.origin.z, box_.z2(), self.cell_z);
        let mut result = Vec::new();
        for x in x1..x2 {
            for y in y1..y2 {
                for z in z1..z2 {
                    result.push((x, y, z));
                }
            }
        }
        result
    }
}

fn cell_size(axis: i64) -> i64 {
    ceil_div(axis.max(1), 8).max(1)
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
            let broad = index.query(query).into_iter().collect::<BTreeSet<_>>();
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
