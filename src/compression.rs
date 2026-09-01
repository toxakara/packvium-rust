//! Occupied height of a `compressible` item under the load resting on it.
//!
//! The model is fixed by `docs/IRREGULAR-ITEMS.md`. Pressure is an exact reduced rational
//! carried as a numerator and a denominator rather than a float: the crush limit is a
//! comparison, which cross multiplication answers without dividing at all, and the boundary is
//! inclusive, so a value one part in a million over the limit has to land on the right side of
//! it.
//!
//! The fraction is reduced on construction. Unreduced, the divisor in the height formula
//! reaches `limit * 1e6 * denominator`, which passes `i128` for a heavy load over a large
//! footprint; reduced, the tick constants' shared factors of two and five collapse it to
//! something small. The reduction is therefore load-bearing rather than tidiness, and the
//! arithmetic is checked so an overflow becomes a refusal instead of a wrong number.

use crate::units::{Length, Weight};

/// Parts per million, the scale `compression_ratio` is carried at once parsed.
pub const PPM: i128 = 1_000_000;

/// Conventional standard gravity, exactly 9.80665 m/s^2, as a rational.
const GRAVITY_NUMERATOR: i128 = 980_665;
const GRAVITY_DENOMINATOR: i128 = 100_000;

const PASCALS_PER_KILOPASCAL: i128 = 1_000;

/// Applied pressure exceeded what the arithmetic can carry exactly.
///
/// A refusal rather than a wrapped number: this path decides whether an item survives, and
/// the one thing worse than declining to answer is answering wrongly.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompressionOverflow;

/// An exact pressure in kPa, held as a reduced non-negative rational.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pressure {
    pub numerator: i128,
    pub denominator: i128,
}

impl Pressure {
    fn reduced(numerator: i128, denominator: i128) -> Self {
        let divisor = gcd(numerator, denominator).max(1);
        Self {
            numerator: numerator / divisor,
            denominator: denominator / divisor,
        }
    }

    /// Cross multiplication, so the comparison never leaves exact integer arithmetic.
    pub fn exceeds(&self, limit_kilopascals: i64) -> Result<bool, CompressionOverflow> {
        let ceiling = i128::from(limit_kilopascals)
            .checked_mul(self.denominator)
            .ok_or(CompressionOverflow)?;
        Ok(self.numerator > ceiling)
    }
}

/// Pressure from the cumulative mass resting above an item, over its footprint.
///
/// The item's own mass is excluded -- it is not a load on itself -- and the footprint is the
/// uncompressed one, which compression never changes.
pub fn applied_pressure(
    top_load_ticks: i64,
    footprint_area_ticks: i128,
) -> Result<Pressure, CompressionOverflow> {
    if footprint_area_ticks <= 0 {
        return Err(CompressionOverflow);
    }
    let metre = i128::from(Length::TICKS_PER_MM) * 1_000;
    let numerator = i128::from(top_load_ticks)
        .checked_mul(GRAVITY_NUMERATOR)
        .and_then(|value| value.checked_mul(metre))
        .and_then(|value| value.checked_mul(metre))
        .ok_or(CompressionOverflow)?;
    let denominator = i128::from(Weight::TICKS_PER_KG)
        .checked_mul(GRAVITY_DENOMINATOR)
        .and_then(|value| value.checked_mul(PASCALS_PER_KILOPASCAL))
        .and_then(|value| value.checked_mul(footprint_area_ticks))
        .ok_or(CompressionOverflow)?;
    Ok(Pressure::reduced(numerator, denominator))
}

