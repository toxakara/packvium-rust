//! Boxes and convex hulls in ticks, written the slow, obvious way the reference writes them:
//! a validator that took the engine's shortcuts would stop being a second opinion.

use std::collections::BTreeSet;

use super::json::Res;
use super::num::{abs, add, mul, sub};

pub(crate) type Vertex = [i128; 3];

/// Axis permutation each orientation code applies to (length, width, height).
pub(crate) const ROTATIONS: [(&str, [usize; 3]); 6] = [
    ("LWH", [0, 1, 2]),
    ("LHW", [0, 2, 1]),
    ("WLH", [1, 0, 2]),
    ("WHL", [1, 2, 0]),
    ("HLW", [2, 0, 1]),
    ("HWL", [2, 1, 0]),
];

/// An axis-aligned box. Its far corner and base area are computed once, checked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Cuboid {
    pub(crate) x: i128,
    pub(crate) y: i128,
    pub(crate) z: i128,
    pub(crate) length: i128,
    pub(crate) width: i128,
    pub(crate) height: i128,
    pub(crate) x2: i128,
    pub(crate) y2: i128,
    pub(crate) z2: i128,
    pub(crate) base_area: i128,
}

impl Cuboid {
    pub(crate) fn new(
        x: i128,
        y: i128,
        z: i128,
        length: i128,
        width: i128,
        height: i128,
    ) -> Res<Cuboid> {
        Ok(Cuboid {
            x,
            y,
            z,
            length,
            width,
            height,
            x2: add(x, length)?,
            y2: add(y, width)?,
            z2: add(z, height)?,
            base_area: mul(length, width)?,
        })
    }

    pub(crate) fn intersects(&self, other: &Cuboid) -> bool {
        self.x < other.x2
            && other.x < self.x2
            && self.y < other.y2
            && other.y < self.y2
            && self.z < other.z2
            && other.z < self.z2
    }

    pub(crate) fn contains(&self, other: &Cuboid) -> bool {
        self.x <= other.x
            && self.y <= other.y
            && self.z <= other.z
            && other.x2 <= self.x2
            && other.y2 <= self.y2
            && other.z2 <= self.z2
    }

    /// Never larger than either base area, so the product cannot overflow.
    pub(crate) fn overlap_area_xy(&self, other: &Cuboid) -> i128 {
        let dx = (self.x2.min(other.x2) - self.x.max(other.x)).max(0);
        let dy = (self.y2.min(other.y2) - self.y.max(other.y)).max(0);
        dx * dy
    }

    pub(crate) fn vertices(&self) -> Vec<Vertex> {
        let mut corners = Vec::with_capacity(8);
        for dx in [0, 1] {
            for dy in [0, 1] {
                for dz in [0, 1] {
                    corners.push([
                        if dx == 1 { self.x2 } else { self.x },
                        if dy == 1 { self.y2 } else { self.y },
                        if dz == 1 { self.z2 } else { self.z },
                    ]);
                }
            }
        }
        corners
    }
}

pub(crate) fn difference(left: Vertex, right: Vertex) -> Res<Vertex> {
    Ok([
        sub(left[0], right[0])?,
        sub(left[1], right[1])?,
        sub(left[2], right[2])?,
    ])
}

pub(crate) fn cross(left: Vertex, right: Vertex) -> Res<Vertex> {
    Ok([
        sub(mul(left[1], right[2])?, mul(left[2], right[1])?)?,
        sub(mul(left[2], right[0])?, mul(left[0], right[2])?)?,
        sub(mul(left[0], right[1])?, mul(left[1], right[0])?)?,
    ])
}

pub(crate) fn dot(left: Vertex, right: Vertex) -> Res<i128> {
    add(
        add(mul(left[0], right[0])?, mul(left[1], right[1])?)?,
        mul(left[2], right[2])?,
    )
}

fn gcd(mut left: i128, mut right: i128) -> i128 {
    while right != 0 {
        (left, right) = (right, left % right);
    }
    left
}

/// The axis reduced by its gcd and signed so its first non-zero component is positive.
pub(crate) fn primitive_axis(axis: Vertex) -> Res<Vertex> {
    let divisor = gcd(gcd(abs(axis[0])?, abs(axis[1])?), abs(axis[2])?);
    if divisor == 0 {
        return Ok([0, 0, 0]);
    }
    let reduced = [axis[0] / divisor, axis[1] / divisor, axis[2] / divisor];
    Ok(match reduced.iter().find(|value| **value != 0) {
        Some(first) if *first < 0 => [-reduced[0], -reduced[1], -reduced[2]],
        _ => reduced,
    })
}

/// Reorient a hull the way `ROTATIONS` reorients its box, flipping one axis on an odd
/// permutation so it stays a proper rotation, then translated so its bounds start at 0.
pub(crate) fn rotate_hull(vertices: &[Vertex], axes: [usize; 3]) -> Res<Vec<Vertex>> {
    let inversions = (0..3)
        .flat_map(|first| (first + 1..3).map(move |second| (first, second)))
        .filter(|&(first, second)| axes[first] > axes[second])
        .count();
    let sign = if inversions % 2 == 1 { -1 } else { 1 };
    let turned: Vec<Vertex> = vertices
        .iter()
        .map(|vertex| [sign * vertex[axes[0]], vertex[axes[1]], vertex[axes[2]]])
        .collect();
    let low: Vec<i128> = (0..3)
        .map(|axis| turned.iter().map(|vertex| vertex[axis]).min().unwrap_or(0))
        .collect();
    turned
        .iter()
        .map(|vertex| difference(*vertex, [low[0], low[1], low[2]]))
        .collect()
}

