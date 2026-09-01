# Security policy

## Supported versions

Only the latest `1.x` release receives fixes. The `0.1.x` line is superseded by `1.0.0`
and receives none. There is no long-term support branch for older majors.

## Reporting a vulnerability

Please report privately, not in a public issue. Use GitHub's **Report a vulnerability**
button under the Security tab, which opens a private advisory.

Include the version, the platform, a minimal request that reproduces the problem, and
what you observed. A crash, a hang or a wildly wrong result on a small input is worth
reporting even if you are unsure it is a security matter.

You can expect an acknowledgement within a few working days and an assessment after that.
Please give us a chance to ship a fix before disclosing publicly.

## Threat model

This is a computation library. It has **no runtime dependencies**, opens no network
connections, spawns no processes, reads no files except through the CLI's standard input,
and executes no user-supplied code except the extension objects you register yourself.

The realistic risks are therefore about untrusted **input**:

- **Resource exhaustion.** A request with a large item count, many container types or a
  permissive solver profile can consume substantial CPU and memory. If you accept
  requests from untrusted callers, set `time_limit_ms`, cap item quantities and container
  inventory, and run the call where you can bound memory. The deadline is honoured by the
  search, but a single enormous request can still allocate a lot before the first check.
- **Integer magnitude.** Dimensions are converted to exact integer ticks — 16 000 per
  millimetre. Absurd inputs produce very large integers rather than overflow, but they
  cost time and memory. Validate dimensions against a sane maximum before passing them in.
- **Malformed input.** Bad units, unparseable numbers and contradictory constraints raise
  errors rather than producing a wrong packing. Do not suppress those errors.

A result that reports success has passed independent validation, but validation checks
the constraints you declared. It cannot know about a constraint you did not express.

## Extensions

Custom constraints, orderings, scorers, container selectors and solvers run with the
privileges of your process. Treat a third-party extension as you would any other
dependency: read it before you register it.

## Signing and keyless publishing

Every release is built and published from CI, never from a maintainer's workstation,
so there is one attacker-controlled surface to defend: the release workflow itself.

- **Signed tags.** The tag a release is cut from is a signed git tag (`git tag -s`),
  verifiable against the maintainers' published keys. An unsigned tag is not released
  from.
- **OIDC trusted publishing instead of long-lived tokens, where the registry supports
  it.** PyPI and npm both accept a short-lived token minted from the release workflow's
  GitHub Actions OIDC identity instead of a static API token stored as a secret — there
  is no password to leak because none is issued until the moment of publish, and it is
  scoped to that one run. Packagist resolves packages directly from the tagged git
  repository and has no comparable upload token to eliminate. crates.io does not yet
  support trusted publishing for this ecosystem; until it does, its token is scoped to
  this crate only, stored as a CI secret, and rotated on the schedule below regardless
  of whether compromise is suspected.
- **Provenance attestations** (see the release process)
  bind each published artifact back to the exact workflow run, commit and tag that
  produced it, independent of which upload method the registry used.
- **Recovery.** If the release workflow, its OIDC trust configuration or the crates.io
  token is suspected compromised: revoke the trust relationship (or rotate the token)
  immediately, audit the workflow's recent runs and any artifacts they published,
  and re-cut the release from a clean, re-reviewed commit under a new version — never
  by force-pushing or reusing the affected tag.
- **Revocation.** A compromised or defective published version is pulled from
  circulation using each registry's own mechanism (PyPI yank, npm deprecate, Packagist
  abandon, crates.io yank) rather than deleted outright, so that projects already
  pinned to it get a clear signal instead of a broken install. The security advisory
  for the issue names the affected versions explicitly.
