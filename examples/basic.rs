//! Pack a handful of items into a container and print the result.
//!
//!     cargo run --example basic

fn main() {
    let request = r#"{
        "items": [
            {"id": "book", "quantity": 4,
             "dimensions": {"length": "210", "width": "140", "height": "30"}}
        ],
        "containers": [
            {"id": "box", "inner_dimensions": {"length": "400", "width": "300", "height": "250"}}
        ]
    }"#;

    let result = packvium_core::pack_json(request).expect("well-formed request");
    println!("{result}");
}
