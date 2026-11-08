use crate::error::{PackError, PackResult};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::cmp::Ordering;

#[derive(
    Clone, Copy, Debug, Default, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct Length(pub i64);

impl Length {
    pub const TICKS_PER_MM: i64 = 16_000;
    pub const TICKS_PER_INCH: i64 = 406_400;

    pub fn parse(value: &Value, default_unit: &str) -> PackResult<Self> {
        bounded_ticks(value, default_unit, MeasureKind::Length).map(Self)
    }

    pub fn to_json(self, unit: &str) -> Value {
        let multiplier = match unit.to_ascii_lowercase().as_str() {
            "ticks" | "tick" => 1,
            "in" | "inch" | "inches" => Self::TICKS_PER_INCH,
            "cm" => Self::TICKS_PER_MM * 10,
            "m" => Self::TICKS_PER_MM * 1_000,
            "ft" => Self::TICKS_PER_INCH * 12,
            _ => Self::TICKS_PER_MM,
        };
        serde_json::json!({
            "ticks": self.0,
            "value": decimal_string(self.0 as i128, multiplier as i128, 8),
            "unit": unit,
        })
    }
}

#[derive(
    Clone, Copy, Debug, Default, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct Weight(pub i64);

impl Weight {
    pub const TICKS_PER_MG: i64 = 8_000;
    pub const TICKS_PER_G: i64 = 8_000_000;
    pub const TICKS_PER_KG: i64 = 8_000_000_000;
    pub const TICKS_PER_OZ: i64 = 226_796_185;
    pub const TICKS_PER_LB: i64 = 3_628_738_960;

    pub fn parse(value: &Value, default_unit: &str) -> PackResult<Self> {
        bounded_ticks(value, default_unit, MeasureKind::Weight).map(Self)
    }

    pub fn to_json(self, unit: &str) -> Value {
        let multiplier = match unit.to_ascii_lowercase().as_str() {
            "ticks" | "tick" => 1,
            "mg" => Self::TICKS_PER_MG,
            "kg" => Self::TICKS_PER_KG,
            "oz" => Self::TICKS_PER_OZ,
            "lb" | "lbs" => Self::TICKS_PER_LB,
            _ => Self::TICKS_PER_G,
        };
        serde_json::json!({
            "ticks": self.0,
            "value": decimal_string(self.0 as i128, multiplier as i128, 8),
            "unit": unit,
        })
    }
}

/// Which quantity a measure is, so the one parser can tell a length unit from a weight unit.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum MeasureKind {
    Length,
    Weight,
}

impl MeasureKind {
    /// Ticks per `unit`, matched ignoring case and surrounding space as the reference does.
    fn multiplier(self, unit: &str) -> Option<i64> {
        let unit = unit.trim().to_ascii_lowercase();
        Some(match (self, unit.as_str()) {
            (_, "tick" | "ticks") => 1,
            (Self::Length, "mm" | "millimeter" | "millimeters") => Length::TICKS_PER_MM,
            (Self::Length, "cm") => Length::TICKS_PER_MM * 10,
            (Self::Length, "m") => Length::TICKS_PER_MM * 1_000,
            (Self::Length, "in" | "inch" | "inches") => Length::TICKS_PER_INCH,
            (Self::Length, "ft") => Length::TICKS_PER_INCH * 12,
            (Self::Weight, "mg") => Weight::TICKS_PER_MG,
            (Self::Weight, "g") => Weight::TICKS_PER_G,
            (Self::Weight, "kg") => Weight::TICKS_PER_KG,
            (Self::Weight, "oz") => Weight::TICKS_PER_OZ,
            (Self::Weight, "lb" | "lbs") => Weight::TICKS_PER_LB,
            _ => return None,
        })
    }

    pub(crate) fn knows_unit(self, unit: &str) -> bool {
        self.multiplier(unit).is_some()
    }
}

/// The output unit a request without `output.length_unit` is written in: the
/// request's own `units.length`, spelt as the schema's output code, so `Inches` is `in`. A
/// unit no output code names -- the internal `tick` -- gives `mm`, the default of both blocks.
pub(crate) fn output_length_unit(request_unit: &str) -> &'static str {
    let multiplier = MeasureKind::Length.multiplier(request_unit);
    ["mm", "cm", "m", "in", "ft"]
        .into_iter()
        .find(|code| MeasureKind::Length.multiplier(code) == multiplier)
        .unwrap_or("mm")
}

