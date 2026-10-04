//! The independent validator judges a struct-level result, not a JSON one, so the rules a
//! solver never breaks -- and `pack_json` therefore never shows it breaking -- are only
//! reachable by handing it a deliberately broken result. Each test builds the smallest
//! arrangement that breaks exactly one rule and checks the validator names it.
//!
//! Measured from a test crate rather than an in-file module so the tests do not count
//! towards their own file's coverage (see `registry_and_nested.rs`).

mod support;

use std::collections::{BTreeMap, BTreeSet};

use packvium_core::{
    AlgorithmReport, Container, IndependentValidator, Item, ItemInstance, PackedContainer,
    PackingConfig, PackingRequest, PackingResult, PackingStatus, Placement, Rotation, ShapeType,
    UnpackedItem, ValidationReport,
};
use support::{SIDE, container, item, placed};

fn packed(container: &Container, sequence: usize, placements: Vec<Placement>) -> PackedContainer {
    PackedContainer {
        container: container.clone(),
        sequence,
        placements,
        lattice_summary: None,
        lattice_items: Vec::new(),
    }
}

fn request(items: Vec<Item>, container: Container) -> PackingRequest {
    PackingRequest {
        items,
        containers: vec![container],
        config: PackingConfig::default(),
        output_length_unit: "mm".into(),
        output_weight_unit: "g".into(),
        catalog_versions_used: Vec::new(),
        fixed_placements: Vec::new(),
        fixed_containers: Vec::new(),
    }
}

fn result(containers: Vec<PackedContainer>, unpacked: Vec<UnpackedItem>) -> PackingResult {
    PackingResult {
        status: PackingStatus::Feasible,
        containers,
        unpacked,
        algorithm: AlgorithmReport::default(),
        score: Vec::new(),
        warnings: Vec::new(),
        alternatives: Vec::new(),
        feasibility: None,
        termination: None,
        optimality: None,
        objective: "default".into(),
        catalog_versions_used: Vec::new(),
    }
}

fn codes(report: &ValidationReport) -> Vec<&str> {
    report
        .issues
        .iter()
        .map(|issue| issue.code.as_str())
        .collect()
}

/// Validate one container holding `placements` of `items`, and return the issue codes.
fn judge(items: Vec<Item>, container: Container, placements: Vec<Placement>) -> Vec<String> {
    let request = request(items, container.clone());
    let report = IndependentValidator.validate(
        &request,
        &result(vec![packed(&container, 1, placements)], Vec::new()),
    );
    codes(&report).into_iter().map(str::to_owned).collect()
}

fn stacked_pair(lower: &Item, upper: &Item) -> Vec<Placement> {
    vec![placed(lower, 1, 0, 0), placed(upper, 1, 0, SIDE)]
}

#[test]
fn a_sound_arrangement_has_no_issues() {
    let a = item("a");
    assert_eq!(
        judge(vec![a.clone()], container(), vec![placed(&a, 1, 0, 0)]),
        Vec::<String>::new()
    );
}

#[test]
fn an_item_both_placed_and_unpacked_is_double_accounted() {
    let a = item("a");
    let request = request(vec![a.clone()], container());
    let report = IndependentValidator.validate(
        &request,
        &result(
            vec![packed(&container(), 1, vec![placed(&a, 1, 0, 0)])],
            vec![UnpackedItem::new(
                ItemInstance {
                    item: a,
                    sequence: 1,
                },
                "search_exhausted".into(),
                Vec::new(),
            )],
        ),
    );
    assert_eq!(codes(&report), ["duplicate_accounting"]);
}

#[test]
fn opening_more_containers_than_the_inventory_holds_is_refused() {
    let mut a = item("a");
    a.quantity = 2;
    let mut limited = container();
    limited.quantity = Some(1);
    let request = request(vec![a.clone()], limited.clone());
    let report = IndependentValidator.validate_containers(
        &request,
        &[
            packed(&limited, 1, vec![placed(&a, 1, 0, 0)]),
            packed(&limited, 2, vec![placed(&a, 2, 0, 0)]),
        ],
    );
    assert_eq!(codes(&report), ["container_inventory"]);
}

