// Which platforms have a prebuilt native addon, and which package holds the
// one for this machine. TWO callers need this, so it lives here rather than in
// either of them: `native.mjs` resolves the addon from it, and `cli.mjs`
// reports the engine it ended up on and warns when a platform that HAS a
// prebuild compiled through wasm anyway. A second copy of the table in the CLI
// would go stale the first time a target is added, and the fallback it guards
// is invisible by design — exactly the combination that ships wrong.
//
// Side-effect free on purpose: `native.mjs` throws at import time when no addon
// loads, so the CLI cannot ask it these questions through that module.

/** Platform key → the `optionalDependency` that ships that addon. */
export const SUPPORTED = {
  "darwin-arm64": "sasso-native-darwin-arm64",
  "darwin-x64": "sasso-native-darwin-x64",
  "linux-x64-gnu": "sasso-native-linux-x64-gnu",
  "linux-arm64-gnu": "sasso-native-linux-arm64-gnu",
};

/** This machine's key: `<platform>-<arch>`, plus the libc on Linux. */
export function platformKey() {
  const { platform, arch } = process;
  if (platform === "linux") {
    // glibc vs musl: the prebuilds are gnu-only for now.
    const glibc = process.report?.getReport?.()?.header?.glibcVersionRuntime;
    return `linux-${arch}-${glibc ? "gnu" : "musl"}`;
  }
  return `${platform}-${arch}`;
}

/** The addon package prebuilt for this machine, or null where none is. */
export function nativePackage() {
  return SUPPORTED[platformKey()] ?? null;
}
