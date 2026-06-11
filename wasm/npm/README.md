# @momiji-rs/sasso

[sasso](https://github.com/momiji-rs/sasso) — a pure-Rust SCSS → CSS compiler
(a dart-sass alternative) — as a tiny, **dependency-free** WebAssembly module.
No wasm-bindgen, no native add-ons: one small `.wasm` plus a hand-written
loader, so it runs the same in Node and the browser.

```bash
npm install @momiji-rs/sasso
```

```js
import { compile } from "@momiji-rs/sasso";

const css = compile(`
  $brand: #2a7ae2;
  .button {
    color: $brand;
    &:hover { color: darken($brand, 10%); }
  }
`);
console.log(css);

// compressed output
compile("a { color: #ffffff }", { style: "compressed" }); // a{color:#fff}
```

`compile(scss, options?)` returns the CSS string, or throws an `Error` with the
compiler's message on a Sass error. Options: `{ style?: "expanded" | "compressed" }`.

## Two builds: size vs speed

The default import is the **size-optimized** build (`-Oz`, ~350 KB gzip). For
~2× compile throughput on a larger module (~610 KB gzip), import the
**speed-optimized** build instead — same API, same output:

```js
import { compile } from "@momiji-rs/sasso";        // default: smallest module
import { compile } from "@momiji-rs/sasso/speed";  // ~2x faster, larger module
```

## Tuning the bump-arena allocator

Both builds bump-allocate a single compile from a reusable arena region, which
is the bulk of the speed advantage. The region defaults to **32 MiB** of wasm
linear memory (grown once on the first compile, then reused). Tune or disable
it with `configure()` — **before the first `compile()`**:

```js
import { configure, compile } from "@momiji-rs/sasso/speed";

configure({ arenaMiB: 16 }); // smaller footprint (enough for typical sheets)
// configure({ arenaMiB: 0 }); // disable the arena: lowest memory, slower
compile(scss);
```

A stylesheet larger than the arena spills to the system allocator with no loss
of correctness — just less speedup. The compile-time default is also settable
when building from source: `SASSO_WASM_ARENA_MB=16 bash wasm/build.sh`.

For the CLI and the Rust library, see the
[main repository](https://github.com/momiji-rs/sasso).

Licensed under MIT OR Apache-2.0.
