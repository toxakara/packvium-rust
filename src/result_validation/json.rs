//! The JSON documents the validator reads, held the way Python's `json.loads` holds them.
//!
//! The reference reads plain dicts and lists, and some of its details print one (an
//! `axle_reactions` object, a repeated value): Python keeps an object's keys in document order,
//! so this type does too, where `serde_json::Value` would sort them. A repeated key keeps its
//! first position and its last value, as a Python dict built from the pairs does.

use std::cmp::Ordering;
use std::collections::HashMap;
use std::fmt;

use serde::Deserialize;
use serde::de::{self, MapAccess, SeqAccess, Visitor};

use super::MalformedResult;

pub(crate) type Res<T> = Result<T, MalformedResult>;

#[derive(Clone, Debug)]
pub(crate) enum Json {
    Null,
    Bool(bool),
    Int(i128),
    Float(f64),
    Str(String),
    List(Vec<Json>),
    Dict(Vec<(String, Json)>),
}

pub(crate) static NULL: Json = Json::Null;
pub(crate) static EMPTY_LIST: Json = Json::List(Vec::new());
pub(crate) static EMPTY_STR: Json = Json::Str(String::new());

pub(crate) fn parse(text: &str, what: &str) -> Res<Json> {
    serde_json::from_str(text)
        .map_err(|error| MalformedResult::shape(format!("{what} is not JSON: {error}")))
}

impl Json {
    pub(crate) fn str_of(text: &str) -> Json {
        Json::Str(text.to_owned())
    }

    /// Python's `d.get(key)`: `None` when the key is absent. A non-object has no `.get`.
    pub(crate) fn get(&self, key: &str) -> Res<Option<&Json>> {
        match self {
            Json::Dict(pairs) => Ok(pairs
                .iter()
                .find(|(name, _)| name == key)
                .map(|(_, value)| value)),
            other => Err(MalformedResult::shape(format!(
                "expected an object holding {key:?}, got {}",
                other.kind()
            ))),
        }
    }

    /// Python's `d.get(key, default)`: the default only when the key is absent, so a key
    /// present as `null` still reads as `null`.
    pub(crate) fn get_or<'a>(&'a self, key: &str, default: &'a Json) -> Res<&'a Json> {
        Ok(self.get(key)?.unwrap_or(default))
    }

    /// Python's `d.get(key)`, absent reading as `None`.
    pub(crate) fn get_null(&self, key: &str) -> Res<&Json> {
        self.get_or(key, &NULL)
    }

    /// Python's `d[key]`.
    pub(crate) fn at(&self, key: &str) -> Res<&Json> {
        self.get(key)?
            .ok_or_else(|| MalformedResult::shape(format!("missing {key:?}")))
    }

    /// Python's `key in value`, for an object. Anything else reads as not holding it.
    pub(crate) fn has(&self, key: &str) -> bool {
        matches!(self, Json::Dict(pairs) if pairs.iter().any(|(name, _)| name == key))
    }

    pub(crate) fn is_null(&self) -> bool {
        matches!(self, Json::Null)
    }

    pub(crate) fn as_dict(&self) -> Option<&[(String, Json)]> {
        match self {
            Json::Dict(pairs) => Some(pairs),
            _ => None,
        }
    }

    pub(crate) fn is_dict(&self) -> bool {
        matches!(self, Json::Dict(_))
    }

    pub(crate) fn is_str(&self, text: &str) -> bool {
        matches!(self, Json::Str(value) if value == text)
    }

    /// Something the reference iterates. Only a list is admitted: iterating a string or an
    /// object would walk its characters or keys, which no contract means.
    pub(crate) fn list(&self, what: &str) -> Res<&[Json]> {
        match self {
            Json::List(values) => Ok(values),
            other => Err(MalformedResult::shape(format!(
                "{what} must be a list, got {}",
                other.kind()
            ))),
        }
    }

    pub(crate) fn text(&self, what: &str) -> Res<&str> {
        match self {
            Json::Str(text) => Ok(text),
            other => Err(MalformedResult::shape(format!(
                "{what} must be a string, got {}",
                other.kind()
            ))),
        }
    }

