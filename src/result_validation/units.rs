//! Measures to exact ticks, re-derived from the unit tables rather than taken from the engine.

use super::MalformedResult;
use super::json::{Json, Res};
use super::num::{Ratio, parse_fraction};
use super::pyfmt::py_str;

/// Ticks per unit, in the reference's insertion order. One length tick is 1/16000 mm, so an
/// international inch is exactly 406 400 ticks.
pub(crate) type Table = &'static [(&'static str, i128)];

pub(crate) const LENGTH: Table = &[
    ("mm", 16_000),
    ("cm", 160_000),
    ("m", 16_000_000),
    ("in", 406_400),
    ("inch", 406_400),
    ("inches", 406_400),
    ("ft", 4_876_800),
    ("tick", 1),
    ("ticks", 1),
];

/// One weight tick is 1/8 microgram.
pub(crate) const WEIGHT: Table = &[
    ("mg", 8_000),
    ("g", 8_000_000),
    ("kg", 8_000_000_000),
    ("oz", 226_796_185),
    ("lb", 3_628_738_960),
    ("lbs", 3_628_738_960),
    ("tick", 1),
    ("ticks", 1),
];

/// Every name the engines read in `units.length`, after stripping ASCII whitespace and lowering
/// ASCII case, to the [`LENGTH`] key it means.
const REQUEST_LENGTH_UNITS: &[(&str, &str)] = &[
    ("mm", "mm"),
    ("cm", "cm"),
    ("m", "m"),
    ("in", "in"),
    ("inch", "inch"),
    ("inches", "inches"),
    ("ft", "ft"),
    ("tick", "tick"),
    ("ticks", "ticks"),
    ("millimeter", "mm"),
    ("millimeters", "mm"),
];

/// `units.length` as every engine reads it: `units` or `units.length` absent or null
/// is millimetres; `units` that is not an object, or a length unit the engines do not know, is
/// refused naming the field.
pub(crate) fn length_unit(request: &Json) -> Res<&'static str> {
    let units = request.get_null("units")?;
    if units.is_null() {
        return Ok("mm");
    }
    if !matches!(units, Json::Dict(_)) {
        return Err(MalformedResult::contract(
            "units must be an object".to_owned(),
        ));
    }
    let key = match units.get_null("length")? {
        Json::Null => return Ok("mm"),
        Json::Str(text) => text
            .trim_matches(|c: char| matches!(c, ' ' | '\t' | '\n' | '\r' | '\x0b' | '\x0c'))
            .to_ascii_lowercase(),
        _ => String::new(),
    };
    REQUEST_LENGTH_UNITS
        .iter()
        .find(|(name, _)| *name == key)
        .map(|(_, unit)| *unit)
        .ok_or_else(|| {
            MalformedResult::contract("units.length must be one of mm, cm, m, in, ft".to_owned())
        })
}

pub(crate) const TICKS_PER_MM: i128 = 16_000;
pub(crate) const TICKS_PER_GRAM: i128 = 8_000_000;
pub(crate) const TICKS_PER_KG: i128 = 8_000_000_000;

/// `table[unit]`, an exact key.
pub(crate) fn per_unit(table: Table, unit: &str) -> Res<i128> {
    table
        .iter()
        .find(|(name, _)| *name == unit)
        .map(|(_, ticks)| *ticks)
        .ok_or_else(|| MalformedResult::shape(format!("unknown unit {unit:?}")))
}

/// `to_ticks`: a measure (a number, a string with an optional unit suffix, or
/// `{value, unit}`) to exact ticks, rounding halves to even.
pub(crate) fn to_ticks(measure: &Json, default_unit: &str, table: Table) -> Res<i128> {
    let mut unit = default_unit.to_owned();
    let raw: Json;
    match measure {
        Json::Dict(_) => {
            raw = measure.at("value")?.clone();
            if let Some(stated) = measure.get("unit")? {
                unit = stated.text("a measure's unit")?.to_owned();
            }
        }
        Json::Str(text) => {
            let stripped = text.trim();
            raw = match unit_suffix(stripped, table) {
                Some((number, suffix)) => {
                    unit = suffix.to_owned();
                    Json::Str(number)
                }
                None => measure.clone(),
            };
        }
        other => raw = other.clone(),
    }
    let scaled = parse_rational(&raw)?.mul_int(per_unit(table, &unit.to_lowercase())?)?;
    round_half_even(scaled)
}

