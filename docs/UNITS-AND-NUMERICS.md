# Units and numerics

## Length

One internal length tick is `1/16000 mm`.

- `1 mm = 16,000 ticks`
- `1 international inch = 25.4 mm = 406,400 ticks`
- common fractional inches through at least `1/128 in` are exact integers

Accepted inputs include integers, decimal strings, fractions (`3/16`) and mixed fractions (`12 3/8`), with `mm`, `cm`, `m`, `in`, `ft`, and ticks. Binary floating-point input is intentionally rejected by the Python API; PHP type declarations accept only integer/string/structured values.

## Weight

One weight tick is `1/8 microgram`, allowing exact common metric units and exact international avoirdupois conversions used by the package.

## Rounding

Conversion supports floor, ceiling and ties-to-even nearest rounding. Applications should normally round item exterior dimensions outward, container usable dimensions inward, and safety clearance outward before constructing domain objects.

## Geometry versus scores

Feasibility checks are integer-only, including the support ratio: the requested fraction is converted once to parts per million and compared as `supported_area >= floor(base_area x ratio_ppm / 10^6)`, never as a float division against an epsilon. The independent validator rechecks boundaries and intersections using exact integers.

Ordering keys are exact too. Two volumes that differ by one cubic tick must not collapse onto the same value, or two implementations of one algorithm can order the same items differently.

## PHP integer limits

Coordinates remain 64-bit integers. Values that exceed that range are handled with a decimal-string big-integer implementation rather than being approximated:

| Quantity | Why it overflows | How it is handled |
| --- | --- | --- |
| Volume, for output and ordering | A one-metre cube is `4.096 x 10^21` cubic ticks | `BigInt::multiply`/`add`, compared as decimal integers; this includes exact-small's equal-count physical-volume tie-break |
| Objective keys 3 and 4 | Ratio of two volumes | `BigInt::subtract` / `BigInt::divide`, see OBJECTIVE.md |
| Load shares, `weight x area / total_area` | Eighth-microgram counts times squared ticks | `Arithmetic::mulDiv`, exact and overflow-free |
| Serialized decimals | Ticks beyond `2^53` do not survive a `double` | long division on digits, rounded ties-to-even |

Python and the Rust core get the same results from native arbitrary-precision integers and `i128`, subject to the rendering rule below.

The JavaScript fallback also performs load distribution in `BigInt`: every
`weight * contact_area / total_area` floor and the accumulated remainder are exact even
when the intermediate product exceeds `Number` precision. The final tick count is
converted back to a number only at the existing JSON boundary.

## Decimal rendering

`value` fields (e.g. `Length.decimal()`, `Weight.decimal()`, PHP's `RationalParser::decimalString()`) render an exact `ticks / divisor` rational to a fixed number of digits (8 by default). All four engines round the truncated remainder **ties-to-even**, matching Python's `Decimal.quantize` under its default context — the last kept digit rounds up when the discarded remainder is more than half the divisor, stays put when it's less, and on an exact half rounds to whichever choice makes that digit even.

This is a one-way decision: PHP, Rust and the JavaScript fallback previously each rendered differently (PHP and Rust truncated the remainder outright; the JavaScript fallback used a plain `Number` float division with no fixed precision at all). All three now perform the identical digit-by-digit long division as Python's `Fraction`/`Decimal` path and apply the same ties-to-even rule, so a caller comparing rendered `value` strings across engines will always agree, not just the underlying `ticks` integer and the independent validator's geometry checks.
