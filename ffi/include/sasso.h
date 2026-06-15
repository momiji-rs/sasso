/* sasso.h — C ABI for the sasso pure-Rust SCSS -> CSS compiler.
 *
 * One ABI, many languages: load the prebuilt `libsasso.{so,dylib}` from PHP
 * FFI, Python ctypes/cffi, Ruby Fiddle, Go cgo, LuaJIT, etc.
 *
 * Ownership & safety contract:
 *   - Source is a UTF-8 (pointer, length) buffer; it need NOT be
 *     NUL-terminated. Host paths (`url`, `load_paths`) ARE NUL-terminated.
 *   - A SassoResult* returned by sasso_compile() is owned by sasso; release it
 *     (and its css/error strings) with sasso_result_free(). Never free the
 *     css/error pointers with your own free().
 *   - The css/error strings are NUL-terminated AND carry an explicit byte
 *     length (css_len/error_len) so binary-safe callers can avoid strlen().
 *   - Every entry point is panic-safe: an internal Rust panic becomes an error
 *     result rather than crossing the C boundary.
 *
 * This header is curated to match the ABI exactly; it can also be regenerated
 * with `cbindgen` (see cbindgen.toml).
 */
#ifndef SASSO_H
#define SASSO_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* SassoOptions.style */
#define SASSO_STYLE_EXPANDED   0
#define SASSO_STYLE_COMPRESSED 1

/* SassoOptions.syntax */
#define SASSO_SYNTAX_SCSS 0
#define SASSO_SYNTAX_SASS 1
#define SASSO_SYNTAX_CSS  2

/* Compile options. Pass NULL to sasso_compile() for all-defaults, or fill one
 * with sasso_options_init() (which sets struct_size) and override fields. */
typedef struct SassoOptions {
  /* sizeof(SassoOptions) as the caller sees it (forward-compat anchor). */
  uint32_t struct_size;
  /* One of SASSO_STYLE_*. Default SASSO_STYLE_EXPANDED. */
  int32_t style;
  /* One of SASSO_SYNTAX_*. Default SASSO_SYNTAX_SCSS. */
  int32_t syntax;
  /* Non-zero = Unicode diagnostic glyphs; 0 = ASCII. Default non-zero. */
  int32_t unicode;
  /* Optional NUL-terminated UTF-8 display path (enables error snippets), or NULL. */
  const char *url;
  /* Optional array of NUL-terminated UTF-8 load paths, or NULL. */
  const char *const *load_paths;
  /* Number of entries in load_paths. */
  size_t load_paths_len;
} SassoOptions;

/* Result of a compile. Allocated by sasso_compile(); free with
 * sasso_result_free(). */
typedef struct SassoResult {
  /* 1 = success (css set), 0 = failure (error set). */
  int32_t ok;
  /* NUL-terminated UTF-8 CSS on success, else NULL. Owned by sasso. */
  char *css;
  /* Byte length of css (excluding NUL), or 0. */
  size_t css_len;
  /* NUL-terminated UTF-8 diagnostic on failure, else NULL. Owned by sasso. */
  char *error;
  /* Byte length of error (excluding NUL), or 0. */
  size_t error_len;
  /* 1-based error line, or 0 if unknown. */
  uint32_t error_line;
  /* 1-based error column, or 0 if unknown. */
  uint32_t error_column;
} SassoResult;

/* Bundled compiler version as a static NUL-terminated string. Do NOT free. */
const char *sasso_version(void);

/* Fill *options with defaults and set struct_size to the caller's
 * sizeof(SassoOptions). Only that many bytes are written, so an older/smaller
 * caller is never written past — pass sizeof(SassoOptions). No-op if NULL. */
void sasso_options_init(SassoOptions *options, size_t struct_size);

/* Compile a UTF-8 source buffer (source_len bytes) to CSS. Returns a heap
 * SassoResult* the caller must release with sasso_result_free(). options may be
 * NULL for defaults. */
SassoResult *sasso_compile(const char *source, size_t source_len,
                           const SassoOptions *options);

/* Release a SassoResult* from sasso_compile() (and its css/error). NULL-safe. */
void sasso_result_free(SassoResult *result);

#ifdef __cplusplus
} /* extern "C" */
#endif

#endif /* SASSO_H */
