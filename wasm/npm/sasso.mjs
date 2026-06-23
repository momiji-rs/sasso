// sasso — size-optimized (`-Oz`) wasm build (default).
// For ~2x throughput at a larger module, import "sasso/speed".
// Public API mirrors the dart-sass *modern* JS API (drop-in for `sass`).
import { makeApi, Exception, info } from "./_loader.mjs";

const api = makeApi(new URL("./sasso.wasm", import.meta.url));

export const compile = api.compile;
export const compileAsync = api.compileAsync;
export const compileString = api.compileString;
export const compileStringAsync = api.compileStringAsync;
export const initCompiler = api.initCompiler;
export const initAsyncCompiler = api.initAsyncCompiler;
export const configure = api.configure;
export { Exception, info };
export default { ...api, Exception };
