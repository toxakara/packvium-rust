//! Exact integer and rational arithmetic with Python's semantics, in `i128`.
//!
//! The reference computes on unbounded integers and `Fraction`. Every quantity a real request
//! produces fits `i128` (a 13.6 m trailer is about 4e32 cubic ticks against a ceiling of 1.7e38);
//! the two products that do not, a box's volume times a weight unit and a trailer's volume in
//! parts per million, go through `mul_div` with a 256-bit intermediate. Anything that still
//! overflows is refused, never wrapped.

use std::cmp::Ordering;

use super::MalformedResult;
use super::json::Res;

fn overflow() -> MalformedResult {
    MalformedResult::shape(
        "arithmetic overflow: a value is beyond what this validator computes exactly".to_owned(),
    )
}

pub(crate) fn add(left: i128, right: i128) -> Res<i128> {
    left.checked_add(right).ok_or_else(overflow)
}

pub(crate) fn sub(left: i128, right: i128) -> Res<i128> {
    left.checked_sub(right).ok_or_else(overflow)
}

pub(crate) fn mul(left: i128, right: i128) -> Res<i128> {
    left.checked_mul(right).ok_or_else(overflow)
}

pub(crate) fn abs(value: i128) -> Res<i128> {
    value.checked_abs().ok_or_else(overflow)
}

pub(crate) fn sum<I: IntoIterator<Item = i128>>(values: I) -> Res<i128> {
    values.into_iter().try_fold(0, add)
}

/// Python's `left // right`: rounds toward negative infinity.
pub(crate) fn floor_div(left: i128, right: i128) -> Res<i128> {
    if right == 0 {
        return Err(MalformedResult::shape("division by zero".to_owned()));
    }
    let quotient = left.checked_div(right).ok_or_else(overflow)?;
    let exact = quotient.checked_mul(right).ok_or_else(overflow)? == left;
    Ok(if !exact && ((left < 0) != (right < 0)) {
        quotient - 1
    } else {
        quotient
    })
}

/// Python's `left % right`: takes the sign of the divisor.
pub(crate) fn py_mod(left: i128, right: i128) -> Res<i128> {
    sub(left, mul(floor_div(left, right)?, right)?)
}

/// `-(-left // right)`, the reference's ceiling division.
pub(crate) fn ceil_div(left: i128, right: i128) -> Res<i128> {
    Ok(-floor_div(left.checked_neg().ok_or_else(overflow)?, right)?)
}

/// `left * right // divisor` with the product held exactly in 256 bits, floored as Python
/// floors. The quotient itself must fit `i128`.
pub(crate) fn mul_div(left: i128, right: i128, divisor: i128) -> Res<i128> {
    if divisor == 0 {
        return Err(MalformedResult::shape("division by zero".to_owned()));
    }
    let negative = (left < 0) != (right < 0) && left != 0 && right != 0;
    let (high, low) = wide_mul(left.unsigned_abs(), right.unsigned_abs());
    let (quotient, remainder) = wide_div(high, low, divisor.unsigned_abs()).ok_or_else(overflow)?;
    let quotient = i128::try_from(quotient).map_err(|_| overflow())?;
    let negative = negative != (divisor < 0);
    Ok(if negative {
        -quotient - i128::from(remainder != 0)
    } else {
        quotient
    })
}

/// The full 256-bit product of two `u128`, as (high, low).
fn wide_mul(left: u128, right: u128) -> (u128, u128) {
    const MASK: u128 = u64::MAX as u128;
    let (a1, a0) = (left >> 64, left & MASK);
    let (b1, b0) = (right >> 64, right & MASK);
    let low_low = a0 * b0;
    let cross_one = a1 * b0;
    let cross_two = a0 * b1;
    let high_high = a1 * b1;
    let middle = (low_low >> 64) + (cross_one & MASK) + (cross_two & MASK);
    let low = (low_low & MASK) | (middle << 64);
    let high = high_high + (cross_one >> 64) + (cross_two >> 64) + (middle >> 64);
    (high, low)
}

