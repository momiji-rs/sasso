//! The `Importer` trait plus the built-in filesystem importer (`FsImporter`)
//! and its dart-faithful partial / index / import-only resolver.

use std::path::{Path, PathBuf};

use crate::Syntax;

/// An opaque, importer-defined canonical identifier for a resolved stylesheet.
///
/// Two URLs that canonicalize to the same `CanonicalUrl` are the SAME loaded
/// file — it is the module-cache / dedup key. What "canonical" *means* is the
/// importer's to define; sasso only uses it as an `Eq`/`Hash` key. For
/// [`FsImporter`] it is the absolute filesystem path. The field is private so
/// constraints/normalization can be added later without an API break.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct CanonicalUrl(String);

impl CanonicalUrl {
    /// Wrap an importer-chosen canonical identifier.
    pub fn new(url: impl Into<String>) -> Self {
        CanonicalUrl(url.into())
    }

    /// The underlying string.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The source an [`Importer::load`] produced for a [`CanonicalUrl`] — dart-sass's
/// `ImporterResult`.
#[derive(Clone, Debug)]
pub struct ImporterResult {
    /// The stylesheet source text.
    pub contents: String,
    /// The syntax `contents` should be parsed with (SCSS / indented / plain CSS).
    pub syntax: Syntax,
    /// The URL to record for this source in generated source maps (dart-sass
    /// `ImporterResult.sourceMapUrl`); `None` falls back to the canonical URL.
    pub source_map_url: Option<String>,
}

/// A real importer failure — an I/O error, a permission error, an ambiguous
/// match, an invalid URL, … — as distinct from a plain miss. Surfaced as an
/// actionable compile [`crate::Error`], never silently treated as "not found".
#[derive(Clone, Debug)]
pub struct ImporterError {
    /// Human-readable description, used as the compile error message.
    pub message: String,
}

/// Context passed to [`Importer::canonicalize`] (dart-sass's `CanonicalizeContext`).
pub struct CanonicalizeContext<'a> {
    /// `true` when resolving an `@import` (which additionally considers
    /// import-only `*.import.{scss,sass}` files), `false` for `@use`/`@forward`.
    pub from_import: bool,
    /// The canonical URL of the stylesheet containing the rule being resolved,
    /// if any (`None` only when there is no containing file). Relative URLs
    /// resolve against it.
    pub containing_url: Option<&'a CanonicalUrl>,
}

/// Resolves `@use` / `@forward` / `@import` URLs in dart-sass's two phases.
///
/// [`canonicalize`](Importer::canonicalize) maps a (possibly relative,
/// extension-less) URL to a stable canonical identity WITHOUT loading it (the
/// result is the module-cache key); [`load`](Importer::load) then fetches that
/// identity's source. Both methods distinguish three outcomes: `Ok(Some(_))` =
/// handled; `Ok(None)` = this importer doesn't handle the URL; `Err(_)` =
/// handled but failed (a real, reportable error).
///
/// Implementing this gives the caller full control over where partials come
/// from — and, crucially, keeps all file access on the caller's side of a
/// sandbox boundary.
pub trait Importer {
    /// Map `url` to its canonical identity, or `Ok(None)` if this importer
    /// cannot resolve it. MUST NOT load the file.
    fn canonicalize(
        &self,
        url: &str,
        ctx: &CanonicalizeContext<'_>,
    ) -> Result<Option<CanonicalUrl>, ImporterError>;

    /// Load the source for a [`CanonicalUrl`] previously returned by
    /// [`canonicalize`](Importer::canonicalize); `Ok(None)` if it can no longer
    /// be found.
    fn load(&self, canonical: &CanonicalUrl) -> Result<Option<ImporterResult>, ImporterError>;
}

/// A filesystem [`Importer`] resolving Sass partials (`_name.scss`,
/// `name/_index.scss`) against a list of load paths.
///
/// Intended for CLI/standalone use; an embedder inside a sandbox should
/// supply its own [`Importer`] that routes through the sandbox's file
/// capability instead of touching the disk directly.
pub struct FsImporter {
    load_paths: Vec<PathBuf>,
}

impl FsImporter {
    /// Create an importer that searches `load_paths` in order.
    pub fn new(load_paths: Vec<PathBuf>) -> Self {
        FsImporter { load_paths }
    }
}

