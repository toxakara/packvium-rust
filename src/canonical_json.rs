//! RFC 8785 canonical JSON: the one spelling four engines can agree on byte for byte.
//!
//! Held to byte-identical output with `packvium._canonical_json`. A document that is compared,
//! signed or replayed across Python, PHP, Rust and JavaScript needs one serialization, and
//! every language's default writer disagrees with the others somewhere. `serde_json` sorts
//! keys by code point where JavaScript sorts by UTF-16 code unit, and prints `1.0` where
//! JavaScript prints `1`. RFC 8785 settles both the way JavaScript already behaves.
//!
//! - Object keys are sorted by their UTF-16 code units.
//! - Strings escape `"`, `\` and U+0000..U+001F (`\b \t \n \f \r` by name, the rest as
//!   lowercase `\u00xx`); everything else is written as UTF-8.
//! - Numbers are written as ECMAScript's `Number::toString` writes them.
//! - No whitespace.
//!
//! A number whose magnitude exceeds 2^53 - 1, or that is not finite, is refused: JavaScript
//! already holds a different number by the time it can see it.
//!
//! The module also reads JSON text, because `serde_json`'s default float parser is
//! best-effort: it reads `333333333.33333329` one ULP away from the nearest double, and the
//! shortest spelling of that neighbour is different digits. The reader here hands every
//! number to the standard library's correctly rounded parser instead.

use std::cmp::Ordering;
use std::fmt::Write as _;

use serde_json::{Map, Number, Value};

/// The largest magnitude every engine holds exactly.
pub(crate) const MAX_EXACT_MAGNITUDE: u64 = (1 << 53) - 1;

/// Nesting deeper than this is refused rather than risking the stack on hostile input.
const MAX_DEPTH: usize = 512;

/// Which canonical-form rule a value or a text broke.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CanonicalJsonErrorCode {
    NumberOutOfRange,
    InvalidString,
    InvalidJson,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CanonicalJsonError {
    pub(crate) code: CanonicalJsonErrorCode,
    pub(crate) message: String,
}

type CanonicalResult<T> = Result<T, CanonicalJsonError>;

fn refuse<T>(code: CanonicalJsonErrorCode, message: impl Into<String>) -> CanonicalResult<T> {
    Err(CanonicalJsonError {
        code,
        message: message.into(),
    })
}

// ---------------------------------------------------------------------------------- writing

/// The RFC 8785 spelling of `value`. O(N) for a document of N bytes, plus O(k log k) key
/// comparisons per object of k keys.
pub(crate) fn to_canonical_string(value: &Value) -> CanonicalResult<String> {
    let mut out = String::new();
    write_value(value, &mut out)?;
    Ok(out)
}

/// The integer a JSON value is, judged by value as every engine can judge it.
///
/// `1.0` is `1`, because JavaScript cannot tell them apart once the text is parsed; `true`,
/// `"1"` and `1.5` are not integers, and neither is anything past 2^53 - 1, which JavaScript
/// no longer holds exactly. The bound is also why `+ 1` on the answer can never overflow.
pub(crate) fn json_integer(value: &Value) -> Option<i64> {
    let Value::Number(number) = value else {
        return None;
    };
    let integer = match number.as_i64() {
        Some(integer) => integer,
        None if number.is_u64() => return None,
        None => {
            let float = number.as_f64()?;
            if !float.is_finite()
                || float.fract() != 0.0
                || float.abs() > MAX_EXACT_MAGNITUDE as f64
            {
                return None;
            }
            float as i64
        }
    };
    (integer.unsigned_abs() <= MAX_EXACT_MAGNITUDE).then_some(integer)
}

/// How a refusal quotes a value: its canonical JSON, the one spelling four engines share.
pub(crate) fn json_spelling(value: &Value) -> String {
    to_canonical_string(value).unwrap_or_else(|error| match error.code {
        CanonicalJsonErrorCode::NumberOutOfRange => "an out-of-range number".into(),
        _ => "an unspellable value".into(),
    })
}

/// A list of field names as a refusal quotes it, e.g. `["note"]`.
pub(crate) fn spell_names(names: &[&str]) -> String {
    json_spelling(&Value::Array(
        names
            .iter()
            .map(|name| Value::String((*name).to_owned()))
            .collect(),
    ))
}