/// The longest unit suffix the text ends with (case-insensitively) that leaves a non-empty
/// number before it; ties in length go to the table's earlier entry, as Python's stable sort
/// over the table does.
fn unit_suffix(stripped: &str, table: Table) -> Option<(String, &'static str)> {
    let mut suffixes: Vec<&'static str> = table.iter().map(|(name, _)| *name).collect();
    suffixes.sort_by_key(|name| std::cmp::Reverse(name.len()));
    let lowered = stripped.to_lowercase();
    let length = stripped.chars().count();
    suffixes.into_iter().find_map(|suffix| {
        if !lowered.ends_with(suffix) || length < suffix.len() {
            return None;
        }
        let number: String = stripped.chars().take(length - suffix.len()).collect();
        let number = number.trim().to_owned();
        (!number.is_empty()).then_some((number, suffix))
    })
}

fn round_half_even(scaled: Ratio) -> Res<i128> {
    let floor = scaled.floor()?;
    let remainder = Ratio::new(
        scaled.numerator - floor * scaled.denominator,
        scaled.denominator,
    )?;
    Ok(match remainder.compare(Ratio::new(1, 2)?)? {
        std::cmp::Ordering::Greater => floor + 1,
        std::cmp::Ordering::Less => floor,
        std::cmp::Ordering::Equal if floor % 2 == 0 => floor,
        std::cmp::Ordering::Equal => floor + 1,
    })
}

/// `parse_rational`: an integer exactly, anything else through its `str()` as a fraction,
/// a mixed fraction (`12 3/8`) when the text holds a space.
pub(crate) fn parse_rational(value: &Json) -> Res<Ratio> {
    if let Some(integer) = value.py_int_instance() {
        return Ok(Ratio::from_int(integer));
    }
    let text = py_str(value);
    let stripped = text.trim();
    if !stripped.contains(' ') {
        return parse_fraction(stripped);
    }
    let (whole, fraction) = stripped
        .split_once(char::is_whitespace)
        .unwrap_or((stripped, ""));
    let fraction = parse_fraction(fraction.trim_start())?;
    let signed = if whole.starts_with('-') {
        fraction.negate()?
    } else {
        fraction
    };
    parse_fraction(whole)?.add(signed)
}

/// `Fraction(str(value))`, the reference's reading of a ratio field.
pub(crate) fn ratio_of(value: &Json) -> Res<Ratio> {
    parse_fraction(&py_str(value))
}

pub(crate) fn dimensions(value: &Json, unit: &str) -> Res<[i128; 3]> {
    Ok([
        to_ticks(value.at("length")?, unit, LENGTH)?,
        to_ticks(value.at("width")?, unit, LENGTH)?,
        to_ticks(value.at("height")?, unit, LENGTH)?,
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ticks(measure: Json) -> i128 {
        to_ticks(&measure, "mm", LENGTH).unwrap()
    }

    #[test]
    fn measures_read_as_the_reference_reads_them() {
        assert_eq!(ticks(Json::Int(2)), 32_000);
        assert_eq!(ticks(Json::Float(0.5)), 8_000);
        assert_eq!(ticks(Json::str_of("12 3/8 in")), 12 * 406_400 + 152_400);
        assert_eq!(ticks(Json::str_of("-1 1/2 mm")), -24_000);
        assert_eq!(ticks(Json::str_of("3 ticks")), 3);
        assert_eq!(ticks(Json::str_of("10 CM")), 1_600_000);
        assert_eq!(ticks(Json::str_of("0.00003125")), 0); // exactly half a tick rounds to even
        assert_eq!(ticks(Json::str_of("0.00009375")), 2);
    }
}