impl Importer for FsImporter {
    fn canonicalize(
        &self,
        url: &str,
        ctx: &CanonicalizeContext<'_>,
    ) -> Result<Option<CanonicalUrl>, ImporterError> {
        // dart-sass resolves a relative URL against the containing file's
        // directory first, then the configured load paths. A `CanonicalUrl`
        // here IS the absolute fs path, so the containing dir is its parent; a
        // containing URL with no parent (a bare entry name like `input.scss`)
        // means the current directory — reproducing the old base of
        // `dirname_of(url).unwrap_or_default()` (an empty string => CWD).
        //
        // NB: this `parent()`-based dirname must stay consistent with the
        // evaluator's `dirname_of` (eval/mod.rs), which derives
        // `current_file_dir` (the `@import` cache key) from the same canonical
        // URL — both use `Path::parent` + treat an empty parent as CWD.
        let base_dir: PathBuf = match ctx.containing_url {
            Some(c) => match Path::new(c.as_str()).parent() {
                Some(par) if !par.as_os_str().is_empty() => par.to_path_buf(),
                _ => PathBuf::new(),
            },
            None => PathBuf::new(),
        };
        let bases = std::iter::once(base_dir).chain(self.load_paths.iter().cloned());
        for base in bases {
            // `@use`/`@forward` (`from_import == false`) never consider
            // `.import` files (those are an `@import`-only escape hatch).
            match resolve_in_base(&base, url, ctx.from_import) {
                Resolution::Found(p) => {
                    // The canonical key is the resolved absolute path so the
                    // same file loaded via different URLs is cached once.
                    let key = std::fs::canonicalize(&p)
                        .map(|c| c.to_string_lossy().into_owned())
                        .unwrap_or_else(|_| p.to_string_lossy().into_owned());
                    return Ok(Some(CanonicalUrl::new(key)));
                }
                // An ambiguous match is an error in dart-sass; we preserve the
                // existing behavior of surfacing it as a miss (the eval layer
                // turns that into the import error) rather than an `Err`.
                Resolution::Ambiguous => return Ok(None),
                Resolution::NotFound => {}
            }
        }
        Ok(None)
    }

    fn load(&self, canonical: &CanonicalUrl) -> Result<Option<ImporterResult>, ImporterError> {
        let p = Path::new(canonical.as_str());
        match std::fs::read_to_string(p) {
            Ok(contents) => Ok(Some(ImporterResult {
                contents,
                syntax: syntax_for_path(p),
                source_map_url: None,
            })),
            // The file vanished between `canonicalize` and `load` -> a miss.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            // Any other read failure (permission, invalid UTF-8, I/O) is a real,
            // reportable error via the `ImporterError` channel, not a misleading
            // "can't find stylesheet".
            Err(e) => Err(ImporterError {
                message: format!("Cannot read {}: {e}", p.display()),
            }),
        }
    }
}

/// The syntax a resolved file should be parsed with, from its extension: a
/// `.sass` file is the indented syntax, anything else (`.scss`) is SCSS.
fn syntax_for_path(p: &Path) -> Syntax {
    match p.extension().and_then(|e| e.to_str()) {
        Some(e) if e.eq_ignore_ascii_case("sass") => Syntax::Sass,
        Some(e) if e.eq_ignore_ascii_case("css") => Syntax::Css,
        _ => Syntax::Scss,
    }
}

/// Outcome of resolving an `@import` argument within a single load path.
enum Resolution {
    /// Exactly one candidate file matched.
    Found(PathBuf),
    /// Two or more candidates matched at the same precedence tier — dart-sass
    /// treats this as an error ("It's not clear which file to import.").
    Ambiguous,
    /// No candidate matched in this base directory.
    NotFound,
}

