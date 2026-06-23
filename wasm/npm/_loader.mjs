// Shared loader core for the `sasso` npm package's wasm variants.
//
// No wasm-bindgen: this marshals UTF-8 through the module's linear memory by
// hand against the raw `sasso_alloc` / `sasso_free` / `sasso_compile2` /
// `sasso_set_arena_bytes` ABI (see ../src/lib.rs).
//
// The public surface mirrors the dart-sass *modern* JS API — `compileString`,
// `compileStringAsync`, path-based `compile` / `compileAsync`, the
// `CompileResult` shape (`{ css, loadedUrls, sourceMap? }`), an `info` string,
// and an `Exception` error class — so `sasso` is a drop-in for the `sass` npm
// package (sass-loader `api: "modern"`, Vite). Both the default (size, `-Oz`)
// and `/speed` (`-O3`) entry points are thin wrappers around
// `makeApi(<their wasm URL>)`. The wasm is instantiated lazily and
// synchronously on first use.
//
// Phase-2 (see ../../docs/NPM_BARE_NAME_PLAN.md): `@use`/`@forward`/`@import` are
// resolved by calling back into JS. wasm has no filesystem, so when the engine
// resolves an import it invokes the host functions installed here, which drive
// the per-compile importer chain in `_importer.mjs` (user importers + a Node-fs
// importer for `loadPaths` / relative loads) and record `loadedUrls`. The engine
// is synchronous, so importer callbacks must be synchronous too.

import { readFileSync, realpathSync } from "node:fs";
import { pathToFileURL, fileURLToPath } from "node:url";
import {
  makeFsImporter,
  normalizeImporter,
  syntaxCode,
  syntaxForPath,
} from "./_importer.mjs";

const encoder = new TextEncoder();
const decoder = new TextDecoder();

// Package version, for `info`. Read once from the published package.json (the
// release workflow syncs its `version` to the git tag). Best-effort: outside a
// Node filesystem (e.g. some bundlers) this degrades to a placeholder.
let VERSION = "0.0.0";
try {
  VERSION = JSON.parse(
    readFileSync(new URL("./package.json", import.meta.url), "utf8"),
  ).version;
} catch {
  // ignore — `info` falls back to the placeholder
}

/**
 * A Sass compilation error. Approximates the dart-sass `Exception`:
 * `instanceof Error`, `name === "Exception"`, plus `sassMessage` (the message
 * without the leading `Error: `). Structured `span` data awaits a later release.
 */
export class Exception extends Error {
  constructor(message) {
    super(message);
    this.name = "Exception";
    this.sassMessage = message.replace(/^Error:\s*/, "");
  }
  toString() {
    return this.message;
  }
}

// `info` masquerades as dart-sass so build tools that gate on the
// implementation *name* accept sasso as a drop-in. sass-loader hard-rejects any
// `info` whose first tab-field isn't `dart-sass`/`sass-embedded`; Vite ignores
// `info` entirely. The version field claims a recent dart-sass we're
// API-compatible with (bump as the modern API evolves); the descriptive field
// honestly discloses the real engine. See ../../docs/NPM_BARE_NAME_PLAN.md.
const DART_SASS_COMPAT = "1.89.0";
export const info = `dart-sass\t${DART_SASS_COMPAT}\t(sasso ${VERSION})\t[Rust]`;

/** Coerce a path or URL to a `file:` (or other-scheme) URL for `loadedUrls`. */
function toFileUrl(pathOrUrl) {
  if (pathOrUrl instanceof URL) return pathOrUrl;
  // Already a URL string (e.g. "file:///...") — keep its scheme.
  if (typeof pathOrUrl === "string" && /^[a-z][a-z0-9+.-]*:/i.test(pathOrUrl)) {
    return new URL(pathOrUrl);
  }
  return pathToFileURL(String(pathOrUrl));
}

