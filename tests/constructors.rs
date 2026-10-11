//! `Item::new` and `Container::new`.
//!
//! Each promises that every field it does not take holds the value a JSON request gets when
//! it leaves that field out. Comparing the two structs field by field would only restate the
//! constructor, so the promise is checked where it matters: the same scene, built once with
//! the constructors and once as JSON that omits every optional field, packs identically.

use packvium_core::{
    Container, Dimensions, EffortBudget, Item, Length, PackingConfig, PackingRequest, Weight,
    pack_json, pack_request,
};
use serde_json::{Value, json};

const MM: i64 = Length::TICKS_PER_MM;

fn millimetres(length: i64, width: i64, height: i64) -> Dimensions {
    Dimensions {
        length: Length(length * MM),
        width: Length(width * MM),
        height: Length(height * MM),
    }
}

fn configuration() -> PackingConfig {
    PackingConfig {
        time_limit_ms: 60_000,
        effort_budget: Some(EffortBudget {
            max_search_nodes: Some(100_000),
            ..EffortBudget::default()
        }),
        ..PackingConfig::default()
    }
}

#[test]
fn constructed_item_and_container_pack_like_json_that_omits_every_optional_field() {
    let request = PackingRequest {
        items: vec![
            Item::new(
                "book",
                millimetres(210, 140, 30),
                Weight(650 * Weight::TICKS_PER_G),
                4,
            ),
            Item::new(
                "mug",
                millimetres(120, 90, 100),
                Weight(350 * Weight::TICKS_PER_G),
                2,
            ),
        ],
        containers: vec![Container::new("carton", millimetres(400, 300, 250))],
        config: configuration(),
        output_length_unit: "mm".into(),
        output_weight_unit: "g".into(),
        catalog_versions_used: Vec::new(),
        fixed_placements: Vec::new(),
        fixed_containers: Vec::new(),
    };
    let typed = pack_request(&request)
        .expect("the typed scene packs")
        .to_json("mm", "g", false);

    let text = json!({
        "units": { "length": "mm", "weight": "g" },
        "configuration": { "time_limit_ms": 60000, "effort_budget": { "max_search_nodes": 100000 } },
        "items": [
            { "id": "book", "quantity": 4, "weight": "650",
              "dimensions": { "length": "210", "width": "140", "height": "30" } },
            { "id": "mug", "quantity": 2, "weight": "350",
              "dimensions": { "length": "120", "width": "90", "height": "100" } }
        ],
        "containers": [
            { "id": "carton", "inner_dimensions": { "length": "400", "width": "300", "height": "250" } }
        ]
    })
    .to_string();
    let parsed: Value =
        serde_json::from_str(&pack_json(&text).expect("the JSON scene packs")).expect("JSON");

    assert_eq!(typed["containers"], parsed["containers"]);
    assert_eq!(typed["unpacked_items"], parsed["unpacked_items"]);
    assert_eq!(parsed["unpacked_items"], json!([]));
}

#[test]
fn struct_update_syntax_overrides_one_field_and_keeps_the_rest() {
    let fragile = Item {
        stackable: false,
        ..Item::new("vase", millimetres(100, 100, 100), Weight(0), 1)
    };
    assert!(!fragile.stackable);
    assert_eq!(fragile.quantity, 1);
    assert_eq!(fragile.allowed_rotations.len(), 6);

    let priced = Container {
        cost_minor: 120,
        ..Container::new("carton", millimetres(400, 300, 250))
    };
    assert_eq!(priced.cost_minor, 120);
    assert_eq!(priced.quantity, None);
    assert!(!priced.is_flat_bag());
}
