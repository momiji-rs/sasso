#!/usr/bin/env bash
# Build the wasm cdylib, size-optimize it with wasm-opt, and stage it into the
# npm package dir (npm/sasso.wasm). Works both locally and in CI.
#
# Local-dev note: if `rustc` on PATH is Homebrew's, it lacks the wasm rust-std,
# so we fall back to rustup's stable rustc (which has it). CI uses a clean
# rustup toolchain, so no fallback is needed there.
set -euo pipefail
cd "$(dirname "$0")"

TARGET=wasm32-unknown-unknown

rustc_path="$(command -v rustc || true)"
if [[ "$rustc_path" == /opt/homebrew/* || "$rustc_path" == /usr/local/* ]]; then
  for c in "$HOME"/.rustup/toolchains/stable-*/bin/rustc; do
    if [[ -x "$c" ]]; then
      export RUSTC="$c"
      echo "note: PATH rustc is system/Homebrew; using rustup rustc: $RUSTC"
      break
    fi
  done
fi

echo ">> cargo build --release --target $TARGET"
cargo build --lib --release --target "$TARGET"

RAW="target/$TARGET/release/sasso_wasm.wasm"
OUT="npm/sasso.wasm"
mkdir -p npm

if command -v wasm-opt >/dev/null 2>&1; then
  echo ">> wasm-opt -Oz"
  wasm-opt -Oz --enable-bulk-memory --enable-nontrapping-float-to-int "$RAW" -o "$OUT"
else
  echo "note: wasm-opt not found; shipping the unoptimized module"
  cp "$RAW" "$OUT"
fi

raw_sz=$(wc -c < "$RAW"); out_sz=$(wc -c < "$OUT"); gz_sz=$(gzip -9 -c "$OUT" | wc -c)
printf '   raw=%s  wasm-opt=%s  gzip=%s bytes -> %s\n' "$raw_sz" "$out_sz" "$gz_sz" "$OUT"
