// Smoke test for the wasm package's dart-sass *modern* API, against BOTH the
// size (`./npm/sasso.mjs`) and speed (`./npm/sasso.speed.mjs`) builds. Covers
// the Phase-1 surface (compileString / compile(path) / async / source maps /
// loadedUrls / info / Exception) and the Phase-2 importer surface (loadPaths,
// relative imports, partial/index/import-only resolution, user Importer +
// FileImporter, loadedUrls, importer errors, async rejection). Run after
// build.sh: `node wasm/test.mjs`.
import assert from "node:assert/strict";
import { writeFileSync, mkdtempSync, mkdirSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { pathToFileURL } from "node:url";
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

  console.log(`ok: ${name} build — modern API + Compiler API + importers (Phase 1 + 2)`);
}

console.log("all wasm modern-API + importer tests passed");