/// (high, low) divided by `divisor`, bit by bit; `None` when the quotient needs over 128 bits.
fn wide_div(high: u128, low: u128, divisor: u128) -> Option<(u128, u128)> {
    if high >= divisor {
        return None;
    }
    let mut remainder = high;
    let mut quotient: u128 = 0;
    for bit in (0..128).rev() {
        let carry = remainder >> 127;
        remainder = (remainder << 1) | ((low >> bit) & 1);
        quotient <<= 1;
        if carry == 1 || remainder >= divisor {
            remainder = remainder.wrapping_sub(divisor);
            quotient |= 1;
        }
    }
    Some((quotient, remainder))
}

fn gcd(mut left: i128, mut right: i128) -> i128 {
    left = left.abs();
    right = right.abs();
    while right != 0 {
        (left, right) = (right, left % right);
    }
    left
}

/// Python's `Fraction`: always reduced, the denominator always positive, so `numerator` and
/// `denominator` read the same numbers the reference reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Ratio {
    pub(crate) numerator: i128,
    pub(crate) denominator: i128,
}

impl Ratio {
    pub(crate) fn new(numerator: i128, denominator: i128) -> Res<Ratio> {
        if denominator == 0 {
            return Err(MalformedResult::shape(
                "Fraction with a zero denominator".to_owned(),
            ));
        }
        let divisor = gcd(numerator, denominator);
        let (mut numerator, mut denominator) = (numerator / divisor, denominator / divisor);
        if denominator < 0 {
            numerator = numerator.checked_neg().ok_or_else(overflow)?;
            denominator = denominator.checked_neg().ok_or_else(overflow)?;
        }
        Ok(Ratio {
            numerator,
            denominator,
        })
    }

    pub(crate) fn from_int(value: i128) -> Ratio {
        Ratio {
            numerator: value,
            denominator: 1,
        }
    }

    pub(crate) fn add(self, other: Ratio) -> Res<Ratio> {
        let common = gcd(self.denominator, other.denominator);
        let left = mul(self.numerator, other.denominator / common)?;
        let right = mul(other.numerator, self.denominator / common)?;
        Ratio::new(
            add(left, right)?,
            mul(self.denominator / common, other.denominator)?,
        )
    }

    /// Product, reduced crosswise first so the intermediate stays as small as the answer.
    pub(crate) fn mul(self, other: Ratio) -> Res<Ratio> {
        let first = gcd(self.numerator, other.denominator).max(1);
        let second = gcd(other.numerator, self.denominator).max(1);
        Ratio::new(
            mul(self.numerator / first, other.numerator / second)?,
            mul(self.denominator / second, other.denominator / first)?,
        )
    }

    pub(crate) fn mul_int(self, value: i128) -> Res<Ratio> {
        self.mul(Ratio::from_int(value))
    }

    pub(crate) fn negate(self) -> Res<Ratio> {
        Ratio::new(
            self.numerator.checked_neg().ok_or_else(overflow)?,
            self.denominator,
        )
    }

    pub(crate) fn compare(self, other: Ratio) -> Res<Ordering> {
        let left = mul_wide_sign(self.numerator, other.denominator);
        let right = mul_wide_sign(other.numerator, self.denominator);
        Ok(left.cmp(&right))
    }

    pub(crate) fn compare_int(self, value: i128) -> Res<Ordering> {
        self.compare(Ratio::from_int(value))
    }

    /// `numerator // denominator`.
    pub(crate) fn floor(self) -> Res<i128> {
        floor_div(self.numerator, self.denominator)
    }

    /// Python's `float(fraction)`: the nearest double, ties to even.
    pub(crate) fn to_f64(self) -> f64 {
        let magnitude = nearest_double(
            self.numerator.unsigned_abs(),
            self.denominator.unsigned_abs(),
        );
        if self.numerator < 0 {
            -magnitude
        } else {
            magnitude
        }
    }
}