/// A measure's ticks before any range check, so a caller can tell a negative measure from
/// one that does not parse at all. The text of the scalar comes back for error messages.
fn unbounded_ticks(
    value: &Value,
    default_unit: &str,
    kind: MeasureKind,
) -> PackResult<(String, i128)> {
    let (raw, unit) = scalar_and_unit(value, default_unit)?;
    let multiplier = kind
        .multiplier(&unit)
        .ok_or_else(|| PackError::UnsupportedUnit(unit.trim().to_ascii_lowercase()))?;
    let (numerator, denominator) = parse_rational(&raw)?;
    let scaled = numerator
        .checked_mul(multiplier as i128)
        .ok_or_else(|| PackError::InvalidNumber(raw.clone()))?;
    let ticks = round_half_even(scaled, denominator)?;
    Ok((raw, ticks))
}

fn bounded_ticks(value: &Value, default_unit: &str, kind: MeasureKind) -> PackResult<i64> {
    let (raw, ticks) = unbounded_ticks(value, default_unit, kind)?;
    i64::try_from(ticks)
        .ok()
        .filter(|ticks| *ticks >= 0)
        .ok_or(PackError::InvalidNumber(raw))
}

/// The measure's ticks, negative ones included; an error only when it does not parse.
pub(crate) fn measure_ticks(
    value: &Value,
    default_unit: &str,
    kind: MeasureKind,
) -> PackResult<i128> {
    unbounded_ticks(value, default_unit, kind).map(|(_, ticks)| ticks)
}

/// What a measure `measure_ticks` overflowed on is, read exactly.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Overflowed {
    Negative,
    /// Its ticks are past the ceiling the caller named.
    Beyond,
    /// Unreadable here too, or a vanishing fraction with a vast denominator.
    Unreadable,
}

/// Reads over decimal digits a measure `i128` could not hold, to say whether it is negative or
/// past `ceiling` ticks. Ticks round to nearest, so they pass an odd `ceiling` once
/// `2 n m >= (2 ceiling + 1) d`; the tie itself rounds up to even. O(digits^2).
pub(crate) fn overflowed_measure(
    value: &Value,
    default_unit: &str,
    kind: MeasureKind,
    ceiling: u64,
) -> Overflowed {
    let Ok((raw, unit)) = scalar_and_unit(value, default_unit) else {
        return Overflowed::Unreadable;
    };
    let (Some(multiplier), Some((negative, numerator, denominator))) =
        (kind.multiplier(&unit), exact_fraction(&raw))
    else {
        return Overflowed::Unreadable;
    };
    if numerator.is_empty() {
        return Overflowed::Unreadable;
    }
    if negative {
        return Overflowed::Negative;
    }
    let doubled = digits_mul(&numerator, &digits_of(2 * multiplier as u128));
    let threshold = digits_mul(&denominator, &digits_of(2 * u128::from(ceiling) + 1));
    if digits_cmp(&doubled, &threshold) == Ordering::Less {
        Overflowed::Unreadable
    } else {
        Overflowed::Beyond
    }
}

/// `parse_rational` over little-endian decimal digits, without leading zeros: the sign,
/// numerator and denominator, or `None` for any other text.
fn exact_fraction(text: &str) -> Option<(bool, Vec<u8>, Vec<u8>)> {
    let text = text.trim().replace('\u{00a0}', " ");
    let negative = text.starts_with('-');
    let signed = |part: &str| digits(part.strip_prefix(['+', '-']).unwrap_or(part));
    let (numerator, denominator) = if let Some((whole, fraction)) = text.split_once(' ')
        && let Some((numerator, denominator)) = fraction.split_once('/')
    {
        let denominator = digits(denominator.trim())?;
        let whole = digits_mul(&signed(whole)?, &denominator);
        (digits_add(&whole, &digits(numerator.trim())?), denominator)
    } else if let Some((numerator, denominator)) = text.split_once('/') {
        (signed(numerator.trim())?, digits(denominator.trim())?)
    } else if let Some((whole, fraction)) = text.split_once('.') {
        let shifted = format!("{}{fraction}", whole.trim_start_matches(['+', '-']));
        (
            digits(&shifted)?,
            digits(&format!("1{}", "0".repeat(fraction.len())))?,
        )
    } else {
        (signed(&text)?, vec![1])
    };
    if denominator.is_empty() {
        return None;
    }
    Some((negative && !numerator.is_empty(), numerator, denominator))
}

fn digits(text: &str) -> Option<Vec<u8>> {
    if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let mut digits: Vec<u8> = text.bytes().rev().map(|byte| byte - b'0').collect();
    while digits.last() == Some(&0) {
        digits.pop();
    }
    Some(digits)
}

