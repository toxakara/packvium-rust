# Units and numerics

## Length

One internal length tick is `1/16000 mm`.

- `1 mm = 16,000 ticks`
- `1 international inch = 25.4 mm = 406,400 ticks`
- common fractional inches through at least `1/128 in` are exact integers

Accepted inputs include integers, decimal strings, fractions (`3/16`) and mixed fractions (`12 3/8`), with `mm`, `cm`, `m`, `in`, `ft`, and ticks. A binary float with a fractional part is intentionally rejected by the Python API, by `Length` and `Weight` alike (`TypeError`); an integral float such as `100.0` is exact, and is what JavaScript sends for `100`, so it is accepted as that integer. PHP type declarations accept only integer/string/structured values.

## Weight

One weight tick is `1/8 microgram`, allowing exact common metric units and exact international avoirdupois conversions used by the package.

## Rounding

Conversion supports floor, ceiling and ties-to-even nearest rounding. Applications should normally round item exterior dimensions outward, container usable dimensions inward, and safety clearance outward before constructing domain objects.

## Geometry versus scores

Feasibility checks are integer-only, including the support ratio: the requested fraction is converted once to parts per million, `ratio_ppm = floor(ratio x 10^6 + 0.5)` on the IEEE double the request carries (each step correctly rounded, so every language gets the same integer; on about one in a hundred seven-decimal halves it differs from rounding the exact decimal, `0.0001245` giving 124), and compared as `supported_area >= floor(base_area x ratio_ppm / 10^6)`, never as a float division against an epsilon. Every result validator applies this same rule rather than the exact rational. `void_fill_reserve_ratio` goes through the same conversion and reserves `floor(inner_volume x ratio_ppm / 10^6)` cubic ticks, which the validators recompute the same way. The independent validator rechecks boundaries and intersections using exact integers.

The Rust solver and validator were the exception until 2026-09-02: both compared `area / base_area + 1e-12 < ratio` in `f64`. On a real-sized base (a square centimetre is `2.56 x 10^10` square ticks) an area one square tick short of the requirement sits well inside that epsilon, so Rust admitted a placement the shared validator refused. Both now go through `support_area_sufficient`, the rule above; the `f64` ratio survives only as the reported `support_ratio` on the placement record.

Ordering keys are exact too. Two volumes that differ by one cubic tick must not collapse onto the same value, or two implementations of one algorithm can order the same items differently.

## PHP integer limits

Coordinates remain 64-bit integers. Values that exceed that range are handled with a decimal-string big-integer implementation rather than being approximated:

| Quantity | Why it overflows | How it is handled |
| --- | --- | --- |
| Volume, for output and ordering | A one-metre cube is `4.096 x 10^21` cubic ticks | `BigInt::multiply`/`add`, compared as decimal integers; this includes exact-small's equal-count physical-volume tie-break |
| Objective keys 3 and 4 | `unused_volume_ppm`, per container `floor((V - used) x 10^6 / V)`, scales a volume by 10^6; `stack_height_ppm` is `floor(top x 10^6 / H)` | `BigInt::subtract` / `BigInt::divide` |
| Load shares, `weight x area / total_area` | Eighth-microgram counts times squared ticks | `Arithmetic::mulDiv`, exact and overflow-free |
| Serialized decimals | Ticks beyond `2^53` do not survive a `double` | long division on digits, rounded ties-to-even |

Python and the Rust core get the same results from native arbitrary-precision integers and `i128`, subject to the rendering rule below.

The JavaScript fallback also performs load distribution in `BigInt`: every
`weight * contact_area / total_area` floor and the accumulated remainder are exact even
when the intermediate product exceeds `Number` precision. The final tick count is
converted back to a number only at the existing JSON boundary.

## The bound path's declared ceiling

The engines compute lower bounds on the objective during search. Their arithmetic is
integer-only, and the limit at which they refuse is **declared rather than inherited from the
language**: every sum in that path must stay below `10^30`, and exceeding it is a structured
refusal in all four engines rather than a number.

The reason is the same one this document already gives for coordinates, taken one step
further. Python's integers are unbounded, PHP's silently become doubles on overflow,
JavaScript's `Number` stops being exact past `2^53`, and Rust's `i128` wraps. Four engines
refusing at four native limits would disagree about which requests are answerable at all --
a caller would get a number from one and a refusal from another for the same input. So each
carries the guarded sums in a representation that holds `10^30` exactly: Python's `int`,
PHP's `BigInt` decimal strings, JavaScript's `BigInt`, Rust's `i128`.

