//! `rebalance_weight` over hand-built results: a move onto another box's top face, and a pass
//! that tries every item -- including a weightless one, which no move can help -- and commits
//! nothing when the only other container holds none of them.

mod support;

use packvium_core::{
    AlgorithmReport, Container, Dimensions, Item, Length, PackedContainer, PackingConfig,
    PackingRequest, PackingResult, PackingStatus, Placement, Weight, WeightMove, rebalance_weight,
};
use support::{SIDE, container, cube, item, placed};

fn weighing(id: &str, grams: i64) -> Item {
    let mut item = item(id);
    item.weight = Weight(grams * Weight::TICKS_PER_G);
    item
}

fn sized(id: &str, inner: Dimensions) -> Container {
    let mut container = container();
    container.id = id.into();
    container.inner_dimensions = inner;
    container
}

fn packed(container: &Container, placements: Vec<Placement>) -> PackedContainer {
    PackedContainer {
        container: container.clone(),
        sequence: 1,
        placements,
        lattice_summary: None,
        lattice_items: Vec::new(),
    }
}

fn request(items: Vec<Item>, containers: Vec<Container>) -> PackingRequest {
    PackingRequest {
        items,
        containers,
        config: PackingConfig::default(),
        output_length_unit: "mm".into(),
        output_weight_unit: "g".into(),
        catalog_versions_used: Vec::new(),
        currency: None,
        fixed_placements: Vec::new(),
        fixed_containers: Vec::new(),
    }
}

fn result(containers: Vec<PackedContainer>) -> PackingResult {
    PackingResult {
        status: PackingStatus::Feasible,
        containers,
        unpacked: Vec::new(),
        algorithm: AlgorithmReport::default(),
        score: Vec::new(),
        warnings: Vec::new(),
        alternatives: Vec::new(),
        feasibility: None,
        termination: None,
        optimality: None,
        objective: "default".into(),
        catalog_versions_used: Vec::new(),
        currency: None,
    }
}

/// Where each container's placements stand, by item id.
fn layout(containers: &[PackedContainer]) -> Vec<Vec<(String, i64, i64)>> {
    containers
        .iter()
        .map(|packed| {
            packed
                .placements
                .iter()
                .map(|placement| {
                    (
                        placement.instance.id(),
                        placement.position.x,
                        placement.position.z,
                    )
                })
                .collect()
        })
        .collect()
}

/// The only free place in the lighter container is on top of the box already there, so
/// the moved item is seated on that face, fully supported by it.
#[test]
fn an_item_moves_onto_the_top_face_of_the_lighter_container() {
    let heavy = weighing("heavy", 500);
    let light = weighing("light", 100);
    let alone = weighing("alone", 100);
    let wide = sized("wide", cube(4 * SIDE));
    let column = sized(
        "column",
        Dimensions {
            length: Length(SIDE),
            width: Length(SIDE),
            height: Length(2 * SIDE),
        },
    );
    let original = result(vec![
        packed(
            &wide,
            vec![placed(&heavy, 1, 0, 0), placed(&light, 1, SIDE, 0)],
        ),
        packed(&column, vec![placed(&alone, 1, 0, 0)]),
    ]);
    let request = request(vec![heavy, light, alone], vec![wide, column]);

    let balanced = rebalance_weight(&request, &original, 4);

    assert_eq!(
        balanced.moves,
        [WeightMove {
            item_id: "light#1".into(),
            from_container_id: "wide#1".into(),
            to_container_id: "column#1".into(),
        }]
    );
    let seated = &balanced.containers[1].placements[1];
    assert_eq!((seated.position.x, seated.position.z), (0, SIDE));
    assert_eq!(seated.support_ratio, 1.0);
}

/// Every item of the heavier container is tried, the bridge's two-way load split is
/// recomputed for each trial, and none fits the other container -- so nothing moves.
#[test]
fn a_pass_with_no_admissible_destination_moves_nothing() {
    let loose = weighing("loose", 300);
    let mut base = weighing("base", 100);
    base.quantity = 2;
    let mut bridge = weighing("bridge", 100);
    bridge.dimensions.length = Length(2 * SIDE);
    let feather = weighing("feather", 0);
    let wide = sized("wide", cube(4 * SIDE));
    let tiny = sized("tiny", cube(SIDE / 2));
    let original = result(vec![
        packed(
            &wide,
            vec![
                placed(&base, 1, 0, 0),
                placed(&base, 2, SIDE, 0),
                placed(&bridge, 1, 0, SIDE),
                placed(&loose, 1, 2 * SIDE, 0),
                placed(&feather, 1, 3 * SIDE, 0),
            ],
        ),
        packed(&tiny, Vec::new()),
    ]);
    let request = request(vec![loose, base, bridge, feather], vec![wide, tiny]);

    let balanced = rebalance_weight(&request, &original, 4);

    assert!(!balanced.improved());
    assert_eq!(layout(&balanced.containers), layout(&original.containers));
}
