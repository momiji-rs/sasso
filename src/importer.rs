//! The `Importer` trait plus the built-in filesystem importer (`FsImporter`)
//! and its dart-faithful partial / index / import-only resolver.

use std::path::{Path, PathBuf};

use crate::Syntax;

/// Resolves `@import` arguments to SCSS source.
///
/// Implementing this gives the caller full control over where partials
/// come from — and, crucially, keeps all file access on the caller's side
/// of a sandbox boundary.
pub trait Importer {
    /// Resolve an `@import` argument (e.g. `"minima/base"`) to SCSS
    /// source, or `None` if it cannot be found.
    fn resolve(&self, path: &str) -> Option<String>;

    /// Resolve a `@use`/`@forward` module URL to a `(canonical_key, source)`
    /// pair, or `None` if it cannot be found. The canonical key uniquely
    /// identifies the loaded file so the module system can evaluate it once and
    /// share the instance between every `@use`/`@forward` of the same file
    /// (regardless of the spelling of the URL). The default implementation
    /// resolves through [`Importer::resolve`] and uses the URL itself as the
    /// key (adequate when each distinct file is referenced by a single URL).
    fn resolve_module(&self, path: &str) -> Option<(String, String)> {
        self.resolve(path).map(|src| (path.to_string(), src))
    }

    /// Like [`Importer::resolve`], but also reports the syntax of the resolved
    /// file so a `.sass` partial imported from `.scss` (or vice versa) is parsed
    /// with the correct front-end. The default keeps backward compatibility by
    /// resolving through [`Importer::resolve`] and reporting [`Syntax::Scss`].
    fn resolve_with_syntax(&self, path: &str) -> Option<(String, Syntax)> {
        self.resolve(path).map(|src| (src, Syntax::Scss))
    }

    /// Like [`Importer::resolve_module`], but also reports the resolved file's
    /// syntax (see [`Importer::resolve_with_syntax`]). The default reports
    /// [`Syntax::Scss`].
    fn resolve_module_with_syntax(&self, path: &str) -> Option<(String, String, Syntax)> {
        self.resolve_module(path)
            .map(|(key, src)| (key, src, Syntax::Scss))
    }

    /// Like [`Importer::resolve_module_with_syntax`], but tries `base_dir`
    /// (the directory of the file containing the rule) before the importer's
    /// own search paths — dart-sass resolves relative URLs against the
    /// containing file first. The default ignores the base.
    fn resolve_module_with_syntax_in(
        &self,
        path: &str,
        _base_dir: Option<&str>,
    ) -> Option<(String, String, Syntax)> {
        self.resolve_module_with_syntax(path)
    }

    /// Like [`Importer::resolve_with_syntax`] for `@import`, but reports the
    /// resolved file's canonical path (empty when unknown) and tries
    /// `base_dir` first. The default ignores the base and reports no path.
    fn resolve_import_with_path(
        &self,
        path: &str,
        _base_dir: Option<&str>,
    ) -> Option<(String, String, Syntax)> {
        self.resolve_with_syntax(path)
            .map(|(src, syntax)| (String::new(), src, syntax))
    }
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
    fn resolve(&self, path: &str) -> Option<String> {
        for base in &self.load_paths {
            match resolve_in_base(base, path, true) {
                Resolution::Found(p) => {
                    if let Ok(src) = std::fs::read_to_string(&p) {
                        return Some(src);
                    }
                }
                // An ambiguous match is an error in dart-sass; we surface it as
                // "not found" (the eval layer turns that into an import error),
                // which is enough for callers that only care about pass/fail.
                Resolution::Ambiguous => return None,
                Resolution::NotFound => {}
            }
        }
        None
    }

    fn resolve_module(&self, path: &str) -> Option<(String, String)> {
        self.resolve_module_with_syntax(path)
            .map(|(key, src, _)| (key, src))
    }

    fn resolve_with_syntax(&self, path: &str) -> Option<(String, Syntax)> {
        for base in &self.load_paths {
            match resolve_in_base(base, path, true) {
                Resolution::Found(p) => {
                    if let Ok(src) = std::fs::read_to_string(&p) {
                        return Some((src, syntax_for_path(&p)));
                    }
                }
                Resolution::Ambiguous => return None,
                Resolution::NotFound => {}
            }
        }
        None
    }

    fn resolve_module_with_syntax(&self, path: &str) -> Option<(String, String, Syntax)> {
        self.resolve_module_with_syntax_in(path, None)
    }

    fn resolve_module_with_syntax_in(
        &self,
        path: &str,
        base_dir: Option<&str>,
    ) -> Option<(String, String, Syntax)> {
        // dart-sass resolves a relative URL against the containing file's
        // directory first, then the configured load paths.
        let bases = base_dir
            .map(|b| vec![PathBuf::from(b)])
            .unwrap_or_default()
            .into_iter()
            .chain(self.load_paths.iter().cloned());
        for base in bases {
            // `@use`/`@forward` never consider `.import` files (those are an
            // `@import`-only escape hatch).
            match resolve_in_base(&base, path, false) {
                Resolution::Found(p) => {
                    if let Ok(src) = std::fs::read_to_string(&p) {
                        // The canonical key is the resolved absolute path so the
                        // same file loaded via different URLs is cached once.
                        let key = std::fs::canonicalize(&p)
                            .map(|c| c.to_string_lossy().into_owned())
                            .unwrap_or_else(|_| p.to_string_lossy().into_owned());
                        return Some((key, src, syntax_for_path(&p)));
                    }
                }
                Resolution::Ambiguous => return None,
                Resolution::NotFound => {}
            }
        }
        None
    }

    fn resolve_import_with_path(
        &self,
        path: &str,
        base_dir: Option<&str>,
    ) -> Option<(String, String, Syntax)> {
        let bases = base_dir
            .map(|b| vec![PathBuf::from(b)])
            .unwrap_or_default()
            .into_iter()
            .chain(self.load_paths.iter().cloned());
        for base in bases {
            match resolve_in_base(&base, path, true) {
                Resolution::Found(p) => {
                    if let Ok(src) = std::fs::read_to_string(&p) {
                        let key = std::fs::canonicalize(&p)
                            .map(|c| c.to_string_lossy().into_owned())
                            .unwrap_or_else(|_| p.to_string_lossy().into_owned());
                        return Some((key, src, syntax_for_path(&p)));
                    }
                }
                Resolution::Ambiguous => return None,
                Resolution::NotFound => {}
            }
        }
        None
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
