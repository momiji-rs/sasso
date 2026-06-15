# sasso-ffi — C ABI for sasso

A thin, stable **C ABI** around the [`sasso`](https://crates.io/crates/sasso)
pure-Rust SCSS → CSS compiler. One prebuilt `libsasso.{so,dylib}` + one header
([`include/sasso.h`](include/sasso.h)) is callable from **any language with a C
FFI** — PHP FFI, Python `ctypes`/`cffi`, Ruby `Fiddle`, Go `cgo`, LuaJIT, … —
in-process, with no Node, no Dart VM, and no per-language native extension.

> Status: **v1 scaffold.** Compile + options + load paths are implemented and
> smoke-tested. A userland importer callback is planned for v2 (see Roadmap).

## Build

```console
$ cargo build --release
# -> target/release/libsasso.dylib (macOS) / libsasso.so (Linux) + libsasso.a
```

The header is committed at [`include/sasso.h`](include/sasso.h) (and can be
regenerated with `cbindgen --config cbindgen.toml --output include/sasso.h`).

## API (see [`include/sasso.h`](include/sasso.h))

| Symbol | Purpose |
| --- | --- |
| `const char *sasso_version(void)` | Bundled compiler version (static; do not free). |
| `void sasso_options_init(SassoOptions*)` | Fill an options struct with defaults + `struct_size`. |
| `SassoResult *sasso_compile(const char *src, size_t len, const SassoOptions*)` | Compile a UTF-8 buffer; returns an owned result. |
| `void sasso_result_free(SassoResult*)` | Release a result (and its `css`/`error`). |

### Ownership & safety contract

- **Source** is a UTF-8 `(pointer, length)` buffer — it need **not** be
  NUL-terminated. Host paths (`url`, `load_paths`) **are** NUL-terminated.
- A `SassoResult*` from `sasso_compile` is **owned by sasso**; release it with
  `sasso_result_free` (which frees the `css`/`error` strings too). Never free
  `css`/`error` with your own `free()`.
- `css`/`error` are NUL-terminated **and** carry an explicit byte length
  (`css_len`/`error_len`) for binary-safe callers.
- Every entry point is **panic-safe**: an internal Rust panic becomes an error
  result rather than unwinding across the C boundary (which would be UB).
- `SassoOptions` is `#[repr(C)]` with a leading `struct_size` for forward
  compatibility — initialize with `sasso_options_init`, then override fields.
  Pass `NULL` options for all defaults.

## Examples

- **Python** (ctypes): [`examples/smoke.py`](examples/smoke.py) — also the smoke test:
  ```console
  $ python3 examples/smoke.py
  ```
- **PHP** (ext-ffi, no compiled extension): [`examples/example.php`](examples/example.php)
  ```console
  $ php examples/example.php
  ```

A minimal C use:

```c
#include "sasso.h"
#include <stdio.h>
#include <string.h>

int main(void) {
    const char *src = ".a { color: red; }";
    SassoResult *r = sasso_compile(src, strlen(src), NULL);
    int ok = r->ok;                 /* capture before freeing the result */
    if (ok) printf("%s", r->css);
    else    fprintf(stderr, "%s\n", r->error);
    sasso_result_free(r);
    return ok ? 0 : 1;
}
```

## Roadmap

- **v2 — userland importer callback.** A function-pointer importer
  (`const char* (*)(void *user_data, const char *url, size_t url_len)`) so
  `@import`/`@use`/`@forward` can be resolved from a host (DB, virtual FS,
  archive), mirroring the library's `Importer` trait. Deferred so the v1 ABI can
  stabilize first; the callback's string-ownership and re-entrancy rules are the
  delicate part.
- **Source maps.** The core already exposes `compile_with_source_map`; a
  result-carrying-map variant can follow.

## Relationship to php-sasso

[`shyim/php-sasso`](https://github.com/shyim/php-sasso) is a *compiled* PHP
extension (ext-php-rs). This C ABI is the *lighter, language-agnostic* path
(load the shared library at runtime via FFI) and also serves Python, Go, Ruby,
etc. The two are complementary.
