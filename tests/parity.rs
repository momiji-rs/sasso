//! Live parity tests against dart-sass.
//!
//! These are **opt-in**: they only run when `SASSO_PARITY=1` is set and
//! a dart-sass CLI is reachable (via `$SASS_BIN`, default `npx --yes sass`).
//! Otherwise each test returns early so a plain `cargo test` stays fast and
//! offline. CI sets the env var and installs dart-sass.

use std::io::Write as _;
use std::process::{Command, Stdio};

use sasso::{compile, Options};

fn enabled() -> bool {
    std::env::var("SASSO_PARITY").map(|v| v != "0").unwrap_or(false)
}

/// Compile `scss` with dart-sass (expanded, via stdin), returning its CSS.
fn dart_sass(scss: &str) -> Option<String> {
    let bin = std::env::var("SASS_BIN").unwrap_or_else(|_| "npx".to_string());
    let mut cmd = if bin == "npx" {
        let mut c = Command::new("npx");
        c.args(["--yes", "sass", "--no-source-map", "--stdin"]);
        c
    } else {
        let mut c = Command::new(bin);
        c.args(["--no-source-map", "--stdin"]);
        c
    };
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    child.stdin.take()?.write_all(scss.as_bytes()).ok()?;
    let out = child.wait_with_output().ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8(out.stdout).ok()
}

fn assert_parity(scss: &str) {
    if !enabled() {
        return;
    }
    let ours = compile(scss, &Options::default()).expect("our compile failed");
    match dart_sass(scss) {
        Some(theirs) => assert_eq!(ours, theirs, "\n--- scss ---\n{scss}\n"),
        None => eprintln!("skipping parity case: dart-sass unavailable"),
    }
}

#[test]
fn parity_variables_nesting() {
    assert_parity("$c: #336699;\n.a {\n  color: $c;\n  .b { color: lighten($c, 10%); }\n  &:hover { color: mix($c, white, 50%); }\n}\n");
}

#[test]
fn parity_colors() {
    assert_parity("$brand: #2a7ae2;\n.x {\n  color: rgba($brand, 0.5);\n  background: darken($brand, 15%);\n  border-color: hsl(120, 50%, 40%);\n  width: percentage(0.25);\n}\n");
}

#[test]
fn parity_nesting_combinators() {
    assert_parity(".a, .b {\n  margin: 0;\n  > .c { padding: 1px; }\n  &.active { color: red; }\n  .d & { color: blue; }\n}\n.menu { li + li { margin-left: 5px; } }\n");
}

#[test]
fn parity_interpolation() {
    assert_parity("$name: warning;\n$i: 3;\n.icon-#{$name} { content: \"#{$name}-#{$i}\"; }\n.col-#{$i} { width: 10px * $i; }\n");
}

#[test]
fn parity_lists_and_functions() {
    assert_parity("$stack: \"Helvetica Neue\", Arial, sans-serif;\n.t {\n  font-family: $stack;\n  margin: 1px 2px 3px 4px;\n  transform: translateX(10px);\n  width: calc(100% - 20px);\n}\n");
}

#[test]
fn parity_if_else() {
    assert_parity("$t: dark;\n.a {\n  @if $t == dark { color: white; background: black; }\n  @else if $t == light { color: black; }\n  @else { color: gray; }\n  padding: 1px;\n}\n@if 2 > 1 { .b { x: 1; } } @else { .c { y: 2; } }\n");
}

#[test]
fn parity_loops() {
    assert_parity("@for $i from 1 through 3 {\n  .col-#{$i} { width: $i * 10px; }\n}\n@each $a, $b in (x 1), (y 2) {\n  .pair-#{$a} { order: $b; }\n}\n.counter {\n  $i: 0;\n  @while $i < 3 { p-#{$i}: $i * 2px; $i: $i + 1; }\n}\n");
}

#[test]
fn parity_functions_and_mixins() {
    assert_parity("@function clamp-val($v, $min: 0, $max: 100) {\n  @if $v < $min { @return $min; }\n  @else if $v > $max { @return $max; }\n  @return $v;\n}\n@function sum($nums...) {\n  $t: 0;\n  @each $n in $nums { $t: $t + $n; }\n  @return $t;\n}\n@mixin box($pad, $color: blue) { padding: $pad; color: $color; }\n@mixin surround { border: 1px solid; @content; margin: 0; }\n.a {\n  z-index: clamp-val(150);\n  order: sum(1, 2, 3, 4);\n  @include box(4px);\n}\n.b { @include surround { background: yellow; } }\n");
}

#[test]
fn parity_modulo_sign() {
    // Sass modulo is a floored modulo whose result takes the divisor's sign.
    assert_parity(
        "a {\n  b: 1.2 % -4.7;\n  c: -1.2 % 4.7;\n  d: 5 % 3;\n  e: 10px % 3px;\n  f: -8 % 3;\n}\n",
    );
}

#[test]
fn parity_large_numbers() {
    // Huge literals print as plain decimals, scientific notation expands,
    // and fractions round to ten places exactly like dart-sass.
    assert_parity(concat!(
        "a {\n",
        "  big: 99999999999999999999999999999;\n",
        "  bigdec: 1234567890123456789;\n",
        "  neg: -123456789012345;\n",
        "  sci: 1e20;\n",
        "  sci2: 1.5e3;\n",
        "  sci3: 1e-3;\n",
        "  unit: 1e3px;\n",
        "  third: (1 / 3);\n",
        "  precise: 0.1 + 0.2;\n",
        "}\n",
    ));
}

#[test]
fn parity_calc() {
    // Fully numeric calc() interiors simplify and unwrap; mixed ones keep a
    // canonical calc() with folded numeric subtrees and minimal parens.
    assert_parity(concat!(
        "a {\n",
        "  c1: calc(1px + 2px);\n",
        "  c2: calc(100% / 4);\n",
        "  c3: calc(2 * 3);\n",
        "  c4: calc(50px);\n",
        "  c5: calc((1 + 2) * 3px);\n",
        "  k1: calc(100% - 50px);\n",
        "  k2: calc(var(--a) + 1px);\n",
        "  k3: calc(1px + 1em);\n",
        "  k4: calc(1px + 2px + 1%);\n",
        "  k5: calc(3px * 2 + 1%);\n",
        "  k6: calc(1% + -1px);\n",
        "  k7: calc(1px + (2% * var(--c)));\n",
        "  k8: calc(1px - (2% + var(--c)));\n",
        "  k9: calc(1px + (var(--c)));\n",
        "  pi: calc(pi);\n",
        "  e: calc(e);\n",
        "  pi2: calc(pi * 2);\n",
        "  pimix: calc(pi * (1% + 1px));\n",
        "}\n",
    ));
}

#[test]
fn parity_slash_division() {
    // The deprecated `/` keeps a slash spelling between number literals but
    // performs real division once an operand is computed, parenthesized,
    // read from a variable, or passed through a Sass function.
    assert_parity(concat!(
        "$x: 8px;\n",
        "@function id($v) {@return $v}\n",
        "a {\n",
        "  keep: 16px/1.5;\n",
        "  chain: 1/2/3;\n",
        "  list: 1 2/3 4;\n",
        "  comma: 1, 2/3, 4;\n",
        "  same-unit: 10px/5px;\n",
        "  paren: (10px / 2);\n",
        "  computed: (1 + 1) / 2;\n",
        "  var: $x / 2;\n",
        "  func: inspect(10px / 2);\n",
        "  unknown: foo(1/2);\n",
        "}\n",
    ));
}
