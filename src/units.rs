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
        let (raw, unit) = scalar_and_unit(value, default_unit)?;
        let multiplier = match unit.to_ascii_lowercase().as_str() {
            "tick" | "ticks" => 1,
            "mm" | "millimeter" | "millimeters" => Self::TICKS_PER_MM,
            "cm" => Self::TICKS_PER_MM * 10,
            "m" => Self::TICKS_PER_MM * 1_000,
            "in" | "inch" | "inches" => Self::TICKS_PER_INCH,
            "ft" => Self::TICKS_PER_INCH * 12,
            other => return Err(PackError::UnsupportedUnit(other.to_owned())),
        };
        let (numerator, denominator) = parse_rational(&raw)?;
        let scaled = numerator
            .checked_mul(multiplier as i128)
            .ok_or_else(|| PackError::InvalidNumber(raw.clone()))?;
        let ticks = round_half_even(scaled, denominator)?;
        if !(0..=i64::MAX as i128).contains(&ticks) {
            return Err(PackError::InvalidNumber(raw));
        }
        Ok(Self(ticks as i64))
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
        let (raw, unit) = scalar_and_unit(value, default_unit)?;
        let multiplier = match unit.to_ascii_lowercase().as_str() {
            "tick" | "ticks" => 1,
            "mg" => Self::TICKS_PER_MG,
            "g" => Self::TICKS_PER_G,
            "kg" => Self::TICKS_PER_KG,
            "oz" => Self::TICKS_PER_OZ,
            "lb" | "lbs" => Self::TICKS_PER_LB,
            other => return Err(PackError::UnsupportedUnit(other.to_owned())),
        };
        let (numerator, denominator) = parse_rational(&raw)?;
        let scaled = numerator
            .checked_mul(multiplier as i128)
            .ok_or_else(|| PackError::InvalidNumber(raw.clone()))?;
        let ticks = round_half_even(scaled, denominator)?;
        if !(0..=i64::MAX as i128).contains(&ticks) {
            return Err(PackError::InvalidNumber(raw));
        }
        Ok(Self(ticks as i64))
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
