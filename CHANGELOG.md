# Changelog

The format follows [Keep a Changelog](https://keepachangelog.com/1.1.0/) and this project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html) — with the caveat
that the public API is not frozen until `1.0.0`. Pin an exact version.

## [0.1.1]

A patch over `0.1.0`. Every package is released together at the new version, including
the ones `0.1.0` did not break, so that one version number still describes one tested
set.

### Fixed

- **`lowest_landed_cost` could choose a container its own rate card cannot price.** When
  one container billed lighter than another but its rate table ran out before the
  shipment's billed weight, the search preferred it — returning the one packing you
  cannot actually buy over a priced alternative. Every engine now compares candidates by
  the money the rate table charges rather than by billed weight, which also fixes the
  case this objective exists for: a bracket step or a minimum charge can make the
  cheaper shipment the heavier one. If no container on offer can price the load, the
  request is refused with a message naming the container, its billed weight and the last
  bracket, in all four languages — previously two of them returned a result carrying a
  sentinel cost, and two aborted requests that had a shippable answer. `RateTable` gains
  a non-throwing `charge_minor_or_none` / `chargeMinorOrNull`; the throwing form is
  unchanged. No request or result field changed.

- **`@packvium/engine@0.1.0` could not be imported.** The published tarball was missing
  a runtime module that the fallback engine imports, so the first `import` of the package
  threw `ERR_MODULE_NOT_FOUND`. npm versions are immutable, which is why the fix has to
  arrive as a new version rather than a re-upload. Package assembly now dry-packs the
  tarball and resolves every relative import in the real published inventory, so a
  missing runtime file fails the release build instead of the consumer's first import.
  Only the Node package was affected; the Python, PHP and Rust `0.1.0` releases install
  and run correctly.

### Added

- **Commercial and control-plane API.** Three deterministic functions over one canonical
  JSON document: `quote` returns a landed cost together with the tariff version that
  produced it, `evaluate_policy` returns an eligibility decision together with the rule id
  and version that decided it, and `catalog_version_info` returns the metadata of one
  pinned catalog version. Exported as `packvium.commerce` (Python), `Packvium\Commerce\`
  (PHP), `packvium_core::commerce` (Rust, plus three C ABI entry points) and `commerce`
  on `@packvium/engine` and `@packvium/browser`. Prices are exact integers in minor
  currency units and every inexact division rounds up, so a quote is reproducible rather
  than approximately equal. No packing-request or packing-result field changed. Each
  package ships a runnable `commerce` example; the contract is in `docs/COMMERCE-API.md`.

## [0.1.0]

First release.

### Added

- Exact fixed-point units: length in 1/16000 mm, weight in 1/8 µg. Integer, decimal,
  fractional (`3/16`) and mixed-fraction (`12 3/8`) input across mm, cm, m, in and ft.
  Common fractional inches through 1/128 are exact integers.
- Immutable domain model — items, item instances, containers, obstacles, rotations,
  placements, requests and results. A call cannot mutate its inputs.
- Hard placement constraints: container boundaries, collisions, weight limits, permitted
  orientations, floor-only, non-stackable, direct top-load limits, minimum support ratio,
  tag compatibility and rectangular obstacles.
- A solver portfolio chosen per problem — regular grid, layer, extreme-point,
  maximal-space and bounded exact-small — with deadlines and a deterministic seed.
- Lexicographic objective ranking with top-K alternatives.
- Independent post-solve validation: every returned solution is re-checked by logic
  separate from the search that produced it. A solution that fails is reported as
  `invalid_result` rather than returned as if it were sound.
- Structured reasons for every unplaced item.
- JSON serialization and a command-line interface reading a request on standard input.
- Sequential nested packing — a packed container becomes an item at the next level.
- Extension points for custom placement constraints, item orderings, candidate scorers,
  container selectors, solution scorers and complete solvers.

### Known limitations

Rigid axis-aligned cuboids only. No global optimality guarantee for the heuristic
profiles, no transport physics, and top-load limits apply to what rests directly on an
item rather than to a whole stack. See `docs/GUARANTEES.md` for the full statement of
what is and is not promised.
