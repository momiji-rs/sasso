// Smoke test for the wasm package: the plain CSS path + the source-map path,
// against BOTH the size (`./npm/sasso.mjs`) and speed (`./npm/sasso.speed.mjs`)
// builds. Run after build.sh: `node wasm/test.mjs`.
import assert from "node:assert/strict";
import * as size from "./npm/sasso.mjs";
import * as speed from "./npm/sasso.speed.mjs";

const SCSS = ".a {\n  color: red;\n  .b { width: 10px; }\n}\n";

for (const [name, mod] of [["size", size], ["speed", speed]]) {
  // plain compile -> string (backwards compatible)
  const css = mod.compile(SCSS);
  assert.equal(typeof css, "string", `${name}: plain compile returns a string`);
  assert.ok(css.includes(".a .b {"), `${name}: nested selector flattened`);

  // sourceMap: true -> { css, sourceMap }
  const r = mod.compile(SCSS, { sourceMap: true });
  assert.equal(typeof r, "object", `${name}: sourceMap returns an object`);
  assert.equal(r.css, css, `${name}: .css matches the plain result`);
  assert.equal(r.sourceMap.version, 3, `${name}: map version 3`);
  assert.deepEqual(r.sourceMap.names, [], `${name}: names empty`);
  assert.equal(r.sourceMap.mappings, "AAAA;EACE;;AACA;EAAK", `${name}: mappings byte-exact vs dart`);
  assert.ok(!("sourcesContent" in r.sourceMap), `${name}: no sourcesContent unless asked`);

  // include sources
  const withSrc = mod.compile(".a { color: red; }\n", { sourceMap: true, sourceMapIncludeSources: true });
  assert.equal(withSrc.sourceMap.sourcesContent.length, withSrc.sourceMap.sources.length, `${name}: sourcesContent parallel`);

  // compressed still maps
  const cmp = mod.compile(SCSS, { sourceMap: true, style: "compressed" });
  assert.ok(cmp.css.length > 0 && cmp.sourceMap.mappings.length > 0, `${name}: compressed map`);

  // errors throw
  assert.throws(() => mod.compile(".a { color: ; }"), /Error/, `${name}: invalid SCSS throws`);

  console.log(`ok: ${name} build — plain + source map`);
}

console.log("all wasm source-map smoke tests passed");