#[test]
fn one_instance_placed_twice_is_a_duplicate() {
    let a = item("a");
    let issues = judge(
        vec![a.clone()],
        container(),
        vec![placed(&a, 1, 0, 0), placed(&a, 1, SIDE, 0)],
    );
    assert_eq!(issues, ["duplicate_item"]);
}

#[test]
fn a_group_split_across_containers_is_refused() {
    let mut a = item("a");
    a.quantity = 2;
    a.group = Some("pair".into());
    let request = request(vec![a.clone()], container());
    let report = IndependentValidator.validate(
        &request,
        &result(
            vec![
                packed(&container(), 1, vec![placed(&a, 1, 0, 0)]),
                packed(&container(), 2, vec![placed(&a, 2, 0, 0)]),
            ],
            Vec::new(),
        ),
    );
    assert_eq!(codes(&report), ["group_split"]);
}

#[test]
fn an_item_in_a_container_it_is_not_eligible_for_is_refused() {
    let mut a = item("a");
    a.eligible_container_tags = BTreeSet::from(["cold".to_owned()]);
    assert_eq!(
        judge(vec![a.clone()], container(), vec![placed(&a, 1, 0, 0)]),
        ["container_ineligible"]
    );
}

#[test]
fn a_rotation_the_item_forbids_is_refused() {
    let a = item("a");
    let mut turned = placed(&a, 1, 0, 0);
    turned.rotation = Rotation::Wlh;
    assert_eq!(
        judge(vec![a], container(), vec![turned]),
        ["rotation_forbidden"]
    );
}

#[test]
fn a_floor_item_off_the_floor_is_refused() {
    let base = item("base");
    let mut floor = item("floor");
    floor.must_be_on_floor = true;
    assert_eq!(
        judge(
            vec![base.clone(), floor.clone()],
            container(),
            stacked_pair(&base, &floor)
        ),
        ["floor_required"]
    );
}

#[test]
fn container_level_limits_are_each_named() {
    let mut a = item("a");
    a.quantity = 2;
    a.tags = BTreeSet::from(["fragile".to_owned()]);
    let two = vec![placed(&a, 1, 0, 0), placed(&a, 2, SIDE, 0)];

    let mut counted = container();
    counted.max_items = Some(1);
    assert_eq!(judge(vec![a.clone()], counted, two.clone()), ["max_items"]);

    let mut tagged = container();
    tagged.tag_limits = BTreeMap::from([("fragile".to_owned(), 1)]);
    assert_eq!(
        judge(vec![a.clone()], tagged, two.clone()),
        ["tag_count_exceeded"]
    );

    let mut reserved = container();
    reserved.void_fill_reserve_ppm = 1_000_000;
    assert_eq!(
        judge(vec![a], reserved, two),
        ["void_fill_reserve_exceeded"]
    );
}

#[test]
fn a_crushed_compressible_item_is_refused() {
    let mut soft = item("soft");
    soft.shape_type = ShapeType::Compressible;
    soft.compression_ratio_ppm = Some(0);
    soft.max_compression_pressure_kpa = Some(0);
    let heavy = item("heavy");
    assert_eq!(
        judge(
            vec![soft.clone(), heavy.clone()],
            container(),
            stacked_pair(&soft, &heavy)
        ),
        ["crush_violation"]
    );
}

#[test]
fn an_unknown_ground_contact_rule_does_not_invent_a_violation() {
    let base = item("base");
    let mut upper = item("upper");
    upper.ground_contact_rule = Some("levitating".into());
    assert_eq!(
        judge(
            vec![base.clone(), upper.clone()],
            container(),
            stacked_pair(&base, &upper)
        ),
        Vec::<String>::new()
    );
}

#[test]
fn a_later_stop_resting_on_an_earlier_one_breaks_unloading_order() {
    let mut early = item("early");
    early.stop_index = Some(1);
    let mut late = item("late");
    late.stop_index = Some(2);
    assert_eq!(
        judge(
            vec![early.clone(), late.clone()],
            container(),
            stacked_pair(&early, &late)
        ),
        ["unloading_order_violation"]
    );
}
