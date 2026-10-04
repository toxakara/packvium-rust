//! Units: why there are no floats anywhere, and what you can type.
//!
//!     cargo run --example units
//!
//! Packing is arithmetic about physical space, and floating point is the wrong tool for it:
//! `0.1 + 0.2` is not `0.3`, and a box that "almost" fits either fits or does not. So every
//! length and weight is an exact integer count of ticks -- 1/16000 mm for length, 1/8
//! microgram for weight -- and nothing in the request, the search or the result is a float.

use packvium_core::{Dimensions, Length, PackResult, Weight};
use serde_json::{Value, json};

fn length(text: &str) -> PackResult<Length> {
    // The second argument is the unit a bare number is read in; a unit in the text wins.
    Length::parse(&json!(text), "mm")
}

fn weight(text: &str) -> PackResult<Weight> {
    Weight::parse(&json!(text), "g")
}

/// A measure rendered for a person: `to_json` returns the exact ticks beside a decimal
/// string in the unit you ask for.
fn rendered(measure: &Value) -> String {
    format!(
        "{} {}",
        measure["value"].as_str().unwrap_or_default(),
        measure["unit"].as_str().unwrap_or_default()
    )
}

fn cube(edge: Length) -> Dimensions {
    Dimensions {
        length: edge,
        width: edge,
        height: edge,
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // ------------------------------------------------------------ what you can type
    //
    // Integers, decimals, fractions and mixed fractions, in mm, cm, m, in and ft -- because
    // a spec sheet says "12 3/8 in", and retyping that as 12.375 is a step where mistakes
    // live.
    for text in ["30", "30.5 mm", "3/16 in", "12 3/8 in", "2 ft", "1.5 m"] {
        let parsed = length(text)?;
        println!(
            "{text:>12}  ->  {:>12} ticks  =  {}",
            parsed.0,
            rendered(&parsed.to_json("mm"))
        );
    }
    println!();
    for text in ["450 g", "12 oz", "1.5 kg", "2 3/4 lb"] {
        let parsed = weight(text)?;
        println!(
            "{text:>12}  ->  {:>14} ticks  =  {}",
            parsed.0,
            rendered(&parsed.to_json("g"))
        );
    }

    // ------------------------------------------------------------ exact inches
    //
    // 1/16000 mm was chosen so that every binary fraction of an inch down to 1/128 lands
    // on a whole number of ticks: an imperial spec survives the trip to millimetres and back.
    println!();
    println!(
        "1 inch is {} ticks, so 1/128 in is {} ticks exactly",
        Length::TICKS_PER_INCH,
        Length::TICKS_PER_INCH / 128
    );
    println!(
        "128 x (1/128 in) == 1 in ? {}",
        Length(length("1/128 in")?.0 * 128) == length("1 in")?
    );
    // The honest other half: a third of an inch is not a binary fraction, so it cannot land
    // on a whole tick. It is rounded once, deterministically, rather than carried as an
    // error that only shows up as a box that "almost" fits.
    println!(
        "  3 x (1/3 in) == 1 in ? {} -- 1/3 in is {}/3 ticks, which is not an integer",
        Length(length("1/3 in")?.0 * 3) == length("1 in")?,
        Length::TICKS_PER_INCH
    );

    // ------------------------------------------------------------ where floats bite
    println!();
    println!("0.1 + 0.2 == 0.3 in floats: {}", 0.1_f64 + 0.2 == 0.3);
    println!(
        "the same three tenths as lengths: {}",
        length("0.1 mm")?.0 + length("0.2 mm")?.0 == length("0.3 mm")?.0
    );

    // The same exactness decides whether something fits: a 100 mm cube into a 100 mm
    // opening fits, and one tick over is refused, with no tolerance to tune.
    println!();
    let opening = cube(length("100 mm")?);
    let one_tick_over = Dimensions {
        length: Length(opening.length.0 + 1),
        ..opening
    };
    println!(
        "exactly 100 mm fits:      {}",
        cube(length("100")?).fits_inside(opening)
    );
    println!(
        "one tick over does not:   {}",
        one_tick_over.fits_inside(opening)
    );

    // ------------------------------------------------------------ in a request
    //
    // A request states its default length unit once; any measure may still carry its own.
    // The result is rendered in the unit you ask for under `output`, and its ticks are the
    // same whichever unit you choose.
    let request = json!({
        "units": { "length": "in" },
        "output": { "length_unit": "mm", "weight_unit": "lb" },
        "configuration": {
            "time_limit_ms": 60000,
            "effort_budget": {
                "max_candidates_evaluated": 1000000,
                "max_placement_attempts": 1000000,
                "max_search_nodes": 1000000
            }
        },
        "items": [{ "id": "shelf", "quantity": 3, "weight": "2 3/4 lb",
                    "dimensions": { "length": "12 3/8", "width": "9 1/2", "height": "3/4" } }],
        "containers": [{ "id": "carton", "max_payload": "20 lb",
                         "inner_dimensions": { "length": "13", "width": "10", "height": "4" } }]
    });
    let result: Value = serde_json::from_str(&packvium_core::pack_json(&request.to_string())?)?;
    let carton = &result["containers"][0];
    let placements = carton["placements"].as_array().map_or(0, Vec::len);
    println!();
    println!(
        "packed {placements} shelves, {} left over; payload {}",
        result["unpacked_items"].as_array().map_or(0, Vec::len),
        rendered(&carton["payload_weight"])
    );
    let top = carton["placements"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|placement| &placement["position"]["z"])
        .max_by_key(|z| z["ticks"].as_i64())
        .ok_or("the carton holds no shelves")?;
    println!(
        "top shelf sits at {} = {} ticks, which is exactly 2 x 3/4 in",
        rendered(top),
        top["ticks"]
    );
    Ok(())
}
