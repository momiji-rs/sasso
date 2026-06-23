// Importer machinery for the `sasso` npm package.
//
// wasm has no filesystem, so every `@use`/`@forward`/`@import` is resolved here
// in JS and bridged into the engine through the wasm host functions (see
// `_loader.mjs`). This module provides the two resolver kinds sasso merges into
// one chain per compile:
//
//   • a Node-fs importer (`makeFsImporter`) for `loadPaths` and relative-to-
//     containing-file resolution — a faithful JS port of the dart-sass
//     partial / index / import-only precedence in `../../src/importer.rs`; and
//   • a bridge (`normalizeImporter`) for user-supplied dart-sass *modern*
//     importers — both `{ canonicalize, load }` Importers and `{ findFileUrl }`
//     FileImporters.
//
// The internal resolver interface is sync and string-based:
//   canonicalize(url, fromImport, containingHref|null) -> canonicalHref|null
//   load(canonicalHref) -> { contents, syntax: 0|1|2, sourceMapUrl: string|null } | null
// `syntax`: 0 = SCSS, 1 = indented `.sass`, 2 = plain CSS.
//
// **Synchronous only.** The engine is sync, so importer callbacks cannot await;
// a user importer that returns a Promise throws a clear error (even under
// `compileStringAsync`). Truly-async importers are unsupported.

import { existsSync, statSync, readFileSync, realpathSync } from "node:fs";
import { pathToFileURL, fileURLToPath } from "node:url";
import * as nodePath from "node:path";

const SYNTAX_SCSS = 0;
const SYNTAX_SASS = 1;
const SYNTAX_CSS = 2;

function isThenable(x) {
  return x != null && typeof x.then === "function";
}

const ASYNC_UNSUPPORTED =
  "sasso: asynchronous importers are not supported — the wasm engine is " +
  "synchronous, so importer callbacks must return synchronously (even under " +
  "compileStringAsync).";

/** Map a dart-sass syntax string to the wasm syntax code. */
export function syntaxCode(syntax) {
  if (syntax === "indented" || syntax === "sass") return SYNTAX_SASS;
  if (syntax === "css") return SYNTAX_CSS;
  return SYNTAX_SCSS;
}

/** The syntax code for a resolved file path, from its extension. */
export function syntaxForPath(p) {
  const ext = nodePath.extname(p).toLowerCase();
  if (ext === ".sass") return SYNTAX_SASS;
  if (ext === ".css") return SYNTAX_CSS;
  return SYNTAX_SCSS;
}

function isFile(p) {
  try {
    return statSync(p).isFile();
  } catch {
    return false;
  }
}

// --- dart-sass filesystem resolution (port of src/importer.rs) -------------

/** Lexically remove `.` / `..` segments from a URL path (no fs access). */
function lexicalNormalize(path) {
  const out = [];
  for (const seg of path.split("/")) {
    if (seg === "" || seg === ".") continue;
    if (seg === "..") {
      if (out.length && out[out.length - 1] !== "..") out.pop();
      else out.push("..");
    } else {
      out.push(seg);
    }
  }
  let s = out.join("/");
  if (path.startsWith("/")) s = "/" + s;
  if (s === "") s = ".";
  return s;
}

// One precedence tier: collect existing candidates, returning a single match,
// `"ambiguous"` for >1 at the same tier, or `null` for none.
function tierExact(dir, stem, exts, importOnly) {
  const found = [];
  const suffix = importOnly ? ".import" : "";
  for (const ext of exts) {
    for (const name of [`_${stem}${suffix}.${ext}`, `${stem}${suffix}.${ext}`]) {
      const cand = nodePath.join(dir, name);
      if (isFile(cand)) found.push(cand);
    }
  }
  if (found.length === 0) return null;
  if (found.length > 1) return "ambiguous";
  return found[0];
}

function tierWithExtensions(dir, stem, importOnly) {
  return tierExact(dir, stem, ["scss", "sass"], importOnly);
}

// Resolve `path` against `base` following dart-sass precedence. Returns an
// absolute path, the sentinel `"ambiguous"`, or `null` (not found here).
function resolveInBase(base, path, allowImportOnly) {
  const normalized = lexicalNormalize(path);
  const parsed = nodePath.posix.parse(normalized.replace(/\\/g, "/"));
  // The directory portion of the (normalized) import path, joined onto `base`.
  const subDir = parsed.dir && parsed.dir !== "" ? parsed.dir : "";
  const dir = subDir ? nodePath.join(base, subDir) : base;
  const file = parsed.base || normalized;

  // Explicit `.css`: only the plain-CSS candidate.
  if (file.endsWith(".css")) {
    return tierExact(dir, file.slice(0, -4), ["css"], false);
  }

  // Explicit `.scss`/`.sass`: only that extension (+ import-only override).
  const explicitExt = [".scss", ".sass"].find((e) => file.endsWith(e));
  if (explicitExt) {
    const stem = file.slice(0, -explicitExt.length);
    const ext = explicitExt.slice(1);
    if (allowImportOnly) {
      const r = tierExact(dir, stem, [ext], true);
      if (r) return r;
    }
    return tierExact(dir, stem, [ext], false);
  }

  // Extensionless: scss/sass equal precedence, then css, then index dirs.
  const nonIndex = [];
  if (allowImportOnly) nonIndex.push([file, true]);
  nonIndex.push([file, false]);
  for (const [stem, importOnly] of nonIndex) {
    const r = tierWithExtensions(dir, stem, importOnly);
    if (r) return r;
  }

  const cssr = tierExact(dir, file, ["css"], false);
  if (cssr) return cssr;

  const indexDir = nodePath.join(dir, file);
  const indexModes = allowImportOnly ? [true, false] : [false];
  for (const importOnly of indexModes) {
    const r = tierWithExtensions(indexDir, "index", importOnly);
    if (r) return r;
  }

  return null;
}

