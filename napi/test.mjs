// Test suite for the sasso-napi native addon (F4). Two pillars:
//
//   1. OUTPUT CORRECTNESS BY CONSTRUCTION: the wasm engine's output is
//      byte-exact against dart-sass (the sass-spec ratchet + parity CI jobs),
//      so the native engine is verified by BYTE-PARITY AGAINST THE WASM
//      ENGINE over the real corpora (modular incl. loadPaths + all 10
//      entries, handwritten, generated/large), expanded and compressed, sync
//      and async, plus sourceMap and loadedUrls equivalence.
//
//   2. BEHAVIOR GUARDS mirroring wasm/test.mjs's async-path guards: importer
//      bridging (sync/async/FileImporter/mixed chains), error mapping
//      (Exception shape, sync-throwing importers, mixed outcomes under
//      concurrency), logger routing, custom functions over the byte
//      protocol, concurrent isolation (thread-per-compile), and true
//      overlap (a compile completes while another is suspended).
//
// Run: bash napi/build.sh && node napi/test.mjs   (wasm/npm must be built too)
import assert from "node:assert/strict";
import { writeFileSync, mkdtempSync, mkdirSync, realpathSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { pathToFileURL, fileURLToPath } from "node:url";

import * as napi from "./npm/index.mjs";
import * as wasm from "../wasm/npm/sasso.mjs";

const delay = (ms) => new Promise((r) => setTimeout(r, ms));
const REPO = fileURLToPath(new URL("..", import.meta.url));

// ============================ 1. wasm byte-parity ============================

const MODULAR = join(REPO, "bench", "corpus", "modular");
const VENDOR = join(MODULAR, "vendor");

async function parityCase(label, run) {
  const [n, w] = await Promise.all([run(napi), run(wasm)]);
  assert.equal(n.css, w.css, `${label}: native CSS is byte-identical to the wasm engine`);
  assert.deepEqual(
    n.loadedUrls.map((u) => u.href),
    w.loadedUrls.map((u) => u.href),
    `${label}: loadedUrls identical (same URLs, same order)`,
  );
  if (n.sourceMap || w.sourceMap) {
    assert.deepEqual(n.sourceMap.sources, w.sourceMap.sources, `${label}: sourceMap sources identical`);
    assert.equal(n.sourceMap.mappings, w.sourceMap.mappings, `${label}: sourceMap mappings identical`);
    assert.deepEqual(n.sourceMap.sourcesContent, w.sourceMap.sourcesContent, `${label}: sourcesContent identical`);
    assert.deepEqual(n.sourceMap.names, w.sourceMap.names, `${label}: sourceMap names identical`);
  }
  return n;
}

// All ten modular-corpus entries, async, expanded (the realistic bundler shape).
for (const i of ["01", "02", "03", "04", "05", "06", "07", "08", "09", "10"]) {
  const entry = join(MODULAR, `entry_${i}.scss`);
  const r = await parityCase(`modular entry_${i} (async)`, (eng) =>
    eng.compileAsync(entry, { loadPaths: [VENDOR], style: "expanded" }),
  );
  assert.ok(r.css.includes(".sasso-corpus-sanity"), `entry_${i}: sanity marker present`);
  assert.ok(r.loadedUrls.length >= 20, `entry_${i}: loaded a real module graph (${r.loadedUrls.length} files)`);
}
// Entry 01 again: sync, compressed, and with a source map.
await parityCase("modular entry_01 (sync)", (eng) => eng.compile(join(MODULAR, "entry_01.scss"), { loadPaths: [VENDOR] }));
await parityCase("modular entry_01 (compressed)", (eng) =>
  eng.compileAsync(join(MODULAR, "entry_01.scss"), { loadPaths: [VENDOR], style: "compressed" }),
);
await parityCase("modular entry_01 (sourceMap)", (eng) =>
  eng.compileAsync(join(MODULAR, "entry_01.scss"), { loadPaths: [VENDOR], sourceMap: true, sourceMapIncludeSources: true }),
);
// The other in-repo corpora, compileString round (url + loadPaths chain).
for (const [label, path, loadPaths] of [
  ["handwritten", join(REPO, "bench", "corpus", "handwritten", "main.scss"), [join(REPO, "bench", "corpus", "handwritten")]],
  ["generated/large", join(REPO, "bench", "corpus", "generated", "large.scss"), []],
]) {
  const src = (await import("node:fs")).readFileSync(path, "utf8");
  await parityCase(`${label} (compileString async)`, (eng) =>
    eng.compileStringAsync(src, { url: pathToFileURL(path), loadPaths, style: "expanded" }),
  );
  await parityCase(`${label} (compileString compressed sync)`, (eng) =>
    Promise.resolve(eng.compileString(src, { url: pathToFileURL(path), loadPaths, style: "compressed" })),
  );
}
// Indented syntax, plain CSS via @use, and the charset/BOM non-ASCII paths.
await parityCase("indented syntax", (eng) =>
  eng.compileStringAsync(".a\n  b: c\n  .n\n    d: e\n", { syntax: "indented" }),
);
{
  const cssDir = realpathSync(mkdtempSync(join(tmpdir(), "sasso-cssuse-")));
  writeFileSync(join(cssDir, "plain.css"), ".raw { keep: me }\n");
  await parityCase("plain CSS via @use", (eng) =>
    eng.compileStringAsync('@use "plain";\n.x { y: 1 }\n', { loadPaths: [cssDir] }),
  );
}
for (const style of ["expanded", "compressed"]) {
  const r = await parityCase(`charset non-ASCII (${style})`, (eng) =>
    eng.compileStringAsync('.uni::before { content: "こんにちは"; }', { style }),
  );
  if (style === "expanded") assert.ok(r.css.startsWith('@charset "UTF-8";'), "expanded @charset prefix present");
  else assert.ok(r.css.startsWith("﻿"), "compressed BOM preserved");
}
// Error parity: the same broken inputs produce the same Exception on both
// engines (the napi error transport is a separate hand-rolled path).
{
  const errOf = (eng, fn) => {
    try {
      fn(eng);
      return null;
    } catch (e) {
      return { message: e.message, sassMessage: e.sassMessage, span: e.span };
    }
  };
  for (const [label, fn] of [
    ["parse error", (eng) => eng.compileString("a { b: ", { url: "file:///x/in.scss" })],
    ["undefined variable", (eng) => eng.compileString(".a { c: $nope; }", { url: "file:///x/vars.scss" })],
    ["missing import", (eng) => eng.compileString('@use "ghost";', { url: "file:///x/imp.scss" })],
  ]) {
    assert.deepEqual(errOf(napi, fn), errOf(wasm, fn), `error parity: ${label}`);
  }
}
console.log("ok: wasm byte-parity — corpora + indented/css/charset + error parity");

// =========================== 2. behavior guards =============================

// --- fixtures on disk (shared) ---
// realpath: tmpdir is often a symlink (/var -> /private/var on macOS) and the
// wrapper realpaths entries, so expectations must use the resolved form.
const root = realpathSync(mkdtempSync(join(tmpdir(), "sasso-napi-")));
mkdirSync(join(root, "sub"), { recursive: true });
writeFileSync(join(root, "_dep.scss"), "$w: 42px;\n");
writeFileSync(join(root, "sub", "_inner.scss"), '@use "../dep" as d;\n.inner { width: d.$w; }\n');
writeFileSync(join(root, "entry.scss"), '@use "sub/inner";\n.top { t: 1; }\n');
writeFileSync(join(root, "fi.scss"), "$s: 10px;\n");

// (a) entry-relative + nested-relative native fs resolution, sync === async.
{
  const rs = napi.compile(join(root, "entry.scss"));
  const ra = await napi.compileAsync(join(root, "entry.scss"));
  assert.equal(rs.css, ra.css, "compile(path): sync equals async");
  assert.ok(rs.css.includes("width: 42px"), "nested relative @use resolved natively");
  assert.equal(rs.loadedUrls[0].href, pathToFileURL(join(root, "entry.scss")).href, "entry first in loadedUrls");
  assert.ok(rs.loadedUrls.every((u) => u.protocol === "file:"), "fs loadedUrls are file: URLs");
}

// (b) user importer precedence over fs + containing URL delivery.
{
  const seen = [];
  const imp = {
    canonicalize(url, ctx) {
      seen.push([url, ctx.containingUrl ? ctx.containingUrl.protocol : null]);
      return url === "virtual" ? new URL("custom:v") : null;
    },
    load: (u) => (u.href === "custom:v" ? { contents: ".v { ok: 1 }", syntax: "scss" } : null),
  };
  const r = await napi.compileAsync(join(root, "entry.scss"), { importers: [imp] });
  assert.ok(r.css.includes(".top"), "compile succeeds with a missing-everything importer in front");
  assert.ok(seen.some(([u]) => u === "sub/inner"), "user importer consulted BEFORE native fs");
  assert.ok(seen.every(([, p]) => p === null || p === "file:"), "containing urls arrive as file: URLs");
  const r2 = await napi.compileStringAsync('@use "virtual";', { importers: [imp] });
  assert.ok(r2.css.includes(".v"), "user importer hit resolves");
  assert.deepEqual(r2.loadedUrls.map((u) => u.href), ["custom:v"], "user canonical in loadedUrls");
}

// (c) FileImporter (findFileUrl) sync + async.
{
  const fi = { findFileUrl: (url) => (url === "shared" ? pathToFileURL(join(root, "fi")) : null) };
  const src = '@use "shared" as s;\n.a { height: s.$s; }\n';
  const rs = napi.compileString(src, { importers: [fi] });
  const ra = await napi.compileStringAsync(src, {
    importers: [{ findFileUrl: async (url) => (url === "shared" ? pathToFileURL(join(root, "fi")) : null) }],
  });
  assert.equal(rs.css, ra.css, "FileImporter: sync equals async");
  assert.ok(rs.css.includes("height: 10px"), "FileImporter resolved on disk");
}

// (d) mixed chains: async-miss -> sync-hit and sync-miss -> async-hit.
{
  const asyncMiss = { canonicalize: async () => null, load: async () => null };
  const syncHit = { canonicalize: (u) => (u === "mx" ? new URL("custom:mx1") : null), load: () => ({ contents: ".mx { from: sync }", syntax: "scss" }) };
  const syncMiss = { canonicalize: () => null, load: () => null };
  const asyncHit = { canonicalize: async (u) => (u === "mx" ? new URL("custom:mx2") : null), load: async () => ({ contents: ".mx { from: async }", syntax: "scss" }) };
  const m1 = await napi.compileStringAsync('@use "mx";', { importers: [asyncMiss, syncHit] });
  const m2 = await napi.compileStringAsync('@use "mx";', { importers: [syncMiss, asyncHit] });
  assert.ok(m1.css.includes("from: sync") && m2.css.includes("from: async"), "mixed chains walk correctly");
}

// (e) errors: Exception shape, sync API rejects Promise importers, sync throws.
{
  assert.throws(
    () => napi.compileString("a { b: ", { url: "file:///x/in.scss" }),
    (e) => e instanceof napi.Exception && typeof e.sassMessage === "string" && e.span && e.span.start.line >= 0,
    "parse error throws an Exception with sassMessage + span",
  );
  await assert.rejects(
    () => napi.compileStringAsync('@use "q";', { importers: [{ canonicalize() { throw new Error("napi-canon-throw"); }, load: () => null }] }),
    (e) => e instanceof napi.Exception && e.message.includes("napi-canon-throw"),
    "sync-throwing canonicalize rejects with the message",
  );
  await assert.rejects(
    () => napi.compileStringAsync('@use "q";', { importers: [{ canonicalize: () => new URL("custom:q"), load: () => Promise.reject(new Error("napi-load-boom")) }] }),
    (e) => e instanceof napi.Exception && e.message.includes("napi-load-boom"),
    "rejecting load rejects with the message",
  );
  assert.throws(
    () => napi.compileString('@use "p";', { importers: [{ canonicalize: async () => new URL("custom:p"), load: () => null }] }),
    /asynchronous importers are not supported/,
    "sync API rejects Promise-returning importers (wasm parity)",
  );
}

// (f) logger: @warn/@debug routed on both APIs, deprecation flagged.
{
  for (const mode of ["sync", "async"]) {
    const logged = [];
    const logger = {
      warn: (m, o) => logged.push(["warn", m, o.deprecation]),
      debug: (m) => logged.push(["debug", m]),
    };
    const src = '@warn "nwmsg"; @debug 40 + 2; .a { b: c; }';
    const r = mode === "sync" ? napi.compileString(src, { logger }) : await napi.compileStringAsync(src, { logger });
    assert.ok(r.css.includes(".a"), `${mode} logger compile emits CSS`);
    assert.deepEqual(logged, [["warn", "nwmsg", false], ["debug", "42"]], `${mode}: @warn + @debug routed`);
  }
  // Deprecation flag parity with the wasm engine (@import is deprecated).
  const depOf = (eng) => {
    const dep = [];
    const imp = { canonicalize: (u) => (u === "legacy" ? new URL("custom:legacy") : null), load: () => ({ contents: ".l { i: 1 }", syntax: "scss" }) };
    eng.compileString('@import "legacy";', { importers: [imp], logger: { warn: (m, o) => dep.push([o.deprecation, o.deprecationType ?? null]) } });
    return dep;
  };
  assert.deepEqual(depOf(napi), depOf(wasm), "deprecation warnings (flag + type) match the wasm engine");
}

// (g) custom functions over the byte protocol (native valueOp engine).
{
  const powFns = { "pow($base, $exp)": (args) => new napi.SassNumber(args[0].value ** args[1].value) };
  const src = ".a { x: pow(2, 10); }";
  assert.equal(napi.compileString(src, { functions: powFns }).css, wasm.compileString(src, { functions: powFns }).css, "sync custom function matches wasm");
  const tag = { "tag()": async () => { await delay(1); return new napi.SassString("t-async", { quotes: false }); } };
  const ra = await napi.compileStringAsync(".b { y: tag(); }", { functions: tag });
  assert.ok(ra.css.includes("t-async"), "async custom function on the async API");
  await assert.rejects(
    () => napi.compileStringAsync(".c { z: nil(); }", { functions: { "nil()": () => null } }),
    (e) => e instanceof napi.Exception && e.message.includes("returned no value"),
    "null-returning custom function rejects",
  );
  assert.throws(
    () => napi.compileString(".d { w: later(); }", { functions: { "later()": async () => new napi.SassNumber(1) } }),
    /asynchronous custom functions require/,
    "async custom function rejected on the sync API",
  );
}

// (h) concurrent ISOLATION: 4 threads, distinct importers/loggers/functions.
{
  const isoLogs = [[], [], [], []];
  const iso = await Promise.all(
    [0, 1, 2, 3].map((i) =>
      napi.compileStringAsync(`@use "isomod";\n@warn "w${i}";\n.o-${i} { t: tag(); }\n`, {
        importers: [{
          async canonicalize(url) { await delay(1); return url === "isomod" ? new URL(`custom:iso-${i}`) : null; },
          async load(u) { await delay(1); return u.href === `custom:iso-${i}` ? { contents: `.uniq-${i} { v: ${i}; }`, syntax: "scss" } : null; },
        }],
        logger: { warn: (m) => isoLogs[i].push(m) },
        functions: { "tag()": async () => { await delay(1); return new napi.SassString(`t${i}`, { quotes: false }); } },
      }),
    ),
  );
  for (let i = 0; i < 4; i++) {
    assert.ok(iso[i].css.includes(`.uniq-${i}`) && iso[i].css.includes(`t${i}`), `concurrent #${i} got its own importer + function`);
    for (let j = 0; j < 4; j++) {
      if (j !== i) assert.ok(!iso[i].css.includes(`.uniq-${j}`) && !iso[i].css.includes(`t${j}`), `#${i} has no leakage from #${j}`);
    }
    assert.deepEqual(isoLogs[i], [`w${i}`], `logger #${i} isolated`);
    assert.deepEqual(iso[i].loadedUrls.map((u) => u.href), [`custom:iso-${i}`], `loadedUrls #${i} isolated`);
  }
}

// (i) TRUE overlap: B completes while A is suspended (thread-per-compile).
{
  let releaseGate;
  const gate = new Promise((r) => (releaseGate = r));
  const blocked = napi.compileStringAsync('@use "g";', {
    importers: [{
      canonicalize: async (u) => (u === "g" ? new URL("custom:gated") : null),
      load: async () => { await gate; return { contents: ".gated { ok: 1 }", syntax: "scss" }; },
    }],
  });
  const quick = await napi.compileStringAsync(".q { fast: 1 }");
  assert.ok(quick.css.includes(".q"), "a compile completes while another is suspended");
  releaseGate();
  assert.ok((await blocked).css.includes(".gated"), "the suspended compile completes after its gate opens");
}

// (j) mixed outcomes under concurrency: middle rejects, flanks fulfill.
{
  const ok = (tag) => ({
    canonicalize: async (u) => (u === "mix" ? new URL(`custom:mix-${tag}`) : null),
    load: async () => { await delay(2); return { contents: `.mix-${tag} { m: 1; }`, syntax: "scss" }; },
  });
  const bad = { canonicalize: async (u) => (u === "mix" ? new URL("custom:mix-bad") : null), load: () => Promise.reject(new Error("napi-mid-boom")) };
  const settled = await Promise.allSettled([
    napi.compileStringAsync('@use "mix";', { importers: [ok("a")] }),
    napi.compileStringAsync('@use "mix";', { importers: [bad] }),
    napi.compileStringAsync('@use "mix";', { importers: [ok("b")] }),
  ]);
  assert.equal(settled[0].status, "fulfilled");
  assert.ok(settled[0].value.css.includes(".mix-a"));
  assert.equal(settled[1].status, "rejected");
  assert.ok(settled[1].reason instanceof napi.Exception && settled[1].reason.message.includes("napi-mid-boom"));
  assert.equal(settled[2].status, "fulfilled");
  assert.ok(settled[2].value.css.includes(".mix-b"));
}

// (j2) MALFORMED importer results must be compile errors, never a process
// crash (async: uncaught TSFN exception) or a silent empty stylesheet (sync).
{
  const badImp = { canonicalize: (u) => (u === "bad" ? new URL("custom:bad") : null), load: () => ({ contents: 123, syntax: "scss" }) };
  await assert.rejects(
    () => napi.compileStringAsync('@use "bad";', { importers: [badImp] }),
    (e) => e instanceof napi.Exception && e.message.includes("string contents"),
    "async: non-string contents rejects with an Exception (no crash)",
  );
  assert.throws(
    () => napi.compileString('@use "bad";', { importers: [badImp] }),
    (e) => e instanceof napi.Exception && e.message.includes("string contents"),
    "sync: non-string contents throws (never an empty stylesheet)",
  );
}

// (j3) CWD must NEVER be a resolution base (hermeticity — wasm parity). A trap
// partial sits in the CWD; url-less entries and custom-scheme containers must
// fail to resolve it exactly like the wasm engine, not silently load it.
{
  const trapDir = realpathSync(mkdtempSync(join(tmpdir(), "sasso-cwdtrap-")));
  writeFileSync(join(trapDir, "_trap.scss"), ".trapped { by: cwd; }\n");
  const prevCwd = process.cwd();
  process.chdir(trapDir);
  try {
    for (const [label, run] of [
      ["url-less compileString", (eng) => eng.compileStringAsync('@use "trap";')],
      ["custom-scheme container", (eng) =>
        eng.compileStringAsync('@use "v";', {
          importers: [{
            canonicalize: (u) => (u === "v" ? new URL("custom:v") : null),
            load: (u) => (u.href === "custom:v" ? { contents: '@use "trap";\n.v { ok: 1 }', syntax: "scss" } : null),
          }],
        })],
    ]) {
      const [n, w] = await Promise.allSettled([run(napi), run(wasm)]);
      assert.equal(n.status, "rejected", `napi ${label}: CWD trap NOT resolved`);
      assert.equal(w.status, "rejected", `wasm ${label}: CWD trap NOT resolved (control)`);
    }
  } finally {
    process.chdir(prevCwd);
  }
}

// (j4) relative imports inside a user-canonicalized file: module resolve via
// the native fs against the module's own directory (wasm parity).
{
  const fiDir = realpathSync(mkdtempSync(join(tmpdir(), "sasso-firel-")));
  writeFileSync(join(fiDir, "mod.scss"), '@use "./sibling" as s;\n.mod { v: s.$k; }\n');
  writeFileSync(join(fiDir, "_sibling.scss"), "$k: 7;\n");
  const fi = { findFileUrl: (url) => (url === "mod" ? pathToFileURL(join(fiDir, "mod")) : null) };
  await parityCase("file:-container relative import", (eng) =>
    eng.compileStringAsync('@use "mod";', { importers: [fi] }),
  );
}

// (j5) the same physical file reached via a FileImporter AND via loadPaths is
// ONE module (canonical-namespace unification — wasm parity).
{
  const dupDir = realpathSync(mkdtempSync(join(tmpdir(), "sasso-dup-")));
  writeFileSync(join(dupDir, "_shared.scss"), ".sh { s: 1; }\n");
  const fi = { findFileUrl: (url) => (url === "sh" ? pathToFileURL(join(dupDir, "shared")) : null) };
  const r = await parityCase("same file via FileImporter + loadPaths", (eng) =>
    eng.compileStringAsync('@use "sh";\n@use "shared";\n', { importers: [fi], loadPaths: [dupDir] }),
  );
  assert.equal(r.css.match(/\.sh /g).length, 1, "the shared module's CSS is emitted exactly once");
}

// (k) re-entrant sync compile from inside a custom function (nested bridge).
{
  const r = napi.compileString(".outer { n: inner(); }", {
    functions: {
      "inner()": () => {
        const nested = napi.compileString(".x { y: 7 }");
        return new napi.SassNumber(nested.css.includes("y: 7") ? 1 : 0);
      },
    },
  });
  assert.ok(r.css.includes("n: 1"), "re-entrant sync compile inside a custom function");
}

// (l) Value-op engine smoke (SassNumber.convert routes through native valueOp).
{
  const n = new napi.SassNumber(1, "in").convert(["px"], []);
  assert.equal(n.value, 96, "SassNumber.convert via the native valueOp engine");
}

console.log("ok: behavior guards — importers, errors, logger, functions, isolation, overlap, re-entrancy, valueOp");
console.log("all sasso-napi native-addon tests passed");
