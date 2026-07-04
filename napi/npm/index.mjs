// sasso-native — the F4 native Node addon entry point (docs/
// ASYNC_PERF_ARCHITECTURE.md). Same dart-sass *modern* API surface as the wasm
// `sasso` package, backed by `sasso.node` (napi, ../src/lib.rs): no asyncify,
// async compiles each run on their own OS thread (real CPU parallelism), and
// `loadPaths`/relative resolution happen natively in Rust — zero JS
// round-trips for the common case. Only USER importers, custom functions, and
// the logger cross the thread bridge.
//
// Deliberate reuse from the wasm package (single source of truth in-repo):
//   • `_importer.mjs` — user-importer normalization (Importer/FileImporter,
//     maybe-async semantics, sync-mode Promise rejection);
//   • `_value.mjs` — the whole Value type system + byte protocol for custom
//     functions (the native `valueOp` replaces the wasm `sasso_value_op`);
//   • `Exception` / `Logger` from `_loader.mjs` — identical error shape.
//
// Canonical-URL note: the native fs importer's canonical form is an ABSOLUTE
// PATH (the core's own), converted to `file:` URLs at this boundary
// (loadedUrls, importer containingUrl). User-importer canonicals pass through
// untouched.

import { createRequire } from "node:module";
import { readFileSync, realpathSync } from "node:fs";
import { pathToFileURL, fileURLToPath } from "node:url";
import {
  isThenable,
  normalizeImporter,
  syntaxCode,
  syntaxForPath,
} from "../../wasm/npm/_importer.mjs";
import { Exception, Logger } from "../../wasm/npm/_loader.mjs";
import { deserializeArgs, serializeValue, setEngine, valueApi } from "../../wasm/npm/_value.mjs";

const require_ = createRequire(import.meta.url);
const native = require_("./sasso.node");

// Route the Value-method engine (SassNumber.convert, SassColor.toSpace, …)
// through the native valueOp. Module-global by design (same caveat as the
// wasm entries: in a process importing several sasso entries, the last import
// wins the engine slot — all engines are equivalent).
setEngine((op, argsBytes) => {
  try {
    return new Uint8Array(native.valueOp(op, Buffer.from(argsBytes)));
  } catch (e) {
    throw new Error(e.message);
  }
});

export { Exception, Logger };
export const info = `dart-sass\t1.101.0\t(sasso-napi ${native.nativeVersion()})\t[Rust native]`;

function errMessage(e) {
  return e && e.message ? String(e.message) : String(e);
}

/** A Windows drive-letter path would false-positive a naive scheme test. */
function hasScheme(s) {
  return /^[a-z][a-z0-9+.-]*:/i.test(s) && !/^[A-Za-z]:[\\/]/.test(s);
}

/** Coerce a path or URL to a URL for `loadedUrls` (scheme-aware). */
function toUrl(s) {
  return hasScheme(s) ? new URL(s) : pathToFileURL(s);
}

/**
 * Containing canonical → href for user importers. Only real containers count:
 * an absolute path (→ file: URL) or a URL with a scheme; anything else (the
 * engine's synthetic entry names) means "no containing url", like wasm.
 */
function containingHref(s) {
  if (s == null || s === "") return null;
  if (hasScheme(s)) return s;
  if (s.startsWith("/") || /^[A-Za-z]:[\\/]/.test(s)) return pathToFileURL(s).href;
  return null;
}

/** Rebuild the wasm loader's Exception from the native structured-JSON error. */
function toException(e, origHref, urlForCore) {
  try {
    const parsed = JSON.parse(errMessage(e));
    if (parsed && parsed.sassoError) {
      const se = parsed.sassoError;
      const url = se.url && se.url === urlForCore && origHref ? origHref : se.url || undefined;
      const pos = { line: Math.max(0, se.line - 1), column: Math.max(0, se.col - 1), offset: 0 };
      const span = se.line > 0 ? { url, start: pos, end: pos, text: "", context: "" } : undefined;
      return new Exception(se.rendered, se.sassMessage, span);
    }
  } catch {
    // not a structured sasso error — fall through
  }
  return e instanceof Error ? e : new Error(String(e));
}

