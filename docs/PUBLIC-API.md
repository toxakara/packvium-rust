# Public API contract

## Core inputs

- `Item`: id, dimensions, weight, quantity, rotations, upright/floor/stacking rules, top-load limit, support ratio, group, tags, metadata, and an optional `nesting_height` (how much this item sinks into an identical one beneath it when stacked). Only the same item type with the exact same footprint may nest; its adjacent predecessor is one full-footprint direct supporter for support ratio, ground-contact, stack/load and route rules, while non-adjacent same-column face coincidences are shadowed. An optional, exact non-negative integer `value` (no unit or currency — the caller's own economic scale) means nothing to placement or to any other objective; only the `maximum_value` objective reads it.
- **Item geometry beyond the box.** `shape_type` is an enum —
  `rigid_cuboid` (the default), `convex_hull`, `compressible` — accompanied by
  `hull_vertices`, `compression_ratio` and `max_compression_pressure_kpa`. **A request that
  omits `shape_type` is unaffected**, byte for byte: the default is `rigid_cuboid`, the whole
  existing golden corpus is unchanged, and writing `"shape_type": "rigid_cuboid"` explicitly
  is served rather than refused.

  The two other values are rolling out one engine at a time, which is what the fields were
  reserved before the freeze for, and the rollout is finished: **all four engines implement
  both**, and are held to byte-identical results on the shared fixtures. The one
  recorded difference is the JavaScript fallback on a scene whose answer depends on item
  ordering -- it makes a single ordering pass where the other three run a portfolio, so it can
  pack validly but less densely; that is pinned in `conformance/native-quality-budget.json`
  rather than left to drift.

  Where they are implemented: a `convex_hull` item's collisions are decided by an exact
  integer separating-axis test rather than by its box, and its occupied volume is the hull's
  own — two complementary wedges share a crate that fits one of their bounding boxes. A
  `compressible` item loses height linearly with the pressure resting on it, rounded up, and
  a load above `max_compression_pressure_kpa` is refused as `crush_violation` rather than
  packed. Three situations deliberately fall back to the bounding box, always over-reserving
  space: a configured `clearance`, an item on a route (`stop_index`), and the uniform-lattice
  fast path. Hull coordinates are non-negative offsets from the corner of the item's own
  bounding box and are capped at 10^8 length ticks -- 6.25 m, beyond ordinary parcel sizes, and the
  bound that keeps the exact geometry inside a 64-bit integer everywhere it can be.
  IRREGULAR-ITEMS.md is the model and states why.
- `Container`: id, inner/outer dimensions, tare/payload, cost, inventory quantity,
  obstacles (each a union of one or more exact boxes — `additional_boxes`
  approximates a non-rectangular zone such as a wheel arch or tapered roof without
  modelling slants directly; an omitted origin means `(0,0,0)`, overlapping
  components remain one union and never double-count free/occupied volume),
  tags, maximum item count, metadata, an optional `max_stack_density` floor-loading
  limit (weight per square metre of base area, checked at every level of the stack,
  not only the floor), and optional two-axle enforcement (`axles`: exactly
  `[front, rear]`, each a position and an optional `max_load`, exact two-point beam
  statics, rejected as a constraint rather than merely reported), and an
  optional `rate_table` carrier tariff (ascending `weight_brackets_g` paired with
  `prices_minor`, an optional `minimum_charge_minor` floor and an optional
  `fuel_surcharge_permille`). The rate card is request data rather than a registered
  scorer precisely so all four engines can be held to the same price; only the
  `lowest_landed_cost` objective reads it, and a request that omits it behaves exactly
  as it did before the field existed.
- `PackingConfig`: solver profile, deadline, alternatives, deterministic seed, clearance,
  support threshold, exact-search limit, beam/order budgets, the portable
  `max_candidate_points` search-memory bound, deterministic global-plan controls
  `container_plan_beam_width` / `container_plan_node_limit`, and an optional `effort_budget`
  that bounds all four stable implementations by counted work instead of wall-clock
  time. `solvers` is an ordered list: every named strategy is run, stable ties prefer
  the earlier name, and an unknown name is a structured error. The optional
  The stable solver names are `grid`, `extreme_points`, `homogeneous_blocks`, `layer`,
  `maximal_spaces` and `exact_small`. `homogeneous_blocks` is the unrestricted
  solid-lattice quality strategy and safely delegates to `extreme_points` when a scene
  contains placement-distinguishing rules. In the Rust core, an explicit single
  `grid` selection likewise reports `grid:fallback` and delegates to its deterministic
  constraint-aware order packer when the closed-form lattice is inapplicable (for
  example, for multiple declared item types or a custom hard constraint); it does not
  silently return an empty lattice result. The optional `shipping_cost` objective accepts exact dimensional-weight divisor/unit inputs and
  ranks billable weight from the shipped container's outer dimensions. These fields
  have identical semantics in all four implementations — see
  ALGORITHMS-AND-COMPLEXITY.md.
  `require_placement_coordinates` (Python/PHP/Rust/JavaScript, default `true`) lets a caller
  trade per-item coordinates for speed and memory on a bulk single-item-type order:
  when `false` and `GridSolver`'s regular-lattice preconditions already hold (see the
  Grid solver section below), the result's `placements` for that container stays
  empty and a `lattice_summary` carries the same information in `O(1)`/`O(r)`
  instead of one entry per instance. Default `true` reproduces every existing result
  byte-for-byte; this is a strict, opt-in addition, not a behaviour change.
  The JavaScript fallback uses the same compact wire form and an `O(c*r)` grid path
  (`c` emitted containers, `r <= 6` rotations), so large identical quantities do not
  create per-item JavaScript objects either. Requests outside the regular-lattice
  preconditions keep using the general solver and may still return coordinates.
