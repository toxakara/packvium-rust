//! Loading and removal sequencing over bare boxes and over placements, from outside the
//! module: the dependency graphs, the replay refusals a finished solve never produces, and
//! the placement-level entry point that also replays business rules.

mod support;

use packvium_core::{
    ALL_DIRECTIONS, Aabb, Dimensions, Length, LoadingDependencyGraph, Point, SequenceError,
    UnloadingDependencyGraph, placement_reachability, replay_removal_order,
    safe_loading_order_for_placements, safe_removal_order_with_evidence,
    verify_loading_prefix_business_rules,
};
use support::{SIDE, container, item, placed};

fn dimensions(length: i64, width: i64, height: i64) -> Dimensions {
    Dimensions {
        length: Length(length),
        width: Length(width),
        height: Length(height),
    }
}

fn box_at(x: i64, z: i64, height: i64) -> Aabb {
    Aabb {
        origin: Point { x, y: 0, z },
        dimensions: dimensions(10, 10, height),
    }
}

fn replay_reason(result: Result<(), SequenceError>) -> &'static str {
    match result {
        Err(SequenceError::Replay { reason, .. }) => reason,
        other => panic!("expected a replay refusal, got {other:?}"),
    }
}

#[test]
fn a_stack_is_acyclic_in_both_directions() {
    let boxes = [box_at(0, 0, 10), box_at(0, 10, 10)];
    assert!(LoadingDependencyGraph::build(&boxes).is_acyclic());
    assert!(UnloadingDependencyGraph::build(&boxes).is_acyclic());
}

#[test]
fn two_flat_boxes_on_one_plane_support_each_other_in_a_cycle() {
    // A zero-height box's top face is its bottom face, so two of them overlapping on the
    // same plane each rest on the other -- the one geometry that closes a cycle.
    let boxes = [box_at(0, 5, 0), box_at(5, 5, 0)];
    assert!(!LoadingDependencyGraph::build(&boxes).is_acyclic());
    assert!(!UnloadingDependencyGraph::build(&boxes).is_acyclic());
}

#[test]
fn removal_replay_refuses_what_a_safe_order_never_contains() {
    let container = dimensions(20, 10, 20);
    let stack = [box_at(0, 0, 10), box_at(0, 10, 10)];
    assert_eq!(
        replay_reason(replay_removal_order(
            &stack,
            container,
            &[0],
            &ALL_DIRECTIONS
        )),
        "order is not a permutation of every placement index exactly once"
    );
    assert_eq!(
        replay_reason(replay_removal_order(
            &stack,
            container,
            &[0, 1],
            &ALL_DIRECTIONS
        )),
        "something still resting on it has not been removed yet"
    );
    let row = [box_at(0, 0, 10), box_at(10, 0, 10)];
    assert_eq!(
        replay_reason(replay_removal_order(
            &row,
            dimensions(20, 10, 10),
            &[1, 0],
            &["-x"]
        )),
        "no allowed direction is clear of the remaining placements"
    );
    let steps = safe_removal_order_with_evidence(&stack, container, &ALL_DIRECTIONS)
        .expect("a stack unloads top first");
    assert_eq!(
        steps.iter().map(|step| step.index).collect::<Vec<_>>(),
        [1, 0]
    );
}

#[test]
fn loading_replay_refuses_a_box_walled_in_by_what_is_already_loaded() {
    // With only the -x door, the far box must go in first; loading the near one first
    // leaves the far one no clear path.
    let row = [box_at(0, 0, 10), box_at(10, 0, 10)];
    let error = packvium_core::replay_loading_order(&row, dimensions(20, 10, 10), &[0, 1], &["-x"]);
    assert_eq!(
        replay_reason(error),
        "no allowed direction is clear of what has already been loaded"
    );
}

#[test]
fn reachability_needs_one_stop_per_placement() {
    let boxes = [box_at(0, 0, 10)];
    let error = placement_reachability(
        &boxes,
        dimensions(10, 10, 10),
        Some(&[Some(1), Some(2)]),
        &ALL_DIRECTIONS,
    )
    .expect_err("two stops for one placement");
    assert!(matches!(
        error,
        SequenceError::Replay {
            reason: "stops must contain exactly one entry per placement",
            ..
        }
    ));
}

#[test]
fn placements_load_in_a_safe_order_that_also_keeps_business_rules() {
    let base = item("base");
    let upper = item("upper");
    let placements = [placed(&upper, 1, 0, SIDE), placed(&base, 1, 0, 0)];
    let order = safe_loading_order_for_placements(&placements, &container(), &ALL_DIRECTIONS)
        .expect("a stack of two loads bottom first");
    assert_eq!(order, [1, 0]);
    assert!(matches!(
        verify_loading_prefix_business_rules(&placements, &[0], &container()),
        Err(SequenceError::Replay { index: -1, .. })
    ));
}

#[test]
fn every_entry_point_refuses_an_unknown_door() {
    let boxes = [box_at(0, 0, 10)];
    let container = dimensions(10, 10, 10);
    for error in [
        placement_reachability(&boxes, container, None, &["sideways"]).map(|_| ()),
        replay_removal_order(&boxes, container, &[0], &["sideways"]),
        packvium_core::replay_loading_order(&boxes, container, &[0], &["sideways"]),
        safe_removal_order_with_evidence(&boxes, container, &["sideways"]).map(|_| ()),
        packvium_core::safe_loading_order_with_evidence(&boxes, container, &["sideways"])
            .map(|_| ()),
    ] {
        assert!(matches!(error, Err(SequenceError::InvalidDirection(_))));
    }
}

#[test]
fn replays_refuse_a_box_outside_the_container_or_a_partial_order() {
    let outside = [box_at(15, 0, 10)];
    let container = dimensions(20, 10, 10);
    assert_eq!(
        replay_reason(replay_removal_order(
            &outside,
            container,
            &[0],
            &ALL_DIRECTIONS
        )),
        "placement is outside the container"
    );
    let stack = [box_at(0, 0, 10), box_at(0, 10, 10)];
    assert_eq!(
        replay_reason(packvium_core::replay_loading_order(
            &stack,
            dimensions(20, 10, 20),
            &[1],
            &ALL_DIRECTIONS
        )),
        "order is not a permutation of every placement index exactly once"
    );
}

#[test]
fn a_placement_order_refuses_geometry_and_business_rule_violations() {
    let base = item("base");
    let floating = [placed(&base, 1, 10 * SIDE, 0)];
    assert!(safe_loading_order_for_placements(&floating, &container(), &ALL_DIRECTIONS).is_err());

    let mut soft = item("soft");
    soft.shape_type = packvium_core::ShapeType::Compressible;
    soft.compression_ratio_ppm = Some(0);
    soft.max_compression_pressure_kpa = Some(0);
    let heavy = item("heavy");
    let crushed = [placed(&soft, 1, 0, 0), placed(&heavy, 1, 0, SIDE)];
    assert!(matches!(
        safe_loading_order_for_placements(&crushed, &container(), &ALL_DIRECTIONS),
        Err(SequenceError::Replay {
            reason: "business rule violated",
            ..
        })
    ));
}
