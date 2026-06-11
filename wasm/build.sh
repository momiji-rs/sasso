#!/usr/bin/env bash
# Build BOTH wasm variants, size-optimize each with wasm-opt, and stage them
# into the npm package dir:
#   • size  (default): cargo opt-level=z + wasm-opt -Oz  -> npm/sasso.wasm
#   • speed ("/speed"): cargo opt-level=3 + wasm-opt -O3  -> npm/sasso.speed.wasm
# Both embed the scoped bump arena (see ../src/arena.rs); the arena reservation
# defaults to 32 MiB and is overridable at build time via SASSO_WASM_ARENA_MB
# and at runtime via the loader's `configure({ arenaMiB })`.
#
# Each variant builds in its OWN target dir: switching cargo's opt-level within
# one target dir can reuse a stale artifact (the env override doesn't always
# refingerprint), so isolating them guarantees a true z/3 build each.
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

mkdir -p npm

# build_variant <name> <cargo-opt-level> <wasm-opt-flag> <out.wasm>
build_variant() {
  local name="$1" opt="$2" wopt="$3" out="$4"
  local tdir="target-$name"
  echo ">> [$name] cargo build --release (opt-level=$opt) --target $TARGET"
  CARGO_TARGET_DIR="$tdir" CARGO_PROFILE_RELEASE_OPT_LEVEL="$opt" \
    cargo build --lib --release --target "$TARGET"
  local raw="$tdir/$TARGET/release/sasso_wasm.wasm"
  if command -v wasm-opt >/dev/null 2>&1; then
    echo ">> [$name] wasm-opt $wopt"
    wasm-opt "$wopt" --enable-bulk-memory --enable-nontrapping-float-to-int "$raw" -o "$out"
  else
    echo "note: wasm-opt not found; shipping the unoptimized $name module"
    cp "$raw" "$out"
  fi
  local raw_sz out_sz gz_sz
  raw_sz=$(wc -c <"$raw"); out_sz=$(wc -c <"$out"); gz_sz=$(gzip -9 -c "$out" | wc -c)
  printf '   [%s] raw=%s  wasm-opt=%s  gzip=%s bytes -> %s\n' "$name" "$raw_sz" "$out_sz" "$gz_sz" "$out"
}

build_variant size  z -Oz npm/sasso.wasm
build_variant speed 3 -O3 npm/sasso.speed.wasm

echo ">> done: npm/sasso.wasm (size), npm/sasso.speed.wasm (speed)"
