# Commercial and control-plane API

The packing engines and the commercial layer around them — carrier rating,
eligibility/policy evaluation and catalog versioning — are public APIs. This document
defines the latter contract: three exported
functions, one canonical input document, one canonical result shape and one closed set
of rejection codes, identical in all four languages.

**No packing-request or packing-result schema field is added or changed by this API.**
Both existing wire schemas are untouched. `container.rate_table`
and `policy` are already public wire fields; what was missing was the
catalog/versioning layer *around* them and a callable entry point, not a field.

## Design rules

1. **One implementation per language, never two.** Python's export is the workspace
   modules themselves, relocated into the installable package with the workspace paths
   kept alive as re-export shims (see [Traceability](#traceability)). PHP, Rust and
   JavaScript are independent implementations of this contract, held to the same
   cross-language standard as every other capability in this project (see
   [Conformance standard](#conformance-standard)).
2. **Data in, data out.** Every function takes plain JSON-shaped data and returns
   plain JSON-shaped data. No registry object crosses the API boundary, so the same
   fixture can drive all four languages over a subprocess boundary — the only way the
   conformance harness can check that they agree.
3. **Deterministic, no clock.** Nothing reads wall-clock time. Every "which version
   applies" question is answered from an explicit `version` pin or an explicit `as_of`
   value supplied by the caller, so a stored result replays byte-for-byte.
4. **Exact integers only.** Ticks for length, grams for weight, minor currency units
   for money, permille for percentage-shaped rates. Every inexact division rounds up.
   No floats appear anywhere in the input, the arithmetic or the output.
5. **One ordering: by Unicode code point.** Every sorted list in a result — the catalog
   id lists, the accessorial ids in an `unavailable_accessorial` rejection — and every
   deterministic tie-break on an id is ordered by code point, never by a locale
   collation and never by UTF-16 code unit. The distinction is not academic: by code
   unit, an emoji sorts *before* a fullwidth Latin A, and by code point it sorts after.
   `conformance/commerce/fixtures/catalog-unicode-id-ordering.json` and
   `policy-astral-rule-id-tie-break.json` hold every implementation to this.
6. **Structured rejections, not silent zeros.** A zone with no rate, an accessorial the
   tariff does not offer, a catalog version that does not exist — each is a named
   rejection code with structured fields, never an empty or zero-valued success.

## Exported functions

| Function | Python | PHP | Rust | JavaScript |
| --- | --- | --- | --- | --- |
| Quote | `packvium.commerce.quote(document, request)` | `Packvium\Commerce\quote(array $document, array $request)` | `packvium_core::commerce::quote_json(&str)` | `import { commerce } from '@packvium/engine'; commerce.quote(document, request)` |
| Policy | `packvium.commerce.evaluate_policy(document, request)` | `Packvium\Commerce\evaluatePolicy(...)` | `packvium_core::commerce::evaluate_policy_json(&str)` | `commerce.evaluatePolicy(document, request)` |
| Catalog | `packvium.commerce.catalog_version_info(document, request)` | `Packvium\Commerce\catalogVersionInfo(...)` | `packvium_core::commerce::catalog_version_info_json(&str)` | `commerce.catalogVersionInfo(document, request)` |

The original API proposal sketches these as `quote(request, catalog_version,
policy_version)`, `evaluate_policy(request, policy_version)` and
`catalog_version_info(version)`. The version pins are carried *inside* the request
object rather than as positional arguments, for one reason: a pin is only meaningful
against the history it indexes into, so the history (`document`) and the pin
(`request.tariff_version` / `request.rule_versions` / `request.version`) must arrive
together or a caller can pin version 3 of a document that has two. The information
content is identical; the shape makes the invalid combination unrepresentable as two
independent arguments.

The C ABI adds `packvium_commerce_quote`, `packvium_commerce_evaluate_policy` and
`packvium_commerce_catalog_version_info`, each `const char* -> char*` over the same
JSON, freed with the existing `packvium_free_string`. See
[PUBLIC-API.md](PUBLIC-API.md).

## The commerce document

One object holding the three append-only histories. Every history is a list of
versions in publication order; **a version's number is its 1-based position in that
list**, exactly as `CarrierRegistry.publish`, `PolicyRegistry.publish` and
`CatalogRegistry.publish` already number them. A document therefore cannot express a
history with a hole or a duplicated version number.

```json
{
  "tariffs": [
    {
      "carrier_id": "acme",
      "service_id": "ground",
      "versions": [
        {
          "effective_at": 0,
          "dimensional_weight_divisor": 5000,
          "cost_per_dimensional_kg_minor": {"zone-a": 450, "zone-b": 610},
          "minimum_charge_minor": 900,
          "fuel_surcharge_permille": 120,
          "accessorials": [
            {"accessorial_id": "liftgate", "flat_charge_minor": 250},
            {"accessorial_id": "residential", "permille_of_base": 75}
          ]
        }
      ]
    }
  ],
  "policy_rules": [
    {
      "rule_id": "no-hazmat-air",
      "versions": [
        {
          "scope": "hazmat",
          "action": "reject",
          "priority": 10,
          "effective_at": 0,
          "reason": "class 1.4 is not accepted on air services",
          "predicates": [
            {"scope": "hazmat", "field": "un_class", "operator": "equals", "value": "1.4"}
          ]
        }
      ]
    }
  ],
  "catalogs": [
    {
      "catalog_id": "dc-12",
      "versions": [
        {
          "effective_at": 0,
          "published_at": 0,
          "note": "initial",
          "snapshot": {
            "items": [
              {"id": "sku-1", "dimensions_mm": [100, 200, 300], "weight_g": 1200, "description": ""}
            ],
            "cartons": [
              {"id": "box-m", "inner_dimensions_mm": [320, 240, 180], "max_payload_g": 15000, "cost_minor": 85}
            ],
            "pallets": [
              {"id": "euro", "deck_dimensions_mm": [1200, 800], "max_payload_g": 1000000,
               "max_stack_height_mm": 1800}
            ],
            "exclusions": [
              {"id": "x1", "scope": "item_carton", "subject_id": "sku-1",
               "excluded_id": "box-m", "reason": "hazmat"}
            ],
            "overrides": [
              {"id": "o1", "facility_id": "DC-12", "entry_id": "box-m",
               "kind": "carton",
               "override": {"id": "box-m", "inner_dimensions_mm": [300, 240, 180],
                            "max_payload_g": 14000, "cost_minor": 85}}
            ]
          }
        },
        {"rollback_to": 1, "published_at": 900, "effective_at": 900, "note": "revert bad correction"}
      ]
    }
  ]
}
```

All three top-level keys are optional; a document that only needs to price a shipment
may carry only `tariffs`. Field-level rules:

- `accessorials` is an ordered **list**, not an object, so no language has to agree
  about key order; each entry sets exactly one of `flat_charge_minor` or
  `permille_of_base`. Duplicate `accessorial_id` values are an input error.
- A catalog version is either a full `snapshot` version or a `rollback_to` version
  (never both). A rollback publishes a *new*, higher-numbered version whose snapshot
  equals the referenced one's; history is never rewritten.
- A facility override names its `kind` (`item` / `carton` / `pallet`) explicitly rather
  than leaving it to be inferred from which fields the payload happens to carry.
- An omitted optional field and an explicit JSON `null` mean the same thing: absent.
  Writing `"minimum_charge_minor": null` is exactly writing nothing.
- Every integer is exact and, except where a model explicitly allows zero, positive.
  The per-field bounds are the ones the models already enforce — see
  CATALOG-VERSIONING.md and POLICY-RULES.md.

### Input errors versus rejections

A malformed document — a missing required key, a negative weight, an unknown policy
operator, a duplicate id — is an **input error**: `CommerceInputError` in Python,
`Packvium\Commerce\CommerceInputError` in PHP, `Err(CommerceError::Input)` in Rust, a
thrown `CommerceInputError` in JavaScript. It is a caller bug, reported the way each
language reports caller bugs.

A well-formed request the commercial model cannot answer — no tariff effective as of
that instant, no rate for that zone — is a **rejection**: a successful call returning
`"status": "rejected"` with a code from the closed set below. This mirrors how the
packing API already treats an infeasible request: a `PackingResult` with a status, not
an exception.

## Result shapes

Every result is an object with `api_version` (currently `1`) and `status`
(`"ok"` or `"rejected"`).

### `quote`

Request:

```json
{"carrier_id": "acme", "service_id": "ground", "tariff_version": 1,
 "zone": "zone-a", "actual_weight_g": 1200, "volume_mm3": 6000000,
 "requested_accessorials": ["liftgate"]}
```

Exactly one of `tariff_version` (pinned replay) or `as_of` (effective-dated lookup)
must be present. `requested_accessorials` defaults to `[]` and must be unique.

Success — the fields of `commerce/rating/model.py`'s `RateBreakdown`, one for one:

```json
{"api_version": 1, "status": "ok",
 "quote": {"carrier_id": "acme", "service_id": "ground", "tariff_version": 1,
           "zone": "zone-a", "actual_weight_g": 1200, "dimensional_weight_g": 1200,
           "billed_weight_g": 1200, "base_charge_minor": 900,
           "minimum_charge_applied": true, "fuel_surcharge_minor": 108,
           "accessorial_charges_minor": [["liftgate", 250]], "total_minor": 1258}}
```

`accessorial_charges_minor` is a list of `[accessorial_id, amount_minor]` pairs in the
order the request asked for them — the same ordering `RateBreakdown` already records,
preserved rather than sorted so a caller can line the charges up against the request.

`total_minor` is the identical number `commerce/rating/objective.py`'s
`CarrierRateSolutionScorer` and `CarrierRateContainerSelector` already rank containers
and solutions by. That is the point of this function: the price a caller is quoted and
the price the engine optimised against come from one code path.

### `evaluate_policy`

Request:

```json
{"scope": "hazmat", "context": {"un_class": "1.4"}, "as_of": 1000}
```

Exactly one of `as_of` or `rule_versions` (a list of `[rule_id, version]` pairs, each
rule id at most once) must be present. `rule_versions` is the pinned-replay form and
resolves through `PolicyRegistry.resolve_versions`, which sorts the pins so the
snapshot is order-independent.

Success:

```json
{"api_version": 1, "status": "ok",
 "decision": {"scope": "hazmat", "allowed": false,
              "citation": {"rule_id": "no-hazmat-air", "version": 1, "action": "reject",
                           "priority": 10, "reason": "class 1.4 is not accepted on air services"}}}
```

`citation` is `null` exactly when nothing matched (the open-by-default ALLOW). A
`false` `allowed` always carries a citation — the model refuses to construct a
citation-free rejection.

### `catalog_version_info`

Request:

```json
{"catalog_id": "dc-12", "version": 2, "resolved_at": 1700}
```

`resolved_at` is required. At most one of `version` or `as_of`; supplying neither is
allowed only when the catalog has zero or one published version, and is otherwise the
`ambiguous_catalog_reference` rejection.

Success — metadata about the version, deliberately *not* the whole snapshot (a caller
who wants the master data resolves it through the catalog itself; this function
answers "which version am I looking at and what does it contain"):

```json
{"api_version": 1, "status": "ok",
 "catalog": {"catalog_id": "dc-12", "version": 2, "effective_at": 900,
             "published_at": 900, "resolved_at": 1700, "rolled_back_from": 1,
             "note": "revert bad correction",
             "entry_counts": {"items": 1, "cartons": 1, "pallets": 1,
                              "exclusions": 1, "overrides": 1},
             "item_ids": ["sku-1"], "carton_ids": ["box-m"], "pallet_ids": ["euro"]}}
```

`rolled_back_from` is `null` for an ordinary publication. The three id lists are sorted
ascending by code-point so no language's map or set ordering can leak into the answer.

### Rejections

```json
{"api_version": 1, "status": "rejected",
 "error": {"code": "unavailable_zone",
           "fields": {"carrier_id": "acme", "service_id": "ground",
                      "tariff_version": 1, "zone": "zone-z"}}}
```

The closed set of codes, and the workspace error each one corresponds to:

| Code | Raised by | Meaning |
| --- | --- | --- |
| `tariff_not_found` | `TariffNotFoundError` | No history for that `(carrier_id, service_id)`, or no such version number |
| `no_effective_tariff` | `TariffNotFoundError` | A history exists but no version is effective as of `as_of` |
| `unavailable_zone` | `UnavailableServiceError` | The resolved tariff prices no such zone |
| `unavailable_accessorial` | `UnavailableServiceError` | The resolved tariff does not offer a requested accessorial |
| `policy_rule_not_found` | `PolicyRuleNotFoundError` | A pinned `rule_id` has no history |
| `policy_version_not_found` | `PolicyVersionNotFoundError` | A pinned rule version number does not exist |
| `catalog_not_found` | `CatalogError` | No catalog with that `catalog_id` in the document |
| `catalog_version_not_found` | `CatalogVersionNotFoundError` | No such version number, or the catalog has none |
| `no_effective_catalog_version` | `NoEffectiveCatalogVersionError` | `as_of` predates every version |
| `ambiguous_catalog_reference` | `AmbiguousCatalogReferenceError` | Neither `version` nor `as_of`, with more than one version published |

`error.fields` carries only structured values — ids, version numbers, the offending
zone or accessorial id — never a prose message. Human-readable text is a property of
each language's own exception type and is deliberately excluded from the result
document, because prose is the one thing four independent implementations cannot be
held byte-identical on.

`unavailable_accessorial` reports every missing accessorial at once, in a sorted
`accessorial_ids` list, matching what `rate_tariff` already does.

## Traceability

Every exported behaviour resolves to code that already existed before this epic. No
function below computes anything itself.

| Exported | Wraps | Now lives at |
| --- | --- | --- |
| `quote` | `rate_tariff`, `CarrierRegistry.rate` / `.rate_with_version`, `Tariff`, `AccessorialCharge`, `RateBreakdown`, `RatingRequest` | `packvium/commerce/rating.py`, re-exported from `commerce/rating/model.py` |
| `evaluate_policy` | `PolicyRegistry.evaluate` / `.resolve_versions`, `decide`, `PolicyRule`, `PolicyPredicate`, `PolicyDecision`, `PolicyCitation` | `packvium/commerce/policy.py`, re-exported from `domain/policy/model.py` |
| `catalog_version_info` | `CatalogRegistry.publish` / `.rollback` / `.resolve`, `CatalogVersion`, `CatalogSnapshot`, `CatalogReference` | `packvium/commerce/catalog.py`, re-exported from `domain/catalog/model.py` |

The Python relocation is a move, not a copy. `commerce/rating/model.py`,
`domain/policy/model.py` and `domain/catalog/model.py` remain importable at their
original paths and re-export the relocated definitions, so every workspace test,
`integration/product/`, `simulation/` and `recommendations/` import keeps working
against the exact same objects. There is one definition of `rate_tariff` in the Python
tree, and the installed wheel contains it. `commerce/rating/objective.py` — the solver
adapter — is untouched and stays a workspace module: it registers in-process scorer and
selector objects, which EXTENDING.md explains are deliberately not
cross-language features.

## Conformance standard

Held to the standard every other capability in this project is held to, per
TESTING-AND-RELEASE.md:

- **Python and PHP: byte-identical.** Both are ports of one contract; canonical JSON
  (sorted keys, `,`/`:` separators, no trailing whitespace) of the result document must
  match exactly.
- **Rust and JavaScript: valid, and no worse than the floor.** Both are independent
  implementations. Every result must be accepted by the independent validator, and for
  `quote` the `total_minor` must equal the fixture's objective floor — a price is a
  single exact integer, so "no worse than the floor" and "equal" coincide here; there
  is no room for an alternative-but-equally-good answer the way there is for a
  placement.
- **Uniform rejection.** A fixture no engine can price is rejected by all four with the
  same `error.code` and the same `error.fields`.
- **Uniform refusal.** A malformed fixture must make all four *fail* rather than answer.
  This half matters as much as the others: four implementations that agree on every
  well-formed input can still disagree about what counts as well-formed, and the language
  that quietly accepts a string where a list belongs is the one that later returns a
  different answer. Every documented rejection code must be reached by some fixture, and
  the runner fails if one is not.


## Complexity

`h` = versions in one history, `n` = histories, `p` = predicates per rule,
`e` = entries in a catalog snapshot, `a` = requested accessorials.

| Operation | Time | Space |
| --- | --- | --- |
| Load document | `O(total input size)` | `O(total input size)` |
| `quote` (pinned) | `O(h + a)` | `O(a)` |
| `quote` (`as_of`) | `O(h + a)` | `O(a)` |
| `evaluate_policy` (`as_of`) | `O(n * (h + p))` | `O(n)` |
| `evaluate_policy` (pinned) | `O(k log k + k * (h + p))` for `k` pins | `O(k)` |
| `catalog_version_info` | `O(h + e log e)` | `O(e)` |

The `e log e` term is sorting the three id lists; every other bound is a linear scan of
the relevant history. These match the bounds already published for the underlying
models in ALGORITHMS-AND-COMPLEXITY.md — the wrapper
adds parsing and serialization, both linear in the payload, and nothing else.

## Limitations

- The document is supplied by the caller. Nothing here fetches, scrapes or embeds any
  real carrier's published rates; live rate-card ingestion is still out of scope and
  still tracked in LIMITATIONS-AND-ROADMAP.md.
- `catalog_version_info` returns metadata and id lists, not the resolved master-data
  records. Exporting the full snapshot is a larger surface with its own wire-format
  question and is not part of this epic.
- Policy evaluation covers the closed `PolicyScope` / `PolicyOperator` vocabulary.
  An unrecognised scope or operator fails document admission rather than being
  silently ignored — the guarantee `domain/policy/model.py` already makes.
- A JSON number must be written without a fractional part. `1` is an integer; `1.0` and
  `1e3` are not, and Python, PHP and Rust refuse them. JavaScript cannot tell the
  difference — `JSON.parse` gives the same `Number` for `1` and `1.0` — so this is the
  one shape where the four implementations cannot be made to agree, and the contract
  resolves it by requiring callers not to emit it. No fixture uses one.
- JavaScript refuses, rather than rounds, a quote whose components exceed
  `Number.MAX_SAFE_INTEGER`. The arithmetic itself runs in `BigInt`, so nothing drifts
  through a double; what cannot be done is *reporting* a value a JSON number cannot hold
  exactly. Python has no such ceiling, Rust takes every product in `i128`, and PHP falls
  back to decimal-string arithmetic. The bound is far above any real tariff — nine
  quadrillion minor currency units.