- `catalog_versions_used`: optional immutable catalog references resolved before
  packing. Each reference has exactly `catalog_id`, positive `version`,
  non-negative `published_at_epoch_ms`, and `content_digest`; catalog IDs are unique.

Production packing uses a monotonic system clock. Test harnesses and deterministic
simulations may inject a monotonic clock into Python `Packer(..., clock=...)`, PHP
`Packer(..., clock: ...)`, Rust `Deadline::with_clock(...)` with
`SolverRegistry::solve_with_deadline(...)`, or the JavaScript fallback's optional clock
argument. The callable returns an implementation-native monotonic value; it must never
move backwards. This is a testing seam, not a serialized request field.

## Core output

- three independent result facts:
  - `feasibility.code`: `feasible`, `infeasible`, or `unknown`;
  - `termination.code`: `complete`, `time_limit`, `effort_limit`, or `error`;
  - `optimality.code`: `proven_optimal`, `proven_infeasible`, `best_found`, or `not_proven`.
    The 1.1.0 freeze reserved and typed `gap_key`, `absolute_gap` and `relative_gap`
    alongside it. The pre-1.1.0 extension point remains open: closing the object would
    make a previously valid producer fail a 1.1.0 validator, which is a breaking change
    and therefore not legal in this minor release. No engine emits the reserved fields
    yet, so `code` is still the only key any Packvium result carries. See
    OPTIMALITY-CERTIFICATES.md;
- legacy `status`: `optimal`, `feasible`, `best_found`, `time_limit`, `infeasible`, or `invalid_result`;
- packed containers with exact coordinates, dimensions, rotations, and a
  `centre_of_mass_offset_ppm` — the weighted centre of mass's exact-integer Chebyshev
  offset from the container's own centre, needed for axle load and side-to-side
  balance;
- optional exact `axle_reactions` when the request declares two axles. The basis is
  always `gross`: payload acts at each item's physical longitudinal centre and tare
  acts at the container's geometric centre. `front_numerator` and `rear_numerator`
  share the positive `denominator`; signed numerators intentionally expose a centre
  of mass outside the axle span instead of clipping it;
- unpacked instances with reason codes, proof levels and rejection observations;
- objective score — a five-key lexicographic vector of exact integers, identical across every implementation (OBJECTIVE.md) — and solver statistics;
- the exact `catalog_versions_used` lineage from the request (an empty array when no
  catalog was involved), preserved by Python, PHP, Rust and JavaScript;
- warnings and top-K alternatives.

### `alternatives`, and what it is not