/// Every 3-combination of `points`, in `itertools.combinations` order.
fn triples(points: &[Vertex]) -> impl Iterator<Item = (Vertex, Vertex, Vertex)> + '_ {
    let count = points.len();
    (0..count).flat_map(move |first| {
        (first + 1..count).flat_map(move |second| {
            (second + 1..count).map(move |third| (points[first], points[second], points[third]))
        })
    })
}

fn pair_directions(points: &[Vertex]) -> Res<Vec<Vertex>> {
    let mut directions = Vec::new();
    for first in 0..points.len() {
        for second in first + 1..points.len() {
            directions.push(difference(points[second], points[first])?);
        }
    }
    Ok(directions)
}

/// The complete separating-axis superset: every face normal of each hull and every cross
/// product of an edge direction from one with one from the other.
fn hull_axes(left: &[Vertex], right: &[Vertex]) -> Res<BTreeSet<Vertex>> {
    let mut axes = BTreeSet::new();
    for hull in [left, right] {
        for (first, second, third) in triples(hull) {
            axes.insert(primitive_axis(cross(
                difference(second, first)?,
                difference(third, first)?,
            )?)?);
        }
    }
    let right_directions = pair_directions(right)?;
    for one in pair_directions(left)? {
        for other in &right_directions {
            axes.insert(primitive_axis(cross(one, *other)?)?);
        }
    }
    axes.remove(&[0, 0, 0]);
    Ok(axes)
}

/// Whether two convex solids share interior volume. Touching is contact, not collision.
pub(crate) fn solids_overlap(left: &[Vertex], right: &[Vertex]) -> Res<bool> {
    for axis in hull_axes(left, right)? {
        let (left_low, left_high) = extent(left, axis)?;
        let (right_low, right_high) = extent(right, axis)?;
        if left_high <= right_low || right_high <= left_low {
            return Ok(false);
        }
    }
    Ok(true)
}

fn extent(points: &[Vertex], axis: Vertex) -> Res<(i128, i128)> {
    let mut low = i128::MAX;
    let mut high = i128::MIN;
    for point in points {
        let value = dot(*point, axis)?;
        low = low.min(value);
        high = high.max(value);
    }
    Ok((low, high))
}

/// Exact volume of a convex hull, `6V = sum of a . (b x c)` over its outward faces.
pub(crate) fn hull_volume(vertices: &[Vertex]) -> Res<i128> {
    let mut normals = BTreeSet::new();
    for (first, second, third) in triples(vertices) {
        let axis = primitive_axis(cross(
            difference(second, first)?,
            difference(third, first)?,
        )?)?;
        if axis != [0, 0, 0] {
            normals.insert(axis);
        }
    }
    let mut six_volumes: i128 = 0;
    for axis in normals {
        for outward in [axis, [-axis[0], -axis[1], -axis[2]]] {
            let (_, highest) = extent(vertices, outward)?;
            let mut face = Vec::new();
            for vertex in vertices {
                if dot(*vertex, outward)? == highest {
                    face.push(*vertex);
                }
            }
            if face.len() < 3 {
                continue;
            }
            let ordered = wind_face(&face, outward)?;
            let apex = ordered[0];
            for pair in ordered[1..].windows(2) {
                six_volumes = add(six_volumes, dot(apex, cross(pair[0], pair[1])?)?)?;
            }
        }
    }
    Ok(abs(six_volumes)? / 6)
}

/// Corners of one planar convex face in cyclic order seen from outside, by gift-wrapping from
/// the lexicographically smallest vertex; collinear candidates resolve to the farthest.
fn wind_face(face: &[Vertex], outward: Vertex) -> Res<Vec<Vertex>> {
    let start = *face
        .iter()
        .min()
        .expect("a face has at least three vertices");
    let mut ordered = vec![start];
    let mut current = start;
    for _ in 0..face.len() {
        let mut following: Option<Vertex> = None;
        for candidate in face {
            if *candidate == current {
                continue;
            }
            let Some(held_vertex) = following else {
                following = Some(*candidate);
                continue;
            };
            let turn = dot(
                cross(
                    difference(held_vertex, current)?,
                    difference(*candidate, current)?,
                )?,
                outward,
            )?;
            let reach_vector = difference(*candidate, current)?;
            let held_vector = difference(held_vertex, current)?;
            let reach = dot(reach_vector, reach_vector)?;
            let held = dot(held_vector, held_vector)?;
            if turn < 0 || (turn == 0 && reach > held) {
                following = Some(*candidate);
            }
        }
        match following {
            Some(next) if next != start => {
                ordered.push(next);
                current = next;
            }
            _ => break,
        }
    }
    Ok(ordered)
}
