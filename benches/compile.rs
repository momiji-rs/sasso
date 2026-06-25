//! CodSpeed benchmarks for the `sasso` SCSS -> CSS pipeline.
//!
//! Each benchmark drives the public [`sasso::compile`] entry point, which runs
//! the full pipeline (scan -> parse -> evaluate -> emit) over a representative
//! input. Inputs range from a large generated stylesheet to focused snippets
//! that stress nesting, control flow and the color functions.

use divan::{black_box, Bencher};
use sasso::{compile, Options, OutputStyle};

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