fn write_value(value: &Value, out: &mut String) -> CanonicalResult<()> {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(true) => out.push_str("true"),
        Value::Bool(false) => out.push_str("false"),
        Value::Number(number) => write_number(number, out)?,
        Value::String(text) => write_string(text, out),
        Value::Array(elements) => {
            out.push('[');
            for (index, element) in elements.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                write_value(element, out)?;
            }
            out.push(']');
        }
        Value::Object(object) => write_object(object, out)?,
    }
    Ok(())
}

fn write_object(object: &Map<String, Value>, out: &mut String) -> CanonicalResult<()> {
    let mut keys: Vec<&String> = object.keys().collect();
    // `Map` iterates in byte order, which is code-point order. That differs from UTF-16
    // code-unit order for a key outside the Basic Multilingual Plane against one in
    // U+E000..U+FFFF, and JavaScript sorts the second way.
    keys.sort_by(|left, right| utf16_order(left, right));
    out.push('{');
    for (index, key) in keys.into_iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        write_string(key, out);
        out.push(':');
        write_value(&object[key.as_str()], out)?;
    }
    out.push('}');
    Ok(())
}

fn utf16_order(left: &str, right: &str) -> Ordering {
    left.encode_utf16().cmp(right.encode_utf16())
}

fn write_string(text: &str, out: &mut String) {
    out.push('"');
    for character in text.chars() {
        match character {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{8}' => out.push_str("\\b"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\u{c}' => out.push_str("\\f"),
            '\r' => out.push_str("\\r"),
            control if control < '\u{20}' => {
                let _ = write!(out, "\\u{:04x}", control as u32);
            }
            other => out.push(other),
        }
    }
    out.push('"');
}

fn write_number(number: &Number, out: &mut String) -> CanonicalResult<()> {
    if let Some(unsigned) = number.as_u64() {
        return write_integer(unsigned, false, out);
    }
    if let Some(signed) = number.as_i64() {
        return write_integer(signed.unsigned_abs(), signed < 0, out);
    }
    write_float(number.as_f64().unwrap_or(f64::NAN), out)
}

fn write_integer(magnitude: u64, negative: bool, out: &mut String) -> CanonicalResult<()> {
    if magnitude > MAX_EXACT_MAGNITUDE {
        let sign = if negative { "-" } else { "" };
        return refuse(
            CanonicalJsonErrorCode::NumberOutOfRange,
            format!("{sign}{magnitude} is beyond what every engine holds exactly"),
        );
    }
    if negative {
        out.push('-');
    }
    let _ = write!(out, "{magnitude}");
    Ok(())
}

fn write_float(value: f64, out: &mut String) -> CanonicalResult<()> {
    if !value.is_finite() || value.abs() > MAX_EXACT_MAGNITUDE as f64 {
        return refuse(
            CanonicalJsonErrorCode::NumberOutOfRange,
            format!("{value:e} is beyond what every engine holds exactly"),
        );
    }
    if value == 0.0 {
        out.push('0');
        return Ok(());
    }
    let (digits, point) = shortest_digits(value);
    let count = digits.len() as i32;
    if value < 0.0 {
        out.push('-');
    }
    if count <= point && point <= 21 {
        out.push_str(&digits);
        push_zeros(out, point - count);
    } else if 0 < point && point <= 21 {
        let (whole, fraction) = digits.split_at(point as usize);
        out.push_str(whole);
        out.push('.');
        out.push_str(fraction);
    } else if -6 < point && point <= 0 {
        out.push_str("0.");
        push_zeros(out, -point);
        out.push_str(&digits);
    } else {
        let power = point - 1;
        let (first, rest) = digits.split_at(1);
        out.push_str(first);
        if !rest.is_empty() {
            out.push('.');
            out.push_str(rest);
        }
        let sign = if power >= 0 { '+' } else { '-' };
        let _ = write!(out, "e{sign}{}", power.unsigned_abs());
    }
    Ok(())
}

fn push_zeros(out: &mut String, count: i32) {
    for _ in 0..count {
        out.push('0');
    }
}

