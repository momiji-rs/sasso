# sasso

[![crates.io](https://img.shields.io/crates/v/sasso.svg)](https://crates.io/crates/sasso)
[![docs.rs](https://img.shields.io/docsrs/sasso)](https://docs.rs/sasso)
[![CI](https://github.com/momiji-rs/sasso/actions/workflows/ci.yml/badge.svg)](https://github.com/momiji-rs/sasso/actions/workflows/ci.yml)
[![sass-spec](https://img.shields.io/badge/sass--spec-100%25_of_attempted-brightgreen)](#conformance)
[![dart-sass](https://img.shields.io/badge/dart--sass-1.101_parity-blue)](#conformance)
[![runtime deps](https://img.shields.io/badge/runtime_deps-0-brightgreen)](Cargo.toml)
[![license](https://img.shields.io/crates/l/sasso.svg)](#license)

A pure-Rust **SCSS → CSS compiler** — a from-scratch dart-sass alternative.
Zero runtime dependencies, wasm-friendly, usable as a **library** and a
**CLI**, and designed to match **current** dart-sass byte-for-byte on the
subset it implements.

> Status: v0.x, maturing fast. Compiles real-world SCSS and indented `.sass`
> byte-identically to dart-sass 1.101, and **passes 100% of the *attempted*
> official [sass-spec](https://github.com/sass/sass-spec) suite
> (13,896 / 13,896, zero failures)** — tracked as a ratchet (see
> [Conformance](#conformance) for exactly what that denominator means). What
> remains is real-world breadth and hardening, not spec coverage.

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
- **Source maps (v3)** — byte-exact to dart-sass 1.101, on the library
  (`compile_with_source_map`), the CLI (`--source-map`), and wasm
  (`compile(scss, { sourceMap: true })`)
- Verbatim preservation of CSS functions it doesn't own (`calc`, `var`,
  `clamp`, `translateX`, …)

Since this slice was written the ratchet has added a great deal more —
`@mixin`/`@function`, control flow (`@if`/`@each`/`@for`/`@while`), `@extend`
and `%placeholder`s, a `calc()` engine, the CSS unit system + math functions,
full CSS Color 4 color spaces (`oklch`/`lab`/`color()`…), structured
`@media`/`@supports`, maps, the `@use`/`@forward` module system (built-in
`sass:*` modules + user files), and the indented `.sass` syntax. **The
compiler now passes 100% of the attempted sass-spec suite (13,896 / 13,896,
zero failures)** byte-for-byte against dart-sass 1.101 — 11,405 byte-exact CSS
outputs plus 2,491 error specs it correctly rejects (see
[Conformance](#conformance)).

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

**WebAssembly — npm.** A tiny, dependency-free wasm build that mirrors the
dart-sass *modern* JS API, so it's a drop-in for the `sass` npm package in build
tools (no wasm-bindgen, no native add-ons):

```console
$ npm install sasso
```

```js
import { compileString } from "sasso";
compileString("a { color: #ffffff }", { style: "compressed" }).css; // a{color:#fff}
```

Works as the `sass` implementation in **webpack/sass-loader** (`implementation:
require("sasso"), api: "modern"`) and **Vite** (alias `"sass": "npm:sasso"`).

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
| sass-spec commit | `1b03109a` (dart-sass 1.101.0) |
| Total cases | 13,904 |
| Attempted (excl. 8 dart-sass `:todo`) | 13,896 |
| **Passing** | **13,896 — 100% of attempted · 0 failures** (99.94% of all 13,904) |
| ↳ byte-exact CSS output | 11,405 |
| ↳ error specs correctly rejected | 2,491 |

*Passing* = byte-exact CSS output match **plus** error specs the compiler
correctly rejects — the standard sass-spec conformance metric (the harness
checks that an error spec errors; the error *message* is tracked separately as
a non-gating metric). The 8 excluded cases are tagged `:todo` for **dart-sass
itself** upstream — dart-sass doesn't pass them either; sasso matches
dart-sass's actual behaviour on all 8 regardless.

**Strict input validation, too.** Matching dart-sass means rejecting what
dart-sass rejects, not just reproducing its output. sasso errors — rather than
silently accepting — on an invalid hex literal (`#00000`), out-of-grammar
`rgb()`/`hsl()` arguments, a duplicate `@mixin`/`@function` parameter, a
misplaced `@content`/`@extend`, a style rule or declaration inside a
`@function` body, a malformed `:nth-child()` / empty `:not()` selector, a bad
`@charset` or `@at-root (…)` query, and more — each with dart-sass's exact
message.

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
beating dart-sass by 19–30× end-to-end and leading `grass` (the incumbent Rust
compiler) by ~2.3–2.9×:

| Axis | sasso | grass | dart-sass (bin) | npx sass |
| --- | --- | --- | --- | --- |
| Startup² | **1.8 ms** | 1.8 ms | 139 ms | 495 ms |
| Cold single large file | **11.7 ms** | 26.5 ms | 354 ms | 710 ms |
| Batch (40 files, 1 process) | **49.0 ms** | 136 ms | 916 ms | — |
| Pure compile (startup removed) | **7.4 ms** | 21.3 ms | ~216 ms¹ | — |

¹ derived (cold − startup) — dart-sass has no in-process loop mode. So sasso is
~29× faster than dart-sass on **pure compute**, ~30× on a cold single file, and
~77× on startup; vs `grass` it is ~2.3× cold / ~2.8× batch / ~2.9× pure.

² Startup compiles a 1-rule file, so it sits at the OS process-spawn floor —
sasso and grass measure **identically (1.8 ms)** here (the *mean* is dominated
by scheduler jitter at this sub-2 ms scale). The native library and wasm builds
remove process startup entirely. A
**scoped bump-arena allocator** (one audited `unsafe` module, Miri- and
AddressSanitizer-verified; the rest of the library stays `unsafe`-free) gives a
further ~1.5× by turning each compile's allocations into a pointer bump freed
wholesale at the end. Composite values (strings, lists, maps) are
reference-counted, so reading a `$variable` is an O(1) refcount bump, not a
deep copy — on a large list passed through a call chain without mutation this
cuts both instructions (~7×) and peak memory (~13×). A round of evaluator
allocation trimming — skipping the per-rule selector clone when nothing extends
it, iterating `@each` over the list's shared handle, and dropping redundant
per-declaration copies — shaves a further ~2.8% off pure compile on
representative stylesheets (measured by instructions-retired, since the win is
below wall-clock jitter at this ms scale; byte-identical output). Full
methodology, per-file numbers and the correctness diff are in
[`bench/three_way.md`](bench/three_way.md); run it yourself with
`cd bench && RUNS=12 WARMUP=3 LOOP_N=200 bash scripts/run_bench.sh`.

## WebAssembly

Because the library is zero-dependency and pure `std`, it compiles to
`wasm32-unknown-unknown` and `wasm32-wasip1` out of the box (built in CI). The
deployable `.wasm` cdylib ships in two variants, published to npm as
[`sasso`](https://www.npmjs.com/package/sasso):

| Variant | Build | Over the wire | Compile (large, in Node)³ |
| --- | --- | --- | --- |
| **size** (default) | `opt-level = "z"` + LTO + `panic = "abort"` + `strip` + `wasm-opt -Oz` | **~854 KB / ~356 KB gzip** | ~27 ms |
| **speed** (`sasso/speed`) | `opt-level = 3` + `wasm-opt -O3` | **~1.84 MB / ~637 KB gzip** | ~12 ms |

³ in-process compile of the same large file, Node 22 (best-of-N). The wasm tax
over native `sasso` (7.7 ms) is ~1.5× for the speed build and ~3.5× for the
size build; the wasm build runs without the bump arena. Even so the **speed
build (~12 ms) beats native `grass` (21 ms)** and every dart-sass form a Node
toolchain can run.

A whole modern Sass compiler — `@use`/`@forward`, `@extend`, the calc engine,
CSS Color 4 — in a few hundred KB gzipped, far smaller than shipping the
dart-sass compiler as JavaScript. A browser playground is tracked in the issues.

## Language bindings

sasso ships as a Rust crate and is usable from other languages too. **First-party**
packages are released by this project and pin a published `sasso` crate version;
**community** bindings are maintained in their own repos.

| Language | Package | Maintained by | How |
| --- | --- | --- | --- |
| **Rust** | [`sasso`](https://crates.io/crates/sasso) (crates.io) | First-party | the core library — see [Library usage](#library-usage) above |
| **JavaScript / wasm** | [`sasso`](https://www.npmjs.com/package/sasso) (npm) | First-party | the in-repo [`wasm/`](wasm/) cdylib — see [WebAssembly](#webassembly) above |
| **Ruby** | [`sasso`](https://rubygems.org/gems/sasso) (RubyGems) | First-party | [`momiji-rs/sasso-ruby`](https://github.com/momiji-rs/sasso-ruby) — an in-process native extension (`magnus` + `rb-sys`) around this crate |
| **Python** | [`sasso`](https://pypi.org/project/sasso/) (PyPI) | First-party | [`momiji-rs/sasso-python`](https://github.com/momiji-rs/sasso-python) — `ctypes` over the C ABI; one prebuilt wheel per platform, no build step |
| **Go** | [`sasso-go`](https://github.com/momiji-rs/sasso-go) (`go get`) | First-party | [`momiji-rs/sasso-go`](https://github.com/momiji-rs/sasso-go) — **pure Go, no cgo**: embeds the [wasm build](#webassembly) and runs it with [`wazero`](https://github.com/tetratelabs/wazero), so `CGO_ENABLED=0` and cross-compilation just work. String-in/CSS-out (for file-based importers use the C ABI via cgo, [below](#from-go)) |
| **PHP** | [`shyim/php-sasso`](https://github.com/shyim/php-sasso) (PIE / pecl) | Community ([@shyim](https://github.com/shyim)) | an [ext-php-rs](https://github.com/davidcole1340/ext-php-rs) extension (`Sasso\Compiler`) wrapping this crate in-process; prebuilt for PHP 8.2–8.5 (Linux glibc/musl, macOS) |

**Ruby framework integrations** build on that gem — drop-in Sass for your stack,
compiled **in-process** (no Node, no Dart, no subprocess) and byte-for-byte
identical to dart-sass:

| Framework | Gem | Repo |
| --- | --- | --- |
| **Rails** (Propshaft + Sprockets) | [`sasso-rails`](https://rubygems.org/gems/sasso-rails) | [`momiji-rs/sasso-rails`](https://github.com/momiji-rs/sasso-rails) |
| **Bridgetown** | [`bridgetown-sasso`](https://rubygems.org/gems/bridgetown-sasso) | [`momiji-rs/bridgetown-sasso`](https://github.com/momiji-rs/bridgetown-sasso) |
| **Hanami** (2.1+) | [`hanami-sasso`](https://rubygems.org/gems/hanami-sasso) | [`momiji-rs/hanami-sasso`](https://github.com/momiji-rs/hanami-sasso) |

Each compiles Sass without a Node toolchain — typically ~6–7× faster per compile
than the Node `sass` default (and far faster cold, with no process spawn).

## C ABI — use sasso from any language

Beyond the packages above, sasso ships a **C ABI** ([`ffi/`](ffi/)) so any
language with a C FFI can drive the compiler in-process. Each
[release](https://github.com/momiji-rs/sasso/releases) attaches a per-target
**`sasso-<version>-<target>-c-api.tar.xz`** (`.zip` on Windows) containing the
prebuilt library and the header — the universal substrate every binding sits on:

```
include/sasso.h
lib/  libsasso.a            # static — link it for a self-contained binary, no runtime dep
      libsasso.so|.dylib    # dynamic (Windows: sasso.dll + sasso.dll.lib + sasso.lib)
```

The ABI is two owned calls plus an optional importer callback — see
[`ffi/include/sasso.h`](ffi/include/sasso.h), the contract notes there, and
runnable bindings for **8 languages** under
[`ffi/examples/`](ffi/examples/) (C, Go, Ruby, Swift, Deno, Bun, LuaJIT, C#).

### From Go

Statically link `libsasso.a` via cgo for a single self-contained binary
(no `CGO_ENABLED=0`, but no runtime dependency either):

```go
package main

/*
#cgo CFLAGS: -I./sasso-c-api/include
#cgo LDFLAGS: ./sasso-c-api/lib/libsasso.a
#include <stdlib.h>
#include "sasso.h"
*/
import "C"
import (
	"fmt"
	"unsafe"
)

func main() {
	src := ".a { .b { color: #336699 } }"
	cs := C.CString(src)
	defer C.free(unsafe.Pointer(cs))
	r := C.sasso_compile(cs, C.size_t(len(src)), nil)
	defer C.sasso_result_free(r)
	if r.ok == 0 {
		panic(C.GoString(r.error))
	}
	fmt.Print(C.GoStringN(r.css, C.int(r.css_len)))
}
```

A fuller cgo binding — options, errors, and a custom importer — is in
[`ffi/examples/go/`](ffi/examples/go/). Prefer no cgo? The first-party
**[`sasso-go`](https://github.com/momiji-rs/sasso-go)** package is ready-made:
`go get` it and it embeds the [wasm build](#webassembly) and runs it with
[`wazero`](https://github.com/tetratelabs/wazero), keeping `CGO_ENABLED=0` and
trivial cross-compilation (string-in/CSS-out; use the cgo path above when you
need file-based importers). Or `dlopen` the dynamic lib at runtime with
[`purego`](https://github.com/ebitengine/purego).

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
