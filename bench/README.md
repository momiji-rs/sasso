# SCSS compiler benchmark harness

Compares SCSS compilers on this machine on two axes that matter:

1. **Process startup cost** — what you pay per invocation when you shell out
   (e.g. `npx sass <file>`) vs running in-process.
2. **Pure compile throughput** — parse → evaluate → serialize, with startup
   removed.

Current engines: **sasso** (this repo's real CLI, in-process throughput via its
own `--loop`/`--quiet` flags), **dart-sass** (`npx sass` + the cached binary),
and **grass** (Rust, in-process via `grass_runner`).

Latest results: [`three_way.md`](./three_way.md) (sasso vs dart-sass vs grass).
The older two-way report is [`dart_vs_grass.md`](./dart_vs_grass.md).

## Layout

```
bench/
├── README.md                 # this file
├── dart_vs_grass.md          # results report (regenerate after a run)
├── corpus/
│   ├── handwritten/
│   │   ├── main.scss         # exercises every requested feature
│   │   └── partials/         # _variables.scss, _mixins.scss (@import targets)
│   ├── generated/large.scss  # big file (gen_corpus.rb), ~26k lines of CSS
│   └── batch/                # 40 medium generated files (amortized-startup test)
├── grass_runner/             # tiny Rust crate: grass CLI wrapper (build --release)
│   └── src/main.rs
├── scripts/
│   ├── gen_corpus.rb         # corpus generator (deterministic, offline)
│   ├── run_bench.sh          # the harness (hyperfine-driven)
│   ├── normalize_css.sh      # whitespace-only CSS normalizer
│   └── canon_css.py          # + canonicalizes color serialization (rgb<->hex)
└── results/                  # raw hyperfine JSON (git-ignorable)
```

## Prerequisites

- macOS/Linux, `bash`, `python3`, `ruby`
- `cargo` (build the runner)
- `npx` (runs dart-sass; first call downloads it, then it's cached)
- `hyperfine` (recommended). If absent, the harness still works for the grass
  `--loop` numbers; for wall-time you'd fall back to a `date +%s.%N` median loop.

## One-time setup

```bash
cd bench

# 1. Build the grass runner (release — required for fair numbers).
cargo build --release --manifest-path grass_runner/Cargo.toml

# 2. (Re)generate the corpus. Deterministic; safe to re-run.
ruby scripts/gen_corpus.rb 400 corpus/generated/large.scss
for i in $(seq -w 1 40); do ruby scripts/gen_corpus.rb 60 corpus/batch/file_$i.scss; done

# 3. Prime the dart-sass cache (downloads on first use).
npx --yes sass --version
```

`gen_corpus.rb N OUTFILE` emits a self-contained SCSS file with `N` themed
components (default 400). Bigger `N` → more work per compile.

## Run the benchmark

```bash
cd bench
RUNS=15 WARMUP=3 LOOP_N=200 bash scripts/run_bench.sh
```

Knobs (env vars): `RUNS` (timed runs), `WARMUP`, `LOOP_N` (in-process loop count
for grass). It prints hyperfine tables and grass `--loop` throughput, and writes
raw JSON to `results/`. Transcribe the key numbers into `dart_vs_grass.md`.

What it measures:

- **Startup** — compiles a ~empty file; that wall time ≈ pure process startup.
  Reported for grass, the dart-sass bin, and `npx sass` (so the npx tax is visible).
- **Cold single-file** — the large generated file, end-to-end (startup + compile).
- **Amortized batch** — the whole `corpus/batch/` dir in **one** invocation
  (dart-sass `dir:dir`; grass loops over argv), so startup is shared across files.
- **grass pure throughput** — `grass_runner --loop N` recompiles in-process N
  times and divides. For dart-sass, pure compute is derived by amortizing startup
  over K copies in one invocation (see methodology in `dart_vs_grass.md`).

## Correctness check

```bash
cd bench
RUNNER=./grass_runner/target/release/grass_runner
SASS=$(find "$HOME/.npm/_npx" -path '*node_modules/.bin/sass' | head -1)

for f in corpus/handwritten/main.scss corpus/generated/large.scss; do
  $SASS   "$f" 2>/dev/null | python3 scripts/canon_css.py > /tmp/dart.canon
  $RUNNER "$f" 2>/dev/null | python3 scripts/canon_css.py > /tmp/grass.canon
  echo "== $f =="; diff /tmp/dart.canon /tmp/grass.canon | head
done
```

`normalize_css.sh` forgives only whitespace; `canon_css.py` additionally folds
`rgb()`↔hex color serialization (rounding, lowercasing) **without reordering**,
so real structural/value divergences still surface. Known cosmetic differences
between dart-sass 1.100 and grass 0.13 are documented in `dart_vs_grass.md`.

## The grass runner

`grass_runner` (depends only on `grass = { version = "0.13.4", default-features = false }`):

```
grass_runner <file.scss> [more.scss ...]   # compile each once -> stdout
grass_runner --loop N <file.scss>          # compile N times in-process (throughput)
grass_runner --quiet ...                    # suppress CSS stdout (timing-only)
```

`--loop` prints a one-line `ms/compile` + `compiles/sec` summary to **stderr**
and the CSS once to stdout (unless `--quiet`). Exit code non-zero on compile error.

## Adding a third engine

The harness is engine-agnostic. To add `sasso`, provide a CLI that matches the
contract the others use:

> **Contract:** `engine <file.scss>` reads an SCSS path from argv and prints the
> compiled CSS to **stdout**; non-CSS chatter (warnings, timings) goes to
> **stderr**; exit non-zero on error. Optionally support `--loop N <file>` for an
> in-process throughput number, and `--quiet` to suppress stdout.

Then:

1. **Build a runner** the same way `grass_runner` wraps `grass` — a tiny
   `bin` crate depending on the `sasso` library, with `from_path -> String`
   in one-shot mode and an N-iteration `--loop` mode. Build `--release`.

2. **Register it in `scripts/run_bench.sh`.** Add a variable next to `GRASS`:
   ```bash
   RUSTSASS=../target/release/sasso        # or wherever it lands
   ```
   and add a `-n "sasso …" "$RUSTSASS …"` line to each `hyperfine` block
   (startup / cold-large / batch), plus a `$RUSTSASS --loop "$LOOP_N" "$LARGE"`
   call in the throughput section. The corpus and `canon_css.py` correctness step
   need no changes.

3. **Re-run** `bash scripts/run_bench.sh`, add a row to the results table in a new
   report (e.g. `three_way.md`), and run the correctness diff against both
   dart-sass and grass.

Because every engine is just "argv in, CSS on stdout," nothing else in the harness
has to change.