/** Canonical key for a resolved path: its realpath as a `file:` URL href. */
function canonicalHrefFor(path) {
  let real = path;
  try {
    real = realpathSync(path);
  } catch {
    // keep the un-canonicalized path (mirrors src/importer.rs's fallback)
  }
  return pathToFileURL(real).href;
}

/** Read a resolved file as an importer result (`null` if it vanished). */
function loadFsPath(path) {
  let contents;
  try {
    contents = readFileSync(path, "utf8");
  } catch (e) {
    if (e && e.code === "ENOENT") return null; // raced between resolve and load
    throw new Error(`Cannot read ${path}: ${e && e.message ? e.message : e}`);
  }
  return { contents, syntax: syntaxForPath(path), sourceMapUrl: null };
}

/**
 * A Node-fs importer searching, in order, the containing file's directory then
 * `loadPaths`, with dart-faithful partial/index/import-only precedence.
 */
export function makeFsImporter(loadPaths) {
  const bases = (loadPaths || []).map((p) => String(p));
  return {
    canonicalize(url, fromImport, containingHref) {
      // Base directories: the containing file's dir (when it is a file: URL),
      // then the configured load paths. Unlike the CLI's FsImporter we do NOT
      // fall back to the CWD when there is no containing file — dart-sass's
      // `compileString` only resolves relative URLs when given a `url` (or via
      // `loadPaths`), so an import with neither simply misses.
      const baseDirs = [];
      if (containingHref) {
        try {
          baseDirs.push(nodePath.dirname(fileURLToPath(containingHref)));
        } catch {
          // containing URL isn't a file: URL — skip relative resolution
        }
      }
      for (const b of bases) baseDirs.push(b);

      for (const base of baseDirs) {
        const r = resolveInBase(base, url, fromImport);
        if (r === "ambiguous") return null; // dart errors; we treat as a miss
        if (r) return canonicalHrefFor(r);
      }
      return null;
    },
    load(canonicalHref) {
      let path;
      try {
        path = fileURLToPath(canonicalHref);
      } catch {
        return null;
      }
      return loadFsPath(path);
    },
  };
}

// --- user (dart-sass modern) importer bridging -----------------------------

function ctxFor(fromImport, containingHref) {
  return {
    fromImport,
    containingUrl: containingHref ? new URL(containingHref) : undefined,
  };
}

function toHref(urlOrString) {
  return urlOrString instanceof URL ? urlOrString.href : new URL(urlOrString).href;
}

// Settle a possibly-thenable user return into the chain interface. In `async`
// mode the result is always a Promise (so async importers are awaited by the
// async engine); in sync mode a Promise is a hard error (the sync engine can't
// await it), and a plain value maps through immediately.
function settle(raw, map, async) {
  if (async) return Promise.resolve(raw).then((v) => (v == null ? null : map(v)));
  if (isThenable(raw)) throw new Error(ASYNC_UNSUPPORTED);
  return raw == null ? null : map(raw);
}

const loadMap = (r) => ({
  contents: r.contents,
  syntax: syntaxCode(r.syntax),
  sourceMapUrl: r.sourceMapUrl != null ? String(r.sourceMapUrl) : null,
});

/** Wrap a user `{ canonicalize, load }` dart-sass Importer (`async` = await Promises). */
function wrapImporter(imp, async) {
  return {
    canonicalize(url, fromImport, containingHref) {
      return settle(imp.canonicalize(url, ctxFor(fromImport, containingHref)), toHref, async);
    },
    load(canonicalHref) {
      return settle(imp.load(new URL(canonicalHref)), loadMap, async);
    },
  };
}

/**
 * Wrap a user `{ findFileUrl }` dart-sass FileImporter: `findFileUrl` returns a
 * `file:` URL, which we then resolve on disk with the standard partial/index
 * precedence and read. (`async` = await an async `findFileUrl`.)
 */
function wrapFileImporter(imp, async) {
  const finish = (r, fromImport) => {
    if (r == null) return null;
    const fileUrl = r instanceof URL ? r : new URL(r);
    if (fileUrl.protocol !== "file:") {
      throw new Error(
        `sasso: FileImporter.findFileUrl must return a file: URL, got ${fileUrl.protocol}`,
      );
    }
    const target = fileURLToPath(fileUrl);
    const resolved = resolveInBase(nodePath.dirname(target), nodePath.basename(target), fromImport);
    return !resolved || resolved === "ambiguous" ? null : canonicalHrefFor(resolved);
  };
  return {
    canonicalize(url, fromImport, containingHref) {
      const raw = imp.findFileUrl(url, ctxFor(fromImport, containingHref));
      if (async) return Promise.resolve(raw).then((r) => finish(r, fromImport));
      if (isThenable(raw)) throw new Error(ASYNC_UNSUPPORTED);
      return finish(raw, fromImport);
    },
    load(canonicalHref) {
      try {
        return loadFsPath(fileURLToPath(canonicalHref));
      } catch {
        return null;
      }
    },
  };
}

/**
 * Normalize one user importer (Importer or FileImporter) to the chain interface.
 * Pass `async = true` for the asyncified engine (callbacks may return Promises);
 * the default (sync engine) rejects Promises with a clear error.
 */
export function normalizeImporter(imp, async = false) {
  if (imp && typeof imp.canonicalize === "function" && typeof imp.load === "function") {
    return wrapImporter(imp, async);
  }
  if (imp && typeof imp.findFileUrl === "function") {
    return wrapFileImporter(imp, async);
  }
  throw new Error(
    "sasso: each importer must be a dart-sass Importer ({ canonicalize, load }) " +
      "or FileImporter ({ findFileUrl }).",
  );
}
