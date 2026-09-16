//! CodSpeed benchmarks for the `sasso` SCSS -> CSS pipeline.
//!
//! Each benchmark drives the public [`sasso::compile`] entry point, which runs
//! the full pipeline (scan -> parse -> evaluate -> emit) over a representative
//! input. Inputs range from a large generated stylesheet to focused snippets
//! that stress nesting, control flow and the color functions.
//!
//! **These benchmarks must measure the program that ships.** Two ways of
//! getting that wrong were measured on 2026-09-16 and are fixed below; see
//! `docs/PERF_PLAN_2026-09-16.md` (B0) for the numbers.
//!
//! 1. The `ScopedAlloc` bump arena is installed here, exactly as `src/main.rs`
//!    and `wasm/src/lib.rs` install it. Without it every recorded number
//!    measures sasso against the system malloc: -41% on macOS, -22% on Linux,
//!    and it *inverts* the cross-machine ranking rather than shifting it by a
//!    constant.
//! 2. At least one workload supplies a display URL. `compile` enables
//!    diagnostics only when `options.url` is `Some`, so without one the whole
//!    deprecation path -- ~16% of a compile, and where the 2026-09-15
//!    regression landed -- never executes.

use divan::{black_box, Bencher};
use sasso::{compile, Options, OutputStyle};

/// The allocator the CLI and the wasm package ship with (`src/main.rs:40`,
/// `wasm/src/lib.rs:40`). Benchmarking without it measures a different program.
#[global_allocator]
static GLOBAL: sasso::ScopedAlloc = sasso::ScopedAlloc;

fn main() {
    divan::main();
}

/// ~400-component generated stylesheet (`bench/corpus/generated/large.scss`).
/// Self-contained: only pulls in the built-in `sass:math` module.
const LARGE: &str = include_str!("../bench/corpus/generated/large.scss");

/// Deeply nested rules exercising the `&` parent selector and selector emission.
const NESTING: &str = r#"
.card {
  color: #333;
  .header {
    font-weight: bold;
    &:hover { color: #0066cc; }
    .title { a { color: red; &:visited { color: purple; } } }
  }
  .body {
    p { margin: 0; line-height: 1.5; }
    .footer { small { opacity: 0.6; &::after { content: ""; } } }
  }
}
"#;

/// Control-flow heavy input: `@for` / `@each` loops with interpolation.
const CONTROL_FLOW: &str = r#"
@use "sass:math";
@for $i from 1 through 100 {
  .col-#{$i} { width: math.div($i, 100) * 100%; }
}
@each $name, $size in (sm: 4px, md: 8px, lg: 16px, xl: 32px) {
  .pad-#{$name} { padding: $size; }
  .gap-#{$name} { gap: $size; }
}
"#;

/// Color-function heavy input: builds a shade scale from a base color.
const COLORS: &str = r#"
@use "sass:color";
$base: #3498db;
@for $i from 1 through 40 {
  .shade-#{$i} {
    background: color.adjust($base, $lightness: $i * 1%);
    border-color: rgba($base, 0.5);
    box-shadow: 0 0 ($i * 1px) mix($base, white, 50%);
  }
}
"#;

#[divan::bench]
fn large_expanded(bencher: Bencher<'_, '_>) {
    bencher.bench(|| compile(black_box(LARGE), &Options::default()).unwrap());
}

/// `large_expanded` with diagnostics live: a display URL turns on the
/// deprecation bookkeeping that the URL-less benchmarks skip entirely.
///
/// The handler is a no-op on purpose. A benchmark has no business writing to
/// stderr, and it costs nothing to stay quiet: printing measured 9.478 ms
/// against this variant's 9.408 ms fastest on macOS/arm64, the same within
/// noise, so the surcharge is bookkeeping rather than I/O.
///
/// Read this **against** `large_expanded`. The ratio of the two is the
/// diagnostics surcharge, and it is that ratio, not either number alone, that
/// would have flagged the two deprecation merges of 2026-09-15. Measured at
/// +15.7% (macOS/arm64) and +9.6% (Linux/x86_64) on 2026-09-16 against the
/// printing arm, and +12.5% fastest / +10.8% median here against the silent
/// one; treat the spread as the reason CI wants instruction counts rather than
/// wall time.
#[divan::bench]
fn large_expanded_with_url_silent(bencher: Bencher<'_, '_>) {
    bencher.bench(|| {
        // `WarnHandler` is an `Rc`, which is not `Sync`, so divan cannot hoist
        // it out of the benched closure -- it has to be built in here. One
        // `Rc::new` per iteration is noise against a ~9 ms compile.
        let sink: sasso::WarnHandler = std::rc::Rc::new(|_ev: &sasso::WarnEvent<'_>| {});
        let opts = Options::default().with_url("large.scss").with_warn_handler(sink);
        compile(black_box(LARGE), &opts).unwrap()
    });
}

#[divan::bench]
fn large_compressed(bencher: Bencher<'_, '_>) {
    bencher.bench(|| {
        let mut opts = Options::default();
        opts.style = OutputStyle::Compressed;
        compile(black_box(LARGE), &opts).unwrap()
    });
}

#[divan::bench]
fn nesting(bencher: Bencher<'_, '_>) {
    bencher.bench(|| compile(black_box(NESTING), &Options::default()).unwrap());
}

#[divan::bench]
fn control_flow(bencher: Bencher<'_, '_>) {
    bencher.bench(|| compile(black_box(CONTROL_FLOW), &Options::default()).unwrap());
}

#[divan::bench]
fn colors(bencher: Bencher<'_, '_>) {
    bencher.bench(|| compile(black_box(COLORS), &Options::default()).unwrap());
}