Each entry in `alternatives[]` is a **complete result in its own right**, with its own score
vector, containers and unpacked items — the schema says `$ref: "#"` and means it.

**`configuration.alternatives` counts the winner.** It is the size of the ranked set the
portfolio keeps, so the list of runners-up holds at most `alternatives - 1` entries. The
schema's own minimum, `alternatives: 1`, therefore returns an **empty** list. Ask for `2` to
receive one.

**The list is per-engine and is not part of the cross-language contract.** Implementations
are not required to explore the same number of portfolio starts — under this engine's own
profiles a request may run one start, nine, or twenty-two — so PHP, Python, Rust and
JavaScript will not generally return the same runners-up, the same number of them, or the
same scores. Do not diff `alternatives` across implementations. Three properties *are*
required of every implementation:

1. each entry is a complete result that satisfies the result schema on its own;
2. no alternative's score is lexicographically better than the winner's — if one were, it
   would be the winner;
3. the list holds at most `configuration.alternatives - 1` entries.

**An empty list is normal, and it does not mean "nothing else was possible".** Read
`termination.starts`, which names every portfolio start, whether it completed, and which was
selected. Four situations produce an empty list, and only the last is about the difficulty of
the request:

| Situation | What `termination.starts` shows |
| --- | --- |
| `solver_profile: fast` runs a single solver | one start — this profile can never rank a runner-up |
| `alternatives: 1` | any number of starts, and a cap of zero runners-up |
| only one start finished within the budget | several starts, `completed: true` on one |
| the grid lattice packed everything, so the portfolio stopped | one start, `grid:volume`, selected |

If you want alternatives, ask for a profile that explores (`balanced` or `quality`) and a
count of at least `2`.

### Solver metrics

Every result, including each alternative, contains an `algorithm.metrics` object:

| Counter | Meaning |
|---|---|
| `candidate_points_considered` | Spatial origins visited before expanding rotations. |
| `orientations_considered` | Point/orientation placement attempts. |
| `feasible_candidates` | Candidates that passed geometry and hard constraints. |
| `collision_checks` | AABB intersection predicates evaluated during search. |
| `support_checks` | Candidate support-rule evaluations. |
| `space_partitions` | Free-space regions processed while carving occupied volume. |
| `search_nodes_expanded` | Beam, DFS, item or lattice search nodes expanded. |

All counters are non-negative integers. A zero means the selected solver did not use
that phase; it does not mean the metric is unsupported. Compatibility fields
`algorithm.placements_attempted` and `algorithm.candidates_evaluated` remain and must
equal `metrics.orientations_considered` and `metrics.feasible_candidates`.
Metrics are diagnostics: they never participate in feasibility, ranking or the
objective vector.

`algorithm.time_limit_reached` and `algorithm.effort_limit_reached` identify distinct,
mutually exclusive causes. The former is only a wall-clock deadline. The latter means
one of the request's counted-work boundaries was reached. An unpacked `time_limit` or
`effort_limit` reason must match the corresponding flag.

## Loading and unloading sequences

Python, PHP, Rust and JavaScript expose distinct loading and unloading dependency
graphs plus deterministic safe-order and replay APIs. Loading begins with an empty
container and requires supporters before an item; unloading begins with the final
scene and requires children to leave first. Both check an axis-aligned insertion/exit
sweep in one of `+x`, `-x`, `+y`, `-y`, `+z`, `-z`; unknown directions are errors.
Evidence-bearing variants return the selected direction and structural dependencies
for every step. Replay independently checks the order, containment, collisions,
support dependencies and accessibility rather than trusting the generator.

### Sequence step and error DTOs

A step from an evidence-bearing generator (`safe_loading_order_with_evidence`,
`safeLoadingOrderWithEvidence`, `safe_loading_order_with_evidence`, and their removal
counterparts) serializes to the same shape in every implementation
(`SequenceStep.to_dict()` in Python, `SequenceStep::toArray()` in PHP,
`SequenceStep::to_json()` in Rust, and the plain object JavaScript's fallback already
returns):

```json
{"index": 1, "direction": "+x", "depends_on": [0]}
```

`depends_on` is a sorted list of placement indices, never an unordered set or map, so
the JSON is stable across languages regardless of each one's native container.

