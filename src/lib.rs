//! `sasso` — a pure-Rust SCSS → CSS compiler.
//!
//! A small, zero-dependency, embeddable Sass engine aiming at byte-exact
//! parity with **current** dart-sass on the subset it implements (e.g.
//! computed colors serialize as `rgb(63.75, 127.5, 191.25)`, not rounded
//! hex). It is sandbox-friendly: `@import` resolution goes through a
//! caller-supplied [`Importer`], so an embedder controls all file access.
//!
//! # Example
//!
//! ```
//! use sasso::{compile, Options};
//!
//! let css = compile("$c: #333; a { color: $c; &:hover { color: $c; } }", &Options::default()).unwrap();
//! assert!(css.contains("a {"));
//! assert!(css.contains("a:hover {"));
//! ```
//!
//! ## Scope
//!
//! This is an in-progress vertical slice: variables (`!default`/`!global`),
//! nesting and the `&` parent selector, `#{}` interpolation, `//` and
//! `/* */` comments, unit arithmetic, a focused color-function set
//! (`rgb`/`rgba`/`hsl`/`mix`/`lighten`/`darken`/`percentage`), and
//! `@import` inlining. Control flow, mixins/functions, `@extend` and the
//! module system are not yet implemented. The north-star target is 100%
//! of the official `sass-spec` suite, tracked by the harness in `spec/`.

mod ast;
mod builtins;
mod emit;
mod error;
mod eval;
mod parser;
mod scanner;
mod selector;
mod value;

pub use error::Error;

use std::path::{Path, PathBuf};

/// Output formatting style.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OutputStyle {
    /// Human-readable, indented output (the default).
    #[default]
    Expanded,
    /// Minified, single-line output.
    Compressed,
}

/// Resolves `@import` arguments to SCSS source.
///
/// Implementing this gives the caller full control over where partials
/// come from — and, crucially, keeps all file access on the caller's side
/// of a sandbox boundary.
pub trait Importer {
    /// Resolve an `@import` argument (e.g. `"minima/base"`) to SCSS
    /// source, or `None` if it cannot be found.
    fn resolve(&self, path: &str) -> Option<String>;
}

/// Compilation options.
#[derive(Default)]
pub struct Options<'a> {
    /// Output style.
    pub style: OutputStyle,
    /// Importer used to resolve `@import`; `None` disables file imports.
    pub importer: Option<&'a dyn Importer>,
}

impl<'a> Options<'a> {
    /// Create default options (expanded, no importer).
    pub fn new() -> Self {
        Self::default()
    }

    /// Builder: set the output style.
    #[must_use]
    pub fn with_style(mut self, style: OutputStyle) -> Self {
        self.style = style;
        self
    }

    /// Builder: set the importer.
    #[must_use]
    pub fn with_importer(mut self, importer: &'a dyn Importer) -> Self {
        self.importer = Some(importer);
        self
    }
}

/// Compile SCSS source to CSS.
///
/// # Errors
///
/// Returns [`Error`] on a parse or evaluation failure (with a 1-based
/// source position when known).
pub fn compile(source: &str, options: &Options<'_>) -> Result<String, Error> {
    let sheet = parser::parse(source)?;
    let mut ev = eval::Evaluator::new(eval::EvalOptions {
        style: options.style,
        importer: options.importer,
    });
    let mut out = Vec::new();
    ev.eval_sheet(&sheet, &mut out)?;
    Ok(emit::emit(&out, options.style))
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
            match resolve_in_base(base, path) {
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
fn resolve_in_base(base: &Path, path: &str) -> Resolution {
    let p = Path::new(path);
    let dir = match p.parent() {
        Some(par) if !par.as_os_str().is_empty() => base.join(par),
        _ => base.to_path_buf(),
    };
    let file = p.file_name().and_then(|s| s.to_str()).unwrap_or(path);

    // Explicit `.scss`/`.sass` extension: only that extension is considered.
    // (`.css` is handled upstream as a plain CSS import and never reaches here.)
    let explicit_ext = [".scss", ".sass"].into_iter().find(|ext| file.ends_with(ext));

    if let Some(ext) = explicit_ext {
        let stem = &file[..file.len() - ext.len()];
        // Strip the leading dot so `ext` is e.g. "scss".
        let ext = &ext[1..];
        // Tier 1: import-only override for the explicit extension.
        match tier_exact(&dir, stem, &[ext], true) {
            Tier::One(p) => return Resolution::Found(p),
            Tier::Many => return Resolution::Ambiguous,
            Tier::None => {}
        }
        // Tier 2: the file as written (+ partial).
        return match tier_exact(&dir, stem, &[ext], false) {
            Tier::One(p) => Resolution::Found(p),
            Tier::Many => Resolution::Ambiguous,
            Tier::None => Resolution::NotFound,
        };
    }

    // Extensionless: scss/sass are equal precedence, css is a fallback.
    let non_index = [
        // (stem, import_only)
        (file.to_string(), true),
        (file.to_string(), false),
    ];
    for (stem, import_only) in &non_index {
        match tier_with_extensions(&dir, stem, *import_only) {
            Tier::One(p) => return Resolution::Found(p),
            Tier::Many => return Resolution::Ambiguous,
            Tier::None => {}
        }
    }

    // Index files live in a subdirectory named after the import path.
    let index_dir = dir.join(file);
    for import_only in [true, false] {
        match tier_with_extensions(&index_dir, "index", import_only) {
            Tier::One(p) => return Resolution::Found(p),
            Tier::Many => return Resolution::Ambiguous,
            Tier::None => {}
        }
    }

    Resolution::NotFound
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
/// (import-only files). Import-only candidates we cannot compile (their body is
/// only `@forward`/`@use`) are skipped. Returns how many matched.
fn tier_exact(dir: &Path, stem: &str, exts: &[&str], import_only: bool) -> Tier {
    let mut found = Vec::new();
    let suffix = if import_only { ".import" } else { "" };
    for ext in exts {
        for name in [format!("_{stem}{suffix}.{ext}"), format!("{stem}{suffix}.{ext}")] {
            let cand = dir.join(&name);
            if !cand.is_file() {
                continue;
            }
            if import_only && !import_only_is_usable(&cand) {
                continue;
            }
            found.push(cand);
        }
    }
    Tier::from(found)
}

/// Whether an import-only file is something this build can actually inline.
///
/// Real `.import.scss` files re-export another module with `@forward`/`@use`,
/// which this build has no support for. Such files are reported as unusable so
/// that resolution falls back to the corresponding normal file. A file with
/// any other meaningful content (e.g. plain rules) is considered usable.
fn import_only_is_usable(path: &Path) -> bool {
    let Ok(src) = std::fs::read_to_string(path) else {
        // Unreadable: let the normal resolution path report "not found".
        return false;
    };
    for line in src.lines() {
        let t = line.trim();
        if t.is_empty() || t.starts_with("//") || t.starts_with("/*") {
            continue;
        }
        return !(t.starts_with("@forward") || t.starts_with("@use"));
    }
    // Empty (only comments/blank lines) — nothing to inline, treat as usable
    // (an empty import-only file is valid and contributes no output).
    true
}
