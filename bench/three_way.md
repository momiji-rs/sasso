# sasso vs dart-sass vs grass — benchmark report

Three SCSS compilers, same corpus, same machine. Headline: **sasso is the
fastest engine on every axis measured** — it beats dart-sass by 8–16× on
end-to-end CLI runs (and ~12× on pure compute), and edges out `grass` (the
incumbent Rust compiler) by ~1.2–1.4×.

> Reproduce: `cd bench && RUNS=12 WARMUP=3 LOOP_N=200 bash scripts/run_bench.sh`
> (then transcribe the numbers here). Correctness is verified separately — see
> [Correctness](#correctness).

## Environment

| | |
| --- | --- |
| Machine | Apple M2 Max, 12 cores |
| OS | macOS 26.3.1 (arm64) |
| Tool | `hyperfine`, 12 timed runs, 3 warmups |
| sasso | 0.1.0 (this repo, `--release`) |
| dart-sass | 1.100.0 (dart2js 3.12.0) — measured as the cached binary **and** via `npx sass` |
| grass | 0.13.4 (`grass_runner`, `--release`) |

`dart-sass` is measured two ways: the **cached binary** directly (its own
startup, no Node-resolution tax) and via **`npx sass`** (what most build configs
actually invoke — the ~1 s npx resolution is on the record because it's real).

## Results

### 1. Startup cost (compile a 1-rule file ≈ pure process startup)

| Engine | Time | vs sasso |
| --- | --- | --- |
| **sasso** | **2.0 ms** | 1× |
| grass | 2.3 ms | 1.14× slower |
| dart-sass (bin) | 143.8 ms | **72× slower** |
| npx sass | 1.085 s | **545× slower** |

Both Rust engines start in ~2 ms (native binary, no VM). dart-sass pays ~144 ms
of Dart-VM/dart2js boot **per invocation**; `npx sass` adds a ~0.9 s Node module
resolution tax on top. This is what dominates when a build shells out per file.

### 2. Cold single-file (one large file, ~25k lines of CSS out, end-to-end)

| Engine | Time | vs sasso |
| --- | --- | --- |
| **sasso** | **22.8 ms** | 1× |
| grass | 28.0 ms | 1.23× slower |
| dart-sass (bin) | 364.5 ms | **16.0× slower** |
| npx sass | 1.698 s | **74.4× slower** |

### 3. Amortized batch (40 medium files, **one** invocation — startup shared)

| Engine | Total | Per file | vs sasso |
| --- | --- | --- | --- |
| **sasso** | **113.3 ms** | **2.83 ms** | 1× |
| grass | 142.6 ms | 3.57 ms | 1.26× slower |
| dart-sass (bin) | 959.6 ms | 23.99 ms | **8.5× slower** |

(sasso compiles all 40 files in one process via multi-file argv; dart-sass uses
its `dir:dir` mode; grass loops over argv in-process. npx is omitted — its tax is
already on the record above.)

### 4. Pure compile throughput (in-process loop, **startup removed**)

`--loop N` recompiles the same source N times in one process and divides, so
this is parse → evaluate → serialize with **zero** per-invocation startup.

| Source | sasso | grass | sasso advantage |
| --- | --- | --- | --- |
| large file (×200) | **18.17 ms/compile** (55.0/s) | 22.41 ms/compile (44.6/s) | **1.23× faster** |
| handwritten (×1000) | **0.326 ms/compile** (3072/s) | 0.442 ms/compile (2263/s) | **1.36× faster** |

dart-sass has no in-process loop mode here, but its pure compute can be derived:
cold-large (364.5 ms) − startup (143.8 ms) ≈ **~220 ms** of pure compile for the
large file, vs sasso's **18.2 ms** — i.e. **sasso is ~12× faster than dart-sass
on pure compute**, with the remaining end-to-end gap being dart's startup.

## Takeaways

- **vs dart-sass**: 8–16× faster end-to-end on the CLI, ~12× on pure compute,
  72× on startup — and 545× vs the `npx sass` path most pipelines use. As a
  native in-process **library**, sasso has effectively no startup, which is the
  single biggest win when a build compiles many files.
- **vs grass** (the strong incumbent Rust compiler): consistently ~1.2–1.4×
  faster across cold, batch and pure-throughput, while targeting *current*
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
  end-to-end ratio shrinks toward the pure-compute ratio (~12×), and on many
  small files it grows (startup dominates).
