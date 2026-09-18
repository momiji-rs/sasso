// Smoke test for the wasm package's dart-sass *modern* API, against BOTH the
// size (`./npm/sasso.mjs`) and speed (`./npm/sasso.speed.mjs`) builds. Covers
// the Phase-1 surface (compileString / compile(path) / async / source maps /
// loadedUrls / info / Exception) and the Phase-2 importer surface (loadPaths,
// relative imports, partial/index/import-only resolution, user Importer +
// FileImporter, loadedUrls, importer errors, async rejection), plus the
// async-path correctness guards (sync importers/throws on the async API,
// async loggers, concurrent isolation, mixed outcomes) that the F1/F3
// asyncify refactors must preserve (docs/HANDOFF_ASYNC_IMPORTER_PERF.md).
// Run after build.sh: `node wasm/test.mjs`.
import assert from "node:assert/strict";
import { writeFileSync, mkdtempSync, mkdirSync, readFileSync, existsSync, statSync, symlinkSync, rmSync, openSync, closeSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { pathToFileURL, fileURLToPath } from "node:url";
import { execFileSync, spawn, spawnSync } from "node:child_process";
import * as size from "./npm/sasso.mjs";
import * as speed from "./npm/sasso.speed.mjs";

const SCSS = ".a {\n  color: red;\n  .b { width: 10px; }\n}\n";

// --- shared filesystem fixtures (created once, used by both builds) ---
const root = mkdtempSync(join(tmpdir(), "sasso-imp-"));
const write = (rel, body) => {
  const p = join(root, rel);
  mkdirSync(join(p, ".."), { recursive: true });
  writeFileSync(p, body);
  return p;
};
// relative @use + partial
const mainRel = write("proj/main.scss", `@use "vars" as v;\n.a { color: v.$c; }\n`);
write("proj/_vars.scss", `$c: blue;\n`);
// loadPaths target (in a sibling dir, not next to main)
write("inc/_lib.scss", `$w: 7px;\n`);
// @import partial + index dir
const impMain = write("imp/main.scss", `@import "base";\n@import "theme";\n`);
write("imp/_base.scss", `.b { x: 1; }\n`);
write("imp/theme/_index.scss", `.t { y: 2; }\n`);
// FileImporter target partial
write("fi/_shared.scss", `$s: 10px;\n`);

for (const [name, mod] of [["size", size], ["speed", speed]]) {
  // === Phase 1: core modern API ===

  const r = mod.compileString(SCSS);
  assert.equal(typeof r.css, "string", `${name}: compileString.css is a string`);
  assert.ok(r.css.includes(".a .b {"), `${name}: nested selector flattened`);
  assert.deepEqual(r.loadedUrls, [], `${name}: no loadedUrls without url`);
  assert.ok(!("sourceMap" in r), `${name}: no sourceMap unless asked`);

  const ru = mod.compileString(SCSS, { url: "file:///x.scss" });
  assert.ok(ru.loadedUrls[0] instanceof URL, `${name}: loadedUrls are URLs`);
  assert.equal(ru.loadedUrls[0].href, "file:///x.scss", `${name}: url -> loadedUrls`);

  const rm = mod.compileString(SCSS, { sourceMap: true });
  assert.equal(rm.css, r.css, `${name}: .css matches the plain result`);
  assert.equal(rm.sourceMap.version, 3, `${name}: map version 3`);
  assert.deepEqual(rm.sourceMap.names, [], `${name}: names empty`);
  assert.equal(rm.sourceMap.mappings, "AAAA;EACE;;AACA;EAAK", `${name}: mappings byte-exact vs dart`);
  assert.ok(!("sourcesContent" in rm.sourceMap), `${name}: no sourcesContent unless asked`);

  const rs = mod.compileString(".a { color: red; }\n", { sourceMap: true, sourceMapIncludeSources: true });
  assert.equal(rs.sourceMap.sourcesContent.length, rs.sourceMap.sources.length, `${name}: sourcesContent parallel`);

  const rc = mod.compileString(SCSS, { sourceMap: true, style: "compressed" });
  assert.ok(rc.css.length > 0 && rc.sourceMap.mappings.length > 0, `${name}: compressed map`);

  const ra = await mod.compileStringAsync(SCSS);
  assert.equal(ra.css, r.css, `${name}: compileStringAsync matches sync`);

  let threw;
  try { mod.compileString(".a { color: ; }"); } catch (e) { threw = e; }
  assert.ok(threw instanceof Error, `${name}: error is an Error`);
  assert.ok(threw instanceof mod.Exception, `${name}: error is the exported Exception`);
  assert.equal(threw.name, "Exception", `${name}: error name is Exception`);
  assert.ok(threw.sassMessage && !threw.sassMessage.startsWith("Error:"), `${name}: sassMessage has no Error: prefix`);
  await assert.rejects(() => mod.compileStringAsync(".a { color: ; }"), `${name}: async rejects`);

  // === Phase 1: Compiler API (Vite/sass-loader) ===
  const sync = mod.initCompiler();
  assert.equal(sync.compileString(SCSS).css, r.css, `${name}: initCompiler().compileString`);
  sync.dispose();
  const acomp = await mod.initAsyncCompiler();
  const ac = await acomp.compileStringAsync(SCSS, { url: "file:///x.scss", sourceMap: true });
  assert.equal(ac.css, r.css, `${name}: initAsyncCompiler().compileStringAsync`);
  assert.equal(ac.loadedUrls[0].protocol, "file:", `${name}: compiler loadedUrls are file: URLs`);
  await acomp.dispose();

  assert.ok(mod.info.startsWith("dart-sass\t"), `${name}: info passes the sass-loader name gate`);
  assert.ok(mod.info.includes("sasso"), `${name}: info discloses the real engine`);

  // === Phase 2: importers / loadPaths ===

  // compile(path): relative @use resolves the partial from the entry's dir
  const rp = mod.compile(mainRel);
  assert.ok(rp.css.includes("color: blue"), `${name}: compile(path) relative @use partial`);
  const rpHrefs = rp.loadedUrls.map((u) => u.href);
  // Compare by basename — tmpdir is often a symlink (/var -> /private/var) and
  // canonical URLs are realpath'd, so absolute prefixes differ across macOS.
  assert.ok(rpHrefs.every((h) => h.startsWith("file://")), `${name}: loadedUrls are file: URLs`);
  assert.ok(rpHrefs.some((h) => h.endsWith("/main.scss")), `${name}: loadedUrls includes entry`);
  assert.ok(rpHrefs.some((h) => h.endsWith("/_vars.scss")), `${name}: loadedUrls includes the partial`);

  // compileString with url: same relative resolution against the given url
  const rpS = mod.compileString(`@use "vars" as v;\n.a { color: v.$c; }\n`, { url: pathToFileURL(mainRel) });
  assert.ok(rpS.css.includes("color: blue"), `${name}: compileString({url}) relative @use`);

  // loadPaths: a partial found only via a configured load path
  const rl = mod.compileString(`@use "lib" as l;\n.a { width: l.$w; }\n`, {
    url: pathToFileURL(mainRel),
    loadPaths: [join(root, "inc")],
  });
  assert.ok(rl.css.includes("width: 7px"), `${name}: loadPaths resolves the partial`);

  // @import partial + index directory
  const ri = mod.compile(impMain);
  assert.ok(ri.css.includes(".b") && ri.css.includes(".t"), `${name}: @import partial + index dir`);

  // user Importer (custom scheme, in-memory contents)
  const customImporter = {
    canonicalize(url) { return url === "foo" ? new URL("custom:foo") : null; },
    load(u) { return u.href === "custom:foo" ? { contents: "$c: green;", syntax: "scss" } : null; },
  };
  const rui = mod.compileString(`@use "foo" as f;\n.a { color: f.$c; }\n`, { importers: [customImporter] });
  assert.ok(rui.css.includes("color: green"), `${name}: user Importer canonicalize/load`);
  assert.ok(rui.loadedUrls.some((u) => u.href === "custom:foo"), `${name}: loadedUrls includes the custom canonical`);

  // user FileImporter (findFileUrl -> on-disk partial resolution)
  const fileImporter = {
    findFileUrl(url) { return url === "shared" ? pathToFileURL(join(root, "fi", "shared")) : null; },
  };
  const rfi = mod.compileString(`@use "shared" as s;\n.a { height: s.$s; }\n`, { importers: [fileImporter] });
  assert.ok(rfi.css.includes("height: 10px"), `${name}: user FileImporter findFileUrl`);

  // importer load error -> reported compile error
  const boom = { canonicalize: () => new URL("custom:boom"), load() { throw new Error("kaboom-load"); } };
  assert.throws(() => mod.compileString(`@use "boom";`, { importers: [boom] }), /kaboom-load/, `${name}: importer load error surfaces`);

  // async importer -> clear, synchronous failure
  const asyncImp = { canonicalize: () => Promise.resolve(new URL("custom:x")), load: () => null };
  assert.throws(() => mod.compileString(`@use "x";`, { importers: [asyncImp] }), /asynchronous importers are not supported/, `${name}: async importer rejected`);

  // unresolved import -> Exception (no importer handles it)
  assert.throws(() => mod.compileString(`@use "definitely-missing";`, { url: pathToFileURL(mainRel) }), mod.Exception, `${name}: unresolved import throws`);

  // imports also work through the async + Compiler API paths
  const rasync = await mod.compileAsync(mainRel);
  assert.ok(rasync.css.includes("color: blue"), `${name}: compileAsync resolves imports`);

  // === Phase 2.5: ASYNC importers (asyncify suspends the engine across await) ===

  const delay = (ms) => new Promise((r) => setTimeout(r, ms));
  const asyncImporter = {
    async canonicalize(url) { await delay(2); return url === "remote" ? new URL("custom:remote") : null; },
    async load(u) { await delay(2); return u.href === "custom:remote" ? { contents: "$c: rebeccapurple;", syntax: "scss" } : null; },
  };

  // compileStringAsync awaits an async importer that the sync API rejects.
  const ar = await mod.compileStringAsync(`@use "remote" as r;\n.a { color: r.$c; }\n`, { importers: [asyncImporter] });
  assert.ok(ar.css.includes("rebeccapurple"), `${name}: async importer suspends/resumes the engine`);
  assert.ok(ar.loadedUrls.some((u) => u.href === "custom:remote"), `${name}: async importer loadedUrls`);
  // the SYNC API still rejects the very same async importer
  assert.throws(() => mod.compileString(`@use "remote";`, { importers: [asyncImporter] }), /asynchronous importers are not supported/, `${name}: sync API rejects async importer`);

  // Compiler API async path (this is exactly how Vite drives it)
  const acompiler = await mod.initAsyncCompiler();
  const cr = await acompiler.compileStringAsync(`@use "remote" as r;\n.b { color: r.$c; }\n`, { importers: [asyncImporter] });
  assert.ok(cr.css.includes("rebeccapurple"), `${name}: Compiler API async importer (Vite path)`);
  await acompiler.dispose();

  // async FileImporter (async findFileUrl -> on-disk resolution)
  const asyncFile = {
    async findFileUrl(url) { await delay(2); return url === "shared" ? pathToFileURL(join(root, "fi", "shared")) : null; },
  };
  const af = await mod.compileStringAsync(`@use "shared" as s;\n.a { height: s.$s; }\n`, { importers: [asyncFile] });
  assert.ok(af.css.includes("height: 10px"), `${name}: async FileImporter`);

  // the async path also resolves plain sync fs imports (loadPaths/relative)
  const amix = await mod.compileStringAsync(`@use "vars" as v;\n.a { color: v.$c; }\n`, { url: pathToFileURL(mainRel) });
  assert.ok(amix.css.includes("color: blue"), `${name}: async path resolves sync fs imports`);

  // concurrent async compiles must serialize on the single asyncify stack
  const [c1, c2] = await Promise.all([
    mod.compileStringAsync(`@use "remote" as r;\n.x { color: r.$c; }\n`, { importers: [asyncImporter] }),
    mod.compileStringAsync(`@use "remote" as r;\n.y { color: r.$c; }\n`, { importers: [asyncImporter] }),
  ]);
  assert.ok(c1.css.includes(".x") && c1.css.includes("rebeccapurple"), `${name}: concurrent async compile #1`);
  assert.ok(c2.css.includes(".y") && c2.css.includes("rebeccapurple"), `${name}: concurrent async compile #2`);

  // async importer error -> rejected promise carrying the message
  const asyncBoom = { canonicalize: async () => new URL("custom:boom2"), load: async () => { throw new Error("async-kaboom"); } };
  await assert.rejects(() => mod.compileStringAsync(`@use "boom2";`, { importers: [asyncBoom] }), /async-kaboom/, `${name}: async importer error rejects`);

  // after an error the asyncify stack is clean — a subsequent async compile works
  const recover = await mod.compileStringAsync(`@use "remote" as r;\n.z { color: r.$c; }\n`, { importers: [asyncImporter] });
  assert.ok(recover.css.includes("rebeccapurple"), `${name}: async engine recovers after an importer error`);

  // --- Async-path correctness guards for the asyncify refactors ---
  // Pins the behavior that F1 (asyncLock -> instance pool) and F3 (sync
  // fast-path in asyncHostFn) must preserve — see
  // docs/HANDOFF_ASYNC_IMPORTER_PERF.md. Today the lock serializes all async
  // compiles so isolation holds trivially; once a pool lands these become the
  // real regression guards.

  // (a) F3: a sync-RETURNING importer on the ASYNC API (plain values, no
  // Promises) must produce exactly the sync API's output — this is the path
  // the sync fast-path rewrites.
  const syncRetImporter = {
    canonicalize(url) { return url === "syncret" ? new URL("custom:syncret") : null; },
    load(u) { return u.href === "custom:syncret" ? { contents: "$c: teal;", syntax: "scss" } : null; },
  };
  const syncRetSrc = `@use "syncret" as s;\n.a { color: s.$c; }\n`;
  const aRet = await mod.compileStringAsync(syncRetSrc, { importers: [syncRetImporter] });
  assert.ok(aRet.css.includes("teal"), `${name}: sync-returning importer works on the async API`);
  assert.equal(aRet.css, mod.compileString(syncRetSrc, { importers: [syncRetImporter] }).css, `${name}: async CSS with a sync importer equals the sync API's CSS`);

  // (b) F3: a SYNC throw in canonicalize/load on the async API must reject
  // with an Exception carrying the thrown text (today the throw is absorbed
  // into pendingDelivery; the fast-path's synchronous branch must keep the
  // rc=-1 delivery identical).
  const syncThrowCanon = { canonicalize() { throw new Error("sync-canon-throw"); }, load: () => null };
  await assert.rejects(
    () => mod.compileStringAsync(`@use "q";`, { importers: [syncThrowCanon] }),
    (e) => e instanceof mod.Exception && e.message.includes("sync-canon-throw"),
    `${name}: sync-throwing canonicalize rejects the async compile with the message`,
  );
  const syncThrowLoad = { canonicalize: () => new URL("custom:sthrow"), load() { throw new Error("sync-load-throw"); } };
  await assert.rejects(
    () => mod.compileStringAsync(`@use "sthrow";`, { importers: [syncThrowLoad] }),
    (e) => e instanceof mod.Exception && e.message.includes("sync-load-throw"),
    `${name}: sync-throwing load rejects the async compile with the message`,
  );

  // (b2) F3: MIXED chains — the maybe-async walk must continue past a missing
  // resolver in both directions: a thenable miss followed by a sync hit, and a
  // sync miss followed by a thenable hit.
  const asyncMiss = { canonicalize: async (url) => { await delay(1); return null; }, load: async () => null };
  const syncHit = {
    canonicalize: (url) => (url === "mx" ? new URL("custom:mx-sync") : null),
    load: (u) => (u.href === "custom:mx-sync" ? { contents: ".mx { from: sync; }", syntax: "scss" } : null),
  };
  const syncMiss = { canonicalize: () => null, load: () => null };
  const asyncHit = {
    canonicalize: async (url) => (url === "mx" ? new URL("custom:mx-async") : null),
    load: async (u) => (u.href === "custom:mx-async" ? { contents: ".mx { from: async; }", syntax: "scss" } : null),
  };
  const mx1 = await mod.compileStringAsync(`@use "mx";`, { importers: [asyncMiss, syncHit] });
  assert.ok(mx1.css.includes("from: sync"), `${name}: async-miss then sync-hit chain resolves`);
  const mx2 = await mod.compileStringAsync(`@use "mx";`, { importers: [syncMiss, asyncHit] });
  assert.ok(mx2.css.includes("from: async"), `${name}: sync-miss then async-hit chain resolves`);

  // (b3) F3: a sync-returning FileImporter on the ASYNC API (findFileUrl
  // returns a plain file: URL) matches the sync API's output.
  const syncFi = { findFileUrl(url) { return url === "shared" ? pathToFileURL(join(root, "fi", "shared")) : null; } };
  const fiSrc = `@use "shared" as s;\n.a { height: s.$s; }\n`;
  const aFi = await mod.compileStringAsync(fiSrc, { importers: [syncFi] });
  assert.equal(aFi.css, mod.compileString(fiSrc, { importers: [syncFi] }).css, `${name}: sync FileImporter on the async API equals the sync API`);

  // (b4) F3: custom functions on the async API — a plain (non-Promise) return
  // takes the fast path with identical output, and a null return is an ERROR
  // ("returned no value"), never a canonicalize-style miss.
  const powSrc = `.a { x: pow(3, 4); }`;
  const powFns = { "pow($base, $exp)": (args) => new mod.SassNumber(args[0].value ** args[1].value) };
  const aPow = await mod.compileStringAsync(powSrc, { functions: powFns });
  assert.equal(aPow.css, mod.compileString(powSrc, { functions: powFns }).css, `${name}: sync-returning custom function on the async API equals the sync API`);
  await assert.rejects(
    () => mod.compileStringAsync(`.a { x: nil(); }`, { functions: { "nil()": () => null } }),
    (e) => e instanceof mod.Exception && e.message.includes("returned no value"),
    `${name}: null-returning custom function rejects the async compile`,
  );

  // (c) logger on the async path: @warn/@debug during compileStringAsync
  // route through asyncHost.host_warn to the user logger (dart shape).
  const aLogged = [];
  const alr = await mod.compileStringAsync('@warn "awmsg"; @debug 40 + 2; .a { b: c; }', {
    logger: {
      warn: (m, o) => aLogged.push(["warn", m, o.deprecation]),
      debug: (m) => aLogged.push(["debug", m]),
    },
  });
  assert.ok(alr.css.includes(".a"), `${name}: async logger compile still emits CSS`);
  assert.deepEqual(aLogged, [["warn", "awmsg", false], ["debug", "42"]], `${name}: async @warn + @debug routed to the logger`);

  // (d) concurrent ISOLATION (pool regression guard): 4 concurrent compiles,
  // each with a DISTINCT importer (same "isomod" specifier, per-compile
  // canonical + content), a DISTINCT logger, and a DISTINCT async custom
  // function — nothing may leak across compiles, including loadedUrls.
  const isoLogs = [[], [], [], []];
  const isoCompile = (i) =>
    mod.compileStringAsync(`@use "isomod";\n@warn "w${i}";\n.o-${i} { t: tag(); }\n`, {
      importers: [{
        async canonicalize(url) { await delay(1); return url === "isomod" ? new URL(`custom:iso-${i}`) : null; },
        async load(u) { await delay(1); return u.href === `custom:iso-${i}` ? { contents: `.uniq-${i} { v: ${i}; }`, syntax: "scss" } : null; },
      }],
      logger: { warn: (m) => isoLogs[i].push(m) },
      functions: { "tag()": async () => { await delay(1); return new mod.SassString(`t${i}`, { quotes: false }); } },
    });
  const iso = await Promise.all([0, 1, 2, 3].map(isoCompile));
  for (let i = 0; i < 4; i++) {
    assert.ok(iso[i].css.includes(`.uniq-${i}`) && iso[i].css.includes(`t${i}`), `${name}: concurrent compile #${i} got its own importer + custom function`);
    for (let j = 0; j < 4; j++) {
      if (j === i) continue;
      assert.ok(!iso[i].css.includes(`.uniq-${j}`) && !iso[i].css.includes(`t${j}`), `${name}: concurrent compile #${i} has no leakage from #${j}`);
    }
    assert.deepEqual(isoLogs[i], [`w${i}`], `${name}: concurrent logger #${i} captured exactly its own warn`);
    assert.deepEqual(iso[i].loadedUrls.map((u) => u.href), [`custom:iso-${i}`], `${name}: loadedUrls #${i} isolated to its own module`);
  }

  // (e) MIXED outcomes under concurrency: the middle compile's load()
  // rejects; the lock (or a future pool slot) must be released on error so
  // the flanking compiles still fulfill with the right CSS.
  const mixOk = (tag) => ({
    canonicalize: async (url) => (url === "mix" ? new URL(`custom:mix-${tag}`) : null),
    load: async () => { await delay(2); return { contents: `.mix-${tag} { m: 1; }`, syntax: "scss" }; },
  });
  const mixBad = {
    canonicalize: async (url) => (url === "mix" ? new URL("custom:mix-bad") : null),
    load: () => Promise.reject(new Error("mid-load-boom")),
  };
  const settled = await Promise.allSettled([
    mod.compileStringAsync(`@use "mix";`, { importers: [mixOk("a")] }),
    mod.compileStringAsync(`@use "mix";`, { importers: [mixBad] }),
    mod.compileStringAsync(`@use "mix";`, { importers: [mixOk("b")] }),
  ]);
  assert.equal(settled[0].status, "fulfilled", `${name}: mixed-outcome compile #0 fulfilled`);
  assert.ok(settled[0].value.css.includes(".mix-a"), `${name}: mixed-outcome compile #0 CSS correct`);
  assert.equal(settled[1].status, "rejected", `${name}: mixed-outcome compile #1 rejected`);
  assert.ok(settled[1].reason instanceof mod.Exception && settled[1].reason.message.includes("mid-load-boom"), `${name}: mixed-outcome rejection carries the load error`);
  assert.equal(settled[2].status, "fulfilled", `${name}: mixed-outcome compile #2 fulfilled`);
  assert.ok(settled[2].value.css.includes(".mix-b"), `${name}: mixed-outcome compile #2 CSS correct`);

  // (f) F1 pool OVERLAP: while compile A is suspended on a gated importer, an
  // independent compile B must run to completion on another engine. Under the
  // old asyncLock this deadlocks (B queued behind A forever), so this is the
  // pool's defining semantic test — keep it FIRST awaiting B, not the gate.
  // Pin the cap >= 2 explicitly: the default is min(4, cores), which is 1 on
  // a cpu-limited CI container — and at cap 1 this test would deadlock.
  mod.configure({ asyncInstances: 2 });
  let releaseGate;
  const gate = new Promise((r) => { releaseGate = r; });
  const blocked = mod.compileStringAsync(`@use "g";`, {
    importers: [{
      canonicalize: async (u) => (u === "g" ? new URL("custom:gated") : null),
      load: async () => { await gate; return { contents: ".gated { ok: 1; }", syntax: "scss" }; },
    }],
  });
  const overlapped = await mod.compileStringAsync(".quick { fast: 1; }");
  assert.ok(overlapped.css.includes(".quick"), `${name}: a compile completes while another is suspended (engine pool overlap)`);
  releaseGate();
  const gated = await blocked;
  assert.ok(gated.css.includes(".gated"), `${name}: the suspended compile completes after its importer resolves`);

  // (g) F1 cap semantics: configure({ asyncInstances: 1 }) serializes again —
  // B must NOT finish while A holds the only engine — and the queue drains in
  // order once the gate opens. Restore the default cap afterwards.
  mod.configure({ asyncInstances: 1 });
  try {
    let release1;
    const gate1 = new Promise((r) => { release1 = r; });
    const holdA = mod.compileStringAsync(`@use "h";`, {
      importers: [{
        canonicalize: async (u) => (u === "h" ? new URL("custom:held") : null),
        load: async () => { await gate1; return { contents: ".held { ok: 1; }", syntax: "scss" }; },
      }],
    });
    let bDone = false;
    const queuedB = mod.compileStringAsync(".b { v: 1; }").then((r) => { bDone = true; return r; });
    await delay(25);
    assert.equal(bDone, false, `${name}: with asyncInstances=1 a second compile queues behind the suspended one`);
    release1();
    const [ra, rb] = await Promise.all([holdA, queuedB]);
    assert.ok(ra.css.includes(".held") && rb.css.includes(".b"), `${name}: the single-engine queue drains after the gate opens`);
  } finally {
    mod.configure({ asyncInstances: 4 });
  }

  // === Phase 4: custom functions (sync path, both builds) ===
  const rfn = mod.compileString(`.a { x: pow(2, 10); }`, {
    functions: { "pow($base, $exp)": (args) => new mod.SassNumber(args[0].value ** args[1].value) },
  });
  assert.ok(rfn.css.includes("x: 1024"), `${name}: sync custom function`);

  console.log(`ok: ${name} build — modern + Compiler API + sync & async importers + custom fns (Phase 1+2+2.5+4)`);
}

// === Packaging guards: what `npm pack` ships must match what the code loads ===
// A .wasm missing from the files array ships a production-broken entry while
// every repo-checkout test stays green (the binaries exist locally); likewise
// the speed entry silently falling back to the size async module is invisible
// to behavior tests. Assert the wiring textually.
{
  const pkg = JSON.parse(readFileSync(new URL("./npm/package.json", import.meta.url), "utf8"));
  const shipped = new Set(pkg.files);
  for (const w of ["sasso.wasm", "sasso.speed.wasm", "sasso.async.wasm", "sasso.speed.async.wasm"]) {
    assert.ok(shipped.has(w), `package.json files array ships ${w}`);
  }
  const speedEntry = readFileSync(new URL("./npm/sasso.speed.mjs", import.meta.url), "utf8");
  assert.ok(speedEntry.includes('"./sasso.speed.wasm"'), "speed entry loads the -O3 sync module");
  assert.ok(speedEntry.includes('"./sasso.speed.async.wasm"'), "speed entry loads the -O3 async module (F2)");

  // sasso/native wiring: subpath exported, wrapper + types shipped, and the
  // runtime platform-package list matches the release generator's target list
  // (a drifted pair ships prebuilds the loader can never resolve).
  assert.ok(pkg.exports["./native"] && pkg.exports["./native"].import === "./native.mjs", "exports map has ./native");
  assert.ok(shipped.has("native.mjs") && shipped.has("native.d.ts"), "files array ships the native wrapper + types");
  const nativeSrc = readFileSync(new URL("./npm/native.mjs", import.meta.url), "utf8");
  const genSrc = readFileSync(new URL("../napi/make-platform-package.mjs", import.meta.url), "utf8");
  for (const target of ["darwin-arm64", "darwin-x64", "linux-x64-gnu", "linux-arm64-gnu"]) {
    assert.ok(nativeSrc.includes(`"sasso-native-${target}"`), `native.mjs resolves sasso-native-${target}`);
    assert.ok(genSrc.includes(`"${target}"`), `make-platform-package.mjs stages ${target}`);
  }
  console.log("ok: packaging — wasm binaries + speed wiring + sasso/native subpath and platform-target consistency");
}

// === Phase 3: CLI (bin) smoke test ===
const cliPath = fileURLToPath(new URL("./npm/cli.mjs", import.meta.url));
const cli = (args, input) =>
  execFileSync(process.execPath, [cliPath, ...args], { input, encoding: "utf8" });

// Not just "version-shaped": the engine's `info` carries dart's compatibility
// version too, and printing THAT looks perfectly valid to a regex.
{
  const pkg = JSON.parse(readFileSync(new URL("./npm/package.json", import.meta.url), "utf8"));
  assert.equal(cli(["--version"]).trim(), pkg.version, "cli: --version prints the package's version");
  assert.equal(
    spawnSync(process.execPath, [cliPath, "--version"], { encoding: "utf8", env: { ...process.env, SASSO_ENGINE: "wasm" } }).stdout.trim(),
    pkg.version,
    "cli: … the same on either engine",
  );
}
assert.ok(cli(["--help"]).includes("Usage: sasso"), "cli: --help");
assert.equal(cli(["--stdin"], ".a{b: 1 + 2}\n").trim(), ".a {\n  b: 3;\n}", "cli: --stdin compile");
assert.equal(cli(["--style=compressed", "--stdin"], ".a{b:1+2}\n").trim(), ".a{b:3}", "cli: --style=compressed");
assert.ok(cli([mainRel]).includes("color: blue"), "cli: file compile resolves relative @use");
assert.ok(cli(["-I", join(root, "inc"), "--stdin"], "@use 'lib' as l;\n.a{width: l.$w}\n").includes("width: 7px"), "cli: -I load-path");
let cliErr = false;
try { cli(["--stdin"], ".a{color:}\n"); } catch { cliErr = true; }
assert.ok(cliErr, "cli: a Sass error exits non-zero");
let cliMissing = false;
// The native CLI's wording, verbatim: one CLI in two implementations.
try { cli(["/no/such/file.scss"]); } catch (e) { cliMissing = /Error reading \/no\/such\/file\.scss: Cannot open file\./.test(String(e.stderr || "")); }
assert.ok(cliMissing, "cli: a missing input file errors cleanly");

// CLI polish flags: --embed-source-map / --quiet / multiple input:output / --update
assert.ok(
  // dart writes `Uri.dataFromString` — percent-encoded JSON, not base64.
  cli(["--embed-source-map", "--stdin"], ".a{b:1}\n").includes(
    "sourceMappingURL=data:application/json;charset=utf-8,%7B%22version%22:3",
  ),
  "cli: --embed-source-map inlines the map as dart's data: URI",
);
{
  const warnSrc = '@warn "x"; .a{b:c}\n';
  const loud = spawnSync(process.execPath, [cliPath, "--stdin"], { input: warnSrc, encoding: "utf8" });
  assert.ok(loud.stderr.includes("WARNING"), "cli: @warn prints to stderr by default");
  const quiet = spawnSync(process.execPath, [cliPath, "--quiet", "--stdin"], { input: warnSrc, encoding: "utf8" });
  assert.equal(quiet.stderr.trim(), "", "cli: --quiet suppresses @warn");
}
{
  const mio = mkdtempSync(join(tmpdir(), "sasso-mio-"));
  const ina = join(mio, "a.scss"), inb = join(mio, "b.scss"), outa = join(mio, "a.css"), outb = join(mio, "b.css");
  writeFileSync(ina, ".a{x:1}\n");
  writeFileSync(inb, ".b{y:2}\n");
  cli(["--quiet", `${ina}:${outa}`, `${inb}:${outb}`]);
  assert.ok(existsSync(outa) && existsSync(outb), "cli: multiple input:output pairs");
  assert.ok(readFileSync(outa, "utf8").includes("x: 1") && readFileSync(outb, "utf8").includes("y: 2"), "cli: multi-IO contents");
  const before = statSync(outa).mtimeMs;
  cli(["--quiet", "--update", `${ina}:${outa}`]);
  assert.equal(statSync(outa).mtimeMs, before, "cli: --update leaves a fresh output untouched");
}
console.log("ok: cli — version/help/stdin/style/file @use/load-path/errors + embed-map/quiet/multi-IO/update");

// === Phase 3b: the npm CLI must accept every flag the NATIVE CLI accepts ===
// These are two separate implementations of one command. The dart-compatible
// flags were added to the Rust CLI (src/main.rs) and this one was left behind,
// so `sasso@0.13.0` rejected every flag a dart-sass build script passes except
// `--quiet` — reported on momiji-rs/sasso#24 by someone whose build it broke.
// This derives the flag set from the Rust parser rather than a hand-kept list,
// so the next flag added there fails here until this CLI takes it too.
{
  const mainRs = readFileSync(new URL("../src/main.rs", import.meta.url), "utf8");
  // Every flag the native parser handles, taken from the parser's own shape —
  // the `"--x" | "-y" => …` match arms and the `--x=value` forms it strips a
  // prefix for — rather than from a fixed slice of the file. The first version
  // of this guard read 8,000 characters after `match a.as_str()`, which would
  // have quietly stopped covering flags added past the cutoff: a drift guard
  // that drifts.
  const found = new Set();
  for (const line of mainRs.split("\n")) {
    const arm = /^\s*("-[^"]*"(?:\s*\|\s*"-[^"]*")*)\s*=>/.exec(line);
    if (arm) for (const m of arm[1].matchAll(/"(-[^"]*)"/g)) found.add(m[1]);
    for (const m of line.matchAll(/strip_prefix\("(--[a-zA-Z-]+)=/g)) found.add(m[1]);
  }
  const flags = [...found];
  assert.ok(flags.length > 25, `drift: extracted a plausible flag set (got ${flags.length})`);
  // If the extraction itself breaks, the set above goes quietly empty-ish and
  // every "is it accepted?" probe below passes vacuously. These are flags the
  // native CLI has had since 0.10.0: their absence means the guard, not the
  // CLI, is what changed.
  for (const flag of ["-s", "--style", "-I", "--load-path", "-o", "--output", "--quiet-deps", "--no-css", "--source-map-urls", "--loop", "--jobs", "--stop-on-error", "--embed-source-map", "--no-unicode"]) {
    assert.ok(found.has(flag), `drift: the extraction still finds ${flag} in src/main.rs`);
  }

  // Flags that take a value, and a value that is valid for each.
  const withValue = {
    "-s": "expanded", "--style": "expanded",
    "-I": ".", "--load-path": ".",
    "-j": "1", "--jobs": "1",
    "--loop": "1",
    "--source-map-urls": "relative",
  };
  // `-o`/`--output` name the output themselves, so they are probed with a
  // POSITIONAL input rather than an `in:out` pair (which they may not be
  // combined with, here or in the native CLI).
  const outputFlags = new Set(["-o", "--output"]);
  // Flags that exit before compiling, so they cannot be probed this way.
  const terminal = new Set(["-h", "--help", "--version", "--"]);

  const dir = mkdtempSync(join(tmpdir(), "sasso-drift-"));
  const src = join(dir, "in.scss");
  writeFileSync(src, ".a{b:1}\n");

  const rejected = [];
  for (const f of flags) {
    if (terminal.has(f)) continue;
    const argv = outputFlags.has(f)
      ? [cliPath, "--no-source-map", f, join(dir, "out.css"), src]
      : [cliPath, "--no-source-map", ...(f in withValue ? [f, withValue[f]] : [f]), `${src}:${join(dir, "out.css")}`];
    const r = spawnSync(process.execPath, argv, { encoding: "utf8" });
    if (/unknown option/.test(r.stderr || "")) rejected.push(f);
  }
  assert.deepEqual(
    rejected,
    [],
    `cli: these flags are accepted by the native CLI and rejected here: ${rejected.join(" ")}`,
  );

  // The shape that actually broke: the flag set a dart-sass build passes.
  const lila = ["--no-error-css", "--stop-on-error", "--no-color", "--quiet", "--quiet-deps"];
  const a = join(dir, "a.scss"), b = join(dir, "b.scss");
  writeFileSync(a, ".a{x:1}\n");
  writeFileSync(b, ".b{y:2}\n");
  const r = spawnSync(
    process.execPath,
    [cliPath, ...lila, "--style=compressed", "--no-source-map", `${a}:${join(dir, "a.css")}`, `${b}:${join(dir, "b.css")}`],
    { encoding: "utf8" },
  );
  assert.equal(r.status, 0, `cli: a dart-sass build's flag set compiles (stderr: ${r.stderr})`);
  assert.equal(readFileSync(join(dir, "a.css"), "utf8").trim(), ".a{x:1}", "cli: dart flag set output a");
  assert.equal(readFileSync(join(dir, "b.css"), "utf8").trim(), ".b{y:2}", "cli: dart flag set output b");
  console.log(`ok: cli flag parity — ${flags.length} native flags, none rejected + a dart-sass build's flag set`);
}

// === Phase 3c: directory pairs, --no-css, --stop-on-error ===
{
  const dir = mkdtempSync(join(tmpdir(), "sasso-dir-"));
  mkdirSync(join(dir, "src", "sub"), { recursive: true });
  writeFileSync(join(dir, "src", "one.scss"), ".a{x:1}\n");
  writeFileSync(join(dir, "src", "sub", "two.scss"), ".b{y:2}\n");
  writeFileSync(join(dir, "src", "_partial.scss"), ".c{z:3}\n");
  cli(["--quiet", "--no-source-map", `${join(dir, "src")}:${join(dir, "out")}`]);
  assert.ok(existsSync(join(dir, "out", "one.css")), "cli: directory pair compiles a top-level file");
  assert.ok(existsSync(join(dir, "out", "sub", "two.css")), "cli: directory pair preserves the tree");
  assert.ok(!existsSync(join(dir, "out", "_partial.css")), "cli: directory pair skips partials");

  // --no-css compiles and writes nothing.
  const nocss = join(dir, "nocss.css");
  cli(["--quiet", "--no-source-map", "--no-css", `${join(dir, "src", "one.scss")}:${nocss}`]);
  assert.ok(!existsSync(nocss), "cli: --no-css writes no output");

  // --stop-on-error stops at the first failure; the default keeps going. Both
  // exit non-zero.
  const bad = join(dir, "bad.scss");
  writeFileSync(bad, ".a{b:}\n");
  const second = join(dir, "second.css");
  // `--stop-on-error` is "don't START more files once one fails", so what it
  // skips depends on how many are already running. At `-j 1` the second job
  // never starts; with the default one-per-CPU it may already have, and the
  // NATIVE CLI behaves the same way (measured 2026-09-17: `sasso
  // --stop-on-error bad:a good:b` writes b, `-j 1` does not). So the
  // deterministic claim is pinned at -j 1, and the parallel case is pinned on
  // what it does guarantee: a non-zero exit.
  const stop = spawnSync(
    process.execPath,
    [cliPath, "--no-source-map", "-j", "1", "--stop-on-error", `${bad}:${join(dir, "s1.css")}`, `${join(dir, "src", "one.scss")}:${second}`],
    { encoding: "utf8" },
  );
  assert.equal(stop.status, 1, "cli: --stop-on-error exits non-zero");
  assert.ok(!existsSync(second), "cli: --stop-on-error -j 1 skips the rest");
  const stopParallel = spawnSync(
    process.execPath,
    [cliPath, "--no-source-map", "--stop-on-error", `${bad}:${join(dir, "s3.css")}`, `${join(dir, "src", "one.scss")}:${join(dir, "s4.css")}`],
    { encoding: "utf8" },
  );
  assert.equal(stopParallel.status, 1, "cli: --stop-on-error exits non-zero in parallel too");
  const go = spawnSync(
    process.execPath,
    [cliPath, "--no-source-map", `${bad}:${join(dir, "s2.css")}`, `${join(dir, "src", "one.scss")}:${second}`],
    { encoding: "utf8" },
  );
  assert.equal(go.status, 1, "cli: a failed job still exits non-zero without --stop-on-error");
  assert.ok(existsSync(second), "cli: without --stop-on-error the rest still compiles");
  console.log("ok: cli — directory pairs, --no-css, --stop-on-error");
}

// === Phase 3d: directory mode follows dart's tree, not Node's Dirent ===
// dart compiles `.css` sources too and FOLLOWS symlinked directories (measured
// against dart-sass 1.104.1 on 2026-09-17); `Dirent.isDirectory()` reports
// false for a directory symlink, which silently dropped whole subtrees.
{
  const dir = mkdtempSync(join(tmpdir(), "sasso-tree-"));
  mkdirSync(join(dir, "src", "sub"), { recursive: true });
  mkdirSync(join(dir, "outside"), { recursive: true });
  writeFileSync(join(dir, "src", "a.scss"), ".a{x:1}\n");
  writeFileSync(join(dir, "src", "sub", "b.sass"), ".b\n  y: 2\n");
  writeFileSync(join(dir, "src", "plain.css"), ".p{z:3}\n");
  writeFileSync(join(dir, "src", "_partial.scss"), ".c{q:4}\n");
  writeFileSync(join(dir, "src", "UPPER.SCSS"), ".u{r:5}\n");
  writeFileSync(join(dir, "outside", "c.scss"), ".o{w:6}\n");
  symlinkSync(join(dir, "outside"), join(dir, "src", "link"));
  cli(["--quiet", "--no-source-map", `${join(dir, "src")}:${join(dir, "built")}`]);
  const built = (rel) => existsSync(join(dir, "built", rel));
  assert.ok(built("a.css"), "cli: directory mode compiles .scss");
  assert.ok(built(join("sub", "b.css")), "cli: directory mode compiles .sass and keeps the tree");
  assert.ok(built("plain.css"), "cli: directory mode compiles a plain .css source");
  assert.ok(built(join("link", "c.css")), "cli: directory mode follows a symlinked directory");
  assert.ok(!built("_partial.css"), "cli: directory mode skips partials");
  assert.ok(!built("UPPER.css"), "cli: directory mode's suffixes are exact-lowercase");

  // A symlink back to an ancestor must not loop: each directory is visited
  // once by canonical identity.
  symlinkSync(join(dir, "src"), join(dir, "src", "sub", "loop"));
  cli(["--quiet", "--no-source-map", `${join(dir, "src")}:${join(dir, "cyc")}`]);
  assert.ok(existsSync(join(dir, "cyc", "a.css")), "cli: a symlink cycle still compiles the tree");
  assert.ok(!existsSync(join(dir, "cyc", "sub", "loop", "a.css")), "cli: a symlink cycle is visited once");

  // A destination nested in the source tree does not mirror itself.
  cli(["--quiet", "--no-source-map", `${join(dir, "src")}:${join(dir, "src", "css")}`]);
  cli(["--quiet", "--no-source-map", `${join(dir, "src")}:${join(dir, "src", "css")}`]);
  assert.ok(existsSync(join(dir, "src", "css", "a.css")), "cli: nested destination compiles the tree");
  assert.ok(!existsSync(join(dir, "src", "css", "css")), "cli: a nested destination is not mirrored into itself");
  console.log("ok: cli — directory mode: .css, symlinks, cycles, nested destination");
}

// === Phase 3e: source maps as dart writes them ===
// The npm CLI used to write absolute `sources`, a base64 data: URI and no blank
// line before the footer — none of which is what dart-sass (or the native CLI)
// produces. Worse, it wrote the `.map` BEFORE creating the output directory, so
// a `<dir>:<dir>` job into a fresh tree died with ENOENT unless --no-source-map
// was passed.
{
  const dir = mkdtempSync(join(tmpdir(), "sasso-map-"));
  mkdirSync(join(dir, "sub"), { recursive: true });
  writeFileSync(join(dir, "in.scss"), '@use "sub/x";\n.e{a:1}\n');
  writeFileSync(join(dir, "sub", "_x.scss"), ".x{b:2}\n");
  const out = join(dir, "deep", "out.css");
  cli([join(dir, "in.scss"), out]); // no --no-source-map: the map is the point
  const map = JSON.parse(readFileSync(out + ".map", "utf8"));
  assert.deepEqual(
    map.sources,
    ["../sub/_x.scss", "../in.scss"],
    "cli: map sources are relative to the .map file (dart's default)",
  );
  assert.equal(map.file, "out.css", "cli: map file field");
  assert.equal(map.sourceRoot, "", "cli: map sourceRoot, as dart writes it");
  assert.ok(
    readFileSync(out, "utf8").endsWith("}\n\n/*# sourceMappingURL=out.css.map */\n"),
    "cli: expanded output has dart's blank line before the footer",
  );

  const abs = join(dir, "abs.css");
  cli(["--source-map-urls=absolute", join(dir, "in.scss"), abs]);
  const absMap = JSON.parse(readFileSync(abs + ".map", "utf8"));
  assert.ok(
    absMap.sources.every((u) => u.startsWith("file://")),
    "cli: --source-map-urls=absolute writes file: URLs",
  );
  assert.notDeepEqual(absMap.sources, map.sources, "cli: --source-map-urls actually changes the map");

  const cmp = join(dir, "cmp.css");
  cli(["--style=compressed", join(dir, "in.scss"), cmp]);
  assert.ok(
    readFileSync(cmp, "utf8").endsWith("}/*# sourceMappingURL=cmp.css.map */\n"),
    "cli: compressed output has no blank line before the footer",
  );

  // An empty stylesheet still terminates a FILE with exactly one newline.
  writeFileSync(join(dir, "empty.scss"), "// nothing\n");
  const emptyOut = join(dir, "empty.css");
  cli(["--no-source-map", join(dir, "empty.scss"), emptyOut]);
  assert.equal(readFileSync(emptyOut, "utf8"), "\n", "cli: an empty stylesheet writes a lone newline to a file");

  // Printing a map to stdout is an error unless it is embedded (dart's rules).
  for (const args of [["--source-map"], ["--embed-sources"], ["--source-map-urls=relative"], ["--source-map-urls=absolute"]]) {
    const r = spawnSync(process.execPath, [cliPath, ...args, join(dir, "in.scss")], { encoding: "utf8" });
    assert.equal(r.status, 1, `cli: ${args[0]} to stdout is rejected`);
    assert.match(r.stderr, /stdout/, `cli: ${args[0]} to stdout explains why`);
  }
  console.log("ok: cli — source maps: relative sources, --source-map-urls, footers, stdout rules");
}

// === Phase 3f: -o/--output, stale output, --no-css on every path ===
{
  const dir = mkdtempSync(join(tmpdir(), "sasso-out-"));
  const src = join(dir, "in.scss");
  writeFileSync(src, ".a{b:1}\n");

  // -o names the output, exactly like a second positional.
  const viaFlag = join(dir, "flag.css"), viaPos = join(dir, "pos.css");
  cli(["--no-source-map", "-o", viaFlag, src]);
  cli(["--no-source-map", src, viaPos]);
  assert.equal(readFileSync(viaFlag, "utf8"), readFileSync(viaPos, "utf8"), "cli: -o matches a positional output");
  cli(["--no-source-map", `--output=${join(dir, "eq.css")}`, src]);
  assert.ok(existsSync(join(dir, "eq.css")), "cli: --output=<file>");
  const bothForms = spawnSync(process.execPath, [cliPath, "-o", viaFlag, `${src}:${viaPos}`], { encoding: "utf8" });
  assert.equal(bothForms.status, 1, "cli: --output with an in:out pair is rejected");
  // Repeating the flag is an assignment in the native parser, not an error:
  // the last one wins. (Naming the output twice in DIFFERENT ways — `-o` plus
  // a second positional — is what it rejects.)
  const first = join(dir, "first.css"), second = join(dir, "second.css");
  cli(["--no-source-map", `--output=${first}`, `--output=${second}`, src]);
  assert.ok(!existsSync(first), "cli: a repeated --output does not write the earlier one");
  assert.ok(existsSync(second), "cli: … it writes the last one");
  const twoWays = spawnSync(process.execPath, [cliPath, "-o", first, src, second], { encoding: "utf8" });
  assert.equal(twoWays.status, 1, "cli: but --output plus a positional output is still rejected");

  // A failed compile drops a stale output (this CLI is always --no-error-css,
  // and dart removes the file rather than leave the last good build in place).
  const stale = join(dir, "stale.css");
  cli(["--no-source-map", src, stale]);
  assert.ok(existsSync(stale), "cli: the first build wrote an output");
  writeFileSync(src, ".a{b:}\n");
  const failed = spawnSync(process.execPath, [cliPath, "--no-source-map", src, stale], { encoding: "utf8" });
  assert.equal(failed.status, 1, "cli: the second build fails");
  assert.ok(!existsSync(stale), "cli: a failed compile removes the stale output");

  // ... unless --no-css, which means no output-side effects at all.
  const kept = join(dir, "kept.css");
  writeFileSync(src, ".a{b:1}\n");
  cli(["--no-source-map", src, kept]);
  writeFileSync(src, ".a{b:}\n");
  spawnSync(process.execPath, [cliPath, "--no-source-map", "--no-css", src, kept], { encoding: "utf8" });
  assert.ok(existsSync(kept), "cli: --no-css leaves an existing output alone, even on failure");

  // --no-css discards stdout output too, not just a file.
  const nocssStdin = spawnSync(process.execPath, [cliPath, "--no-source-map", "--no-css", "--stdin"], {
    input: ".a{b:1}\n",
    encoding: "utf8",
  });
  assert.equal(nocssStdin.status, 0, "cli: --no-css --stdin compiles");
  assert.equal(nocssStdin.stdout, "", "cli: --no-css --stdin writes no CSS");
  console.log("ok: cli — -o/--output, stale output dropped on failure, --no-css everywhere");
}

// === Phase 3g: --quiet-deps silences dependencies, by PROVENANCE ===
// dart's rule is about how a file was REACHED, not where it lives: a stylesheet
// found through a load path is a dependency (and so is whatever it loads
// relatively), but one the entry loads relatively is not — even when it sits
// inside a load-path directory. Only deprecation warnings are dropped; a
// dependency's own @warn still prints. All four measured against dart-sass
// 1.104.1 on 2026-09-17.
{
  const dir = mkdtempSync(join(tmpdir(), "sasso-qd-"));
  mkdirSync(join(dir, "lib"), { recursive: true });
  writeFileSync(join(dir, "lib", "dep.scss"), '@warn "dep-warn";\n.d{color: lighten(#036, 10%)}\n');
  writeFileSync(join(dir, "entry.scss"), '@use "dep";\n.e{color: lighten(#036, 20%)}\n');
  // The same directory, reached relatively from the entry instead.
  writeFileSync(join(dir, "lib", "rel.scss"), ".r{color: lighten(#036, 30%)}\n");
  writeFileSync(join(dir, "entry2.scss"), '@use "lib/rel";\n');

  const run = (args) =>
    spawnSync(process.execPath, [cliPath, "--no-source-map", "-I", join(dir, "lib"), ...args], {
      encoding: "utf8",
      cwd: dir,
    }).stderr;

  // One diagnostic runs from its heading line to the next one (a deprecation
  // block has blank lines INSIDE it, so blank lines do not separate them), and
  // ends in a stack whose FIRST frame is where it was raised. The entry appears
  // in a dependency's stack too, so a diagnostic cannot be attributed by
  // searching it for a file name — an earlier draft of this test did, and could
  // not fail.
  const diagnostics = (out) => {
    const found = [];
    for (const line of out.split("\n")) {
      if (/^(DEPRECATION WARNING|WARNING|DEBUG)\b/.test(line)) found.push(line);
      else if (found.length > 0) found[found.length - 1] += `\n${line}`;
    }
    return found;
  };
  const originOf = (diag) => {
    const m = /^\s+(\S+)\s+\d+:\d+/m.exec(diag);
    return m ? m[1].split(/[\\/]/).pop() : "";
  };
  const deprecationsFrom = (out, file) =>
    diagnostics(out).filter((d) => d.startsWith("DEPRECATION WARNING") && originOf(d) === file);

  const loud = run([join(dir, "entry.scss")]);
  assert.ok(deprecationsFrom(loud, "dep.scss").length > 0, "cli: a dependency's deprecations print by default");
  assert.ok(deprecationsFrom(loud, "entry.scss").length > 0, "cli: the entry's deprecations print by default");

  const quiet = run(["--quiet-deps", join(dir, "entry.scss")]);
  assert.equal(deprecationsFrom(quiet, "dep.scss").length, 0, "cli: --quiet-deps drops a dependency's deprecations");
  assert.ok(deprecationsFrom(quiet, "entry.scss").length > 0, "cli: --quiet-deps keeps the entry's own");
  assert.match(quiet, /WARNING: dep-warn/, "cli: --quiet-deps keeps a dependency's @warn (as dart does)");
  assert.match(quiet, /╷/, "cli: a warning that survives keeps its formatted source snippet");

  const rel = run(["--quiet-deps", join(dir, "entry2.scss")]);
  assert.ok(
    deprecationsFrom(rel, "rel.scss").length > 0,
    "cli: a file loaded relatively is not a dependency, wherever it lives",
  );
  console.log("ok: cli — --quiet-deps by provenance (load path vs relative), @warn kept");
}

// === Phase 3h: the argument grammar the native CLI enforces ===
// Accepting a flag is not implementing it, and accepting an ARGUMENT SHAPE the
// native CLI rejects is its own kind of drift: the same command line then means
// different things depending on which sasso is installed. Every message below
// is the native CLI's, and all but the empty-pair sides are dart-sass 1.104.1's
// own wording (measured 2026-09-17; dart answers an empty side with an I/O
// error instead, which the native CLI deliberately improves on).
{
  const dir = mkdtempSync(join(tmpdir(), "sasso-args-"));
  mkdirSync(join(dir, "src"), { recursive: true });
  mkdirSync(join(dir, "empty"), { recursive: true });
  mkdirSync(join(dir, "partials"), { recursive: true });
  const src = join(dir, "in.scss");
  writeFileSync(src, ".a{b:1}\n");
  writeFileSync(join(dir, "two.scss"), ".b{c:2}\n");
  writeFileSync(join(dir, "src", "a.scss"), ".x{y:2}\n");
  writeFileSync(join(dir, "partials", "_p.scss"), ".p{q:3}\n");
  // A timeout, because half of what this block asserts is that the CLI REFUSES
  // to start work: a count it should have rejected (`--loop=4294967296`) would
  // otherwise run four billion compiles and wedge the suite instead of failing
  // it.
  const run = (args, input) =>
    spawnSync(process.execPath, [cliPath, "--no-source-map", ...args], {
      encoding: "utf8",
      input: input ?? "",
      cwd: dir,
      timeout: 20000,
    });
  const rejects = (args, wanted) => {
    const r = run(args);
    assert.equal(r.status, 1, `cli: ${args.join(" ")} is rejected`);
    assert.ok(r.stderr.includes(wanted), `cli: ${args.join(" ")} says "${wanted}" (got: ${r.stderr.split("\n")[0]})`);
  };

  rejects(["in.scss", "a.css", "b.css"], "Only two positional args may be passed.");
  rejects(["--stdin", "a.css", "b.css"], "Only one argument is allowed with --stdin.");
  rejects([":out.css"], "expected <source>:<destination>");
  rejects(["in.scss:"], "expected <source>:<destination>");
  rejects(["in.scss:out:other.css"], 'may only contain one ":".');
  rejects(["in.scss:one.css", "in.scss:two.css"], 'Duplicate source "in.scss".');
  rejects(["--no-source-map", "--embed-sources", "in.scss", "out.css"], "--embed-sources isn't allowed with --no-source-map.");
  rejects(["--no-source-map", "--embed-source-map", "in.scss", "out.css"], "--embed-source-map isn't allowed with --no-source-map.");
  rejects(["--no-source-map", "--source-map-urls=absolute", "in.scss", "out.css"], "--source-map-urls isn't allowed with --no-source-map.");
  rejects(["--jobs", "0"], "--jobs expects a positive integer");
  rejects(["--loop", "nope"], "--loop expects a positive integer");

  // The same file named by a directory pair AND an explicit pair compiles once,
  // to the destination named last — dart keeps its sources in a path-keyed map.
  // (Spellings are compared lexically against the cwd, as the native CLI does,
  // so these stay relative: on macOS an absolute /var path and the cwd's
  // /private/var realpath are two different keys, there as here.)
  const both = run(["src:out", "src/a.scss:elsewhere.css"]);
  assert.equal(both.status, 0, `cli: a directory pair plus an explicit pair compiles (stderr: ${both.stderr})`);
  assert.ok(existsSync(join(dir, "elsewhere.css")), "cli: the LAST destination wins");
  assert.ok(!existsSync(join(dir, "out", "a.css")), "cli: the earlier destination is not written too");
  // Two spellings of one path are one source (and not a "duplicate" either).
  const spellings = run(["src/a.scss:first.css", "./src/a.scss:second.css"]);
  assert.equal(spellings.status, 0, `cli: two spellings of one source compile (stderr: ${spellings.stderr})`);
  assert.ok(!existsSync(join(dir, "first.css")), "cli: two spellings coalesce …");
  assert.ok(existsSync(join(dir, "second.css")), "cli: … to the later destination");

  // A directory that expands to nothing is not an error (dart exits 0); no
  // input at all still is.
  assert.equal(run([`empty:${join(dir, "e1")}`]).status, 0, "cli: an empty directory pair succeeds");
  assert.equal(run([`partials:${join(dir, "e2")}`]).status, 0, "cli: a directory of only partials succeeds");
  const noInput = run([]);
  assert.equal(noInput.status, 1, "cli: no input at all is an error");
  assert.match(noInput.stderr, /no input file/, "cli: and says so");

  // `-` is standard input, in both the positional and the pair form.
  assert.match(run(["-"], ".s{t:1}\n").stdout, /\.s/, "cli: `-` reads standard input");
  const pairDash = run([`-:${join(dir, "dash.css")}`], ".u{v:2}\n");
  assert.equal(pairDash.status, 0, `cli: \`-\` as a pair source (stderr: ${pairDash.stderr})`);
  assert.match(readFileSync(join(dir, "dash.css"), "utf8"), /\.u/, "cli: and writes its output");
  // A count is a decimal integer TOKEN, not whatever `Number()` will coerce:
  // the native CLI parses it as Rust does, taking `+3` and `03` but refusing
  // `1.0`, `1e3`, `0x2` and anything padded with spaces.
  for (const flag of ["--jobs", "--loop"]) {
    for (const value of ["1.0", "1e3", "0x2", " 3", "3 ", "2_0", "-1", ""]) {
      rejects([`${flag}=${value}`, "in.scss"], `${flag} expects a positive integer`);
    }
    for (const value of ["+3", "03"]) {
      const r = run([`${flag}=${value}`, "in.scss"]);
      assert.equal(r.status, 0, `cli: ${flag}=${value} is accepted, as Rust's parse is (stderr: ${r.stderr})`);
    }
  }

  // A DIRECTORY may not be the output, however it is named. The --stdin path
  // does not go through parseJobs, and used to die with an uncaught EISDIR.
  mkdirSync(join(dir, "adir"), { recursive: true });
  rejects(["in.scss", "adir"], 'Directory "adir" may not be a positional arg.');
  rejects(["-o", "adir", "in.scss"], 'Directory "adir" may not be a positional arg.');
  const stdinDir = run(["--stdin", "adir"], ".a{b:1}\n");
  assert.equal(stdinDir.status, 1, "cli: --stdin with a directory output is rejected");
  assert.match(stdinDir.stderr, /may not be a positional arg\./, "cli: … with the native CLI's message");
  // A pair destination that is a directory only fails on the write, as it does
  // natively — but it fails as an ERROR, not as a raw stack trace.
  const pairDir = run(["in.scss:adir"]);
  assert.equal(pairDir.status, 1, "cli: a directory as a pair destination exits non-zero");
  assert.match(pairDir.stderr, /^error: cannot write adir: /m, "cli: … reporting the write, not throwing");
  assert.ok(!/at \w+ \(node:/.test(pairDir.stderr), "cli: … with no Node stack trace");
  // --indented is documented for stdin, but dart applies it to FILE inputs too
  // (measured 2026-09-17), and so does the native CLI: the extension does not
  // get a vote once it is passed.
  writeFileSync(join(dir, "indented.scss"), ".a\n  b: 1\n");
  const indented = run(["--indented", "indented.scss"]);
  assert.equal(indented.status, 0, `cli: --indented parses a .scss file as Sass (stderr: ${indented.stderr})`);
  assert.match(indented.stdout, /b: 1/, "cli: … and compiles it");
  const notIndented = run(["indented.scss"]);
  assert.equal(notIndented.status, 1, "cli: without --indented the extension decides, and this file is not SCSS");
  // Short options with an ATTACHED value (`-Ilib`, `-j4`). dart's own parser
  // takes them — `sass -Ilib in.scss` compiles, measured against 1.104.1 on
  // 2026-09-17 — so this CLI takes them too, and is pinned here because the
  // NATIVE CLI currently rejects them (momiji-rs/sasso#78): if that parser
  // gains the form, these stay true; if this one ever loses it, a build script
  // written for `sass` breaks.
  mkdirSync(join(dir, "lib"), { recursive: true });
  writeFileSync(join(dir, "lib", "_v.scss"), "$w: 7px;\n");
  writeFileSync(join(dir, "uses.scss"), '@use "v" as v;\n.a{width: v.$w}\n');
  for (const args of [["-I", "lib"], ["-Ilib"], ["--load-path=lib"], ["--load-path", "lib"]]) {
    const r = run([...args, "uses.scss"]);
    assert.equal(r.status, 0, `cli: ${args.join(" ")} resolves the load path (stderr: ${r.stderr})`);
    assert.match(r.stdout, /width: 7px/, `cli: ${args.join(" ")} compiles`);
  }
  assert.equal(run(["-j4", "in.scss"]).status, 0, "cli: -j4 (attached) is accepted, like dart's short options");
  // What dart does NOT take: a value attached to a flag that has none, and a
  // bundle. Both CLIs reject them.
  rejects(["-scompressed", "in.scss"], "unknown option -scompressed");
  rejects(["-qc", "in.scss"], "unknown option -qc");

  // A count that overflows the Rust integer the native parser uses is a
  // rejection, not four billion compiles.
  rejects(["--loop=4294967296", "in.scss"], "--loop expects a positive integer");
  assert.equal(run(["--jobs=4294967296", "in.scss"]).status, 0, "cli: --jobs is a usize natively, so this fits");
  // Mixing the two operand forms, and naming the output twice.
  rejects(["in.scss:out.css", "two.scss"], 'Positional and ":" arguments may not both be used.');
  rejects(["--stdin", "-o", "a.css", "b.css"], "--output requires a single input");
  rejects(["--loop", "2", "--stdin", "out.css"], "--loop compiles to stdout only");
  console.log("ok: cli — the argument grammar: arity, pairs, duplicates, `-`, counts, directories, --indented");
}

// === Phase 3k: symlinks keep the path they were REACHED through ===
// dart-sass resolves a load lexically and leaves symlinks alone: a map names
// the link, not its target — a pnpm `node_modules/<pkg>` path rather than the
// `.pnpm` store it points into, which is what makes such a map navigable. The
// npm package used to `realpathSync` every entry and every resolved import, so
// all three of these named the physical file (measured against dart-sass
// 1.104.1 on 2026-09-17).
{
  const dir = mkdtempSync(join(tmpdir(), "sasso-link-"));
  mkdirSync(join(dir, "real"), { recursive: true });
  mkdirSync(join(dir, "src"), { recursive: true });
  writeFileSync(join(dir, "real", "c.scss"), ".c{d:1}\n");
  symlinkSync(join(dir, "real"), join(dir, "src", "link"));
  symlinkSync(join(dir, "real", "c.scss"), join(dir, "linkfile.scss"));
  const sourcesOf = (mapPath) => JSON.parse(readFileSync(mapPath, "utf8")).sources;

  cli([`${join(dir, "src")}:${join(dir, "out")}`]);
  assert.deepEqual(
    sourcesOf(join(dir, "out", "link", "c.css.map")),
    ["../../src/link/c.scss"],
    "cli: a directory job through a symlink mirrors the logical tree",
  );
  cli([join(dir, "src", "link", "c.scss"), join(dir, "one", "x.css")]);
  assert.deepEqual(
    sourcesOf(join(dir, "one", "x.css.map")),
    ["../src/link/c.scss"],
    "cli: a file reached through a symlinked directory keeps that path",
  );
  cli([join(dir, "linkfile.scss"), join(dir, "two", "y.css")]);
  assert.deepEqual(
    sourcesOf(join(dir, "two", "y.css.map")),
    ["../linkfile.scss"],
    "cli: a symlinked file keeps its own name",
  );

  // The JS API answers the same way — this is the loader's rule, not the CLI's.
  for (const [name, mod] of [["size", size], ["speed", speed]]) {
    const result = mod.compile(join(dir, "linkfile.scss"), { sourceMap: true });
    assert.equal(
      result.loadedUrls[0].href,
      pathToFileURL(join(dir, "linkfile.scss")).href,
      `loadedUrls(${name}): a symlinked entry is named by its link`,
    );
  }
  console.log("ok: symlinks — maps and loadedUrls name the path taken, not the target");
}

// === Phase 3i: --no-unicode, dart's URL encoding, the stdin data: URI ===
{
  const dir = mkdtempSync(join(tmpdir(), "sasso-diag-"));
  mkdirSync(join(dir, "sub"), { recursive: true });
  const dep = join(dir, "dep.scss");
  writeFileSync(dep, ".d{color: lighten(#036, 10%)}\n");
  const glyphs = (args) =>
    spawnSync(process.execPath, [cliPath, "--no-source-map", ...args, dep], { encoding: "utf8" }).stderr;
  assert.match(glyphs([]), /╷/, "cli: diagnostics use the Unicode gutter by default");
  const ascii = glyphs(["--no-unicode"]);
  assert.ok(!/[╷│╵]/.test(ascii), "cli: --no-unicode renders the ASCII glyph set");
  assert.match(ascii, /^\s*,$/m, "cli: … which opens the snippet with a comma, as dart does");

  // dart's URL encoder keeps the sub-delims `!$&'()*+,;=@`; encodeURIComponent
  // escapes `+` and `,`, which would spell these sources differently.
  writeFileSync(join(dir, "sub", "_the+me,1.scss"), ".x{y:2}\n");
  writeFileSync(join(dir, "ent.scss"), '@use "sub/the+me,1" as t;\n.e{a:1}\n');
  const out = join(dir, "out", "a+b,c.css");
  cli([join(dir, "ent.scss"), out]);
  const map = JSON.parse(readFileSync(out + ".map", "utf8"));
  assert.ok(
    map.sources.includes("../sub/_the+me,1.scss"),
    `cli: a source's sub-delims survive the map (got ${JSON.stringify(map.sources)})`,
  );
  assert.equal(map.file, "a+b,c.css", "cli: and so do the output's");
  assert.ok(
    readFileSync(out, "utf8").includes("sourceMappingURL=a+b,c.css.map"),
    "cli: and the footer's",
  );

  // A stdin entry has no path: dart records its TEXT as a data: URI.
  const stdinOut = join(dir, "stdin.css");
  spawnSync(process.execPath, [cliPath, "--stdin", stdinOut], { encoding: "utf8", input: ".a{b:1}\n" });
  const stdinMap = JSON.parse(readFileSync(stdinOut + ".map", "utf8"));
  assert.deepEqual(
    stdinMap.sources,
    ["data:;charset=utf-8,.a%7Bb:1%7D%0A"],
    "cli: a --stdin map names its source by the text, as dart does",
  );

  // --no-css builds no map for an output it is about to discard, and writes
  // neither the CSS nor the sidecar.
  const discarded = join(dir, "none.css");
  const nocss = spawnSync(process.execPath, [cliPath, "--no-css", "--source-map", join(dir, "ent.scss"), discarded], {
    encoding: "utf8",
  });
  assert.equal(nocss.status, 0, `cli: --no-css --source-map compiles (stderr: ${nocss.stderr})`);
  assert.ok(!existsSync(discarded) && !existsSync(discarded + ".map"), "cli: --no-css writes neither CSS nor map");
  console.log("ok: cli — --no-unicode, dart's URL encoding, the stdin data: URI");
}

// === Phase 3j: --loop measures the compiler, on stdout only ===
{
  const dir = mkdtempSync(join(tmpdir(), "sasso-loop-"));
  const src = join(dir, "in.scss");
  writeFileSync(src, '@warn "said once";\n.a{b: 1 + 1}\n');
  const looped = spawnSync(process.execPath, [cliPath, "--loop", "3", src], { encoding: "utf8" });
  assert.equal(looped.status, 0, `cli: --loop compiles (stderr: ${looped.stderr})`);
  assert.match(looped.stdout, /b: 2/, "cli: --loop prints the last CSS");
  assert.match(looped.stderr, /sasso: 3 compiles in .* ms\/compile, .* compiles\/sec/, "cli: --loop reports throughput");
  // An untimed WARM pass runs first and is the one that talks; the timed
  // iterations are silent. So a warning appears exactly once however many
  // times the loop runs — and the number measures compiling rather than the
  // engine's first-compile costs (measured: 3.699 -> 0.114 ms/compile here).
  assert.equal(
    (looped.stderr.match(/said once/g) || []).length,
    1,
    "cli: --loop reports a warning once, from the warm pass",
  );
  const looped9 = spawnSync(process.execPath, [cliPath, "--loop", "9", src], { encoding: "utf8" });
  assert.equal(
    (looped9.stderr.match(/said once/g) || []).length,
    1,
    "cli: … once whatever N is, so the timed loop really is silent",
  );
  // A failure is reported by that same pass, before anything is timed.
  const badLoop = join(dir, "bad.scss");
  writeFileSync(badLoop, ".a{b:}\n");
  const failed = spawnSync(process.execPath, [cliPath, "--loop", "3", badLoop], { encoding: "utf8" });
  assert.equal(failed.status, 1, "cli: --loop on a broken stylesheet exits non-zero");
  assert.match(failed.stderr, /^Error: /m, "cli: … with the compile error");
  assert.ok(!/compiles in/.test(failed.stderr), "cli: … and no throughput line");
  const quietLoop = spawnSync(process.execPath, [cliPath, "--loop", "2", "--no-css", src], { encoding: "utf8" });
  assert.equal(quietLoop.stdout, "", "cli: --loop --no-css prints no CSS");
  assert.match(quietLoop.stderr, /2 compiles/, "cli: … but still reports throughput");
  for (const [args, wanted] of [
    [[`${src}:${join(dir, "out.css")}`], "--loop compiles to stdout only"],
    [["-o", join(dir, "out.css"), src], "--loop compiles to stdout only"],
    [["--source-map", src], "--loop does not generate source maps"],
  ]) {
    const r = spawnSync(process.execPath, [cliPath, "--loop", "2", ...args], { encoding: "utf8" });
    assert.equal(r.status, 1, `cli: --loop ${args.join(" ")} is rejected`);
    assert.ok(r.stderr.includes(wanted), `cli: --loop ${args.join(" ")} says "${wanted}"`);
  }
  console.log("ok: cli — --loop: warm pass, silent timing, stdout only");
}

// === Phase 3l: the CLI's engine choice and its worker pool ===
// The CLI picks the native addon when the platform package is installed and
// falls back to wasm, and it compiles jobs across worker threads. Both are
// invisible in the output BY DESIGN — which is exactly why they need a test
// that pins the output rather than the mechanism.
{
  const dir = mkdtempSync(join(tmpdir(), "sasso-engine-"));
  mkdirSync(join(dir, "src"), { recursive: true });
  // Enough jobs that the pool actually splits them, and varied enough that a
  // shared-state bug would show up as crossed output.
  const expected = new Map();
  for (let i = 0; i < 24; i++) {
    writeFileSync(join(dir, "src", `s${i}.scss`), `.s${i}{a: ${i} + 1; b: "x${i}"}\n`);
    expected.set(`s${i}.css`, `.s${i}{a:${i + 1};b:"x${i}"}`);
  }
  // The engine variables are CLEARED and only what a case asks for is put
  // back. Inheriting them would turn the unforced runs into forced ones: an
  // exported `SASSO_ENGINE=wasm` makes the "default jobs" case test the
  // override, and `SASSO_ENGINE=native` makes the fallback case fail instead
  // of exercising the fallback. Both are supported things to have in a shell.
  const engineEnv = (env) => {
    const base = { ...process.env };
    delete base.SASSO_ENGINE;
    delete base.SASSO_NATIVE_BINARY;
    return { ...base, ...env };
  };
  const compileAll = (out, extra, env) => {
    const args = [cliPath, "--no-source-map", "--style=compressed", ...extra];
    for (const [name] of expected) args.push(`${join(dir, "src", name.replace(".css", ".scss"))}:${join(out, name)}`);
    return spawnSync(process.execPath, args, { encoding: "utf8", env: engineEnv(env), timeout: 60000 });
  };
  const check = (label, out, r) => {
    assert.equal(r.status, 0, `cli: ${label} compiles (stderr: ${r.stderr})`);
    for (const [name, css] of expected) {
      assert.equal(readFileSync(join(out, name), "utf8").trim(), css, `cli: ${label} — ${name} is its own output`);
    }
  };

  const seq = join(dir, "seq");
  check("-j 1", seq, compileAll(seq, ["-j", "1"], {}));
  const par = join(dir, "par");
  check("-j 4 (worker pool)", par, compileAll(par, ["-j", "4"], {}));
  const def = join(dir, "def");
  check("default jobs", def, compileAll(def, [], {}));

  // Both engines, forced, must agree with each other byte for byte.
  const wasm = join(dir, "wasm");
  check("SASSO_ENGINE=wasm", wasm, compileAll(wasm, [], { SASSO_ENGINE: "wasm" }));
  const native = join(dir, "native");
  const nativeRun = compileAll(native, [], { SASSO_ENGINE: "native" });
  if (nativeRun.status === 0) {
    check("SASSO_ENGINE=native", native, nativeRun);
    for (const [name] of expected) {
      assert.equal(
        readFileSync(join(wasm, name), "utf8"),
        readFileSync(join(native, name), "utf8"),
        `cli: the two engines agree on ${name}`,
      );
    }
  } else {
    // No prebuild for this platform: the CLI must still say so clearly rather
    // than falling back silently when the engine was demanded by name.
    assert.match(nativeRun.stderr, /SASSO_ENGINE=native/, "cli: a demanded engine that is missing says so");
  }
  // The same path, forced on EVERY platform: `SASSO_NATIVE_BINARY` is
  // native.mjs's own override, so pointing it at nothing makes the addon
  // unloadable here too. A DEMANDED engine must fail loudly; only the default
  // falls back quietly.
  const demanded = compileAll(join(dir, "nope"), [], {
    SASSO_ENGINE: "native",
    SASSO_NATIVE_BINARY: join(dir, "no-such-addon.node"),
  });
  assert.equal(demanded.status, 1, "cli: SASSO_ENGINE=native with an unloadable addon exits non-zero");
  assert.match(demanded.stderr, /SASSO_ENGINE=native/, "cli: … naming the engine that was demanded");
  const fellBack = join(dir, "fallback");
  check("the default engine falls back to wasm", fellBack, compileAll(fellBack, [], {
    SASSO_NATIVE_BINARY: join(dir, "no-such-addon.node"),
  }));

  // `--help` and `--version` answer from the package alone, so they must work
  // where no engine can be loaded at all — a metadata question must not need a
  // compiler.
  {
    const blind = { ...process.env, SASSO_ENGINE: "native", SASSO_NATIVE_BINARY: join(dir, "no-such-addon.node") };
    const pkg = JSON.parse(readFileSync(new URL("./npm/package.json", import.meta.url), "utf8"));
    const v = spawnSync(process.execPath, [cliPath, "--version"], { encoding: "utf8", env: blind, timeout: 20000 });
    assert.equal(v.status, 0, `cli: --version without a loadable engine (stderr: ${v.stderr})`);
    assert.equal(v.stdout.trim(), pkg.version, "cli: … and it is still the package's version");
    const h = spawnSync(process.execPath, [cliPath, "--help"], { encoding: "utf8", env: blind, timeout: 20000 });
    assert.equal(h.status, 0, `cli: --help without a loadable engine (stderr: ${h.stderr})`);
    // The help prints the negatable spelling, `--[no-]stop-on-error` — the
    // bare flag name matches nothing (this assertion caught itself).
    assert.match(h.stdout, /--\[no-\]stop-on-error/, "cli: … and it is the real help text");
    // And it describes what the flag does now that files run concurrently:
    // the ones already running finish. dart's own wording says exactly that,
    // so use dart's (measured from 1.104.1's --help, 2026-09-17).
    assert.match(
      h.stdout,
      /Don't compile more files once an error is\s+encountered\./,
      "cli: … and --stop-on-error is described as dart describes it",
    );
  }

  // With no output file the CSS goes to the terminal the warnings are on, so
  // the order between them is what the user sees under `2>&1`: dart and the
  // native binary both print the warning during the compile, ahead of the CSS
  // (measured 2026-09-17). Buffering a job's diagnostics must not reverse it.
  //
  // Both streams go to ONE file descriptor — the same thing `2>&1` does — so
  // this reads the real interleaving. Reading two pipes and concatenating them
  // would order the streams by hand and could never fail.
  {
    const sodir = join(dir, "stdout-order");
    mkdirSync(sodir, { recursive: true });
    const src = join(sodir, "warns.scss");
    writeFileSync(src, `@warn "before-the-css";\n.a{x:1}\n`);
    const merged = join(sodir, "merged.log");
    const fd = openSync(merged, "w");
    const r = spawnSync(process.execPath, [cliPath, "--style=compressed", "--no-source-map", src], {
      stdio: ["ignore", fd, fd],
      timeout: 20000,
    });
    closeSync(fd);
    const text = readFileSync(merged, "utf8");
    assert.equal(r.status, 0, `cli: a stdout job with a warning (output: ${text})`);
    assert.ok(text.includes("before-the-css") && text.includes(".a{x:1}"), `cli: both reached the terminal (${text})`);
    assert.ok(
      text.indexOf("before-the-css") < text.indexOf(".a{x:1}"),
      `cli: a stdout job's warning is written before its CSS (got: ${text})`,
    );
  }

  // The headline of the engine work is that the DEFAULT picks the addon. No
  // output test can see that — the two engines are byte-identical on purpose,
  // which is the point — so the only honest observable is throughput, and
  // `--loop` reports it per compile with process start-up and file I/O already
  // out of the way.
  //
  // Measured 2026-09-17 on one 300-rule stylesheet, three runs each:
  // default 0.362-0.376 ms/compile, native 0.374-0.385, wasm 1.403-1.458. The
  // default tracks native and wasm is ~3.9x slower, so the 0.7 threshold below
  // sits about 5x away from both sides. A regression that always loaded
  // `sasso.speed.mjs` would land at the wasm number and fail.
  {
    const ldir = join(dir, "engine-speed");
    mkdirSync(ldir, { recursive: true });
    const big = join(ldir, "big.scss");
    writeFileSync(
      big,
      `@use "sass:math";\n@for $i from 1 through 300 { .c#{$i} { width: math.div($i,3)*1px; color: rgba(0,0,0,math.div($i,100)) } }\n`,
    );
    // `engineEnv` for the same reason as above: inheriting `SASSO_ENGINE=wasm`
    // would make the "default" run measure wasm and fail the comparison below.
    // `undefined` means ONE thing: this platform has no prebuilt addon, which
    // `loadEngine` reports by name. Any other failure is a failure — treating
    // it as "no addon" would skip the default-engine assertions below and let
    // the guard pass while the thing it guards is broken.
    const perCompile = (env) => {
      let best = Infinity;
      for (let k = 0; k < 3; k++) {
        const r = spawnSync(process.execPath, [cliPath, "--loop", "60", "--no-css", big], {
          encoding: "utf8",
          env: engineEnv(env),
          timeout: 60000,
        });
        if (r.status !== 0) {
          assert.match(
            r.stderr,
            /SASSO_ENGINE=native but the addon is unavailable/,
            `cli: --loop failed for a reason other than a missing addon (status ${r.status}: ${r.stderr})`,
          );
          return undefined;
        }
        const m = /=> ([\d.]+) ms\/compile/.exec(r.stderr);
        assert.ok(m, `cli: --loop reports a per-compile time (stderr: ${r.stderr})`);
        best = Math.min(best, Number(m[1]));
      }
      return best;
    };

    const native = perCompile({ SASSO_ENGINE: "native" });
    if (native === undefined) {
      // No prebuild for this platform: there is no addon to prefer, and the
      // "demanded engine is missing" path above already covers saying so.
      console.log("  (no native addon here — default-engine preference not checked)");
    } else {
      const wasm = perCompile({ SASSO_ENGINE: "wasm" });
      const dflt = perCompile({});
      assert.ok(wasm !== undefined && dflt !== undefined, "cli: --loop runs on both engines");
      assert.ok(
        native < wasm * 0.7,
        `cli: the addon is the faster engine here (native ${native} ms, wasm ${wasm} ms) — otherwise this test proves nothing`,
      );
      assert.ok(
        dflt < wasm * 0.7,
        `cli: the DEFAULT engine is the addon, not wasm (default ${dflt} ms, native ${native} ms, wasm ${wasm} ms)`,
      );
    }
  }

  // `-j` must actually run jobs AT THE SAME TIME. Correct output cannot show
  // that, and neither can the "exactly once" guard below — a sequential run
  // satisfies both — so a regression that ignored `-j` and kept the batch in
  // this thread would pass the whole suite.
  //
  // The observable is WRITE ORDER, not a stopwatch: make the first job the
  // slow one and the rest trivial. In order, its output is written first; with
  // workers pulling from the shared index, the others overtake it and it is
  // written last. Measured 2026-09-17, three runs each: `-j 1` wrote
  // `0 1 2 3 4 5 6 7` every time, `-j 4` ended `… 0` every time.
  {
    const cdir = join(dir, "concurrent");
    mkdirSync(cdir, { recursive: true });
    writeFileSync(
      join(cdir, "j0.scss"),
      `@use "sass:math";\n@for $i from 1 through 30000 { .slow-#{$i} { width: math.div($i,3)*1px } }\n`,
    );
    for (let i = 1; i < 8; i++) writeFileSync(join(cdir, `j${i}.scss`), `.j${i}{a:${i}}\n`);
    const writeTimes = (jobs) => {
      for (let i = 0; i < 8; i++) rmSync(join(cdir, `j${i}.css`), { force: true });
      const args = [cliPath, "--no-source-map", "--style=compressed", "-j", String(jobs)];
      for (let i = 0; i < 8; i++) args.push(`${join(cdir, `j${i}.scss`)}:${join(cdir, `j${i}.css`)}`);
      const r = spawnSync(process.execPath, args, { encoding: "utf8", timeout: 60000 });
      assert.equal(r.status, 0, `cli: the -j ${jobs} run compiles (stderr: ${r.stderr})`);
      const times = [];
      for (let i = 0; i < 8; i++) times.push(statSync(join(cdir, `j${i}.css`)).mtimeMs);
      return times;
    };

    const seqTimes = writeTimes(1);
    if (!(seqTimes[0] < seqTimes[7])) {
      // A filesystem whose timestamps are too coarse to separate two writes
      // milliseconds apart cannot answer this question either way.
      console.log("  (file timestamps too coarse to order writes — concurrency not checked)");
    } else {
      for (let attempt = 0; attempt < 3; attempt++) {
        const par = writeTimes(4);
        const last = par.indexOf(Math.max(...par));
        assert.equal(
          last,
          0,
          `cli: -j 4 runs jobs at the same time — the slow FIRST job finishes last (attempt ${attempt}, write times ${par.map((t) => Math.round(t - Math.min(...par))).join(",")})`,
        );
      }
    }
  }

  // `a.scss:a.scss` writes over its own input — dart compiles it and leaves
  // the CSS there (exit 0, measured 2026-09-17, as do both sasso CLIs). It
  // reads before it writes inside ONE job, so there is no order between
  // threads to get wrong and it must not serialize the batch. Same write-order
  // observable as above: the slow first job still has to finish last.
  {
    const sdir = join(dir, "self-write");
    mkdirSync(sdir, { recursive: true });
    for (let attempt = 0; attempt < 3; attempt++) {
      writeFileSync(
        join(sdir, "j0.scss"),
        `@use "sass:math";\n@for $i from 1 through 30000 { .slow-#{$i} { width: math.div($i,3)*1px } }\n`,
      );
      for (let i = 1; i < 8; i++) writeFileSync(join(sdir, `j${i}.scss`), `.j${i}{a:${i}}\n`);
      writeFileSync(join(sdir, "self.scss"), `$c: #2a7ae2;\n.self{color: $c}\n`);
      for (let i = 0; i < 8; i++) rmSync(join(sdir, `j${i}.css`), { force: true });
      const args = [cliPath, "--no-source-map", "--style=compressed", "-j", "4", `${join(sdir, "self.scss")}:${join(sdir, "self.scss")}`];
      for (let i = 0; i < 8; i++) args.push(`${join(sdir, `j${i}.scss`)}:${join(sdir, `j${i}.css`)}`);
      const r = spawnSync(process.execPath, args, { encoding: "utf8", timeout: 60000 });
      assert.equal(r.status, 0, `cli: a self-writing job compiles (stderr: ${r.stderr})`);
      assert.equal(
        readFileSync(join(sdir, "self.scss"), "utf8").trim(),
        ".self{color:#2a7ae2}",
        `cli: the self-writing job replaced its own file (attempt ${attempt})`,
      );
      const times = [];
      for (let i = 0; i < 8; i++) times.push(statSync(join(sdir, `j${i}.css`)).mtimeMs);
      assert.equal(
        times.indexOf(Math.max(...times)),
        0,
        `cli: … and the rest of the batch kept the pool (attempt ${attempt})`,
      );
    }
  }

  // `package.json`'s `files` is a WHITELIST: a module the CLI imports but the
  // list omits is missing from the published tarball, and `npx sasso` dies on
  // start with a resolution error that no test in this repo would see, because
  // every test runs against the working tree where the file is present.
  // Adding `_jobs.mjs` nearly shipped exactly that.
  {
    const pkgDir = new URL("./npm/", import.meta.url);
    const pkg = JSON.parse(readFileSync(new URL("package.json", pkgDir), "utf8"));
    const shipped = new Set(pkg.files);

    // The roots come from the manifest rather than a list kept by hand, so a
    // new export subpath is covered the day it is added: every `./…` the
    // manifest points at is a file the installed package must contain.
    const roots = new Set();
    const collect = (node) => {
      if (typeof node === "string") {
        if (node.startsWith("./")) roots.add(node.slice(2));
      } else if (node && typeof node === "object") {
        for (const value of Object.values(node)) collect(value);
      }
    };
    for (const field of ["bin", "main", "module", "types", "exports"]) collect(pkg[field]);
    assert.ok(roots.has("cli.mjs") && roots.size >= 5, `packaging: found only ${[...roots]}`);

    // `from "./x"` rather than a whole import statement: an import clause may
    // span lines, and a matcher that stops at the first newline silently skips
    // those — which is how `_importer.mjs`, reached only through the multiline
    // imports in `_loader.mjs` and `native.mjs`, went unchecked here.
    const statics = /\bfrom\s*["'](\.\/[^"']+)["']/g;
    const dynamics = /\bimport\(\s*["'](\.\/[^"']+)["']\s*\)/g;
    // `import "./x.mjs"` has no `from` to key on. Nothing in the package does
    // this today, which is exactly why the matcher has to exist: the first one
    // added would otherwise be invisible here.
    const sideEffects = /(?:^|[\s;}])import\s*["'](\.\/[^"']+)["']/g;

    // The matchers are the whole guard, so prove they see each shape rather
    // than trusting that they do. A matcher that silently matches nothing
    // satisfies every assertion below it.
    {
      const sample = [
        'import { a } from "./one.mjs";',
        'import {\n  b,\n  c,\n} from "./two.mjs";',
        'export * from "./three.mjs";',
        'const d = await import("./four.mjs");',
        'import "./five.mjs";',
        'import fs from "node:fs";', // must NOT match: bare specifier
      ].join("\n");
      const found = new Set();
      for (const re of [statics, dynamics, sideEffects]) {
        for (const m of sample.matchAll(re)) found.add(m[1]);
      }
      assert.deepEqual(
        [...found].sort(),
        ["./five.mjs", "./four.mjs", "./one.mjs", "./three.mjs", "./two.mjs"],
        "packaging: the import matchers miss a shape the package may legally use",
      );
    }

    const seen = new Set();
    const queue = [...roots];
    let checked = 0;
    while (queue.length) {
      const name = queue.pop();
      if (seen.has(name)) continue;
      seen.add(name);
      assert.ok(
        shipped.has(name),
        `packaging: the package points at ./${name}, which package.json's "files" does not ship`,
      );
      let text;
      try {
        text = readFileSync(new URL(name, pkgDir), "utf8");
      } catch {
        continue; // a .wasm or .d.ts leaf, nothing to follow
      }
      for (const re of [statics, dynamics, sideEffects]) {
        for (const m of text.matchAll(re)) {
          // Inside a `.d.ts`, TypeScript resolves a `./x.js` specifier to
          // `x.d.ts` — following it literally would demand a file that neither
          // exists nor needs to, while skipping the declaration graph entirely.
          const dep = name.endsWith(".d.ts")
            ? m[1].slice(2).replace(/\.js$/, ".d.ts")
            : m[1].slice(2);
          assert.ok(
            shipped.has(dep),
            `packaging: ${name} imports ./${dep}, which package.json's "files" does not ship`,
          );
          queue.push(dep);
          checked += 1;
        }
      }
    }
    // The traversal is only a guard if it actually reached the module graph;
    // a matcher that quietly matched nothing would pass every assertion above.
    assert.ok(seen.has("_importer.mjs"), "packaging: the walk never reached _importer.mjs");
    assert.ok(checked >= 10, `packaging: followed only ${checked} imports, the walk is not working`);
  }

  // The pool's default size is PHYSICAL cores, not SMT threads: a compile is
  // pure computation, so two hyperthreads on one core contend for the same
  // execution units instead of overlapping stalls. Measured on a Ryzen 7
  // 8745HS (8 cores / 16 threads), 138 Lichess stylesheets: `-j 8` beat
  // `-j 16` by 16% here and 13% through the native binary, and the default
  // taking the core count moved that corpus from 444 ms to 364 ms.
  //
  // The host's own topology cannot be asserted, so the detector is fed
  // synthetic `/proc/cpuinfo` text instead — the shapes that matter are an SMT
  // machine, a dual-socket one (where `core id` repeats per socket), and the
  // containers that publish no topology at all.
  {
    const jobs = await import("./npm/_jobs.mjs");
    const logical = jobs.logicalCpus();

    const smt = Array.from({ length: 8 }, (_v, i) =>
      `processor\t: ${i}\nphysical id\t: 0\ncore id\t: ${i >> 1}\n`,
    ).join("\n");
    assert.equal(jobs.physicalCoresFromCpuinfo(smt), 4, "cli: 8 threads on 4 cores reads as 4");

    // Two sockets, four cores each: `core id` 0-3 appears twice and must not
    // collapse into four.
    const dual = [];
    for (const pkg of [0, 1]) {
      for (let c = 0; c < 4; c++) dual.push(`processor\t: ${pkg * 4 + c}\nphysical id\t: ${pkg}\ncore id\t: ${c}\n`);
    }
    assert.equal(jobs.physicalCoresFromCpuinfo(dual.join("\n")), 8, "cli: two sockets of 4 read as 8, not 4");

    assert.equal(
      jobs.physicalCoresFromCpuinfo("processor\t: 0\nmodel name\t: Whatever\n"),
      undefined,
      "cli: no topology reported means no answer, not zero",
    );

    // A file that names the socket for some processors and not others must not
    // file the later ones under whichever socket happened to come before: two
    // sockets' worth of `core id: 0` are two cores, however incomplete the file.
    assert.equal(
      jobs.physicalCoresFromCpuinfo(
        "processor\t: 0\nphysical id\t: 0\ncore id\t: 0\n\nprocessor\t: 1\ncore id\t: 0\n",
      ),
      2,
      "cli: the socket resets at each processor record",
    );
    // But a file that names no socket at all is still usable: every core lands
    // under one unnamed socket, which is what a single-socket VM reports.
    assert.equal(
      jobs.physicalCoresFromCpuinfo("processor\t: 0\ncore id\t: 0\n\nprocessor\t: 1\ncore id\t: 0\n"),
      1,
      "cli: no socket named at all is one socket, not a giving-up",
    );

    // A cgroup CPU *quota* is not an affinity mask: `docker run --cpus=2`
    // leaves the mask at the whole machine and writes `cpu.max` instead
    // (verified 2026-09-17 — the container reported `Cpus_allowed_list: 0-15`
    // and `cpu.max: 200000 100000`). Node >= 18.14 already answers 2 there;
    // the pre-18.14 fallback answers 16, which is what this covers.
    assert.equal(jobs.quotaCpusFromCgroup({ v2: "200000 100000\n" }), 2, "cli: v2 quota");
    assert.equal(jobs.quotaCpusFromCgroup({ v2: "max 100000\n" }), undefined, "cli: v2 unlimited");
    assert.equal(jobs.quotaCpusFromCgroup({ v2: "150000 100000\n" }), 2, "cli: a fraction rounds up");
    assert.equal(
      jobs.quotaCpusFromCgroup({ v1Quota: "400000\n", v1Period: "100000\n" }),
      4,
      "cli: v1 quota",
    );
    assert.equal(
      jobs.quotaCpusFromCgroup({ v1Quota: "-1\n", v1Period: "100000\n" }),
      undefined,
      "cli: v1 -1 is no limit",
    );
    assert.equal(jobs.quotaCpusFromCgroup({}), undefined, "cli: no cgroup files, no answer");

    // `Cpus_allowed_list` is the affinity mask this process actually has.
    assert.equal(jobs.allowedCpusFromStatus("Cpus_allowed_list:\t0-1,8-9\n"), 4, "cli: ranges and lists");
    assert.equal(jobs.allowedCpusFromStatus("Cpus_allowed_list:\t3\n"), 1, "cli: a single cpu");
    assert.equal(jobs.allowedCpusFromStatus("Name:\tnode\n"), undefined, "cli: absent means no answer");
    assert.equal(jobs.allowedCpusFromStatus("Cpus_allowed_list:\t9-3\n"), undefined, "cli: a backwards range");

    // …and the default that is built on it.
    const readFake = (text) => () => text;
    // These tests are about the topology, so the rest of the host is held
    // still: left real, they would read THIS machine's affinity mask and
    // cgroup, and a run inside a restricted container would fail assertions
    // that have nothing to do with what they are testing.
    const unrestricted = {
      readStatus: () => undefined,
      readCgroup: () => ({}),
    };
    assert.equal(
      jobs.defaultJobs({ platform: "linux", readCpuinfo: readFake(smt), ...unrestricted }),
      Math.min(4, logical),
      "cli: on Linux the default is the core count",
    );
    assert.equal(
      jobs.defaultJobs({ platform: "linux", readCpuinfo: () => undefined, ...unrestricted }),
      logical,
      "cli: an unreadable /proc/cpuinfo falls back to the kernel's count",
    );

    // `os.availableParallelism` only exists on Node >= 18.14, and the package
    // supports >= 16; below it `os.cpus().length` answers the HOST's count and
    // knows nothing of the affinity mask (16 against 4 under `taskset -c
    // 0,1,8,9`, measured 2026-09-17). `Cpus_allowed_list` is what makes the cap
    // hold on every supported Node, so it has to bind even when the reported
    // count is the whole machine.
    const eightCores = Array.from(
      { length: 16 },
      (_v, i) => `processor\t: ${i}\nphysical id\t: 0\ncore id\t: ${i >> 1}\n`,
    ).join("\n");
    assert.equal(
      jobs.defaultJobs({
        platform: "linux",
        readCpuinfo: readFake(eightCores),
        readStatus: readFake("Cpus_allowed_list:\t0-1,8-9\n"),
        readCgroup: () => ({}),
      }),
      Math.min(4, logical),
      "cli: the affinity mask caps the default even when the CPU count does not",
    );
    assert.equal(
      jobs.defaultJobs({
        platform: "linux",
        readCpuinfo: readFake(eightCores),
        readStatus: () => undefined,
        readCgroup: () => ({}),
      }),
      Math.min(8, logical),
      "cli: an unreadable /proc/self/status leaves the core count alone",
    );
    // The container shape that has no mask to find: quota only.
    assert.equal(
      jobs.defaultJobs({
        platform: "linux",
        readCpuinfo: readFake(eightCores),
        readStatus: readFake("Cpus_allowed_list:\t0-15\n"),
        readCgroup: () => ({ v2: "200000 100000\n" }),
      }),
      Math.min(2, logical),
      "cli: a cgroup quota caps the default even with the whole machine in the mask",
    );
    assert.equal(
      jobs.defaultJobs({ platform: "darwin", readCpuinfo: readFake(smt), ...unrestricted }),
      logical,
      "cli: off Linux the logical count stands — Apple silicon has no SMT",
    );
    // A cgroup- or taskset-restricted process sees fewer CPUs than the machine
    // has cores; the smaller number has to win.
    // Sized from the host: a fixed number would stop being "more cores than
    // this process may use" on a big enough machine, and the assertion would
    // then be testing the opposite of what it says.
    const many = Array.from(
      { length: logical + 8 },
      (_v, i) => `physical id\t: 0\ncore id\t: ${i}\n`,
    ).join("\n");
    assert.equal(
      jobs.defaultJobs({ platform: "linux", readCpuinfo: readFake(many), ...unrestricted }),
      logical,
      "cli: never more workers than the kernel offers this process",
    );
  }

  // Each job must run EXACTLY once. Correct output does not prove that — a pool
  // where every worker walks the whole list from 0 produces the same files,
  // just N times over — so make the repetition audible: one `@warn` per
  // stylesheet, counted on stderr.
  {
    const wdir = join(dir, "warn");
    mkdirSync(wdir, { recursive: true });
    const args = [cliPath, "--no-source-map", "--style=compressed", "-j", "4"];
    for (let i = 0; i < 12; i++) {
      writeFileSync(join(wdir, `w${i}.scss`), `@warn "once-${i}";\n.w${i}{a:1}\n`);
      args.push(`${join(wdir, `w${i}.scss`)}:${join(wdir, `w${i}.css`)}`);
    }
    const r = spawnSync(process.execPath, args, { encoding: "utf8", timeout: 60000 });
    assert.equal(r.status, 0, `cli: the warning run compiles (stderr: ${r.stderr})`);
    for (let i = 0; i < 12; i++) {
      const seen = (r.stderr.match(new RegExp(`once-${i}\\b`, "g")) || []).length;
      assert.equal(seen, 1, `cli: job w${i} ran exactly once (saw its @warn ${seen} times)`);
    }
  }

  // A `-` job reads the one stdin there is — and does not drag the other jobs
  // out of the pool with it.
  {
    const sdir = join(dir, "stdin");
    mkdirSync(sdir, { recursive: true });
    const args = [cliPath, "--no-source-map", "--style=compressed", "-j", "4", `-:${join(sdir, "from-stdin.css")}`];
    for (let i = 0; i < 6; i++) {
      writeFileSync(join(sdir, `f${i}.scss`), `.f${i}{a:${i}}\n`);
      args.push(`${join(sdir, `f${i}.scss`)}:${join(sdir, `f${i}.css`)}`);
    }
    // Not ASCII: standard input reaches the worker as shared BYTES, so the
    // content has the same round trip to get wrong as the job paths do.
    const stdin = `.stdin{content:"\u65e5\u672c\u8a9e \u{1f3a8} caf\u00e9";b:1}\n`;
    const r = spawnSync(process.execPath, args, { encoding: "utf8", input: stdin, timeout: 60000 });
    assert.equal(r.status, 0, `cli: a stdin job alongside file jobs (stderr: ${r.stderr})`);
    assert.equal(
      readFileSync(join(sdir, "from-stdin.css"), "utf8").trim(),
      stdin.trim(),
      "cli: the `-` job read stdin, byte for byte",
    );
    for (let i = 0; i < 6; i++) {
      assert.equal(readFileSync(join(sdir, `f${i}.css`), "utf8").trim(), `.f${i}{a:${i}}`, `cli: f${i} compiled too`);
    }
  }

  // Several sources naming one destination: dart compiles them all and the
  // LAST on the command line wins, the same file every run (1.104.1, measured
  // both orders). Run them in parallel and the winner is whoever finishes
  // last, so a collision has to serialize the batch.
  {
    const cdir = join(dir, "collide");
    mkdirSync(cdir, { recursive: true });
    for (const name of ["a", "b", "c"]) writeFileSync(join(cdir, `${name}.scss`), `.${name}{x:"${name}"}\n`);
    const target = join(cdir, "out.css");
    const order = (names) => [
      cliPath, "--no-source-map", "--style=compressed", "-j", "4",
      ...names.map((n) => `${join(cdir, `${n}.scss`)}:${target}`),
    ];
    for (let attempt = 0; attempt < 5; attempt++) {
      for (const names of [["a", "b", "c"], ["c", "b", "a"]]) {
        rmSync(target, { force: true });
        const r = spawnSync(process.execPath, order(names), { encoding: "utf8", timeout: 60000 });
        assert.equal(r.status, 0, `cli: colliding destinations compile (stderr: ${r.stderr})`);
        const last = names[names.length - 1];
        assert.equal(
          readFileSync(target, "utf8").trim(),
          `.${last}{x:"${last}"}`,
          `cli: the LAST source on the command line wins (${names.join(" ")}, attempt ${attempt})`,
        );
      }
    }
  }

  // Diagnostics belong to their job and print in COMMAND-LINE order, never in
  // completion order — which is what a dozen threads writing to one stderr
  // gives you. The first stylesheet is deliberately the slow one, so its
  // warning finishes LAST: an unordered run cannot pass by luck.
  //
  // (Order measured 2026-09-17 against the native binary, which reports each
  // job in input order at every `-j`. dart-sass prints every warning first and
  // its errors at the end; the native CLI has never done that and this does
  // not change it.)
  {
    const odir = join(dir, "order");
    mkdirSync(odir, { recursive: true });
    // The warning comes AFTER the slow loop, so in completion order it is the
    // last one written, not the first.
    writeFileSync(join(odir, "j0.scss"), `@for $i from 1 through 4000 { .slow-#{$i} { a: $i * 2 } }\n@warn "mark-0";\n`);
    for (let i = 1; i < 8; i++) writeFileSync(join(odir, `j${i}.scss`), `@warn "mark-${i}";\n.j${i}{a:${i}}\n`);
    // One failure in the middle: its Error takes the failing job's place in
    // the sequence, rather than being hoisted or trailed.
    writeFileSync(join(odir, "j4.scss"), `.j4{a:}\n`);
    const args = [cliPath, "--no-source-map", "--style=compressed", "-j", "4"];
    for (let i = 0; i < 8; i++) args.push(`${join(odir, `j${i}.scss`)}:${join(odir, `j${i}.css`)}`);

    for (let attempt = 0; attempt < 3; attempt++) {
      const r = spawnSync(process.execPath, args, { encoding: "utf8", timeout: 60000 });
      assert.equal(r.status, 1, "cli: the batch with one bad job exits non-zero");
      const seq = (r.stderr.match(/mark-\d|^Error: /gm) || []).map((m) => (m === "Error: " ? "E" : m));
      assert.deepEqual(
        seq,
        ["mark-0", "mark-1", "mark-2", "mark-3", "E", "mark-5", "mark-6", "mark-7"],
        `cli: diagnostics print in command-line order (attempt ${attempt})`,
      );
      // The block, not just the message: a warning carries its stack frame and
      // ends in a blank line, the shape dart prints.
      assert.match(r.stderr, /WARNING: mark-1\n\s+\S*j1\.scss 1:1\s+root stylesheet\n\n/, "cli: … as whole blocks");
    }
  }

  // The same collision through the sourcemap SIDECAR: `a.scss:out.css` writes
  // `out.css.map` too, which is exactly what `b.scss:out.css.map` writes. The
  // command-line order and the completion order are made to disagree — a.scss
  // is first and slow — so a run that ignores the sidecar writes a's map over
  // b's CSS (measured: dart and `-j 1` keep b's CSS, `-j 4` did not).
  {
    const mdir = join(dir, "sidecar");
    mkdirSync(mdir, { recursive: true });
    writeFileSync(join(mdir, "a.scss"), `@for $i from 1 through 4000 { .slow-#{$i}{a:$i} }\n.a{x:"a"}\n`);
    writeFileSync(join(mdir, "b.scss"), `.b{x:"b"}\n`);
    const target = join(mdir, "out.css.map");
    for (let attempt = 0; attempt < 3; attempt++) {
      rmSync(target, { force: true });
      rmSync(join(mdir, "out.css"), { force: true });
      const r = spawnSync(
        process.execPath,
        [cliPath, "--style=compressed", "-j", "4", `${join(mdir, "a.scss")}:${join(mdir, "out.css")}`, `${join(mdir, "b.scss")}:${target}`],
        { encoding: "utf8", timeout: 60000 },
      );
      assert.equal(r.status, 0, `cli: the sidecar collision compiles (stderr: ${r.stderr})`);
      assert.match(
        readFileSync(target, "utf8"),
        /\.b\{x:"b"\}/,
        `cli: the last job on the command line owns out.css.map, sidecar or not (attempt ${attempt})`,
      );
    }
  }

  // The job list reaches a worker as shared BYTES, decoded on claim, so a path
  // that is not ASCII has to survive the round trip — and a worker must get
  // the right job, not its neighbour's, when the byte lengths differ.
  {
    const udir = join(dir, "unicode");
    mkdirSync(udir, { recursive: true });
    const names = ["\u65e5\u672c\u8a9e", "caf\u00e9", "\u00f6\u00df\u00e9-\u00fc", "emoji-\u{1f3a8}", "plain"];
    const args = [cliPath, "--no-source-map", "--style=compressed", "-j", "4"];
    names.forEach((name, i) => {
      writeFileSync(join(udir, `${name}.scss`), `.n${i}{content:"${name}"}\n`);
      args.push(`${join(udir, `${name}.scss`)}:${join(udir, `${name}.css`)}`);
    });
    const r = spawnSync(process.execPath, args, { encoding: "utf8", timeout: 60000 });
    assert.equal(r.status, 0, `cli: non-ASCII paths compile in the pool (stderr: ${r.stderr})`);
    names.forEach((name, i) => {
      assert.equal(
        readFileSync(join(udir, `${name}.css`), "utf8").trim(),
        `.n${i}{content:"${name}"}`,
        `cli: ${name}.css holds its own output`,
      );
    });
  }

  // Under `--no-css` a repeated destination is not a collision: nothing is
  // written, so there is no last-writer to get right and the batch keeps its
  // parallelism. What must not change is the compiling and the reporting.
  {
    const ndir = join(dir, "nocss");
    mkdirSync(ndir, { recursive: true });
    const target = join(ndir, "out.css");
    const args = [cliPath, "--no-css", "--no-source-map", "-j", "4"];
    for (let i = 0; i < 6; i++) {
      writeFileSync(join(ndir, `n${i}.scss`), `@warn "nocss-${i}";\n.n${i}{a:${i}}\n`);
      args.push(`${join(ndir, `n${i}.scss`)}:${target}`);
    }
    const r = spawnSync(process.execPath, args, { encoding: "utf8", timeout: 60000 });
    assert.equal(r.status, 0, `cli: --no-css with a repeated destination (stderr: ${r.stderr})`);
    assert.ok(!existsSync(target), "cli: --no-css wrote nothing, collision or not");
    const seq = (r.stderr.match(/nocss-\d/g) || []);
    assert.deepEqual(
      seq,
      ["nocss-0", "nocss-1", "nocss-2", "nocss-3", "nocss-4", "nocss-5"],
      "cli: every job still ran, exactly once, reported in command-line order",
    );

    // …and that it KEPT the pool, which the assertions above cannot show: a
    // serialized run produces the same files (none) and the same warnings.
    // Nothing is written under `--no-css`, so write order is no help either.
    //
    // `--stop-on-error` is: run in order, a failing FIRST job stops the rest
    // before they warn; in the pool the others have already started and do
    // warn. Job 0 is slow, so the workers are certainly past their claim by
    // the time it fails. Measured 2026-09-17, five runs each: `-j 1` saw 0
    // warnings every time, `-j 4` saw 5.
    const cdir2 = join(dir, "nocss-conc");
    mkdirSync(cdir2, { recursive: true });
    writeFileSync(
      join(cdir2, "j0.scss"),
      `@for $i from 1 through 30000 { .slow-#{$i}{a:$i} }\n.bad{a: 1px + #fff}\n`,
    );
    for (let i = 1; i < 6; i++) writeFileSync(join(cdir2, `j${i}.scss`), `@warn "conc-${i}";\n.n${i}{a:${i}}\n`);
    const shared = join(cdir2, "out.css");
    const concArgs = (jobs) => {
      const a = [cliPath, "--no-css", "--no-source-map", "--stop-on-error", "-j", String(jobs)];
      for (let i = 0; i < 6; i++) a.push(`${join(cdir2, `j${i}.scss`)}:${shared}`);
      return a;
    };
    const warnCount = (jobs) => {
      const run = spawnSync(process.execPath, concArgs(jobs), { encoding: "utf8", timeout: 60000 });
      assert.notEqual(run.status, 0, `cli: the --no-css -j ${jobs} run fails on its first job`);
      return (run.stderr.match(/conc-\d/g) || []).length;
    };
    // The control: in order, nothing after the failure gets to warn. If this
    // ever stopped being true the comparison below would prove nothing.
    assert.equal(warnCount(1), 0, "cli: --stop-on-error at -j 1 stops the rest before they warn");
    for (let attempt = 0; attempt < 3; attempt++) {
      assert.ok(
        warnCount(4) > 0,
        `cli: a --no-css batch with one destination keeps the pool (attempt ${attempt})`,
      );
    }
  }

  // One job WRITES a path another job READS: `a.scss:b.scss b.scss:out.css`.
  // dart compiles a into b.scss and then b.scss into out.css, so out.css holds
  // a's output; the pool read whichever b.scss it found first. a.scss is the
  // slow one, so a run that does not serialize reads the ORIGINAL b.scss every
  // time (measured 2026-09-17: dart and `-j 1` say a, the pool said b).
  {
    const wdir = join(dir, "write-read");
    mkdirSync(wdir, { recursive: true });
    for (let attempt = 0; attempt < 3; attempt++) {
      writeFileSync(join(wdir, "a.scss"), `@for $i from 1 through 4000 { .slow-#{$i}{a:$i} }\n.from-a{x:1}\n`);
      writeFileSync(join(wdir, "b.scss"), `.original-b{y:2}\n`);
      rmSync(join(wdir, "out.css"), { force: true });
      const r = spawnSync(
        process.execPath,
        [cliPath, "--no-source-map", "--style=compressed", "-j", "4",
         `${join(wdir, "a.scss")}:${join(wdir, "b.scss")}`, `${join(wdir, "b.scss")}:${join(wdir, "out.css")}`],
        { encoding: "utf8", timeout: 60000 },
      );
      assert.equal(r.status, 0, `cli: write-then-read compiles (stderr: ${r.stderr})`);
      const out = readFileSync(join(wdir, "out.css"), "utf8");
      assert.match(out, /from-a/, `cli: the second job read what the first job wrote (attempt ${attempt})`);
      assert.doesNotMatch(out, /original-b/, `cli: … not the file as it was before the batch (attempt ${attempt})`);
    }
  }

  // Diagnostics have to survive the exit. `process.stderr.write` on a PIPE is
  // asynchronous and `process.exit` discards whatever has not reached the
  // kernel, so a batch that printed a lot and then exited non-zero lost the
  // tail of it (measured 2026-09-17: 374 of 400 warnings through a pipe, and
  // a 480 KB error cut to exactly 131072 bytes).
  //
  // The pipe is the point: to a FILE both paths were always whole, so a test
  // that redirects to a file proves nothing. `stdio: "pipe"` is what spawnSync
  // gives us, and the payload has to be bigger than the 64 KB pipe buffer or
  // the write finishes in one go and nothing can be lost.
  {
    const tdir = join(dir, "drain");
    mkdirSync(tdir, { recursive: true });
    const args = [cliPath, "--no-source-map", "--style=compressed"];
    // 400 x ~2 KB is several times the 64 KB pipe buffer. At 400 bytes each the
    // loss was intermittent (374 of 400 once in three runs); at 2 KB the
    // broken version came back with 31-54 of 400, every run.
    const padding = "x".repeat(2000);
    for (let i = 0; i < 400; i++) {
      writeFileSync(join(tdir, `w${i}.scss`), `@warn "mark-${i} ${padding}";\n.w${i}{a:1}\n`);
      args.push(`${join(tdir, `w${i}.scss`)}:${join(tdir, `w${i}.css`)}`);
    }
    // One failure at the end, so the run exits 1 right after the flush.
    writeFileSync(join(tdir, "bad.scss"), `.bad{a:}\n`);
    args.push(`${join(tdir, "bad.scss")}:${join(tdir, "bad.css")}`);
    const r = spawnSync(process.execPath, args, { encoding: "utf8", timeout: 120000 });
    assert.equal(r.status, 1, "cli: the batch with one bad job exits 1");
    const seen = (r.stderr.match(/WARNING: mark-\d+/g) || []).length;
    assert.equal(seen, 400, `cli: every warning survived the non-zero exit through a pipe (saw ${seen})`);

    // The same for the single-error path, which exits from `fail` and cannot
    // wait for a stream to drain — its write has to be synchronous.
    const huge = join(tdir, "huge.scss");
    writeFileSync(huge, `.x{${"a:1;".repeat(60000)}b:}\n`);
    const one = spawnSync(process.execPath, [cliPath, "--no-source-map", huge], {
      encoding: "utf8",
      timeout: 120000,
      maxBuffer: 64 * 1024 * 1024,
    });
    assert.equal(one.status, 1, "cli: the huge broken stylesheet fails");
    assert.ok(
      one.stderr.length > 200_000,
      `cli: a diagnostic larger than the pipe buffer is not cut short (got ${one.stderr.length} bytes)`,
    );
    assert.match(one.stderr, /root stylesheet/, "cli: … and it ends with the stack frame, not mid-line");

    // The single-job paths do not go through the pool, so they have their own
    // copy of this hazard: the engine's logger writes a warning through the
    // asynchronous stream and `fail` then exits at once. Measured 2026-09-17,
    // a 1.2 MB warning followed by an evaluation error: `--stdin` gave 65584
    // bytes through a pipe against 1200070 to a file, `--loop` 65808 against
    // 1200469. The error must arrive, the warning must arrive WHOLE, and the
    // warning must come first.
    const warnThenFail = join(tdir, "warn-then-fail.scss");
    const bigWarning = "w".repeat(1_200_000);
    writeFileSync(warnThenFail, `@warn "kept ${bigWarning}";\n.bad{a: 1px + #fff}\n`);
    const direct = [
      ["--stdin", [cliPath, "--stdin", "--no-source-map"], readFileSync(warnThenFail, "utf8")],
      ["--loop", [cliPath, "--loop", "2", "--no-css", warnThenFail], undefined],
      // A lone positional compiles to stdout and reports through the pool's
      // path; kept here so all three single-job shapes are covered together.
      ["positional", [cliPath, "--no-source-map", warnThenFail], undefined],
    ];
    for (const [label, argv, input] of direct) {
      const d = spawnSync(process.execPath, argv, {
        encoding: "utf8",
        input,
        timeout: 120000,
        maxBuffer: 64 * 1024 * 1024,
      });
      assert.equal(d.status, 1, `cli: ${label} reports the evaluation error`);
      assert.match(d.stderr, /Undefined operation/, `cli: ${label} — the error reached stderr`);
      assert.ok(
        d.stderr.includes(`kept ${bigWarning}`),
        `cli: ${label} — the whole warning reached stderr, not the first 64 KB of it (got ${d.stderr.length} bytes)`,
      );
      assert.ok(
        d.stderr.indexOf("WARNING: kept") < d.stderr.indexOf("Undefined operation"),
        `cli: ${label} — the warning comes before the error that followed it`,
      );
    }
  }

  // Outputs conflict without matching: `a.scss:out` writes the FILE `out` while
  // `b.scss:out/sub.css` needs `out` to be a DIRECTORY, so on a fresh tree
  // whichever job runs first decides which one fails. dart writes the file and
  // then fails the nested job, the same way every run; the pool alternated
  // (measured 2026-09-17: four runs of six left a directory, two left a file).
  {
    const ndir = join(dir, "nested-out");
    for (let attempt = 0; attempt < 4; attempt++) {
      rmSync(ndir, { recursive: true, force: true });
      mkdirSync(ndir, { recursive: true });
      writeFileSync(join(ndir, "a.scss"), `@for $i from 1 through 4000 { .slow-#{$i}{a:$i} }\n.from-a{x:1}\n`);
      writeFileSync(join(ndir, "b.scss"), `.from-b{y:2}\n`);
      const r = spawnSync(
        process.execPath,
        [cliPath, "--no-source-map", "--style=compressed", "-j", "4",
         `${join(ndir, "a.scss")}:${join(ndir, "out")}`, `${join(ndir, "b.scss")}:${join(ndir, "out", "sub.css")}`],
        { encoding: "utf8", timeout: 60000 },
      );
      assert.notEqual(r.status, 0, `cli: the nested-output batch fails, as dart's does (attempt ${attempt})`);
      assert.ok(
        statSync(join(ndir, "out")).isFile(),
        `cli: the first job wrote the file and the nested one lost, as in dart (attempt ${attempt})`,
      );
      const out = readFileSync(join(ndir, "out"), "utf8");
      assert.match(out, /\.from-a\{x:1\}/, "cli: … and it is a's output");
      assert.doesNotMatch(out, /from-b/, "cli: … not b's");
    }
  }

  // The promise of the SHARED index — a heavy stylesheet must not leave other
  // workers idle — is not what "exactly once" checks: fixed contiguous slices
  // also run every job once and write every file correctly.
  //
  // Four heavy jobs first, eight trivial after, `-j 4`. Sharing the index,
  // every worker takes a heavy job, so no trivial output can be written before
  // the first heavy one finishes. Splitting the list into slices leaves two
  // workers holding only trivial jobs, which they write at once. Measured
  // 2026-09-17, five runs each: with the shared index the first trivial write
  // came 0-1 ms AFTER the first heavy one; with slices, 22-23 ms BEFORE it.
  {
    const bdir = join(dir, "balance");
    mkdirSync(bdir, { recursive: true });
    const HEAVY = 4;
    const TOTAL = 12;
    for (let i = 0; i < TOTAL; i++) {
      writeFileSync(
        join(bdir, `j${i}.scss`),
        i < HEAVY
          ? `@use "sass:math";\n@for $j from 1 through 20000 { .h${i}-#{$j} { width: math.div($j,3)*1px } }\n`
          : `.t${i}{a:${i}}\n`,
      );
    }
    for (let attempt = 0; attempt < 3; attempt++) {
      for (let i = 0; i < TOTAL; i++) rmSync(join(bdir, `j${i}.css`), { force: true });
      const args = [cliPath, "--no-source-map", "--style=compressed", "-j", "4"];
      for (let i = 0; i < TOTAL; i++) args.push(`${join(bdir, `j${i}.scss`)}:${join(bdir, `j${i}.css`)}`);
      const r = spawnSync(process.execPath, args, { encoding: "utf8", timeout: 60000 });
      assert.equal(r.status, 0, `cli: the load-balancing run compiles (stderr: ${r.stderr})`);
      const times = [];
      for (let i = 0; i < TOTAL; i++) times.push(statSync(join(bdir, `j${i}.css`)).mtimeMs);
      const firstHeavy = Math.min(...times.slice(0, HEAVY));
      const firstTrivial = Math.min(...times.slice(HEAVY));
      // `>=`, not `>`: with the shared index the two can land in the same
      // millisecond, and that is fine. What must not happen is a trivial
      // output appearing while every heavy job is still running.
      assert.ok(
        firstTrivial >= firstHeavy,
        `cli: no worker was left holding only cheap jobs (attempt ${attempt}, first trivial ${Math.round(
          firstTrivial - firstHeavy,
        )} ms before the first heavy one)`,
      );
    }
  }

  // A failure inside the pool is still reported and still exits non-zero.
  writeFileSync(join(dir, "src", "s7.scss"), ".s7{a:}\n");
  const broken = compileAll(join(dir, "broken"), ["-j", "4"], {});
  assert.equal(broken.status, 1, "cli: a job that fails in a worker exits non-zero");
  assert.match(broken.stderr, /Error: /, "cli: … and its diagnostic reaches stderr");
  console.log("ok: cli — engine selection (wasm/native agree) and the worker pool");
}

// === The `quietDeps` option, on the JS API and on both engines ===
{
  const importer = {
    canonicalize: (u) => (u.startsWith("virt:") ? new URL(u) : null),
    load: () => ({ contents: '@warn "dep-warn";\n.v{color: lighten(#036, 10%)}', syntax: "scss" }),
  };
  const collect = (mod, quietDeps) => {
    const seen = [];
    mod.compileString('@use "virt:a" as v;\n', {
      url: "file:///entry.scss",
      importers: [importer],
      quietDeps,
      logger: {
        warn: (m, o) => seen.push(`${o.deprecation ? "DEPRECATION" : "WARNING"}:${m.split("\n")[0]}`),
        debug: (m) => seen.push(`DEBUG:${m}`),
      },
    });
    return seen;
  };
  for (const [name, mod] of [["size", size], ["speed", speed]]) {
    const loud = collect(mod, false);
    assert.ok(loud.some((w) => w.startsWith("DEPRECATION")), `quietDeps(${name}): deprecations warn by default`);
    const quiet = collect(mod, true);
    assert.ok(!quiet.some((w) => w.startsWith("DEPRECATION")), `quietDeps(${name}): silenced for an importer's stylesheet`);
    assert.ok(quiet.some((w) => w.includes("dep-warn")), `quietDeps(${name}): @warn still reaches the logger`);
  }
  // The asyncify engine takes the same path (a separate wasm instance).
  {
    const seen = [];
    await size.compileStringAsync('@use "virt:a" as v;\n', {
      url: "file:///entry.scss",
      importers: [importer],
      quietDeps: true,
      logger: { warn: (m, o) => seen.push(`${o.deprecation ? "DEPRECATION" : "WARNING"}:${m.split("\n")[0]}`) },
    });
    assert.ok(!seen.some((w) => w.startsWith("DEPRECATION")), "quietDeps(async): silenced");
    assert.ok(seen.some((w) => w.includes("dep-warn")), "quietDeps(async): @warn kept");
  }
  console.log("ok: quietDeps — dependencies silenced, @warn kept, sync + async engines");
}

// === Trailing-newline parity: the JS API omits it, the CLI appends one ===
// dart-sass's `compileString().css` carries NO trailing newline (either style);
// the CLI terminates NON-empty output with exactly one (empty output stays
// empty). These pin both halves so the wasm package stays byte-identical to
// dart-sass on both the sass-loader/Vite path (JS API) and the CLI path.
{
  for (const style of ["expanded", "compressed"]) {
    for (const mod of [size, speed]) {
      const out = mod.compileString(".a { color: red; }", { style }).css;
      assert.ok(out.length > 0, `nl: ${style} css non-empty`);
      assert.ok(!out.endsWith("\n"), `nl: JS API ${style} css has NO trailing newline`);
    }
    // CLI: exactly one trailing newline on non-empty output.
    const styleArg = style === "compressed" ? ["--style=compressed"] : [];
    const cliOut = cli([...styleArg, "--stdin"], ".a { color: red; }\n");
    assert.ok(cliOut.endsWith("\n") && !cliOut.endsWith("\n\n"), `nl: CLI ${style} ends with exactly one newline`);
  }
  // Empty-output stylesheet: JS API "" and CLI 0 bytes (no lone newline) — dart parity.
  assert.equal(size.compileString("$x: 1;").css, "", "nl: empty-output JS API css is empty");
  assert.equal(cli(["--stdin"], "$x: 1;\n"), "", "nl: empty-output CLI emits nothing");
  console.log("ok: trailing-newline parity — JS API omits, CLI appends one (empty stays empty)");
}

// Polish: structured Exception (sassMessage + span), shape verified vs dart-sass
{
  let caught;
  try {
    size.compileString(".a { color: ; }", { url: "file:///x.scss" });
  } catch (e) {
    caught = e;
  }
  assert.equal(caught.name, "Exception", "error: name is Exception");
  assert.ok(caught instanceof Error, "error: instanceof Error");
  assert.equal(caught.span.url, "file:///x.scss", "error: span.url");
  assert.equal(caught.span.start.line, 0, "error: span.start.line is 0-based");
  assert.equal(caught.span.start.column, 12, "error: span.start.column (matches dart)");
  assert.ok(caught.message.startsWith("Error:"), "error: message is the rendered block");
  assert.ok(caught.sassMessage.length > 0 && !caught.sassMessage.includes("\n"), "error: sassMessage is a raw one-liner");
  console.log("ok: structured Exception (sassMessage + span)");
}

// === Phase 3: CLI --watch (recompiles on dependency change) ===
{
  const waitFor = async (pred, timeoutMs) => {
    const deadline = Date.now() + timeoutMs;
    while (Date.now() < deadline) {
      if (pred()) return true;
      await new Promise((r) => setTimeout(r, 50));
    }
    return false;
  };
  const wdir = mkdtempSync(join(tmpdir(), "sasso-watch-"));
  writeFileSync(join(wdir, "main.scss"), `@use "v" as v;\n.a { color: v.$c; }\n`);
  writeFileSync(join(wdir, "_v.scss"), `$c: red;\n`);
  const outFile = join(wdir, "out.css");
  const proc = spawn(process.execPath, [cliPath, "--watch", join(wdir, "main.scss"), outFile], { stdio: "ignore" });
  try {
    assert.ok(await waitFor(() => existsSync(outFile) && readFileSync(outFile, "utf8").includes("red"), 10000), "cli --watch: initial compile");
    writeFileSync(join(wdir, "_v.scss"), `$c: blue;\n`); // change a DEPENDENCY, not the entry
    assert.ok(await waitFor(() => readFileSync(outFile, "utf8").includes("blue"), 10000), "cli --watch: recompiles on dependency change");
    console.log("ok: cli --watch — initial + recompile on dependency change");
  } finally {
    proc.kill();
  }
}

// === Phase 4: custom functions — full Value coverage (sync + async) ===
{
  const { SassNumber, SassString, SassColor, SassList, SassMap, sassTrue, sassFalse, sassNull } = size;

  // number with units
  const rn = size.compileString(`.a { w: rem(32); }`, {
    functions: { "rem($px)": (a) => new SassNumber(a[0].assertNumber().value / 16, "rem") },
  });
  assert.ok(rn.css.includes("w: 2rem"), "fn: number with unit");

  // string assert + quotes
  const rs = size.compileString(`.a { content: shout("hi"); }`, {
    functions: { "shout($s)": (a) => new SassString(a[0].assertString().text.toUpperCase() + "!", { quotes: true }) },
  });
  assert.ok(rs.css.includes('"HI!"'), "fn: string in/out");

  // color in/out (read channel, build a new color)
  const rc = size.compileString(`.a { color: setred(rgb(1, 2, 3)); }`, {
    functions: { "setred($c)": (a) => { const c = a[0].assertColor(); return new SassColor({ red: 255, green: c.green, blue: c.blue, alpha: c.alpha }); } },
  });
  assert.ok(rc.css.includes("#ff0203"), "fn: color in/out");

  // modern color space round-trip (oklch built in JS)
  const rok = size.compileString(`.a { color: brand(); }`, {
    functions: { "brand()": () => new SassColor({ space: "oklch", lightness: 0.7, chroma: 0.15, hue: 250, alpha: 1 }) },
  });
  assert.ok(rok.css.includes("oklch("), "fn: modern color space");

  // list + map args, boolean/null returns
  // list arg -> immutable List (.size / .get), incl. negative indexing
  const rl = size.compileString(`.a { n: len((a, b, c)); l: last((a, b, c)); }`, {
    functions: {
      "len($l)": (a) => new SassNumber(a[0].asList.size),
      "last($l)": (a) => a[0].get(-1),
    },
  });
  assert.ok(rl.css.includes("n: 3") && rl.css.includes("l: c"), "fn: list arg (immutable List + negative get)");
  // map arg -> value-equality lookup via .contents.get (dart-sass shape)
  const rm = size.compileString(`.a { v: pick((x: 1, y: 2), y); }`, {
    functions: { "pick($m, $k)": (a) => a[0].assertMap().contents.get(a[1]) ?? sassNull },
  });
  assert.ok(rm.css.includes("v: 2"), "fn: map arg + value-equality get");

  // rest args ($args...)
  const rr = size.compileString(`.a { s: total(1, 2, 3, 4); }`, {
    functions: { "total($nums...)": (a) => new SassNumber(a[0].asList.reduce((s, n) => s + n.value, 0)) },
  });
  assert.ok(rr.css.includes("s: 10"), "fn: rest args");

  // Tier 0/1: sassIndexToListIndex (1-based + negative), tryMap, assertNoUnits
  const rt = size.compileString(`.a { x: nth((10, 20, 30), -1); }`, {
    functions: {
      "nth($l, $i)": (a) => a[0].get(a[0].sassIndexToListIndex(a[1], "i")),
    },
  });
  assert.ok(rt.css.includes("x: 30"), "fn: sassIndexToListIndex negative");
  const rempty = size.compileString(`.a { x: ismap(()); }`, {
    functions: { "ismap($v)": (a) => (a[0].tryMap() ? sassTrue : sassFalse) },
  });
  assert.ok(rempty.css.includes("x: true"), "fn: tryMap on empty list");

  // a custom function overrides a builtin global, loses to a user @function
  const rov = size.compileString(`.a { x: type-of(1); }`, {
    functions: { "type-of($v)": () => new SassString("custom", { quotes: false }) },
  });
  assert.ok(rov.css.includes("x: custom"), "fn: overrides builtin");

  // error from a function surfaces as a compile error
  let fnErr;
  try { size.compileString(`.a { x: boom(1); }`, { functions: { "boom($x)": () => { throw new Error("kaboom"); } } }); } catch (e) { fnErr = e; }
  assert.ok(fnErr && /kaboom/.test(fnErr.message), "fn: error surfaces");

  // async custom function suspends/resumes the engine
  const ra = await size.compileStringAsync(`.a { x: aplus(40); }`, {
    functions: { "aplus($n)": async (a) => { await new Promise((r) => setTimeout(r, 2)); return new SassNumber(a[0].value + 2); } },
  });
  assert.ok(ra.css.includes("x: 42"), "fn: async custom function");

  // a Promise-returning function is rejected on the SYNC path
  assert.throws(
    () => size.compileString(`.a { x: ap(1); }`, { functions: { "ap($n)": async () => new SassNumber(1) } }),
    /asynchronous custom functions require/,
    "fn: sync path rejects async function",
  );

  // boolean / sassTrue usable
  const rb = size.compileString(`.a { x: yes(); }`, { functions: { "yes()": () => sassTrue } });
  assert.ok(rb.css.includes("x: true"), "fn: boolean return");

  // Tier 2: engine-routed SassNumber unit conversion (standalone + re-entrant)
  assert.equal(new SassNumber(96, "px").convert(["in"], []).value, 1, "Tier2: convert 96px -> 1in (standalone)");
  assert.equal(new SassNumber(1, "in").convertValue(["px"], []), 96, "Tier2: convertValue 1in -> 96px");
  assert.equal(new SassNumber(5).coerce(["px"], []).toString(), "5px", "Tier2: coerce unitless");
  assert.equal(new SassNumber(1, "in").compatibleWithUnit("px"), true, "Tier2: compatibleWithUnit true");
  assert.equal(new SassNumber(1, "s").compatibleWithUnit("px"), false, "Tier2: compatibleWithUnit false");
  assert.throws(() => new SassNumber(1, "s").convert(["px"], []), /can't be converted/, "Tier2: incompatible convert throws");
  const rconv = size.compileString(`.a { w: topx(2in); }`, {
    functions: { "topx($n)": (a) => a[0].assertNumber().convert(["px"], []) },
  });
  assert.ok(rconv.css.includes("w: 192px"), "Tier2: re-entrant convert inside a custom function");
  const rconvA = await size.compileStringAsync(`.a { w: topx(1in); }`, {
    functions: { "topx($n)": async (a) => a[0].assertNumber().convertToMatch(new SassNumber(0, "px")) },
  });
  assert.ok(rconvA.css.includes("w: 96px"), "Tier2: re-entrant convert in an async custom function");

  // Tier 2b: engine-routed SassColor space conversion (standalone + re-entrant)
  const red = new SassColor({ red: 255, green: 0, blue: 0 });
  assert.equal(red.toSpace("oklch").space, "oklch", "Tier2: toSpace returns target space");
  assert.ok(Math.abs(red.toSpace("oklch").channel("lightness") - 0.628) < 0.01, "Tier2: oklch lightness of red");
  assert.equal(red.channel("lightness", { space: "hsl" }), 50, "Tier2: channel(name,{space})");
  assert.equal(new SassColor({ space: "oklch", lightness: 0.7, chroma: 0.15, hue: 250 }).isInGamut("srgb"), true, "Tier2: isInGamut");
  const rcolor = size.compileString(`.a { l: light(#3366cc); }`, {
    functions: { "light($c)": (a) => new SassNumber(Math.round(a[0].assertColor().toSpace("hsl").channel("lightness"))) },
  });
  assert.ok(rcolor.css.includes("l: 50"), "Tier2: re-entrant toSpace inside a custom function");

  // Tier 2c: change / interpolate / isChannelPowerless
  assert.equal(red.change({ green: 128 }).toSpace("rgb").channels.toArray().join(","), "255,128,0", "Tier2c: change channel");
  assert.equal(red.change({ space: "oklch", lightness: 0.9 }).channel("lightness"), 0.9, "Tier2c: change with space");
  assert.equal(
    red.interpolate(new SassColor({ red: 0, green: 0, blue: 255 }), { weight: 0.5, method: "srgb" }).toSpace("rgb").channels.toArray().map(Math.round).join(","),
    "128,0,128",
    "Tier2c: interpolate",
  );
  assert.equal(new SassColor({ space: "hsl", hue: 0, saturation: 0, lightness: 50 }).isChannelPowerless("hue"), true, "Tier2c: isChannelPowerless");

  // Tier 3a: SassCalculation round-trip (receive + inspect, and return)
  const { SassCalculation, CalculationOperation } = size;
  const rcalcIn = size.compileString(`.a { x: probe(calc(1px + 2%)); }`, {
    functions: {
      "probe($c)": (a) => {
        const c = a[0].assertCalculation();
        const op = c.arguments.get(0);
        return new SassString(`${c.name}|${op.operator}|${op.left}|${op.right}`, { quotes: true });
      },
    },
  });
  assert.ok(rcalcIn.css.includes('"calc|+|1px|2%"'), "Tier3a: receive + inspect calc()");
  const rcalcOut = size.compileString(`.a { width: build(); }`, {
    functions: { "build()": () => SassCalculation.calc(new CalculationOperation("+", new SassNumber(1, "px"), new SassNumber(2, "%"))) },
  });
  assert.ok(rcalcOut.css.includes("width: calc(1px + 2%)"), "Tier3a: return a SassCalculation");
  const rcalcMin = size.compileString(`.a { width: mn(); }`, {
    functions: { "mn()": () => SassCalculation.min([new SassNumber(10, "px"), new SassString("var(--x)", { quotes: false })]) },
  });
  assert.ok(rcalcMin.css.includes("width: min(10px, var(--x))"), "Tier3a: return min() with var()");

  // Tier 3b: first-class function/mixin refs round-trip as opaque handles
  const rfnref = size.compileString(
    `@use "sass:meta";\n@function double($x) { @return $x * 2; }\n.a { x: meta.call(passthru(meta.get-function("double")), 5); }`,
    { functions: { "passthru($f)": (a) => a[0].assertFunction() } },
  );
  assert.ok(rfnref.css.includes("x: 10"), "Tier3b: SassFunction opaque round-trip (meta.call)");
  const rmixref = size.compileString(
    `@use "sass:meta";\n@mixin paint { color: red; }\n.a { @include meta.apply(passmix(meta.get-mixin("paint"))); }`,
    { functions: { "passmix($m)": (a) => a[0].assertMixin() } },
  );
  assert.ok(rmixref.css.includes("color: red"), "Tier3b: SassMixin opaque round-trip (meta.apply)");

  // Polish: unit-aware SassNumber equality + hashCode (verified == dart-sass 1.101)
  const inch = new SassNumber(1, "in");
  assert.equal(inch.equals(new SassNumber(96, "px")), true, "equals: 1in == 96px");
  assert.equal(inch.hashCode() === new SassNumber(96, "px").hashCode(), true, "equals: 1in/96px hash equal");
  assert.equal(inch.equals(new SassNumber(2, "px")), false, "equals: 1in != 2px");
  assert.equal(new SassNumber(1).equals(new SassNumber(1, "px")), false, "equals: 1 != 1px (unitless vs united)");
  assert.equal(inch.equals(new SassNumber(1, "s")), false, "equals: 1in != 1s (incompatible)");
  assert.equal(new SassNumber(0.1 + 0.2).equals(new SassNumber(0.3)), true, "equals: 0.1+0.2 == 0.3 (fuzzy)");
  const mUnit = new SassMap(new Map([[inch, new SassString("hit", { quotes: true })]]));
  assert.equal(mUnit.contents.get(new SassNumber(96, "px"))?.text, "hit", "equals: SassMap key 1in matched by 96px");

  // Polish: assert / index error messages — byte-for-byte vs dart-sass 1.101
  const expectMsg = (fn, want, label) => {
    let msg = null;
    try {
      fn();
    } catch (e) {
      msg = e.message;
    }
    assert.equal(msg, want, label);
  };
  expectMsg(() => new SassString("hi").assertNumber(), '"hi" is not a number.', "msg: assertNumber");
  expectMsg(() => new SassNumber(5).assertString("foo"), "$foo: 5 is not a string.", "msg: assertString named");
  expectMsg(() => new SassNumber(5).assertColor(), "5 is not a color.", "msg: assertColor");
  expectMsg(() => new SassNumber(5).assertFunction(), "5 is not a function reference.", "msg: assertFunction");
  expectMsg(() => new SassNumber(5).assertMixin(), "5 is not a mixin reference.", "msg: assertMixin");
  expectMsg(() => new SassNumber(5.5).assertInt(), "5.5 is not an int.", "msg: assertInt");
  expectMsg(() => new SassNumber(5, "px").assertUnit("em"), 'Expected 5px to have unit "em".', "msg: assertUnit");
  expectMsg(() => new SassNumber(5, "px").assertNoUnits("foo"), "$foo: Expected 5px to have no units.", "msg: assertNoUnits named");
  expectMsg(() => new SassNumber(5).assertInRange(0, 3), "Expected 5 to be within 0 and 3.", "msg: assertInRange");
  const idxList = new SassList([new SassNumber(1), new SassNumber(2)]);
  expectMsg(() => idxList.sassIndexToListIndex(new SassNumber(0)), "List index may not be 0.", "msg: index 0");
  expectMsg(() => idxList.sassIndexToListIndex(new SassNumber(9)), "Invalid index 9 for a list with 2 elements.", "msg: index out of range");
  expectMsg(() => new SassString("hi").sassIndexToStringIndex(new SassNumber(9)), "Invalid index 9 for a string with 2 characters.", "msg: string index out of range");

  // Polish: logger option — @warn / @debug routed to the JS logger (dart shape)
  const logged = [];
  size.compileString('@warn "wmsg"; @debug 1 + 2; .a { b: c; }', {
    logger: {
      warn: (m, o) => logged.push(["warn", m, o.deprecation]),
      debug: (m) => logged.push(["debug", m]),
    },
  });
  assert.deepEqual(logged, [["warn", "wmsg", false], ["debug", "3"]], "logger: @warn + @debug routed");
  assert.equal(typeof size.Logger.silent.warn, "function", "logger: Logger.silent present");

  // Polish: charset option (verified == dart-sass 1.101)
  const nonAscii = '.a { content: "café"; }';
  assert.ok(size.compileString(nonAscii).css.startsWith("@charset"), "charset: default emits @charset");
  assert.ok(!size.compileString(nonAscii, { charset: false }).css.startsWith("@charset"), "charset: false suppresses @charset");
  assert.equal(size.compileString(nonAscii, { style: "compressed" }).css.charCodeAt(0), 0xfeff, "charset: compressed default BOM");
  assert.notEqual(size.compileString(nonAscii, { style: "compressed", charset: false }).css.charCodeAt(0), 0xfeff, "charset: compressed false no BOM");

  console.log("ok: custom functions — number/string/color/list/map/rest, override, error, async (Phase 4)");
}

console.log("all wasm modern-API + importer + CLI + custom-function tests passed");