/// The shortest digit string that round-trips `value`, without trailing zeros, and the
/// decimal point's position: `value == 0.DIGITS * 10^point` in magnitude. This is the digit
/// string ECMAScript and Python's `repr` both start from; only the layout around it differs.
///
/// `{:e}` rounds an exact tie half up. When the value lies exactly halfway between the two
/// shortest candidates -- `1561489596588225.25` between `...225.2` and `...225.3` -- ECMAScript
/// and Python's `repr` both take the even one, so the tie is settled here. O(1): a double's
/// exact expansion has at most 767 significant digits.
pub(crate) fn shortest_digits(value: f64) -> (String, i32) {
    let magnitude = value.abs();
    let (digits, point) = scientific_digits(&format!("{magnitude:e}"));
    let (exact, exact_point) = scientific_digits(&format!("{magnitude:.767e}"));
    let exact = exact.trim_end_matches('0');
    if exact.len() != digits.len() + 1 || !exact.ends_with('5') {
        return (digits, point);
    }
    let lower = &exact[..digits.len()];
    let lower_is_even = lower.bytes().last().is_some_and(|digit| digit % 2 == 0);
    let even = if lower_is_even {
        Some(lower.to_string())
    } else {
        incremented(lower)
    };
    match even {
        Some(even)
            if !even.ends_with('0')
                && format!("0.{even}e{exact_point}").parse::<f64>() == Ok(magnitude) =>
        {
            (even, exact_point)
        }
        _ => (digits, point),
    }
}

fn scientific_digits(scientific: &str) -> (String, i32) {
    let (mantissa, exponent) = scientific.split_once('e').unwrap_or((scientific, "0"));
    let digits = mantissa.chars().filter(char::is_ascii_digit).collect();
    (digits, exponent.parse::<i32>().unwrap_or(0) + 1)
}

/// A digit string plus one in its last place, or `None` when the carry would lengthen it.
fn incremented(digits: &str) -> Option<String> {
    let mut bytes = digits.as_bytes().to_vec();
    for byte in bytes.iter_mut().rev() {
        if *byte == b'9' {
            *byte = b'0';
        } else {
            *byte += 1;
            return String::from_utf8(bytes).ok();
        }
    }
    None
}

// ---------------------------------------------------------------------------------- reading

/// Read JSON text exactly: every float correctly rounded, every integer that fits kept as an
/// integer. O(N) for N bytes.
///
/// A number too large to hold is kept, at a magnitude every range check refuses, rather than
/// refused here: an integer beyond i64 as `i64::MAX`/`i64::MIN`, a float beyond a double as
/// `±f64::MAX`. The reference refuses such a number only if it lands in the document it
/// writes, so a field the writer never sees (a result's `duration_ms`, say) cannot refuse a
/// document on its own.
pub(crate) fn parse(text: &str) -> CanonicalResult<Value> {
    let mut reader = Reader {
        bytes: text.as_bytes(),
        text,
        position: 0,
        depth: 0,
    };
    reader.skip_whitespace();
    let value = reader.value()?;
    reader.skip_whitespace();
    if reader.position != reader.bytes.len() {
        return reader.malformed("trailing characters after the document");
    }
    Ok(value)
}

struct Reader<'a> {
    bytes: &'a [u8],
    text: &'a str,
    position: usize,
    depth: usize,
}

