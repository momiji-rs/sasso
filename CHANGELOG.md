# Changelog

All notable changes to **sasso** are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

Conformance is tracked separately as a ratchet against the official
[sass-spec](https://github.com/sass/sass-spec) suite — see the
[Conformance](README.md#conformance) section for the current pass rate.

## [Unreleased]

### Fixed

- Compressed output now emits a color's canonical CSS name when it is no longer
  than the shortest hex, matching dart-sass (`red` not `#f00`, `aqua` not
  `#0ff`; duplicate names resolve to dart's canonical pick — `cyan`/`grey` →
  `aqua`/`gray`). Expanded output (which preserves the authored spelling) is
  unchanged.

## [0.3.0] - 2026-06-13

Since `0.2.0`. Conformance holds at **100% of the attempted sass-spec suite**
(13,896 / 13,896) — but that suite covers *valid* inputs plus the errors it
expects; this cycle hardened sasso to reject the same *malformed* inputs
dart-sass rejects, and cut more of the `@extend` and value hot paths.

### Changed

- **Strict input validation.** Beyond matching dart-sass's output, sasso now
  *errors* — rather than silently accepting — on malformed input, each with
  dart-sass's exact message: an invalid hex literal (`#00000`, `#0g`),
  out-of-grammar `rgb()`/`hsl()` channel units and legacy-vs-modern argument
  shapes, a duplicate `@mixin`/`@function` parameter, a malformed number
  exponent (`1e-`), a non-identifier `@use`/`@forward` namespace, a misplaced
  `@content`/`@extend`, a style rule / declaration / `@extend` in a `@function`
  body, a map or empty list used as a CSS value (`#{(a:1)}`, `-()`), a
  malformed `:nth-child()` An+B or empty `:not()` selector, a stray `!` in a
  selector, a leading-empty `@extend` target, and a malformed `@charset` /
  `@at-root (…)` query. Found by a leniency-mining sweep that diffed every
  category against dart-sass; the fixes are uncovered by the spec, so the
  ratchet is unchanged.

### Performance

- **Transitive `@extend`** went from ~151× *slower* than dart-sass to *faster*
  on a deep extend chain: a match pre-filter with typed dedup, an incremental
  per-rule fold (killing the O(N²) closure re-derivation), borrowed
  scope-originals, and cached typed selector hashes — the `@extend` maps are
  now FxHash + typed `Complex`/`Simple` keys, guarded by a render-injectivity
  parity proof. Byte-identical output throughout.
- **Reference-counted composite values** — `Str`/`List`/`Map` are `Rc`-backed,
  so cloning a read-only `$variable` is an O(1) refcount bump instead of a deep
  copy (copy-on-write for the mutating builtins): ~7× fewer instructions and
  ~13× less peak memory when a large list/map is passed through a call chain.
- **`Cow`-borrowed argument-name normalization** (called 4–6× per function
  call) plus trimmed function-call-path allocations — ~15% fewer instructions
  on a function-heavy compile.
- **Arena in-place `realloc`** — the scoped bump arena extends its tail
  allocation in place instead of stranding a dead buffer on every `Vec`
  doubling, trimming peak memory on parse-heavy compiles.
- Net: pure-compile throughput ~7.4 ms on the large benchmark (was ~9–10),
  ~2.3–2.9× faster than `grass` and ~19–30× faster than the dart-sass JS bin.

### Internal

- `eval.rs` split into an `eval/` module directory and `color.rs` into a
  `color/` directory (pure code moves); the typed selector model gained a
  parity-proof harness; the stringly hoist markers became typed `OutNode`
  variants; the `@extend` cartesian-order bool became a `CartesianOrder` enum;
  `OutNode` rule/at-rule constructors collapsed duplicated construction sites.
  All byte-identical, each verified base-binary-vs-refactor.

### Tooling

- The WebAssembly npm package publishes via OIDC Trusted Publishing (no token).
- Benchmark harness uses portable temp-file handling (`mktemp -d`).

## [0.2.0] - 2026-06-11

Everything since the initial `0.1.0` crates.io publish. This grew the compiler
from an early vertical slice to **100% of the *attempted* official sass-spec
suite** (13,896 / 13,896, zero failures — 11,405 byte-exact CSS outputs plus
2,491 error specs correctly rejected; the 8 remaining cases are tagged `:todo`
for dart-sass itself upstream), matching current dart-sass (1.100) byte-for-byte.
The pass is measured against a conformance harness tightened to reproduce the
official sass-spec comparator (`normalizeOutput`) exactly — collapse newline
runs only, no extra whitespace leniency — so the count holds under the upstream
comparator, not just a looser local one.

### Added

- **Byte-exact diagnostics** — errors, `@error`, `@warn`, and `@debug` now
  reproduce dart-sass's stderr byte-for-byte: source-span `╷│╵` snippets with
  carets and right-aligned gutters (tab→4 spaces), aligned stack frames
  (`root stylesheet` / `name()` / `@import`), a `--no-unicode` flag, and the
  `@import` deprecation warning (with a per-id cap/dedup deprecation registry).
  238 of the suite's 3,256 stderr expectations now match byte-for-byte (a
  `spec/run_spec.py --check-stderr` metric tracks it); the rest (other
  deprecations, multi-span layouts) build on this foundation.
- **`@use` / `@forward` module system** — built-in `sass:*` modules and user
  files, `with` configuration, namespacing, `@forward` prefix/`show`/`hide`,
  dash-insensitive member access, forward conflict resolution, and star
  (`as *`) modules.
- **Indented `.sass` syntax** — a full front-end (`Options::with_syntax`, the
  CLI `--indented` flag, `.sass` extension inference), including cross-syntax
  `@import` of partials by file extension.
- **CSS Color 4 color spaces** — `srgb`/`display-p3`/`lab`/`lch`/`oklab`/
  `oklch`/`xyz` via `color()`, with modern color serialization.
- **`@extend` and `%placeholder`s** — a faithful port of dart-sass's
  `ExtensionStore` engine: registration-order extension folding, selector
  weaving/unification/trimming, `@use`/`@forward` cross-module visibility, and
  the self-referential `:not`/`:has` pseudo cases — closing the suite's
  `@extend` family to byte-exact parity.
- **Built-in function modules** — `meta` (first-class function references via
  `get-function`/`call`, existence predicates), `math` (`clamp`/`min`/`max`/
  `round`/`log` and friends), `list` (bracket-preserving `join`/`append`),
  `map` (nested key paths, `deep-merge`/`deep-remove`), `string`
  (`split`/`unique-id`), and `selector` functions.
- **First-class mixins** — `meta.get-mixin` returns a mixin value and
  `meta.apply` invokes it (with `@content` support).
- **CLI** — compile multiple input files in one process (`sasso a.scss b.scss`,
  startup shared across files); `--loop <N>` for in-process throughput and
  `-q`/`--quiet` to suppress stdout (used by the benchmark harness).
- **Benchmark harness** — sasso registered as a first-class engine in `bench/`;
  three-way report [`bench/three_way.md`](bench/three_way.md) (sasso vs
  dart-sass vs grass).
- **`CODE_OF_CONDUCT.md`** adopting the
  [Sass Community Guidelines](https://sass-lang.com/community-guidelines/), as
  the Sass project asks every implementation to do.
- **This CHANGELOG.**

### Changed

- Selector resolution now matches dart-sass on combinator normalization,
  adjacent-compound separation, and bogus-combinator omission.
- `color` functions match dart-sass strictness: channel-unit leniency in
  `adjust`/`change`, missing/powerless-channel errors, the Microsoft `alpha()`
  filter overload, and `adjust-hue` rejecting non-legacy colors.
- `selector` functions coerce string/list arguments and validate arity, and
  accept a list of extendees in `extend`/`replace`.
- `list` builtins validate fixed-arity arguments and preserve list shape.
- Unquoted string serialization collapses newlines to spaces; custom-property
  values are emitted verbatim — both matching dart-sass.
- Control-flow blocks use semi-global scoping with a global-write guard.

### Performance

Profiling showed the compiler is allocation- and hashing-bound; a series of
hot-path cuts followed, with no behavior change (cumulative **~2× faster** on the
large benchmark vs. the original, lifting the lead over `grass` to ~1.9–2.4× and
over dart-sass to ~16–25×):

- Selector helpers `split_commas`/`tokenize_complex` return borrowed `&str`
  slices, and `copy_name`/`normalize_selector` avoid their intermediate
  `String`/`Vec` — no per-part/per-token/per-name heap allocation on the hot
  selector-resolution path.
- The compiler's internal `String`-keyed maps (variable scope, function/mixin
  tables, module maps) use a small inline FxHash hasher instead of std's
  DoS-resistant-but-slow SipHash. (Still zero runtime dependencies.)
- A **scoped bump-arena allocator** (`ScopedAlloc`): within each `compile()` a
  per-thread arena turns every allocation into a pointer bump and frees them
  wholesale (reset) at the end — a further ~1.5×. It is installed as the CLI's
  `#[global_allocator]`; library/wasm embedders can opt in the same way (it
  forwards to the system allocator outside a compile, so it's safe to install
  unconditionally). This is the library's one audited `unsafe` module —
  verified by unit tests, Miri (no UB), AddressSanitizer, and the full sass-spec
  suite run through it (zero crashes, byte-identical output); the
  rest of the crate is `deny(unsafe_code)`. Still zero runtime dependencies.
- **Smaller `Value`** — the `Color` variant's modern-color payload is boxed, so
  the `Value` enum drops from 128 to **64 bytes** (a compile-time `size_of`
  guard prevents regressions). Halves every scope-map slot, `Vec<Value>` element
  and lookup clone, with byte-identical output.
- **Zero-dependency Ryū float formatter** — a from-scratch `d2s` shortest-round-
  trip formatter on the float-to-string hot path, replacing `core::fmt`, with a
  differential fuzz test against a reference.

### Tooling

- The conformance ratchet pins the sass-spec commit (`spec/SPEC_VERSION.txt`)
  for reproducibility, with a `--latest`/`--canary` drift-detection mode.
- The conformance harness now reproduces the official sass-spec comparator
  (`sass-spec/lib/test-case/compare.ts` `normalizeOutput`) exactly, so a "pass"
  means byte-identical under the upstream comparator — no extra local leniency.

## [0.1.0] - 2026-06-06

Initial crates.io publish — an early vertical slice that already compiled
real-world SCSS byte-identically to dart-sass.

### Added

- `$variables` with lexical scoping, `!default` and `!global`.
- Nesting, the `&` parent selector with selector-list multiplication, and
  combinator normalization (`>`, `+`, `~`).
- `#{}` interpolation in selectors, property names and values.
- `//` (stripped) and `/* */` (preserved) comments.
- Numbers with units and unit arithmetic.
- A color model with fractional channels and author-spelling preservation;
  color functions (`rgb`/`rgba`/`hsl`/`hsla`/`mix`/`lighten`/`darken`/
  `percentage`/`red`/`green`/`blue`/`alpha`).
- `@import` partial inlining through a pluggable `Importer` (CSS imports pass
  through); a ready-made `FsImporter`.
- `expanded` and `compressed` output styles.
- Distribution: CLI binary (prebuilt via cargo-dist), library crate, and a
  zero-dependency WebAssembly build published to npm as `@momiji-rs/sasso`.

[Unreleased]: https://github.com/momiji-rs/sasso/compare/v0.3.0...HEAD
[0.3.0]: https://github.com/momiji-rs/sasso/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/momiji-rs/sasso/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/momiji-rs/sasso/releases/tag/v0.1.0
