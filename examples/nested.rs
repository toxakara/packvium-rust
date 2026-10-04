//! Nested packing: units into cartons, cartons onto a pallet.
//!
//!     cargo run --example nested
//!
//! Real fulfilment is rarely one level. Mugs and plates go into cartons, and the cartons go
//! onto a pallet. `pack_nested` takes those levels as data -- a name and an ordinary typed
//! `PackingRequest` each -- solves them in order, and hands every level's result back under
//! its name, so a two-level plan can be stored, replayed and audited as one thing.
//!
//! A level with no items of its own takes the previous level's result: each packed carton
//! becomes one item with the carton's *outer* dimensions and its gross weight, as Python's
//! `NestedPacker` does. A level that lists its own items keeps them. The levels stay
//! independent otherwise -- a carton already taped shut is not repacked to make a better
//! pallet -- and the chain stops at the first level that cannot pack everything.
//!
//! This is also the typed side of the API: `Item`, `Container` and `PackingRequest` are
//! plain structs with public fields, and `pack_request` returns a `PackingResult` whose
//! placements and weights are exact integers.

use packvium_core::{
    Container, Dimensions, Item, Length, NestedLevel, NestedPackingRequest, PackResult,
    PackingConfig, PackingRequest, Rotation, ShapeType, SolverProfile, Weight, pack_nested,
};
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};

fn length(text: &str) -> PackResult<Length> {
    Length::parse(&json!(text), "mm")
}

fn weight(text: &str) -> PackResult<Weight> {
    Weight::parse(&json!(text), "g")
}

fn dimensions(length_mm: &str, width_mm: &str, height_mm: &str) -> PackResult<Dimensions> {
    Ok(Dimensions {
        length: length(length_mm)?,
        width: length(width_mm)?,
        height: length(height_mm)?,
    })
}

/// An item with no rule beyond its size, weight and count. Every field is public and has to
/// be named; these are the values a JSON request gets when it leaves the field out.
fn item(id: &str, size: Dimensions, item_weight: Weight, quantity: usize) -> Item {
    Item {
        id: id.into(),
        dimensions: size,
        weight: item_weight,
        quantity,
        allowed_rotations: Rotation::ALL.to_vec(),
        stackable: true,
        must_be_on_floor: false,
        max_top_load: None,
        minimum_support_ratio: 0.0,
        group: None,
        tags: BTreeSet::new(),
        incompatible_tags: BTreeSet::new(),
        priority: 0,
        metadata: BTreeMap::new(),
        nesting_height: None,
        max_stacked_items: None,
        ground_contact_rule: None,
        stop_index: None,
        eligible_container_tags: BTreeSet::new(),
        value: None,
        shape_type: ShapeType::RigidCuboid,
        hull_vertices: None,
        compression_ratio_ppm: None,
        max_compression_pressure_kpa: None,
    }
}

struct ContainerSpec<'a> {
    id: &'a str,
    inner: Dimensions,
    outer: Option<Dimensions>,
    tare: Weight,
    max_payload: Weight,
    cost_minor: i64,
}

fn container(spec: ContainerSpec<'_>) -> Container {
    Container {
        id: spec.id.into(),
        inner_dimensions: spec.inner,
        outer_dimensions: spec.outer,
        tare_weight: spec.tare,
        max_payload: Some(spec.max_payload),
        cost_minor: spec.cost_minor,
        quantity: None,
        obstacles: Vec::new(),
        tags: BTreeSet::new(),
        max_items: None,
        metadata: BTreeMap::new(),
        axles: None,
        void_fill_reserve_ppm: 0,
        tag_limits: BTreeMap::new(),
        max_stack_density: None,
        rate_table: None,
        access_directions: Vec::new(),
        preloaded: Vec::new(),
    }
}

/// Counted work bounds the search, so each level gives the same answer on every host; the
/// time limit is only a safety fuse. One deterministic start is plenty for this scene.
fn configuration() -> PackingConfig {
    let mut config = PackingConfig {
        profile: SolverProfile::Fast,
        time_limit_ms: 60_000,
        ..PackingConfig::default()
    };
    let mut budget = config.effort_budget.unwrap_or_default();
    budget.max_candidates_evaluated = Some(1_000_000);
    budget.max_placement_attempts = Some(1_000_000);
    budget.max_search_nodes = Some(1_000_000);
    config.effort_budget = Some(budget);
    config
}

fn request(items: Vec<Item>, containers: Vec<Container>) -> PackingRequest {
    PackingRequest {
        items,
        containers,
        config: configuration(),
        output_length_unit: "mm".into(),
        output_weight_unit: "kg".into(),
        catalog_versions_used: Vec::new(),
        fixed_placements: Vec::new(),
        fixed_containers: Vec::new(),
    }
}

fn kilograms(value: Weight) -> String {
    let grams = value.0 / Weight::TICKS_PER_G;
    format!("{}.{:03} kg", grams / 1000, grams % 1000)
}

fn main() -> PackResult<()> {
    // What the customer ordered.
    let order = vec![
        item("mug", dimensions("120", "120", "100")?, weight("400")?, 24),
        item("plate", dimensions("260", "260", "20")?, weight("600")?, 16),
    ];

    // Level 1: choose cartons. `outer_dimensions` matters here -- the pallet packs the
    // outside of each carton, wall thickness included.
    let cartons = vec![
        container(ContainerSpec {
            id: "box-s",
            inner: dimensions("300", "300", "300")?,
            outer: Some(dimensions("310", "310", "310")?),
            tare: weight("300")?,
            max_payload: weight("15000")?,
            cost_minor: 120,
        }),
        container(ContainerSpec {
            id: "box-l",
            inner: dimensions("400", "400", "400")?,
            outer: Some(dimensions("412", "412", "412")?),
            tare: weight("500")?,
            max_payload: weight("25000")?,
            cost_minor: 180,
        }),
    ];
    let carton_level = NestedLevel {
        name: "carton".into(),
        request: request(order, cartons),
    };

    // Level 2: put those cartons on a pallet. The deck is the inner footprint and the
    // usable stack height is the rest.
    let pallet = container(ContainerSpec {
        id: "euro",
        inner: dimensions("1200", "800", "1400")?,
        outer: None,
        tare: weight("25000")?,
        max_payload: weight("700000")?,
        cost_minor: 1500,
    });
    // No items: this level packs whatever the carton level packed.
    let pallet_level = NestedLevel {
        name: "pallet".into(),
        request: request(Vec::new(), vec![pallet]),
    };

    // The whole plan as one value: both levels, solved in order, each result under its name.
    let plan = pack_nested(&NestedPackingRequest {
        levels: vec![carton_level, pallet_level],
        beam_width: 1,
    })?;

    for (name, result) in &plan.levels {
        println!("== {name} ==");
        println!("   status: {}", result.status.as_str());
        println!("   containers used: {}", result.containers.len());
        for packed in &result.containers {
            println!(
                "     {:<8} holding {:>2} item(s), gross {}",
                packed.id(),
                packed.placement_count(),
                kilograms(packed.gross_weight())
            );
        }
        if !result.unpacked.is_empty() {
            println!("   left over: {}", result.unpacked.len());
        }
        println!();
    }

    let (cartons, pallets) = (&plan.levels[0].1, &plan.levels[1].1);
    println!(
        "{} carton(s) travelling on {} pallet(s); every carton is on a pallet: {}",
        cartons.containers.len(),
        pallets.containers.len(),
        pallets.packed_item_count() == cartons.containers.len()
    );
    Ok(())
}
