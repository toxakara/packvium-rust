//! Values the reference computes with Python's unbounded integers but that leave `i128`
//! somewhere along the check: this port refuses the result rather than wrap or saturate, at
//! whichever product, sum or bound first exceeds it.

use super::*;

/// `i128::MAX` as a measure: the largest tick count a request can state exactly.
const MAX_TICKS: &str = "170141183460469231731687303715884105727";
/// 10^19 ticks: a side whose square still fits `i128` but whose doubled square does not.
const HUGE_SIDE: u64 = 10_000_000_000_000_000_000;

#[test]
fn an_obstacle_reaching_past_i128_is_refused() {
    let mut request = request();
    let result = packed(&request);
    request["containers"][0]["obstacles"] = json!([{
        "origin": {"x": MAX_TICKS},
        "dimensions": {"length": "1", "width": "1", "height": "1"}
    }]);
    assert_overflow(&request, &result);
}

#[test]
fn a_requested_volume_past_i128_is_refused_by_the_volume_objective() {
    let mut request = request();
    request["configuration"] = json!({"objective": "maximum_volume"});
    let result = packed(&request);
    let slab = |id: &str| {
        json!({"id": id, "quantity": 1,
               "dimensions": {"length": "10000000000000", "width": "10000000000000",
                              "height": "1000000000000"}})
    };
    request["items"]
        .as_array_mut()
        .unwrap()
        .extend([slab("b"), slab("c")]);
    assert_overflow(&request, &result);
}

fn machine_with(minimum: [&str; 3], increment: [&str; 3]) -> Value {
    json!({
        "minimum": {"length": minimum[0], "width": minimum[1], "height": minimum[2]},
        "increment": {"length": increment[0], "width": increment[1], "height": increment[2]}
    })
}

/// The machine grid `minimum + k * increment` past `i128`, and each sum of the board-area
/// rounding `2(LW + LH + WH)`: `LW + LH` fits at 9e18 ticks a side but adding `WH` does not,
/// and at 6e18 the sum fits but doubling it does not.
#[test]
fn a_made_to_measure_box_past_i128_is_refused() {
    let machines = [
        machine_with(
            ["-100000000000000000000000000000000000000", "1", "1"],
            ["90000000000000000000000000000000000000", "1", "1"],
        ),
        machine_with(["9000000000000000000"; 3], ["1"; 3]),
        machine_with(["6000000000000000000"; 3], ["1"; 3]),
    ];
    for machine in machines {
        let mut request = request();
        let result = packed(&request);
        request["containers"][0]["made_to_measure"] = machine;
        assert_overflow(&request, &result);
    }
}

#[test]
fn a_made_to_measure_tare_past_i128_is_refused() {
    let mut request = request();
    let result = packed(&request);
    request["containers"][0]["tare_weight"] = json!(format!("{MAX_TICKS} tick"));
    let mut machine = machine_with(["1"; 3], ["1"; 3]);
    machine["tare_per_square_metre"] = json!("1");
    request["containers"][0]["made_to_measure"] = machine;
    assert_overflow(&request, &result);
}

/// Two 10^19 x 10^19 x 1 tick slabs, apart: each volume fits `i128`, their sum does not.
#[test]
fn a_used_volume_past_i128_is_refused() {
    let request = request();
    let result = with_placements(vec![
        placement("slab#1", "slab", [0, 0, 0], [HUGE_SIDE, HUGE_SIDE, 1]),
        placement("slab#2", "slab", [0, 0, 10], [HUGE_SIDE, HUGE_SIDE, 1]),
    ]);
    assert_overflow(&request, &result);
}

/// A reserve ratio of 10^33 is a ppm figure past `i128`, which the solvers' double rounding
/// saturates; the reserve built from it then overflows, opened or only declared.
#[test]
fn a_void_fill_reserve_past_i128_is_refused_in_the_container_and_in_the_bound() {
    let spare = json!({"id": "spare", "void_fill_reserve_ratio": "1e33",
        "inner_dimensions": {"length": "101", "width": "9901", "height": "1"}});
    let mut opened = request();
    let result = packed(&opened);
    opened["containers"][0] = spare.clone();
    opened["containers"][0]["id"] = json!("box");
    assert_overflow(&opened, &result);
    let mut declared = request();
    declared["containers"].as_array_mut().unwrap().push(spare);
    assert_overflow(&declared, &result);
}

/// A 6e37-tick load centred in a 2-tick box with a 6e37-tick tare: the centre of mass and the
/// gross weight fit, the tare's moment plus the load's does not.
#[test]
fn an_axle_moment_past_i128_is_refused() {
    let heavy = "60000000000000000000000000000000000000 tick";
    let request = json!({
        "units": {"length": "tick"},
        "items": [{"id": "a", "quantity": 1, "weight": heavy,
                   "dimensions": {"length": "2", "width": "1", "height": "1"}}],
        "containers": [{"id": "box", "tare_weight": heavy,
                        "axles": [{"position": "0"}, {"position": "1"}],
                        "inner_dimensions": {"length": "2", "width": "1", "height": "1"}}]
    });
    let result = with_placements(vec![placement("a#1", "a", [0, 0, 0], [2, 1, 1])]);
    assert_overflow(&request, &result);
}