/// A sign-aware 256-bit product, as an orderable tuple: (sign, high, low) with the magnitude
/// negated for negatives so the tuple order is the numeric order.
fn mul_wide_sign(left: i128, right: i128) -> (i8, u128, u128) {
    let negative = (left < 0) != (right < 0) && left != 0 && right != 0;
    let (high, low) = wide_mul(left.unsigned_abs(), right.unsigned_abs());
    if high == 0 && low == 0 {
        (0, 0, 0)
    } else if negative {
        (-1, !high, !low)
    } else {
        (1, high, low)
    }
}

/// The double nearest `numerator / denominator`, ties to even, by exact long division to more
/// bits than a double holds plus a sticky remainder bit.
fn nearest_double(numerator: u128, denominator: u128) -> f64 {
    if numerator == 0 {
        return 0.0;
    }
    let integer = numerator / denominator;
    let mut remainder = numerator % denominator;
    let mut mantissa = integer;
    let mut exponent: i32 = 0;
    while bit_length(mantissa) < 66 {
        // `remainder < denominator <= 2^127`, so doubling it cannot overflow.
        remainder <<= 1;
        mantissa = (mantissa << 1) | u128::from(remainder >= denominator);
        if remainder >= denominator {
            remainder -= denominator;
        }
        exponent -= 1;
    }
    let sticky = remainder != 0;
    let shift = bit_length(mantissa) - 53;
    let dropped = mantissa & ((1u128 << shift) - 1);
    let half = 1u128 << (shift - 1);
    mantissa >>= shift;
    exponent += shift as i32;
    if dropped > half || (dropped == half && (sticky || mantissa & 1 == 1)) {
        mantissa += 1;
        if bit_length(mantissa) > 53 {
            mantissa >>= 1;
            exponent += 1;
        }
    }
    (mantissa as f64) * 2f64.powi(exponent)
}

fn bit_length(value: u128) -> u32 {
    128 - value.leading_zeros()
}

/// `Fraction(text)`, by the grammar of CPython 3.11's `fractions._RATIONAL_FORMAT`: optional
/// whitespace and sign, then `n/d`, or a decimal with an optional exponent; digit groups may
/// be split by single underscores. Only ASCII digits are read.
pub(crate) fn parse_fraction(text: &str) -> Res<Ratio> {
    let invalid = || MalformedResult::shape(format!("Invalid literal for Fraction: {text:?}"));
    let body = text.trim();
    let (negative, body) = match body.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, body.strip_prefix('+').unwrap_or(body)),
    };
    let starts_well = body.starts_with(|c: char| c.is_ascii_digit())
        || (body.starts_with('.') && body[1..].starts_with(|c: char| c.is_ascii_digit()));
    if !starts_well {
        return Err(invalid());
    }
    let (whole, rest) = digit_group(body);
    let mut numerator = digits_value(&whole)?.unwrap_or(0);
    let mut denominator: i128 = 1;
    if let Some(after) = rest.strip_prefix('/') {
        let (digits, tail) = digit_group(after);
        if digits.is_empty() || !tail.is_empty() {
            return Err(invalid());
        }
        denominator = digits_value(&digits)?.ok_or_else(invalid)?;
    } else {
        let mut tail = rest;
        if let Some(after) = tail.strip_prefix('.') {
            let (decimals, after_decimals) = digit_group(after);
            if !decimals.is_empty() {
                let scale = pow10(decimals.len() as u32)?;
                numerator = add(
                    mul(numerator, scale)?,
                    digits_value(&decimals)?.unwrap_or(0),
                )?;
                denominator = scale;
            }
            tail = after_decimals;
        }
        if let Some(after) = tail.strip_prefix(['e', 'E']) {
            let (exponent_negative, after) = match after.strip_prefix('-') {
                Some(rest) => (true, rest),
                None => (false, after.strip_prefix('+').unwrap_or(after)),
            };
            let (digits, after_exponent) = digit_group(after);
            if digits.is_empty() {
                return Err(invalid());
            }
            let power = u32::try_from(digits_value(&digits)?.ok_or_else(invalid)?)
                .map_err(|_| overflow())?;
            let factor = pow10(power)?;
            if exponent_negative {
                denominator = mul(denominator, factor)?;
            } else {
                numerator = mul(numerator, factor)?;
            }
            tail = after_exponent;
        }
        if !tail.is_empty() {
            return Err(invalid());
        }
    }
    if negative {
        numerator = -numerator;
    }
    Ratio::new(numerator, denominator)
}