`SequenceError` (no safe order exists), `SequenceReplayError` (a supplied order is
infeasible at a given step) and `InvalidDirectionError` (an unrecognised direction
string) each carry an open, forward-compatible string `code` -- the same pattern as
the result `code` facts above -- alongside their own fields, and serialize the same
way in every language (`to_dict()` / `toArray()` / `to_json()` / the JavaScript error's
own `code` property):

```json
{"code": "sequence_stuck", "stuck": [0, 2]}
{"code": "sequence_replay", "index": 3, "step": 1, "reason": "..."}
{"code": "invalid_direction", "direction": "sideways"}
```

`conformance/scene/sequence-fixtures.json` pins this shape across all four languages;
see `docs/SERIALIZATION.md`.

Sequence diagnostics use a separate canonical `SequenceWarning` DTO instead of
putting caller-facing prose into an untyped string. Its four fields are stable in
Python, PHP, Rust and JavaScript:

```json
{
  "code": "route_deferred",
  "index": 2,
  "message_key": "packvium.sequence.route_deferred",
  "arguments": {"required_stop": "1", "stop": "3"}
}
```

`arguments` is a string-to-string map serialized in lexicographic key order. The
`code` and `message_key` are machine/localization contracts; rendered text is an
application concern. Constructors copy and sort the arguments so caller mutation or
hash iteration order cannot change canonical JSON.

Use the composed safe-loading API when executing a plan. It computes the geometry
order and then validates every loading prefix against stackability, top load,
transitive stack-count, ground-contact and stack-density rules. The lower-level graph
and business-rule functions remain available for diagnostics, but a caller does not
need to remember to compose them manually.

### Per-placement reachability

All four implementations expose per-placement reachability:
`placement_reachability` (Python),
`UnloadingDependencyGraph::placementReachability` (PHP),
`placement_reachability` (Rust), and `placementReachability` (JavaScript). They answer
a snapshot question distinct from the safe-order APIs above — "if the container
opened right now, what could actually come out first" — rather than searching for a
complete order for the whole load. A geometrically valid packing (it passes every
placement, collision and support check) can still box an item in on every exposed
side, or place it behind cargo due at an earlier route stop; this reports exactly
that per placement instead of only the whole-load yes/no
`safe_route_removal_order` gives.

One `Reachability` entry per placement, in input order, each carrying:

| Field | Meaning |
|---|---|
| `reachable` | Whether this placement could be removed right now. |
| `blocked_by_support` | Still-present children (`ContactGraph`) resting on top of it. |
| `blocked_by_neighbors` | Present placements blocking every allowed exit sweep — empty whenever at least one allowed direction is clear. |
| `blocked_by_route` | Present placements due at an earlier stop — empty unless `stops` is supplied. |

`reachable` is true only when all three are empty and at least one allowed direction
has a clear sweep. `stops` is optional, mirrors `safe_route_removal_order`'s own
argument exactly, and defaults to leaving `blocked_by_route` empty for every
placement — a single-stop caller that never populates it sees no behavior change.
This is a pure function over the finished scene, not a field the solver populates
during placement (contrast `support_ratio`/`top_load` below): it reuses the same
`UnloadingDependencyGraph` and accessibility sweep the safe-order APIs are built
from, so there is no second, possibly-disagreeing notion of "blocked" to maintain.
Each implementation exposes a canonical serializer (`to_dict`, `toArray`, `to_json`
or the JavaScript plain object); the shared fixture asserts byte-equivalent fields and
ordering in every port. Supplying a `stops` list whose length differs from the
placement list is an admission error rather than a partially applied route policy.

## Post-pack weight rebalancing

Python (`rebalance_weight`), PHP (`WeightRebalancer::rebalance`), Rust
(`rebalance_weight`) and JavaScript (`rebalanceWeight`) expose the same explicit,
opt-in operation for reducing payload spread across containers that are already
packed. It is not run implicitly by `pack`.

Every relocation is atomic: the item remains in the source result until a complete
candidate scene has been constructed and independently validated. A candidate that
would lose or duplicate an item, split a group, exceed a destination rule, or strand
a supported item in the source is discarded without changing the working result.
The returned move list records `item_id`, `from_container_id` and `to_container_id`;
an empty list means no safe strict improvement was found. The shared fixture
`conformance/scene/rebalance-fixtures.json` pins accounting and move semantics across
all four implementations.