/** Coerce a path or `file:` URL to a filesystem path for `readFileSync`. */
function toFsPath(pathOrUrl) {
  if (pathOrUrl instanceof URL) return fileURLToPath(pathOrUrl);
  if (typeof pathOrUrl === "string" && pathOrUrl.startsWith("file:")) {
    return fileURLToPath(pathOrUrl);
  }
  return pathOrUrl;
}

function errMessage(e) {
  return e && e.message ? String(e.message) : String(e);
}

/**
 * Build the per-compile importer chain: user `importers` (in order) then a
 * Node-fs importer for `loadPaths` and relative loads. Tracks the canonical URL
 * that resolved each import (so `load` routes back to the right resolver) and
 * the URLs actually loaded (for `loadedUrls`).
 */
function buildChain(options) {
  const userImporters = (options.importers || []).map(normalizeImporter);
  const fsImporter = makeFsImporter(options.loadPaths);
  const resolvers = [...userImporters, fsImporter];
  const byCanonical = new Map();
  const loaded = [];
  return {
    loaded,
    canonicalize(url, fromImport, containingHref) {
      for (const r of resolvers) {
        const canon = r.canonicalize(url, fromImport, containingHref);
        if (canon != null) {
          byCanonical.set(canon, r);
          return canon;
        }
      }
      return null;
    },
    load(canon) {
      const r = byCanonical.get(canon);
      const res = r ? r.load(canon) : null;
      if (res != null) loaded.push(canon);
      return res;
    },
  };
}

/** Assemble a dart-sass `CompileResult` from a raw compile + the loaded URLs. */
function makeResult(raw, entryHref, loaded) {
  const urls = [];
  const seen = new Set();
  const add = (href) => {
    if (href && !seen.has(href)) {
      seen.add(href);
      urls.push(new URL(href));
    }
  };
  add(entryHref);
  for (const h of loaded) add(h);
  const result = { css: raw.css, loadedUrls: urls };
  if (raw.sourceMap !== undefined) result.sourceMap = raw.sourceMap;
  return result;
}

/**
 * Build the public API bound to one wasm module URL.
 * @param {URL} wasmUrl
 */
