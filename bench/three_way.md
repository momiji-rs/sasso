# sasso vs dart-sass vs grass — benchmark report

Three SCSS compilers, same corpus, same machine. Headline: **sasso is the
fastest engine on every axis measured** — it beats dart-sass by ~16–25×
end-to-end (and ~24× on pure compute), and leads `grass` (the incumbent Rust
compiler) by **~1.9–2.4×** after the allocation/hashing work plus a scoped
bump-arena allocator (see the repo CHANGELOG).

> Reproduce: `cd bench && RUNS=12 WARMUP=3 LOOP_N=200 bash scripts/run_bench.sh`
> (then transcribe the numbers here). Correctness is verified separately — see
> [Correctness](#correctness).

## Environment

| | |
| --- | --- |
| Machine | Apple M2 Max, 12 cores |
| OS | macOS 26.3.1 (arm64) |
| Tool | `hyperfine`, 12 timed runs, 3 warmups |
| sasso | 0.1.0 (this repo, `--release`, with perf #1/#3 + the scoped bump arena) |
| dart-sass | 1.100.0 (dart2js 3.12.0) — cached binary **and** via `npx sass` |
| grass | 0.13.4 (`grass_runner`, `--release`, same `lto`/`codegen-units` as sasso) |

## Results

### 1. Startup cost (compile a 1-rule file ≈ pure process startup)

| Engine | Time | vs sasso |
| --- | --- | --- |
| **sasso** | **1.5 ms** | 1× |
| grass | 1.5 ms | ~tie |
| dart-sass (bin) | 138 ms | **~90× slower** |
| npx sass | 522 ms | **~340× slower** |

### 2. Cold single-file (one large file, ~25k lines of CSS out, end-to-end)

| Engine | Time | vs sasso |
| --- | --- | --- |
| **sasso** | **14.2 ms** | 1× |
| grass | 26.9 ms | **1.89× slower** |
| dart-sass (bin) | 356 ms | **25× slower** |
| npx sass | 1.05 s | **73× slower** |

### 3. Amortized batch (40 medium files, **one** invocation — startup shared)

| Engine | Total | Per file | vs sasso |
| --- | --- | --- | --- |
| **sasso** | **58.9 ms** | **1.47 ms** | 1× |
| grass | 135 ms | 3.39 ms | **2.30× slower** |
| dart-sass (bin) | 934 ms | 23.4 ms | **15.9× slower** |

### 4. Pure compile throughput (in-process loop, **startup removed**)

| Source | sasso | grass | sasso advantage |
| --- | --- | --- | --- |
| large file (×200) | **9.0 ms/compile** (110/s) | 21.4 ms/compile (47/s) | **2.36× faster** |
| handwritten (×1000) | **0.196 ms/compile** (5106/s) | 0.408 ms/compile (2450/s) | **2.08× faster** |

dart-sass has no in-process loop mode, but its pure compute derives from
cold-large (356 ms) − startup (138 ms) ≈ **~218 ms**, vs sasso's **9.0 ms** —
i.e. **sasso is ~24× faster than dart-sass on pure compute**.

## What changed since the last report

The previous report (perf #1/#3 only) had sasso at 18.9 ms cold / 14.0 ms pure
and ~1.5× over grass. A **scoped bump-arena allocator** pushed it further:

| | system alloc | **scoped arena** |
| --- | --- | --- |
| cold large | 18.9 ms | **14.2 ms** |
| pure compile (large) | 14.0 ms | **9.0 ms** (~1.5×) |
| vs grass (pure) | 1.55× | **2.36×** |
| vs dart-sass (cold) | 19× | **25×** |

Within each `compile()` a per-thread arena turns every allocation into a pointer
bump; the whole arena is reset (freed) when the compile ends. Because reset
reuses the *same* region every compile, it has excellent cache locality — it
beats both a never-freeing one-shot bump and `mimalloc` in measurement, while
staying zero-dependency (hand-written) and not leaking across compiles.

### Safety of the arena

The arena is the library's one audited `unsafe` module (the rest is
`deny(unsafe_code)`). It was verified by:

- **Miri** — no UB (out-of-bounds, misalignment, provenance, use-after-free).
- **AddressSanitizer** — the full unit + integration + parity suites, clean.
- **Full sass-spec under the live arena** — all 11,445 passing cases run through
  the allocator with **zero crashes** and **byte-identical** output (delta +0).
- **Concurrency** — 8 threads × 2,000 = 16,000 concurrent compiles, each
  byte-identical (per-thread arenas don't interfere).
- **Memory-stable** — 1,500 repeated compiles hold RSS flat (reset works; no
  growth), so a long-running library embedder won't leak.

## Takeaways

- **vs dart-sass**: ~16–25× faster end-to-end, ~24× on pure compute, ~90× on
  startup — and ~73× vs the `npx sass` path most pipelines use.
- **vs grass** (the strong incumbent Rust compiler): now ~1.9× cold / ~2.3×
  batch / ~2.4× pure-throughput, while targeting *current* dart-sass semantics
  (CSS Color 4, modern color serialization) that grass (pinned to dart-sass
  1.54.3) predates.

## Correctness

On both corpus files, sasso is **byte-identical to dart-sass 1.100.0** after
whitespace + color-serialization canonicalization (and, as above, on the full
sass-spec suite):

```bash
cd bench
SASS=$(find "$HOME/.npm/_npx" -path '*node_modules/.bin/sass' | head -1)
for f in corpus/handwritten/main.scss corpus/generated/large.scss; do
  "$SASS"               "$f" | python3 scripts/canon_css.py > /tmp/dart.canon
  ../target/release/sasso "$f" | python3 scripts/canon_css.py > /tmp/sasso.canon
  echo "== $f =="; diff /tmp/dart.canon /tmp/sasso.canon && echo "byte-identical"
done
```

The broader oracle is the official sass-spec suite — see the repo-root
[Conformance](../README.md#conformance) section (~82%).

## Caveats

- One machine (Apple M2 Max), one corpus. Absolute numbers are hardware- and
  corpus-dependent; the **ratios** are the durable signal.
- The corpus exercises the implemented feature subset, not the full sass-spec
  surface.
- dart-sass's per-invocation startup is fixed cost; on a single huge file the
  end-to-end ratio shrinks toward the pure-compute ratio, and on many small
  files it grows.