/// A 10^19-tick slab resting on exactly half of another: the support-hull orientation test
/// multiplies two doubled 10^19 spans.
#[test]
fn a_support_hull_past_i128_is_refused() {
    let side = HUGE_SIDE.to_string();
    let mut request = request();
    request["items"] = json!([{"id": "slab", "quantity": 2, "minimum_support_ratio": "0.5",
        "dimensions": {"length": side, "width": side, "height": "1"}}]);
    let result = with_placements(vec![
        placement("slab#1", "slab", [0, 0, 0], [HUGE_SIDE, HUGE_SIDE, 1]),
        placement(
            "slab#2",
            "slab",
            [HUGE_SIDE / 2, 0, 1],
            [HUGE_SIDE, HUGE_SIDE, 1],
        ),
    ]);
    assert_overflow(&request, &result);
}

fn hull_request(vertices: [[&str; 3]; 4]) -> Value {
    let mut request = request();
    request["items"][0]["shape_type"] = json!("convex_hull");
    request["items"][0]["hull_vertices"] = vertices
        .iter()
        .map(|[x, y, z]| json!({"x": x, "y": y, "z": z}))
        .collect();
    request
}

/// A tetrahedron with legs of 10^30 ticks: its face normals are products past `i128`.
fn huge_hull() -> Value {
    let leg = "1000000000000000000000000000000";
    hull_request([
        ["0", "0", "0"],
        [leg, "0", "0"],
        ["0", leg, "0"],
        ["0", "0", leg],
    ])
}

#[test]
fn a_hull_past_i128_is_refused_by_the_collision_checks() {
    let mut obstacle = huge_hull();
    let result = packed(&request());
    obstacle["containers"][0]["obstacles"] =
        json!([{"dimensions": {"length": "5", "width": "5", "height": "5"}}]);
    assert_overflow(&obstacle, &result);

    let overlapping = with_placements(vec![
        placement("a#1", "a", [0, 0, 0], [10, 10, 10]),
        placement("a#2", "a", [5, 5, 5], [10, 10, 10]),
    ]);
    assert_overflow(&huge_hull(), &overlapping);
}

#[test]
fn a_hull_past_i128_is_refused_by_the_volume_check() {
    assert_overflow(&huge_hull(), &packed(&request()));
}

/// Legs near 10^11 ticks: every face normal (~10^22) and every vertex's reach along one
/// (~10^33) fit, but winding the slanted face multiplies a normal by a normal (~10^44).
#[test]
fn a_hull_whose_face_winding_passes_i128_is_refused() {
    let request = hull_request([
        ["0", "0", "0"],
        ["100000000003", "0", "0"],
        ["0", "100000000019", "0"],
        ["0", "0", "100000000057"],
    ]);
    assert_overflow(&request, &packed(&self::request()));
}

/// A face whose normal reduces to an axis but whose winding, taken from its lowest corner,
/// multiplies a 10^19 span by a 2 * 10^19 one.
#[test]
fn a_hull_whose_face_corners_wind_past_i128_is_refused() {
    let side = HUGE_SIDE.to_string();
    let minus = format!("-{side}");
    let request = hull_request([
        ["0", "0", "0"],
        ["0", &side, "0"],
        [&minus, &minus, "0"],
        ["0", "0", "1"],
    ]);
    assert_overflow(&request, &packed(&self::request()));
}

/// A negative clearance shrinks each envelope below its item, so a nested column's envelopes
/// fit `i128` while the items' own shared footprint, 1.31e19 ticks square, does not. Small
/// convex hulls keep every other product small.
#[test]
fn a_nested_footprint_past_i128_is_refused() {
    const ITEM: u64 = 13_100_000_000_000_000_000;
    const SHRINK: u64 = 1_000_000_000_000_000_000;
    let envelope = (ITEM - 2 * SHRINK).to_string();
    let request = json!({
        "units": {"length": "tick"},
        "configuration": {"clearance": format!("-{SHRINK}")},
        "items": [{"id": "cup", "quantity": 2, "nesting_height": "10",
                   "shape_type": "convex_hull",
                   "hull_vertices": [{"x": "0", "y": "0", "z": "0"}, {"x": "1", "y": "0", "z": "0"},
                                     {"x": "0", "y": "1", "z": "0"}, {"x": "0", "y": "0", "z": "1"}],
                   "dimensions": {"length": envelope, "width": envelope,
                                  "height": SHRINK.to_string()}}],
        "containers": [{"id": "box",
                        "inner_dimensions": {"length": "1", "width": "1", "height": "1"}}]
    });
    let result = with_placements(vec![
        placement("cup#1", "cup", [0, 0, 0], [ITEM, ITEM, 3 * SHRINK]),
        placement(
            "cup#2",
            "cup",
            [0, 0, SHRINK - 10],
            [ITEM, ITEM, 3 * SHRINK],
        ),
    ]);
    assert_overflow(&request, &result);
}