impl Reader<'_> {
    fn malformed<T>(&self, what: &str) -> CanonicalResult<T> {
        refuse(
            CanonicalJsonErrorCode::InvalidJson,
            format!("{what} at byte {}", self.position),
        )
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.position).copied()
    }

    fn skip_whitespace(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.position += 1;
        }
    }

    fn value(&mut self) -> CanonicalResult<Value> {
        match self.peek() {
            Some(b'{') => self.nested(Self::object),
            Some(b'[') => self.nested(Self::array),
            Some(b'"') => self.string().map(Value::String),
            Some(b't') => self.literal("true", Value::Bool(true)),
            Some(b'f') => self.literal("false", Value::Bool(false)),
            Some(b'n') => self.literal("null", Value::Null),
            Some(b'-' | b'0'..=b'9') => self.number(),
            Some(_) => self.malformed("unexpected character"),
            None => self.malformed("unexpected end of text"),
        }
    }

    fn nested(&mut self, read: fn(&mut Self) -> CanonicalResult<Value>) -> CanonicalResult<Value> {
        if self.depth == MAX_DEPTH {
            return self.malformed("nesting too deep");
        }
        self.depth += 1;
        let value = read(self);
        self.depth -= 1;
        value
    }

    fn literal(&mut self, word: &str, value: Value) -> CanonicalResult<Value> {
        if !self.bytes[self.position..].starts_with(word.as_bytes()) {
            return self.malformed("unexpected character");
        }
        self.position += word.len();
        Ok(value)
    }

    fn expect(&mut self, byte: u8) -> CanonicalResult<()> {
        if self.peek() != Some(byte) {
            return self.malformed(&format!("expected '{}'", byte as char));
        }
        self.position += 1;
        Ok(())
    }

    fn object(&mut self) -> CanonicalResult<Value> {
        self.expect(b'{')?;
        let mut object = Map::new();
        self.skip_whitespace();
        if self.peek() == Some(b'}') {
            self.position += 1;
            return Ok(Value::Object(object));
        }
        loop {
            self.skip_whitespace();
            if self.peek() != Some(b'"') {
                return self.malformed("expected a string key");
            }
            let key = self.string()?;
            self.skip_whitespace();
            self.expect(b':')?;
            self.skip_whitespace();
            // The last of two equal keys wins, as it does for every engine's parser.
            let value = self.value()?;
            object.insert(key, value);
            self.skip_whitespace();
            match self.peek() {
                Some(b',') => self.position += 1,
                Some(b'}') => {
                    self.position += 1;
                    return Ok(Value::Object(object));
                }
                _ => return self.malformed("expected ',' or '}'"),
            }
        }
    }

    fn array(&mut self) -> CanonicalResult<Value> {
        self.expect(b'[')?;
        let mut elements = Vec::new();
        self.skip_whitespace();
        if self.peek() == Some(b']') {
            self.position += 1;
            return Ok(Value::Array(elements));
        }
        loop {
            self.skip_whitespace();
            elements.push(self.value()?);
            self.skip_whitespace();
            match self.peek() {
                Some(b',') => self.position += 1,
                Some(b']') => {
                    self.position += 1;
                    return Ok(Value::Array(elements));
                }
                _ => return self.malformed("expected ',' or ']'"),
            }
        }
    }

    fn string(&mut self) -> CanonicalResult<String> {
        self.expect(b'"')?;
        let mut text = String::new();
        loop {
            let start = self.position;
            while let Some(byte) = self.peek() {
                if byte == b'"' || byte == b'\\' || byte < 0x20 {
                    break;
                }
                self.position += 1;
            }
            // Only ASCII bytes stop the scan, so `start..position` is on character boundaries.
            text.push_str(&self.text[start..self.position]);
            match self.peek() {
                Some(b'"') => {
                    self.position += 1;
                    return Ok(text);
                }
                Some(b'\\') => {
                    self.position += 1;
                    self.escape(&mut text)?;
                }
                Some(_) => return self.malformed("unescaped control character in a string"),
                None => return self.malformed("unterminated string"),
            }
        }
    }

    fn escape(&mut self, text: &mut String) -> CanonicalResult<()> {
        let Some(byte) = self.peek() else {
            return self.malformed("unterminated escape");
        };
        self.position += 1;
        let character = match byte {
            b'"' => '"',
            b'\\' => '\\',
            b'/' => '/',
            b'b' => '\u{8}',
            b'f' => '\u{c}',
            b'n' => '\n',
            b'r' => '\r',
            b't' => '\t',
            b'u' => self.unicode_escape()?,
            _ => return self.malformed("invalid escape"),
        };
        text.push(character);
        Ok(())
    }

    fn unicode_escape(&mut self) -> CanonicalResult<char> {
        let first = self.hex4()?;
        if (0xDC00..=0xDFFF).contains(&first) {
            return self.lone_surrogate();
        }
        if !(0xD800..=0xDBFF).contains(&first) {
            return char::from_u32(first).map_or_else(|| self.lone_surrogate(), Ok);
        }
        if !self.bytes[self.position..].starts_with(b"\\u") {
            return self.lone_surrogate();
        }
        self.position += 2;
        let second = self.hex4()?;
        if !(0xDC00..=0xDFFF).contains(&second) {
            return self.lone_surrogate();
        }
        let scalar = 0x10000 + ((first - 0xD800) << 10) + (second - 0xDC00);
        char::from_u32(scalar).map_or_else(|| self.lone_surrogate(), Ok)
    }

    /// A lone surrogate has no UTF-8 spelling. It is the reference's `invalid_string`, not
    /// malformed text: Python reads it and refuses it only when writing.
    fn lone_surrogate<T>(&self) -> CanonicalResult<T> {
        refuse(
            CanonicalJsonErrorCode::InvalidString,
            format!(
                "a string carries a lone UTF-16 surrogate at byte {}",
                self.position
            ),
        )
    }

    fn hex4(&mut self) -> CanonicalResult<u32> {
        let Some(digits) = self.text.get(self.position..self.position + 4) else {
            return self.malformed("truncated \\u escape");
        };
        if !digits.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return self.malformed("invalid \\u escape");
        }
        self.position += 4;
        Ok(u32::from_str_radix(digits, 16).unwrap_or(0))
    }

    fn number(&mut self) -> CanonicalResult<Value> {
        let start = self.position;
        if self.peek() == Some(b'-') {
            self.position += 1;
        }
        match self.peek() {
            Some(b'0') => self.position += 1,
            Some(b'1'..=b'9') => self.skip_digits(),
            _ => return self.malformed("invalid number"),
        }
        let mut integral = true;
        if self.peek() == Some(b'.') {
            integral = false;
            self.position += 1;
            self.require_digits()?;
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            integral = false;
            self.position += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.position += 1;
            }
            self.require_digits()?;
        }
        let spelled = &self.text[start..self.position];
        Ok(Value::Number(number_from(spelled, integral)))
    }

    fn skip_digits(&mut self) {
        while matches!(self.peek(), Some(b'0'..=b'9')) {
            self.position += 1;
        }
    }

    fn require_digits(&mut self) -> CanonicalResult<()> {
        if !matches!(self.peek(), Some(b'0'..=b'9')) {
            return self.malformed("invalid number");
        }
        self.skip_digits();
        Ok(())
    }
}

