//! The centre-of-gravity support test, ported from Python's `support_polygon.py`
//! and PHP's `SupportPolygon` so the four engines refuse the same candidates.
//!
//! Every coordinate is doubled so a footprint centroid is an exact integer even for odd
//! extents; a doubled coordinate fits `i64` and a cross product of two differences fits
//! `i128`, so nothing here rounds.

use crate::geometry::Aabb;

type Point2 = (i128, i128);

/// Whether `candidate`'s footprint centroid lies inside or on the convex hull of the areas
/// where it rests on `supporters`. `O(s log s)` time and `O(s)` space for `s` supporters:
/// the hull is over at most four overlap corners per supporter.
pub(crate) fn centre_of_gravity_supported(
    candidate: Aabb,
    supporters: impl IntoIterator<Item = Aabb>,
) -> bool {
    let hull = convex_hull(contact_points(candidate, supporters));
    point_in_hull(doubled_centroid(candidate), &hull)
}

fn contact_points(candidate: Aabb, supporters: impl IntoIterator<Item = Aabb>) -> Vec<Point2> {
    let mut points = Vec::new();
    for other in supporters {
        let x1 = candidate.origin.x.max(other.origin.x);
        let y1 = candidate.origin.y.max(other.origin.y);
        let x2 = candidate.x2().min(other.x2());
        let y2 = candidate.y2().min(other.y2());
        if x2 > x1 && y2 > y1 {
            for x in [x1, x2] {
                for y in [y1, y2] {
                    points.push((2 * i128::from(x), 2 * i128::from(y)));
                }
            }
        }
    }
    points
}

fn doubled_centroid(box_: Aabb) -> Point2 {
    (
        i128::from(box_.origin.x) + i128::from(box_.x2()),
        i128::from(box_.origin.y) + i128::from(box_.y2()),
    )
}

fn cross(o: Point2, a: Point2, b: Point2) -> i128 {
    (a.0 - o.0) * (b.1 - o.1) - (a.1 - o.1) * (b.0 - o.0)
}

/// Andrew's monotone chain, counter-clockwise with duplicates collapsed. The points come four
/// to an overlap rectangle of positive area, so the hull is empty or has at least three
/// vertices: `support_polygon.py`'s degenerate one- and two-point cases cannot arise here.
fn convex_hull(mut points: Vec<Point2>) -> Vec<Point2> {
    points.sort_unstable();
    points.dedup();
    let mut hull = half_chain(points.iter().copied());
    hull.extend(half_chain(points.iter().rev().copied()));
    hull
}

fn half_chain(order: impl Iterator<Item = Point2>) -> Vec<Point2> {
    let mut out: Vec<Point2> = Vec::new();
    for point in order {
        while out.len() >= 2 && cross(out[out.len() - 2], out[out.len() - 1], point) <= 0 {
            out.pop();
        }
        out.push(point);
    }
    out.pop();
    out
}

