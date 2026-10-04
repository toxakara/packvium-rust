//! `Length::parse` and `Weight::parse` from outside: every scalar spelling the parser
//! accepts, the ones it refuses, and ties rounding to even at the tick boundary.

use packvium_core::{Length, PackError, Weight};
use serde_json::{Value, json};

fn ticks(value: Value) -> i64 {
    Length::parse(&value, "mm")
        .unwrap_or_else(|error| panic!("{value}: {error}"))
        .0
}

fn refused(value: Value) -> String {
    match Length::parse(&value, "mm") {
        Err(PackError::InvalidNumber(text)) => text,
        other => panic!("{value}: expected an invalid number, got {other:?}"),
    }
}

#[test]
fn a_bare_json_number_takes_the_default_unit() {
    assert_eq!(ticks(json!(12)), 12 * Length::TICKS_PER_MM);
    assert_eq!(
        Weight::parse(&json!(3), "g").expect("grams").0,
        3 * Weight::TICKS_PER_G
    );
}

#[test]
fn a_value_that_is_not_a_scalar_is_refused() {
    assert_eq!(refused(json!(true)), "true");
    assert_eq!(refused(json!({"value": [1]})), "[1]");
}

#[test]
fn a_fraction_over_zero_is_refused() {
    assert_eq!(refused(json!("1 1/0")), "1 1/0");
    assert_eq!(refused(json!("1/0")), "1/0");
}

#[test]
fn a_part_tick_rounds_to_nearest_and_ties_to_even() {
    // One tick is 1/16000 mm.
    assert_eq!(ticks(json!("1/20000")), 1, "0.8 of a tick rounds up");
    assert_eq!(ticks(json!("1/32000")), 0, "half a tick ties to even zero");
    assert_eq!(
        ticks(json!("3/32000")),
        2,
        "one and a half ties to even two"
    );
}

#[test]
fn every_malformed_rational_spelling_is_refused() {
    for text in [
        "x 1/2",
        "1 x/2",
        "1 1/x",
        "x/2",
        "1/x",
        "x.5",
        "1.x",
        "1.000000000000000000000000000000000000000001",
    ] {
        assert_eq!(refused(json!(text)), text);
    }
    assert_eq!(refused(json!({"unit": "mm"})), "missing value");
    assert!(matches!(
        Length::parse(&json!({"value": "1", "unit": "furlong"}), "mm"),
        Err(PackError::UnsupportedUnit(unit)) if unit == "furlong"
    ));
    assert!(matches!(
        Length::parse(&json!("170141183460469231731687303715884105727 m"), "mm"),
        Err(PackError::InvalidNumber(_))
    ));
}

#[test]
fn negative_whole_and_decimal_spellings_keep_their_sign_until_the_range_check() {
    assert!(Length::parse(&json!("-1 1/2"), "mm").is_err());
    assert!(Length::parse(&json!("-1.5"), "mm").is_err());
}

#[test]
fn every_unit_spelling_reads_and_writes_exactly() {
    assert_eq!(ticks(json!("3 ticks")), 3);
    for (unit, per) in [
        ("tick", 1),
        ("mg", Weight::TICKS_PER_MG),
        ("oz", Weight::TICKS_PER_OZ),
        ("lb", Weight::TICKS_PER_LB),
    ] {
        let weight = Weight::parse(&json!({"value": "2", "unit": unit}), "g").expect(unit);
        assert_eq!(weight.0, 2 * per);
        assert_eq!(weight.to_json(unit)["value"], "2");
    }
    assert_eq!(Length(5).to_json("ticks")["value"], "5");
    assert_eq!(Length(-8_000).to_json("mm")["value"], "-0.5");
}
