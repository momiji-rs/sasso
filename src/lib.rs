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
mod importer;
mod musl_math;
mod parser;
mod ryu;
mod sass_parser;
mod scanner;
mod selector;
// Source Map v3 generation: the encoding primitives + JSON model (Phase A),
// wired into emit (Phase B/C) and surfaced through `compile_with_source_map`.
mod sourcemap;
mod value;

pub use arena::{set_arena_bytes, ScopedAlloc};
pub use error::Error;
pub use importer::{FsImporter, Importer};
pub use sourcemap::SourceMap;

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
    /// Whether [`compile_with_source_map`] populates the source map's
    /// `sourcesContent` field with the full text of each source (dart-sass
    /// `--embed-sources`). Default `false` (the map references sources by URL
    /// only). Ignored by the plain [`compile`] path.
    pub source_map_include_sources: bool,
}

impl Default for Options<'_> {
    fn default() -> Self {
        Options {
            style: OutputStyle::default(),
            syntax: Syntax::default(),
            importer: None,
            url: None,
            unicode: true,
            source_map_include_sources: false,
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

    /// Builder: whether [`compile_with_source_map`] embeds each source's full
    /// text in the map's `sourcesContent` (default `false`).
    #[must_use]
    pub fn with_source_map_include_sources(mut self, include: bool) -> Self {
        self.source_map_include_sources = include;
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

/// The CSS plus its source map, returned by [`compile_with_source_map`].
#[derive(Clone, Debug)]
pub struct CompileResult {
    /// The compiled CSS (identical to what [`compile`] would return for the
    /// same `source`/`options` — the map is generated alongside, not instead).
    pub css: String,
    /// The Source Map v3 describing `css`. Serialize it with
    /// [`SourceMap::to_json`].
    pub source_map: SourceMap,
}

/// Compile SCSS source to CSS *and* a [Source Map v3](SourceMap).
///
/// The `css` field is byte-for-byte what [`compile`] returns; the map is built
/// alongside it. The map's `file` is the basename of [`Options::url`] (or
/// `"stdin"` when no URL is set) and its `sources` are the source URLs.
/// [`Options::with_source_map_include_sources`] controls whether each source's
/// full text is embedded in `sourcesContent`.
///
/// V1 granularity maps the start of each selector, declaration property name,
/// at-rule keyword, and comment; the declaration *value* start is not yet
/// mapped.
///
/// # Errors
///
/// Returns [`Error`] on a parse or evaluation failure, like [`compile`].
pub fn compile_with_source_map(source: &str, options: &Options<'_>) -> Result<CompileResult, Error> {
    // Mirror `compile`'s arena bracketing so the returned value is deep-cloned
    // out to the system allocator before the arena is reset.
    let guard = arena::Scope::enter();
    let result = compile_inner_sm(source, options);
    let outermost = arena::leave_no_reset();
    let owned = result.clone();
    drop(result);
    if outermost {
        arena::reset();
    }
    std::mem::forget(guard);
    owned
}

/// The basename of a path/URL (everything after the last `/`), used for the
/// source map's `file` field.
fn basename(url: &str) -> &str {
    url.rsplit('/').next().unwrap_or(url)
}

/// The source-map compile pipeline: parse + evaluate exactly like
/// [`compile_inner`], then emit with the source-map collector and assemble the
/// [`SourceMap`].
fn compile_inner_sm(source: &str, options: &Options<'_>) -> Result<CompileResult, Error> {
    let glyphs = if options.unicode {
        diag::GlyphSet::Unicode
    } else {
        diag::GlyphSet::Ascii
    };
    let sheet = match options.syntax {
        Syntax::Scss => parser::parse(source),
        Syntax::Css => parser::parse_plain_css(source),
        Syntax::Sass => sass_parser::parse(source),
    };
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
                    e.rendered = Some(diag::render_error(&e.message, source, url, span, glyphs));
                }
            }
            return Err(e);
        }
    };
    eval::validate_declarations(&sheet)?;
    // The entry name labels the entry source in the map (`file`/`sources[0]`).
    // It is also the evaluator's `current_url`, so every entry-file node is
    // stamped with a non-zero file id; its source text is kept for
    // `sourcesContent`. The source-map path always passes the real source (so
    // `sourcesContent` works even without a diagnostic URL); this only enriches
    // the *error* path with snippets — the CSS/map success path is unaffected.
    let entry_name = options.url.unwrap_or("stdin");
    let mut ev = eval::Evaluator::new(eval::EvalOptions {
        style: options.style,
        importer: options.importer,
        source,
        url: entry_name,
        glyphs,
    });
    let mut out = Vec::new();
    ev.eval_sheet(&sheet, &mut out)?;
    let (css, body_off, collector) = emit::emit_with_map(&out, options.style);
    let mappings = collector.finalize(&css, body_off).encode();
    let (sources, sources_content) = ev.source_table(entry_name, options.source_map_include_sources);
    let source_map = SourceMap {
        file: Some(basename(entry_name).to_string()),
        sources,
        sources_content,
        mappings,
    };
    Ok(CompileResult { css, source_map })
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
