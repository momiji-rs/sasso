// sasso/speed — speed-optimized (`-O3`) wasm build.
// Larger module (~2x size) for ~2x compile throughput; same API and output
// as the default "sasso" entry point. Sync APIs use the speed module; the async
// APIs share the size-optimized asyncify'd module (the async path is
// importer-I/O bound, so its raw throughput matters little).
import { makeApi, Exception, info } from "./_loader.mjs";

const api = makeApi(
  new URL("./sasso.speed.wasm", import.meta.url),
  new URL("./sasso.async.wasm", import.meta.url),
);

export const compile = api.compile;
export const compileAsync = api.compileAsync;
export const compileString = api.compileString;
export const compileStringAsync = api.compileStringAsync;
export const initCompiler = api.initCompiler;
export const initAsyncCompiler = api.initAsyncCompiler;
export const configure = api.configure;
export { Exception, info };
export default { ...api, Exception };
