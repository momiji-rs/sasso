// @momiji-rs/sasso/speed — speed-optimized (`-O3`) wasm build.
// Larger module (~2x size) for ~2x compile throughput; same API and output
// as the default "@momiji-rs/sasso" entry point.
import { makeApi } from "./_loader.mjs";

const api = makeApi(new URL("./sasso.speed.wasm", import.meta.url));

export const compile = api.compile;
export const configure = api.configure;
export default api;
