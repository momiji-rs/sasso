# Changelog

All notable changes to **sasso** are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

Conformance is tracked separately as a ratchet against the official
[sass-spec](https://github.com/sass/sass-spec) suite — see the
[Conformance](README.md#conformance) section for the current pass rate.

## [Unreleased]

Everything since the initial `0.1.0` crates.io publish. This grew the compiler
from an early vertical slice to roughly **82% of the official sass-spec suite**,
matching current dart-sass (1.100) byte-for-byte on the implemented subset.

### Added

- **`@use` / `@forward` module system** — built-in `sass:*` modules and user
  files, `with` configuration, namespacing, `@forward` prefix/`show`/`hide`,
  dash-insensitive member access, forward conflict resolution, and star
  (`as *`) modules.
- **Indented `.sass` syntax** — a full front-end (`Options::with_syntax`, the
  CLI `--indented` flag, `.sass` extension inference), including cross-syntax
  `@import` of partials by file extension.
- **CSS Color 4 color spaces** — `srgb`/`display-p3`/`lab`/`lch`/`oklab`/
  `oklch`/`xyz` via `color()`, with modern color serialization.
- **`@extend` and `%placeholder`s** — selector weaving with dart-sass-compatible
  trailing-combinator handling and escape canonicalization.
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
  suite run through it (11,445 cases, zero crashes, byte-identical output); the
  rest of the crate is `deny(unsafe_code)`. Still zero runtime dependencies.

### Tooling

- The conformance ratchet pins the sass-spec commit (`spec/SPEC_VERSION.txt`)
  for reproducibility, with a `--latest`/`--canary` drift-detection mode.

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

[Unreleased]: https://github.com/momiji-rs/sasso/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/momiji-rs/sasso/releases/tag/v0.1.0
