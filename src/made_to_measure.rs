//! Made-to-measure containers (`docs/MADE-TO-MEASURE-CONTAINERS.md` "1.6
//! implementation contract").
//!
//! A made-to-measure container is cut by an on-demand box machine. Its declared
//! `inner_dimensions` is the largest box the machine can cut; [`MadeToMeasure`] adds the
//! smallest size, the cut step and the prices. A trial packs the round's items into up to four
//! solving envelopes ([`MadeToMeasure::solving_envelopes`]) and keeps the first that places
//! every item; the committed box is the achieved extent rounded up onto the machine grid
//! ([`MadeToMeasure::fabricate`]).
//!
//! Every quantity is an exact integer. Lengths are `i64` ticks; areas, volumes and products are
//! `i128`, and the material formula is split so no intermediate passes `2^127` for any request
//! admission lets through.

use crate::geometry::Dimensions;
use crate::model::Container;
use crate::units::{Length, Weight};

/// One square metre in square length ticks: a tick is 1/16 000 000 m.
const SQUARE_METRE_TICKS2: i128 = 16_000_000 * 16_000_000;
/// The largest integer every binding returns unchanged; no fabricated price may pass it.
pub(crate) const MAX_EXACT_INTEGER: i128 = 9_007_199_254_740_991;
/// Each compact rung's side as a fraction of the integer cube root of the round's volume.
const RUNG_FACTORS: [(i128, i128); 3] = [(5, 4), (3, 2), (2, 1)];

/// What an on-demand box machine can cut beyond the container's declared maximum.
///
/// Per axis (length, width, height) in ticks: `minimum` equal to the maximum pins the axis.
/// `tare_per_square_metre` is weight ticks per square metre of inner surface, and
/// `outer_allowance` the board added to each inner side.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MadeToMeasure {
    pub minimum: [i64; 3],
    pub increment: [i64; 3],
    pub cost_per_square_metre_minor: i64,
    pub tare_per_square_metre: i64,
    pub outer_allowance: [i64; 3],
}

/// The box cut for one packed container.
pub(crate) struct Fabricated {
    pub(crate) inner: Dimensions,
    pub(crate) outer: Dimensions,
    pub(crate) cost_minor: i128,
    pub(crate) tare: i128,
}

impl MadeToMeasure {
    /// The smallest size the machine can cut on `axis` that is at least `value`.
    pub fn grid(&self, axis: usize, value: i64) -> i64 {
        let (minimum, step) = (self.minimum[axis], self.increment[axis]);
        let span = value.max(minimum) - minimum;
        minimum + (span + step - 1) / step * step
    }

    /// `grid`, capped at the machine maximum: sizes a solving envelope, never a box.
    pub fn clamp(&self, axis: usize, value: i64, maximum: i64) -> i64 {
        maximum.min(self.grid(axis, value))
    }

    /// The rungs one trial tries, ascending on every axis, the machine maximum last.
    ///
    /// `edges` are each remaining instance's envelope edges (nominal plus twice the
    /// clearance). `O(r)` over the `r` instances plus the cube root; the list depends only
    /// on their multiset.
    pub fn solving_envelopes(
        &self,
        maximum: [i64; 3],
        edges: impl IntoIterator<Item = [i64; 3]>,
    ) -> Vec<[i64; 3]> {
        let mut volume = 0_i128;
        let mut floors = [0_i64; 3];
        for mut instance in edges {
            volume += instance
                .iter()
                .map(|edge| i128::from(*edge))
                .product::<i128>();
            instance.sort_unstable_by(|left, right| right.cmp(left));
            for rank in 0..3 {
                floors[rank] = floors[rank].max(instance[rank]);
            }
        }
        let side = icbrt_ceil(volume);
        let mut rungs: Vec<[i64; 3]> = Vec::new();
        for (numerator, denominator) in RUNG_FACTORS {
            let scaled = i64::try_from((side * numerator + denominator - 1) / denominator)
                .unwrap_or(i64::MAX);
            let rung: [i64; 3] = std::array::from_fn(|axis| {
                self.clamp(axis, scaled.max(floors[axis]), maximum[axis])
            });
            let holds = rung.iter().map(|edge| i128::from(*edge)).product::<i128>() >= volume;
            if holds && !rungs.contains(&rung) {
                rungs.push(rung);
            }
        }
        if !rungs.contains(&maximum) {
            rungs.push(maximum);
        }
        rungs
    }