fn digits_of(value: u128) -> Vec<u8> {
    digits(&value.to_string()).unwrap_or_default()
}

fn digits_add(a: &[u8], b: &[u8]) -> Vec<u8> {
    let mut sum = Vec::with_capacity(a.len().max(b.len()) + 1);
    let mut carry = 0;
    for index in 0..a.len().max(b.len()) {
        let digit = a.get(index).unwrap_or(&0) + b.get(index).unwrap_or(&0) + carry;
        sum.push(digit % 10);
        carry = digit / 10;
    }
    if carry > 0 {
        sum.push(carry);
    }
    sum
}

fn digits_mul(a: &[u8], b: &[u8]) -> Vec<u8> {
    let mut product = vec![0_u32; a.len() + b.len()];
    for (i, &left) in a.iter().enumerate() {
        for (j, &right) in b.iter().enumerate() {
            product[i + j] += u32::from(left) * u32::from(right);
        }
    }
    let mut carry = 0;
    for digit in &mut product {
        *digit += carry;
        carry = *digit / 10;
        *digit %= 10;
    }
    let mut product: Vec<u8> = product.into_iter().map(|digit| digit as u8).collect();
    while product.last() == Some(&0) {
        product.pop();
    }
    product
}

fn digits_cmp(a: &[u8], b: &[u8]) -> Ordering {
    a.len()
        .cmp(&b.len())
        .then_with(|| a.iter().rev().cmp(b.iter().rev()))
}

fn scalar_and_unit(value: &Value, default_unit: &str) -> PackResult<(String, String)> {
    match value {
        Value::Object(map) => {
            let raw = map
                .get("value")
                .ok_or_else(|| PackError::InvalidNumber("missing value".into()))?;
            let unit = map
                .get("unit")
                .and_then(Value::as_str)
                .unwrap_or(default_unit);
            Ok((scalar_text(raw)?, unit.to_owned()))
        }
        Value::String(text) => {
            let trimmed = text.trim();
            for suffix in [
                "millimeters",
                "millimeter",
                "inches",
                "ticks",
                "inch",
                "lbs",
                "tick",
                "mm",
                "cm",
                "ft",
                "in",
                "mg",
                "kg",
                "oz",
                "lb",
                "g",
                "m",
            ] {
                if let Some(prefix) = trimmed.strip_suffix(suffix)
                    && !prefix.trim().is_empty()
                {
                    return Ok((prefix.trim().to_owned(), suffix.to_owned()));
                }
            }
            Ok((trimmed.to_owned(), default_unit.to_owned()))
        }
        Value::Number(_) => Ok((scalar_text(value)?, default_unit.to_owned())),
        _ => Err(PackError::InvalidNumber(value.to_string())),
    }
}

fn scalar_text(value: &Value) -> PackResult<String> {
    match value {
        Value::String(value) => Ok(value.clone()),
        Value::Number(value) => Ok(value.to_string()),
        _ => Err(PackError::InvalidNumber(value.to_string())),
    }
}

fn parse_rational(text: &str) -> PackResult<(i128, i128)> {
    let text = text.trim().replace('\u{00a0}', " ");
    if let Some((whole, fraction)) = text.split_once(' ')
        && let Some((numerator, denominator)) = fraction.split_once('/')
    {
        let whole: i128 = whole
            .parse()
            .map_err(|_| PackError::InvalidNumber(text.clone()))?;
        let numerator: i128 = numerator
            .trim()
            .parse()
            .map_err(|_| PackError::InvalidNumber(text.clone()))?;
        let denominator: i128 = denominator
            .trim()
            .parse()
            .map_err(|_| PackError::InvalidNumber(text.clone()))?;
        if denominator <= 0 {
            return Err(PackError::InvalidNumber(text));
        }
        let sign = if whole < 0 { -1 } else { 1 };
        return Ok((whole * denominator + sign * numerator, denominator));
    }
    if let Some((numerator, denominator)) = text.split_once('/') {
        let numerator: i128 = numerator
            .trim()
            .parse()
            .map_err(|_| PackError::InvalidNumber(text.clone()))?;
        let denominator: i128 = denominator
            .trim()
            .parse()
            .map_err(|_| PackError::InvalidNumber(text.clone()))?;
        if denominator <= 0 {
            return Err(PackError::InvalidNumber(text));
        }
        return Ok((numerator, denominator));
    }
    if let Some((whole, fraction)) = text.split_once('.') {
        let negative = whole.starts_with('-');
        let absolute_whole = whole
            .trim_start_matches(['+', '-'])
            .parse::<i128>()
            .map_err(|_| PackError::InvalidNumber(text.clone()))?;
        let denominator = 10_i128
            .checked_pow(fraction.len() as u32)
            .ok_or_else(|| PackError::InvalidNumber(text.clone()))?;
        let fraction = fraction
            .parse::<i128>()
            .map_err(|_| PackError::InvalidNumber(text.clone()))?;
        let numerator = absolute_whole * denominator + fraction;
        return Ok((if negative { -numerator } else { numerator }, denominator));
    }
    Ok((
        text.parse()
            .map_err(|_| PackError::InvalidNumber(text.clone()))?,
        1,
    ))
}