Areas follow the same rule in the search itself (1.9.0). Two lengths below
`PHP_INT_MAX` ticks multiply past a native `int` -- a 10 mm wide item longer than about
3,600 km already does -- and Python and Rust hold that product exactly. PHP forms every such
product (base and contact areas, their sums and the load shared by them, the centre-of-gravity
hull, lattice slot counts, the candidate score's room-left term) through
`Packvium\Support\ExactInteger` and `ExactGeometry`: the native `int` while it fits, an exact
decimal string past it, so an ordinary request pays one `is_int` check. The public
`Dimensions::baseAreaTicks()` and `AxisAlignedBox::overlapAreaXY()` keep their `int` contract.
`support_ratio` and `volume_utilization` are Python's correctly rounded `int / int`, not a
quotient of two doubles.

**The admission ceiling (1.9.0).** Every engine refuses, before any model is built, a
JSON request with a length past `2^53 - 1` ticks (about 563,000 km) or a box -- an item, a
container's inner or outer dimensions, an obstacle, a made-to-measure step -- enclosing more than
`10^30` cubic ticks (about 244 million cubic metres). Both are `invalid_request` with reason
`above_maximum` at the offending pointer (ERRORS.md). The first is the largest tick
count JavaScript's `Number` holds exactly; the second is the bound path's own ceiling, which keeps
`volume x 10^6` inside Rust's `i128`. Until 1.9.0 each engine failed past its own native limit: Rust
and PHP refused a length past `2^63 - 1` ticks with different errors, Python and JavaScript answered,
Python died with `decimal.InvalidOperation` at `10^20` mm, and Rust overflowed `i128` on a container
past about `1.7 x 10^32` cubic ticks. A whole JSON number past `2^53 - 1` is refused the
same way however a parser stored it, and a value past a native integer is read exactly to tell
which way it is out of range.

**Weights (1.9.0).** A weight is at most `2^63 - 1` ticks (about 1.15 million
tonnes), and so is every item's weight times its quantity plus the heaviest container tare, so
no container's payload or gross weight can pass a signed 64-bit integer, which is how Rust and
PHP add them. Past either, the request is `invalid_request` / `above_maximum`, at the weight or at
`/items`. A weight written as a JSON number past `2^53 - 1` is `invalid_value`: JavaScript has
rounded such a number before any engine code reads it, so a weight that large is written as a
string. Inside the ceiling every engine is exact: Python's `int`, PHP's `int` and `BigInt`,
Rust's `i64` with `i128` products, and in JavaScript a `BigInt` for every weight sum. A
JavaScript result carries a weight's `ticks`, and a `shipping_cost` score position, as a `Number`
while it is exact and as a `BigInt` past `2^53 - 1`; `stringifyExact` and `parseExact` move such
a result through JSON text without rounding it, and `pack` uses them on both backends.

That intermediate ceiling is deliberately not the result ceiling. The five bound keys can
cross JSON and are returned as JavaScript `Number` values, so every engine also refuses a
final key above **`2^53 - 1` (`9,007,199,254,740,991`)**. This is the largest integer all four
bindings can return without changing its value. In particular, an unlimited inventory does
not make an expensive container count as one during validation: the selected opening costs
are summed exactly, checked against `10^30`, then checked against the portable result ceiling
before PHP or JavaScript converts them to a native integer.

The value comes from the widest intermediate the formulas form -- a summed volume times
`10^6` -- which puts the largest product at `10^36`, about 170-fold inside an `i128`. It never
binds on a real request: `10^30` cubic ticks is 244 million cubic metres.

## Decimal rendering

`value` fields (e.g. `Length.decimal()`, `Weight.decimal()`, PHP's `RationalParser::decimalString()`) render an exact `ticks / divisor` rational to a fixed number of digits (8 by default). All four engines round the truncated remainder **ties-to-even**, matching Python's `Decimal.quantize` under its default context — the last kept digit rounds up when the discarded remainder is more than half the divisor, stays put when it's less, and on an exact half rounds to whichever choice makes that digit even.

This is a one-way decision: PHP, Rust and the JavaScript fallback previously each rendered differently (PHP and Rust truncated the remainder outright; the JavaScript fallback used a plain `Number` float division with no fixed precision at all). All three now perform the identical digit-by-digit long division as Python's `Fraction`/`Decimal` path and apply the same ties-to-even rule, so a caller comparing rendered `value` strings across engines will always agree, not just the underlying `ticks` integer and the independent validator's geometry checks.