    /// The box cut for an achieved extent: its sides, outer sides, price and tare. O(1).
    ///
    /// Containment is a theorem, not a check: every placement lies inside `achieved`, and
    /// the grid never rounds below it or past the maximum (D4), which the assertion guards.
    pub(crate) fn fabricate(
        &self,
        maximum: [i64; 3],
        achieved: [i64; 3],
        fee_minor: i64,
        declared_tare: i64,
    ) -> Fabricated {
        let sides: [i64; 3] = std::array::from_fn(|axis| self.grid(axis, achieved[axis]));
        assert!(
            (0..3).all(|axis| sides[axis] <= maximum[axis]),
            "a fabricated box never exceeds its machine"
        );
        let outer: [i64; 3] = std::array::from_fn(|axis| sides[axis] + self.outer_allowance[axis]);
        let area = surface_area(sides);
        Fabricated {
            inner: dimensions(sides),
            outer: dimensions(outer),
            cost_minor: i128::from(fee_minor)
                + per_square_metre(i128::from(self.cost_per_square_metre_minor), area),
            tare: i128::from(declared_tare)
                + per_square_metre(i128::from(self.tare_per_square_metre), area),
        }
    }

    /// The price of the largest box the machine can cut: the admission ceiling. Cost grows
    /// with every side, so it bounds every box any search can produce.
    pub(crate) fn largest_box_cost_minor(&self, maximum: [i64; 3], fee_minor: i64) -> i128 {
        i128::from(fee_minor)
            + per_square_metre(
                i128::from(self.cost_per_square_metre_minor),
                surface_area(maximum),
            )
    }

    /// The board weight of the largest box the machine can cut.
    pub(crate) fn largest_box_tare(&self, maximum: [i64; 3], declared_tare: i64) -> i128 {
        i128::from(declared_tare)
            + per_square_metre(
                i128::from(self.tare_per_square_metre),
                surface_area(maximum),
            )
    }
}

impl Container {
    /// The declared container solved inside `rung`: same id, everything else unchanged.
    pub(crate) fn with_envelope(&self, rung: [i64; 3]) -> Container {
        let mut envelope = self.clone();
        envelope.inner_dimensions = dimensions(rung);
        envelope
    }

    /// The box cut for an achieved extent, equal to this declared machine except its size,
    /// outer size, price and tare. `None` for a stock container.
    pub(crate) fn fabricate(&self, achieved: [i64; 3]) -> Option<Container> {
        let machine = self.made_to_measure?;
        let cut = machine.fabricate(
            sides_of(self.inner_dimensions),
            achieved,
            self.cost_minor,
            self.tare_weight.0,
        );
        let mut fabricated = self.clone();
        fabricated.inner_dimensions = cut.inner;
        fabricated.outer_dimensions = Some(cut.outer);
        // Admission bounded the largest box's price below 2^53 and its tare inside `i64`.
        fabricated.cost_minor = i64::try_from(cut.cost_minor).expect("admission bounds the price");
        fabricated.tare_weight =
            Weight(i64::try_from(cut.tare).expect("admission bounds the tare"));
        Some(fabricated)
    }
}

/// The inner surface of a closed box, `2(LW + LH + WH)`, in square ticks.
pub(crate) fn surface_area([length, width, height]: [i64; 3]) -> i128 {
    let (l, w, h) = (i128::from(length), i128::from(width), i128::from(height));
    2 * (l * w + l * h + w * h)
}

/// `ceil(rate * area / 1 m^2)` as `p*q + ceil(p*r / T^2)` with `q, r = divmod(area, T^2)`:
/// `p*r < 2^101` always, and `p*q` stays inside `i128` wherever admission let `p` through.
pub(crate) fn per_square_metre(rate: i128, area: i128) -> i128 {
    let (whole, fraction) = (area / SQUARE_METRE_TICKS2, area % SQUARE_METRE_TICKS2);
    rate * whole + (rate * fraction + SQUARE_METRE_TICKS2 - 1) / SQUARE_METRE_TICKS2
}

/// The smallest non-negative `c` with `c^3 >= value`: doubling, then bisection, under 90
/// exact steps for any volume below `2^128`. A cube past `i128` counts as large enough.
pub(crate) fn icbrt_ceil(value: i128) -> i128 {
    let at_least = |candidate: i128| {
        candidate
            .checked_mul(candidate)
            .and_then(|square| square.checked_mul(candidate))
            .is_none_or(|cube| cube >= value)
    };
    let mut high = 1_i128;
    while !at_least(high) {
        high *= 2;
    }
    let mut low = 0_i128;
    while low < high {
        let middle = (low + high) / 2;
        if at_least(middle) {
            high = middle;
        } else {
            low = middle + 1;
        }
    }
    low
}

