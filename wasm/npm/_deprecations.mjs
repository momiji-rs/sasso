// The deprecation ids `silenceDeprecations` / `--silence-deprecation` accept,
// and the one place that decides what to do with an id that is not among them.
//
// Shared by all three JS consumers (`cli.mjs`, `_loader.mjs`, `native.mjs`) so
// there is one copy to keep right. The list had been written out twice and the
// two copies were the same guess, missing seven ids — `if-function` among them,
// which sasso emits, so silencing it failed on a warning sasso had just
// printed. `src/main.rs` holds the Rust CLI's copy and a test in `wasm/test.mjs`
// derives this one from it, so the remaining two cannot drift apart.
//
// The list is dart-sass 1.104.1's whole `Deprecation` enum, in its declaration
// order, taken from the enum rather than guessed. The package ships most of it
// as `sass/types/deprecations.d.ts`, but that file omits the future-only ids
// (`calc-interp`), and the CLI accepts those, so the enum in `sass.dart.js` is
// the authority.
export const DEPRECATION_IDS = new Set([
  "call-string", "elseif", "moz-document", "relative-canonical",
  "new-global", "color-module-compat", "slash-div", "bogus-combinators",
  "strict-unary", "function-units", "duplicate-var-flags", "null-alpha",
  "abs-percent", "fs-importer-cwd", "css-function-mixin", "mixed-decls",
  "feature-exists", "color-4-api", "color-functions", "legacy-js-api",
  "import", "global-builtin", "type-function",
  "compile-string-relative-url", "misplaced-rest", "with-private",
  "if-function", "function-name", "adjacent-compounds", "user-authored",
  "calc-interp",
]);

// Normalise a `silenceDeprecations` option for the core, reporting ids
// dart-sass does not know.
//
// The CLI and the JS API differ here, and both follow dart. `sass` the command
// exits 64 on an unknown id, because a typo there would otherwise leave the
// warning printing with nothing to say why. The JS API does NOT throw: dart
// warns `Invalid deprecation "nope".` through the caller's own logger and
// compiles anyway — measured against 1.104.1, including that the warning is a
// plain one (`deprecation: false`, no span) and that it is emitted once per
// occurrence, duplicates included, in the order given.
//
// So throwing here would be stricter than dart and would break builds dart
// accepts; staying silent, which is what this did first, contradicts the option
// documented in `sasso.d.ts` and leaves a typo doing nothing with no trace.
// Unknown ids stay in the returned list: they match no deprecation, so passing
// them through costs nothing and keeps one behaviour instead of two.
export function normalizeSilenced(value, warn) {
  if (!Array.isArray(value)) return [];
  const ids = value.map(String);
  if (typeof warn === "function") {
    for (const id of ids) if (!DEPRECATION_IDS.has(id)) warn(`Invalid deprecation "${id}".`);
  }
  return ids;
}
