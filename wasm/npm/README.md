# sasso

[sasso](https://github.com/momiji-rs/sasso) — a pure-Rust SCSS → CSS compiler
(a dart-sass alternative) — as a tiny, **dependency-free** WebAssembly module.
No wasm-bindgen, no native add-ons: one small `.wasm` plus a hand-written
loader. It mirrors the **dart-sass *modern* JS API**, so it's a drop-in for the
[`sass`](https://www.npmjs.com/package/sass) npm package in build tools.

```bash
npm install sasso
```

## Quick start

```js
import { compileString } from "sasso";

const { css } = compileString(`
  $brand: #2a7ae2;
  .button {
    color: $brand;
    &:hover { color: color.adjust($brand, $lightness: -10%); }
  }
`);
console.log(css);

// compressed output
compileString("a { color: #ffffff }", { style: "compressed" }).css; // a{color:#fff}
```

## API — dart-sass *modern* compatible

```ts
compileString(source, options?)      -> CompileResult
compileStringAsync(source, options?) -> Promise<CompileResult>
compile(path, options?)              -> CompileResult            // Node only (reads the file)
compileAsync(path, options?)         -> Promise<CompileResult>

interface CompileResult { css: string; loadedUrls: URL[]; sourceMap?: object }
```

- `options`: `{ style?: "expanded" | "compressed", sourceMap?: boolean,
  sourceMapIncludeSources?: boolean, loadPaths?: string[], importers?: [...] }`.
  `compileString` also accepts `url?: string | URL` (the source's canonical URL,
  the base for its relative imports) and `syntax?: "scss" | "indented" | "css"`.
- **Imports.** `@use` / `@forward` / `@import` resolve via `loadPaths`, relative
  paths (against `url` / the `compile(path)` file), and custom `importers` —
  dart-sass *modern* `Importer` (`{ canonicalize, load }`) and `FileImporter`
  (`{ findFileUrl }`). All loaded URLs are reported in `loadedUrls`.
- A Sass error throws an `Exception` (an `Error` subclass with `name ===
  "Exception"` and a `sassMessage`).
- `info` is exported for build-tool auto-detection; `initCompiler()` /
  `initAsyncCompiler()` implement the dart-sass Compiler API (Vite uses these).

```js
import { compileString } from "sasso";

// loadPaths + a relative partial
compileString(`@use "theme";`, { url: "file:///app/main.scss", loadPaths: ["scss"] });

// a custom (synchronous) importer
compileString(`@use "virtual:colors" as c; .a { color: c.$brand; }`, {
  importers: [{
    canonicalize: (url) => url === "virtual:colors" ? new URL("virtual:colors") : null,
    load: (u) => u.href === "virtual:colors" ? { contents: "$brand: #2a7ae2;", syntax: "scss" } : null,
  }],
});
```

> **Sync vs. async importers.** The **async** APIs (`compileStringAsync`,
> `compileAsync`, the Compiler API) support **asynchronous** importers — the kind
> sass-loader and Vite inject by default — via an `asyncify`'d wasm module that
> suspends the compile across each `await`. The **synchronous** APIs
> (`compileString`, `compile`) run a faster non-asyncify'd module and therefore
> require synchronous importers; an importer (or `findFileUrl`) that returns a
> `Promise` throws there. Use the async API when your importers are async.

## Custom functions

Define Sass functions in JS with the dart-sass `functions` option. A callback
receives the bound `Value` arguments and returns a `Value` (the full type
system — `SassNumber`, `SassString`, `SassColor`, `SassList`, `SassMap`,
`SassBoolean`, `sassNull` — is exported):

```js
import { compileString, SassNumber, SassColor } from "sasso";

compileString(`.a { width: pow(2, 10) * 1px; color: brand(); }`, {
  functions: {
    "pow($base, $exp)": (args) =>
      new SassNumber(args[0].assertNumber().value ** args[1].assertNumber().value),
    "brand()": () =>
      new SassColor({ space: "oklch", lightness: 0.7, chroma: 0.15, hue: 250 }),
  },
});
```

Custom functions override built-in globals but lose to user `@function`s. A
callback may be **async** — but only under the async APIs (`compileStringAsync`
/ `compileAsync` / the Compiler API); the sync APIs throw on a `Promise`.

> **Migrating from `@momiji-rs/sasso`?** The old `compile(scss) → string` is now
> `compileString(scss).css`, and `compile` takes a **file path** (to match
> dart-sass). See the [changelog](https://github.com/momiji-rs/sasso/releases).

## Drop-in for `sass` in build tools

Both tools drive sasso through the dart-sass *modern* async API and its default
asynchronous importer — both work **zero-config**, including cross-file
`@use`/`@import`.

**webpack / sass-loader** — pass sasso as the implementation:

```js
{
  loader: "sass-loader",
  options: { implementation: require("sasso"), api: "modern" },
}
```

**Vite** — Vite resolves its Sass implementation by the package name `sass`, so
alias it to sasso in `package.json`, then use it as usual:

```json
{ "devDependencies": { "sass": "npm:sasso@^0.7.0" } }
```

```js
// vite.config.js — modern API (Vite's default)
export default { css: { preprocessorOptions: { scss: {} } } };
```

## CLI — `npx sasso`

The package ships a `sasso` bin (pure Node + wasm), with a subset of the
dart-sass `sass` CLI flags:

```bash
npx sasso input.scss                      # compile to stdout
npx sasso input.scss output.css           # write output.css (+ output.css.map)
npx sasso --style=compressed input.scss
npx sasso -I node_modules -I scss main.scss   # add load paths
echo '.a{b:1+2}' | npx sasso --stdin
npx sasso --help
```

Flags: `-s/--style <expanded|compressed>`, `-I/--load-path <dir>` (repeatable),
`--stdin`, `--indented`, `--[no-]source-map` (on by default when writing a file),
`--embed-sources`, `--help`, `--version`.

## Two builds: size vs speed

The default import is the **size-optimized** build (`-Oz`, ~350 KB gzip). For
~2× compile throughput on a larger module (~610 KB gzip), import the
**speed-optimized** build instead — same API, same output:

```js
import { compileString } from "sasso";        // default: smallest module
import { compileString } from "sasso/speed";  // ~2x faster, larger module
```

## Tuning the bump-arena allocator

Both builds bump-allocate a single compile from a reusable arena region, which
is the bulk of the speed advantage. The region defaults to **32 MiB** of wasm
linear memory (grown once on the first compile, then reused). Tune or disable
it with `configure()` — **before the first compile**:

```js
import { configure, compileString } from "sasso/speed";

configure({ arenaMiB: 16 }); // smaller footprint (enough for typical sheets)
// configure({ arenaMiB: 0 }); // disable the arena: lowest memory, slower
compileString(scss);
```

A stylesheet larger than the arena spills to the system allocator with no loss
of correctness — just less speedup. The compile-time default is also settable
when building from source: `SASSO_WASM_ARENA_MB=16 bash wasm/build.sh`.

The loader reads the `.wasm` from disk via `node:fs`, so it targets **Node** (and
bundlers that resolve `node:fs`). For the CLI and the Rust library, see the
[main repository](https://github.com/momiji-rs/sasso).

Licensed under MIT OR Apache-2.0.
