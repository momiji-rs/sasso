// Shared loader core for the `sasso` npm package's wasm variants.
//
// No wasm-bindgen: this marshals UTF-8 through the module's linear memory by
// hand against the raw `sasso_alloc` / `sasso_free` / `sasso_compile2` /
// `sasso_set_arena_bytes` ABI (see ../src/lib.rs).
//
// The public surface mirrors the dart-sass *modern* JS API — `compileString`,
// `compileStringAsync`, path-based `compile` / `compileAsync`, the
// `CompileResult` shape (`{ css, loadedUrls, sourceMap? }`), an `info` string,
// the Compiler API, and an `Exception` error class — so `sasso` is a drop-in for
// the `sass` npm package (sass-loader `api: "modern"`, Vite).
//
// TWO wasm modules (each lazily instantiated):
//   • a SYNC module (fast) drives the synchronous APIs (`compileString`,
//     `compile`). A custom importer that returns a Promise is rejected here —
//     the engine cannot await.
//   • an ASYNCIFY'd module (built with `wasm-opt --asyncify`) drives the async
//     APIs (`compileStringAsync`, `compileAsync`, the Compiler API's async
//     methods). It can SUSPEND the whole compile across an `await`, so
//     asynchronous importers — the kind sass-loader and Vite inject by default —
//     work. This is the dart-sass async story without a native binary.
//
// Importers (`@use`/`@forward`/`@import`) are resolved by calling back into JS
// (wasm has no filesystem): the host functions below drive the per-compile
// importer chain in `_importer.mjs` (user importers + a Node-fs importer for
// `loadPaths`/relative loads) and record `loadedUrls`.

import { readFileSync, realpathSync } from "node:fs";
import { pathToFileURL, fileURLToPath } from "node:url";
import {
  makeFsImporter,
  normalizeImporter,
  syntaxCode,
  syntaxForPath,
} from "./_importer.mjs";
import { deserializeArgs, serializeValue, setEngine } from "./_value.mjs";

const encoder = new TextEncoder();
const decoder = new TextDecoder();
// The default decoder strips a leading BOM; the compressed-output charset prefix
// IS a U+FEFF BOM, so decode CSS with a BOM-preserving decoder to keep it (dart).
const cssDecoder = new TextDecoder("utf-8", { ignoreBOM: true });

// Asyncify runtime states (from `asyncify_get_state`).
const ASYNCIFY_UNWINDING = 1;
const ASYNCIFY_REWINDING = 2;
// Asyncify stack-state region: [curr: u32][end: u32] header + this much stack.
// Sized for a deep compile suspended at a nested import; a spill would corrupt,
// so we reserve generously (one allocation per async instance).
const ASYNCIFY_STACK_BYTES = 1 << 20; // 1 MiB

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

/** dart-sass `Logger` namespace. `Logger.silent` discards all warnings/debugs. */
export const Logger = {
  silent: { warn() {}, debug() {} },
};