pub(crate) fn sides_of(dimensions: Dimensions) -> [i64; 3] {
    [dimensions.length.0, dimensions.width.0, dimensions.height.0]
}

fn dimensions([length, width, height]: [i64; 3]) -> Dimensions {
    Dimensions {
        length: Length(length),
        width: Length(width),
        height: Length(height),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MM: i64 = 16_000;

    fn machine(minimum: i64, increment: i64, price: i64) -> MadeToMeasure {
        MadeToMeasure {
            minimum: [minimum * MM; 3],
            increment: [increment * MM; 3],
            cost_per_square_metre_minor: price,
            tare_per_square_metre: 0,
            outer_allowance: [0; 3],
        }
    }

    fn cube(side: i64) -> [i64; 3] {
        [side * MM; 3]
    }

    #[test]
    fn the_grid_rounds_up_and_never_below_the_minimum() {
        let spec = machine(60, 25, 0);
        assert_eq!(spec.grid(0, 200 * MM), 210 * MM);
        assert_eq!(spec.grid(0, 210 * MM), 210 * MM);
        assert_eq!(spec.grid(0, 1), 60 * MM);
        assert_eq!(spec.clamp(0, 2000 * MM, 1510 * MM), 1510 * MM);
    }

    #[test]
    fn the_split_cost_equals_the_direct_ceiling() {
        let mut state = 2702_u64;
        for _ in 0..200 {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            let rate = i128::from(state >> 11);
            let area = i128::from(state >> 1) * 977;
            let direct = (rate * area + SQUARE_METRE_TICKS2 - 1) / SQUARE_METRE_TICKS2;
            assert_eq!(per_square_metre(rate, area), direct);
        }
        assert_eq!(per_square_metre(1, 1), 1);
        let worked = machine(60, 25, 1000).fabricate(cube(1510), cube(200), 40, 0);
        assert_eq!(worked.cost_minor, 305);
    }

    #[test]
    fn prices_near_the_exact_limit_are_computed_without_wrapping() {
        let side = 1_i64 << 53;
        let spec = MadeToMeasure {
            minimum: [1; 3],
            increment: [1; 3],
            cost_per_square_metre_minor: (1 << 53) - 1,
            tare_per_square_metre: i64::MAX,
            outer_allowance: [0; 3],
        };
        assert!(spec.largest_box_cost_minor([side; 3], 0) > MAX_EXACT_INTEGER);
        assert!(spec.largest_box_tare([side; 3], 0) > i128::from(i64::MAX));
    }

    #[test]
    fn the_integer_cube_root_is_the_least_cube_at_or_above() {
        for (value, root) in [
            (0, 0),
            (1, 1),
            (7, 2),
            (8, 2),
            (9, 3),
            (10_i128.pow(21), 10_i128.pow(7)),
            (10_i128.pow(21) + 1, 10_i128.pow(7) + 1),
        ] {
            assert_eq!(icbrt_ceil(value), root);
        }
        assert!(icbrt_ceil(i128::MAX) > 0);
    }

    #[test]
    fn the_worked_rung_lists_match_the_contract() {
        let eight = vec![cube(100); 8];
        assert_eq!(
            machine(60, 25, 0).solving_envelopes(cube(1510), eight),
            vec![cube(260), cube(310), cube(410), cube(1510)]
        );
        let pinned = MadeToMeasure {
            minimum: [200 * MM, 100 * MM, 100 * MM],
            increment: [70 * MM, 10 * MM, 10 * MM],
            ..machine(60, 25, 0)
        };
        let maximum = [1950 * MM, 100 * MM, 100 * MM];
        assert_eq!(
            pinned.solving_envelopes(maximum, vec![cube(100); 5]),
            vec![maximum]
        );
        assert_eq!(
            machine(50, 10, 0).solving_envelopes(cube(300), vec![cube(100); 3]),
            vec![cube(190), cube(220), cube(290), cube(300)]
        );
        let rod = [1000 * MM, 50 * MM, 50 * MM];
        let first = machine(60, 25, 0).solving_envelopes(cube(1510), vec![rod, cube(100)])[0];
        assert!(first[0] >= 1000 * MM && first[1] < 1000 * MM && first[2] < 1000 * MM);
        let mixed = vec![
            [300 * MM, 20 * MM, 40 * MM],
            cube(100),
            [50 * MM, 60 * MM, 70 * MM],
        ];
        let reversed = mixed.iter().rev().copied().collect::<Vec<_>>();
        assert_eq!(
            machine(60, 25, 0).solving_envelopes(cube(1510), mixed),
            machine(60, 25, 0).solving_envelopes(cube(1510), reversed)
        );
    }
}
