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
  any platform and in any of the implementations;
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
- **Support for non-rectangular shapes.** Items and containers are rigid axis-aligned
  cuboids; obstacles are rectangular. No cylinders, deformable bags, meshes or
  irregular geometry.
- **A result before the deadline.** With a time limit set, the search may return
  `time_limit` with a partial packing, or nothing placed at all.
- **Full load propagation.** Top-load limits are enforced against what rests *directly*
  on an item, not against the accumulated mass of an entire stack above it.

## Not modelled at all

Deformable goods, dynamic shipping physics, contact-load propagation through arbitrary
support graphs, pallet-overhang standards, axle loads, unloading-route optimization, and
carrier tariff or rating catalogs. None of these are built in, and none are approximated
silently — if you need them, they belong in your own layer above this library.

## Status of this release

Version `0.1.3` is an early release. The public API is not yet frozen: field names,
status codes and the objective vector may change before `1.0.0`. Pin an exact version.

The algorithm complexities documented in `ALGORITHMS-AND-COMPLEXITY.md` are design
intent that has not yet been confirmed by profiling. Treat them as guidance for choosing
a solver profile, not as a performance contract.
