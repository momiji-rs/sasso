# Custom-functions / `Value` API — dart-sass compatibility gap analysis

> Status: Phase 4 shipped a **working** custom-functions feature in `sasso@0.7.2`
> (number/string/color/list/map/bool/null, sync + async). This document tracks
> what remains to reach **100% dart-sass JS `Value` API compatibility**, so a
> custom function written for `sass` runs unchanged on `sasso`.
>
> Authoritative reference: the dart-sass JS API (`Value`, `SassNumber`,
> `SassColor`, …). Frequency = how often real-world custom functions hit it.

## What works today (`sasso@0.7.2`)

- Value types across the boundary: `null`, `bool`, `SassNumber` (full unit
  lists), `SassString`, `SassColor` (every CSS Color 4 space), `SassList` /
  `SassArgumentList`, `SassMap`.
- Base: `isTruthy`, `realNull`, `asList`, `hasBrackets`, `separator`, `equals`,
  `get(index)` (non-negative), `assertNumber/String/Color/Map/Boolean`.
- `SassNumber`: `value`, `numeratorUnits`, `denominatorUnits`, `hasUnits`,
  `isInt`, `asInt`, `hasUnit`, `assertInt`, `assertUnit`.
- `SassColor`: `space`, `channels`, `channelsOrNull`, `alpha`, `channel(name)`
  (current space), `isChannelMissing`, `red/green/blue`.
- Sync + async callbacks; precedence (user `@function` > custom > builtin); error
  surfacing.

## Gaps, by tier

### TIER 0 — return-type fidelity (HIGH impact, pervasive)

dart-sass returns **`immutable.List` / `immutable.OrderedMap`** from `asList`,
`numeratorUnits`, `denominatorUnits`, `channels`, `channelsOrNull`,
`SassList`'s contents, and `SassMap.contents`. We return plain JS arrays/`Map`s,
so a function calling `.get(i)` / `.size` / `.equals()` on them — or doing
`map.contents.get(key)` (value-equality keyed) — breaks.

- **Decision needed:** (A) depend on the `immutable` npm package (exactly what
  dart-sass does, but ends our zero-dependency story), or (B) ship a tiny
  dependency-free shim implementing the `immutable` `List`/`OrderedMap` *read*
  API we expose. Recommend **B**.
- `SassMap.contents` must be value-equality-keyed; our convenience
  `SassMap.get(key)` is non-standard (dart uses `map.contents.get(key)`).

### TIER 1 — pure-JS methods, no conversion math (MODERATE, low effort)

- `Value.sassIndexToListIndex(sassIndex, name?)` + negative indexing in `get()`
  (1-based, negatives, bounds errors) — used by most list-handling functions.
- `Value.tryMap()` (empty list `()` → empty map), `Value.hashCode()`.
- `Value.assertCalculation/assertFunction/assertMixin` (throwers).
- `SassNumber.assertNoUnits`, `assertInRange(min,max,name?)`.
- `SassString.sassIndexToStringIndex(sassIndex, name?)`, `SassString.empty`.
- `SassColor.isLegacy`.
- **Error-message fidelity:** `assert*`/index errors must match dart's exact
  wording (incl. the `$name:` prefix). Ours are approximate.

### TIER 2 — conversion-dependent methods (MODERATE, HIGH effort)

These need sasso's Rust unit / CSS Color 4 conversion math. **Recommended
approach: route through the engine** — add wasm exports (e.g.
`sasso_number_convert`, `sasso_color_to_space`, `sasso_value_equals`) operating
on serialized values, callable re-entrantly from a JS `Value` method during a
host call — so we reuse the battle-tested Rust math with ZERO divergence
(reimplementing the conversion tables/matrices in JS risks last-ulp drift from
dart).

- `SassNumber`: `convert` / `convertToMatch` / `convertValue` /
  `convertValueToMatch` / `coerce` / `coerceToMatch` / `coerceValue` /
  `coerceValueToMatch`, `compatibleWithUnit`.
- `SassColor`: `toSpace`, `channel(name, {space})`, `change({…})`, `isInGamut`,
  `toGamut`, `isChannelPowerless`, and the legacy getters that imply conversion
  (`hue/saturation/lightness/whiteness/blackness`).
- **Value equality with unit conversion** (`1in == 96px`) + matching `hashCode`,
  and color equality semantics — needed for correct `SassMap` keys. Route to
  sasso's `sass_eq`.

### TIER 3 — missing value TYPES (LOW frequency, varied effort)

- `SassCalculation` (`calc()` as an argument or return) + `CalculationOperation`
  / `CalculationInterpolation` / `CalculationValue`, and `assertCalculation`.
  Needs the `CalcNode` tree serialized both ways. *(Currently a clear error.)*
- `SassFunction` (first-class function ref, for `meta.call`) — needs an
  engine-side **opaque handle table** (JS holds an opaque handle, passes it
  back). *(Currently a clear error.)*
- `SassMixin` (first-class mixin ref) — same handle mechanism. *(Very rare.)*

## CLI gaps (separate track)

- **`--watch`** (flagged important): re-compile on change; track dependencies via
  `loadedUrls`. Moderate effort (`fs.watch` + recompile loop).
- `--embed-source-map` (inline), `--update`, `--error-css`, multiple
  `input:output` pairs, `--quiet`, `--color`, `--no-charset`,
  `--[no-]source-map-urls=relative|absolute`, `--stop-on-error`.

## Recommended sequencing

1. **Tier 0** (immutable shim + `SassMap.contents`) — pervasive; unblocks
   real-world functions.
2. **Tier 1** (pure-JS methods + error-message fidelity) — cheap, high coverage.
3. **CLI `--watch`** — user-flagged, independent.
4. **Tier 2** (engine-routed conversions) — the big one; design the
   re-entrant value-conversion bridge once, reuse for number + color + equals.
5. **Tier 3** (calc / function / mixin types) — niche; do last.