/// Occupied height under load, rounded up, never below one tick.
///
/// Rounding up keeps a discrete packer honest: it may never claim less space than the
/// continuous model allows. The one-tick floor stops a fully compressible item reaching zero
/// height, where it would slip past collision and support invariants entirely rather than
/// merely occupying very little.
pub fn effective_height(
    height_ticks: i64,
    ratio_ppm: i64,
    limit_kilopascals: i64,
    pressure: Pressure,
) -> Result<i64, CompressionOverflow> {
    if height_ticks <= 0 || ratio_ppm < 0 || i128::from(ratio_ppm) > PPM || limit_kilopascals < 0 {
        return Err(CompressionOverflow);
    }
    // With no headroom declared the only admissible pressure is zero, so the item is simply
    // uncompressed. Returning here also keeps the divisor below non-zero.
    if limit_kilopascals == 0 {
        return Ok(height_ticks);
    }
    let divisor = i128::from(limit_kilopascals)
        .checked_mul(PPM)
        .and_then(|value| value.checked_mul(pressure.denominator))
        .ok_or(CompressionOverflow)?;
    // Non-negative: the crush guard bounds the numerator by `limit * denominator` and the
    // ratio by PPM, so the product cannot exceed the divisor.
    let retained = divisor
        - i128::from(ratio_ppm)
            .checked_mul(pressure.numerator)
            .ok_or(CompressionOverflow)?;
    let scaled = i128::from(height_ticks)
        .checked_mul(retained)
        .ok_or(CompressionOverflow)?;
    let rounded = (scaled + divisor - 1) / divisor;
    Ok(rounded.max(1) as i64)
}

/// The published ratio rule, `floor(ratio * 1000000 + 0.5)`, applied once at the boundary.
///
/// Applied once and here, so the float a caller supplied never reaches the geometry.
pub fn ratio_to_ppm(ratio: f64) -> Option<i64> {
    if !(0.0..=1.0).contains(&ratio) {
        return None;
    }
    Some((ratio * PPM as f64 + 0.5) as i64)
}

fn gcd(mut a: i128, mut b: i128) -> i128 {
    while b != 0 {
        let next = a % b;
        a = b;
        b = next;
    }
    a.abs()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kilopascals(value: i128) -> Pressure {
        Pressure {
            numerator: value,
            denominator: 1,
        }
    }

    #[test]
    fn the_documented_compression_examples() {
        assert_eq!(effective_height(100, 250_000, 100, kilopascals(0)), Ok(100));
        assert_eq!(
            effective_height(100, 250_000, 100, kilopascals(50)),
            Ok(88),
            "87.5 rounds up"
        );
        assert_eq!(
            effective_height(100, 250_000, 100, kilopascals(100)),
            Ok(75)
        );
    }

    #[test]
    fn the_crush_boundary_is_inclusive() {
        assert_eq!(kilopascals(100).exceeds(100), Ok(false));
        let just_over = Pressure {
            numerator: 100_000_001,
            denominator: 1_000_000,
        };
        assert_eq!(just_over.exceeds(100), Ok(true));
    }

    #[test]
    fn a_zero_limit_admits_only_zero_pressure() {
        assert_eq!(effective_height(40, PPM as i64, 0, kilopascals(0)), Ok(40));
        let sliver = Pressure {
            numerator: 1,
            denominator: 1_000_000,
        };
        assert_eq!(sliver.exceeds(0), Ok(true));
    }

    /// Zero height would let an item escape collision and support invariants rather than
    /// merely occupy very little, so the floor is part of the contract.
    #[test]
    fn a_fully_compressible_item_still_occupies_one_tick() {
        assert_eq!(
            effective_height(100, PPM as i64, 10, kilopascals(10)),
            Ok(1)
        );
    }

    /// One kilogram over one square metre is 9.80665 Pa, so 980665/100000000 kPa, which
    /// reduces to 196133/20000000. Reduced, so two engines agreeing on the value cannot
    /// disagree on the representation -- and because the unreduced denominator would push the
    /// height formula's divisor past `i128`.
    #[test]
    fn pressure_is_exact_under_standard_gravity() {
        let metre = i128::from(Length::TICKS_PER_MM) * 1_000;
        let pressure = applied_pressure(Weight::TICKS_PER_KG, metre * metre).unwrap();
        assert_eq!(
            (pressure.numerator, pressure.denominator),
            (196_133, 20_000_000)
        );
    }

    #[test]
    fn the_ratio_rule_is_applied_once_at_the_boundary() {
        assert_eq!(ratio_to_ppm(0.25), Some(250_000));
        assert_eq!(ratio_to_ppm(0.0), Some(0));
        assert_eq!(ratio_to_ppm(1.0), Some(PPM as i64));
        assert_eq!(ratio_to_ppm(1.5), None);
    }
}
