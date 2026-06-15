//! C ABI for the [`sasso`](https://crates.io/crates/sasso) pure-Rust SCSS → CSS
//! compiler.
//!
//! This is a thin, stable `extern "C"` surface so any language with a C FFI
//! (PHP FFI, Python `ctypes`/`cffi`, Ruby `Fiddle`, Go `cgo`, LuaJIT, …) can
//! drive sasso in-process — one ABI, many languages, no per-language native
//! extension. The generated/curated header is [`include/sasso.h`](../include/sasso.h).
//!
//! ## Contract (read before binding)
//!
//! - **Strings in** are UTF-8 `(pointer, length)` pairs (NOT required to be
//!   NUL-terminated), except host paths (`url`, `load_paths`) which are
//!   NUL-terminated C strings.
//! - **Strings out** ([`SassoResult::css`] / [`SassoResult::error`]) are
//!   NUL-terminated AND carry an explicit byte length; they are owned by sasso
//!   and **must** be released with [`sasso_result_free`] — never with the
//!   caller's own `free()`.
//! - Every entry point is panic-safe: a Rust panic is caught at the boundary
//!   and turned into an error result (a panic unwinding across the C ABI is
//!   undefined behavior).
//! - [`SassoOptions`] is `#[repr(C)]` with a leading `struct_size` for forward
//!   compatibility; fill it with [`sasso_options_init`] and override fields.

use std::ffi::{c_char, CStr, CString};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::PathBuf;
use std::ptr;
use std::slice;

use sasso_core::{compile, FsImporter, Options, OutputStyle, Syntax};

/// Output style: human-readable, indented CSS (`SassoOptions::style`).
pub const SASSO_STYLE_EXPANDED: i32 = 0;
/// Output style: minified, single-line CSS.
pub const SASSO_STYLE_COMPRESSED: i32 = 1;

/// Input syntax: brace/semicolon SCSS (`SassoOptions::syntax`).
pub const SASSO_SYNTAX_SCSS: i32 = 0;
/// Input syntax: indented `.sass`.
pub const SASSO_SYNTAX_SASS: i32 = 1;
/// Input syntax: plain CSS (Sass features rejected, values emitted verbatim).
pub const SASSO_SYNTAX_CSS: i32 = 2;

/// Compile options. `#[repr(C)]`; a `NULL` pointer means "all defaults".
///
/// The leading `struct_size` lets the ABI grow without breaking older callers:
/// initialize with `sasso_options_init(opts, sizeof(SassoOptions))` (which sets
/// it), then set the fields you care about.
#[repr(C)]
pub struct SassoOptions {
    /// `sizeof(SassoOptions)` as the caller sees it — set by [`sasso_options_init`].
    pub struct_size: u32,
    /// One of the `SASSO_STYLE_*` constants. Default `SASSO_STYLE_EXPANDED`.
    pub style: i32,
    /// One of the `SASSO_SYNTAX_*` constants. Default `SASSO_SYNTAX_SCSS`.
    pub syntax: i32,
    /// Non-zero to use Unicode box-drawing glyphs in diagnostics; `0` for ASCII.
    pub unicode: i32,
    /// Optional NUL-terminated UTF-8 display path for diagnostics (enables
    /// byte-exact error snippets). `NULL` to disable.
    pub url: *const c_char,
    /// Optional array of NUL-terminated UTF-8 load paths searched for
    /// `@import`/`@use`/`@forward`. `NULL` (or `load_paths_len == 0`) for none.
    pub load_paths: *const *const c_char,
    /// Number of entries in `load_paths`.
    pub load_paths_len: usize,
}

/// The outcome of a compile. Allocated by [`sasso_compile`]; release with
/// [`sasso_result_free`]. On success `ok == 1` and `css` is set; on failure
/// `ok == 0` and `error` (plus `error_line`/`error_column`) is set.
#[repr(C)]
pub struct SassoResult {
    /// `1` on success, `0` on failure.
    pub ok: i32,
    /// NUL-terminated UTF-8 CSS on success, else `NULL`. Owned by sasso.
    pub css: *mut c_char,
    /// Byte length of `css` (excluding the NUL), or `0`.
    pub css_len: usize,
    /// NUL-terminated UTF-8 diagnostic on failure, else `NULL`. Owned by sasso.
    pub error: *mut c_char,
    /// Byte length of `error` (excluding the NUL), or `0`.
    pub error_len: usize,
    /// 1-based line of the error, or `0` if unknown.
    pub error_line: u32,
    /// 1-based column of the error, or `0` if unknown.
    pub error_column: u32,
}

/// Return the bundled compiler version as a static NUL-terminated string.
///
/// The returned pointer is `'static` and must **not** be freed.
#[no_mangle]
pub extern "C" fn sasso_version() -> *const c_char {
    concat!(env!("CARGO_PKG_VERSION"), "\0").as_ptr() as *const c_char
}