    /// An integer the reference does arithmetic on. A boolean is Python's `int`, so it counts;
    /// a float would carry the reference into float arithmetic this port does not follow, so
    /// it is refused rather than approximated.
    pub(crate) fn int(&self, what: &str) -> Res<i128> {
        match self {
            Json::Int(value) => Ok(*value),
            Json::Bool(flag) => Ok(i128::from(*flag)),
            other => Err(MalformedResult::shape(format!(
                "{what} must be an integer, got {}",
                other.kind()
            ))),
        }
    }

    pub(crate) fn opt_int(&self, what: &str) -> Res<Option<i128>> {
        if self.is_null() {
            Ok(None)
        } else {
            self.int(what).map(Some)
        }
    }

    /// `isinstance(value, int)`, which a boolean satisfies.
    pub(crate) fn py_int_instance(&self) -> Option<i128> {
        match self {
            Json::Int(value) => Some(*value),
            Json::Bool(flag) => Some(i128::from(*flag)),
            _ => None,
        }
    }

    /// `isinstance(value, int) and not isinstance(value, bool)`.
    pub(crate) fn strict_int(&self) -> Option<i128> {
        match self {
            Json::Int(value) => Some(*value),
            _ => None,
        }
    }

    /// Python's `int(value)`: a number truncates, a string parses as a base-10 integer.
    pub(crate) fn py_int(&self, what: &str) -> Res<i128> {
        match self {
            Json::Int(value) => Ok(*value),
            Json::Bool(flag) => Ok(i128::from(*flag)),
            Json::Float(value) if value.is_finite() && value.abs() < 1.0e38 => {
                Ok(value.trunc() as i128)
            }
            Json::Str(text) => parse_int_literal(text).ok_or_else(|| {
                MalformedResult::shape(format!("{what}: invalid literal for int(): {text:?}"))
            }),
            other => Err(MalformedResult::shape(format!(
                "{what}: int() cannot convert {}",
                other.kind()
            ))),
        }
    }

    /// Python truthiness.
    pub(crate) fn truthy(&self) -> bool {
        match self {
            Json::Null => false,
            Json::Bool(flag) => *flag,
            Json::Int(value) => *value != 0,
            Json::Float(value) => *value != 0.0,
            Json::Str(text) => !text.is_empty(),
            Json::List(values) => !values.is_empty(),
            Json::Dict(pairs) => !pairs.is_empty(),
        }
    }

    /// Python `==`: a boolean equals its integer, an integer equals the float of the same
    /// value, containers compare element-wise and objects regardless of key order.
    pub(crate) fn py_eq(&self, other: &Json) -> bool {
        match (self.number(), other.number()) {
            (Some(left), Some(right)) => return left.equals(right),
            (Some(_), None) | (None, Some(_)) => return false,
            (None, None) => {}
        }
        match (self, other) {
            (Json::Null, Json::Null) => true,
            (Json::Str(left), Json::Str(right)) => left == right,
            (Json::List(left), Json::List(right)) => {
                left.len() == right.len() && left.iter().zip(right).all(|(a, b)| a.py_eq(b))
            }
            (Json::Dict(left), Json::Dict(right)) => {
                left.len() == right.len()
                    && left.iter().all(|(key, value)| {
                        right
                            .iter()
                            .any(|(name, candidate)| name == key && value.py_eq(candidate))
                    })
            }
            _ => false,
        }
    }

    pub(crate) fn eq_int(&self, value: i128) -> bool {
        self.number()
            .is_some_and(|number| number.equals(Number::Int(value)))
    }