The Rust implementation is also available through every native boundary, all backed
by the same `packvium_core::rebalance_json` adapter:

- C ABI: `packvium_rebalance_json(request_json, result_json, max_moves)`;
- Python/PyO3 and WebAssembly: `rebalance_json(request, result, max_moves)`;
- N-API: `rebalanceJson(request, result, maxMoves)`.

The Node package's public `rebalanceWeight` automatically uses that N-API operation
when the addon is healthy and otherwise runs the deterministic JavaScript
implementation. Boundary adapters exchange canonical JSON and do not duplicate the
rebalancing algorithm.

## Human-readable explanations

Every stable implementation converts a structured reason/proof object into both a
deterministic English message and a localization descriptor:
`message_key`, ordered string `arguments`, and `default_message`. Unknown reason codes
produce a structured error. Applications can therefore localize without parsing logs
or extracting variables from rendered English.

## Status semantics

Feasibility, termination and optimality answer different questions and must not be
collapsed. For example, a run can place every item and still hit its quality-search
deadline:

```json
{
  "status": "feasible",
  "feasibility": {"code": "feasible"},
  "termination": {"code": "time_limit"},
  "optimality": {"code": "not_proven"}
}
```

Each fact is an open object with a required string `code`. Consumers must preserve an
unknown code and any additional fields, allowing future effort/node/cancellation limits
without turning a minor release into a parsing break. `status` remains for one
compatibility cycle and is checked against the three facts. The alpha release emits no
`proven_optimal` claim without a future proof/certificate.

## Portfolio termination

`termination` describes the whole solver portfolio instead of copying one ambiguous
boolean from the selected result:

```json
{
  "code": "complete",
  "any_start_truncated": true,
  "all_required_starts_completed": false,
  "winning_start_truncated": false,
  "global_deadline_reached": false,
  "starts": [
    {
      "id": "extreme_points:volume",
      "started": true,
      "completed": true,
      "truncated": false,
      "selected": true,
      "global_deadline_reached": false
    },
    {
      "id": "maximal_spaces:volume",
      "started": true,
      "completed": false,
      "truncated": true,
      "selected": false,
      "global_deadline_reached": false
    }
  ]
}
```

The fields answer separate questions:

| Field | Meaning |
| --- | --- |
| `any_start_truncated` | At least one started solver was cut off. |
| `all_required_starts_completed` | Every planned start completed; false also covers starts not launched before the global deadline. |
| `winning_start_truncated` | The selected packing itself came from an interrupted start. |
| `global_deadline_reached` | The portfolio-wide deadline expired, independently of individual start slices. |

The aggregate fields are derived only from `starts`. Exactly one record is selected.
`completed` and `truncated` are mutually exclusive and both require `started`.
`termination.code` is `time_limit` when the selected start or portfolio hit the wall
clock, and `effort_limit` when the selected start stopped at a counted-work boundary.
A truncated loser alone leaves the selected answer's code `complete`, while still
making `all_required_starts_completed` false. The two algorithm limit flags preserve
the exact cause.
`winning_start_truncated || global_deadline_reached`.

## Rejection proof

Every entry in `unpacked_items` carries the same evidence shape in Python, PHP, Rust
and JavaScript:

```json
{
  "item_id": "oversized#1",
  "item_type": "oversized",
  "reason": "no_compatible_container_dimensions",
  "details": [],
  "proof": {
    "level": "proven",
    "observations": [
      {
        "code": "no_compatible_container_dimensions",
        "count": 1,
        "details": []
      }
    ]
  }
}
```

The four levels have deliberately different meanings:

| Level | Meaning |
| --- | --- |
| `proven` | A request-level bound proves rejection without relying on search completeness. Currently `no_compatible_container_dimensions`, `payload_exceeded`, `rotation_restricted` and `no_eligible_container` — each a pure geometry or tag check across every container, none dependent on how far the search got. |
| `observed` | The engine exhausted or rejected the candidates it actually examined; this is not a global impossibility proof. |
| `inferred` | The reason follows from solver state or a compound rule but has no complete certificate. |
| `unknown_due_to_limit` | A deadline stopped the search, so no negative conclusion is claimed. |