/** Coerce a path or URL to a `file:` (or other-scheme) URL for `loadedUrls`. */
function toFileUrl(pathOrUrl) {
  if (pathOrUrl instanceof URL) return pathOrUrl;
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

/** Frame an importer load result for the wasm: [syntax][smu?][smuLen][smu][contents]. */
function frameLoad(res) {
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

/**
 * Build the per-compile importer chain. `async` selects the asyncify path
 * (importer callbacks may return Promises and are awaited) vs. the sync path
 * (a Promise is a hard error). Tracks the resolver that owns each canonical URL
 * (so `load` routes back) and the URLs actually loaded (for `loadedUrls`).
 */
function buildChain(options, async) {
  const userImporters = (options.importers || []).map((i) => normalizeImporter(i, async));
  const fsImporter = makeFsImporter(options.loadPaths);
  const resolvers = [...userImporters, fsImporter];
  const byCanonical = new Map();
  const loaded = [];
  if (async) {
    return {
      loaded,
      async canonicalize(url, fromImport, containing) {
        for (const r of resolvers) {
          const canon = await r.canonicalize(url, fromImport, containing);
          if (canon != null) {
            byCanonical.set(canon, r);
            return canon;
          }
        }
        return null;
      },
      async load(canon) {
        const r = byCanonical.get(canon);
        const res = r ? await r.load(canon) : null;
        if (res != null) loaded.push(canon);
        return res;
      },
    };
  }
  return {
    loaded,
    canonicalize(url, fromImport, containing) {
      for (const r of resolvers) {
        const canon = r.canonicalize(url, fromImport, containing);
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
 * Build the public API bound to one SYNC wasm URL and one ASYNCIFY'd wasm URL
 * (both lazily instantiated; the async module is loaded only when an async API
 * is first used).
 * @param {URL} syncWasmUrl
 * @param {URL} asyncWasmUrl
 */
export function makeApi(syncWasmUrl, asyncWasmUrl) {
  let pendingArenaBytes = null; // applied at instantiation, before any compile

  // --- shared marshalling against an instance's exports ---
  function readStr(ex, ptr, len) {
    if (!ptr || !len) return "";
    // .slice() copies out of wasm memory immediately, before any alloc below
    // can grow (and detach) the backing buffer.
    return decoder.decode(new Uint8Array(ex.memory.buffer, ptr, len).slice());
  }
  function deliver(ex, bytes, outPtrCell, outLenCell) {
    const p = bytes.length ? ex.sasso_alloc(bytes.length) : 0;
    // `sasso_alloc` may have grown memory — take fresh views afterwards.
    if (bytes.length) new Uint8Array(ex.memory.buffer, p, bytes.length).set(bytes);
    const view = new DataView(ex.memory.buffer);
    view.setUint32(outPtrCell, p, true); // *out_ptr  (u32 pointer on wasm32)
    view.setUint32(outLenCell, bytes.length, true); // *out_len (usize == u32)
  }

  // Read the result buffer after a sasso_compile2 call, free scratch, and throw
  // on a Sass error. Shared by the sync and async drivers.
  function readResult(w, outPtr, scratch, inPtr, inLen, urlPtr, urlLen, wantMap) {
    const view = new DataView(w.memory.buffer);
    const outLen = view.getUint32(scratch, true);
    const ok = view.getUint8(scratch + 4);
    const out = new Uint8Array(w.memory.buffer, outPtr, outLen).slice();
    if (inLen) w.sasso_free(inPtr, inLen);
    if (urlLen) w.sasso_free(urlPtr, urlLen);
    w.sasso_free(scratch, 8);
    w.sasso_free(outPtr, outLen);
    if (!ok) throw new Exception(decoder.decode(out));
    if (!wantMap) return { css: cssDecoder.decode(out) };
    const cssLen = new DataView(out.buffer, out.byteOffset, 4).getUint32(0, true);
    return {
      css: cssDecoder.decode(out.subarray(4, 4 + cssLen)),
      sourceMap: JSON.parse(decoder.decode(out.subarray(4 + cssLen))),
    };
  }

  // Allocate input + url + scratch and write them in. Returns the handles the
  // driver passes to sasso_compile2 and on to readResult.
  function marshalIn(w, scss, opts) {
    const input = encoder.encode(scss);
    const urlBytes = opts.url ? encoder.encode(opts.url) : null;
    const urlLen = urlBytes ? urlBytes.length : 0;
    const inPtr = input.length ? w.sasso_alloc(input.length) : 0;
    const urlPtr = urlLen ? w.sasso_alloc(urlLen) : 0;
    const scratch = w.sasso_alloc(8);
    if (input.length) new Uint8Array(w.memory.buffer, inPtr, input.length).set(input);
    if (urlLen) new Uint8Array(w.memory.buffer, urlPtr, urlLen).set(urlBytes);
    return { inPtr, inLen: input.length, urlPtr, urlLen, scratch };
  }

  function callCompile2(w, m, opts) {
    return w.sasso_compile2(
      m.inPtr, m.inLen, opts.compressed ? 1 : 0, opts.syntax, 1,
      m.urlPtr, m.urlLen, opts.wantMap ? 1 : 0, opts.includeSources ? 1 : 0, opts.charset ? 1 : 0,
      m.scratch, m.scratch + 4,
    );
  }

  // Register `options.functions` on a freshly-cleared instance registry and
  // return the callbacks array (index -> user function), matching the indices
  // `sasso_register_function` assigns.
  function registerFunctions(w, options) {
    w.sasso_clear_functions();
    const callbacks = [];
    if (options.functions) {
      for (const [sig, fn] of Object.entries(options.functions)) {
        const b = encoder.encode(sig);
        const p = b.length ? w.sasso_alloc(b.length) : 0;
        if (b.length) new Uint8Array(w.memory.buffer, p, b.length).set(b);
        w.sasso_register_function(p, b.length);
        if (b.length) w.sasso_free(p, b.length);
        callbacks.push(fn);
      }
    }
    return callbacks;
  }

  // ============================ SYNC instance ============================
  let syncEx; // cached sync exports
  let syncChain = null; // importer chain for the in-flight sync compile
  let syncFunctions = []; // index -> user custom function for the in-flight compile
  let syncLogger = null; // dart-sass `logger` for the in-flight sync compile

  // Decode a host_warn buffer (see wasm/src/lib.rs make_host_warn).
  function decodeWarn(ex, ptr, len) {
    const view = new DataView(ex.memory.buffer, ptr, len);
    let o = 0;
    const kind = view.getUint8(o);
    o += 1;
    const deprecation = view.getUint8(o) !== 0;
    o += 1;
    const line = view.getUint32(o, true);
    o += 4;
    const rs = () => {
      const n = view.getUint32(o, true);
      o += 4;
      const s = readStr(ex, ptr + o, n);
      o += n;
      return s;
    };
    const deprecationId = rs();
    const url = rs();
    const message = rs();
    const formatted = rs();
    return { kind, deprecation, line, deprecationId, url, message, formatted };
  }
  // A best-effort SourceSpan (full span is a separate item); enough for the
  // common `span.url` / `span.start.line` reads. Lines are 0-based, like dart.
  function spanOf(ev) {
    if (!ev.url && !ev.line) return undefined;
    const start = { line: ev.line > 0 ? ev.line - 1 : 0, column: 0 };
    return { url: ev.url || undefined, start, end: start, text: "", context: "" };
  }
  // Print the formatted block to stderr (matching native sasso/dart output).
  function defaultLog(ev) {
    if (typeof process !== "undefined" && process.stderr) process.stderr.write(ev.formatted + "\n");
    else console.error(ev.formatted);
  }
  // Route a decoded diagnostic to the in-flight `logger`, else print it.
  function dispatchWarn(logger, ev) {
    if (ev.kind === 1) {
      if (logger && typeof logger.debug === "function") return logger.debug(ev.message, { span: spanOf(ev) });
    } else if (logger && typeof logger.warn === "function") {
      return logger.warn(ev.message, {
        deprecation: ev.deprecation,
        deprecationType: ev.deprecationId || undefined,
        span: spanOf(ev),
        stack: undefined,
      });
    }
    defaultLog(ev);
  }

  const syncHost = {
    host_canonicalize(uPtr, uLen, fromImport, cPtr, cLen, outPtr, outLen) {
      const url = readStr(syncEx, uPtr, uLen);
      const containing = cLen ? readStr(syncEx, cPtr, cLen) : null;
      try {
        const canon = syncChain ? syncChain.canonicalize(url, fromImport !== 0, containing) : null;
        if (canon == null) return 0;
        deliver(syncEx, encoder.encode(canon), outPtr, outLen);
        return 1;
      } catch (e) {
        deliver(syncEx, encoder.encode(errMessage(e)), outPtr, outLen);
        return -1;
      }
    },
    host_load(kPtr, kLen, outPtr, outLen) {
      const canon = readStr(syncEx, kPtr, kLen);
      try {
        const res = syncChain ? syncChain.load(canon) : null;
        if (res == null) return 0;
        deliver(syncEx, frameLoad(res), outPtr, outLen);
        return 1;
      } catch (e) {
        deliver(syncEx, encoder.encode(errMessage(e)), outPtr, outLen);
        return -1;
      }
    },
    host_call_function(index, argsPtr, argsLen, outPtr, outLen) {
      try {
        const argBytes = argsLen ? new Uint8Array(syncEx.memory.buffer, argsPtr, argsLen).slice() : new Uint8Array(0);
        const fn = syncFunctions[index];
        if (!fn) throw new Error(`sasso: custom function #${index} is not registered`);
        const r = fn(deserializeArgs(argBytes));
        if (r && typeof r.then === "function") {
          throw new Error("sasso: asynchronous custom functions require compileStringAsync / compileAsync");
        }
        if (r == null) throw new Error("sasso: a custom function returned no value");
        deliver(syncEx, serializeValue(r), outPtr, outLen);
        return 1;
      } catch (e) {
        deliver(syncEx, encoder.encode(errMessage(e)), outPtr, outLen);
        return -1;
      }
    },
    host_warn(bufPtr, bufLen) {
      try {
        dispatchWarn(syncLogger, decodeWarn(syncEx, bufPtr, bufLen));
      } catch {
        // A logging failure must never fail the compile.
      }
    },
  };

  function syncInstance() {
    if (syncEx) return syncEx;
    const module = new WebAssembly.Module(readFileSync(syncWasmUrl));
    syncEx = new WebAssembly.Instance(module, { sasso_host: syncHost }).exports;
    if (pendingArenaBytes !== null) syncEx.sasso_set_arena_bytes(pendingArenaBytes);
    return syncEx;
  }

  function compileRawSync(scss, opts) {
    const w = syncInstance();
    const m = marshalIn(w, scss, opts);
    const outPtr = callCompile2(w, m, opts);
    return readResult(w, outPtr, m.scratch, m.inPtr, m.inLen, m.urlPtr, m.urlLen, opts.wantMap);
  }

  // Engine for routed Value methods (SassNumber.convert, SassColor.toSpace, …):
  // runs `sasso_value_op` on the sync instance, independent of any compile, so
  // the methods work standalone and re-entrantly during a compile. Returns the
  // result bytes; throws the engine's message on failure.
  function valueOpEngine(op, argsBytes) {
    const w = syncInstance();
    const inPtr = argsBytes.length ? w.sasso_alloc(argsBytes.length) : 0;
    const scratch = w.sasso_alloc(8);
    if (argsBytes.length) new Uint8Array(w.memory.buffer, inPtr, argsBytes.length).set(argsBytes);
    const outPtr = w.sasso_value_op(op, inPtr, argsBytes.length, scratch, scratch + 4);
    const view = new DataView(w.memory.buffer);
    const outLen = view.getUint32(scratch, true);
    const ok = view.getUint8(scratch + 4);
    const out = new Uint8Array(w.memory.buffer, outPtr, outLen).slice();
    if (argsBytes.length) w.sasso_free(inPtr, argsBytes.length);
    w.sasso_free(scratch, 8);
    w.sasso_free(outPtr, outLen);
    if (!ok) throw new Error(decoder.decode(out));
    return out;
  }
  setEngine(valueOpEngine);

  // ========================== ASYNCIFY instance ==========================
  let asyncEx; // cached asyncify'd exports
  let asyncData = 0; // asyncify stack-state struct pointer
  let asyncReady = false; // module actually carries the asyncify_* exports
  let asyncChain = null; // importer chain for the in-flight async compile
  let asyncFunctions = []; // index -> user custom function for the in-flight compile
  let asyncLogger = null; // dart-sass `logger` for the in-flight async compile
  let pendingDelivery = null; // a Promise<{rc, bytes}>, then its resolved value
  let asyncLock = Promise.resolve(); // serialize async compiles (one asyncify stack)

  // On a normal call, kick off the (possibly async) chain lookup, suspend the
  // engine, and stash a Promise<{rc, bytes}>. On the rewind re-entry, stop the
  // rewind and hand that delivery back to the engine.
  function asyncHostFn(lookup, encodeOk) {
    return (...args) => {
      const outPtr = args[args.length - 2];
      const outLen = args[args.length - 1];
      if (asyncEx.asyncify_get_state() === ASYNCIFY_REWINDING) {
        asyncEx.asyncify_stop_rewind();
        const d = pendingDelivery;
        pendingDelivery = null;
        if (!d || d.rc === 0) return 0;
        deliver(asyncEx, d.bytes, outPtr, outLen);
        return d.rc;
      }
      pendingDelivery = Promise.resolve()
        .then(() => lookup(args))
        .then((v) => (v == null ? { rc: 0 } : { rc: 1, bytes: encodeOk(v) }))
        .catch((e) => ({ rc: -1, bytes: encoder.encode(errMessage(e)) }));
      asyncEx.asyncify_start_unwind(asyncData);
      return 0; // ignored while unwinding
    };
  }

  const asyncHost = {
    host_canonicalize: asyncHostFn(
      (args) => {
        const url = readStr(asyncEx, args[0], args[1]);
        const containing = args[4] ? readStr(asyncEx, args[3], args[4]) : null;
        return asyncChain.canonicalize(url, args[2] !== 0, containing);
      },
      (canon) => encoder.encode(canon),
    ),
    host_load: asyncHostFn(
      (args) => asyncChain.load(readStr(asyncEx, args[0], args[1])),
      (res) => frameLoad(res),
    ),
    host_call_function: asyncHostFn(
      (args) => {
        const argBytes = args[2] ? new Uint8Array(asyncEx.memory.buffer, args[1], args[2]).slice() : new Uint8Array(0);
        const fn = asyncFunctions[args[0]];
        if (!fn) throw new Error(`sasso: custom function #${args[0]} is not registered`);
        // Resolve sync or async; reject on a null return (functions must return).
        return Promise.resolve(fn(deserializeArgs(argBytes))).then((v) => {
          if (v == null) throw new Error("sasso: a custom function returned no value");
          return v;
        });
      },
      (v) => serializeValue(v),
    ),
    host_warn(bufPtr, bufLen) {
      try {
        dispatchWarn(asyncLogger, decodeWarn(asyncEx, bufPtr, bufLen));
      } catch {
        // A logging failure must never fail the compile.
      }
    },
  };

  function asyncInstance() {
    if (asyncEx) return asyncEx;
    const module = new WebAssembly.Module(readFileSync(asyncWasmUrl));
    asyncEx = new WebAssembly.Instance(module, { sasso_host: asyncHost }).exports;
    if (pendingArenaBytes !== null) asyncEx.sasso_set_arena_bytes(pendingArenaBytes);
    asyncReady = typeof asyncEx.asyncify_start_unwind === "function";
    if (asyncReady) {
      asyncData = asyncEx.sasso_alloc(8 + ASYNCIFY_STACK_BYTES);
      const v = new DataView(asyncEx.memory.buffer);
      v.setUint32(asyncData, asyncData + 8, true); // current stack pointer
      v.setUint32(asyncData + 4, asyncData + 8 + ASYNCIFY_STACK_BYTES, true); // end
    }
    return asyncEx;
  }

  // Drives the asyncify unwind/rewind loop so async importers can suspend.
  async function compileRawAsync(scss, opts) {
    const w = asyncInstance();
    const m = marshalIn(w, scss, opts);
    let outPtr = callCompile2(w, m, opts);
    if (asyncReady) {
      while (w.asyncify_get_state() === ASYNCIFY_UNWINDING) {
        w.asyncify_stop_unwind();
        pendingDelivery = await pendingDelivery; // Promise<{rc,bytes}> -> value
        w.asyncify_start_rewind(asyncData);
        outPtr = callCompile2(w, m, opts);
      }
    }
    return readResult(w, outPtr, m.scratch, m.inPtr, m.inLen, m.urlPtr, m.urlLen, opts.wantMap);
  }

  function rawOpts(options, syntax) {
    return {
      compressed: options.style === "compressed",
      syntax,
      url: options.url ? toFileUrl(options.url).href : null,
      wantMap: !!options.sourceMap,
      includeSources: !!options.sourceMapIncludeSources,
      charset: options.charset !== false, // dart-sass default: true
    };
  }

  // ------------------------- configure -------------------------
  function configure(options = {}) {
    if (typeof options.arenaMiB === "number") {
      pendingArenaBytes = Math.max(0, Math.floor(options.arenaMiB * 1024 * 1024));
      // Apply to whichever instances already exist (and stash for the rest).
      if (syncEx) syncEx.sasso_set_arena_bytes(pendingArenaBytes);
      if (asyncEx) asyncEx.sasso_set_arena_bytes(pendingArenaBytes);
      if (!syncEx && !asyncEx) syncInstance().sasso_set_arena_bytes(pendingArenaBytes);
    }
  }

  // --------------------- dart-sass *modern* API ---------------------

  function compileString(source, options = {}) {
    if (typeof source !== "string") {
      throw new TypeError("compileString(source): source must be a string");
    }
    const chain = buildChain(options, false);
    const callbacks = registerFunctions(syncInstance(), options);
    const prevChain = syncChain;
    const prevFns = syncFunctions;
    const prevLog = syncLogger;
    syncChain = chain;
    syncFunctions = callbacks;
    syncLogger = options.logger ?? null;
    let raw;
    try {
      raw = compileRawSync(source, rawOpts(options, syntaxCode(options.syntax)));
    } finally {
      syncChain = prevChain;
      syncFunctions = prevFns;
      syncLogger = prevLog;
      syncEx.sasso_clear_functions();
    }
    return makeResult(raw, options.url ? toFileUrl(options.url).href : null, chain.loaded);
  }

  function compile(path, options = {}) {
    const fsPath = toFsPath(path);
    const source = readFileSync(fsPath, "utf8");
    let realPath = fsPath;
    try {
      realPath = realpathSync(fsPath);
    } catch {
      // keep fsPath if realpath fails
    }
    const entryHref = toFileUrl(realPath).href;
    const syntax = options.syntax != null ? syntaxCode(options.syntax) : syntaxForPath(realPath);
    const chain = buildChain(options, false);
    const callbacks = registerFunctions(syncInstance(), options);
    const prevChain = syncChain;
    const prevFns = syncFunctions;
    const prevLog = syncLogger;
    syncChain = chain;
    syncFunctions = callbacks;
    syncLogger = options.logger ?? null;
    let raw;
    try {
      raw = compileRawSync(source, { ...rawOpts(options, syntax), url: entryHref });
    } finally {
      syncChain = prevChain;
      syncFunctions = prevFns;
      syncLogger = prevLog;
      syncEx.sasso_clear_functions();
    }
    return makeResult(raw, entryHref, chain.loaded);
  }

  // Async APIs run on the asyncify'd module and SERIALIZE (one asyncify stack).
  function runAsyncLocked(task) {
    const result = asyncLock.then(task, task);
    asyncLock = result.then(
      () => {},
      () => {},
    );
    return result;
  }

  function compileStringAsync(source, options = {}) {
    return runAsyncLocked(async () => {
      if (typeof source !== "string") {
        throw new TypeError("compileStringAsync(source): source must be a string");
      }
      const chain = buildChain(options, true);
      const callbacks = registerFunctions(asyncInstance(), options);
      asyncChain = chain;
      asyncFunctions = callbacks;
      asyncLogger = options.logger ?? null;
      try {
        const raw = await compileRawAsync(source, rawOpts(options, syntaxCode(options.syntax)));
        return makeResult(raw, options.url ? toFileUrl(options.url).href : null, chain.loaded);
      } finally {
        asyncChain = null;
        asyncFunctions = [];
        asyncLogger = null;
        asyncEx.sasso_clear_functions();
      }
    });
  }

  function compileAsync(path, options = {}) {
    return runAsyncLocked(async () => {
      const fsPath = toFsPath(path);
      const source = readFileSync(fsPath, "utf8");
      let realPath = fsPath;
      try {
        realPath = realpathSync(fsPath);
      } catch {
        // keep fsPath
      }
      const entryHref = toFileUrl(realPath).href;
      const syntax = options.syntax != null ? syntaxCode(options.syntax) : syntaxForPath(realPath);
      const chain = buildChain(options, true);
      const callbacks = registerFunctions(asyncInstance(), options);
      asyncChain = chain;
      asyncFunctions = callbacks;
      asyncLogger = options.logger ?? null;
      try {
        const raw = await compileRawAsync(source, { ...rawOpts(options, syntax), url: entryHref });
        return makeResult(raw, entryHref, chain.loaded);
      } finally {
        asyncChain = null;
        asyncFunctions = [];
        asyncLogger = null;
        asyncEx.sasso_clear_functions();
      }
    });
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
