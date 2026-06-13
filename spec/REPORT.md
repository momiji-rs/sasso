# sasso conformance report

A ratchet that measures **sasso** (our hand-rolled pure-Rust SCSS->CSS
compiler) against the OFFICIAL [`sass/sass-spec`](https://github.com/sass/sass-spec)
suite. Goal: 100% of sass-spec, raised over time.

## Pinned spec version

| | |
|---|---|
| repo | `https://github.com/sass/sass-spec.git` |
| commit | **`1b03109a6205c8cff146defeae8488094b147c88`** |
| upstream date | 2026-06-11 |
| fetched | 2026-06-13 |

Recorded in [`SPEC_VERSION.txt`](./SPEC_VERSION.txt). The upstream tree is
`.gitignore`'d; re-fetch with `bash spec/fetch.sh`.

## Total case count

**13,904 runnable conformance cases** (cases with either an `output.css` or an
`error` expectation), extracted from **3,016 `.hrx` archives** (one archive
holds many cases) plus a handful of loose directory-style cases. `js-api-spec/`
(the JavaScript API tests) is excluded -- it tests the JS binding, not the
SCSS->CSS language.

## Breakdown by top-level spec directory

| Directory | Cases | of which error-specs | Notes |
|-----------|------:|---------------------:|-------|
| `core_functions`        | 8,647 | 1,202 | built-in functions; ~795 archives use `@use "sass:..."` |
| `values`                | 1,227 |   482 | numbers, colors, strings, lists, maps |
| `css`                   |   967 |   238 | plain-CSS passthrough, at-rules, comments |
| `non_conformant`        |   941 |   107 | legacy pre-style-guide specs (still valid) |
| `directives`            |   896 |   240 | `@if/@each/@for/@mixin/@extend/@at-root/@use/...` |
| `libsass-closed-issues` |   595 |    94 | regression tests from libsass issues |
| `expressions`           |   248 |    67 | operator/expression parsing & evaluation |
| `libsass`               |   170 |    18 | libsass-originated tests |
| `callable`              |   101 |    10 | function/mixin argument handling |
| `operators`             |    37 |     2 | arithmetic/comparison operators |
| `libsass-todo-issues`   |    29 |    19 | known-unfixed libsass issues |
| `parser`                |    22 |     8 | parser edge cases |
| `variables`             |    20 |     3 | variable scoping/defaults |
| `libsass-todo-tests`    |     4 |     2 | |
| **TOTAL**               | **13,904** | **2,560** | |

### Scope view: would-attempt vs. skipped (full suite)

The runner tags out-of-scope features so we can see how much is reachable
*today* vs. gated behind features sasso hasn't built yet. Projected over the
whole suite with the default skip tags:

| Skip tag | Cases | Why skipped |
|----------|------:|-------------|
| `use`              | 8,759 | input uses `@use` (incl. `@use "sass:math"` etc.) |
| `indented-syntax`  |   414 | `.sass` indented syntax (`input.sass`) |
| `extend`           |   105 | `@extend` / placeholder `%selectors` |
| `forward`          |    92 | `@forward` |
| `todo`             |     6 | upstream `options.yml :todo: [dart-sass]` |
| **total skipped**  | **9,376** | |
| **would-attempt**  | **4,528** | the reachable surface for our current scope |

> The `@use` skip dominates because `core_functions` was migrated to call
> built-ins through the module system (`@use "sass:color"; color.red(...)`).
> As sasso gains `@use`, drop the tag with `--no-skip use` and the
> attemptable surface jumps. The breakdown is regenerated on every run
> (`skip_breakdown` in `results.json`), so it stays honest as scope changes.

## dart-sass validation of the harness

To prove the harness + normalization are correct, we scored **real dart-sass**
(via `spec/dartsass.sh` = `npx --yes sass --no-source-map`) on samples. A
correct implementation must pass ~100% of the *attempted* (non-skipped) cases;
any failure would mean our normalization is wrong, not the implementation.

* **dart-sass version:** `1.101.0`
* npx cold-starts at ~1s/case, so we validated on representative slices.

| Sample (`--filter`) | total | skip | attempted | PASS | ERROR_EXPECTED | FAIL | PASS% attempted |
|---------------------|------:|-----:|----------:|-----:|---------------:|-----:|----------------:|
| `css/`        (limit 250) | 396 | 146 | 250 | 208 |  42 | **0** | **100.00%** |
| `values/`     (limit 200) | 208 |   8 | 200 |  76 | 124 | **0** | **100.00%** |
| `operators/`  (limit 120) |  37 |   8 |  29 |  29 |   0 | **0** | **100.00%** |
| **combined sample** | 641 | 162 | **479** | 313 | 166 | **0** | **100.00%** |

**Result:** dart-sass scores **100.00% of attempted** across **479 attempted
cases** spanning plain CSS, error specs, values (numbers/colors/lists/maps), and
operators (number formatting / division) -- **0 FAIL**. This confirms the HRX
parser, directory parser, implementation-specific-expectation override, error
categorization, and whitespace normalization are all correct. Error specs are
categorized as `ERROR_EXPECTED` (dart-sass correctly rejecting them), not
counted as failures.

> Reproduce any slice:
> `SASS_BIN=spec/dartsass.sh python3 spec/run_spec.py --filter 'values/' --limit 200`

## How to score sasso (once the binary is built)

The binary does **not** exist yet, so we have **not** scored sasso -- only
validated the harness against dart-sass. When `target/release/sasso` exists:

```sh
cargo build --release                                              # owned by src/
SASS_BIN=target/release/sasso python3 spec/run_spec.py         # whole suite
SASS_BIN=target/release/sasso python3 spec/run_spec.py --filter 'operators/'
SASS_BIN=target/release/sasso python3 spec/run_spec.py --no-skip use   # widen scope
SASS_BIN=target/release/sasso python3 spec/run_spec.py --style compressed
```

The runner exits non-zero if any case is a real `FAIL`, so it drops straight
into CI. It writes `spec/results.json` with per-case status.

### CLI contract sasso must satisfy

`run_spec.py` invokes `sasso --style=<expanded|compressed> <input-file>`,
reads **stdout** for CSS, and treats a **non-zero exit** as "errored" (matched
against error specs). Warnings/deprecations should go to stderr.

## Ratchet plan

1. **Commit a baseline.** After the first real run, record the pass-count
   (PASS + ERROR_EXPECTED of attempted) as the floor.
2. **Never regress.** CI fails if a change drops below the baseline. The
   runner's non-zero exit on FAIL already enforces "no new failures."
3. **Raise the floor.** Each feature that lands -> re-run -> bump the committed
   baseline up. The number only goes up.
4. **Widen scope deliberately.** As `@use`/`@forward`/`@extend`/`.sass` get
   built, drop the matching skip tag (`--no-skip <tag>`), converting thousands
   of SKIPs into attempted cases; pass% of *total* climbs in steps.
5. **Track two numbers.** "PASS% of attempted" = correctness within current
   scope; "PASS% of total" = progress toward 100% of sass-spec. Report both.
