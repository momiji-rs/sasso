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
            for cand in candidate_paths(base, path) {
                if let Ok(src) = std::fs::read_to_string(&cand) {
                    return Some(src);
                }
            }
        }
        None
    }
}

fn candidate_paths(base: &Path, path: &str) -> Vec<PathBuf> {
    let p = Path::new(path);
    let stem = p.file_name().and_then(|s| s.to_str()).unwrap_or(path);
    let dir = match p.parent() {
        Some(par) if !par.as_os_str().is_empty() => base.join(par),
        _ => base.to_path_buf(),
    };
    let mut out = Vec::new();
    if path.ends_with(".scss") {
        out.push(base.join(path));
        out.push(dir.join(format!("_{stem}")));
    } else {
        out.push(dir.join(format!("{stem}.scss")));
        out.push(dir.join(format!("_{stem}.scss")));
        out.push(base.join(path).join("_index.scss"));
        out.push(base.join(path).join("index.scss"));
    }
    out
}
