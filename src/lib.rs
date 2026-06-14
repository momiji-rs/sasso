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
//! This covers a large slice of Sass: variables (`!default`/`!global`),
//! nesting and the `&` parent selector, `#{}` interpolation, `//` and
//! `/* */` comments, unit arithmetic, the color functions, control flow,
//! mixins/functions, `@extend`, `@import`, and the `@use`/`@forward` module
//! system. Both input syntaxes are supported — the brace/semicolon SCSS
//! syntax and the indented `.sass` syntax (selected via [`Options::with_syntax`]
//! or, in the CLI, the input file's extension) — parsing into the same AST and
//! sharing the evaluator and emitter. The north-star target is 100% of the
//! official `sass-spec` suite, tracked by the harness in `spec/`.

// The library's `unsafe` is confined to one audited module — `arena`, the
// scoped bump allocator (perf #5), verified by unit tests + Miri. Every other
// module is `deny(unsafe_code)` (see Cargo.toml `[lints]`); `arena` is the only
// `#[allow]`. The wasm wrapper (`/wasm`, a separate crate) has its own FFI unsafe.
mod arena;

mod ast;
mod builtins;
mod deprecation;
mod diag;
mod emit;
mod error;
mod eval;
mod fxhash;
mod musl_math;
mod parser;
mod ryu;
mod sass_parser;
mod scanner;
mod selector;
// Source Map v3 generation. Phase A landed the encoding primitives + JSON model;
// `allow(dead_code)` until later phases wire it into emit + the public API.
#[allow(dead_code)]
mod sourcemap;
mod value;

pub use arena::{set_arena_bytes, ScopedAlloc};
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

/// The input syntax flavour.
///
/// Both flavours parse into the same AST and share the evaluator and emitter;
/// only the *block structure* differs (`{}`/`;` for SCSS, indentation +
/// newlines for the indented `.sass` syntax).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Syntax {
    /// The brace/semicolon SCSS syntax (the default).
    #[default]
    Scss,
    /// The indented `.sass` syntax: blocks come from indentation, statements
    /// end at a newline.
    Sass,
    /// Plain CSS (a `.css` file loaded via `@use`/`@forward`): the brace/semicolon
    /// grammar, but Sass features are rejected, nesting is preserved verbatim,
    /// and values are emitted without SassScript evaluation.
    Css,
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

/// Compilation options.
pub struct Options<'a> {
    /// Output style.
    pub style: OutputStyle,
    /// Input syntax (SCSS or indented `.sass`).
    pub syntax: Syntax,
    /// Importer used to resolve `@import`; `None` disables file imports.
    pub importer: Option<&'a dyn Importer>,
    /// The input's path/URL as it should appear in diagnostics (e.g.
    /// `input.scss`). `None` disables byte-exact diagnostic snippets (errors
    /// then render as the legacy `Error: <msg> (line:col)` one-liner).
    pub url: Option<&'a str>,
    /// Whether to draw diagnostic snippets with Unicode box-drawing glyphs
    /// (`true`, the default) or the ASCII fallback (`false`, dart's
    /// `--no-unicode`).
    pub unicode: bool,
}

impl Default for Options<'_> {
    fn default() -> Self {
        Options {
            style: OutputStyle::default(),
            syntax: Syntax::default(),
            importer: None,
            url: None,
            unicode: true,
        }
    }
}

impl<'a> Options<'a> {
    /// Create default options (expanded, SCSS, no importer).
    pub fn new() -> Self {
        Self::default()
    }

    /// Builder: set the output style.
    #[must_use]
    pub fn with_style(mut self, style: OutputStyle) -> Self {
        self.style = style;
        self
    }

    /// Builder: set the input syntax.
    #[must_use]
    pub fn with_syntax(mut self, syntax: Syntax) -> Self {
        self.syntax = syntax;
        self
    }

    /// Builder: set the importer.
    #[must_use]
    pub fn with_importer(mut self, importer: &'a dyn Importer) -> Self {
        self.importer = Some(importer);
        self
    }

    /// Builder: set the diagnostic display URL (enables byte-exact snippets).
    #[must_use]
    pub fn with_url(mut self, url: &'a str) -> Self {
        self.url = Some(url);
        self
    }