export function makeApi(wasmUrl) {
  let ex; // cached wasm exports
  let pendingArenaBytes = null; // applied at instantiation, before any compile
  let activeChain = null; // the importer chain for the in-flight compile

  // --- wasm host functions (the import object) ---
  // The engine calls these (re-entrantly, during a compile) to resolve imports.
  // They read the request from linear memory, run the active importer chain,
  // and `deliver` the result back into a `sasso_alloc` buffer whose (ptr, len)
  // is written into the two out-cells the engine passed. Tri-state return:
  // 1 = handled, 0 = miss, -1 = error (the buffer then holds the message).

  function readStr(ptr, len) {
    if (!ptr || !len) return "";
    // .slice() copies out of wasm memory immediately, before any alloc below
    // can grow (and detach) the backing buffer.
    return decoder.decode(new Uint8Array(ex.memory.buffer, ptr, len).slice());
  }

  function deliver(bytes, outPtrCell, outLenCell) {
    const p = bytes.length ? ex.sasso_alloc(bytes.length) : 0;
    // `sasso_alloc` may have grown memory — take fresh views afterwards.
    if (bytes.length) new Uint8Array(ex.memory.buffer, p, bytes.length).set(bytes);
    const view = new DataView(ex.memory.buffer);
    view.setUint32(outPtrCell, p, true); // *out_ptr  (u32 pointer on wasm32)
    view.setUint32(outLenCell, bytes.length, true); // *out_len (usize == u32)
  }

  function frameLoad(res) {
    // [syntax: u8][smu_present: u8][smu_len: u32 LE][smu bytes][contents bytes]
    const contents = encoder.encode(res.contents);
    const smu = res.sourceMapUrl != null ? encoder.encode(res.sourceMapUrl) : null;
    const frame = new Uint8Array(6 + (smu ? smu.length : 0) + contents.length);
    frame[0] = res.syntax & 0xff;
    frame[1] = smu ? 1 : 0;
    new DataView(frame.buffer).setUint32(2, smu ? smu.length : 0, true);
    let off = 6;
    if (smu) {
      frame.set(smu, off);
      off += smu.length;
    }
    frame.set(contents, off);
    return frame;
  }

  const hostFns = {
    host_canonicalize(urlPtr, urlLen, fromImport, cPtr, cLen, outPtr, outLen) {
      const url = readStr(urlPtr, urlLen);
      const containing = cLen ? readStr(cPtr, cLen) : null;
      try {
        const canon = activeChain
          ? activeChain.canonicalize(url, fromImport !== 0, containing)
          : null;
        if (canon == null) return 0;
        deliver(encoder.encode(canon), outPtr, outLen);
        return 1;
      } catch (e) {
        deliver(encoder.encode(errMessage(e)), outPtr, outLen);
        return -1;
      }
    },
    host_load(canonPtr, canonLen, outPtr, outLen) {
      const canon = readStr(canonPtr, canonLen);
      try {
        const res = activeChain ? activeChain.load(canon) : null;
        if (res == null) return 0;
        deliver(frameLoad(res), outPtr, outLen);
        return 1;
      } catch (e) {
        deliver(encoder.encode(errMessage(e)), outPtr, outLen);
        return -1;
      }
    },
  };

  function instance() {
    if (ex) return ex;
    const bytes = readFileSync(wasmUrl);
    const module = new WebAssembly.Module(bytes);
    ex = new WebAssembly.Instance(module, { sasso_host: hostFns }).exports;
    // Apply a pending arena override before the first compile reserves it.
    if (pendingArenaBytes !== null) ex.sasso_set_arena_bytes(pendingArenaBytes);
    return ex;
  }

  /**
   * Configure the bump-arena allocator. MUST be called before the first
   * compile — the arena region is reserved on first use and then fixed.
   *
   * @param {{ arenaMiB?: number }} [options]
   *   `arenaMiB`: arena reservation in MiB (default 32 at build time). `0`
   *   disables the arena: every allocation forwards to the system allocator
   *   (lower memory footprint, slower). Fractional MiB are rounded down.
   */
  function configure(options = {}) {
    if (typeof options.arenaMiB === "number") {
      pendingArenaBytes = Math.max(0, Math.floor(options.arenaMiB * 1024 * 1024));
      // Instantiate now (if not already) so the override lands before the
      // first compile's first allocation reserves the region.
      instance().sasso_set_arena_bytes(pendingArenaBytes);
    }
  }

  /**
   * Raw compile -> `{ css, sourceMap? }`. Throws {@link Exception} on a Sass
   * error. The caller installs `activeChain` first; the host functions above
   * service any imports the engine hits during the call.
   */
  function compileRaw(scss, { compressed, syntax, url, wantMap, includeSources }) {
    const w = instance();
    const input = encoder.encode(scss);
    const urlBytes = url ? encoder.encode(url) : null;
    const urlLen = urlBytes ? urlBytes.length : 0;

    // Allocate input + url + an 8-byte scratch cell ([outLen: u32][ok: u8]) up
    // front, then write — so a memory grow during alloc can't strand a view.
    const inPtr = input.length ? w.sasso_alloc(input.length) : 0;
    const urlPtr = urlLen ? w.sasso_alloc(urlLen) : 0;
    const scratch = w.sasso_alloc(8);
    if (input.length) new Uint8Array(w.memory.buffer, inPtr, input.length).set(input);
    if (urlLen) new Uint8Array(w.memory.buffer, urlPtr, urlLen).set(urlBytes);

    const outPtr = w.sasso_compile2(
      inPtr,
      input.length,
      compressed ? 1 : 0,
      syntax,
      1, // use_importer: the chain (incl. the fs importer) is always installed
      urlPtr,
      urlLen,
      wantMap ? 1 : 0,
      includeSources ? 1 : 0,
      scratch,
      scratch + 4,
    );

    // Re-read against the current buffer (compile may have grown memory).
    const view = new DataView(w.memory.buffer);
    const outLen = view.getUint32(scratch, true);
    const ok = view.getUint8(scratch + 4);
    const out = new Uint8Array(w.memory.buffer, outPtr, outLen).slice();

    if (input.length) w.sasso_free(inPtr, input.length);
    if (urlLen) w.sasso_free(urlPtr, urlLen);
    w.sasso_free(scratch, 8);
    w.sasso_free(outPtr, outLen);

    if (!ok) throw new Exception(decoder.decode(out));
    if (!wantMap) return { css: decoder.decode(out) };

    // Framed result: [cssLen: u32 LE][css bytes][sourceMap JSON bytes].
    const cssLen = new DataView(out.buffer, out.byteOffset, 4).getUint32(0, true);
    return {
      css: decoder.decode(out.subarray(4, 4 + cssLen)),
      sourceMap: JSON.parse(decoder.decode(out.subarray(4 + cssLen))),
    };
  }

  // Run one compile with its importer chain installed, then assemble the result.
  function runCompile(source, options, entryHref, syntax) {
    const chain = buildChain(options);
    const prev = activeChain;
    activeChain = chain;
    let raw;
    try {
      raw = compileRaw(source, {
        compressed: options.style === "compressed",
        syntax,
        url: entryHref,
        wantMap: !!options.sourceMap,
        includeSources: !!options.sourceMapIncludeSources,
      });
    } finally {
      activeChain = prev;
    }
    return makeResult(raw, entryHref, chain.loaded);
  }

  // --- dart-sass *modern* API ---

  /**
   * Compile an SCSS source string. dart-sass `compileString`.
   *
   * @param {string} source
   * @param {{ style?, sourceMap?, sourceMapIncludeSources?, syntax?, url?,
   *   loadPaths?: string[], importers?: object[] }} [options]
   */
  function compileString(source, options = {}) {
    if (typeof source !== "string") {
      throw new TypeError("compileString(source): source must be a string");
    }
    const entryHref = options.url ? toFileUrl(options.url).href : null;
    return runCompile(source, options, entryHref, syntaxCode(options.syntax));
  }

  /**
   * Compile an SCSS file by path. dart-sass `compile` — **Node only** (reads
   * the file from disk). For an in-memory string, use {@link compileString}.
   *
   * @param {string|URL} path
   * @param {object} [options] same as {@link compileString} (minus `url`)
   */
  function compile(path, options = {}) {
    const fsPath = toFsPath(path);
    const source = readFileSync(fsPath, "utf8");
    // Realpath the entry so its URL matches the canonical URL the fs importer
    // derives for the same file (it realpaths too) — keeps `loadedUrls`
    // consistent and lets a relative self-import dedup against the entry.
    let realPath = fsPath;
    try {
      realPath = realpathSync(fsPath);
    } catch {
      // keep fsPath if realpath fails (e.g. unusual mounts)
    }
    const entryHref = toFileUrl(realPath).href;
    const syntax = options.syntax != null ? syntaxCode(options.syntax) : syntaxForPath(realPath);
    return runCompile(source, options, entryHref, syntax);
  }

  // Async variants: the engine is synchronous, so these just resolve the sync
  // result (a thrown error becomes a rejected promise via the async function).
  async function compileStringAsync(source, options) {
    return compileString(source, options);
  }
  async function compileAsync(path, options) {
    return compile(path, options);
  }

  // dart-sass Compiler API (1.70+). Vite's scss pipeline calls
  // `initAsyncCompiler()` then `compiler.compileStringAsync(...)`. Our engine is
  // stateless, so a "compiler" is just the same entry points with a no-op
  // `dispose()`.
  function initCompiler() {
    return { compile, compileString, dispose() {} };
  }
  async function initAsyncCompiler() {
    return { compileAsync, compileStringAsync, async dispose() {} };
  }

  return {
    compile,
    compileAsync,
    compileString,
    compileStringAsync,
    initCompiler,
    initAsyncCompiler,
    configure,
    info,
  };
}