/// Fill `options` with defaults (expanded, SCSS, Unicode diagnostics, no url /
/// load paths). `struct_size` is the caller's `sizeof(SassoOptions)`: only that
/// many bytes are written (capped at this build's size), so a smaller/older
/// caller is never written past. Pass `sizeof(SassoOptions)`.
///
/// # Safety
/// `options` must be `NULL` or point to at least `struct_size` writable bytes.
#[no_mangle]
pub unsafe extern "C" fn sasso_options_init(options: *mut SassoOptions, struct_size: usize) {
    if options.is_null() {
        return;
    }
    let defaults = SassoOptions {
        struct_size: struct_size as u32,
        style: SASSO_STYLE_EXPANDED,
        syntax: SASSO_SYNTAX_SCSS,
        unicode: 1,
        url: ptr::null(),
        load_paths: ptr::null(),
        load_paths_len: 0,
    };
    // Write only what the caller's struct can hold — never past their buffer.
    let n = struct_size.min(std::mem::size_of::<SassoOptions>());
    ptr::copy_nonoverlapping(
        (&defaults as *const SassoOptions).cast::<u8>(),
        options.cast::<u8>(),
        n,
    );
}

/// Read a caller-supplied `SassoOptions` in a version-tolerant way: the leading
/// `struct_size` says how many bytes the caller actually provided, so we copy
/// only that prefix over a defaults-filled local. A caller with an older,
/// smaller struct gets our defaults for the missing tail, and we never read past
/// their allocation. `NULL` yields all defaults.
///
/// # Safety
/// `options` must be `NULL` or point to at least its own leading `struct_size`
/// bytes (the minimal contract a `SassoOptions` pointer already implies).
unsafe fn read_options(options: *const SassoOptions) -> SassoOptions {
    let size = std::mem::size_of::<SassoOptions>();
    let mut local = SassoOptions {
        struct_size: size as u32,
        style: SASSO_STYLE_EXPANDED,
        syntax: SASSO_SYNTAX_SCSS,
        unicode: 1,
        url: ptr::null(),
        load_paths: ptr::null(),
        load_paths_len: 0,
    };
    if options.is_null() {
        return local;
    }
    let caller_size = ptr::read_unaligned(ptr::addr_of!((*options).struct_size)) as usize;
    let n = caller_size.min(size);
    ptr::copy_nonoverlapping(
        options.cast::<u8>(),
        (&mut local as *mut SassoOptions).cast::<u8>(),
        n,
    );
    // Drop any tail field the caller's `struct_size` did not FULLY cover, so we
    // never dereference a half-copied pointer. Gated per field (not "any time
    // n < size") so that if a future version appends fields *after* these
    // pointers, an older caller that did provide url/load_paths keeps them.
    if caller_size < std::mem::offset_of!(SassoOptions, load_paths) {
        local.url = ptr::null(); // url spans [offset_of(url), offset_of(load_paths))
    }
    if caller_size < std::mem::offset_of!(SassoOptions, load_paths_len) {
        local.load_paths = ptr::null();
    }
    if caller_size < std::mem::offset_of!(SassoOptions, load_paths_len) + std::mem::size_of::<usize>() {
        local.load_paths_len = 0;
    }
    local
}

/// Compile `source` (a UTF-8 buffer of `source_len` bytes) to CSS.
///
/// Returns a heap-allocated [`SassoResult`] (never `NULL` under normal
/// operation) that the caller must release with [`sasso_result_free`]. A panic
/// inside the compiler is caught and reported as an error result.
///
/// # Safety
/// `source` must point to `source_len` readable bytes. `options` must be `NULL`
/// or a valid `SassoOptions` whose `url`/`load_paths` (when non-null) point to
/// valid NUL-terminated strings for the duration of the call.
#[no_mangle]
pub unsafe extern "C" fn sasso_compile(
    source: *const c_char,
    source_len: usize,
    options: *const SassoOptions,
) -> *mut SassoResult {
    match catch_unwind(AssertUnwindSafe(|| compile_inner(source, source_len, options))) {
        Ok(result) => result,
        Err(_) => make_error("sasso: internal panic during compilation", 0, 0),
    }
}

/// Release a [`SassoResult`] returned by [`sasso_compile`] (frees the struct and
/// its `css`/`error` strings). Passing `NULL` is a no-op.
///
/// # Safety
/// `result` must be `NULL` or a pointer obtained from [`sasso_compile`] that has
/// not already been freed.
#[no_mangle]
pub unsafe extern "C" fn sasso_result_free(result: *mut SassoResult) {
    if result.is_null() {
        return;
    }
    // Reclaim the box and its owned strings; ignore any (impossible) panic so
    // free never unwinds across the boundary.
    let _ = catch_unwind(AssertUnwindSafe(|| {
        let r = Box::from_raw(result);
        if !r.css.is_null() {
            drop(CString::from_raw(r.css));
        }
        if !r.error.is_null() {
            drop(CString::from_raw(r.error));
        }
    }));
}

