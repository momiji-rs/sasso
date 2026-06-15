# RFC: a dart-faithful `Importer` API (before we freeze it in the FFI)

Status: **draft for discussion** (raised by @shyim in #4). Not implemented yet —
this proposes the trait shape so we can agree on it before the FFI v2 importer
callback (#5) bakes it into a C ABI contract. *"Stable API, then work on how to
expose."*

## Why

dart-sass's importer is a two-phase interface returning a rich result:

- [`Importer.canonicalize(Uri url)` → `Uri?`](https://pub.dev/documentation/sass/latest/sass/Importer/canonicalize.html)
  — maps a (possibly relative, extension-less) URL to a **canonical identity**,
  *without loading*. The canonical URL is the module-cache key, so the same file
  reached via different spellings is evaluated once.
- [`Importer.load(Uri canonicalUrl)` → `ImporterResult?`](https://pub.dev/documentation/sass/latest/sass/Importer/load.html)
  — fetches the source for a canonical URL.
- [`ImporterResult`](https://pub.dev/documentation/sass/latest/sass/ImporterResult-class.html)
  carries `contents` **plus `syntax` plus `sourceMapUrl`** — not just a string.

## Where sasso is today

The public surface most bindings see (`Importer::resolve(path) -> Option<String>`,
and what the PHP ext / FFI expose) is a **lossy subset**: one string in, one
string out. But the core trait already grew, ad-hoc, most of what dart needs —
just not in dart's shape:

| Capability | dart | sasso core today | exposed in bindings? |
| --- | --- | --- | --- |
| canonical-key dedup (`@use` once) | `canonicalize` | ✅ `resolve_module*` (key, src) | ❌ only `resolve` |
| per-file syntax (`.sass` from `.scss`) | `ImporterResult.syntax` | ✅ `resolve_with_syntax` | ❌ |
| relative-to-containing-file | canonicalize ctx | ✅ `resolve_*_in(base_dir)` | ❌ |
| canonical path for diagnostics | canonical Uri | ✅ `resolve_import_with_path` | ❌ |
| **source-map URL of a loaded file** | `ImporterResult.sourceMapUrl` | ❌ **missing** | ❌ |
| **clean canonicalize / load split** | two methods | ❌ accreted `resolve_*` overloads | ❌ |
| `@import` vs `@use` context (import-only files) | `fromImport` | partial (`resolve_import_*` vs `resolve_module_*`) | ❌ |

So the work is **consolidate + complete**, not build-from-zero: fold the
`resolve_*` overload soup into dart's two-phase shape, add `sourceMapUrl`, and
expose *that* (not the lossy `resolve`) to the gem / PHP ext / FFI.

(Wiring confirmed: `@use`/`@forward` → `resolve_module_with_syntax_in`,
`@import` → `resolve_import_with_path`, `FsImporter` dedups via
`std::fs::canonicalize`.)

## Proposed trait

```rust
/// The canonical, absolute identity of a stylesheet (dart's canonical `Uri`).
/// Two import URLs that canonicalize to the same `CanonicalUrl` are the same
/// stylesheet — loaded and evaluated once, shared by every `@use`/`@forward`.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct CanonicalUrl(pub String);

/// What `Importer::load` returns — dart's `ImporterResult`.
pub struct ImporterResult {
    /// The stylesheet source.
    pub contents: String,
    /// Syntax to parse `contents` with (SCSS / indented `.sass` / plain CSS).
    pub syntax: Syntax,
    /// URL to record for this source in generated source maps
    /// (dart `ImporterResult.sourceMapUrl`); `None` ⇒ use the canonical URL.
    pub source_map_url: Option<String>,
}

/// Context for `canonicalize` (dart `CanonicalizeContext`).
pub struct CanonicalizeContext<'a> {
    /// True for `@import` (which also considers import-only `*.import.scss`
    /// files), false for `@use`/`@forward`.
    pub from_import: bool,
    /// Canonical URL of the stylesheet doing the importing, if any — relative
    /// URLs resolve against it first.
    pub containing_url: Option<&'a CanonicalUrl>,
}

/// Resolves `@use` / `@forward` / `@import` URLs in dart's two phases.
pub trait Importer {
    /// Map `url` to its canonical identity, or `None` if this importer can't
    /// handle it. MUST NOT load the file. The returned `CanonicalUrl` is the
    /// module-cache key.
    fn canonicalize(&self, url: &str, ctx: &CanonicalizeContext<'_>) -> Option<CanonicalUrl>;

    /// Load the source for a `CanonicalUrl` previously returned by
    /// `canonicalize`. `None` if it can no longer be found.
    fn load(&self, canonical: &CanonicalUrl) -> Option<ImporterResult>;
}
```

## Backward compatibility

The current one-method resolvers (and the gem / PHP ext, which only do
`resolve(path) -> string`) keep working via a blanket adapter:

```rust
/// Adapts a legacy `Fn(&str) -> Option<String>` to the two-phase `Importer`:
/// canonicalizes a URL to itself (dedup by spelling — fine when each file has
/// one URL) and loads via the closure, assuming SCSS with no source-map URL.
pub struct LegacyResolver<F>(pub F);

impl<F: Fn(&str) -> Option<String>> Importer for LegacyResolver<F> {
    fn canonicalize(&self, url: &str, _: &CanonicalizeContext<'_>) -> Option<CanonicalUrl> {
        self.0(url).map(|_| CanonicalUrl(url.to_string())) // ⚠ see open Q1
    }
    fn load(&self, c: &CanonicalUrl) -> Option<ImporterResult> {
        self.0(&c.0).map(|contents| ImporterResult {
            contents, syntax: Syntax::Scss, source_map_url: None,
        })
    }
}
```

`FsImporter` implements the new trait *natively* (it already canonicalizes via
`std::fs::canonicalize` and infers syntax from the extension).

## Migration (each step independently shippable, parity-gated)

1. **Core:** add the trait + `ImporterResult` + `CanonicalUrl`; make `FsImporter`
   native; route `eval` through `canonicalize`/`load`; replace the `resolve_*`
   overloads with the `LegacyResolver` shim. Add parity tests for import syntax,
   `@use` dedup, and `sourceMapUrl`. → core minor/major bump.
2. **Bindings:** gem + PHP ext keep their simple resolver via `LegacyResolver`;
   optionally surface the richer interface.
3. **FFI v2 (#5):** expose the two phases as C function pointers + a result
   struct (sketch below), now that the shape is stable.

## FFI v2 mapping (sketch, for when we get there)

```c
typedef struct {
  const char *contents; size_t contents_len;
  int32_t     syntax;               /* SASSO_SYNTAX_* */
  const char *source_map_url;       /* NUL-terminated, or NULL */
} SassoImporterResult;

/* return a heap canonical URL (caller frees via a provided fn), or NULL */
typedef char *(*sasso_canonicalize_fn)(void *user_data, const char *url,
                                       size_t url_len, int from_import);
/* fill *out (sasso copies it), return 1 on hit / 0 on miss */
typedef int   (*sasso_load_fn)(void *user_data, const char *canonical_url,
                               size_t len, SassoImporterResult *out);
```

Ownership/re-entrancy rules across the boundary are the delicate part — exactly
why this waits for the Rust shape to settle first.

## Open questions (for @shyim / discussion)

1. **`canonicalize` cheapness for legacy/userland resolvers.** dart assumes
   `canonicalize` is cheap (no I/O beyond a stat). A one-method `resolve` can't
   separate "does it exist?" from "load it", so the shim probes by loading
   (double work). Acceptable, or keep a `resolve` fast-path for simple importers?
2. **Multiple importers / load-path chain.** dart takes an *ordered list* of
   importers and tries each `canonicalize`. sasso currently has a single
   `Option<&dyn Importer>`. Move to a list now (cleaner for the FFI too), or
   later?
3. **`modificationTime`** (dart has it for caching) — include now or skip?
4. **URL vs path type.** dart uses `Uri`; sasso uses path strings. Keep
   `String`/newtype, or introduce a real URL type (schemes, `file:`)?
5. **`sourceMapUrl` plumbing.** Threading it through the source-map builder is
   the one genuinely new capability — worth doing in step 1, or a follow-up?
