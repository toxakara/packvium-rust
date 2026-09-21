# Changelog

What changed in `packvium` on crates.io, release by release. The format follows
[Keep a Changelog](https://keepachangelog.com/1.1.0/) and this project adheres to
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [1.3.0]

Portable operational artifacts for the Rust core, and execution plans that match the other
engines beyond the test corpus. Nothing breaks 1.2.0.

### Added

- **`packvium_core::artifacts::build_artifact_json(request_json, result_json, loading_orders_json)`**
  builds one `packvium-operational-artifact/v1` document: the execution plan, exact geometry,
  the values a work order shows, and the request that produced it. Every Packvium
  engine builds the same bytes from the same result. Errors are `ArtifactError` with an
  `ArtifactErrorCode`.
- **`packvium_core::artifact_exports`** — `export_json`, `export_csv` (RFC 4180, 18 columns) and
  `export_work_order_html`: a printable work order in one HTML file with no scripts and no
  external resources.
- `examples/artifacts.rs`.

### Fixed

- **`execution::build_plan_json` disagreed with the other engines on results outside the test
  corpus.** Score terms are now copied and compared by value, a `null` status is refused, a
  `null` loading order means no order and a non-integer entry is refused, and a missing
  unplaced reason is written as the other engines write it. Floats are read exactly: the
  default JSON parser could read one a step off and write different digits back.
- The plan is written as RFC 8785, and a number beyond 2^53 − 1 is refused instead of written.

### Changed

- **The beam search caches its sort keys and the incumbent's key.** Results are byte-identical.
  On the wide cases measured (48 items, beam width 12) it took 18–26% less time for about
  0.5 MB more resident memory.
- A pinned catalog version resolves without scanning the version history.

## [1.2.0]

Execution plans for the Rust core, and a faster group-batching path. Nothing breaks 1.1.0.

### Added

- **`packvium_core::execution::build_plan_json(result_json, loading_orders_json)`** turns an
  already validated result into a work order: what to lift, why a carton was chosen, and what
  was not packed. It calls no solver and no validator. Solver decisions sit under `facts`,
  readable text under `presentation`, and the step order is taken from `loading_orders` or
  reported as `"unavailable"`. The canonical JSON is byte-identical to the other Packvium engines.

### Changed

- **Group batching does less work.** The extreme-point solver drops two quadratic scans over
  group members for a single pass. Results are byte-identical.

## Earlier releases

Up to 1.1.0 one changelog covered every Packvium language. Those entries are kept in
this repository's GitHub Releases for each tag.
