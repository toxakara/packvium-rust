//! A JSON value spelled as the Python reference's `str()` spells it.
//!
//! The plan's summaries, the CSV and the HTML work order interpolate values a result carried,
//! and the reference writes each with `str()`. A string is itself, but `None`, `True` and a
//! float have Python spellings (`None`, `True`, `1.0`, `1e-05`). Matching them keeps a result
//! that holds a float or a null where a string was expected byte-identical across engines.
//!
//! Arrays and objects are spelled as Python's `repr` spells the common case. They are not
//! byte-identical in general: the reference keeps an object's insertion order, which
//! `serde_json`'s map does not, and decides per character what is printable. No field a
//! well-formed result carries into an export holds one.

use std::fmt::Write as _;

use serde_json::{Number, Value};

use crate::canonical_json::shortest_digits;

/// `str(value)` for a JSON value.
pub(crate) fn text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => repr(other),
    }
}

/// `repr(value)` for a float that is finite, as the reader guarantees.
fn float_text(value: f64) -> String {
    if value == 0.0 {
        return if value.is_sign_negative() {
            "-0.0"
        } else {
            "0.0"
        }
        .to_string();
    }
    let (digits, point) = shortest_digits(value);
    let count = digits.len() as i32;
    let mut out = String::from(if value < 0.0 { "-" } else { "" });
    if -4 < point && point <= 16 {
        if point <= 0 {
            out.push_str("0.");
            out.push_str(&"0".repeat(point.unsigned_abs() as usize));
            out.push_str(&digits);
        } else if point >= count {
            out.push_str(&digits);
            out.push_str(&"0".repeat((point - count) as usize));
            out.push_str(".0");
        } else {
            let (whole, fraction) = digits.split_at(point as usize);
            let _ = write!(out, "{whole}.{fraction}");
        }
    } else {
        let power = point - 1;
        let (first, rest) = digits.split_at(1);
        out.push_str(first);
        if !rest.is_empty() {
            let _ = write!(out, ".{rest}");
        }
        let sign = if power >= 0 { '+' } else { '-' };
        let _ = write!(out, "e{sign}{:02}", power.unsigned_abs());
    }
    out
}

fn repr(value: &Value) -> String {
    match value {
        Value::Null => "None".to_string(),
        Value::Bool(true) => "True".to_string(),
        Value::Bool(false) => "False".to_string(),
        Value::Number(number) => number_text(number),
        Value::String(text) => string_repr(text),
        Value::Array(elements) => {
            let spelled: Vec<String> = elements.iter().map(repr).collect();
            format!("[{}]", spelled.join(", "))
        }
        Value::Object(object) => {
            let spelled: Vec<String> = object
                .iter()
                .map(|(key, element)| format!("{}: {}", string_repr(key), repr(element)))
                .collect();
            format!("{{{}}}", spelled.join(", "))
        }
    }
}

fn number_text(number: &Number) -> String {
    if number.is_f64() {
        return float_text(number.as_f64().unwrap_or(0.0));
    }
    number.to_string()
}

fn string_repr(text: &str) -> String {
    let quote = if text.contains('\'') && !text.contains('"') {
        '"'
    } else {
        '\''
    };
    let mut out = String::with_capacity(text.len() + 2);
    out.push(quote);
    for character in text.chars() {
        match character {
            '\\' => out.push_str("\\\\"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            quoted if quoted == quote => {
                out.push('\\');
                out.push(quoted);
            }
            control if (control as u32) < 0x20 || (0x7f..0xa0).contains(&(control as u32)) => {
                let _ = write!(out, "\\x{:02x}", control as u32);
            }
            other => out.push(other),
        }
    }
    out.push(quote);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn scalars_are_spelled_as_python_str_spells_them() {
        assert_eq!(text(&json!("as is")), "as is");
        assert_eq!(text(&Value::Null), "None");
        assert_eq!(text(&json!(true)), "True");
        assert_eq!(text(&json!(false)), "False");
        assert_eq!(text(&json!(-7)), "-7");
        assert_eq!(
            text(&json!(18446744073709551615_u64)),
            "18446744073709551615"
        );
    }

    #[test]
    fn floats_are_spelled_as_python_repr_spells_them() {
        let cases = [
            (0.5, "0.5"),
            (1.0, "1.0"),
            (123.0, "123.0"),
            (-0.0, "-0.0"),
            (0.0, "0.0"),
            (0.0001, "0.0001"),
            (0.00001, "1e-05"),
            (1.5e-7, "1.5e-07"),
            (1e16, "1e+16"),
            (1234567890123456.0, "1234567890123456.0"),
            (12345678901234567.0, "1.2345678901234568e+16"),
            (333333333.3333333, "333333333.3333333"),
            (-2.5, "-2.5"),
            (5e-324, "5e-324"),
        ];
        for (value, spelled) in cases {
            assert_eq!(float_text(value), spelled, "{value:e}");
        }
    }

    #[test]
    fn compound_values_are_spelled_as_python_repr_spells_the_common_case() {
        assert_eq!(
            text(&json!([1, "a'b", null, {"k": "v"}])),
            r#"[1, "a'b", None, {'k': 'v'}]"#
        );
    }

    #[test]
    fn strings_inside_a_compound_value_are_escaped_as_python_repr_escapes_them() {
        // Expected text produced by CPython's own `str()` of the same list.
        let value = json!([
            "back\\slash\ttab\nline\rreturn",
            "it's \"quoted\"",
            "only 'single'",
            "\u{1}\u{7f}\u{85}"
        ]);
        assert_eq!(
            text(&value),
            r#"['back\\slash\ttab\nline\rreturn', 'it\'s "quoted"', "only 'single'", '\x01\x7f\x85']"#
        );
    }
}
