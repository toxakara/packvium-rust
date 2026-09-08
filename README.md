# Packvium for Rust

Deterministic 3D cartonization and rectangular bin packing. No unsafe code, no runtime
dependencies beyond `serde`, exact integer geometry.

Full documentation, the constraint reference and benchmarks live at
[packvium.com](https://packvium.com).

> **Version 1.1.0 — the public API is frozen.** Field names, status codes and the
> objective vector do not change without a major version, so any `1.x` is a safe upgrade
> from any earlier `1.x`.
> Read [docs/GUARANTEES.md](docs/GUARANTEES.md) before relying on a result.

```toml
[dependencies]
packvium = "1.0"
```

## Quick start

The crate's API is a JSON contract — one request in, one result out — the same contract
every language binding built on this engine speaks:

```rust
let request = r#"{
    "items": [
        {"id": "book", "quantity": 4,
         "dimensions": {"length": "210", "width": "140", "height": "30"}}
    ],
    "containers": [
        {"id": "box", "inner_dimensions": {"length": "400", "width": "300", "height": "250"}}
    ]
}"#;

let result = packvium_core::pack_json(request)?;
```

`use packvium_core::...` is correct even though the package is `packvium` in
`Cargo.toml` — that is the crate's own internal name, kept stable across the release.

```bash
cargo run --example basic
```

## Examples

Runnable, in [`examples/`](examples). Each one is a single file you can read top to bottom
and execute without a project around it.

| File | What it shows |
| --- | --- |
| [`basic.rs`](examples/basic.rs) | The smallest useful call: items in, placements out. |
| [`shapes.rs`](examples/shapes.rs) | Items that are not their box: complementary wedges sharing one crate as `convex_hull`, and a cushion that compresses under load until the crush limit refuses it. |
| [`pack-stdin.rs`](examples/pack-stdin.rs) | Read a shared-contract request on stdin and write the result to stdout. |
| [`commerce.rs`](examples/commerce.rs) | Rate a shipment, apply an eligibility rule, and pin a catalog version. |
| [`commerce-stdin.rs`](examples/commerce-stdin.rs) | The same three functions over stdin/stdout. |

```bash
cargo run --example basic
```

## What it does

- **Exact arithmetic.** Length is measured in ticks of 1/16000 mm and weight in 1/8 µg.
  No coordinate is ever a float, so no placement decision depends on rounding.
- **Real constraints.** Weight and payload limits, permitted rotations, keep-upright,
  floor-only, non-stackable, top-load limits, minimum support ratio, tag incompatibility,
  clearance and rectangular obstacles.
- **A solver portfolio, not one algorithm.** Regular-grid, layer, extreme-point,
  maximal-space and bounded exact search, selected by problem shape and profile.
- **Answers you can check.** Every solution is re-validated by logic independent of the
  search. Unplaced items come back with a reason code, not silently missing.
- **Deterministic.** The same input and seed produce the same result, always.
- **Multi-container and nested.** Split across containers, or pack containers into
  containers.
- **Extensible.** Register your own constraints, item orderings, candidate scorers,
  container selectors or complete solvers.
- **`#![forbid(unsafe_code)]`.** The engine itself contains no `unsafe`.

## Documentation

| Document | Covers |
| --- | --- |
| [docs/GUARANTEES.md](docs/GUARANTEES.md) | What is promised and what is not. Start here. |
| [docs/PUBLIC-API.md](docs/PUBLIC-API.md) | Inputs, outputs and status semantics. |
| [docs/UNITS-AND-NUMERICS.md](docs/UNITS-AND-NUMERICS.md) | Units, accepted input forms, rounding policy. |

## Requirements

Rust 1.88 or newer (edition 2024).

## The Packvium family

One request and result contract, implemented independently in four engines (Rust,
Python, PHP, JavaScript) and held to identical placements on a shared fixture set.
Pick the package for your stack; mixing them in one system is safe.

Documentation, the constraint reference and the benchmarks are at
[packvium.com](https://packvium.com).

| Package | Install | Source |
| --- | --- | --- |
| Python — [`packvium`](https://pypi.org/project/packvium/) | `pip install packvium` | [packvium-python](https://github.com/toxakara/packvium-python) |
| PHP — [`packvium/packvium`](https://packagist.org/packages/packvium/packvium) | `composer require packvium/packvium` | [packvium-php](https://github.com/toxakara/packvium-php) |
| Rust — [`packvium`](https://crates.io/crates/packvium) | `packvium = "1.0"` | [packvium-rust](https://github.com/toxakara/packvium-rust) |
| Node.js — [`@packvium/engine`](https://www.npmjs.com/package/@packvium/engine) | `npm install @packvium/engine` | [packvium-node](https://github.com/toxakara/packvium-node) |
| Browser / WebAssembly — [`@packvium/browser`](https://www.npmjs.com/package/@packvium/browser) | `npm install @packvium/browser` | [packvium-wasm](https://github.com/toxakara/packvium-wasm) |
| PHP FFI bridge — [`packvium/native-bridge`](https://packagist.org/packages/packvium/native-bridge) | `composer require packvium/native-bridge` | [packvium-php-bridge](https://github.com/toxakara/packvium-php-bridge) |
| Python native selector — `packvium-native` | from source until the native wheels ship | [packvium-python-adapter](https://github.com/toxakara/packvium-python-adapter) |

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md). Security reports go through the process in
[SECURITY.md](SECURITY.md), not public issues.

## License

MIT. See [LICENSE](LICENSE).
