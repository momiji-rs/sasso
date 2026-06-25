# Changelog

All notable changes to **sasso** are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

Conformance is tracked separately as a ratchet against the official
[sass-spec](https://github.com/sass/sass-spec) suite — see the
[Conformance](README.md#conformance) section for the current pass rate.

## [Unreleased]

## [0.6.2] - 2026-06-25

### Fixed

- **Compressed output now emits the shortest equivalent legacy-color form**,
  matching dart-sass 1.101.0 byte-for-byte. A computed color such as
  `darken(#336699, 10%)` is written as `hsl(210,50%,30%)` instead of the longer
  `rgb(38.25,76.5,114.75)`, and an integer-rgb-equivalent hsl literal
  (`hsl(210, 50%, 40%)`) collapses to `#369`. The serializer now compares the
  hex/name, `rgb()`/`rgba()`, and `hsl()`/`hsla()` candidates and keeps the
  shortest — the rgb form winning ties — while a powerless (zero-saturation)
  hue is preserved. Expanded output is unchanged. This compressed path had no
  cross-check before (the conformance ratchet and the parity suite were both
  expanded-only), so a compressed dart-sass parity battery now runs in
  `tests/parity.rs`.

## [0.6.1] - 2026-06-16

### Fixed

- A relative `meta.load-css` inside a **first-class mixin** (captured with
  `meta.get-mixin` and invoked via `meta.apply`) now resolves against the
  mixin's **defining** file, not the caller's (issue #8). The regular
  namespaced include path was already correct.

### Added

- A **C ABI** (`ffi/`, `libsasso` + `sasso.h`) — drive sasso in-process from any
  language with a C FFI, with a userland importer callback. Releases now attach a
  per-target `sasso-<version>-<target>-c-api.{tar.xz,zip}` (prebuilt static +
  dynamic library + header). See the "C ABI" section in the README.

## [0.6.0] - 2026-06-15

### Changed (breaking)

- **The `Importer` trait is now dart-sass's two-phase `canonicalize`/`load`**
  (issue #4, RFC in `docs/IMPORTER_REDESIGN.md`). It replaces the old
  `resolve(path) -> Option<String>` plus the accreted `resolve_*` overloads:

  ```rust
  fn canonicalize(&self, url: &str, ctx: &CanonicalizeContext)
      -> Result<Option<CanonicalUrl>, ImporterError>;
  fn load(&self, canonical: &CanonicalUrl)
      -> Result<Option<ImporterResult>, ImporterError>;
  ```

  `canonicalize` resolves a URL to a stable identity without loading (its result
  is the module-cache key); `load` fetches the source as an
  `ImporterResult { contents, syntax, source_map_url }`. Three outcomes:
  `Ok(Some)` = handled, `Ok(None)` = not handled, `Err(ImporterError)` =
  handled-but-failed (an actionable compile error rather than a silent miss).
  New public types: `CanonicalUrl`, `ImporterResult`, `ImporterError`,
  `CanonicalizeContext`. A clean break with no compatibility shim (pre-1.0).
  `FsImporter` and the built-in resolution are unchanged in behavior (sass-spec
  ratchet delta +0); only custom `Importer` implementations must migrate.

### Added

- `ImporterResult.source_map_url` lets an importer set the URL recorded for a
  loaded file in generated source maps (dart-sass `ImporterResult.sourceMapUrl`).

## [0.5.3] - 2026-06-15

### Fixed

- **`!default` no longer evaluates its right-hand side when the variable is
  already set.** dart-sass short-circuits a guarded (`!default`) assignment
  *before* evaluating the RHS, so an expression that would otherwise error is
  harmless once the variable already holds a non-null value; sasso evaluated the
  RHS first. This surfaced in Bootstrap-on-Shopware setups where, after an
  override sets `$w: 1rem`, a later `$p: $w + .5em !default` raised an
  "incompatible units" error instead of being skipped. Thanks to
  [@shyim](https://github.com/shyim) (#2).
- **Legacy `rgb()`/`hsl()` preserve the caller's `rgba`/`hsla` spelling in
  special-value and relative-color passthroughs.** When a call can't resolve to
  a concrete color (a channel or alpha is a `var()`/`env()`/non-foldable
  `calc()`), dart-sass keeps the call *and* the exact function name written;
  sasso normalized `rgba`/`hsla` down to `rgb`/`hsl`, breaking Bootstrap's
  `rgba(var(--bs-body-color-rgb), …)` output. The called name is now threaded
  through every passthrough, keeping dart's carve-out that a `none`-only call
  still normalizes to the canonical `rgb`/`hsl` (and a `calc()` alpha that folds
  to a number resolves to a real color rather than a passthrough). Thanks to
  [@shyim](https://github.com/shyim) (#3).

## [0.5.2] - 2026-06-14

### Fixed

- **Expanded `@at-root` group-separation blank lines.** dart-sass writes one
  blank line at an `@at-root` hoist→resume boundary when the hoisted chunk ends
  in a style rule, while keeping a nested-`@at-root` chain and a rule + its own
  bubbled `@media` contiguous. sasso previously diverged BOTH ways — it never
  emitted the blank before a resumed parent rule, and it over-emitted (three
  blanks between top-level bare-`@at-root` siblings, spurious blanks between a
  nested-`@at-root` chain's rules / between a rule and its own bubbled `@media` /
  before an `@at-root` body's trailing comment). Now byte-exact vs dart-sass
  1.101 across a dedicated 54-shape group-separation sweep, with non-`@at-root`
  output byte-identical. Compressed output is unaffected (no blank lines).
  (sass-spec does not cover these `@at-root`-resume blanks.)

## [0.5.1] - 2026-06-14

Source-map fidelity + compressed-output corrections, all byte-exact vs
dart-sass 1.101.

### Fixed

- **Source maps: `@media`/`@at-root`/`@supports` bubbled parent selector.** When
  one of these at-rules nested in a style rule bubbles a copy of the enclosing
  selector out (`@media screen { .a { … } }`), that copy now maps back to the
  ORIGINAL rule's source position, matching dart-sass. It previously had no
  mapping at all — which in compressed output also let the consecutive-same-
  source-line coalescing drop a following declaration's mapping (a 0.5.0
  regression vs 0.4.0 for `@media`/`@at-root`-bubbled rules). CSS is unchanged.
- **Source maps: `@supports` header.** The `@supports (…)` at-rule header now
  maps to its `@supports` keyword (as `@media` already did); previously it had
  no mapping. CSS is unchanged.
- **Compressed `@media`/`@supports` whitespace.** Compressed output now omits the
  space before a prelude beginning with `(` for `@media`/`@supports`
  (`@media(min-width: 1px)`), and within a `@media` query drops the space before
  `and`/`or` after a `)` (`(a)and (b)`) and after the comma between queries
  (`(a),(b)`) — matching dart-sass. Other at-rules (`@container`) and `@supports`
  conditions keep their spaces. (Compressed CSS output change; expanded
  unchanged.)

## [0.5.0] - 2026-06-14

### Added

- **wasm: source maps.** The `@momiji-rs/sasso` package's `compile(scss, {
  sourceMap: true [, sourceMapIncludeSources: true] })` now returns
  `{ css, sourceMap }` (the v3 map as a parsed object) instead of a bare CSS
  string; without `sourceMap` it still returns the string (backwards
  compatible). New `sasso_compile_map` export returns a framed `[u32 css_len][css]
  [map json]` buffer. Source maps are now exposed on every surface (lib, CLI,
  wasm).

### Fixed

- **Compressed source maps** now emit one segment per source line, matching
  dart-sass (compressed packs many tokens onto a line; dart maps only the first
  per source line). Expanded maps are unchanged. The map's CSS is unaffected.

## [0.4.0] - 2026-06-14

### Added

- **Source map (v3) support.** New `compile_with_source_map(source, &Options)
  -> CompileResult { css, source_map: SourceMap }`, with `SourceMap::to_json()`
  and `Options::with_source_map_include_sources(bool)`. The CLI gains
  `-o/--output <file>` (write CSS to a file), `--source-map` (also write a
  `<output>.map` sidecar + append the `sourceMappingURL` footer),
  `--embed-sources`, and `--source-map-urls=relative|absolute`. Output is
  byte-for-byte identical to dart-sass for the common cases (selector +
  declaration-name mappings; expanded + compressed). The plain `compile` path
  and stdout output are unchanged. (Deferred for now: declaration-value-start
  mappings, the inline `--embed-source-map` data URI.)

### Changed

- Internal maintainability refactors only (no behaviour change, byte-identical
  output): the `.sass` line scanners, the `is_builtin` name table, and the
  oversized `eval`/`selector`/`parser` files were split into domain modules;
  the string serializers gained a no-escape fast path.

## [0.3.1] - 2026-06-13

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
