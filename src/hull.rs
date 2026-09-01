//! Exact separating-axis collision and volume for `convex_hull` items.
//!
//! The rule is fixed by `docs/IRREGULAR-ITEMS.md` and reproduced here rather than translated
//! from another engine, so the shared golden fixtures compare implementations.
//!
//! ## Arithmetic
//!
//! A separating axis is a cross product of two edge vectors, so its components grow as the
//! square of a coordinate, and a projection `v . d` grows as the cube. Measured on a 100 mm
//! hull whose vertex coordinates are coprime -- where the gcd reduction cannot shrink the
//! axes -- the worst projection is 12_287_800_320_884_799_202, past what a signed 64-bit
//! integer holds. PHP needs a decimal-string fallback for that and JavaScript needs `BigInt`.
//!
//! Rust needs neither: `i128` covers it outright. With coordinates capped at
//! [`MAX_COORDINATE`], a cross product is bounded by `8 * C^2 = 8e16` and a projection by
//! `3 * C * 8e16 = 2.4e25`, against an `i128` ceiling of 1.7e38 -- thirteen orders of
//! headroom. Every product below is therefore plain `i128` and there is no second path to
//! keep in step with the first.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;

/// A point in an item's local tick frame.
///
/// Deliberately not the engine's `Point`, which forbids negative coordinates: a hull is
/// authored around whatever origin its author chose and is only moved into container
/// coordinates at placement time.
pub type Vertex = [i64; 3];
pub type Axis = [i64; 3];

/// Smallest number of vertices that can enclose a volume.
pub const MINIMUM_VERTICES: usize = 4;

/// Largest vertex coordinate a hull may carry, in ticks -- 6.25 m.
///
/// Shared verbatim with the other engines. It is what bounds every product in this module,
/// and in PHP and JavaScript it is what decides when their exact fallbacks are needed, so the
/// four must refuse exactly the same hulls or they disagree about which requests are legal.
pub const MAX_COORDINATE: i64 = 100_000_000;

/// Both the face normals and the edge directions of any axis-aligned box.
const UNIT_AXES: [Axis; 3] = [[0, 0, 1], [0, 1, 0], [1, 0, 0]];

/// Hull vertices do not enclose a three-dimensional volume, or stray outside the bound.
///
/// Rejected rather than repaired. A flat or duplicated vertex set has no interior, so every
/// separating-axis answer about it would be vacuously "no collision" -- an item that passes
/// through everything, which reads as a successful pack.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DegenerateHull(pub String);

impl std::fmt::Display for DegenerateHull {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HullShape {
    pub vertices: Vec<Vertex>,
    pub face_axes: Vec<Axis>,
    pub edge_directions: Vec<Axis>,
    /// Exact occupied volume in cubic ticks. Computed once with the axes, because a hull's
    /// volume is a property of the shape and utilisation would otherwise be reported from the
    /// bounding box -- two interlocking wedges in one crate reading as 200% full.
    pub volume: i128,
}

impl HullShape {
    pub fn of(vertices: &[Vertex]) -> Result<Self, DegenerateHull> {
        let points = validate(vertices)?;
        let mut faces: BTreeSet<Axis> = BTreeSet::new();
        for first in 0..points.len() {
            for second in first + 1..points.len() {
                for third in second + 1..points.len() {
                    let normal = cross(
                        difference(points[second], points[first]),
                        difference(points[third], points[first]),
                    );
                    // A triple whose plane cuts through the solid is not a face, and its
                    // normal separates nothing the real face normals do not.
                    if let Some(axis) = primitive(normal)
                        && is_supporting(&points, points[first], axis)
                    {
                        faces.insert(axis);
                    }
                }
            }
        }
        let face_axes: Vec<Axis> = faces.into_iter().collect();
        let wound = wound_faces(&points, &face_axes);
        Ok(Self {
            vertices: points,
            face_axes,
            edge_directions: edge_directions(&wound),
            volume: volume_of(&wound),
        })
    }