/// A run of `\d+(_\d+)*` (possibly empty) and what follows it.
fn digit_group(text: &str) -> (String, &str) {
    let mut digits = String::new();
    let mut end = 0;
    let bytes = text.as_bytes();
    while end < bytes.len() {
        if bytes[end].is_ascii_digit() {
            digits.push(bytes[end] as char);
            end += 1;
        } else if bytes[end] == b'_'
            && !digits.is_empty()
            && bytes.get(end + 1).is_some_and(u8::is_ascii_digit)
        {
            end += 1;
        } else {
            break;
        }
    }
    (digits, &text[end..])
}

fn digits_value(digits: &str) -> Res<Option<i128>> {
    if digits.is_empty() {
        return Ok(None);
    }
    digits.parse::<i128>().map(Some).map_err(|_| overflow())
}

fn pow10(power: u32) -> Res<i128> {
    10i128.checked_pow(power).ok_or_else(overflow)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn floor_division_rounds_toward_negative_infinity() {
        assert_eq!(floor_div(-7, 2).unwrap(), -4);
        assert_eq!(floor_div(7, -2).unwrap(), -4);
        assert_eq!(ceil_div(7, 2).unwrap(), 4);
        assert_eq!(py_mod(-7, 3).unwrap(), 2);
    }

    #[test]
    fn mul_div_holds_a_product_past_i128() {
        let volume = 9_600_000_000i128 * 9_600_000_000 * 9_600_000_000;
        let expected = volume / 16_000; // exact: 9.6e9 is a multiple of 16000 cubed out
        assert_eq!(
            mul_div(volume, 3_628_738_960, 3_628_738_960 * 16_000).unwrap(),
            expected
        );
        assert_eq!(mul_div(-7, 1, 2).unwrap(), -4);
    }

    #[test]
    fn fractions_parse_as_python_reads_them() {
        let cases = [
            ("1_000", (1000, 1)),
            (".5", (1, 2)),
            ("5.", (5, 1)),
            ("1e3", (1000, 1)),
            ("1E-2", (1, 100)),
            (" 3/16 ", (3, 16)),
            ("+.5e1", (5, 1)),
            ("1_0.0_1", (1001, 100)),
            ("-0.25", (-1, 4)),
        ];
        for (text, (numerator, denominator)) in cases {
            assert_eq!(
                parse_fraction(text).unwrap(),
                Ratio {
                    numerator,
                    denominator
                },
                "{text}"
            );
        }
        for bad in ["1 / 2", "0x10", "", ".", "1__0", "_1", "1/0", "None"] {
            assert!(parse_fraction(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn float_conversion_is_correctly_rounded() {
        assert_eq!(Ratio::new(5, 2_000_000).unwrap().to_f64(), 2.5e-6);
        assert_eq!(Ratio::new(1, 3).unwrap().to_f64(), 1.0 / 3.0);
        // 2^60 + 1 is not a double; the nearest one is 2^60, and the sticky bit decides a tie.
        assert_eq!(Ratio::from_int((1 << 60) + 1).to_f64(), 2f64.powi(60));
        assert_eq!(
            Ratio::from_int((1 << 54) + 3).to_f64(),
            ((1u64 << 54) + 4) as f64
        );
    }
}
