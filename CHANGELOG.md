# Changelog

What changed in `packvium` on crates.io, release by release. The format follows
[Keep a Changelog](https://keepachangelog.com/1.1.0/) and this project adheres to
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [1.4.0]

Replanning a job that has already started, a beam search that no longer trades memory for
speed, and container ids that match the other engines.

### Added

- **Fixed placements.** `fixed_placements` in a JSON request, or
  `PackingRequest.fixed_placements` with `packvium_core::FixedPlacement`. Each entry pins an
  item type to a container type and instance, at the origin a result reports, in one
  orientation. Fixed items keep their place, count toward weight, support and top load, and
  come back marked `"fixed": true`. A fixed set that is not a valid packing on its own is
  refused with `invalid_fixed_placement: ...` before any search.
- **`packvium_core::revisions`** — `root_revision_json`, `derive_revision_json`,
  `apply_events_json`, `verify_revision_chain_json`, `document_digest_json` and
  `canonical_revision_json`. A `packvium-plan-revision/v1` chain records what happened on the
  dock against the artifact it changed, linked by SHA-256, and carries the request the next
  plan solves. Every Packvium engine computes the same bytes from the same inputs. Errors are
  `RevisionError` with a `RevisionErrorCode`.
- **A new dependency, `sha2`** (RustCrypto, no default features), for the digest.
- **`PackError::InvalidRequest(RequestError)`.** A malformed request names what is wrong:
  `code()` is `invalid_request`, `reason()` one of a closed set (`missing_field`, `wrong_type`,
  `below_minimum`, `above_maximum`, `negative_measure`, `invalid_unit`, `duplicate_id`,
  `not_allowed`, `invalid_value`), `field()` the JSON Pointer of the bad value, and the message
  reads `invalid_request: /items/0/quantity: must be at least 1`. The fixed-placement refusal is
  a `RequestError` too, with code `invalid_fixed_placement`.
- `examples/revisions.rs`.

### Changed

- **`PackError` is `#[non_exhaustive]`, and a malformed request is `InvalidRequest`.** It used
  to be `InvalidInput`, `InvalidNumber` or `UnsupportedUnit` with free text. An exhaustive
  `match` on `PackError` needs a wildcard arm; formatting the error is unaffected.
- **The container-plan beam keeps 1.3.0's speed without its memory cost.** It ranks nodes by
  five integers, compares placement signatures in place only when ranks tie, and orders only
  the nodes it keeps. Results are byte-identical. At equal work it took 0.70× and 0.71× the
  time of 1.3.0's, and peaked at 0.986× and 0.992× of 1.2.0's memory in a Linux container.
- **Container ids number each type from 1.** The greedy and beam searches counted containers
  across types, so a crate opened after a box was `crate#2` where the other engines said
  `crate#1`. Only answers with more than one container type change, and only their ids.
- **`PackingRequest` gains `fixed_placements` and `fixed_containers`, `Container` gains
  `preloaded`, and `Placement` gains `fixed`.** JSON callers are unaffected. Code that builds
  these structs with a literal must add the fields: empty vectors and `false` keep 1.3.0's
  behaviour.

### Fixed

- **Numbers below their floor are refused.** A negative item or container quantity was
  packed, and a zero or negative `time_limit_ms`, `max_containers`, `max_items`,
  `alternatives`, search width or rate-table amount was silently replaced by its default. Each
  is now an error naming the field; an absent or null value keeps its default.
- **The independent validator checks incompatible tags.** It had no `incompatible_items` rule,
  so an answer with incompatible items in one container passed it. The search already
  enforced the rule, so only a hand-built answer or a fixed set could slip through.
- **A placement or obstacle far outside its container is refused.** An origin near the top of
  the integer range overflowed when its extent was added, and the box then looked inside.
- **A fixed placement is refused, never coerced, when it is not the schema's shape.** An
  instance of `"1"` or `1.5`, a position given as a list or null, or an unknown key is refused
  naming the entry, instead of falling back to instance 1 or the origin.

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
