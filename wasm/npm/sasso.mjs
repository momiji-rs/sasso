// @momiji-rs/sasso — a pure-Rust SCSS→CSS compiler as a tiny wasm module.
//
// No wasm-bindgen: this loader marshals UTF-8 through the module's linear
// memory by hand against the raw `sasso_alloc`/`sasso_free`/`sasso_compile`
// ABI (see ../src/lib.rs). The wasm is instantiated lazily and synchronously
// on first use.

import { readFileSync } from "node:fs";

let ex; // cached wasm exports

function instance() {
  if (ex) return ex;
  const bytes = readFileSync(new URL("./sasso.wasm", import.meta.url));
  const module = new WebAssembly.Module(bytes);
  ex = new WebAssembly.Instance(module, {}).exports;
  return ex;
}

const encoder = new TextEncoder();
const decoder = new TextDecoder();

/**
 * Compile an SCSS string to CSS. Throws an Error (with the compiler's message)
 * on a Sass error.
 *
 * @param {string} scss
 * @param {{ style?: "expanded" | "compressed" }} [options]
 * @returns {string} the compiled CSS
 */
export function compile(scss, options = {}) {
  if (typeof scss !== "string") {
    throw new TypeError("compile(scss): scss must be a string");
  }
  const w = instance();
  const input = encoder.encode(scss);

  // Allocate input + an 8-byte scratch cell ([outLen: u32 LE][ok: u8]) up
  // front, then write the input — so a memory grow during alloc can't strand
  // the view we write through.
  const inPtr = input.length ? w.sasso_alloc(input.length) : 0;
  const scratch = w.sasso_alloc(8);
  if (input.length) {
    new Uint8Array(w.memory.buffer, inPtr, input.length).set(input);
  }

  const compressed = options.style === "compressed" ? 1 : 0;
  const outPtr = w.sasso_compile(inPtr, input.length, compressed, scratch, scratch + 4);

  // Re-read against the current buffer (compile may have grown memory).
  const view = new DataView(w.memory.buffer);
  const outLen = view.getUint32(scratch, true);
  const ok = view.getUint8(scratch + 4);
  const out = new Uint8Array(w.memory.buffer, outPtr, outLen).slice();

  if (input.length) w.sasso_free(inPtr, input.length);
  w.sasso_free(scratch, 8);
  w.sasso_free(outPtr, outLen);

  const text = decoder.decode(out);
  if (!ok) throw new Error(text);
  return text;
}

export default { compile };
