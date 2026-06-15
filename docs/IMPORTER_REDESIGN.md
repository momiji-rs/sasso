# RFC: a dart-faithful `Importer` API (before we freeze it in the FFI)

Status: **draft for discussion** (raised by @shyim in #4). Not implemented yet —
this proposes the trait shape so we can agree on it before the FFI v2 importer
callback (#5) bakes it into a C ABI contract. *"Stable API, then work on how to
expose."*

**Decision (per #6, w/ @shyim):** the project is early-stage / pre-1.0 with only
a handful of known, in-house importer implementors, so this is a **clean break**
— replace the old `resolve_*` trait outright and update the implementors in
lockstep. **No backward-compat shim** (a shim would just re-create the lossy old
model we're removing).

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
| resolved key + per-file base dir | canonical Uri | ◑ `resolve_import_with_path` — feeds the import cache + `current_file_dir`; `@import` does NOT switch `current_url`/`current_source` to the imported file the way `@use` does, so per-import diagnostic context isn't wired | ❌ |
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
///
/// Opaque newtype: what "canonical" *means* is the importer's to define (any
/// scheme it likes) and sasso only uses it as an `Eq`/`Hash` key — but keeping
/// the field private lets us add normalization/constraints later without an API
/// break.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct CanonicalUrl(String);

impl CanonicalUrl {
    pub fn new(url: impl Into<String>) -> Self {
        Self(url.into())
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

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
    /// True for `@import` (which also considers import-only files —
    /// `*.import.scss` / `*.import.sass`, plus their `_partial` and index
    /// variants), false for `@use`/`@forward`.
    pub from_import: bool,
    /// Canonical URL of the stylesheet doing the importing, if any — relative
    /// URLs resolve against it first.
    pub containing_url: Option<&'a CanonicalUrl>,
}

/// An importer failure that is NOT a plain miss — an I/O error, a permission
/// error, an ambiguous match (dart: "It's not clear which … to import"), an
/// invalid URL, etc. Surfaced as an actionable compile error, not silently
/// treated as "not found". (Concrete shape TBD — see open questions.)
pub struct ImporterError {
    pub message: String,
}

/// Resolves `@use` / `@forward` / `@import` URLs in dart's two phases.
///
/// Both methods distinguish THREE outcomes (dart's `Uri?` + the ability to
/// throw): `Ok(Some(_))` = handled, here it is; `Ok(None)` = this importer
/// doesn't handle the URL (try the next / fall through to "not found");
/// `Err(_)` = this importer OWNS the URL but failed — a real, reportable error.
pub trait Importer {
    /// Map `url` to its canonical identity. MUST NOT load the file. The returned
    /// `CanonicalUrl` is the module-cache key. `Ok(None)` = not handled;
    /// `Err` = handled but failed (e.g. ambiguous match).
    fn canonicalize(
        &self,
        url: &str,
        ctx: &CanonicalizeContext<'_>,
    ) -> Result<Option<CanonicalUrl>, ImporterError>;

    /// Load the source for a `CanonicalUrl` previously returned by
    /// `canonicalize`. `Ok(None)` = vanished; `Err` = found but unreadable.
    fn load(&self, canonical: &CanonicalUrl) -> Result<Option<ImporterResult>, ImporterError>;
}
```

## No back-compat shim (clean break)

Per #6 this replaces the old `resolve_*` trait outright — no `LegacyResolver`
adapter, no deprecation window. Keeping a one-method-resolver shim would re-admit
the lossy model this redesign removes, and pre-1.0 with a known implementor set
makes a coordinated break cheap. `FsImporter` implements the new trait
*natively* (it already canonicalizes via `std::fs::canonicalize` and infers
syntax from the extension); the gem / PHP ext are updated to the two-phase trait
in the same change.

## Migration (each step independently shippable, parity-gated)

1. **Core:** add the trait + `ImporterResult` + `CanonicalUrl`; make `FsImporter`
   native; route `eval` through `canonicalize`/`load`; **delete the old
   `resolve_*` overloads** (no shim). Add parity tests for import syntax, `@use`
   dedup, and `sourceMapUrl`. → core minor bump (breaking, pre-1.0).
2. **Bindings:** update the gem + PHP ext to the two-phase trait in lockstep
   (their Ruby/PHP `Importer` surface changes — fine pre-1.0).
3. **FFI v2 (#5):** expose the two phases as C function pointers + a result
   struct (sketch below), now that the shape is stable.

## FFI v2 mapping (sketch, for when we get there)

```c
/* The importer-callback boundary uses explicit (ptr, len) for every string,
 * never NUL-terminated (binary-safe, no strlen, no NUL ambiguity). NOTE: this
 * is a deliberate v2 convention, NOT inherited from v1 — the v1 surface (#5)
 * NUL-terminates css/error (alongside their lengths) and host paths; v2's
 * callbacks standardize on (ptr, len) throughout. */
typedef struct {
  const char *contents;       size_t contents_len;
  int32_t     syntax;                                /* SASSO_SYNTAX_* */
  const char *source_map_url; size_t source_map_url_len;  /* or NULL / 0 */
} SassoImporterResult;

/* Three-state return mirroring the Rust trait's Result<Option<_>, _>:
 *   1 = handled (out-params filled), 0 = not handled (miss), -1 = error
 * (set *out_error + *out_error_len to an owned message the caller frees). */

/* Canonicalize `url`. On a hit, set *out_canonical (a heap string the caller
 * frees via a provided fn) + *out_canonical_len. `containing_url` (+ len) is the
 * importing stylesheet's canonical URL (NULL / 0 for an entrypoint), for
 * dart-faithful relative resolution. */
typedef int (*sasso_canonicalize_fn)(void *user_data,
                                     const char *url, size_t url_len,
                                     int from_import,
                                     const char *containing_url, size_t containing_url_len,
                                     char **out_canonical, size_t *out_canonical_len,
                                     char **out_error, size_t *out_error_len);
/* Load a canonical URL: fill *out (sasso copies it). 1 = hit, 0 = miss, -1 = error. */
typedef int (*sasso_load_fn)(void *user_data,
                             const char *canonical_url, size_t canonical_url_len,
                             SassoImporterResult *out,
                             char **out_error, size_t *out_error_len);
```

Ownership/re-entrancy rules across the boundary are the delicate part — exactly
why this waits for the Rust shape to settle first.

## Open questions (for @shyim / discussion)

1. **Ergonomics for trivial importers.** With a pure two-phase trait, an
   in-memory/DB importer must write both `canonicalize` (without loading) and
   `load` + build an `ImporterResult`. dart eases this with a simpler
   [`FileImporter`](https://pub.dev/documentation/sass/latest/sass/FileImporter-class.html)
   (just `findFileUrl`). We've deferred any convenience layer for now (YAGNI) —
   revisit a `FileImporter`-style helper only if real friction shows up. (NOT a
   back-compat shim; a deliberate forward convenience.)
2. **Multiple importers / load-path chain.** dart takes an *ordered list* of
   importers and tries each `canonicalize`. sasso currently has a single
   `Option<&dyn Importer>`. Move to a list now (cleaner for the FFI too), or
   later?
3. **`modificationTime`** (dart has it for caching) — include now or skip?
4. **URL vs path type.** dart uses `Uri`; sasso uses path strings. Keep
   `String`/newtype, or introduce a real URL type (schemes, `file:`)?
5. **`sourceMapUrl` plumbing.** Threading it through the source-map builder is
   the one genuinely new capability — worth doing in step 1, or a follow-up?
6. **`ImporterError` shape.** The error channel (`Result<Option<_>, ImporterError>`)
   distinguishes a miss from a real failure. What does the concrete type carry —
   just a message, or a kind (NotFound vs Ambiguous vs Io vs InvalidUrl) + an
   optional span — and how does it map onto a host exception over FFI?