    /// Builder: select the diagnostic glyph set (`false` = ASCII / `--no-unicode`).
    #[must_use]
    pub fn with_unicode(mut self, unicode: bool) -> Self {
        self.unicode = unicode;
        self
    }
}

/// Compile SCSS source to CSS.
///
/// # Errors
///
/// Returns [`Error`] on a parse or evaluation failure (with a 1-based
/// source position when known).
///
/// # Allocator scope
///
/// When the binary installs [`ScopedAlloc`] as its `#[global_allocator]`, this
/// function brackets the whole compile in a bump-arena scope: every allocation
/// `compile_inner` makes is a pointer bump from a per-thread arena that is freed
/// wholesale when the scope ends. The returned `Result` is allocated *in* the
/// arena, so it is deep-cloned out to the system allocator *before* the arena is
/// reset — the value handed back to the caller never points into the arena. When
/// no `ScopedAlloc` is installed the scope primitives are inert (depth tracking
/// only) and every allocation goes to the system allocator as usual, so this
/// wrapper is correct (just with a redundant clone) under any global allocator.
pub fn compile(source: &str, options: &Options<'_>) -> Result<String, Error> {
    // Enter the arena scope. The RAII guard's `Drop` leaves + resets the arena
    // on the *panic* path; the success path below finishes manually and forgets
    // the guard, so there is no double-leave.
    let guard = arena::Scope::enter();
    // All allocations here bump from the arena (when ScopedAlloc is installed).
    let result = compile_inner(source, options);
    // Leave the scope WITHOUT resetting yet: depth drops to 0, so the arena is
    // now inactive and subsequent allocations route to the system allocator —
    // but the arena memory is still intact and `result` may point into it.
    let outermost = arena::leave_no_reset();
    // Deep-clone the result to the system allocator while the scope is inactive.
    // `Error` derives `Clone`, so both the `Ok(String)` and `Err(message)` cases
    // are copied out byte-for-byte to system-owned memory.
    let owned = result.clone();
    // Drop the arena-resident original (in-arena `dealloc` is a no-op) before
    // the region it lives in is reclaimed.
    drop(result);
    // Only the outermost scope owns the arena's lifetime; reset frees it all.
    if outermost {
        arena::reset();
    }
    // We finished the scope manually; suppress the guard's `Drop` to avoid a
    // second leave/reset.
    std::mem::forget(guard);
    owned
}

/// The actual compile pipeline. Runs inside the arena scope established by
/// [`compile`]; all of its allocations may be arena-resident, so its result is
/// copied out by the wrapper before the arena is reset.
fn compile_inner(source: &str, options: &Options<'_>) -> Result<String, Error> {
    let glyphs_for = || {
        if options.unicode {
            diag::GlyphSet::Unicode
        } else {
            diag::GlyphSet::Ascii
        }
    };
    let sheet = match options.syntax {
        Syntax::Scss => parser::parse(source),
        Syntax::Css => parser::parse_plain_css(source),
        Syntax::Sass => sass_parser::parse(source),
    };
    // A parse error never reached the evaluator, so render its snippet here
    // (single `root stylesheet` frame) when a diagnostic URL is configured.
    let sheet = match sheet {
        Ok(s) => s,
        Err(mut e) => {
            if let Some(url) = options.url {
                if e.rendered.is_none() && e.has_position() {
                    let span = diag::Span {
                        line: e.line,
                        col: e.col,
                        length: e.length,
                    };
                    e.rendered = Some(diag::render_error(&e.message, source, url, span, glyphs_for()));
                }
            }
            return Err(e);
        }
    };
    // Reject `@function`/`@mixin` declarations in control directives or
    // function/mixin bodies (a compile-time restriction, checked before eval).
    eval::validate_declarations(&sheet)?;
    // Diagnostics are enabled only when the caller supplies a display URL; then
    // the evaluator renders byte-exact `Error:`/`WARNING:` blocks against the
    // source. Without a URL it falls back to the legacy one-liner.
    let (diag_source, diag_url) = match options.url {
        Some(url) => (source, url),
        None => ("", ""),
    };
    let glyphs = if options.unicode {
        diag::GlyphSet::Unicode
    } else {
        diag::GlyphSet::Ascii
    };
    let mut ev = eval::Evaluator::new(eval::EvalOptions {
        style: options.style,
        importer: options.importer,
        source: diag_source,
        url: diag_url,
        glyphs,
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
