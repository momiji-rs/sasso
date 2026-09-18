// The rule for pairing `sasso` with its prebuilt native addon.
//
// `sasso` declares the four `sasso-native-*` packages as EXACT-version
// `optionalDependencies`, so a plain `npm install sasso` is always aligned and
// this never fires. It fires when a consumer names those packages itself —
// then there are two places to bump and they can drift.
//
// A drift has to be refused rather than tolerated, because napi silently
// ignores config fields it does not know (measured on the addon at the time of
// writing: an unknown field compiles fine and returns CSS, no error, no
// warning). So an addon one release behind accepts every option the newer JS
// sends and applies only the ones it happens to recognise — the compile
// succeeds and quietly does something other than what was asked. A flag
// accepted that does nothing is the bug that opened momiji-rs/sasso#24; this
// is the same bug with no flag to blame it on. See #114.
//
// `nativeVersion()` from the addon cannot answer this: it reports
// `napi/Cargo.toml`'s version (`0.1.0`), which has nothing to do with the
// published package's. The platform package's own `package.json` does.

/**
 * Refuse a native addon whose version does not match this `sasso`.
 *
 * `ours`/`theirs` are versions read from the two `package.json` files, or
 * `null` where there is no manifest to read — the `SASSO_NATIVE_BINARY`
 * override and the repo-local `napi/npm/sasso.node` are development paths and
 * are deliberately not checked, since neither is a published pairing.
 *
 * Throws an `Error` carrying `code: "SASSO_ADDON_VERSION_MISMATCH"`, which the
 * CLI uses to tell a broken install apart from a platform with no prebuild.
 */
export function assertAddonVersion(ours, theirs, pkg) {
  if (!ours || !theirs || ours === theirs) return;
  const err = new Error(
    `sasso: the native addon is ${pkg}@${theirs}, but this is sasso@${ours}. ` +
      `They are released together and pinned to each other, so a mismatch means ` +
      `${pkg} was pinned separately from sasso. Refusing it: the addon ignores ` +
      `options it does not know without saying so, which would make newer options ` +
      `do nothing instead of failing. Drop any direct "${pkg}" dependency and let ` +
      `sasso pull it in, or set both to ${ours}.`,
  );
  err.code = "SASSO_ADDON_VERSION_MISMATCH";
  throw err;
}
