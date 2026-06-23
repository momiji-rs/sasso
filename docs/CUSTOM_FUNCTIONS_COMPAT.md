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

### TIER 0 — return-type fidelity ✅ DONE (zero-dep shim, `_immutable.mjs`)

`asList`, `numeratorUnits`, `denominatorUnits`, `channels`, `channelsOrNull`,
`SassList` contents and `SassMap.contents` now return the dependency-free
`List`/`OrderedMap` (`_immutable.mjs`) — the common `immutable` read subset
(`get`/`size`/`has`/`keys`/`values`/`forEach`/iterate/`map`/`filter`/`slice`/
`equals`, `toArray`/`toJS` to escape). `SassMap.contents` is value-equality
keyed; the non-standard `SassMap.get(key)` was removed (use
`map.contents.get(key)`, matching dart). Chose the shim over a dependency to keep
the package dep-free; `.toArray()` covers any esoteric `immutable` method.

### TIER 1 — pure-JS methods ✅ DONE (one caveat)

`Value.get()` negative indexing, `sassIndexToListIndex`, `tryMap` (incl. empty
list → empty map), `hashCode`, `assertCalculation/Function/Mixin`;
`SassNumber.assertNoUnits` + `assertInRange`; `SassString.sassIndexToStringIndex`
+ `empty`; `SassColor.isLegacy`. All covered in `wasm/test.mjs` + tsc.
- **Caveat (still open):** `assert*`/index error MESSAGES are dart-like but not
  yet byte-exact (the `$name:` prefix is there; exact wording/inspect formatting
  is follow-up polish).

### TIER 2 — conversion-dependent methods (engine-routed)

The **engine-routing bridge is built**: a JS `Value` method serializes its
operands and calls a wasm `sasso_value_op` export → core `host_value_op` (reuses
the exact Rust math, ZERO divergence). It runs on an independent value instance,
so methods work **standalone** and **re-entrantly** during a (sync or async)
compile.

- **`SassNumber` ✅ DONE (Tier 2a):** `convert` / `convertToMatch` /
  `convertValue` / `convertValueToMatch` / `coerce` / `coerceToMatch` /
  `coerceValue` / `coerceValueToMatch`, `compatibleWithUnit` — routed to
  `unit_lists_factor` with dart's convert/coerce unitless rules. Tested
  standalone + re-entrant (sync & async).
- **`SassColor` ✅ DONE (Tier 2b + 2c):** `toSpace`, `channel(name, {space})`,
  `isInGamut`, `toGamut`, legacy getters (`red/green/blue` cross-space,
  `hue/saturation/lightness`, `whiteness/blackness`), `isChannelPowerless`,
  `interpolate` (→ `color.mix`), and `change({…})` (pure JS: a copy with channels
  replaced, converting via `toSpace` when a `space` is given). All routed to the
  Rust math; tested standalone + re-entrant. **Tier 2 is COMPLETE.**
- **Value equality with unit conversion** (`1in == 96px`) + matching `hashCode`:
  still pure-JS exact-units (can't route — `.equals` is called outside a compile
  too). Minor divergence for unit-mismatched map keys; documented.

### TIER 3 — missing value TYPES (LOW frequency, varied effort)

- **`SassCalculation` ✅ DONE (Tier 3a):** `calc()`/`min()`/`max()`/`clamp()` as
  an argument or return, plus `CalculationOperation` and `assertCalculation`. The
  `CalcNode` tree round-trips both ways over a new `TAG_CALC` wire encoding
  (Number/Str/Op/Func). Tested (receive + inspect `calc(1px + 2%)`; return
  `calc`/`min` incl. `var()`). *(`CalculationInterpolation` is deprecated/legacy —
  not modelled.)*
- **`SassFunction` / `SassMixin` ✅ DONE (Tier 3b):** first-class refs round-trip
  as **opaque handles** — `serialize_args` stores the `Value` in a per-dispatch
  handle table (swapped/restored around each custom-function call for nesting
  safety) and emits its index; JS holds an opaque `SassFunction`/`SassMixin` and
  passes it back, which the engine looks up. Tested: receive a function ref and
  return it → `meta.call(it, 5)` = 10; same for a mixin → `meta.apply(it)`.

  **🎉 THE FULL dart-sass `Value` TYPE SYSTEM IS NOW SUPPORTED.**

## CLI gaps (separate track)

- ~~**`--watch`**~~ ✅ DONE — `-w/--watch` recompiles on change, tracking
  dependencies via `loadedUrls` (watches their directories, debounced).
- Still open: `--embed-source-map` (inline), `--update`, `--error-css`, multiple
  `input:output` pairs, `--quiet`, `--color`, `--no-charset`,
  `--[no-]source-map-urls=relative|absolute`, `--stop-on-error`.

## Recommended sequencing

1. ~~**Tier 0**~~ ✅ done (immutable shim + value-keyed `SassMap.contents`).
2. ~~**Tier 1**~~ ✅ done (pure-JS methods; error-message *exactness* still open).
3. ~~**CLI `--watch`**~~ ✅ done (`-w/--watch`, dependency-tracked).
4. **Tier 2** (engine-routed conversions) — the big one; design the ← next
   re-entrant value-conversion bridge once, reuse for number + color + equals.
5. **Tier 3** (calc / function / mixin types) — niche; do last.
6. Error-message byte-exactness pass (the Tier-1 caveat).