/// Resolve `@import "path"` against `base`, following dart-sass precedence.
///
/// dart-sass tries, in strict order (each "tier" is checked together; if a
/// tier has more than one match it is ambiguous, if it has exactly one that
/// wins, otherwise fall through to the next tier):
///
/// 1. import-only, non-index: `name.import.{scss,sass}` + partials
/// 2. normal, non-index:      `name.{scss,sass}` + partials
/// 3. import-only index:      `name/index.import.{…}` + `_index.import.{…}`
/// 4. normal index:           `name/index.{…}` + `_index.{…}`
///
/// An explicit `.scss`/`.sass` extension keeps only the matching extension but
/// still honours the import-only override (`name.import.scss`).
///
/// We deliberately do **not** resolve plain `.css` files here: importing a CSS
/// file as a stylesheet is a distinct feature (with its own strict-CSS parsing
/// rules) that this build doesn't implement, and treating `foo.css` as a
/// stylesheet would mis-handle constructs that dart-sass rejects.
///
/// An import-only file whose body is only `@forward`/`@use` (the real-world
/// shape — re-exporting another module) is treated as unusable and skipped,
/// since this build has no module system; resolution then falls through to the
/// normal file, matching the output we can actually produce.
fn resolve_in_base(base: &Path, path: &str, allow_import_only: bool) -> Resolution {
    // dart-sass normalizes the URL lexically before touching the filesystem,
    // so `foo/bar/../baz` resolves even when `foo/bar` doesn't exist.
    let normalized = lexical_normalize(path);
    let path = normalized.as_str();
    let p = Path::new(path);
    let dir = match p.parent() {
        Some(par) if !par.as_os_str().is_empty() => base.join(par),
        _ => base.to_path_buf(),
    };
    let file = p.file_name().and_then(|s| s.to_str()).unwrap_or(path);

    // Explicit `.css` extension: only the plain-CSS candidate is considered.
    // (An `@import "x.css"` never reaches here — it's a passthrough upstream —
    // but `@use "x.css"` does.)
    if let Some(stem) = file.strip_suffix(".css") {
        return match tier_exact(&dir, stem, &["css"], false) {
            Tier::One(p) => Resolution::Found(p),
            Tier::Many => Resolution::Ambiguous,
            Tier::None => Resolution::NotFound,
        };
    }

    // Explicit `.scss`/`.sass` extension: only that extension is considered.
    let explicit_ext = [".scss", ".sass"].into_iter().find(|ext| file.ends_with(ext));

    if let Some(ext) = explicit_ext {
        let stem = &file[..file.len() - ext.len()];
        // Strip the leading dot so `ext` is e.g. "scss".
        let ext = &ext[1..];
        // Tier 1: import-only override for the explicit extension.
        if allow_import_only {
            match tier_exact(&dir, stem, &[ext], true) {
                Tier::One(p) => return Resolution::Found(p),
                Tier::Many => return Resolution::Ambiguous,
                Tier::None => {}
            }
        }
        // Tier 2: the file as written (+ partial).
        return match tier_exact(&dir, stem, &[ext], false) {
            Tier::One(p) => Resolution::Found(p),
            Tier::Many => Resolution::Ambiguous,
            Tier::None => Resolution::NotFound,
        };
    }

    // Extensionless: scss/sass are equal precedence, css is a fallback.
    let mut non_index: Vec<(&str, bool)> = Vec::with_capacity(2);
    if allow_import_only {
        non_index.push((file, true));
    }
    non_index.push((file, false));
    for (stem, import_only) in &non_index {
        match tier_with_extensions(&dir, stem, *import_only) {
            Tier::One(p) => return Resolution::Found(p),
            Tier::Many => return Resolution::Ambiguous,
            Tier::None => {}
        }
    }

    // A plain `.css` file (loaded in plain-CSS mode): after the Sass
    // candidates, but BEFORE index files (dart-sass `_tryPathWithExtensions`
    // tries `$path.css` before `_tryPathAsDirectory`).
    match tier_exact(&dir, file, &["css"], false) {
        Tier::One(p) => return Resolution::Found(p),
        Tier::Many => return Resolution::Ambiguous,
        Tier::None => {}
    }

    // Index files live in a subdirectory named after the import path.
    let index_dir = dir.join(file);
    let index_modes: &[bool] = if allow_import_only {
        &[true, false]
    } else {
        &[false]
    };
    for import_only in index_modes {
        match tier_with_extensions(&index_dir, "index", *import_only) {
            Tier::One(p) => return Resolution::Found(p),
            Tier::Many => return Resolution::Ambiguous,
            Tier::None => {}
        }
    }

    Resolution::NotFound
}

/// Lexically remove `.` and `..` segments from a URL path (no filesystem
/// access; leading `..` segments that would escape are kept).
fn lexical_normalize(path: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    for seg in path.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                if matches!(out.last(), Some(&s) if s != "..") {
                    out.pop();
                } else {
                    out.push("..");
                }
            }
            s => out.push(s),
        }
    }
    let mut s = out.join("/");
    if path.starts_with('/') {
        s.insert(0, '/');
    }
    if s.is_empty() {
        s.push('.');
    }
    s
}

/// One precedence tier's matches.
enum Tier {
    None,
    One(PathBuf),
    Many,
}

impl Tier {
    fn from(mut found: Vec<PathBuf>) -> Tier {
        match found.len() {
            0 => Tier::None,
            1 => Tier::One(found.pop().unwrap_or_default()),
            _ => Tier::Many,
        }
    }
}

/// Try a tier with the standard extension grouping: `scss` and `sass` are
/// checked together (any match there wins the tier). Each extension is checked
/// in both non-partial and partial (`_name`) forms.
fn tier_with_extensions(dir: &Path, stem: &str, import_only: bool) -> Tier {
    tier_exact(dir, stem, &["scss", "sass"], import_only)
}

/// Collect existing candidate files for `stem` under `dir` across `exts`, in
/// non-partial and partial forms, optionally inserting the `.import` suffix
/// (import-only files). Returns how many matched.
fn tier_exact(dir: &Path, stem: &str, exts: &[&str], import_only: bool) -> Tier {
    let mut found = Vec::new();
    let suffix = if import_only { ".import" } else { "" };
    for ext in exts {
        for name in [format!("_{stem}{suffix}.{ext}"), format!("{stem}{suffix}.{ext}")] {
            let cand = dir.join(&name);
            if cand.is_file() {
                found.push(cand);
            }
        }
    }
    Tier::from(found)
}
