// Shared loader core for the @momiji-rs/sasso wasm variants.
//
// No wasm-bindgen: this marshals UTF-8 through the module's linear memory by
// hand against the raw `sasso_alloc` / `sasso_free` / `sasso_compile` /
// `sasso_set_arena_bytes` ABI (see ../src/lib.rs). Both the default (size,
// `-Oz`) and `/speed` (`-O3`) entry points are three-line wrappers around
// `makeApi(<their wasm URL>)`. The wasm is instantiated lazily and
// synchronously on first use.

import { readFileSync } from "node:fs";

const encoder = new TextEncoder();
const decoder = new TextDecoder();

/**
 * Build the public API bound to one wasm module URL.
 * @param {URL} wasmUrl
 */
export function makeApi(wasmUrl) {
  let ex; // cached wasm exports
  let pendingArenaBytes = null; // applied at instantiation, before any compile

  function instance() {
    if (ex) return ex;
    const bytes = readFileSync(wasmUrl);
    const module = new WebAssembly.Module(bytes);
    ex = new WebAssembly.Instance(module, {}).exports;
    // Apply a pending arena override before the first compile reserves it.
    if (pendingArenaBytes !== null) ex.sasso_set_arena_bytes(pendingArenaBytes);
    return ex;
  }

  /**
   * Configure the bump-arena allocator. MUST be called before the first
   * `compile()` — the arena region is reserved on first use and then fixed.
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
   * Compile an SCSS string to CSS. Throws an Error (with the compiler's
   * message) on a Sass error.
   *
   * @param {string} scss
   * @param {{ style?: "expanded" | "compressed" }} [options]
   * @returns {string} the compiled CSS
   */
  function compile(scss, options = {}) {
    if (typeof scss !== "string") {
      throw new TypeError("compile(scss): scss must be a string");
    }
    const w = instance();
    const input = encoder.encode(scss);

    // Allocate input + an 8-byte scratch cell ([outLen: u32 LE][ok: u8]) up
    // front, then write the input — so a memory grow during alloc can't
    // strand the view we write through.
    const inPtr = input.length ? w.sasso_alloc(input.length) : 0;
    const scratch = w.sasso_alloc(8);
    if (input.length) {
      new Uint8Array(w.memory.buffer, inPtr, input.length).set(input);
    }

    const compressed = options.style === "compressed" ? 1 : 0;
    const wantMap = !!options.sourceMap;
    const outPtr = wantMap
      ? w.sasso_compile_map(inPtr, input.length, compressed, options.sourceMapIncludeSources ? 1 : 0, scratch, scratch + 4)
      : w.sasso_compile(inPtr, input.length, compressed, scratch, scratch + 4);

    // Re-read against the current buffer (compile may have grown memory).
    const view = new DataView(w.memory.buffer);
    const outLen = view.getUint32(scratch, true);
    const ok = view.getUint8(scratch + 4);
    const out = new Uint8Array(w.memory.buffer, outPtr, outLen).slice();

    if (input.length) w.sasso_free(inPtr, input.length);
    w.sasso_free(scratch, 8);
    w.sasso_free(outPtr, outLen);

    if (!ok) throw new Error(decoder.decode(out));
    if (!wantMap) return decoder.decode(out);

    // Framed result: [cssLen: u32 LE][css bytes][sourceMap JSON bytes].
    const cssLen = new DataView(out.buffer, out.byteOffset, 4).getUint32(0, true);
    return {
      css: decoder.decode(out.subarray(4, 4 + cssLen)),
      sourceMap: JSON.parse(decoder.decode(out.subarray(4 + cssLen))),
    };
  }

  return { compile, configure };
}