    /// A cuboid, built without searching for its own faces.
    ///
    /// A box's face normals and edge directions are both exactly the three unit axes, so the
    /// supporting-plane search would only rediscover what is already known.
    pub fn box_shape(length: i64, width: i64, height: i64) -> Self {
        let mut vertices = Vec::with_capacity(8);
        for x in [0, 1] {
            for y in [0, 1] {
                for z in [0, 1] {
                    vertices.push([x * length, y * width, z * height]);
                }
            }
        }
        Self {
            vertices,
            face_axes: UNIT_AXES.to_vec(),
            edge_directions: UNIT_AXES.to_vec(),
            volume: i128::from(length) * i128::from(width) * i128::from(height),
        }
    }

    /// Closed projection interval on `axis`, in this shape's own coordinates.
    pub fn projection(&self, axis: Axis) -> (i128, i128) {
        let mut low = i128::MAX;
        let mut high = i128::MIN;
        for vertex in &self.vertices {
            let value = dot(*vertex, axis);
            low = low.min(value);
            high = high.max(value);
        }
        (low, high)
    }
}

/// How many rotated hulls stay resident, before the memo is dropped and refilled.
///
/// A request is bounded by its distinct hull items times the six orientations, so this holds
/// far more than any request the solver is sized for. Bounded rather than growing for the
/// life of the process: the point of the memo is to spend less, not to spend it elsewhere.
const SHAPE_CACHE_ENTRIES: usize = 1024;

/// An item's authored vertices with the orientation asked of them -- the whole of what
/// determines a rotated hull, and therefore the memo's key.
type ShapeKey = (Vec<Vertex>, [usize; 3]);

thread_local! {
    static SHAPE_CACHE: RefCell<BTreeMap<ShapeKey, Option<Rc<HullShape>>>> =
        const { RefCell::new(BTreeMap::new()) };
}

/// The rotated hull of one item in one orientation, built at most once.
///
/// A hull depends on the item and the orientation and on nothing about where a candidate
/// sits, but the collision predicate was rebuilding it on every call -- `O(v^4)` work inside
/// an `O(n^2)` loop. Measured on the two-wedge fixture in the Python port, which had the same
/// shape of defect: 78 builds where four are needed.
///
/// Memoisation is safe here in the way it is not in general. `HullShape` is immutable once
/// built, the key is the whole of what determines the value, and callers only ever project
/// through it. Determinism is untouched: this changes how often the answer is computed, never
/// what it is, and a `BTreeMap` keeps even the eviction moment independent of hash order.
///
/// An `Rc` rather than a clone because the hot callers want a borrow: cloning three `Vec`s per
/// collision test would hand back a good part of what the memo saves.
pub fn shape_for(vertices: &[Vertex], source_axes: [usize; 3]) -> Option<Rc<HullShape>> {
    let key = (vertices.to_vec(), source_axes);
    SHAPE_CACHE.with(|cache| {
        if let Some(found) = cache.borrow().get(&key) {
            return found.clone();
        }
        let built = HullShape::of(&rotate(vertices, source_axes))
            .ok()
            .map(Rc::new);
        let mut cache = cache.borrow_mut();
        if cache.len() >= SHAPE_CACHE_ENTRIES {
            cache.clear();
        }
        cache.insert(key, built.clone());
        built
    })
}

/// Canonicalise an authored vertex list or refuse a hull with no interior.
pub fn validate(vertices: &[Vertex]) -> Result<Vec<Vertex>, DegenerateHull> {
    let points: Vec<Vertex> = vertices.to_vec();
    if points.len() < MINIMUM_VERTICES {
        return Err(DegenerateHull(format!(
            "a convex hull needs at least {MINIMUM_VERTICES} vertices, got {}",
            points.len()
        )));
    }
    let unique: BTreeSet<Vertex> = points.iter().copied().collect();
    if unique.len() != points.len() {
        return Err(DegenerateHull("convex hull vertices must be unique".into()));
    }
    if points
        .iter()
        .any(|vertex| vertex.iter().any(|axis| axis.abs() > MAX_COORDINATE))
    {
        return Err(DegenerateHull(format!(
            "convex hull coordinates must stay within {MAX_COORDINATE} ticks"
        )));
    }
    for first in 0..points.len() {
        for second in first + 1..points.len() {
            for third in second + 1..points.len() {
                for fourth in third + 1..points.len() {
                    let volume = dot_i128(
                        difference(points[fourth], points[first]),
                        cross(
                            difference(points[second], points[first]),
                            difference(points[third], points[first]),
                        ),
                    );
                    if volume != 0 {
                        return Ok(points);
                    }
                }
            }
        }
    }
    Err(DegenerateHull(
        "convex hull vertices are coplanar and enclose no volume".into(),
    ))
}

/// Reorient a hull the way `Dimensions::rotated` reorients its box, never mirroring it.
///
/// Three of the six rotations are odd permutations of the coordinate axes. On a cuboid that is
/// invisible; on a hull a bare permutation returns the item's mirror image, a shape the caller
/// does not own. One axis therefore changes sign when the permutation is odd, which makes all
/// six proper rotations. Vertices come back translated so the rotated hull's bounding box
/// starts at the origin, which is the frame every placement position is expressed in.
pub fn source_axes(rotation: crate::geometry::Rotation) -> [usize; 3] {
    use crate::geometry::Rotation;
    match rotation {
        Rotation::Lwh => [0, 1, 2],
        Rotation::Lhw => [0, 2, 1],
        Rotation::Wlh => [1, 0, 2],
        Rotation::Whl => [1, 2, 0],
        Rotation::Hlw => [2, 0, 1],
        Rotation::Hwl => [2, 1, 0],
    }
}

pub fn rotate(vertices: &[Vertex], source_axes: [usize; 3]) -> Vec<Vertex> {
    let mut inversions = 0;
    for first in 0..3 {
        for second in first + 1..3 {
            if source_axes[first] > source_axes[second] {
                inversions += 1;
            }
        }
    }
    let sign = if inversions % 2 == 1 { -1 } else { 1 };
    let turned: Vec<Vertex> = vertices
        .iter()
        .map(|vertex| {
            [
                sign * vertex[source_axes[0]],
                vertex[source_axes[1]],
                vertex[source_axes[2]],
            ]
        })
        .collect();
    let mut low = [i64::MAX; 3];
    for vertex in &turned {
        for axis in 0..3 {
            low[axis] = low[axis].min(vertex[axis]);
        }
    }
    turned
        .into_iter()
        .map(|vertex| [vertex[0] - low[0], vertex[1] - low[1], vertex[2] - low[2]])
        .collect()
}

/// Both hulls' face normals plus every edge-against-edge direction, deduplicated.
pub fn separating_axes(left: &HullShape, right: &HullShape) -> Vec<Axis> {
    let mut axes: BTreeSet<Axis> = BTreeSet::new();
    axes.extend(left.face_axes.iter().copied());
    axes.extend(right.face_axes.iter().copied());
    for left_edge in &left.edge_directions {
        for right_edge in &right.edge_directions {
            if let Some(axis) = primitive(cross(to_i128(*left_edge), to_i128(*right_edge))) {
                axes.insert(axis);
            }
        }
    }
    axes.into_iter().collect()
}

/// Do two placed hulls overlap with positive volume?
///
/// Touching is contact, not collision: the comparison is `<=`, keeping hulls consistent with
/// the half-open convention cuboids already use, so a hull resting exactly on a box is
/// supported rather than colliding with it.
pub fn collide(
    left: &HullShape,
    left_origin: Vertex,
    right: &HullShape,
    right_origin: Vertex,
) -> bool {
    for axis in separating_axes(left, right) {
        let (left_low, left_high) = left.projection(axis);
        let (right_low, right_high) = right.projection(axis);
        let left_shift = dot(left_origin, axis);
        let right_shift = dot(right_origin, axis);
        if left_high + left_shift <= right_low + right_shift
            || right_high + right_shift <= left_low + left_shift
        {
            return false;
        }
    }
    true
}

/// Inclusive lower and upper corners of a hull's axis-aligned envelope.
pub fn bounding_extent(vertices: &[Vertex]) -> (Vertex, Vertex) {
    let mut low = [i64::MAX; 3];
    let mut high = [i64::MIN; 3];
    for vertex in vertices {
        for axis in 0..3 {
            low[axis] = low[axis].min(vertex[axis]);
            high[axis] = high[axis].max(vertex[axis]);
        }
    }
    (low, high)
}

/// Every face of the hull, each as its own corners in outward cyclic order.
///
/// One walk, because the faces answer two questions at once: the volume needs them wound
/// consistently, and the hull's edges are the consecutive corner pairs of the same walk.
/// Each canonical face axis stands for up to two opposite faces, so both the maximal and the
/// minimal supporting plane along it are collected; a plane carrying fewer than three
/// vertices is an edge or a corner of the hull, not a face, and carries no edge its two
/// adjoining faces do not already carry.
fn wound_faces(vertices: &[Vertex], face_axes: &[Axis]) -> Vec<Vec<Vertex>> {
    let mut faces = Vec::new();
    for axis in face_axes {
        for outward in [*axis, [-axis[0], -axis[1], -axis[2]]] {
            let extreme = vertices
                .iter()
                .map(|vertex| dot(*vertex, outward))
                .max()
                .unwrap_or(0);
            let face: Vec<Vertex> = vertices
                .iter()
                .copied()
                .filter(|vertex| dot(*vertex, outward) == extreme)
                .collect();
            if face.len() < 3 {
                continue;
            }
            faces.push(wind_face(&face, outward));
        }
    }
    faces
}

/// Exact volume in cubic ticks, by the divergence theorem over the hull's own faces.
///
/// `6V = sum over outward-oriented surface triangles of a . (b x c)`, an integer for integer
/// vertices and therefore exact -- no tolerance decides whether a wedge is half a cube.
fn volume_of(faces: &[Vec<Vertex>]) -> i128 {
    let mut six_volumes: i128 = 0;
    for ordered in faces {
        let apex = ordered[0];
        for index in 1..ordered.len().saturating_sub(1) {
            six_volumes += dot_i128(
                to_i128(apex),
                cross(to_i128(ordered[index]), to_i128(ordered[index + 1])),
            );
        }
    }
    six_volumes.abs() / 6
}

/// Directions of the hull's real edges, deduplicated and canonical.
///
/// Every edge of a convex polyhedron is shared by exactly two faces, so walking each wound
/// face and taking its consecutive corner pairs -- closing the cycle -- reaches all of them.
/// The separating-axis theorem asks for exactly these, not for every vertex pair.
///
/// The distinction is the whole cost of the predicate. A hull has at most `3v - 6` edges but
/// `v(v - 1) / 2` vertex pairs, and the axis set is the *product* of two hulls' sets, so the
/// gap squares: on a 20-vertex hull, 1351 candidate axes rather than 15616. Vertex pairs were
/// never wrong, only a superset -- a pair that is not an edge names a direction no face can
/// separate along, so it can add an axis but never remove one.
fn edge_directions(faces: &[Vec<Vertex>]) -> Vec<Axis> {
    let mut edges: BTreeSet<Axis> = BTreeSet::new();
    for ordered in faces {
        for index in 0..ordered.len() {
            let start = ordered[index];
            let end = ordered[(index + 1) % ordered.len()];
            if let Some(axis) = primitive(difference(end, start)) {
                edges.insert(axis);
            }
        }
    }
    edges.into_iter().collect()
}

/// Corners of one planar convex face, in cyclic order seen from outside.
///
/// The vertices sharing a supporting plane are *not* all corners of the polygon they lie on:
/// one can sit inside the face, or part-way along one of its edges. Fanning over that raw set
/// triangulates the wrong region and the surface fails to close, which is exactly the defect
/// this shape of code was written wrongly for once already. Gift-wrapping keeps only the
/// corners: start from the smallest vertex, which is extreme in any linear order and therefore
/// a corner, and at each step take the vertex leaving every other on one side, resolving
/// collinear candidates to the farthest so an edge-interior vertex is walked past.
fn wind_face(face: &[Vertex], outward: Axis) -> Vec<Vertex> {
    let start = *face.iter().min().expect("a face has vertices");
    let mut ordered = vec![start];
    let mut current = start;
    for _ in 0..face.len() {
        let mut following: Option<Vertex> = None;
        for candidate in face {
            if *candidate == current {
                continue;
            }
            let Some(held) = following else {
                following = Some(*candidate);
                continue;
            };
            let turn = dot_i128(
                cross(difference(held, current), difference(*candidate, current)),
                to_i128(outward),
            );
            let reach = square_length(difference(*candidate, current));
            let kept = square_length(difference(held, current));
            if turn < 0 || (turn == 0 && reach > kept) {
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
    ordered
}

fn is_supporting(vertices: &[Vertex], origin: Vertex, axis: Axis) -> bool {
    let offset = dot(origin, axis);
    let mut above = false;
    let mut below = false;
    for vertex in vertices {
        let side = dot(*vertex, axis) - offset;
        if side > 0 {
            above = true;
        } else if side < 0 {
            below = true;
        }
        if above && below {
            return false;
        }
    }
    true
}

/// Divide out the gcd and fix the sign, so parallel axes collapse to one entry.
///
/// `None` for the zero vector: a cross product of two parallel directions names no axis, which
/// is an ordinary outcome here rather than an error.
fn primitive(axis: [i128; 3]) -> Option<Axis> {
    let divisor = gcd(gcd(axis[0].abs(), axis[1].abs()), axis[2].abs());
    if divisor == 0 {
        return None;
    }
    let reduced = [axis[0] / divisor, axis[1] / divisor, axis[2] / divisor];
    let leading = reduced.iter().copied().find(|value| *value != 0)?;
    let signed = if leading > 0 {
        reduced
    } else {
        [-reduced[0], -reduced[1], -reduced[2]]
    };
    // A primitive axis of a hull inside `MAX_COORDINATE` is bounded by `8 * C^2`, which is
    // comfortably inside `i64`; the conversion cannot fail for any admitted hull.
    Some([signed[0] as i64, signed[1] as i64, signed[2] as i64])
}

fn gcd(mut a: i128, mut b: i128) -> i128 {
    while b != 0 {
        let next = a % b;
        a = b;
        b = next;
    }
    a
}

fn difference(left: Vertex, right: Vertex) -> [i128; 3] {
    [
        i128::from(left[0]) - i128::from(right[0]),
        i128::from(left[1]) - i128::from(right[1]),
        i128::from(left[2]) - i128::from(right[2]),
    ]
}

fn to_i128(vector: [i64; 3]) -> [i128; 3] {
    [
        i128::from(vector[0]),
        i128::from(vector[1]),
        i128::from(vector[2]),
    ]
}

fn cross(left: [i128; 3], right: [i128; 3]) -> [i128; 3] {
    [
        left[1] * right[2] - left[2] * right[1],
        left[2] * right[0] - left[0] * right[2],
        left[0] * right[1] - left[1] * right[0],
    ]
}

fn dot(point: Vertex, axis: Axis) -> i128 {
    i128::from(point[0]) * i128::from(axis[0])
        + i128::from(point[1]) * i128::from(axis[1])
        + i128::from(point[2]) * i128::from(axis[2])
}

fn dot_i128(point: [i128; 3], axis: [i128; 3]) -> i128 {
    point[0] * axis[0] + point[1] * axis[1] + point[2] * axis[2]
}

fn square_length(vector: [i128; 3]) -> i128 {
    vector[0] * vector[0] + vector[1] * vector[1] + vector[2] * vector[2]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cube(side: i64) -> Vec<Vertex> {
        let mut vertices = Vec::new();
        for x in [0, 1] {
            for y in [0, 1] {
                for z in [0, 1] {
                    vertices.push([x * side, y * side, z * side]);
                }
            }
        }
        vertices
    }

    fn wedge(side: i64) -> Vec<Vertex> {
        vec![
            [0, 0, 0],
            [side, 0, 0],
            [0, side, 0],
            [0, 0, side],
            [side, 0, side],
            [0, side, side],
        ]
    }

    #[test]
    fn the_documented_ten_tick_cube_boundary() {
        let shape = HullShape::of(&cube(10)).unwrap();
        assert!(collide(&shape, [0, 0, 0], &shape, [9, 0, 0]), "9 overlaps");
        assert!(
            !collide(&shape, [0, 0, 0], &shape, [10, 0, 0]),
            "10 touches"
        );
        assert!(
            !collide(&shape, [0, 0, 0], &shape, [11, 0, 0]),
            "11 is clear"
        );
    }

    #[test]
    fn volumes_of_the_shapes_the_model_pins() {
        assert_eq!(HullShape::of(&cube(10)).unwrap().volume, 1000);
        assert_eq!(HullShape::of(&wedge(10)).unwrap().volume, 500);
        assert_eq!(
            HullShape::of(&[[0, 0, 0], [6, 0, 0], [0, 4, 0], [0, 0, 2]])
                .unwrap()
                .volume,
            8
        );
        assert_eq!(HullShape::box_shape(3, 4, 5).volume, 60);
    }

    /// The face triangulation was written wrongly once: fanning over every vertex at a
    /// supporting plane triangulates the wrong region when one of them is not a corner, and
    /// the surface stops closing. Both values were confirmed against a lattice count
    /// converging from above -- 576.12 and 277.59 at a 32-fold refinement.
    #[test]
    fn hulls_whose_faces_carry_a_non_corner_vertex() {
        assert_eq!(
            HullShape::of(&[
                [8, 12, 16],
                [16, 8, 8],
                [12, 0, 0],
                [8, 8, 16],
                [12, 4, 16],
                [8, 12, 8],
                [0, 16, 4]
            ])
            .unwrap()
            .volume,
            576
        );
        assert_eq!(
            HullShape::of(&[
                [4, 4, 8],
                [0, 0, 16],
                [12, 8, 16],
                [8, 16, 0],
                [0, 0, 8],
                [16, 12, 12]
            ])
            .unwrap()
            .volume,
            277
        );
    }

    /// The case that costs PHP a decimal-string fallback and JavaScript a `BigInt`. These
    /// coordinates are coprime, so the gcd reduction cannot shrink the axes and the worst
    /// projection is 12_287_800_320_884_799_202 -- past `i64`, nowhere near `i128`. Every
    /// value here is Python's.
    #[test]
    fn the_hull_that_overflows_sixty_four_bits() {
        let side = 100 * 16_000;
        let shape = HullShape::of(&[
            [0, 0, 0],
            [side, 1, 2],
            [3, side, 5],
            [7, 11, side],
            [side - 13, side - 17, side - 19],
        ])
        .unwrap();
        assert_eq!(shape.volume, 2_047_966_720_147_466_533);
        assert!(collide(&shape, [0, 0, 0], &shape, [0, 0, 0]));
        assert!(collide(
            &shape,
            [0, 0, 0],
            &shape,
            [533_333, 320_000, 228_571]
        ));
        assert!(!collide(&shape, [0, 0, 0], &shape, [2 * side, 0, 0]));
    }

    /// A bare coordinate permutation would mirror the shape for three of the six rotations.
    /// The signed volume of four of its vertices is the cheapest witness that none does.
    #[test]
    fn every_rotation_keeps_the_wedge_a_wedge() {
        let source = wedge(10);
        let signed = |v: &[Vertex]| {
            dot_i128(
                difference(v[3], v[0]),
                cross(difference(v[1], v[0]), difference(v[2], v[0])),
            )
        };
        let reference = signed(&source).signum();
        for axes in [
            [0, 1, 2],
            [0, 2, 1],
            [1, 0, 2],
            [1, 2, 0],
            [2, 0, 1],
            [2, 1, 0],
        ] {
            let turned = rotate(&source, axes);
            assert_eq!(signed(&turned).signum(), reference, "{axes:?} mirrored it");
        }
    }

    #[test]
    fn a_degenerate_hull_is_refused_rather_than_packed() {
        assert!(HullShape::of(&[[0, 0, 0], [1, 0, 0], [0, 1, 0]]).is_err());
        assert!(HullShape::of(&[[0, 0, 0], [1, 0, 0], [0, 1, 0], [0, 1, 0]]).is_err());
        assert!(HullShape::of(&[[0, 0, 0], [10, 0, 0], [0, 10, 0], [10, 10, 0]]).is_err());
        let huge = MAX_COORDINATE * 2;
        assert!(HullShape::of(&[[0, 0, 0], [huge, 0, 0], [0, huge, 0], [0, 0, huge]]).is_err());
    }

    /// The reduction is what keeps the axis set small; a cube has three face normals once
    /// opposite faces collapse onto one canonical direction.
    /// A cuboid has twelve edges in three directions, and finding more would mean the face
    /// walk is emitting diagonals rather than hull edges.
    ///
    /// `box_shape` names those three without searching; a hull authored as the same eight
    /// corners has to find them, so this is the cheapest check that the two agree.
    #[test]
    fn an_authored_box_reduces_to_three_edge_directions() {
        let authored = HullShape::of(&cube(10)).unwrap();
        assert_eq!(authored.edge_directions, UNIT_AXES.to_vec());
        assert_eq!(authored.face_axes, UNIT_AXES.to_vec());
    }

    /// A convex polyhedron on `v` vertices has at most `3v - 6` edges.
    ///
    /// The one cheap statement that separates a real edge set from a plausible wrong one. A
    /// walk that closed a face early would still pass every collision test -- a superset is
    /// always safe -- while quietly giving back the cost this change was made for.
    #[test]
    fn the_edge_count_obeys_the_euler_bound() {
        let shapes = [
            HullShape::of(&cube(10)).unwrap(),
            HullShape::of(&wedge(10)).unwrap(),
            HullShape::of(&[
                [8, 12, 16],
                [16, 8, 8],
                [12, 0, 0],
                [8, 8, 16],
                [12, 4, 16],
                [8, 12, 8],
                [0, 16, 4],
            ])
            .unwrap(),
        ];
        for shape in shapes {
            let bound = 3 * shape.vertices.len() - 6;
            assert!(
                shape.edge_directions.len() <= bound,
                "{} edge directions on {} vertices exceeds the Euler bound {bound}",
                shape.edge_directions.len(),
                shape.vertices.len()
            );
        }
    }

    /// The memo may change how often a shape is built and never what it is.
    ///
    /// Asserted rather than assumed: a cache is the classic place for a determinism
    /// regression to hide, because a wrong entry is only visible on the second call.
    #[test]
    fn the_shape_memo_returns_what_a_fresh_build_would() {
        let vertices = [
            [0, 0, 0],
            [12, 0, 0],
            [0, 9, 0],
            [0, 0, 7],
            [12, 9, 0],
            [4, 3, 7],
        ];
        for axes in [[0, 1, 2], [2, 0, 1], [1, 2, 0]] {
            let fresh = HullShape::of(&rotate(&vertices, axes)).unwrap();
            let first = shape_for(&vertices, axes).expect("a valid hull");
            let second = shape_for(&vertices, axes).expect("the same hull again");
            assert_eq!(*first, fresh);
            assert_eq!(*second, fresh);
        }
    }

    /// Hulls that catch a face walk starting from a small *string* rather than a small
    /// vertex.
    ///
    /// This port was already right -- it orders by the vertex array -- and the case is pinned
    /// here anyway, because the ordered arrays are a cross-engine statement. PHP and
    /// JavaScript ordered by the decimal encoding and found extra directions. A wrong start
    /// does not raise: the collision verdict stays right because a superset of axes is safe,
    /// so every port reads one shared fixture and compares the complete ordered lists.
    #[test]
    fn the_face_walk_starts_from_a_corner_not_from_a_small_string() {
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../../../../conformance/scene/hull-internals.json");
        let text = std::fs::read_to_string(path).expect("the shared hull fixture");
        let document: serde_json::Value = serde_json::from_str(&text).expect("valid fixture");
        assert_eq!(document["format"], "packvium-hull-internals/v1");
        for case in document["cases"].as_array().expect("fixture cases") {
            let vertices: Vec<Vertex> =
                serde_json::from_value(case["vertices"].clone()).expect("integer vertices");
            let expected_faces: Vec<Axis> =
                serde_json::from_value(case["face_axes"].clone()).expect("integer face axes");
            let expected_edges: Vec<Axis> = serde_json::from_value(case["edge_directions"].clone())
                .expect("integer edge directions");
            let expected_volume: i128 = case["volume"]
                .as_str()
                .expect("decimal volume")
                .parse()
                .expect("integer volume");
            let shape = HullShape::of(&vertices).expect("a valid hull");
            assert_eq!(shape.volume, expected_volume);
            assert_eq!(shape.face_axes, expected_faces);
            assert_eq!(shape.edge_directions, expected_edges);
        }
    }

    #[test]
    fn face_axes_exclude_planes_that_cut_through_the_hull() {
        assert_eq!(HullShape::of(&cube(10)).unwrap().face_axes.len(), 3);
    }

    /// A closed surface's outward triangle area vectors sum to zero. Asserted directly rather
    /// than inferred from a volume, because a volume can be wrong and still look plausible.
    #[test]
    fn the_triangulated_surface_closes() {
        for vertices in [
            cube(10),
            wedge(10),
            vec![
                [8, 12, 16],
                [16, 8, 8],
                [12, 0, 0],
                [8, 8, 16],
                [12, 4, 16],
                [8, 12, 8],
                [0, 16, 4],
            ],
            vec![
                [4, 4, 8],
                [0, 0, 16],
                [12, 8, 16],
                [8, 16, 0],
                [0, 0, 8],
                [16, 12, 12],
            ],
        ] {
            let shape = HullShape::of(&vertices).unwrap();
            let mut residual = [0i128; 3];
            for axis in &shape.face_axes {
                for outward in [*axis, [-axis[0], -axis[1], -axis[2]]] {
                    let extreme = shape
                        .vertices
                        .iter()
                        .map(|vertex| dot(*vertex, outward))
                        .max()
                        .unwrap();
                    let face: Vec<Vertex> = shape
                        .vertices
                        .iter()
                        .copied()
                        .filter(|vertex| dot(*vertex, outward) == extreme)
                        .collect();
                    if face.len() < 3 {
                        continue;
                    }
                    let ordered = wind_face(&face, outward);
                    let apex = ordered[0];
                    for index in 1..ordered.len().saturating_sub(1) {
                        let normal = cross(
                            difference(ordered[index], apex),
                            difference(ordered[index + 1], apex),
                        );
                        for axis in 0..3 {
                            residual[axis] += normal[axis];
                        }
                    }
                }
            }
            assert_eq!(
                residual,
                [0, 0, 0],
                "surface of {vertices:?} does not close"
            );
        }
    }
}