fn point_in_hull(point: Point2, hull: &[Point2]) -> bool {
    !hull.is_empty()
        && (0..hull.len())
            .all(|index| cross(hull[index], hull[(index + 1) % hull.len()], point) >= 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::{Dimensions, Point};
    use crate::units::Length;

    fn aabb(x: i64, y: i64, length: i64, width: i64) -> Aabb {
        Aabb {
            origin: Point { x, y, z: 0 },
            dimensions: Dimensions {
                length: Length(length),
                width: Length(width),
                height: Length(10),
            },
        }
    }

    #[test]
    fn a_centroid_beyond_a_single_end_support_is_unsupported() {
        assert!(!centre_of_gravity_supported(
            aabb(0, 0, 100, 10),
            [aabb(0, 0, 30, 10)]
        ));
    }

    #[test]
    fn supports_at_both_ends_carry_the_centroid() {
        let ends = [aabb(0, 0, 20, 10), aabb(80, 0, 20, 10)];
        assert!(centre_of_gravity_supported(aabb(0, 0, 100, 10), ends));
    }

    #[test]
    fn an_odd_extent_keeps_the_centroid_exact() {
        // Centroid x is 50.5; a support ending at 51 still carries it.
        assert!(centre_of_gravity_supported(
            aabb(0, 0, 101, 10),
            [aabb(0, 0, 51, 10)]
        ));
        assert!(!centre_of_gravity_supported(
            aabb(0, 0, 101, 10),
            [aabb(0, 0, 50, 10)]
        ));
    }

    #[test]
    fn duplicate_corners_from_adjacent_supporters_collapse() {
        let halves = [aabb(0, 0, 10, 10), aabb(10, 0, 10, 10)];
        assert!(centre_of_gravity_supported(aabb(0, 0, 20, 10), halves));
    }

    #[test]
    fn a_box_that_only_touches_an_edge_carries_nothing() {
        assert!(!centre_of_gravity_supported(
            aabb(0, 0, 10, 10),
            [aabb(10, 0, 10, 10)]
        ));
    }

    #[test]
    fn no_support_is_unsupported() {
        assert!(!centre_of_gravity_supported(aabb(0, 0, 10, 10), []));
    }

    #[test]
    fn a_one_tick_knife_edge_under_the_centroid_carries_the_box() {
        assert!(centre_of_gravity_supported(
            aabb(0, 0, 10, 10),
            [aabb(5, 0, 1, 10)]
        ));
        assert!(!centre_of_gravity_supported(
            aabb(0, 0, 10, 10),
            [aabb(6, 0, 1, 10)]
        ));
    }

    #[test]
    fn coordinates_at_the_edge_of_i64_stay_exact() {
        let far = i64::MAX - 1_000;
        let candidate = aabb(far, far, 101, 10);
        assert!(centre_of_gravity_supported(
            candidate,
            [aabb(far, far, 51, 10)]
        ));
        assert!(!centre_of_gravity_supported(
            candidate,
            [aabb(far, far, 50, 10)]
        ));
    }

    #[test]
    fn a_supporter_wider_than_the_footprint_is_clipped_to_it() {
        assert!(centre_of_gravity_supported(
            aabb(10, 10, 10, 10),
            [aabb(0, 0, 100, 100)]
        ));
    }

    #[test]
    fn a_corner_square_whose_inner_corner_is_the_centroid_carries_it() {
        assert!(centre_of_gravity_supported(
            aabb(0, 0, 40, 40),
            [aabb(0, 0, 20, 20)]
        ));
        assert!(!centre_of_gravity_supported(
            aabb(0, 0, 40, 40),
            [aabb(0, 0, 20, 19)]
        ));
    }

    fn legs_on_a_circle(keep: impl Fn(f64) -> bool) -> Vec<Aabb> {
        (0..360)
            .map(|degree| f64::from(degree).to_radians())
            .filter(|angle| keep(*angle))
            .map(|angle| {
                // Rounding to whole ticks only moves a leg, never the centroid.
                #[allow(clippy::cast_possible_truncation)]
                let at = |trig: f64| (500.0 + 400.0 * trig).round() as i64;
                aabb(at(angle.cos()), at(angle.sin()), 1, 1)
            })
            .collect()
    }

    #[test]
    fn three_hundred_and_sixty_legs_on_a_circle_carry_the_centroid() {
        let legs = legs_on_a_circle(|_| true);
        assert!(centre_of_gravity_supported(aabb(0, 0, 1000, 1000), legs));
    }

    #[test]
    fn legs_on_half_a_circle_leave_the_centroid_hanging() {
        let legs = legs_on_a_circle(|angle| angle.sin() > 0.1);
        assert!(!centre_of_gravity_supported(aabb(0, 0, 1000, 1000), legs));
    }

    #[test]
    fn repeated_and_reordered_supporters_give_the_same_answer() {
        let legs = [aabb(0, 0, 5, 5), aabb(35, 0, 5, 5), aabb(0, 35, 5, 5)];
        let candidate = aabb(0, 0, 40, 40);
        let reversed = legs.iter().rev().copied();
        let doubled = legs.iter().chain(legs.iter()).copied();
        assert!(centre_of_gravity_supported(candidate, legs));
        assert!(centre_of_gravity_supported(candidate, reversed));
        assert!(centre_of_gravity_supported(candidate, doubled));
    }
}
