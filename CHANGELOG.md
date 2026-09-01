# Changelog

The format follows [Keep a Changelog](https://keepachangelog.com/1.1.0/) and this project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html). As of `1.0.0` the
public API, the request and result schemas, the numeric and units policy, the validation
rules and the compatibility policy are frozen: breaking any of them costs a major version.

## [1.0.0]

The stable core release. It freezes the contract that already exists rather than adding a
feature wave: the public API, the request and result schemas, the numeric and units policy,
the validation rules and the compatibility policy will not break without a major version.

### Read this before depending on 1.0.0

- **Platforms exercised for this release: Linux x86_64, Linux aarch64 and macOS arm64.**
  Python wheels are built and installed into a clean Python 3.9 on all three; the PHP FFI
  bridge is verified against the real shared library on PHP 8.2, 8.3, 8.4 and 8.5 on both
  Linux architectures. **Windows and macOS x86_64 native binaries are not built and not
  published.** Without a native binary the pure Python and PHP engines run unchanged — that
  is the documented default, and it costs speed rather than correctness.
- **Release artifacts carry no build-provenance attestation.** `gh attestation verify` will
  not succeed against 1.0.0. The SBOM and SHA-256 manifests are the integrity evidence for
  this release; check them before installing from anywhere other than the official
  registry.

### Added

- **A sound lower bound on the objective, in every engine.** Computed from the request
  alone before a search begins, and identical across the Python, PHP, Rust and JavaScript
  implementations on 381 corpus cases. It is not a result field: reporting an optimality
  gap would widen the contract this release exists to freeze.
- **A declared numeric ceiling shared by all four engines.** A sum too large to stay exact
  returns a structured refusal instead of a number that one language would round and
  another would not. The limit is stated by the library rather than inherited from each
  language, so the four agree about which requests are answerable.
- **A published coverage frontier for optimality claims.** Where the bound is actually
  attained is measured and committed, so `optimal` is bounded by evidence rather than used
  as a label.

### Fixed

- **A bound could be returned that JavaScript cannot represent exactly.** Values above
  `2^53-1` are now refused rather than silently rounded in one engine and exact in the
  others.
- **The prebuilt Node addon had no quality floor.** It is now held to a per-fixture budget
  like the other independent engines.

### Not claimed

- **Identical placements across engines.** Different engines may return different, equally
  valid arrangements. This is measured and budgeted, not accidental — and it includes the
  JavaScript fallback and the prebuilt native addon, which differ on 18 of 397 corpus
  fixtures.
- **Optimal packings for arbitrary requests.** 3D packing remains NP-hard; the bound says
  what is provable, not that every answer is optimal.
- **Fastest engine.** Measured against other libraries on identical hardware, that claim is
  false on latency, and no Packvium surface makes it. On the report's separate two-axis
  time-and-peak-memory frontier, `packvium-rust` is Pareto-optimal in 6 of 9 profiles and
  the only engine of ten never dominated. The latency result and the trade-off result are
  reported together; neither is a general speed-leadership claim.

## [0.1.3]

An additive release. No packing request/result field or solver algorithm changed.

### Added

- **First-party carrier connectors for UPS, FedEx, DHL Express and USPS**, and a live
  rate-card contract behind them. Connectors prepare ordinary rate-table data before a
  deterministic solve; no network call, clock or carrier module enters a packing engine.
  A rate card records which tariff version priced a quote and when it was fetched, and a
  card that has gone stale is refused rather than served.

### Fixed

- **`@packvium/browser` now works under Node**, not only in a browser. The WebAssembly
  loader generated for the `web` target fetches its own `.wasm` from a `file:` URL, which
  Node's `fetch` refuses — so server-side rendering, a Vitest node environment and
  `node --test` failed at initialization on 0.1.1 and 0.1.2. The package now selects a
  Node entry point that reads the module off disk. Browser behaviour is unchanged.
- **The PHP package's compatibility tree could not autoload a class on old PHP.** Its
  entry point was the one shipped file the downgrade did not process, and it used a PHP 8
  function to match the namespace prefix. Three of its solver files also could not be
  parsed by PHP 8.0 or 8.1, which that tree also serves. Both are fixed, and the tree is
  installed and loaded on 7.3, 7.4, 8.0 and 8.1 before release.
- **The compatibility tree did not place items where the canonical engine does on PHP 7.**
  Several solver comparators were partial and relied on `usort` being stable, which PHP
  guarantees only from 8.0, so three shared fixtures came back with a different rotation
  chosen. The tie-breaking is explicit now — identical results on every supported version —
  and no released package ever carried it, because none was installable below 8.2.

### Changed

- **`packvium/packvium` now requires `php: >=7.3` instead of `>=8.2`.** One package
  carries both the canonical PHP 8.2+ source and a generated `src-legacy/` tree, and its
  `autoload.php` selects by `PHP_VERSION_ID`; PHP parses only what it loads.
  `composer require packvium/packvium` is the right command on 7.3 through 8.5, and every
  one of those versions is held to the same committed placement results. Nothing changes
  for an 8.2+ consumer: the same files load, under the same names.
- Every package's `homepage` now points at <https://packvium.com>.

## [0.1.2]

An additive documentation and integration release. No packing request/result field or
solver algorithm changed.

### Added

- **Runnable examples and guided capability maps.** Python, PHP and Node now ship worked
  examples covering every objective and the major constraint, units, serialization,
  nested-packing and commerce paths. Their printed answers are retained and checked on
  every release, and each package executes its own examples in its test suite.
- **A versioned carrier-connector contract and reference implementation in the public
  workspace.** Connectors prepare ordinary rate-table data before a deterministic solve;
  no network call or carrier module enters a packing engine. The contract, offline replay
  harness, registry and synthetic carrier are application components rather than new
  packing-package API fields.

### Changed

- Every package README now links the whole Packvium family — Python, PHP, Rust, Node,
  browser, PHP FFI bridge and Python native selector — and the PyPI, npm, Packagist and
  crates.io manifests carry repository, homepage and keyword metadata. The PyPI page shows
  the same README as GitHub and states the real Python floor, 3.9.

### Fixed

- Connector responses are revalidated at runtime and bound to the registered carrier and
  requested service. Incomplete brackets, cross-currency price comparison and mutable
  replay/value-object state are refused instead of silently producing a wrong price.
- Linux x86_64 and ARM64 evidence follows the declared suite version, preventing a
  current gate from rewriting a previous release's receipt.
- PHP 7.4 artifact generation safely completes the locked downgrade tool's partial-write
  case and still fails closed if its single retry does not finish.

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
