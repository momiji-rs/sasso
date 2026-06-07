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
fn parity_at_rule_font_face() {
    assert_parity("@font-face {\n  font-family: \"My Font\";\n  src: url(font.woff);\n}\n");
}

#[test]
fn parity_at_rule_page_and_unknown() {
    assert_parity(
        "@page {\n  margin: 1cm;\n}\n@foo bar baz {\n  a: b;\n}\n@blockless;\n@with-prelude value;\n",
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

#[test]
fn parity_at_rule_bubbling() {
    assert_parity(
        ".a {\n  color: black;\n  @media-like screen {\n    color: red;\n  }\n  .b { color: green; }\n}\n",
    );
}

#[test]
fn parity_at_rule_nested_and_mixed() {
    assert_parity("@outer one {\n  @inner two {\n    .a { color: red; }\n  }\n}\n@foo {\n  a: 1;\n  b: 2;\n  .x { c: 3; }\n  d: 4;\n}\n");
}

#[test]
fn parity_warn_debug_emit_no_css() {
    // @warn/@debug write to stderr only; the emitted CSS must be identical
    // to the same stylesheet without them.
    assert_parity("@warn \"a heads up\";\n.a {\n  @debug 1 + 2;\n  color: red;\n}\n");
}

#[test]
fn at_error_aborts_compilation() {
    // @error must abort with an Error (not emit CSS). This runs offline.
    let res = compile("@error \"boom\";\n.a { color: red; }\n", &Options::default());
    assert!(res.is_err(), "@error should abort compilation");
}

#[test]
fn parity_supports() {
    assert_parity("@supports (display: grid) {\n  .a { display: grid; }\n}\n@supports (display: grid) and (gap: 1rem) {\n  .b { gap: 1rem; }\n}\n@supports not (display: grid) {\n  .c { float: left; }\n}\n@supports (a: b) or (c: d) {\n  .d { x: y; }\n}\n");
}

#[test]
fn parity_supports_nested_and_blockless() {
    assert_parity(
        "@supports (a: b) and ((c: d) or (e: f)) {\n  .g { h: i; }\n}\n@supports (x: y) {@inner foo}\n",
    );
}

#[test]
fn parity_at_root_inline() {
    assert_parity(
        ".a {\n  color: red;\n  @at-root .b {\n    color: green;\n  }\n}\n@at-root .c {\n  color: blue;\n}\n",
    );
}

#[test]
fn parity_at_root_block() {
    assert_parity(".a {\n  .b {\n    @at-root .c {\n      x: y;\n    }\n  }\n}\n.outer {\n  @at-root {\n    .single { z: 1; }\n  }\n}\n");
}

#[test]
fn parity_keyframes() {
    assert_parity("@keyframes slide {\n  from { left: 0; }\n  50% { left: 50px; }\n  to { left: 100px; }\n}\n@-webkit-keyframes spin {\n  from { transform: rotate(0); }\n  to { transform: rotate(360deg); }\n}\n");
}

#[test]
fn parity_keyframes_list_and_interpolation() {
    assert_parity("$name: bounce;\n@keyframes #{$name} {\n  0%, 100% { opacity: 0; }\n  50% { opacity: 1; }\n}\n.a {\n  @keyframes nested-#{1 + 1} { from { top: 0; } }\n}\n");
}

#[test]
fn parity_unit_converting_arithmetic() {
    assert_parity(
        ".a {\n  w: 1in + 1cm;\n  x: 1cm + 1in;\n  y: 5s - 100ms;\n  z: 10px % 3pt;\n  cmp: 1in > 2cm;\n  mix: 5 + 1px;\n  turn: 1turn + 90deg;\n}\n",
    );
}

#[test]
fn parity_color_hue_family() {
    assert_parity(
        "a {\n  b: adjust-hue(red, 540);\n  c: adjust-hue(blue, 0);\n  d: adjust-hue(red, -180);\n  e: adjust-hue(red, 60rad);\n  f: complement(aqua);\n  g: invert(red);\n  h: invert(#b37399, 80%);\n}\n",
    );
}

#[test]
fn parity_calc_unit_folding() {
    assert_parity(
        ".a {\n  a: calc(1in + 1cm);\n  b: calc(1cm + 1in);\n  c: calc(5s - 100ms);\n  d: calc(1px + 1pt);\n  e: calc(10px / 2cm);\n  f: calc(1turn + 90deg);\n  g: calc(100% - 10px);\n  h: calc(1px + 1vw);\n}\n",
    );
}

#[test]
fn parity_math_builtins() {
    assert_parity(
        ".a {\n  a: sign(-5px);\n  b: pow(2, 3);\n  c: sqrt(4);\n  d: log(8, 2);\n  e: hypot(3px, 4cm);\n  f: sin(30deg);\n  g: cos(0);\n  h: tan(45deg);\n  i: asin(0.5);\n  j: atan2(1, 1);\n  k: rem(10px, 3pt);\n  l: mod(-10, 3);\n}\n",
    );
}

#[test]
fn parity_min_max_clamp_routing() {
    assert_parity(
        ".a {\n  a: min(1px, 2px);\n  b: max(1, 2, 3);\n  c: clamp(1px, 5px, 3px);\n  d: min(1px + 1px, 2vw);\n  e: min(1in, 2cm);\n  f: min(50%, 30%);\n  g: clamp(1px, 2vw, 3px);\n  h: max(2px, min(1px, 2vw));\n  i: min(1px, var(--x));\n}\n",
    );
}

#[test]
fn parity_color_filter_overloads() {
    assert_parity(
        "a {\n  b: invert(10%);\n  c: grayscale(15%);\n  d: saturate(50%);\n  e: grayscale(var(--c));\n  f: invert(calc(1 + 2));\n}\n",
    );
}

#[test]
fn parity_color_legacy_named_and_alpha() {
    assert_parity(
        "a {\n  b: lighten(red, 100%);\n  c: darken(red, 100%);\n  d: lighten(red, 14%);\n  e: mix(red, red);\n  f: mix(red, white, 50%);\n  g: rgba(red, 1);\n  h: rgba(#102030, 1);\n  i: rgba(red, 0.5);\n}\n",
    );
}

#[test]
fn parity_media_query_grammar() {
    // Logic operators, modifiers, ranges, nested parens, interpolation and
    // SassScript inside feature values; an empty body produces no output.
    assert_parity(concat!(
        "$w: width;\n",
        "@media (a) and (b) { x { y: z; } }\n",
        "@media (a)or (b) { x { y: z; } }\n",
        "@media not a { x { y: z; } }\n",
        "@media not (a) { x { y: z; } }\n",
        "@media only screen and (color) { x { y: z; } }\n",
        "@media a AnD nOt (b) { x { y: z; } }\n",
        "@media (not (a)) { x { y: z; } }\n",
        "@media ((a) and (b)) { x { y: z; } }\n",
        "@media (min-width: 100px + 50px) { x { y: z; } }\n",
        "@media ($w < 600px) { x { y: z; } }\n",
        "@media (50px + 50px < width < 600px) { x { y: z; } }\n",
        "@media (a) and #{\"(b) and (c)\"} { x { y: z; } }\n",
        "@media screen { }\n",
        "@media screen, print { x { y: z; } }\n",
    ));
}

#[test]
fn media_rejects_malformed_queries() {
    // dart-sass rejects these; sasso must error rather than pass them through.
    for src in [
        "@media a or (b) { x { y: z; } }\n",
        "@media (a) and (b) or (c) { x { y: z; } }\n",
        "@media a and { x { y: z; } }\n",
        "@media not { x { y: z; } }\n",
        "@media not(a) { x { y: z; } }\n",
        "@media (a) and(b) { x { y: z; } }\n",
        "@media (1 < width < 2 < 3) { x { y: z; } }\n",
        "@media (1px > width < 2px) { x { y: z; } }\n",
        "@media (width < = 100px) { x { y: z; } }\n",
    ] {
        let res = compile(src, &Options::default());
        assert!(res.is_err(), "expected error for malformed media: {src}");
    }
}

#[test]
fn media_rejects_bare_declarations_at_root() {
    // A bare declaration directly in a media block without an enclosing style
    // rule is an error in dart-sass; with a style rule it is allowed.
    assert!(compile("@media screen { color: red; }\n", &Options::default()).is_err());
    assert!(compile("@media a { @media b { color: red; } }\n", &Options::default()).is_err());
    assert_parity(".x {\n  @media screen {\n    color: red;\n  }\n}\n");
}
