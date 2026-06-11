//! Minimal raw-cdylib wasm export wrapper around [`sasso::compile`].
//!
//! No wasm-bindgen: the ABI is a hand-rolled `alloc` / `free` / `compile`
//! trio so the module stays tiny and dependency-free. The JS loader
//! (`npm/sasso.mjs`) marshals UTF-8 in and out of linear memory by hand.
//!
//! Protocol: JS calls `sasso_alloc(len)` to get a buffer, writes the SCSS
//! bytes, then calls `sasso_compile(ptr, len, compressed, out_len_ptr,
//! ok_ptr)`. That returns a pointer to a UTF-8 result (CSS on success, the
//! error message on failure); the length is written to `*out_len_ptr` and a
//! `1`/`0` ok flag to `*ok_ptr`. JS reads the bytes, then frees the input,
//! the result, and any scratch with `sasso_free(ptr, len)`.

// These are FFI entry points: they dereference raw pointers by contract (the
// JS loader in npm/sasso.mjs upholds the invariants documented per function).
// Keeping the C ABI signatures pointer-typed (not `unsafe fn`) is intentional.
#![allow(clippy::not_unsafe_ptr_arg_deref)]

use std::alloc::{alloc, dealloc, Layout};

// PoC: install sasso's scoped bump arena as the wasm global allocator. Every
// allocation inside a `compile()` scope becomes a pointer bump from a single
// pre-grown region that is reset (not freed) when the compile ends; the FFI
// buffers below (sasso_alloc / the boxed result) are allocated OUTSIDE any
// scope, so they route to the system allocator and outlive the reset. On
// wasm32 the region is 128 MiB (see arena.rs) — grown once on the first
// compile and reused. Outside a scope every request forwards to System, so
// installing it is safe even though sasso_alloc runs before compile.
#[global_allocator]
static GLOBAL: sasso::ScopedAlloc = sasso::ScopedAlloc;

/// Override the bump arena's reservation size, in **bytes**, before the first
/// `sasso_compile`. `0` disables the arena (every allocation forwards to the
/// system allocator: lower memory, slower). The region is reserved on the
/// first compile and then fixed, so this is a no-op once compilation started.
/// The compile-time default is 32 MiB (or `SASSO_WASM_ARENA_MB` at build time).
#[no_mangle]
pub extern "C" fn sasso_set_arena_bytes(bytes: usize) {
    sasso::set_arena_bytes(bytes);
}

/// Allocate `len` bytes in linear memory (align 1, for UTF-8 byte buffers).
///
/// Returns null for `len == 0`. Free with [`sasso_free`].
#[no_mangle]
pub extern "C" fn sasso_alloc(len: usize) -> *mut u8 {
    if len == 0 {
        return std::ptr::null_mut();
    }
    match Layout::from_size_align(len, 1) {
        // SAFETY: len > 0, so the layout is non-zero-sized.
        Ok(layout) => unsafe { alloc(layout) },
        Err(_) => std::ptr::null_mut(),
    }
}

/// Free a buffer returned by [`sasso_alloc`] or [`sasso_compile`].
///
/// `(ptr, len)` must be exactly a pair previously handed to JS by this module.
#[no_mangle]
pub extern "C" fn sasso_free(ptr: *mut u8, len: usize) {
    if ptr.is_null() || len == 0 {
        return;
    }
    if let Ok(layout) = Layout::from_size_align(len, 1) {
        // SAFETY: the caller passes back a (ptr, len) pair this module
        // allocated with the matching align-1 layout.
        unsafe { dealloc(ptr, layout) };
    }
}

/// Compile the SCSS in `[input_ptr, input_ptr + input_len)` to CSS.
///
/// `compressed != 0` selects compressed output. Writes the result byte length
/// to `*out_len_ptr` and `1` (ok) / `0` (error) to `*ok_ptr`, and returns a
/// pointer to the UTF-8 result — the CSS on success, or the error message on
/// failure. Free the result with `sasso_free(result_ptr, *out_len_ptr)`.
#[no_mangle]
pub extern "C" fn sasso_compile(
    input_ptr: *const u8,
    input_len: usize,
    compressed: u8,
    out_len_ptr: *mut usize,
    ok_ptr: *mut u8,
) -> *mut u8 {
    let input: &[u8] = if input_ptr.is_null() || input_len == 0 {
        &[]
    } else {
        // SAFETY: JS guarantees [input_ptr, input_len) is a live buffer it
        // just allocated via sasso_alloc and filled.
        unsafe { std::slice::from_raw_parts(input_ptr, input_len) }
    };

    let (bytes, ok): (Vec<u8>, u8) = match std::str::from_utf8(input) {
        Ok(scss) => {
            let mut opts = sasso::Options::default();
            if compressed != 0 {
                opts = opts.with_style(sasso::OutputStyle::Compressed);
            }
            match sasso::compile(scss, &opts) {
                Ok(css) => (css.into_bytes(), 1),
                Err(err) => (err.to_string().into_bytes(), 0),
            }
        }
        Err(_) => (b"input is not valid UTF-8".to_vec(), 0),
    };

    let len = bytes.len();
    let mut boxed = bytes.into_boxed_slice();
    let ptr = boxed.as_mut_ptr();
    std::mem::forget(boxed);
    // SAFETY: out_len_ptr / ok_ptr are valid 4-byte / 1-byte scratch cells
    // that JS allocated before the call.
    unsafe {
        *out_len_ptr = len;
        *ok_ptr = ok;
    }
    ptr
}
