# Packvium for Rust

Deterministic 3D cartonization and rectangular bin packing with exact integer geometry.
No `unsafe` code. Five runtime dependencies: `serde`, `serde_json`, `sha2` (the digests that
chain plan revisions), `thiserror` and `web-time` (a clock that also works in WebAssembly).

Use it to pick the smallest carton for an order, build a pallet, load a shipping container,
or load a truck within its axle ratings and delivery-stop order. Every answer comes back as
coordinates and rotations for each item, with a reason for anything that did not fit.

Full documentation, the constraint reference and benchmarks live at
[packvium.com](https://packvium.com).

> **Version 1.5.0 — the public API is frozen.** Field names, status codes and the
> objective vector do not change without a major version, so any `1.x` is a safe upgrade
> from any earlier `1.x`.
> Read [docs/GUARANTEES.md](https://github.com/toxakara/packvium-rust/blob/main/docs/GUARANTEES.md)
> before relying on a result.

```toml
[dependencies]
packvium = "1.5"
```

## Quick start

The crate speaks a JSON contract — one request in, one result out — the same contract every
Packvium package speaks, so a request built here packs the same way in Python, PHP or Node.js:

```rust
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let request = r#"{
        "units": {"length": "mm"},
        "configuration": {
            "time_limit_ms": 60000,
            "effort_budget": {"max_search_nodes": 1000000}
        },
        "items": [
            {"id": "book", "quantity": 4, "weight": "650 g",
             "dimensions": {"length": "210", "width": "140", "height": "30"}}
        ],
        "containers": [
            {"id": "box", "inner_dimensions": {"length": "400", "width": "300", "height": "250"}}
        ]
    }"#;

    let result = packvium_core::pack_json(request)?;
    println!("{result}");
    Ok(())
}
```

The package is `packvium` in `Cargo.toml`, and the library is `packvium_core` in code — the
crate's own name, kept stable across releases.

The result is JSON text. Read it with `serde_json` into your own `#[derive(Deserialize)]`
structs, naming only the fields you use; [`basic.rs`](https://github.com/toxakara/packvium-rust/blob/main/examples/basic.rs)
does exactly that. Every length and weight comes back as exact integer `ticks` beside a
rendered decimal `value`: compute with the ticks, print the value.

## Examples

Runnable, each a single file you can read top to bottom:

```bash
cargo run --example basic
```

| File | What it shows |
| --- | --- |
| [`basic.rs`](https://github.com/toxakara/packvium-rust/blob/main/examples/basic.rs) | An order into a carton, read back into typed structs; an item that cannot fit, and why. |
| [`constraints.rs`](https://github.com/toxakara/packvium-rust/blob/main/examples/constraints.rs) | Rotations and keep-upright, stacking limits and top load, support ratio and incompatible tags — each shown with and without the rule. |
| [`objectives.rs`](https://github.com/toxakara/packvium-rust/blob/main/examples/objectives.rs) | The six objectives, and the scenes where two of them pick different containers. |
| [`units.rs`](https://github.com/toxakara/packvium-rust/blob/main/examples/units.rs) | What you can type (fractions, inches, pounds), why nothing is a float, and exact fits. |
| [`errors.rs`](https://github.com/toxakara/packvium-rust/blob/main/examples/errors.rs) | Branching on a refused request with `code()`, `reason()` and `field()`, and why an item that does not fit is not an error. |
| [`trucking.rs`](https://github.com/toxakara/packvium-rust/blob/main/examples/trucking.rs) | A delivery van: stops unloaded in route order through a named door, and axle loads kept inside their ratings. |
| [`nested.rs`](https://github.com/toxakara/packvium-rust/blob/main/examples/nested.rs) | Mugs and plates into cartons, cartons onto a pallet, with the typed `PackingRequest` API and `pack_nested`. |
| [`execution.rs`](https://github.com/toxakara/packvium-rust/blob/main/examples/execution.rs) | A result turned into a numbered work order, with a safe loading order computed from the placed geometry. |
| [`reproducibility.rs`](https://github.com/toxakara/packvium-rust/blob/main/examples/reproducibility.rs) | `effort_budget` against `time_limit_ms`: the same answer on every run and every machine. |
| [`shapes.rs`](https://github.com/toxakara/packvium-rust/blob/main/examples/shapes.rs) | Items that are not their box: complementary wedges sharing one crate as `convex_hull`, and a cushion that compresses under load until the crush limit refuses it. |
| [`commerce.rs`](https://github.com/toxakara/packvium-rust/blob/main/examples/commerce.rs) | Rate a shipment, apply an eligibility rule, and pin a catalog version. |
| [`artifacts.rs`](https://github.com/toxakara/packvium-rust/blob/main/examples/artifacts.rs) | Hand a result to a system with no engine: one document with the plan, geometry and the request that produced it, exported as CSV and a printable HTML work order — byte-identical to the other three engines. |
| [`revisions.rs`](https://github.com/toxakara/packvium-rust/blob/main/examples/revisions.rs) | Replan a half-loaded job: a missing item and a locked placement recorded against the approved plan, a replan that keeps the locked item in place, and a hash-chained record that notices an edit — the same revision bytes as the other three engines. |

## Errors

There are two kinds of failure, and only one of them is an `Err`.

A request that no engine may answer returns `PackError::InvalidRequest(RequestError)` before
anything is solved. It names the problem instead of describing it:

```rust
use packvium_core::PackError;

fn pack(request: &str) -> Result<String, String> {
    match packvium_core::pack_json(request) {
        Ok(result) => Ok(result),
        Err(PackError::InvalidRequest(error)) => {
            // error.code()   -> "invalid_request"
            // error.reason() -> "below_minimum"
            // error.field()  -> "/items/0/quantity", a JSON Pointer into your request
            // Display        -> "invalid_request: /items/0/quantity: must be at least 1"
            Err(format!("{} at {}: {error}", error.reason(), error.field()))
        }
        // `PackError` is `#[non_exhaustive]`, so keep a wildcard arm.
        Err(other) => Err(format!("{}: {other}", other.code())),
    }
}
```

`reason()` is one of `missing_field`, `wrong_type`, `below_minimum`, `above_maximum`,
`negative_measure`, `invalid_unit`, `duplicate_id`, `not_allowed` or `invalid_value`. A fixed
placement that is malformed or cannot hold has the code `invalid_fixed_placement` and the
reason `malformed` or `cannot_hold`.

Every `PackError` has `code()`, one closed string per kind of refusal: `invalid_request`,
`invalid_fixed_placement`, `invalid_input`, `unsupported_feature`, `unsupported_unit`,
`invalid_number`, `solution_failed_validation` or `time_limit`. Branch on the code, the reason
and the field; show the message to a person. The message is the same text in every Packvium
engine.

A request that is valid but does not fit completely is not an error. The result lists what was
left out, and why, in `unpacked_items`, and `packvium_core::explain_reason` turns each reason
code into a sentence.

## Determinism

The same request gives the same placements: the only randomness is seeded from the request's
`seed`, and no result depends on hash-map order. The one way to lose that is the wall clock: `time_limit_ms` stops the search when a
clock says so, and how far a search gets in that time depends on the machine and its load.

To get the same answer on every run and every host, bound the search by counted work with
`configuration.effort_budget` (`max_candidates_evaluated`, `max_placement_attempts`,
`max_search_nodes`, `max_restarts`) and set `time_limit_ms` far above what that budget needs,
as a fuse against a genuine hang. A search stopped by the budget says `effort_limit` in
`termination.code` and is still reproducible; one stopped by the clock says `time_limit`, and
an operational artifact built from it records its replay as `not_guaranteed`. `seed` is part
of the request, so a different seed is a different, equally reproducible request.

## What it does

- **Exact arithmetic.** Length is measured in ticks of 1/16000 mm and weight in 1/8 µg.
  No coordinate is ever a float, so no placement decision depends on rounding.
- **Real constraints.** Weight and payload limits, permitted rotations, keep-upright,
  floor-only, non-stackable, stack-count and top-load limits, minimum support ratio, tag
  incompatibility, clearance and rectangular obstacles; and for vehicles, route order by
  `stop_index`, unloading doors by `access_directions`, and two-axle load limits.
- **Six objectives.** Fewest containers, packaging cost, carrier-billable weight, carrier
  rate cards, open-dimension height and value kept — each a vector of exact integers.
- **A solver portfolio, not one algorithm.** Regular-grid, layer, extreme-point,
  maximal-space and bounded exact search, selected by problem shape and profile.
- **Answers you can check.** Every solution is re-validated by logic independent of the
  search. Unplaced items come back with a reason code, not silently missing.
- **Multi-container and nested.** A request splits across as many containers as it needs;
  `pack_nested` solves caller-defined levels — cartons, then pallets — in order; a level with no
  items of its own packs the previous level's containers, and the chain stops at the first level
  that cannot pack everything.
- **In-process extension points.** `SolverRegistry` accepts your own `PlacementConstraint`,
  `CandidateScorer`, `ItemOrderStrategy` or `Solver` for a typed `PackingRequest`. They live in
  your process only: nothing about them travels in the JSON contract, so a rule every caller
  must honour belongs in the request as data instead.
- **Work orders and portable artifacts.** `execution::build_plan_json` turns a result into
  an operator's step list, with the loading order from `safe_loading_order`.
  `artifacts::build_artifact_json` wraps that plan with geometry, display values and the
  request that produced it, and `artifact_exports` writes it as canonical JSON, CSV or a
  self-contained HTML work order, byte for byte what the Python, PHP and JavaScript packages
  write.
- **Items already in place, and replanning around them.** A request's `fixed_placements` pins
  items to known positions before the solve: they keep their place, carry weight and support,
  and come back marked `fixed: true`. `revisions` records what changed on the dock — a missing
  item, a substituted container, a lock, a verification — as an append-only chain linked by
  SHA-256, and derives the request the next plan solves.
- **`#![forbid(unsafe_code)]`.** The engine itself contains no `unsafe`.

## Documentation

| Document | Covers |
| --- | --- |
| [docs/GUARANTEES.md](https://github.com/toxakara/packvium-rust/blob/main/docs/GUARANTEES.md) | What is promised and what is not. Start here. |
| [docs/PUBLIC-API.md](https://github.com/toxakara/packvium-rust/blob/main/docs/PUBLIC-API.md) | Inputs, outputs and status semantics. |
| [docs/UNITS-AND-NUMERICS.md](https://github.com/toxakara/packvium-rust/blob/main/docs/UNITS-AND-NUMERICS.md) | Units, accepted input forms, rounding policy. |
| [docs/COMMERCE-API.md](https://github.com/toxakara/packvium-rust/blob/main/docs/COMMERCE-API.md) | Carrier rating, eligibility rules and catalog versions. |

## Requirements

Rust 1.88 or newer (edition 2024).

## The Packvium family

One request and result contract, implemented independently in four engines (Rust,
Python, PHP, JavaScript) and checked against each other on a shared fixture set: every
engine's answer is independently validated, and none may score below the agreed floor for
a fixture. Engines may return different, equally valid arrangements for the same request.
Pick the package for your stack; mixing them in one system is safe.

Documentation, the constraint reference and the benchmarks are at
[packvium.com](https://packvium.com).

| Package | Install | Source |
| --- | --- | --- |
| Python — [`packvium`](https://pypi.org/project/packvium/) | `pip install packvium` | [packvium-python](https://github.com/toxakara/packvium-python) |
| PHP — [`packvium/packvium`](https://packagist.org/packages/packvium/packvium) | `composer require packvium/packvium` | [packvium-php](https://github.com/toxakara/packvium-php) |
| Rust — [`packvium`](https://crates.io/crates/packvium) | `packvium = "1.5"` | [packvium-rust](https://github.com/toxakara/packvium-rust) |
| Node.js — [`@packvium/engine`](https://www.npmjs.com/package/@packvium/engine) | `npm install @packvium/engine` | [packvium-node](https://github.com/toxakara/packvium-node) |
| Browser / WebAssembly — [`@packvium/browser`](https://www.npmjs.com/package/@packvium/browser) | `npm install @packvium/browser` | [packvium-wasm](https://github.com/toxakara/packvium-wasm) |
| PHP FFI bridge — [`packvium/native-bridge`](https://packagist.org/packages/packvium/native-bridge) | `composer require packvium/native-bridge` | [packvium-php-bridge](https://github.com/toxakara/packvium-php-bridge) |
| Python native selector — `packvium-native` | from source until the native wheels ship | [packvium-python-adapter](https://github.com/toxakara/packvium-python-adapter) |

## Contributing

See [CONTRIBUTING.md](https://github.com/toxakara/packvium-rust/blob/main/CONTRIBUTING.md).
Security reports go through the process in
[SECURITY.md](https://github.com/toxakara/packvium-rust/blob/main/SECURITY.md), not public issues.

## Citation

If Packvium supports your research, cite it as software. GitHub's **Cite this repository**
button reads [`CITATION.cff`](https://github.com/toxakara/packvium-rust/blob/main/CITATION.cff), and
[`codemeta.json`](https://github.com/toxakara/packvium-rust/blob/main/codemeta.json) carries the same record in
CodeMeta form.

```bibtex
@software{packvium_rust,
  author  = {{Packvium contributors}},
  title   = {Packvium for Rust},
  version = {1.5.0},
  license = {MIT},
  url     = {https://packvium.com}
}
```

## License

MIT. See [LICENSE](https://github.com/toxakara/packvium-rust/blob/main/LICENSE).
