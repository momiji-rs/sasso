# npm bare-name plan — `sasso` as a drop-in `sass` replacement

> **STATUS: 🚧 in progress (2026-06-23).** We acquired the bare npm name
> [`sasso`](https://www.npmjs.com/package/sasso). This document is the plan for
> migrating the wasm package off the scoped name `@momiji-rs/sasso` and onto the
> bare name, and for evolving its JS API into a drop-in replacement for the
> `sass` (dart-sass) npm package.
>
> **Progress:** hard-cut rename ✅ · **Phase 1 (modern JS API) ✅** · **Phase 2
> (sync importers) ✅** · **Phase 2.5 (async importers via Asyncify) ✅ — full
> zero-config sass-loader + Vite drop-in.** · **Phase 3 (CLI bin) ✅ — `npx
> sasso`.** Phase 4 (custom `functions`) deferred. `sasso@0.7.0` published
> (Phases 1+2); 2.5 + 3 ship on the next `npm-v*` cut.

## Why the bare name matters

`npm install sasso` is not just memorable — it unlocks a positioning the scoped
name could not: **a drop-in replacement for the `sass` npm package.** Build
tools (sass-loader's `api: "modern"`, Vite) call the dart-sass *modern* API —
`compileString` / `compileStringAsync` — and resolve the implementation by
name. If `sasso` mirrors that API, a user writes:

```js
// sass-loader / webpack
{ loader: "sass-loader", options: { implementation: require("sasso"), api: "modern" } }
```

```js
import * as sasso from "sasso";
const { css, sourceMap, loadedUrls } = sasso.compileString(src, {
  style: "compressed", sourceMap: true, loadPaths: ["scss"], importers: [...],
});
```

The brand is now consistent across every ecosystem: crates.io `sasso`,
RubyGems `sasso`, PyPI `sasso`, `sasso-go`, and now npm `sasso`.

## Decisions (locked 2026-06-23)

1. **Scope:** full **drop-in `sass`** — mirror the dart-sass *modern* JS API.
2. **Old scoped package:** **hard cut** — stop publishing `@momiji-rs/sasso`,
   `npm deprecate` it pointing at `sasso`.
3. **CLI:** **yes** — ship a `bin` so `npx sasso input.scss` works.
4. **Version start:** **`0.7.0`** (see version policy below).
5. **OIDC Trusted Publishing** for the bare `sasso` name is configured.

## The pre-existing `sasso` package — takeover handling

The bare name was previously an **unrelated, abandoned 2017 package**:

| | |
|---|---|
| Description | "Simple sass framework" (a Sass *mixin* library, not a compiler) |
| Author / repo | Axel Fuhrmann · `github.com/afuh/sasso` · ISC |
| Versions | `1.0.1` → **`1.2.10`** (14 versions, all 2017, abandoned) |
| `latest` at takeover | `1.2.10` |

### Is there a conflict with starting at `0.7.0`?

- **Functional: no.** npm installs resolve the **`latest` dist-tag**, not the
  numerically-highest version. As long as we publish `0.7.0` and then point
  `latest` at it, every real install path (`npm i sasso`, `@latest`,
  `^0.7.0`) lands on our compiler. The only way to reach the old framework is an
  explicit `sasso@1` / `sasso@1.2.x` pin — vanishingly rare, and deprecated.
- **Cosmetic: yes, harmless.** `npm view sasso versions` will list the old `1.x`
  above our `0.7.0`. This is a display artifact only; dist-tag + deprecation
  neutralize it.

`0.x` is the *correct* semver range while the drop-in API churns through the
phases below (pre-1.0 = breaking changes allowed in minor bumps).

### Release mechanism — decoupled `npm-v*` tags (OIDC, from CI)

The npm version line is **decoupled from the crate**, and so are the release
triggers: the crate / cargo-dist / c-api workflows fire on `v*` tags, and the
npm workflow (`release-wasm.yml`) fires on **`npm-v*`** tags. So an npm cut is:

```bash
git tag npm-v0.7.0 && git push origin npm-v0.7.0
```

That triggers `release-wasm.yml` ONLY: it builds both wasm variants, syncs
`package.json` to `0.7.0` (strips the `npm-v` prefix), and `npm publish
--provenance` via **Trusted Publishing (OIDC)** — no token. A plain publish of a
non-prerelease version sets the `latest` dist-tag to `0.7.0` even though the
abandoned squatter's `1.2.10` is numerically higher (the registry sets `latest`
to whatever was just published; it is not "highest-wins").

> ⚠️ `release.yml` is cargo-dist-generated; its `on.tags` was hand-tightened to
> `v[0-9]+…` so `npm-v*` won't fire it. Re-running `dist init`/`generate` would
> revert that — re-apply the `v`-prefix after any cargo-dist regen.

### Post-publish, once (by the npm account owner — needs `npm login`)

```bash
# Deprecate the entire abandoned old line (closed range — cannot ever touch a
# future version of ours). Keeps it installable, just adds a warning.
npm deprecate "sasso@>=1.0.1 <=1.2.10" \
  "This name now hosts sasso — a pure-Rust SCSS→CSS compiler. Versions <=1.2.10 were an unrelated, abandoned 'Simple sass framework' (github.com/afuh/sasso). Install the compiler: npm i sasso@latest"

# Belt-and-suspenders: confirm `latest` points at our build (idempotent).
npm dist-tag add sasso@0.7.0 latest
```

Do **not** `npm unpublish` the old versions: they are far past the 72h
self-unpublish window, and deprecate is friendlier to any residual dependents.

> ⚠️ **FORWARD-LOOKING RULE — do not reuse `1.0.0`–`1.2.10`.** Those numbers are
> permanently associated with the old framework in npm history and downstream
> caches. When sasso reaches its own "stable" milestone, **start the 1.x line at
> `1.3.0`** (or jump straight to `2.0.0`). Never publish a `sasso@1.0.0`–`1.2.10`.

## Version policy

- npm versioning is **decoupled** from the crate (the Ruby gem already is).
- `0.7.x` = API-alignment phase; breaking API changes ride **minor** bumps.
- Promote to the stable line (`1.3.0`+ / `2.0.0`) only once importers are stable
  and the `functions` story is decided (see Phase 4).

## API target — dart-sass *modern* compatibility

```ts
compile(path, options?)              -> CompileResult            // Node only (needs FS)
compileAsync(path, options?)         -> Promise<CompileResult>
compileString(source, options?)      -> CompileResult
compileStringAsync(source, options?) -> Promise<CompileResult>

interface CompileResult { css: string; loadedUrls: URL[]; sourceMap?: RawSourceMap }
```

- ⚠️ **Name collision fix:** dart-sass `compile(path)` takes a *file path*; our
  current `compile(string)` takes *source*. String compilation moves to
  `compileString`; `compile` becomes path-based (Node-only; browser has no FS).
- async variants = sync result wrapped in a resolved Promise (engine is sync).
- ⚠️ **sass-loader auto-detect** reads `implementation.info`. Export an `info`
  string and document `api: "modern"`; verify against sass-loader + Vite during
  Phase 1.
- Errors approximate the dart-sass `Exception` shape (`message`, span when
  available) so sass-loader can format them.

## Roadmap

- **Phase 0 — claim the name** *(no engineering; account-owner runs the
  commands above)*. First publish = current build under the bare name at
  `0.7.0`.
- **Phase 1 — API alignment (JS only, no wasm changes). ✅ DONE 2026-06-23.**
  Added `compileString` / `compileStringAsync` / path-based `compile` /
  `compileAsync`; `CompileResult` (`{ css, loadedUrls, sourceMap? }`); `info`;
  the `Exception` error class; and `initCompiler` / `initAsyncCompiler` (the
  dart-sass Compiler API). Verified end-to-end against **sass-loader 17 +
  webpack 5** and **Vite 8.1** with the real packed tarball (byte-correct CSS).
  Three drop-in blockers surfaced during verification and are now fixed:
  1. **`exports` needed a `require`/`default` condition.** sass-loader's
     canonical usage is `implementation: require("sasso")`; with only `import`,
     Node threw *"No exports main defined"*. Added `"default": "./sasso.mjs"`
     (covers `require()` via Node's require(esm), needs Node ≥ 20.19 / 22).
  2. **Vite 8 dropped the legacy/non-compiler scss workers** — it now *only*
     calls the dart-sass **Compiler API** (`sass.initAsyncCompiler()` →
     `compiler.compileStringAsync()`), and always passes its own `importers`.
     We added a stateless Compiler wrapper (no-op `dispose()`); plain sheets
     compile, cross-file imports await Phase 2.
  3. **sass-loader 17 hard-gates on `info`** — it throws unless the first
     tab-field is `dart-sass` or `sass-embedded` (`node-sass` is no longer
     accepted). So `info` **masquerades as dart-sass** while disclosing the
     real engine: `dart-sass\t1.89.0\t(sasso <ver>)\t[Rust]`. Bump the claimed
     dart-sass version if a consumer ever version-gates above it. (Vite ignores
     `info`.)
- **Phase 2 — importers / loadPaths (wasm boundary). ✅ DONE 2026-06-23
  (synchronous).** The wasm crate gained `sasso_compile2` + a `HostImporter`
  that bridges the two-phase `sasso::Importer` trait to imported host functions
  `host_canonicalize` / `host_load` (`wasm/src/lib.rs` — the wasm analogue of the
  FFI `FfiImporter`). JS (`wasm/npm/_importer.mjs` + `_loader.mjs`) provides the
  per-compile chain: user `importers` (dart-sass `Importer` **and**
  `FileImporter`), then a Node-fs importer for `loadPaths` + relative loads (a
  faithful port of `src/importer.rs`'s partial/index/import-only precedence), and
  records `loadedUrls`. `compile(path)` / `compileString({url})` resolve relative
  `@use`/`@import` from disk. Tested for both wasm variants in `wasm/test.mjs`;
  CI job `wasm-npm` builds + runs it.

  The sync engine requires synchronous importers (a Promise throws). Verification
  found **both sass-loader (`webpackImporter`) and Vite (`internalImporter`)
  inject *async* importers by default** — closed by Phase 2.5 below.

- **Phase 2.5 — async importers via Asyncify. ✅ DONE 2026-06-23.** A THIRD wasm
  module, `sasso.async.wasm`, is the size build run through
  `wasm-opt --asyncify` (only `host_canonicalize`/`host_load` marked as the
  suspend points). The loader (`_loader.mjs`) now lazily instantiates TWO
  modules: the fast sync module backs `compileString`/`compile`; the asyncify'd
  module backs `compileStringAsync`/`compileAsync`/the Compiler API, driving the
  unwind/rewind loop so a compile SUSPENDS across each `await` of an async
  importer, then resumes. So:
  - **Vite** (drives `initAsyncCompiler().compileStringAsync` with its async
    importer): **zero-config cross-file imports work.** ✅ (Verified vs Vite 8.1.)
  - **sass-loader** (default async `webpackImporter`): **works, no
    `webpackImporter:false` needed.** ✅ (Verified vs sass-loader 17.)
  - The **sync** API still requires sync importers (a Promise throws there) — use
    the async API for async importers.
  - Cost: `sasso.async.wasm` ≈ 1.68 MB / 577 KB gzip (~1.55× the sync size
    build), shipped alongside and **loaded lazily only when an async API is
    used**, so the sync fast path is untouched. Async compiles SERIALIZE (one
    asyncify stack; the loader chains them). The async stack region is 1 MiB.
- **Phase 3 — CLI bin. ✅ DONE 2026-06-23.** `"bin": { "sasso": "./cli.mjs" }`
  (`wasm/npm/cli.mjs`), pure Node + wasm, no deps. Flags mirror the dart-sass
  `sass` CLI: `[input] [output]`, `-s/--style`, `-I/--load-path` (repeatable),
  `--stdin`, `--indented`, `--[no-]source-map` (on by default for file output —
  writes `<out>.map` + a `sourceMappingURL` footer), `--embed-sources`,
  `--help`, `--version`. Sass errors print to stderr and exit non-zero.
  `--watch` and inline `--embed-source-map` are still TODO. Smoke-tested in
  `wasm/test.mjs`.
- **Phase 4 — custom `functions` (deferred / optional).** JS-defined Sass
  functions need the full `Value` type system (SassNumber/String/Color/List/Map).
  Large surface, not on the common sass-loader/Vite path. **v1 ships it as
  unsupported;** revisit before tagging the stable line.

## Hard-cut checklist (rename `@momiji-rs/sasso` → `sasso`)

- [x] `wasm/npm/package.json` — `name`, `version` 0.7.0, `bin`, exports
- [x] `release-wasm.yml` — trusted-publisher comment, package name
- [x] `wasm/Cargo.toml` — header comment
- [x] `wasm/npm/{_loader,sasso,sasso.speed}.mjs` — header comments
- [x] `wasm/npm/README.md` — install + import examples
- [x] root `README.md` — WebAssembly section + Language-bindings table
- [x] downstream binding docs — **verified nothing to change**: none of the
  binding repos (sasso-go/ruby/python/rails, hanami-/bridgetown-sasso) ever
  advertised the npm package, so there were no `@momiji-rs/sasso` references to
  rename (checked via `gh search` + each README, 2026-06-23).
- [x] after first publish: `npm deprecate @momiji-rs/sasso` → "Renamed to
  'sasso'" (all versions; done 2026-06-23). Also overwrote the abandoned
  `sasso@1.0.1–1.2.10` deprecation to redirect to the compiler.
- [ ] `CHANGELOG.md` — keep history; add the rename entry at release time
      (CHANGELOG is the *crate*'s; npm 0.7.0 is decoupled — decide whether to log)
