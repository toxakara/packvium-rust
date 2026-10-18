# Changelog

What changed in `packvium` on crates.io, release by release. The format follows
[Keep a Changelog](https://keepachangelog.com/1.1.0/) and this project adheres to
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [1.7.0]

The reference validator ships in the crate, `quality` looks ahead in its block solver and
finishes inside its time limit, and extreme points scan the candidate set they were meant to.
No request or result field changes.

### Added

- **`validate_json(request, result)` and `validate_json_outcome`**, with `ResultIssue`,
  `MalformedResult { kind, message }` and `RefusalKind`: the reference validator over the JSON
  text, returning the same issues, code and detail, in the same order, as every other package.
  It shares no code with the solvers or `IndependentValidator`, which is unchanged.
- **`PlacementConstraint::distinguishes_instances`**, a defaulted trait method, `true` unless
  overridden, so existing implementations compile and keep their behaviour. A constraint that
  reads only item attributes may return `false` to let the portfolio drop a start order that
  repeats another's item-type sequence.

### Changed

- **A `CandidateScorer` leads the extreme-point rank.** The rank is the tuple (sum of
  scorer scores, far corner z, y, x, origin, rotation); before, a scorer's `score` was added to
  one weighted sum of the far-corner ticks at the weight of one x tick. A scorer that returned
  small nudges meant as tie-breaks now decides the rank; return 0 where it has no preference.
  The trait's signature is unchanged, and no request without a custom scorer answers
  differently on the measured corpora.
- **Every schema enum value is refused as `not_allowed`.** A value outside its list in
  `allowed_rotations`, the output and dimensional-weight units, `shape_type`,
  `ground_contact_rule` or `configuration.solvers` is `invalid_request` with reason `not_allowed`
  at its JSON Pointer. An unknown output unit was copied through; **`ticks` as an output unit is
  now refused**, as the schema always said.
- **`quality` looks ahead in the block solver**, completing its leading blocks greedily
  and committing the best, so the answer is never worse than the greedy pass. On our benchmark
  85 of 89 answers improved and none got worse. Under a tight effort budget the block start can
  now report `truncated` / `effort_limit`.
- **`quality` finishes inside its limit.** The staged beam refines the best greedy
  order instead of the first two, and the spatial index stops at the first hit without
  allocating. On the 22 pinned benchmark runs 1.6.0 needed more than 20 s of CPU on 10; this
  release needs at most 8.1 s.
- **Extreme points scan the exposed corners and their projections**, not every
  combination of placed faces. With `solvers: ["extreme_points"]` alone, over 67 benchmark
  instances, `fast`/`balanced`/`quality` leave 2711/1477/1297 items out instead of
  3658/3048/2643. The full portfolio's answers did not change on the 89 benchmark requests, and
  its `quality` CPU fell to 42%.
- **Portfolio starts go to distinct item orders.** A start that repeats another's
  item-type sequence runs a neighbouring order instead, kept only on a strict win. A stacking
  scene that took two containers now takes one.
- **`no_eligible_container` is reported `proven`, and `insufficient_support` is reported**
  (`observed`) where the support rule is the only blocker.
- **A `nesting_height` not below the item's height is refused as `invalid_value`** at its
  pointer, and with no `output` block the result is written in `units.length`'s schema code.

### Fixed

- **`incompatible_tags` is applied item against item**, not against container tags, as the
  validator and every other engine apply it.
- **A panicking start fails a parallel portfolio** as it fails a sequential one; it was dropped.
- **Axle-balanced candidate points use the envelope length under clearance** and are not offered
  twice.

## [1.6.0]

Boxes cut to measure, flat bags, stacking groups and a volume-first objective, and a fix for
small orders with a `convex_hull` item that ran out the clock.

### Added

- **`EffortBudget` is re-exported from the crate root**, and **`Item::new` / `Container::new`**
  fill every other field with the value a JSON request gets when it omits it.
- **`MadeToMeasure` and `Container::made_to_measure`.** An on-demand box machine (largest box,
  minimum, cut step, board price and weight per m²); each packed box is the load's extent
  rounded up onto the machine grid and priced as cut, reported by
  `PackedContainer::achieved_dimensions()` and the result's `made_to_measure`.
- **`DeformableBoundary::FlatBag` and `Container::deformable_boundary`.** A poly mailer admits a
  load when length plus thickness and width plus thickness fit.
- **`Item::stacking_group` and `Item::accepts_stacking_groups`.** Which item classes may rest
  directly on which.
- **`objective: "maximum_volume"`**, ranking the volume left behind ahead of the piece count.
- **A warning for items resting on nothing** when `minimum_support_ratio` is 0. No answer
  changes.

Code that builds `Item` or `Container` with a struct literal must add the new fields; `None`
and an empty set keep 1.5.0's behaviour.

### Changed

- **An unpacked item names the limit that left it out, the same in every engine.**
  Where the search used to fall back to `no_feasible_placement` or `search_exhausted`, an item a
  fresh container would take but none is left reports `container_inventory_exhausted`, and one
  that overloads an axle wherever it stands alone reports `axle_overloaded`; both are
  `inferred`. Code that matched the old reason for these cases must match the new one.
- **An explicit `solvers: ["homogeneous_blocks"]` request packs in the profile's start order
  when the block solver declines the scene**, so the answer no longer depends on the
  order the items are listed in.
- **`quality` orders its beam by a `convex_hull` item's own solid**, not its box, so
  it no longer assumes a second wedge cannot fit when it can.
- **The container plan counts a `convex_hull` item by its own solid too**, so
  `quality` no longer ranks a plan as if wedges that share a container each needed their own.
- **`fast` and `balanced` add the block solver as one more start** and keep its answer only
  when strictly better.

### Fixed

- **Small orders with a `convex_hull` item no longer run out the clock.** The exact search
  bounded a hull by its box volume, so it never stopped early: a five-item order with one jar
  took the whole time limit and now takes 6 search nodes. The same bound left one of two
  wedges that share a crate unpacked. Every release since 1.0.0.

## [1.5.0]

Every error names a closed code. A misspelt configuration key is now refused (see *Fixed*).

### Added

- **`pack_json_with(input, ExecutionOptions)`.** `pack_json` with explicit execution options;
  `ExecutionOptions { parallel: false }` runs independent search starts in sequence. The
  answer is the same either way.
- **`PackError::code()`** — the machine-readable code of every variant: `invalid_input`,
  `invalid_request`, `invalid_fixed_placement`, `unsupported_feature`, `unsupported_unit`,
  `invalid_number`, `solution_failed_validation`, `time_limit`. Branch on it instead of on the
  message.
- **`PackError::request_error()`** — the `RequestError` behind `invalid_request` and
  `invalid_fixed_placement`, with its `reason()` and JSON Pointer `field()`, when there is one.

### Changed

- **Up to 41% faster on beam searches, same results.** The collision index keeps its cells in a
  flat array instead of a tree and sizes each query's result once. Measured: beam requests 41%
  faster, a 60-item order 34%, a 250-item order 18%, peak memory within 4%. Every result is
  byte-identical.
- **`pack_nested` chains levels.** A level whose request lists no items packs the previous
  level's containers, one item each; it is not run after a level that left something unpacked.
  A level that lists its own items runs on them as before. A non-zero `beam_width` now sets
  every level's `container_plan_beam_width`; it was ignored.

### Fixed

- **`quality` with `minimum_support_ratio` answers instead of failing.** The homogeneous block
  solver ran even when the request asked for support, and a block set on a smaller one
  overhangs it, so the request ended in `solution_failed_validation`. Such a request is now
  left to the per-item search. A block answer also reports the support each item really has;
  it wrote `support_ratio` 1.0 for every item.
- **A misspelt `configuration` key is refused.** A key the request schema does not declare in
  `configuration` or its `effort_budget` (`profile` for `solver_profile`, `top_k` for
  `alternatives`) was silently ignored, so the request ran on defaults. `pack_json` now returns
  `PackError::InvalidRequest` with reason `not_allowed` and the key's pointer, for example
  `/configuration/profile`. `parallel`, which this engine read although the schema never
  declared it, is refused too; pass `ExecutionOptions { parallel: false }` to `pack_json_with`
  to run search starts in sequence.
- **An item that fits no orientation no longer spends the exact search's budget.** One such
  item made a scene solved in about 1 ms run to its whole budget (2.7 s and `effort_limit` at
  1M); it is now set aside before the search, which finishes in about 1 ms. Placements are
  unchanged.
- **An unknown `objective` or access direction is refused with its pointer** (`not_allowed`),
  and refusal messages match the documentation.

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