/** Dispatch one decoded warn event to the user logger (dart shape) or stderr. */
function dispatchWarn(logger, ev) {
  const spanOf = () => {
    if (!ev.url && !ev.line) return undefined;
    const start = { line: ev.line > 0 ? ev.line - 1 : 0, column: 0 };
    return { url: ev.url || undefined, start, end: start, text: "", context: "" };
  };
  try {
    if (ev.kind === 1) {
      if (logger && typeof logger.debug === "function") return logger.debug(ev.message, { span: spanOf() });
    } else if (logger && typeof logger.warn === "function") {
      return logger.warn(ev.message, {
        deprecation: ev.deprecation,
        deprecationType: ev.deprecationId || undefined,
        span: spanOf(),
        stack: undefined,
      });
    }
    if (typeof process !== "undefined" && process.stderr) process.stderr.write(ev.formatted + "\n");
    else console.error(ev.formatted);
  } catch {
    // A logging failure must never fail the compile.
  }
}

/**
 * Build the per-compile bridge. In async mode the native side calls it via a
 * ThreadsafeFunction and it ANSWERS through `native.bridgeReply(id, …)`
 * (possibly after awaiting a thenable). In sync mode the native side calls it
 * directly and it RETURNS `[rc, syntax, s1, s2, buf]` — user callbacks must
 * settle synchronously (`normalizeImporter(imp, false)` already throws on a
 * Promise, same contract as the wasm sync path).
 */
function makeBridge(options, asyncMode) {
  const resolvers = (options.importers || []).map((i) => normalizeImporter(i, asyncMode));
  const byCanonical = new Map();
  const callbacks = options.functions ? Object.values(options.functions) : [];
  const logger = options.logger ?? null;

  // Maybe-async walk over USER resolvers only (fs is native; the native chain
  // consults us first, mirroring the wasm chain's user-then-fs precedence).
  const walk = (i, url, fromImport, containing) => {
    for (; i < resolvers.length; i++) {
      const r = resolvers[i];
      const canon = r.canonicalize(url, fromImport, containing);
      if (isThenable(canon)) {
        return canon.then((c) => {
          if (c != null) {
            byCanonical.set(c, r);
            return c;
          }
          return walk(i + 1, url, fromImport, containing);
        });
      }
      if (canon != null) {
        byCanonical.set(canon, r);
        return canon;
      }
    }
    return null;
  };

  // kind handlers produce a settled-or-thenable [rc, syntax, s1, s2, buf].
  const handlers = {
    0: (a, b, c) => {
      const v = walk(0, a, c !== 0, containingHref(b));
      return mapMaybe(v, (canon) => (canon == null ? [0, 0, null, null, null] : [1, 0, String(canon), null, null]));
    },
    1: (a) => {
      const r = byCanonical.get(a);
      const v = r ? r.load(a) : null;
      return mapMaybe(v, (res) => {
        if (res == null) return [0, 0, null, null, null];
        // Validate BEFORE crossing the bridge: a non-string reaching
        // bridgeReply's Option<String> is a native type error (crashes the
        // TSFN callback); the wasm engine surfaces this as a compile error.
        if (typeof res.contents !== "string") {
          throw new Error("sasso: an importer's load() must return string contents");
        }
        return [1, res.syntax, res.contents, res.sourceMapUrl == null ? null : String(res.sourceMapUrl), null];
      });
    },
    2: (a) => {
      dispatchWarn(logger, JSON.parse(a));
      return null; // no reply expected
    },
    3: (a, b, c, buf) => {
      const fn = callbacks[c];
      if (!fn) throw new Error(`sasso: custom function #${c} is not registered`);
      const r = fn(deserializeArgs(buf ? new Uint8Array(buf) : new Uint8Array(0)));
      if (isThenable(r)) {
        if (!asyncMode) throw new Error("sasso: asynchronous custom functions require compileStringAsync / compileAsync");
        return r.then((v) => {
          if (v == null) throw new Error("sasso: a custom function returned no value");
          return [1, 0, null, null, Buffer.from(serializeValue(v))];
        });
      }
      if (r == null) throw new Error("sasso: a custom function returned no value");
      return [1, 0, null, null, Buffer.from(serializeValue(r))];
    },
  };

  const mapMaybe = (v, f) => (isThenable(v) ? v.then(f) : f(v));
  const errReply = (e) => [-1, 0, errMessage(e), null, null];

  if (!asyncMode) {
    // Direct synchronous call; exceptions map to rc=-1 (never thrown across).
    return (id, kind, a, b, c, buf) => {
      try {
        const out = handlers[kind](a, b, c, buf);
        if (out === null) return [0, 0, null, null, null]; // warn: ignored
        if (isThenable(out)) return errReply(new Error("sasso: importer callbacks must be synchronous on the sync API"));
        return out;
      } catch (e) {
        return errReply(e);
      }
    };
  }
  // Every path MUST reply (a lost reply parks the compile thread), and the
  // reply itself must never throw — a marshal failure falls back to an
  // all-plain-strings error reply.
  const reply = (id, r) => {
    try {
      native.bridgeReply(id, ...sliceReply(r));
    } catch (e) {
      native.bridgeReply(id, -1, 0, `sasso: bridge reply failed to marshal: ${errMessage(e)}`, null, null);
    }
  };
  return (id, kind, a, b, c, buf) => {
    let out;
    try {
      out = handlers[kind](a, b, c, buf);
    } catch (e) {
      reply(id, errReply(e));
      return;
    }
    if (out === null) return; // warn
    if (isThenable(out)) {
      out.then(
        (r) => reply(id, r),
        (e) => reply(id, errReply(e)),
      );
      return;
    }
    reply(id, out);
  };
}

