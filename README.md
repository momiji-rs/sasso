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

Not yet implemented: `@mixin`/`@function`, control flow (`@if`/`@each`/
`@for`/`@while`), `@extend`/placeholders, the `@use`/`@forward` module
system, and the indented `.sass` syntax. These are the next ratchet steps.

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
| Attempted (excl. `@use`/`@forward`/`@extend`/indented) | 4,528 |
| **Passing** | **1,110 (24.5% of attempted)** |

Run it yourself:

```console
$ spec/fetch.sh                                      # clone the suite
$ cargo build --release
$ SASS_BIN=target/release/sasso python3 spec/run_spec.py
```

## Performance

`sasso` is a native, in-process library — no subprocess, no Node, no
Dart VM. See [`bench/`](bench/) for the methodology. In-process startup is
effectively free (vs ~140 ms for the dart-sass binary and ~1 s for
`npx sass`), which matters when a build compiles many files.

## Testing & coverage

```console
$ cargo test                                  # unit + integration + doctests (offline)
$ SASSO_PARITY=1 cargo test --test parity # live diff vs dart-sass (needs `npx sass`)
$ cargo llvm-cov --workspace                  # coverage report
$ cargo clippy --all-targets -- -D warnings
$ cargo fmt --check
```

## License

Licensed under either of [Apache-2.0](LICENSE-APACHE) or [MIT](LICENSE-MIT)
at your option.

[`Importer`]: https://docs.rs/sasso
[`FsImporter`]: https://docs.rs/sasso