`observations` is non-empty and open for more detailed rejection codes and counters.
At minimum it contains the emitted reason, its occurrence count and the associated
details. A structural bound takes precedence over a deadline: an oversized item remains
`proven` even if the overall run timed out. Conversely, `time_limit`, `search_exhausted`
and other unfinished-search outcomes can never carry `proven`.

## Commercial and control-plane API

Three deterministic functions over one canonical JSON document -- a carrier quote, an
eligibility decision, and catalog version metadata. They are a separate surface from the
packing API and add no packing-request or packing-result field; the full contract, with
the document format, every result shape, the closed set of rejection codes, complexity
and limitations, is [COMMERCE-API.md](COMMERCE-API.md).

| Language | Entry point |
| --- | --- |
| Python | `packvium.commerce.quote(document, request)`, `.evaluate_policy(...)`, `.catalog_version_info(...)`, `.canonical_json(result)` |
| PHP | `Packvium\Commerce\quote(array $document, array $request)`, `evaluatePolicy(...)`, `catalogVersionInfo(...)`, `canonicalJson(...)` |
| Rust | `packvium_core::commerce::quote_json(&str)`, `evaluate_policy_json(&str)`, `catalog_version_info_json(&str)` |
| JavaScript | `commerce.quote(document, request)`, `.evaluatePolicy(...)`, `.catalogVersionInfo(...)` on `@packvium/engine`; the same three, async, on `@packvium/browser` |
| C ABI | `packvium_commerce_quote(call)`, `packvium_commerce_evaluate_policy(call)`, `packvium_commerce_catalog_version_info(call)` |

The Rust, C ABI and WASM entry points take one JSON string, `{"document": ..., "request":
...}`, and return the result document as a string. Each C ABI function follows the same
pointer contract as `packvium_solve_json`: a valid, immutable, NUL-terminated UTF-8
input, and an owned result string the caller releases exactly once with
`packvium_free_string`. `@packvium/engine` selects the native addon when it is installed
and the deterministic JavaScript implementation otherwise, the same way `pack` does;
`commerce.backend()` reports which answered.

Two kinds of failure, and they are not interchangeable. A malformed document or request
is a caller bug and is raised the way each language raises one (`CommerceInputError`,
`Packvium\Commerce\CommerceInputException`, `Err(CommerceInputError)`, a thrown
`CommerceInputError`). A well-formed request the commercial model cannot answer -- no
tariff effective at that instant, no rate for that zone -- is a successful call returning
`"status": "rejected"` with a code from a closed set, exactly as an infeasible packing
request returns a result with a status rather than raising.

## Execution plan

A document derived from an already validated packing result: what to do first, why this
carton, and what was not packed. It is a view, not a decision — it calls no solver and no
validator, and a test in each language asserts that. The contract is
EXECUTION-PLAN.md.

| Language | Entry point |
| --- | --- |
| Python | `packvium.execution.build_execution_plan(request, result, loading_orders=...)`, `.canonical_plan_json(plan)` |
| PHP | `Packvium\Execution\Plan::build(array $request, array $result, array $loadingOrders = [])`, `::canonicalJson(array $plan)` |
| Rust | `packvium_core::execution::build_plan_json(result_json, loading_orders_json)` |
| JavaScript | `buildExecutionPlan(request, result, {loadingOrders})`, `canonicalPlanJson(plan)` from `@packvium/engine`'s `execution.js` |

All four are held to **byte-identical** output on the canonical form, over the whole golden
corpus. That is stricter than the packing contract, which allows two engines to place items
differently and compares an objective floor: a plan is derived from a result, so there is
nothing left to differ about. The plan's own JSON Schema is closed — an extra key in one
implementation is a divergence, not a nicety.

**The step order is injected.** The engines compute a safe loading order from geometry the
adapter never sees, so `loading_orders` is optional: supply it and each step is numbered,
omit it and the container reports `order: "unavailable"` with every placement still listed.
There is no third behaviour, because falling back to the order placements appear in would
present an artifact of how the solver walked its candidates as an order safe to lift boxes
in.

