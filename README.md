# sasso

A pure-Rust **SCSS → CSS compiler** — a from-scratch dart-sass alternative.
Zero runtime dependencies, wasm-friendly, usable as a **library** and a
**CLI**, and designed to match **current** dart-sass byte-for-byte on the
subset it implements.

> Status: early vertical slice (v0.0.1). It already compiles real-world
> SCSS (variables, nesting, `&`, interpolation, unit math, a focused color
> function set, and `@import` partials) byte-identically to dart-sass 1.100.
> The north-star target is **100% of the official
> [sass-spec](https://github.com/sass/sass-spec) suite**, tracked as a
> ratchet (see [Conformance](#conformance)).

## Why another Sass compiler?

`grass` is the incumbent Rust implementation and a strong one (it compiles
Bootstrap/Bulma byte-accurately and is ~2× faster than dart-sass). But it is
pinned to dart-sass **1.54.3** (mid-2022) and predates the CSS Color Level 4
overhaul, so it diverges from current dart-sass on, e.g., fractional color
channels (`rgb(63.75, 127.5, 191.25)` vs rounded hex) and emits hex where
dart-sass now keeps `rgb()`/`hsl()` forms. `sasso` targets **current**
dart-sass exactly, with a span-first parser, a modern color model, and a
zero-dependency, sandbox-friendly core. See
[`docs/GRASS_LANDSCAPE.md`](docs/GRASS_LANDSCAPE.md) for the full analysis.

## Features (this slice)

- `$variables`, lexical scoping, `!default`, `!global`
- Nesting, the `&` parent selector (with selector-list multiplication), and
  combinator normalization (`>`, `+`, `~`)
- `#{}` interpolation in selectors, property names and values
- `//` (stripped) and `/* */` (preserved) comments
- Numbers with units and unit arithmetic (`$pad * 2 → 16px`)
- A full color model with fractional channels + author-spelling preservation
  (`red`, `#336699`, `rgb()`/`hsl()` round-trip unchanged)
- Color functions: `rgb`/`rgba`/`hsl`/`hsla`/`mix`/`lighten`/`darken`/
  `percentage` (+ `red`/`green`/`blue`/`alpha`)
- `@import` partial inlining through a pluggable [`Importer`] (CSS imports
  pass through)
- `expanded` and `compressed` output styles
- Verbatim preservation of CSS functions it doesn't own (`calc`, `var`,
  `clamp`, `translateX`, …)

Since this slice was written the ratchet has added a great deal more —
`@mixin`/`@function`, control flow (`@if`/`@each`/`@for`/`@while`), `@extend`
and `%placeholder`s, a `calc()` engine, the CSS unit system + math functions,
full CSS Color 4 color spaces (`oklch`/`lab`/`color()`…), structured
`@media`/`@supports`, maps, the `@use`/`@forward` module system (built-in
`sass:*` modules + user files), and the indented `.sass` syntax. **82% of the
official sass-spec suite now passes** (see [Conformance](#conformance)); what
remains is a long tail of byte-level edge cases.

## Install

**CLI — prebuilt binaries.** Every release ships static binaries for Linux
(gnu + musl), macOS and Windows (x86_64 / aarch64), built with
[cargo-dist](https://github.com/axodotdev/cargo-dist). Grab one from the
[Releases](https://github.com/momiji-rs/sasso/releases) page, or:

```console
$ curl -fsSL https://github.com/momiji-rs/sasso/releases/latest/download/sasso-installer.sh | sh   # Linux/macOS
$ cargo binstall sasso        # fetch the prebuilt binary
$ cargo install sasso         # build from source (needs a Rust toolchain)
```

**Library — crates.io.**

```console
$ cargo add sasso
```

**WebAssembly — npm.** A tiny, dependency-free wasm build for JS build tools
and the browser (no wasm-bindgen, no native add-ons):

```console
$ npm install @momiji-rs/sasso
```

```js
import { compile } from "@momiji-rs/sasso";
compile("a { color: #ffffff }", { style: "compressed" }); // a{color:#fff}
```

## Library usage

```rust
use sasso::{compile, Options, OutputStyle};

let scss = "$c: #336699; .a { color: $c; &:hover { color: lighten($c, 10%); } }";
let css = compile(scss, &Options::default()).unwrap();
assert!(css.contains("a:hover"));

// Minified:
let min = compile(scss, &Options::default().with_style(OutputStyle::Compressed)).unwrap();
```

`@import` resolution is controlled by an [`Importer`] you supply, so file
access stays on your side of any sandbox:

```rust
use sasso::{compile, Importer, Options};

struct MyFs;
impl Importer for MyFs {
    fn resolve(&self, path: &str) -> Option<String> {
        std::fs::read_to_string(format!("scss/_{path}.scss")).ok()
    }
}
let css = compile("@import \"base\";", &Options::default().with_importer(&MyFs)).unwrap();
```

A ready-made [`FsImporter`] is provided for standalone/CLI use.

## CLI usage

```console
$ cargo install --path .            # installs the `sasso` binary
$ sasso input.scss              # CSS to stdout (expanded)
$ sasso --style=compressed input.scss
$ sasso -I scss/ main.scss      # add @import load paths
$ echo '.a{color:red}' | sasso --stdin
```

## Conformance

The official sass-spec suite is the parity oracle. The harness in
[`spec/`](spec/) runs the compiler against every spec case and reports a
pass rate; we ratchet it upward over time.

| Metric | Value |
| --- | --- |
| sass-spec commit | `c6ac9a3` (dart-sass 1.100.0) |
| Total cases | 13,904 |
| Attempted (excl. a few :todo) | 13,896 |
| **Passing** | **11,445 (82.4% of attempted, 82.3% of all 13,904)** |

Run it yourself:

```console
$ spec/fetch.sh                                      # clone the suite
$ cargo build --release
$ SASS_BIN=target/release/sasso python3 spec/run_spec.py
```

## Performance

`sasso` is a native, in-process library — no subprocess, no Node, no Dart VM —
so startup is effectively free, which dominates when a build compiles many
files. On an Apple M2 Max it is the **fastest** of the three engines measured,
beating dart-sass by 10–19× end-to-end and leading `grass` (the incumbent Rust
compiler) by ~1.5×:

| Axis | sasso | grass | dart-sass (bin) | npx sass |
| --- | --- | --- | --- | --- |
| Startup | **1.7 ms** | 1.8 ms | 142 ms | 567 ms |
| Cold single large file | **18.9 ms** | 27.8 ms | 363 ms | 1.05 s |
| Batch (40 files, 1 process) | **90.9 ms** | 139 ms | 943 ms | — |
| Pure compile (startup removed) | **14.0 ms** | 21.8 ms | ~221 ms¹ | — |

¹ derived (cold − startup) — dart-sass has no in-process loop mode. So sasso is
~16× faster than dart-sass on **pure compute**, ~19× on a cold single file, and
~82× on startup; vs `grass` it is ~1.5× across cold/batch/pure-throughput. Full
methodology, per-file numbers and the correctness diff are in
[`bench/three_way.md`](bench/three_way.md); run it yourself with
`cd bench && RUNS=12 WARMUP=3 LOOP_N=200 bash scripts/run_bench.sh`.

## WebAssembly

Because the library is zero-dependency and pure `std`, it compiles to
`wasm32-unknown-unknown` and `wasm32-wasip1` out of the box (built in CI).
A whole SCSS compiler stays small: the deployable `.wasm` cdylib
(`opt-level = "z"` + LTO + `panic = "abort"` + `strip` + `wasm-opt -Oz`) is
**~447 KB / ~184 KB gzip** over the wire — published to npm as
[`@momiji-rs/sasso`](https://www.npmjs.com/package/@momiji-rs/sasso), an order
of magnitude smaller than shipping dart-sass as JavaScript. A browser
playground is tracked in the issues.

## Testing & coverage

```console
$ cargo test                                  # unit + integration + doctests (offline)
$ SASSO_PARITY=1 cargo test --test parity # live diff vs dart-sass (needs `npx sass`)
$ cargo llvm-cov --workspace                  # coverage report
$ cargo clippy --all-targets -- -D warnings
$ cargo fmt --check
```

## Changelog

Notable changes are recorded in [`CHANGELOG.md`](CHANGELOG.md).

## Code of Conduct

As a Sass implementation, sasso adopts the
[Sass Community Guidelines](https://sass-lang.com/community-guidelines/) — see
[`CODE_OF_CONDUCT.md`](CODE_OF_CONDUCT.md).

## License

Licensed under either of [Apache-2.0](LICENSE-APACHE) or [MIT](LICENSE-MIT)
at your option.

[`Importer`]: https://docs.rs/sasso
[`FsImporter`]: https://docs.rs/sasso