    /// Python's `<`/`>` ordering for values the reference sorts or compares: numbers with
    /// numbers, strings with strings. Anything else is Python's `TypeError`.
    pub(crate) fn py_cmp(&self, other: &Json) -> Res<Ordering> {
        if let (Some(left), Some(right)) = (self.number(), other.number()) {
            return left.compare(right);
        }
        match (self, other) {
            (Json::Str(left), Json::Str(right)) => Ok(left.cmp(right)),
            (Json::List(left), Json::List(right)) => {
                for (a, b) in left.iter().zip(right) {
                    if !a.py_eq(b) {
                        return a.py_cmp(b);
                    }
                }
                Ok(left.len().cmp(&right.len()))
            }
            _ => Err(MalformedResult::shape(format!(
                "cannot order {} against {}",
                self.kind(),
                other.kind()
            ))),
        }
    }

    /// The identity Python hashes a set member or dict key by, so `1`, `1.0` and `True` share
    /// one. A list or an object is unhashable, as in Python.
    pub(crate) fn hash_key(&self) -> Res<String> {
        if let Some(number) = self.number() {
            return Ok(match number {
                Number::Int(value) => format!("n{value}"),
                Number::Float(value) => format!("f{value:e}"),
            });
        }
        match self {
            Json::Null => Ok("null".to_owned()),
            Json::Str(text) => Ok(format!("s{text}")),
            other => Err(MalformedResult::shape(format!(
                "{} is unhashable",
                other.kind()
            ))),
        }
    }

    fn number(&self) -> Option<Number> {
        match self {
            Json::Bool(flag) => Some(Number::Int(i128::from(*flag))),
            Json::Int(value) => Some(Number::Int(*value)),
            Json::Float(value) => Some(Number::from_float(*value)),
            _ => None,
        }
    }

    pub(crate) fn kind(&self) -> &'static str {
        match self {
            Json::Null => "null",
            Json::Bool(_) => "a boolean",
            Json::Int(_) => "an integer",
            Json::Float(_) => "a number",
            Json::Str(_) => "a string",
            Json::List(_) => "a list",
            Json::Dict(_) => "an object",
        }
    }

    /// Replace or append `key`, keeping its position when it already exists.
    pub(crate) fn set(&mut self, key: &str, value: Json) -> Res<()> {
        match self {
            Json::Dict(pairs) => {
                match pairs.iter_mut().find(|(name, _)| name == key) {
                    Some(existing) => existing.1 = value,
                    None => pairs.push((key.to_owned(), value)),
                }
                Ok(())
            }
            other => Err(MalformedResult::shape(format!(
                "cannot set {key:?} on {}",
                other.kind()
            ))),
        }
    }
}

/// A number compared the way Python compares `int` with `float`: exactly.
#[derive(Clone, Copy, Debug)]
enum Number {
    Int(i128),
    Float(f64),
}

impl Number {
    fn from_float(value: f64) -> Number {
        // An integral float within range is that integer, as Python's comparison treats it.
        if value.fract() == 0.0 && value.abs() < 1.0e38 {
            Number::Int(value as i128)
        } else {
            Number::Float(value)
        }
    }

    fn equals(self, other: Number) -> bool {
        matches!(self.compare(other), Ok(Ordering::Equal))
    }

    fn compare(self, other: Number) -> Res<Ordering> {
        match (self, other) {
            (Number::Int(left), Number::Int(right)) => Ok(left.cmp(&right)),
            (left, right) => left
                .as_float()
                .partial_cmp(&right.as_float())
                .ok_or_else(|| MalformedResult::shape("cannot order a NaN".to_owned())),
        }
    }

    fn as_float(self) -> f64 {
        match self {
            Number::Int(value) => value as f64,
            Number::Float(value) => value,
        }
    }
}

/// `sorted(values)` in place, by Python's ordering; values Python cannot order are its
/// `TypeError`.
pub(crate) fn sort_py(values: &mut [&Json]) -> Res<()> {
    let mut failure = None;
    values.sort_by(|left, right| {
        left.py_cmp(right).unwrap_or_else(|error| {
            failure = Some(error);
            Ordering::Equal
        })
    });
    failure.map_or(Ok(()), Err)
}