**Facts and presentation are separate in the output.** Anything the solver or validator
decided is under `facts`; every human-readable sentence is under `presentation` and names
the authoritative fields it was built from in `cites`. A downstream system that reads only
`facts` loses nothing it is entitled to rely on, and an `unpacked_items[].proof.level` of
`observed` stays `observed` in both.

**Operator locks re-solve; they never edit the plan.** An operator who pins a placement
gets a *new* result beside the approved one rather than an edit of it, so "what was
approved" and "what was proposed" stay two artifacts instead of two states of one.

| Language | Entry point |
| --- | --- |
| Python | `packvium.locks.resolve_with_locks(request, locks)`, `.locks_from_plan(plan, ...)`, `.lock_registry(locks)` |
| PHP, Rust, JavaScript | **not exported** |

Each lock becomes an ordinary placement constraint, so the re-solve is the same portfolio
under the same independent validator that any other request gets. There is no lock-aware
solver and no relaxed validation: **a lock cannot produce a placement the engine would
otherwise refuse**, and the strongest thing it can do is reserve its own slot. A lock the
solve cannot honour comes back on `LockedResolve` as unpreserved and names the lock — it is
an outcome, not an exception — while a lock set that contradicts itself, by overlapping or
by naming an item the request does not contain, is refused with `LockSetError` before any
solve runs.

Python only, and deliberately so: a lock has no representation in the request schema, since
that field is held for the next contract freeze, so there is nothing to hand another engine.

## Scenario and recommendation API

Scenario what-if comparison and catalog/rule recommendation publication, exported as
`packvium.simulation` and `packvium.recommendations`. The full contract is
INTELLIGENCE-API.md; what belongs here is the surface and its two
limits, both of which are part of the API rather than notes about it.

| Language | Entry point |
| --- | --- |
| Python | `packvium.simulation.run_scenario(...)`, `.compare_scenarios(...)`; `packvium.recommendations.propose_recommendation(...)`, `.approve_catalog(...)`, `.approve_policy(...)`; `packvium.holdout.evaluate_on_history(...)` over a `packvium.outcomes.OutcomeLedger` |
| PHP, Rust, JavaScript | **not exported** |

**Python only, and held to no cross-language conformance.** Every other contract in this
document is proved by driving four engines as subprocesses over JSON. `run_scenario` takes
an `OrderEvaluator` *callback* as its central argument, and a callback does not cross a
pipe -- so the harness that proves the packing and commerce contracts cannot express a
scenario at all. Whether the data-in/data-out half ever crosses runtimes is an open
question, not an omission; INTELLIGENCE-API.md scopes it.

**Historical replay ships with the ledger it reads.** `evaluate_on_history` scores a
recommendation against decisions held out by an explicit `split_at`, and could not be
exported until `packvium.outcomes` was, because a function whose central argument a caller
cannot construct is worse than an unexported one. Two rules are enforced as control flow
rather than documented: a packing the injected validator rejects is that arm's failure at
any cost and its metrics never reach the comparison, and the validator is handed the
request/result pair with no score in it. Only the deterministically recomputable part of an
outcome is scored -- realised damage, returns, repacks and operator overrides happened
under the carton that actually shipped, so they are reported against the baseline and never
credited to the treatment.

`compare_scenarios` returns a Pareto report per order and never a blended delta, and
`propose_recommendation` returns `None` rather than a low-confidence proposal when the
paired cohort is too small. Neither writes to a registry: publishing goes only through
`approve_catalog` / `approve_policy`, and a proposal is handed no registry to write to.

**Supported, and not yet frozen.** This surface and `packvium.locks` are reached by
importing the submodule, which puts them outside the frozen API snapshot the packing and
commerce surfaces are held to. They work, they are tested, and they will not be withdrawn
inside 1.x — but a parameter may be renamed or a returned value gain a field in a minor
release, announced in the changelog rather than blocked by a gate. Pin the version if you
depend on the exact shape. COMPATIBILITY.md states why this one surface
is held that way and what would change it.

## JSON API

Python, PHP, Rust and the JavaScript fallback accept the same top-level keys: `units`,
`configuration`, `items`, `containers`, `catalog_versions_used`, and `output`.
