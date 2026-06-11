// @momiji-rs/sasso — size-optimized (`-Oz`) wasm build (default).
// For ~2x throughput at a larger module, import "@momiji-rs/sasso/speed".
import { makeApi } from "./_loader.mjs";

const api = makeApi(new URL("./sasso.wasm", import.meta.url));

export const compile = api.compile;
export const configure = api.configure;
export default api;
