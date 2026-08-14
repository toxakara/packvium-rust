# Changelog

The format follows [Keep a Changelog](https://keepachangelog.com/1.1.0/) and this project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html) — with the caveat
that the public API is not frozen until `1.0.0`. Pin an exact version.

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
