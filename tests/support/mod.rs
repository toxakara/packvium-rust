//! Struct-level builders shared by the test crates that hand the core objects directly
//! rather than JSON. Each test crate compiles its own copy and uses a subset.
#![allow(dead_code)]

use std::collections::{BTreeMap, BTreeSet};

use packvium_core::{
    Container, Dimensions, Item, ItemInstance, Length, Placement, Point, Rotation, ShapeType,
    Weight,
};

pub const SIDE: i64 = 10 * Length::TICKS_PER_MM;

pub fn cube(side: i64) -> Dimensions {
    Dimensions {
        length: Length(side),
        width: Length(side),
        height: Length(side),
    }
}

pub fn item(id: &str) -> Item {
    Item {
        id: id.into(),
        dimensions: cube(SIDE),
        weight: Weight(Weight::TICKS_PER_G),
        quantity: 1,
        allowed_rotations: vec![Rotation::Lwh],
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

pub fn container() -> Container {
    Container {
        id: "box".into(),
        inner_dimensions: cube(4 * SIDE),
        outer_dimensions: None,
        tare_weight: Weight(0),
        max_payload: None,
        cost_minor: 0,
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

pub fn placed(item: &Item, sequence: usize, x: i64, z: i64) -> Placement {
    let position = Point { x, y: 0, z };
    Placement {
        instance: ItemInstance {
            item: item.clone(),
            sequence,
        },
        position,
        rotation: Rotation::Lwh,
        dimensions: item.dimensions,
        envelope_origin: position,
        envelope_dimensions: item.dimensions,
        support_ratio: 1.0,
        top_load: Weight(0),
        fixed: false,
    }
}
