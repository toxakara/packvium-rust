# Guarantees

What this library promises, and what it does not. Read this before relying on a result.

## It guarantees

After a result reports `feasible` or `optimal` and passes independent validation:

- **no overlaps** — no two placed items intersect, clearance included;
- **containment** — every placement lies inside the container's usable dimensions;
- **allowed rotations only** — every orientation is one the item permits;
- **declared hard constraints hold** — weight limits, floor-only, non-stackable, direct
  top-load limits, minimum support ratio, compatibility rules and obstacles;
- **exact arithmetic** — all geometry is integer, in units of 1/16000 mm for length and
  1/8 µg for weight. No coordinate is ever a binary floating-point number, so no
  placement decision depends on rounding;
- **determinism** — identical input and identical seed produce an identical result, on
  any platform, when the search completes or stops on `configuration.effort_budget`. A
  search stopped by `time_limit_ms` stops where the host's speed let it, so it may differ
  between runs; the result says which limit stopped it;
- **complete accounting** — every requested item instance appears exactly once, either
  placed or listed as unpacked with a reason code.

## It does not guarantee

- **A globally optimal packing.** The `fast`, `balanced` and `quality` profiles are
  heuristic. Only `exact_small`, within its item limit, searches exhaustively — and even
  then optimality is not certified in the result. A returned solution is a good solution,
  not a proven best one.
- **Physical safety in transit.** Nothing here models vibration, shock, acceleration,
  tipping, strapping or load shifting. A geometrically valid packing can still be an
  unsafe shipment.
- **That your inputs are right.** Dimensions, weights and clearances are taken as given.
  Round item exteriors outward, container interiors inward, and clearance outward before
  you build the request.
- **Arbitrary shapes.** Containers are axis-aligned cuboids. An item is a cuboid, a convex
  hull of vertices, or a cuboid that compresses by a declared ratio; obstacles are cuboids
  or unions of them. Concave meshes, curved surfaces and free-form deformable bags are not
  modelled — approximate them with a convex hull or a bounding cuboid.
- **A result before the deadline.** With a time limit set, the search may return
  `time_limit` with a partial packing, or nothing placed at all.
- **Full load propagation.** Top-load limits are enforced against what rests *directly*
  on an item, not against the accumulated mass of an entire stack above it.

## Not modelled at all

Dynamic shipping physics, contact-load propagation through arbitrary support graphs,
pallet-overhang standards, and route optimization. Axle loads, the unloading order of a
given stop sequence (`stop_index`, `access_directions`) and carrier rate tables are
modelled; choosing the route itself is not. None of the unmodelled items is approximated
silently — if you need them, they belong in your own layer above this library.

## Status of this release

Version `1.5.0` freezes the public API. Field names, status codes, the objective
vector, the numeric policy and the validation rules do not change without a major
version, so any `1.x` is a safe upgrade from any earlier `1.x`. A caret or tilde
constraint on `1.0` is enough; an exact pin is no longer required.

What the freeze does not cover: which of several equally valid packings a solver
returns. That is bounded by the objective vector, not by the placement list, and a minor
release may return a different arrangement with the same or a better score.

The algorithm complexities documented in `ALGORITHMS-AND-COMPLEXITY.md` are asymptotic
design bounds held by review, not per-release measurements. They are not a wall-clock
performance contract either: constants, input shape and host all move the real number.
Use them to choose a solver profile, not to predict a duration.