fn round_half_even(numerator: i128, denominator: i128) -> PackResult<i128> {
    if denominator <= 0 {
        return Err(PackError::InvalidNumber("zero denominator".into()));
    }
    let sign = if numerator < 0 { -1 } else { 1 };
    let absolute = numerator.abs();
    let quotient = absolute / denominator;
    let remainder = absolute % denominator;
    let rounded = match (remainder * 2).cmp(&denominator) {
        Ordering::Less => quotient,
        Ordering::Greater => quotient + 1,
        Ordering::Equal if quotient % 2 == 0 => quotient,
        Ordering::Equal => quotient + 1,
    };
    Ok(sign * rounded)
}

/// Renders `numerator / denominator` rounded to `places` digits, ties to even
/// (matches Python's `Decimal.quantize` default context, rather than truncating the
/// remainder as this used to).
fn decimal_string(numerator: i128, denominator: i128, places: usize) -> String {
    let negative = numerator < 0;
    let magnitude = numerator.unsigned_abs();
    let denominator = denominator.unsigned_abs();
    let mut whole = magnitude / denominator;
    let mut remainder = magnitude % denominator;
    let mut fraction = String::with_capacity(places);
    for _ in 0..places {
        remainder *= 10;
        fraction.push(char::from(b'0' + (remainder / denominator) as u8));
        remainder %= denominator;
    }
    let last_digit = fraction
        .as_bytes()
        .last()
        .map(|digit| (digit - b'0') % 2 == 1)
        .unwrap_or(whole % 2 == 1);
    let round_up = remainder * 2 > denominator || (remainder * 2 == denominator && last_digit);
    if round_up {
        match fraction.rfind(|digit: char| digit != '9') {
            Some(index) => {
                let bumped = char::from(fraction.as_bytes()[index] + 1);
                fraction.replace_range(index..=index, &bumped.to_string());
                fraction.replace_range(index + 1.., &"0".repeat(fraction.len() - index - 1));
            }
            None => {
                whole += 1;
                fraction = "0".repeat(fraction.len());
            }
        }
    }
    while fraction.ends_with('0') {
        fraction.pop();
    }
    let sign = if negative && (whole != 0 || !fraction.is_empty()) {
        "-"
    } else {
        ""
    };
    if fraction.is_empty() {
        format!("{sign}{whole}")
    } else {
        format!("{sign}{whole}.{fraction}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const VAST: &str = "10000000000000000000000000000000000000000";
    const CEILING: u64 = (1 << 53) - 1;

    fn overflowed(text: &str) -> Overflowed {
        overflowed_measure(
            &Value::String(text.into()),
            "mm",
            MeasureKind::Length,
            CEILING,
        )
    }

    #[test]
    fn half_even_rounding_refuses_a_denominator_that_is_not_positive() {
        assert_eq!(round_half_even(5, 2).unwrap(), 2);
        assert_eq!(round_half_even(7, 2).unwrap(), 4);
        for denominator in [0, -2] {
            assert!(matches!(
                round_half_even(5, denominator),
                Err(PackError::InvalidNumber(message)) if message == "zero denominator"
            ));
        }
    }

    #[test]
    fn every_spelling_of_a_vast_length_is_past_the_ceiling() {
        for text in [
            VAST.to_owned(),
            format!("{VAST}.5"),
            format!("{VAST}/3"),
            format!("{VAST} 1/2"),
            format!("{VAST} cm"),
            format!("+{VAST}"),
        ] {
            assert_eq!(overflowed(&text), Overflowed::Beyond, "{text}");
        }
        let object = serde_json::json!({"value": VAST, "unit": "ticks"});
        assert_eq!(
            overflowed_measure(&object, "mm", MeasureKind::Length, CEILING),
            Overflowed::Beyond
        );
    }

    #[test]
    fn a_vast_negative_measure_is_negative() {
        assert_eq!(overflowed(&format!("-{VAST}")), Overflowed::Negative);
        assert_eq!(overflowed(&format!("-{VAST} 1/2")), Overflowed::Negative);
    }

    #[test]
    fn a_vanishing_or_unreadable_measure_is_neither() {
        for text in [
            format!("1/{VAST}"),
            format!("0/{VAST}"),
            format!("{VAST}/0"),
            format!("{VAST}x"),
            "-0".to_owned(),
        ] {
            assert_eq!(overflowed(&text), Overflowed::Unreadable, "{text}");
        }
        assert_eq!(
            overflowed_measure(&Value::Bool(true), "mm", MeasureKind::Length, CEILING),
            Overflowed::Unreadable
        );
        let furlongs = serde_json::json!({"value": VAST, "unit": "furlong"});
        assert_eq!(
            overflowed_measure(&furlongs, "mm", MeasureKind::Length, CEILING),
            Overflowed::Unreadable
        );
    }

    #[test]
    fn the_ceiling_itself_rounds_to_nearest_with_the_tie_to_even() {
        // MAX + 1/2 ticks rounds to MAX + 1, past the ceiling; MAX + 1/2 - 1/10 does not.
        let doubled = 2 * u128::from(CEILING) + 1;
        let tie = format!("{doubled}/2");
        let ticks = |text: &str| {
            overflowed_measure(
                &Value::String(text.into()),
                "ticks",
                MeasureKind::Length,
                CEILING,
            )
        };
        assert_eq!(ticks(&tie), Overflowed::Beyond);
        assert_eq!(
            ticks(&format!("{}/20", doubled * 10 - 2)),
            Overflowed::Unreadable
        );
        assert_eq!(digits_add(&[9, 9], &[1]), vec![0, 0, 1]);
    }

    #[test]
    fn each_unit_is_worth_its_definition_in_ticks() {
        // 1 in = 25.4 mm, 1 ft = 12 in, 1 lb = 453.59237 g, 1 oz = 1/16 lb.
        let one = |unit: &str| serde_json::json!({"value": "1", "unit": unit});
        for (unit, ticks) in [
            ("mm", 16_000),
            ("cm", 160_000),
            ("m", 16_000_000),
            ("in", 406_400),
            ("ft", 4_876_800),
            ("ticks", 1),
        ] {
            assert_eq!(Length::parse(&one(unit), "mm").unwrap().0, ticks, "{unit}");
        }
        for (unit, ticks) in [
            ("mg", 8_000),
            ("g", 8_000_000),
            ("kg", 8_000_000_000),
            ("oz", 226_796_185),
            ("lb", 3_628_738_960),
            ("ticks", 1),
        ] {
            assert_eq!(Weight::parse(&one(unit), "g").unwrap().0, ticks, "{unit}");
        }
    }

    #[test]
    fn fractional_inches_are_exact() {
        assert_eq!(
            Length::parse(&Value::String("12 3/8".into()), "in")
                .unwrap()
                .0,
            5_029_200
        );
    }

    #[test]
    fn pounds_are_exact() {
        assert_eq!(
            Weight::parse(&serde_json::json!({"value":"2","unit":"lb"}), "g")
                .unwrap()
                .0,
            7_257_477_920
        );
    }

    #[test]
    fn decimal_string_rounds_ties_to_even_matching_python() {
        // This used to truncate to "1.96849901"; Python's Decimal.quantize
        // (ROUND_HALF_EVEN, its default context) renders "1.96849902".
        assert_eq!(decimal_string(799_998, 406_400, 8), "1.96849902");
        assert_eq!(decimal_string(406_400, 16_000, 8), "25.4");
        assert_eq!(decimal_string(0, 16_000, 8), "0");
        assert_eq!(decimal_string(-406_400, 16_000, 8), "-25.4");
    }

    #[test]
    fn decimal_string_breaks_exact_ties_to_the_even_digit() {
        assert_eq!(decimal_string(1, 2, 0), "0");
        assert_eq!(decimal_string(3, 2, 0), "2");
        assert_eq!(decimal_string(5, 4, 1), "1.2");
        assert_eq!(decimal_string(7, 4, 1), "1.8");
    }

    #[test]
    fn decimal_string_carries_a_round_up_through_repeated_nines() {
        // 99999999/100000000 * 10^... rounds the trailing 9s all the way into the whole part.
        assert_eq!(decimal_string(1_999_999_999, 2_000_000_000, 8), "1");
    }
}