const sliceReply = (r) => [r[0], r[1], r[2] ?? null, r[3] ?? null, r[4] ?? null];

// ------------------------------------------------------------- option mapping

function buildCfg(options, syntax, urlForCore) {
  return {
    syntax,
    compressed: options.style === "compressed",
    // napi Option<String> maps `undefined` to None; `null` is a type error.
    url: urlForCore ?? undefined,
    wantMap: !!options.sourceMap,
    includeSources: !!options.sourceMapIncludeSources,
    charset: options.charset !== false,
    loadPaths: (options.loadPaths || []).map(String),
    hasUserImporters: !!(options.importers && options.importers.length),
    functionSignatures: options.functions ? Object.keys(options.functions) : [],
    wantWarn: true,
  };
}

/**
 * Entry url → its href, passed to the core VERBATIM: diagnostics then render
 * the same file: URL text the wasm engine renders, and the native chain
 * decodes file: containers back to paths for fs resolution. An empty string
 * is absent (wasm's `options.url ? …` semantics).
 */
function entryUrls(url) {
  if (url == null || url === "") return { origHref: null, urlForCore: null };
  const u = url instanceof URL ? url : hasScheme(String(url)) ? new URL(url) : pathToFileURL(String(url));
  return { origHref: u.href, urlForCore: u.href };
}

function makeResult(nat, origHref) {
  const urls = [];
  const seen = new Set();
  const add = (u) => {
    const href = u instanceof URL ? u.href : u;
    if (href && !seen.has(href)) {
      seen.add(href);
      urls.push(u instanceof URL ? u : new URL(href));
    }
  };
  if (origHref) add(new URL(origHref));
  for (const s of nat.loadedUrls) add(toUrl(s));
  const result = { css: nat.css, loadedUrls: urls };
  if (nat.sourceMap != null) {
    const map = JSON.parse(nat.sourceMap);
    // The core relativizes map sources against the entry for BOTH engines, so
    // they normally match the wasm output as-is. An ABSOLUTE path source (an
    // unrelativizable file) is the one native-specific case — the wasm engine
    // would carry a file: URL there, so normalize just those.
    if (Array.isArray(map.sources)) {
      map.sources = map.sources.map((s) =>
        typeof s === "string" && (s.startsWith("/") || /^[A-Za-z]:[\\/]/.test(s)) ? pathToFileURL(s).href : s,
      );
    }
    result.sourceMap = map;
  }
  return result;
}

