# Contributing

Thanks for looking. This library is small, dependency-light and deliberately strict about
exactness — please keep it that way.

## Getting set up

```bash
cargo test
cargo run --example basic
```

The suite should be green before you change anything, so that any later failure is yours.

## The rules that matter here

**No floats in geometry or feasibility.** Every length is an `i64` number of ticks
(1/16000 mm) and every weight an `i64` number of 1/8 µg. If a change makes a placement
decision depend on binary floating point, it is wrong even if the tests pass — see
[docs/UNITS-AND-NUMERICS.md](docs/UNITS-AND-NUMERICS.md). Intermediate products that can
exceed 64 bits (a volume in cubic ticks, for instance) go through `i128` with
`checked_mul`/`checked_add`, not a silent wraparound.

**`#![forbid(unsafe_code)]` stays.** It is a crate-level attribute, not a convention;
a change that needs `unsafe` belongs in a binding crate, not here.

**No runtime dependencies beyond `serde`.** `serde`, `serde_json` and `thiserror` are the
whole dependency tree. A new one needs a very good argument.

**Inputs are immutable.** A pack call must never mutate the items or containers it was
given. This is a correctness property people rely on when reusing request values.

**Determinism.** The same input and seed must produce the same output. Avoid iteration
over unordered collections where the order can reach a result; sort explicitly.

**Every fixed defect gets a regression test.** Not "a test somewhere" — a test that
fails before your fix and passes after.

## Behavioural changes

This package is one of three independent implementations of the same documented contract;
the Python and PHP ports must produce identical placements for the same request. That
means:

- A change to solver behaviour, the objective vector, serialization field names or status
  semantics is a **cross-language change**. Open an issue describing it before writing
  code, so the other implementations move with it.
- A change confined to Rust internals — types, refactoring, performance without a change
  in output — is local and needs no coordination.

If you are unsure which kind you have, run your change against the examples and see
whether any placement moves.

## Pull requests

- One logical change per pull request.
- Commit messages in imperative mood, under 72 characters:
  `type(scope): description` with `feat`, `fix`, `refactor`, `chore`, `docs` or `test`.
- Add or update tests. `cargo test` must pass, and `cargo clippy --all-targets -- -D
  warnings` must stay clean.
- Update the relevant document under `docs/` when you change behaviour. Complexity,
  limitations and failure modes are part of the change, not follow-up work.
- Do not bump the version; releases are cut separately.

## Reporting a bug

Please include the full request that reproduces it — items, containers and configuration —
and what you expected instead. A packing bug is nearly impossible to act on without the
exact input.

For anything with security implications, follow [SECURITY.md](SECURITY.md) rather than
opening a public issue.