/// `int(text)` for a string: optional surrounding whitespace, an optional sign, ASCII digits
/// with single underscores between them.
fn parse_int_literal(text: &str) -> Option<i128> {
    let trimmed = text.trim();
    let (negative, digits) = match trimmed.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, trimmed.strip_prefix('+').unwrap_or(trimmed)),
    };
    if digits.is_empty()
        || digits.starts_with('_')
        || digits.ends_with('_')
        || digits.contains("__")
    {
        return None;
    }
    let mut value: i128 = 0;
    for character in digits.chars().filter(|character| *character != '_') {
        let digit = character.to_digit(10)?;
        value = value.checked_mul(10)?.checked_add(i128::from(digit))?;
    }
    Some(if negative { -value } else { value })
}

/// Insertion-ordered map keyed the way a Python dict is: assigning an existing key replaces
/// its value in place. Keys are kept as the JSON value they came from, for printing.
#[derive(Clone, Debug)]
pub(crate) struct Ordered<T> {
    entries: Vec<(Json, T)>,
    index: HashMap<String, usize>,
}

impl<T> Default for Ordered<T> {
    fn default() -> Self {
        Ordered {
            entries: Vec::new(),
            index: HashMap::new(),
        }
    }
}

impl<T> Ordered<T> {
    pub(crate) fn insert(&mut self, key: &Json, value: T) -> Res<()> {
        let hashed = key.hash_key()?;
        match self.index.get(&hashed) {
            Some(&position) => self.entries[position].1 = value,
            None => {
                self.index.insert(hashed, self.entries.len());
                self.entries.push((key.clone(), value));
            }
        }
        Ok(())
    }

    pub(crate) fn get(&self, key: &Json) -> Res<Option<&T>> {
        Ok(self
            .index
            .get(&key.hash_key()?)
            .map(|&position| &self.entries[position].1))
    }

    /// The entry's position in insertion order.
    pub(crate) fn position(&self, key: &Json) -> Res<Option<usize>> {
        Ok(self.index.get(&key.hash_key()?).copied())
    }

    pub(crate) fn get_mut(&mut self, key: &Json) -> Res<Option<&mut T>> {
        let hashed = key.hash_key()?;
        Ok(self
            .index
            .get(&hashed)
            .map(|&position| &mut self.entries[position].1))
    }

    pub(crate) fn contains(&self, key: &Json) -> Res<bool> {
        Ok(self.index.contains_key(&key.hash_key()?))
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = (&Json, &T)> {
        self.entries.iter().map(|(key, value)| (key, value))
    }

    pub(crate) fn values(&self) -> impl Iterator<Item = &T> {
        self.entries.iter().map(|(_, value)| value)
    }

    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

impl<'de> Deserialize<'de> for Json {
    fn deserialize<D: de::Deserializer<'de>>(deserializer: D) -> Result<Json, D::Error> {
        deserializer.deserialize_any(JsonVisitor)
    }
}

struct JsonVisitor;

impl<'de> Visitor<'de> for JsonVisitor {
    type Value = Json;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a JSON value")
    }

    fn visit_unit<E>(self) -> Result<Json, E> {
        Ok(Json::Null)
    }

    fn visit_bool<E>(self, value: bool) -> Result<Json, E> {
        Ok(Json::Bool(value))
    }

    fn visit_i64<E>(self, value: i64) -> Result<Json, E> {
        Ok(Json::Int(i128::from(value)))
    }

    fn visit_u64<E>(self, value: u64) -> Result<Json, E> {
        Ok(Json::Int(i128::from(value)))
    }

    fn visit_f64<E>(self, value: f64) -> Result<Json, E> {
        Ok(Json::Float(value))
    }

    fn visit_str<E>(self, value: &str) -> Result<Json, E> {
        Ok(Json::Str(value.to_owned()))
    }