fn number_from(spelled: &str, integral: bool) -> Number {
    if integral {
        // Beyond i64 an integer is held as `i64::MAX` or `i64::MIN`. Every such integer is
        // beyond 2^53 - 1, so wherever the reference would carry its exact digits -- a tick,
        // a placement position, a catalog version -- the tick check or the writer refuses it
        // with the same `number_out_of_range`, and it stays an integer for every type check.
        return Number::from(
            spelled
                .parse::<i64>()
                .unwrap_or(if spelled.starts_with('-') {
                    i64::MIN
                } else {
                    i64::MAX
                }),
        );
    }
    // The grammar above admits only what `f64::from_str` reads, so this cannot fail; an
    // overflow reads as infinity and is clamped to a finite magnitude the writer refuses.
    let parsed: f64 = spelled.parse().unwrap_or(f64::INFINITY);
    let finite = parsed.clamp(-f64::MAX, f64::MAX);
    Number::from_f64(finite).unwrap_or_else(|| Number::from(0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn canonical(text: &str) -> String {
        to_canonical_string(&parse(text).unwrap()).unwrap()
    }

    fn code_of(text: &str) -> CanonicalJsonErrorCode {
        match parse(text).and_then(|value| to_canonical_string(&value)) {
            Ok(written) => panic!("{text} was written as {written}"),
            Err(error) => error.code,
        }
    }

    #[test]
    fn numbers_are_written_as_ecmascript_writes_them() {
        let cases = [
            ("0.25", "0.25"),
            ("1.0", "1"),
            ("-0.0", "0"),
            ("-0", "0"),
            ("4.5", "4.5"),
            ("0.1", "0.1"),
            ("2e-3", "0.002"),
            ("0.000001", "0.000001"),
            ("1e-7", "1e-7"),
            ("1.5e-7", "1.5e-7"),
            ("1e-27", "1e-27"),
            ("-1.5", "-1.5"),
            ("123456789.5", "123456789.5"),
            ("333333333.33333329", "333333333.3333333"),
            ("9007199254740991.0", "9007199254740991"),
            ("5e-324", "5e-324"),
            ("100.0", "100"),
            ("1E2", "100"),
            ("-1.5e-7", "-1.5e-7"),
        ];
        for (text, spelled) in cases {
            assert_eq!(canonical(text), spelled, "{text}");
        }
    }

    #[test]
    fn an_integer_is_judged_by_value() {
        let integer = |text: &str| json_integer(&parse(text).unwrap());
        assert_eq!(integer("1"), Some(1));
        assert_eq!(integer("1.0"), Some(1));
        assert_eq!(integer("-3e0"), Some(-3));
        assert_eq!(integer("9007199254740991"), Some(9_007_199_254_740_991));
        assert_eq!(integer("-9007199254740991.0"), Some(-9_007_199_254_740_991));
        for refused in [
            "true",
            "\"1\"",
            "1.5",
            "null",
            "[1]",
            "9007199254740992",
            "9007199254740992.0",
            "1e300",
            "99999999999999999999",
        ] {
            assert_eq!(integer(refused), None, "{refused}");
        }
        assert_eq!(json_integer(&Value::from(u64::MAX)), None);
    }

    #[test]
    fn a_refusal_quotes_a_value_by_its_canonical_spelling() {
        assert_eq!(json_spelling(&json!("a\"b")), r#""a\"b""#);
        assert_eq!(json_spelling(&json!(["a", "b"])), r#"["a","b"]"#);
        assert_eq!(json_spelling(&json!(true)), "true");
        assert_eq!(json_spelling(&Value::Null), "null");
        assert_eq!(json_spelling(&json!(2.0)), "2");
        assert_eq!(
            json_spelling(&json!(9_007_199_254_740_992_u64)),
            "an out-of-range number"
        );
    }

    #[test]
    fn floats_are_read_correctly_rounded_and_not_best_effort() {
        // serde_json's default parser reads this one ULP high, which prints as ...325.
        let value = parse("333333333.33333329").unwrap();
        assert_eq!(
            value.as_f64().unwrap().to_bits(),
            333333333.3333333_f64.to_bits()
        );
    }

    #[test]
    fn a_tie_between_two_shortest_spellings_takes_the_even_digit() {
        // Both lie exactly halfway between two 17-digit spellings; Node and Python agree.
        for (text, spelled) in [
            ("-1561489596588225.25", "-1561489596588225.2"),
            ("1561489596588225.75", "1561489596588225.8"),
        ] {
            assert_eq!(canonical(text), spelled, "{text}");
        }
    }

    #[test]
    fn integers_and_literals_keep_their_spelling() {
        assert_eq!(
            canonical("[0,-7,9007199254740991,-9007199254740991,true,false,null]"),
            "[0,-7,9007199254740991,-9007199254740991,true,false,null]"
        );
    }

    #[test]
    fn keys_are_sorted_by_utf16_code_units_not_code_points() {
        // U+FFFD sorts after U+1F600 by code point and before it by UTF-16 code unit, whose
        // first unit is the high surrogate U+D83D. JavaScript sorts the second way.
        let value = json!({"\u{FFFD}": 1, "\u{1F600}": 2, "a": 0});
        assert_eq!(
            to_canonical_string(&value).unwrap(),
            "{\"a\":0,\"\u{1F600}\":2,\"\u{FFFD}\":1}"
        );
    }

    #[test]
    fn only_the_characters_rfc_8785_names_are_escaped() {
        let value = json!("\"\\\u{8}\t\n\u{c}\r\u{0}\u{1f}/\u{7f}\u{2028}\u{2029}é");
        assert_eq!(
            to_canonical_string(&value).unwrap(),
            "\"\\\"\\\\\\b\\t\\n\\f\\r\\u0000\\u001f/\u{7f}\u{2028}\u{2029}é\""
        );
    }

    #[test]
    fn there_is_no_whitespace_and_nesting_is_preserved() {
        assert_eq!(
            canonical(r#" { "b" : [ 1 , { "d" : [ ] , "c" : { } } ] , "a" : "x" } "#),
            r#"{"a":"x","b":[1,{"c":{},"d":[]}]}"#
        );
    }

    #[test]
    fn a_number_no_engine_holds_exactly_is_refused() {
        for text in [
            r#"{"n":9007199254740992}"#,
            r#"{"n":-9007199254740992}"#,
            r#"{"n":9007199254740992.0}"#,
            r#"{"n":18446744073709551616}"#,
            r#"{"n":-18446744073709551616}"#,
            r#"{"n":1e400}"#,
        ] {
            assert_eq!(
                code_of(text),
                CanonicalJsonErrorCode::NumberOutOfRange,
                "{text}"
            );
        }
    }

    #[test]
    fn a_number_too_large_to_read_is_refused_only_when_written() {
        assert!(parse(r#"{"n":1e400,"m":-1e999999}"#).is_ok());
    }

    #[test]
    fn an_integer_beyond_i64_is_still_an_integer_and_still_refused() {
        for text in ["18446744073709551616", "-99999999999999999999999"] {
            let number = parse(text).unwrap();
            assert!(number.is_i64(), "{text}");
            assert_eq!(
                to_canonical_string(&number).unwrap_err().code,
                CanonicalJsonErrorCode::NumberOutOfRange
            );
        }
    }

    #[test]
    fn a_lone_surrogate_is_refused_in_a_value_and_in_a_key() {
        for text in [r#""\ud800""#, r#"{"\udc00":1}"#, r#""\ud800\u0041""#] {
            assert_eq!(
                code_of(text),
                CanonicalJsonErrorCode::InvalidString,
                "{text}"
            );
        }
        assert_eq!(canonical(r#""\ud83d\ude00""#), "\"\u{1F600}\"");
    }

    #[test]
    fn text_that_is_not_json_is_refused_as_invalid_json() {
        for text in [
            "",
            "{",
            "[1,]",
            "{\"a\":1,}",
            "01",
            "1.",
            "-",
            "NaN",
            "Infinity",
            "\"tab\there\"",
            "{} {}",
            "\"\\x\"",
            "truth",
        ] {
            assert_eq!(
                code_of(text),
                CanonicalJsonErrorCode::InvalidJson,
                "{text:?}"
            );
        }
    }

    #[test]
    fn every_malformed_shape_is_refused_as_invalid_json() {
        for text in [
            " ",
            "@",
            "[] x",
            "{\"a\" 1}",
            "{1:2}",
            "{\"a\":1 \"b\":2}",
            "[1 2]",
            "\"open",
            "\"\\",
            "\"\\u12\"",
            "\"\\uZZZZ\"",
            "-a",
            "1e",
            "1.5e+",
            "nul",
        ] {
            assert_eq!(
                code_of(text),
                CanonicalJsonErrorCode::InvalidJson,
                "{text:?}"
            );
        }
    }

    #[test]
    fn every_escape_json_names_is_read_and_written_back_canonically() {
        assert_eq!(
            canonical(r#""\\ \/ \b \f \n \r \t \" \u00e9 \u0041""#),
            "\"\\\\ / \\b \\f \\n \\r \\t \\\" é A\""
        );
    }

    #[test]
    fn a_digit_string_is_incremented_with_its_carry_and_never_lengthened() {
        assert_eq!(incremented("7").as_deref(), Some("8"));
        assert_eq!(incremented("129").as_deref(), Some("130"));
        assert_eq!(incremented("99"), None);
    }

    #[test]
    fn the_last_of_two_equal_keys_wins() {
        assert_eq!(canonical(r#"{"a":1,"a":2}"#), r#"{"a":2}"#);
    }

    #[test]
    fn nesting_beyond_the_depth_limit_is_refused() {
        let deep = "[".repeat(MAX_DEPTH + 1) + &"]".repeat(MAX_DEPTH + 1);
        assert_eq!(code_of(&deep), CanonicalJsonErrorCode::InvalidJson);
        let allowed = "[".repeat(MAX_DEPTH) + &"]".repeat(MAX_DEPTH);
        assert_eq!(canonical(&allowed), allowed);
    }
}
