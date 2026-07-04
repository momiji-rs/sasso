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
import { writeFileSync, mkdtempSync, mkdirSync, readFileSync, existsSync, statSync } from "node:fs";
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
  console.log("ok: packaging — files array ships all four wasm binaries; speed entry wired to -O3 modules");
}

// === Phase 3: CLI (bin) smoke test ===
const cliPath = fileURLToPath(new URL("./npm/cli.mjs", import.meta.url));
const cli = (args, input) =>
  execFileSync(process.execPath, [cliPath, ...args], { input, encoding: "utf8" });

assert.match(cli(["--version"]).trim(), /^\d+\.\d+\.\d+/, "cli: --version prints a version");
assert.ok(cli(["--help"]).includes("Usage: sasso"), "cli: --help");
assert.equal(cli(["--stdin"], ".a{b: 1 + 2}\n").trim(), ".a {\n  b: 3;\n}", "cli: --stdin compile");
assert.equal(cli(["--style=compressed", "--stdin"], ".a{b:1+2}\n").trim(), ".a{b:3}", "cli: --style=compressed");
assert.ok(cli([mainRel]).includes("color: blue"), "cli: file compile resolves relative @use");
assert.ok(cli(["-I", join(root, "inc"), "--stdin"], "@use 'lib' as l;\n.a{width: l.$w}\n").includes("width: 7px"), "cli: -I load-path");
let cliErr = false;
try { cli(["--stdin"], ".a{color:}\n"); } catch { cliErr = true; }
assert.ok(cliErr, "cli: a Sass error exits non-zero");
let cliMissing = false;
try { cli(["/no/such/file.scss"]); } catch (e) { cliMissing = /no such file/.test(String(e.stderr || "")); }
assert.ok(cliMissing, "cli: a missing input file errors cleanly");

// CLI polish flags: --embed-source-map / --quiet / multiple input:output / --update
assert.ok(
  cli(["--embed-source-map", "--stdin"], ".a{b:1}\n").includes("sourceMappingURL=data:application/json;base64,"),
  "cli: --embed-source-map inlines the map",
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
