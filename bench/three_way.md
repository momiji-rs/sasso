# sasso vs dart-sass vs grass — benchmark report

Three SCSS compilers, same corpus, same machine. Headline: **sasso is the
fastest engine on every axis measured** — it beats dart-sass by ~10–19× on
end-to-end CLI runs (and ~16× on pure compute), and now leads `grass` (the
incumbent Rust compiler) by **~1.5×** after the allocation/hashing optimizations
(borrowed selector slices + FxHash; see the repo CHANGELOG).

> Reproduce: `cd bench && RUNS=12 WARMUP=3 LOOP_N=200 bash scripts/run_bench.sh`
> (then transcribe the numbers here). Correctness is verified separately — see
> [Correctness](#correctness).

## Environment

| | |
| --- | --- |
| Machine | Apple M2 Max, 12 cores |
| OS | macOS 26.3.1 (arm64) |
| Tool | `hyperfine`, 12 timed runs, 3 warmups |
| sasso | 0.1.0 (this repo, `--release`, with the perf #1/#3 optimizations) |
| dart-sass | 1.100.0 (dart2js 3.12.0) — measured as the cached binary **and** via `npx sass` |
| grass | 0.13.4 (`grass_runner`, `--release`, same `lto`/`codegen-units` as sasso) |

`dart-sass` is measured two ways: the **cached binary** directly (its own
startup, no Node-resolution tax) and via **`npx sass`** (what most build configs
actually invoke — the npx resolution is on the record because it's real).

## Results

### 1. Startup cost (compile a 1-rule file ≈ pure process startup)

| Engine | Time | vs sasso |
| --- | --- | --- |
| **sasso** | **1.7 ms** | 1× |
| grass | 1.8 ms | 1.02× slower |
| dart-sass (bin) | 142.0 ms | **82× slower** |
| npx sass | 566.8 ms | **328× slower** |

Both Rust engines start in ~2 ms (native binary, no VM). dart-sass pays ~142 ms
of Dart-VM/dart2js boot **per invocation**; `npx sass` adds Node module
resolution on top. This dominates when a build shells out per file.

### 2. Cold single-file (one large file, ~25k lines of CSS out, end-to-end)

| Engine | Time | vs sasso |
| --- | --- | --- |
| **sasso** | **18.9 ms** | 1× |
| grass | 27.8 ms | **1.47× slower** |
| dart-sass (bin) | 362.8 ms | **19.2× slower** |
| npx sass | 1.047 s | **55.5× slower** |

### 3. Amortized batch (40 medium files, **one** invocation — startup shared)

| Engine | Total | Per file | vs sasso |
| --- | --- | --- | --- |
| **sasso** | **90.9 ms** | **2.27 ms** | 1× |
| grass | 138.6 ms | 3.47 ms | **1.52× slower** |
| dart-sass (bin) | 943.2 ms | 23.58 ms | **10.4× slower** |

(sasso compiles all 40 files in one process via multi-file argv; dart-sass uses
its `dir:dir` mode; grass loops over argv in-process. npx is omitted — its tax is
already on the record above.)

### 4. Pure compile throughput (in-process loop, **startup removed**)

`--loop N` recompiles the same source N times in one process and divides, so
this is parse → evaluate → serialize with **zero** per-invocation startup.

| Source | sasso | grass | sasso advantage |
| --- | --- | --- | --- |
| large file (×200) | **14.0 ms/compile** (71.4/s) | 21.8 ms/compile (46.0/s) | **1.55× faster** |
| handwritten (×1000) | **0.272 ms/compile** (3676/s) | 0.411 ms/compile (2434/s) | **1.51× faster** |

dart-sass has no in-process loop mode here, but its pure compute can be derived:
cold-large (362.8 ms) − startup (142.0 ms) ≈ **~221 ms** of pure compile for the
large file, vs sasso's **14.0 ms** — i.e. **sasso is ~16× faster than dart-sass
on pure compute**, with the remaining end-to-end gap being dart's startup.

## What changed since the last report

The earlier report had sasso at 22.8 ms cold-large and ~1.2–1.4× over grass. Two
allocation/hashing optimizations (no behavior change, byte-identical output)
moved the needle:

| | before | after |
| --- | --- | --- |
| cold large | 22.8 ms | **18.9 ms** |
| pure compile (large) | ~18 ms | **14.0 ms** |
| vs grass (pure) | 1.23× | **1.55×** |
| vs dart-sass (cold) | 16× | **19.2×** |

- **Borrowed selector slices** — `split_commas`/`tokenize_complex`/`copy_name`
  return `&str` instead of allocating per part/token/name.
- **FxHash** — the compiler's `String`-keyed maps use a fast inline hasher
  instead of SipHash. (Still zero runtime dependencies.)

A further round (boxing strings in `Rc`, pre-sizing Vecs) was measured and
**reverted** — both were neutral-to-negative on this already-optimized codebase
(`Rc::new` adds a per-construction alloc; eager `with_capacity` over-allocates vs
lazy `Vec::new()`). The win came from eliminating whole classes of allocation,
not micro-tuning individual ones.

## Takeaways

- **vs dart-sass**: ~10–19× faster end-to-end on the CLI, ~16× on pure compute,
  82× on startup — and ~55× vs the `npx sass` path most pipelines use. As a
  native in-process **library**, sasso has effectively no startup, which is the
  single biggest win when a build compiles many files.
- **vs grass** (the strong incumbent Rust compiler): now ~1.5× faster across
  cold, batch and pure-throughput (was ~1.2–1.4×), while targeting *current*
  dart-sass semantics (CSS Color 4, modern color serialization) that grass
  (pinned to dart-sass 1.54.3) predates.

## Correctness

Speed only counts if the output matches. On both corpus files, sasso is
**byte-identical to dart-sass 1.100.0** after whitespace + color-serialization
canonicalization:

```bash
cd bench
SASS=$(find "$HOME/.npm/_npx" -path '*node_modules/.bin/sass' | head -1)
for f in corpus/handwritten/main.scss corpus/generated/large.scss; do
  "$SASS"               "$f" | python3 scripts/canon_css.py > /tmp/dart.canon
  ../target/release/sasso "$f" | python3 scripts/canon_css.py > /tmp/sasso.canon
  echo "== $f =="; diff /tmp/dart.canon /tmp/sasso.canon && echo "byte-identical"
done
# => both: byte-identical
```

The broader correctness oracle is the official sass-spec suite — see the
repo-root [Conformance](../README.md#conformance) section (ratcheted, currently
~82% of the suite).

## Caveats

- One machine (Apple M2 Max), one corpus. Absolute numbers are hardware- and
  corpus-dependent; the **ratios** are the durable signal.
- The corpus exercises the feature subset sasso implements (variables, nesting,
  `&`, interpolation, unit math, color functions, `@mixin`/`@function`, control
  flow, `@import` partials). It is not the full sass-spec surface.
- dart-sass's per-invocation startup is fixed cost; on a single huge file the
  end-to-end ratio shrinks toward the pure-compute ratio (~16×), and on many
  small files it grows (startup dominates).
