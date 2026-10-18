//! Python's `str()` and `repr()` for the values an issue detail prints.
//!
//! The reference builds every detail with an f-string, so a detail carries Python's spelling of
//! whatever it names: `None`, `True`, `'quoted'`, `(1, 2, 3)`, `1e-05`. Matching it is what
//! lets one issue list be compared across languages byte for byte.

use super::json::Json;

/// `str(value)`: a string as itself, anything else as its `repr`.
pub(crate) fn py_str(value: &Json) -> String {
    match value {
        Json::Str(text) => text.clone(),
        other => py_repr(other),
    }
}

pub(crate) fn py_repr(value: &Json) -> String {
    match value {
        Json::Null => "None".to_owned(),
        Json::Bool(true) => "True".to_owned(),
        Json::Bool(false) => "False".to_owned(),
        Json::Int(number) => number.to_string(),
        Json::Float(number) => float_repr(*number),
        Json::Str(text) => str_repr(text),
        Json::List(values) => list(values.iter().map(py_repr)),
        Json::Dict(pairs) => {
            let inner: Vec<String> = pairs
                .iter()
                .map(|(key, value)| format!("{}: {}", str_repr(key), py_repr(value)))
                .collect();
            format!("{{{}}}", inner.join(", "))
        }
    }
}

pub(crate) fn bool_repr(flag: bool) -> &'static str {
    if flag { "True" } else { "False" }
}

/// `[a, b]` from already-formatted elements.
pub(crate) fn list<I: IntoIterator<Item = String>>(elements: I) -> String {
    format!("[{}]", elements.into_iter().collect::<Vec<_>>().join(", "))
}

/// `(a, b)` from already-formatted elements; a one-element tuple keeps its trailing comma.
pub(crate) fn tuple<I: IntoIterator<Item = String>>(elements: I) -> String {
    let elements: Vec<String> = elements.into_iter().collect();
    match elements.len() {
        1 => format!("({},)", elements[0]),
        _ => format!("({})", elements.join(", ")),
    }
}

pub(crate) fn int_tuple(values: &[i128]) -> String {
    tuple(values.iter().map(i128::to_string))
}

pub(crate) fn int_list(values: &[i128]) -> String {
    list(values.iter().map(i128::to_string))
}

/// `repr(float)`: the shortest digits that round-trip, in fixed notation for decimal
/// exponents from -4 to 15 and scientific (`1e-05`, `1e+16`) outside that.
pub(crate) fn float_repr(value: f64) -> String {
    if value.is_nan() {
        return "nan".to_owned();
    }
    if value.is_infinite() {
        return if value > 0.0 { "inf" } else { "-inf" }.to_owned();
    }
    let sign = if value.is_sign_negative() { "-" } else { "" };
    let scientific = format!("{:e}", value.abs());
    let (mantissa, exponent) = scientific.split_once('e').unwrap_or((&scientific, "0"));
    let exponent: i32 = exponent.parse().unwrap_or(0);
    let digits: String = mantissa.chars().filter(char::is_ascii_digit).collect();
    if (-4..16).contains(&exponent) {
        let point = exponent + 1;
        let body = if point <= 0 {
            format!("0.{}{}", "0".repeat((-point) as usize), digits)
        } else if digits.len() as i32 <= point {
            format!(
                "{}{}.0",
                digits,
                "0".repeat((point - digits.len() as i32) as usize)
            )
        } else {
            format!(
                "{}.{}",
                &digits[..point as usize],
                &digits[point as usize..]
            )
        };
        return format!("{sign}{body}");
    }
    let fraction = if digits.len() > 1 {
        format!(".{}", &digits[1..])
    } else {
        String::new()
    };
    let exponent_sign = if exponent < 0 { '-' } else { '+' };
    format!(
        "{sign}{}{fraction}e{exponent_sign}{:02}",
        &digits[..1],
        exponent.abs()
    )
}

/// `f"{value:.6f}"`. Rust and Python both round the exact binary value half to even.
pub(crate) fn fixed6(value: f64) -> String {
    format!("{value:.6}")
}

/// `repr(str)`: single quotes unless the text holds a single quote and no double quote,
/// Python's escapes for the backslash, the quote, `\t`, `\n`, `\r` and other non-printables.
pub(crate) fn str_repr(text: &str) -> String {
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
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c if (c as u32) < 0x20 || c as u32 == 0x7f => {
                out.push_str(&format!("\\x{:02x}", c as u32))
            }
            c if (c as u32) < 0x7f || printable(c) => out.push(c),
            c if (c as u32) <= 0xff => out.push_str(&format!("\\x{:02x}", c as u32)),
            c if (c as u32) <= 0xffff => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push_str(&format!("\\U{:08x}", c as u32)),
        }
    }
    out.push(quote);
    out
}

/// Python's `str.isprintable` for a character at or above U+007F, from the generated table.
fn printable(character: char) -> bool {
    !super::nonprintable::contains(character as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn floats_print_as_python_repr() {
        let cases = [
            (1e16, "1e+16"),
            (1e-5, "1e-05"),
            (0.0001, "0.0001"),
            (123456789012345678.0, "1.2345678901234568e+17"),
            (0.1, "0.1"),
            (-0.0, "-0.0"),
            (5.0, "5.0"),
            (1e15, "1000000000000000.0"),
            (2.5e-6, "2.5e-06"),
        ];
        for (value, expected) in cases {
            assert_eq!(float_repr(value), expected);
        }
    }

    #[test]
    fn strings_print_as_python_repr() {
        assert_eq!(str_repr("abc"), "'abc'");
        assert_eq!(str_repr("\u{e0001}\u{600}"), "'\\U000e0001\\u0600'");
        assert_eq!(str_repr("\u{378}\u{e000}é"), "'\\u0378\\ue000é'");
        assert_eq!(super::super::nonprintable::UNICODE_VERSION, "14.0.0");
        assert_eq!(str_repr("it's"), "\"it's\"");
        assert_eq!(str_repr("'\""), "'\\'\"'");
        assert_eq!(str_repr("a\nb\\"), "'a\\nb\\\\'");
        assert_eq!(str_repr("\u{7}é\u{200b}"), "'\\x07é\\u200b'");
    }

    #[test]
    fn containers_print_as_python_repr() {
        let value = Json::Dict(vec![
            (
                "a".to_owned(),
                Json::List(vec![Json::Int(1), Json::Null, Json::Bool(true)]),
            ),
            ("b".to_owned(), Json::Str("x".to_owned())),
        ]);
        assert_eq!(py_repr(&value), "{'a': [1, None, True], 'b': 'x'}");
        assert_eq!(int_tuple(&[1, 2, 3]), "(1, 2, 3)");
        assert_eq!(tuple(["7".to_owned()]), "(7,)");
        assert_eq!(fixed6(2.5e-6), "0.000003");
    }

    #[test]
    fn special_floats_and_escapes_print_as_python_prints_them() {
        assert_eq!(float_repr(f64::NAN), "nan");
        assert_eq!(float_repr(f64::INFINITY), "inf");
        assert_eq!(float_repr(f64::NEG_INFINITY), "-inf");
        assert_eq!(float_repr(1.5), "1.5");
        assert_eq!(str_repr("a\tb\rc"), "'a\\tb\\rc'");
        assert_eq!(str_repr("\u{2028}"), "'\\u2028'");
        assert_eq!(str_repr("\u{f0000}"), "'\\U000f0000'");
    }
}