// --------------------------------------------------------- dart-sass modern API

export function compileString(source, options = {}) {
  if (typeof source !== "string") {
    throw new TypeError("compileString(source): source must be a string");
  }
  const { origHref, urlForCore } = entryUrls(options.url);
  const cfg = buildCfg(options, syntaxCode(options.syntax), urlForCore);
  const bridge = makeBridge(options, false);
  let nat;
  try {
    nat = native.compileStringSync(source, cfg, bridge);
  } catch (e) {
    throw toException(e, origHref, urlForCore);
  }
  return makeResult(nat, origHref);
}

export function compileStringAsync(source, options = {}) {
  if (typeof source !== "string") {
    return Promise.reject(new TypeError("compileStringAsync(source): source must be a string"));
  }
  const { origHref, urlForCore } = entryUrls(options.url);
  const cfg = buildCfg(options, syntaxCode(options.syntax), urlForCore);
  const bridge = makeBridge(options, true);
  return native.compileStringAsync(source, cfg, bridge).then(
    (nat) => makeResult(nat, origHref),
    (e) => {
      throw toException(e, origHref, urlForCore);
    },
  );
}

function entryFor(path, options) {
  const fsPath = path instanceof URL || String(path).startsWith("file:") ? fileURLToPath(path) : String(path);
  const source = readFileSync(fsPath, "utf8");
  let realPath = fsPath;
  try {
    realPath = realpathSync(fsPath);
  } catch {
    // keep fsPath if realpath fails
  }
  const syntax = options.syntax != null ? syntaxCode(options.syntax) : syntaxForPath(realPath);
  return { source, realPath, entryHref: pathToFileURL(realPath).href, syntax };
}

export function compile(path, options = {}) {
  const { source, entryHref, syntax } = entryFor(path, options);
  const cfg = buildCfg(options, syntax, entryHref);
  const bridge = makeBridge(options, false);
  let nat;
  try {
    nat = native.compileStringSync(source, cfg, bridge);
  } catch (e) {
    throw toException(e, entryHref, entryHref);
  }
  return makeResult(nat, entryHref);
}

export function compileAsync(path, options = {}) {
  let entry;
  try {
    entry = entryFor(path, options);
  } catch (e) {
    return Promise.reject(e);
  }
  const { source, entryHref, syntax } = entry;
  const cfg = buildCfg(options, syntax, entryHref);
  const bridge = makeBridge(options, true);
  return native.compileStringAsync(source, cfg, bridge).then(
    (nat) => makeResult(nat, entryHref),
    (e) => {
      throw toException(e, entryHref, entryHref);
    },
  );
}

/** Accepted for API parity; the native engine has no arena/pool knobs (each
 * async compile is its own OS thread; memory is the process allocator). */
export function configure() {}

export function initCompiler() {
  return { compile, compileString, dispose() {} };
}
export async function initAsyncCompiler() {
  return { compileAsync, compileStringAsync, async dispose() {} };
}

export {
  Value,
  SassBoolean,
  SassColor,
  SassList,
  SassArgumentList,
  SassMap,
  SassNumber,
  SassString,
  SassCalculation,
  CalculationOperation,
  SassFunction,
  SassMixin,
  sassTrue,
  sassFalse,
  sassNull,
} from "../../wasm/npm/_value.mjs";

export default {
  compile,
  compileAsync,
  compileString,
  compileStringAsync,
  initCompiler,
  initAsyncCompiler,
  configure,
  info,
  Exception,
  Logger,
  ...valueApi,
};