    fn visit_string<E>(self, value: String) -> Result<Json, E> {
        Ok(Json::Str(value))
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut sequence: A) -> Result<Json, A::Error> {
        let mut values = Vec::new();
        while let Some(value) = sequence.next_element()? {
            values.push(value);
        }
        Ok(Json::List(values))
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Json, A::Error> {
        let mut pairs: Vec<(String, Json)> = Vec::new();
        while let Some((key, value)) = map.next_entry::<String, Json>()? {
            match pairs.iter_mut().find(|(name, _)| *name == key) {
                Some(existing) => existing.1 = value,
                None => pairs.push((key, value)),
            }
        }
        Ok(Json::Dict(pairs))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(text: &str) -> Json {
        parse(text, "test").unwrap()
    }

    #[test]
    fn values_compare_and_hash_as_python_compares_and_hashes_them() {
        assert_eq!(
            read("[1, 2]").py_cmp(&read("[1]")).unwrap(),
            Ordering::Greater
        );
        assert_eq!(read("[1]").py_cmp(&read("[1, 2]")).unwrap(), Ordering::Less);
        assert_eq!(read("1.5").py_cmp(&read("2")).unwrap(), Ordering::Less);
        assert!(read("[1, [2.0], null]").py_eq(&read("[true, [2], null]")));
        assert!(!read("[1, 2]").py_eq(&read("[1, 3]")));
        assert!(!read("{\"a\": 1}").py_eq(&read("{\"b\": 1}")));
        assert!(read("0.5").truthy());
        assert_eq!(read("null").hash_key().unwrap(), "null");
        assert_eq!(
            read("2.0").hash_key().unwrap(),
            read("2").hash_key().unwrap()
        );
        assert_ne!(
            read("2.5").hash_key().unwrap(),
            read("2").hash_key().unwrap()
        );
        let kinds: Vec<&str> = ["null", "false", "1", "1.5", "\"x\"", "[1]", "{}"]
            .iter()
            .map(|text| read(text).kind())
            .collect();
        assert_eq!(
            kinds,
            [
                "null",
                "a boolean",
                "an integer",
                "a number",
                "a string",
                "a list",
                "an object"
            ]
        );
    }

    #[test]
    fn numbers_read_as_python_int_reads_them() {
        assert_eq!(read("true").int("x").unwrap(), 1);
        assert_eq!(read("true").py_int_instance(), Some(1));
        assert_eq!(read("false").py_int("x").unwrap(), 0);
        assert_eq!(read("-2.7").py_int("x").unwrap(), -2);
        assert_eq!(read("\" +1_000 \"").py_int("x").unwrap(), 1000);
        assert_eq!(read("\"-12\"").py_int("x").unwrap(), -12);
    }

    #[test]
    fn what_the_reference_cannot_read_is_a_shape_refusal() {
        let refusals = [
            read("5").get("x").map(|_| ()),
            read("\"x\"").list("x").map(|_| ()),
            read("1").text("x").map(|_| ()),
            read("1.5").int("x").map(|_| ()),
            read("[]").py_int("x").map(|_| ()),
            read("\"1__0\"").py_int("x").map(|_| ()),
            read("[1]").py_cmp(&read("\"a\"")).map(|_| ()),
            read("[1]").hash_key().map(|_| ()),
            read("5").set("x", Json::Null),
        ];
        for (index, refusal) in refusals.into_iter().enumerate() {
            assert_eq!(
                refusal.unwrap_err().kind,
                super::super::RefusalKind::Shape,
                "refusal {index}"
            );
        }
        let mut values = [read("1"), read("\"a\"")];
        let mut references: Vec<&Json> = values.iter_mut().map(|value| &*value).collect();
        assert!(sort_py(&mut references).is_err());
    }

    #[test]
    fn a_repeated_key_keeps_its_first_place_and_its_last_value() {
        let mut object = read("{\"a\": 1, \"b\": 2, \"a\": 3}");
        object.set("c", Json::Str("d".to_owned())).unwrap();
        object.set("b", Json::Null).unwrap();
        let keys: Vec<&str> = object
            .as_dict()
            .unwrap()
            .iter()
            .map(|(key, _)| key.as_str())
            .collect();
        assert_eq!(keys, ["a", "b", "c"]);
        assert!(object.at("a").unwrap().eq_int(3));
        assert!(object.at("b").unwrap().is_null());
    }
}