/// The real body of [`sasso_compile`], run inside `catch_unwind`.
unsafe fn compile_inner(
    source: *const c_char,
    source_len: usize,
    options: *const SassoOptions,
) -> *mut SassoResult {
    if source.is_null() && source_len != 0 {
        return make_error("sasso: source pointer is null", 0, 0);
    }
    let src_bytes = if source_len == 0 {
        &[][..]
    } else {
        slice::from_raw_parts(source as *const u8, source_len)
    };
    let src = match std::str::from_utf8(src_bytes) {
        Ok(s) => s,
        Err(_) => return make_error("sasso: source is not valid UTF-8", 0, 0),
    };

    // Read the options version-tolerantly (defaults if NULL / for any field the
    // caller's `struct_size` doesn't cover); never reads past the caller's struct.
    let opts = read_options(options);
    let style = match opts.style {
        SASSO_STYLE_EXPANDED => OutputStyle::Expanded,
        SASSO_STYLE_COMPRESSED => OutputStyle::Compressed,
        other => return make_error(&format!("sasso: invalid style {other}"), 0, 0),
    };
    let syntax = match opts.syntax {
        SASSO_SYNTAX_SCSS => Syntax::Scss,
        SASSO_SYNTAX_SASS => Syntax::Sass,
        SASSO_SYNTAX_CSS => Syntax::Css,
        other => return make_error(&format!("sasso: invalid syntax {other}"), 0, 0),
    };
    let unicode = opts.unicode != 0;
    let url_owned: Option<String> = if opts.url.is_null() {
        None
    } else {
        match CStr::from_ptr(opts.url).to_str() {
            Ok(u) => Some(u.to_owned()),
            Err(_) => return make_error("sasso: url is not valid UTF-8", 0, 0),
        }
    };
    let mut load_paths: Vec<PathBuf> = Vec::new();
    if !opts.load_paths.is_null() && opts.load_paths_len > 0 {
        let arr = slice::from_raw_parts(opts.load_paths, opts.load_paths_len);
        for &p in arr {
            if p.is_null() {
                continue;
            }
            match CStr::from_ptr(p).to_str() {
                Ok(s) => load_paths.push(PathBuf::from(s)),
                Err(_) => return make_error("sasso: a load path is not valid UTF-8", 0, 0),
            }
        }
    }

    let mut o = Options::new()
        .with_style(style)
        .with_syntax(syntax)
        .with_unicode(unicode);
    if let Some(u) = &url_owned {
        o = o.with_url(u);
    }
    // `FsImporter` must outlive the `compile` borrow, so bind it here.
    let fs;
    if !load_paths.is_empty() {
        fs = FsImporter::new(load_paths);
        o = o.with_importer(&fs);
    }

    match compile(src, &o) {
        Ok(css) => make_success(css),
        Err(e) => make_error(&e.to_string(), saturate_u32(e.line), saturate_u32(e.col)),
    }
}

/// Narrow a core `usize` line/column to the ABI's `u32`, saturating instead of
/// wrapping. Truncation needs a >4-billion-line/column source (>4 GiB), so this
/// is defensive only — but a silent wraparound would report a wrong small
/// position, whereas saturating keeps it unmistakably large.
fn saturate_u32(v: usize) -> u32 {
    u32::try_from(v).unwrap_or(u32::MAX)
}

/// Box a success result, moving `css` into an owned C string.
fn make_success(css: String) -> *mut SassoResult {
    let len = css.len();
    let css_c = match CString::new(css) {
        Ok(c) => c.into_raw(),
        Err(_) => return make_error("sasso: output contained an interior NUL byte", 0, 0),
    };
    Box::into_raw(Box::new(SassoResult {
        ok: 1,
        css: css_c,
        css_len: len,
        error: ptr::null_mut(),
        error_len: 0,
        error_line: 0,
        error_column: 0,
    }))
}

/// Box an error result. A message with an interior NUL (not expected from
/// sasso) falls back to a fixed string so a result is always produced.
fn make_error(message: &str, line: u32, col: u32) -> *mut SassoResult {
    let (err_c, len) = match CString::new(message) {
        Ok(c) => (c.into_raw(), message.len()),
        Err(_) => {
            let fallback = "sasso: error (message contained an interior NUL byte)";
            (CString::new(fallback).unwrap().into_raw(), fallback.len())
        }
    };
    Box::into_raw(Box::new(SassoResult {
        ok: 0,
        css: ptr::null_mut(),
        css_len: 0,
        error: err_c,
        error_len: len,
        error_line: line,
        error_column: col,
    }))
}
