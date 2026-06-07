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

/// Compile `scss` and return our CSS (panicking on error), for direct
/// expected-output assertions that do not need a live dart-sass.
fn ours(scss: &str) -> String {
    compile(scss, &Options::default()).expect("our compile failed")
}

#[test]
fn rgb_hsl_special_value_passthrough() {
    // A special channel argument (var/env/calc) preserves the call,
    // comma-joined, when there are three components.
    assert_eq!(
        ours("a {b: rgb(var(--foo), 2, 3)}\n"),
        "a {\n  b: rgb(var(--foo), 2, 3);\n}\n"
    );
    assert_eq!(
        ours("a {b: rgb(var(--foo) 2 3)}\n"),
        "a {\n  b: rgb(var(--foo), 2, 3);\n}\n"
    );
    assert_eq!(
        ours("a {b: rgb(calc(1px + 1%), 2, 3, 0.4)}\n"),
        "a {\n  b: rgb(calc(1px + 1%), 2, 3, 0.4);\n}\n"
    );
    assert_eq!(
        ours("a {b: hsl(var(--x) 50% 50%)}\n"),
        "a {\n  b: hsl(var(--x), 50%, 50%);\n}\n"
    );
    // A concrete color plus a special alpha decomposes to channels.
    assert_eq!(
        ours("a {b: rgb(blue, var(--foo))}\n"),
        "a {\n  b: rgb(0, 0, 255, var(--foo));\n}\n"
    );
    // Wrong component count is preserved verbatim (space-joined).
    assert_eq!(
        ours("a {b: rgb(var(--foo) 2)}\n"),
        "a {\n  b: rgb(var(--foo) 2);\n}\n"
    );
    assert_eq!(
        ours("a {b: rgb(var(--foo))}\n"),
        "a {\n  b: rgb(var(--foo));\n}\n"
    );
    assert_eq!(
        ours("a {b: rgb(var(--foo), 0.4)}\n"),
        "a {\n  b: rgb(var(--foo), 0.4);\n}\n"
    );
}

#[test]
fn rgb_hsl_plain_number_channels_keep_function_spelling() {
    // The one-argument channels list computes a color but keeps the rgb()/hsl()
    // spelling (it never collapses to hex).
    assert_eq!(ours("a {b: rgb(18 52 86)}\n"), "a {\n  b: rgb(18, 52, 86);\n}\n");
    assert_eq!(
        ours("a {b: rgb(1 2 3 / 0.5)}\n"),
        "a {\n  b: rgba(1, 2, 3, 0.5);\n}\n"
    );
    assert_eq!(
        ours("a {b: rgb(190 173 237 / 1)}\n"),
        "a {\n  b: rgb(190, 173, 237);\n}\n"
    );
    assert_eq!(
        ours("a {b: hsl(120 50% 50% / 0.4)}\n"),
        "a {\n  b: hsla(120, 50%, 50%, 0.4);\n}\n"
    );
    // Two-argument (color, alpha) form still collapses to a computed color.
    assert_eq!(ours("a {b: rgb(#123, 1)}\n"), "a {\n  b: #112233;\n}\n");
    assert_eq!(
        ours("a {b: rgb(#123, 0.5)}\n"),
        "a {\n  b: rgba(17, 34, 51, 0.5);\n}\n"
    );
    // hsl saturation floors at 0, lightness is left unclamped, hue normalizes.
    assert_eq!(
        ours("a {b: hsl(0, 500%, 50%)}\n"),
        "a {\n  b: hsl(0, 500%, 50%);\n}\n"
    );
    assert_eq!(
        ours("a {b: hsl(0, -100%, 50%)}\n"),
        "a {\n  b: hsl(0, 0%, 50%);\n}\n"
    );
    assert_eq!(
        ours("a {b: hsl(0, 100%, -100%)}\n"),
        "a {\n  b: hsl(0, 100%, -100%);\n}\n"
    );
    assert_eq!(
        ours("a {b: hsl(360, 50%, 50%)}\n"),
        "a {\n  b: hsl(0, 50%, 50%);\n}\n"
    );
}

#[test]
fn hwb_global_conversion_and_passthrough() {
    // Plain-number channels convert HWB -> HSL spelling.
    assert_eq!(
        ours("a {b: hwb(180 30% 40%)}\n"),
        "a {\n  b: hsl(180, 33.3333333333%, 45%);\n}\n"
    );
    assert_eq!(
        ours("a {b: hwb(180 30% 40% / 0.5)}\n"),
        "a {\n  b: hsla(180, 33.3333333333%, 45%, 0.5);\n}\n"
    );
    assert_eq!(
        ours("a {b: hwb(180 30% 40% / 1)}\n"),
        "a {\n  b: hsl(180, 33.3333333333%, 45%);\n}\n"
    );
    // Special and `none` channels preserve the call verbatim, space-joined,
    // with a bare numeric hue suffixed `deg`.
    assert_eq!(
        ours("a {b: hwb(var(--c) 30% 40%)}\n"),
        "a {\n  b: hwb(var(--c) 30% 40%);\n}\n"
    );
    assert_eq!(
        ours("a {b: hwb(none 30% 40%)}\n"),
        "a {\n  b: hwb(none 30% 40%);\n}\n"
    );
    assert_eq!(
        ours("a {b: hwb(0 none 40%)}\n"),
        "a {\n  b: hwb(0deg none 40%);\n}\n"
    );
    assert_eq!(
        ours("a {b: hwb(0 30% none)}\n"),
        "a {\n  b: hwb(0deg 30% none);\n}\n"
    );
}

#[test]
fn lab_family_validation_and_passthrough() {
    // Well-formed, fully numeric and special-value calls are preserved verbatim.
    assert_eq!(ours("a {b: lab(1% 2 3)}\n"), "a {\n  b: lab(1% 2 3);\n}\n");
    assert_eq!(
        ours("a {b: lab(var(--foo) 2 3)}\n"),
        "a {\n  b: lab(var(--foo) 2 3);\n}\n"
    );
    assert_eq!(
        ours("a {b: lab(var(--foo) 2)}\n"),
        "a {\n  b: lab(var(--foo) 2);\n}\n"
    );
    assert_eq!(
        ours("a {b: lab(from var(--c) l a b)}\n"),
        "a {\n  b: lab(from var(--c) l a b);\n}\n"
    );
    assert_eq!(ours("a {b: lch(1% 2 3deg)}\n"), "a {\n  b: lch(1% 2 3deg);\n}\n");
    // Malformed calls raise validation errors.
    for src in [
        "a {b: lab(1% 2)}\n",
        "a {b: lab(1% 2 3 0.4)}\n",
        "a {b: lab(c 2 3)}\n",
        "a {b: lab(1px 2 3)}\n",
        "a {b: lab(1% 2 3/0.4px)}\n",
        "a {b: lab()}\n",
        "a {b: lab(1%, 2, 3, 0.4)}\n",
        "a {b: lab((1%, 2, 3))}\n",
        "a {b: lch(1% 2 3px)}\n",
        "a {b: lch(1% 2 3%)}\n",
    ] {
        assert!(
            compile(src, &Options::default()).is_err(),
            "expected error for {src}"
        );
    }
}

#[test]
fn color_function_validation_and_passthrough() {
    // Well-formed and special/relative calls are preserved verbatim.
    assert_eq!(
        ours("a {b: color(srgb 0.1 0.2 0.3)}\n"),
        "a {\n  b: color(srgb 0.1 0.2 0.3);\n}\n"
    );
    assert_eq!(
        ours("a {b: color(srgb calc(infinity) 0 0)}\n"),
        "a {\n  b: color(srgb calc(infinity) 0 0);\n}\n"
    );
    assert_eq!(
        ours("a {b: color(from var(--c) srgb r g b)}\n"),
        "a {\n  b: color(from var(--c) srgb r g b);\n}\n"
    );
    // Malformed calls raise validation errors.
    for src in [
        "a {b: color()}\n",
        "a {b: color(srgb)}\n",
        "a {b: color(1 2 3)}\n",
        "a {b: color(foo 1 2 3)}\n",
        "a {b: color(srgb 0.1 0.2)}\n",
        "a {b: color(srgb 0.1 0.2 0.3 0.4)}\n",
        "a {b: color(srgb c 0.2 0.3)}\n",
        "a {b: color(srgb 0.1px 0.2 0.3)}\n",
        "a {b: color((srgb, 0.1, 0.2, 0.3))}\n",
        "a {b: color(srgb (0.1 0.2 0.3))}\n",
    ] {
        assert!(
            compile(src, &Options::default()).is_err(),
            "expected error for {src}"
        );
    }
}

#[test]
fn rgb_degenerate_calc_constants_fold() {
    // calc(infinity)/calc(-infinity)/calc(NaN) fold to floating-point channel
    // and alpha values for the legacy rgb function (clamped, NaN -> bound).
    assert_eq!(
        ours("a {b: rgb(calc(infinity), 0, 0, 0.5)}\n"),
        "a {\n  b: rgba(255, 0, 0, 0.5);\n}\n"
    );
    assert_eq!(
        ours("a {b: rgb(calc(-infinity), 0, 0, 0.5)}\n"),
        "a {\n  b: rgba(0, 0, 0, 0.5);\n}\n"
    );
    assert_eq!(
        ours("a {b: rgb(calc(NaN), 0, 0, 0.5)}\n"),
        "a {\n  b: rgba(0, 0, 0, 0.5);\n}\n"
    );
    assert_eq!(
        ours("a {b: rgb(0, 0, 0, calc(infinity))}\n"),
        "a {\n  b: rgb(0, 0, 0);\n}\n"
    );
    assert_eq!(
        ours("a {b: rgb(0, 0, 0, calc(-infinity))}\n"),
        "a {\n  b: rgba(0, 0, 0, 0);\n}\n"
    );
    assert_eq!(
        ours("a {b: rgb(0, 0, 0, calc(NaN))}\n"),
        "a {\n  b: rgba(0, 0, 0, 0);\n}\n"
    );
    // A non-degenerate calc is still a special value preserved verbatim, and a
    // degenerate calc in a modern color function (color()) is preserved too.
    assert_eq!(
        ours("a {b: rgb(calc(1px + 1%), 2, 3, 0.4)}\n"),
        "a {\n  b: rgb(calc(1px + 1%), 2, 3, 0.4);\n}\n"
    );
    assert_eq!(
        ours("a {b: color(srgb calc(infinity) 0 0)}\n"),
        "a {\n  b: color(srgb calc(infinity) 0 0);\n}\n"
    );
}

#[test]
fn modern_if_function() {
    // The modern CSS `if()` conditional: `css()` conditions emit the whole
    // call verbatim; `sass()`/bare conditions are evaluated (first truthy
    // clause wins, else otherwise); the legacy `if($c, $t, $f)` builtin still
    // works. Each case byte-matches dart-sass.
    for src in [
        // verbatim css() forms (single, else, raw chars, not, and/or, parens)
        "a {b: if(css(): c)}\n",
        "a {b: if(css(): c; else: d)}\n",
        "a {b: if(css(!@#$%^&*(){}[]_-+=|:;''\"\"<>,./?): c)}\n",
        "a {b: if(not css(): c)}\n",
        "a {b: if(not (css()): c)}\n",
        "a {b: if(css(1) and css(2): c)}\n",
        "a {b: if(css(1) and (css(2)): c)}\n",
        "a {b: if(css(1) or css(2) or css(3): c)}\n",
        "a {b: if((css()): c)}\n",
        "a {b: if((css(1) and css(2)): c)}\n",
        // arbitrary substitutions (var/attr/nested if/interpolation), verbatim
        "a {b: if(var(--not) css(): c)}\n",
        "a {b: if(css(1) var(--and) css(2): c)}\n",
        "a {b: if(css() if(else: var(--and-clause)): c)}\n",
        "a {b: if(css(1) #{\"and\"} css(2): c)}\n",
        "a {b: if(#{css}(): c)}\n",
        "a {b: if(css(#{1 + 1}): c)}\n",
        // evaluated sass()/bare conditions
        "a {b: if(sass(true): c; else: d)}\n",
        "a {b: if(sass(false): c; else: d)}\n",
        "$a: true;\nb {c: if(sass($a): d; else: e)}\n",
        "a {b: if(not sass(true): c; else: d)}\n",
        "a {b: if(sass(true) and sass(false): c; else: d)}\n",
        "a {b: if(sass(false) or sass(true): c; else: d)}\n",
        // sass folded into a verbatim css() chain
        "a {b: if(sass(true) and css(): c; else: d)}\n",
        "a {b: if(sass(false) or css(): c; else: d)}\n",
        "a {b: if(css(1) and sass(true) and css(2): c; else: d)}\n",
        "a {b: if(sass(true) and (var(--not) css()): c)}\n",
        // short-circuit, else-only, evaluated values
        "a {b: if(sass(true): c; sass($undefined): d)}\n",
        "a {b: if(else: c)}\n",
        "a {b: if(css(): 1 + 2)}\n",
        "a {b: if(css(): (1 2 3))}\n",
        // legacy builtin stays intact
        "a {b: if(true, 1px, 2px)}\n",
        "a {b: if(false, 1px, 2px)}\n",
    ] {
        assert_parity(src);
    }
}

#[test]
fn modern_if_rejects_invalid_conditions() {
    // dart-sass rejects these; sasso must error too (mixing and/or, `not`
    // after a conjunction, reserved keyword as a function, and a `sass()`
    // sharing a boolean level with an unparenthesised arbitrary substitution).
    for src in [
        "a {b: if(css(1) and css(2) or css(3): c)}\n",
        "a {b: if(css(1) and not css(2): c)}\n",
        "a {b: if(not not css(): c)}\n",
        "a {b: if(not and(): d)}\n",
        "a {b: if(css(1) and(css(2)): d)}\n",
        "a {b: if(sass(true) and css(1) var(--and) css(2): c)}\n",
        "a {b: if((sass(true)) and css(1) var(--and) css(2): c)}\n",
        "a {b: if(css(1): c, css(2): d)}\n",
    ] {
        assert!(
            compile(src, &Options::default()).is_err(),
            "expected error for invalid modern if(): {src}"
        );
    }
}

#[test]
fn parity_plain_css_import_passthrough() {
    // A `url(...)` URL, a `.css`/protocol URL, or any URL with trailing
    // media-query / `supports()` modifiers is a plain CSS `@import`, emitted
    // verbatim in source order rather than inlined.
    assert_parity("@import url(\"a.css\") print;\n");
    assert_parity("@import url(whatever);\n");
    assert_parity("@import \"a.css\";\n");
    assert_parity("@import \"http://foo.com/bar\";\n");
    assert_parity("@import \"a\" b;\n");
    assert_parity("@import \"a.css\" supports(calc(1));\n");
    assert_parity("@import \"a.css\" supports(--a: );\n");
    assert_parity("@import \"a\" b, (c: d) and (e: f), g;\n");
    // A `supports()`/function modifier ends the argument at a top-level comma,
    // so the following URL starts a fresh `@import`; a bare media type does not.
    assert_parity("@import \"a\" supports(b: c), \"d.css\";\n");
    assert_parity("@import \"b\" c(d), \"e.css\";\n");
    // Comments around the URL and modifiers are stripped.
    assert_parity("@import \"a.css\" /**/ b;\n");
    assert_parity("@import \"a.css\" b /**/;\n");
    // Interpolation inside a CSS-import modifier is resolved.
    assert_parity("@import \"b\" c#{\"a\"}d;\n");
}

/// A trivial in-memory [`Importer`] for offline `@import` inlining tests.
struct MapImporter(std::collections::HashMap<String, String>);

impl sasso::Importer for MapImporter {
    fn resolve(&self, path: &str) -> Option<String> {
        self.0.get(path).cloned()
    }
}

#[test]
fn import_inlines_sass_partials() {
    let mut files = std::collections::HashMap::new();
    files.insert("p".to_string(), "x { y: z }".to_string());
    files.insert(
        "nested".to_string(),
        "b { color: red; nested { x: y } }".to_string(),
    );
    let imp = MapImporter(files);
    let opts = Options::default().with_importer(&imp);

    // A bare quoted string with no modifiers is inlined at the top level.
    let css = compile("@import \"p\";\n", &opts).expect("import compile failed");
    assert_eq!(css, "x {\n  y: z;\n}\n");

    // A nested `@import` runs the imported statements under the current parent
    // selector, so the imported rules nest beneath it.
    let css = compile("a {\n  @import \"nested\";\n}\n", &opts).expect("nested import failed");
    assert_eq!(css, "a b {\n  color: red;\n}\na b nested {\n  x: y;\n}\n");
}

#[test]
fn import_reimports_and_detects_cycles() {
    let mut files = std::collections::HashMap::new();
    files.insert("p".to_string(), "x { y: z }".to_string());
    files.insert("alpha".to_string(), "@import \"beta\";".to_string());
    files.insert("beta".to_string(), "@import \"alpha\";".to_string());
    let imp = MapImporter(files);
    let opts = Options::default().with_importer(&imp);

    // Re-importing an already-finished file emits its content again (`@import`
    // re-evaluates), rather than being silently deduplicated.
    let css = compile("@import \"p\", \"p\";\n", &opts).expect("import compile failed");
    assert_eq!(css, "x {\n  y: z;\n}\n\nx {\n  y: z;\n}\n");

    // A load cycle is an error rather than a silent skip or an infinite loop.
    assert!(compile("@import \"alpha\";\n", &opts).is_err());
}

#[test]
fn bracketed_list_literals() {
    // `[ ... ]` produces a bracketed list that serializes wrapped in square
    // brackets, preserving the interior separator and nesting (parenthesized
    // unbracketed lists flatten; nested bracketed lists stay nested).
    assert_parity("x { b: []; }\n");
    assert_parity("x { b: [c]; }\n");
    assert_parity("x { b: [c d]; }\n");
    assert_parity("x { b: [a, b]; }\n");
    assert_parity("x { b: [[]]; }\n");
    assert_parity("x { b: [[c]]; }\n");
    assert_parity("x { b: [[c] [d]]; }\n");
    assert_parity("x { b: [()]; }\n");
    assert_parity("x { b: [(c,)]; }\n");
    assert_parity("x { b: [(c,) (d e)]; }\n");
}

#[test]
fn comments_in_value_position() {
    // Loud `/* */` and silent `//` comments act as whitespace between value
    // tokens, and as operator separators (`1 /**/+/**/ 2`).
    assert_parity("a {\n  b: c // d\n}\n");
    assert_parity("a {\n  b: c /* d */ e;\n}\n");
    assert_parity("a {\n  c: 1 /**/+/**/ 2;\n}\n");
    assert_parity("a {\n  c: 1/**/+/**/2;\n}\n");
    assert_parity("a {\n  c: 1 +/**/ 2;\n}\n");
    assert_parity("a {\n  c: a /**/ b;\n}\n");
}

#[test]
fn calc_and_math_infinity_nan() {
    // Non-finite calc results serialize as `calc(infinity)` / `calc(NaN)` /
    // `calc(-infinity)` (with `* 1unit` when they carry a unit), and the math
    // functions accept the bare `infinity`/`-infinity`/`NaN`/`pi`/`e`
    // constants and re-wrap non-finite results in a calculation.
    assert_parity("a { b: calc(1/0); }\n");
    assert_parity("a { b: calc(10px / 0); }\n");
    assert_parity("a { b: calc(0/0); }\n");
    assert_parity("a { b: calc(-1/0); }\n");
    assert_parity("a { b: atan(infinity); }\n");
    assert_parity("a { b: atan(-infinity); }\n");
    assert_parity("a { b: sin(infinity); }\n");
    assert_parity("a { b: abs(infinity); }\n");
    assert_parity("a { b: sign(infinity); }\n");
    assert_parity("a { b: exp(-infinity); }\n");
    assert_parity("a { b: pow(infinity, 2); }\n");
    assert_parity("a { b: min(infinity, 5); }\n");
    assert_parity("a { b: max(5, infinity); }\n");
    assert_parity("a { b: min(NaN, 5); }\n");
    assert_parity("a { b: clamp(1, infinity, 10); }\n");
    assert_parity("a { b: cos(pi); }\n");
}

#[test]
fn round_strategies_and_steps() {
    // round() as a CSS calculation: strategy keyword + step, the two-argument
    // nearest-with-step form, unit coercion, a zero step (NaN), and the
    // non-finite cases — all byte-matched to dart-sass.
    assert_parity("a { b: round(nearest, 117px, 25px); }\n");
    assert_parity("a { b: round(up, 101px, 25px); }\n");
    assert_parity("a { b: round(down, 122px, 25px); }\n");
    assert_parity("a { b: round(to-zero, 120px, 25px); }\n");
    assert_parity("a { b: round(to-zero, -120px, -25px); }\n");
    assert_parity("a { b: round(up, 12px, -7px); }\n");
    assert_parity("a { b: round(117, 25); }\n");
    assert_parity("a { b: round(117cm, 25mm); }\n");
    assert_parity("a { b: round(4.6); }\n");
    assert_parity("a { b: round(nearest, 10px, 0px); }\n");
    assert_parity("a { b: round(nearest, infinity, 5); }\n");
    assert_parity("a { b: round(nearest, -infinity, 5); }\n");
    assert_parity("a { b: round(nearest, infinity, infinity); }\n");
    assert_parity("a { b: round(1px, 10%); }\n");
    assert_parity("a { b: round(1%, 2%); }\n");
    assert_parity("a { b: round(1foo, 2bar); }\n");
}

#[test]
fn trailing_commas_in_params_and_args() {
    // Trailing commas are allowed after ordinary params, defaulted params, the
    // rest param, and call arguments.
    assert_parity("@function a($b, ) { @return $b; }\nc { d: a(e, ); }\n");
    assert_parity("@function a($b: 1, ) { @return $b; }\nc { d: a(); }\n");
    assert_parity("@mixin m($b, $c..., ) { d: $b; e: $c; }\nf { @include m(1, 2, 3); }\n");
}

#[test]
fn splat_argument_expansion() {
    // A list splat spreads into positional args (with explicit positionals
    // bound first), and a map splat spreads into keyword args.
    assert_parity("a { b: rgb([1, 2]..., 3); }\n");
    assert_parity("a { b: rgb([1, 2]..., $blue: 3); }\n");
    assert_parity("@function id($a, $b, $c) { @return $a $b $c; }\nx { y: id(1, [2, 3]...); }\n");
    assert_parity("@function f($a, $b) { @return $a $b; }\nx { y: f((a: 1, b: 2)...); }\n");
}

#[test]
fn map_literals_and_builtins() {
    // Map literals serialize via inspect(); the global map functions and
    // @each over a map all byte-match dart-sass.
    assert_parity("a { b: inspect((c: 1, d: 2)); }\n");
    assert_parity("a { b: inspect((c: (d: 1), \"e\": f g)); }\n");
    assert_parity("a { b: map-get((c: d), c); }\n");
    assert_parity("a { b: map-keys((c: 1, d: 2)); }\n");
    assert_parity("a { b: map-values((c: 1, d: 2)); }\n");
    assert_parity("a { b: map-has-key((c: d), c); }\n");
    assert_parity("@each $k, $v in (a: 1, b: 2) { x-#{$k} { y: $v; } }\n");
}

#[test]
fn special_css_functions_verbatim() {
    // calc/element/expression (with/without vendor prefix) and unprefixed
    // type() preserve their arguments verbatim: vendor-prefixed and uppercase
    // names lower-case, `%`/`@`/`=`/punctuation and IE-hack syntax pass
    // through, loud comments survive, silent comments drop, whitespace
    // collapses, and `#{}` resolves — all byte-matched to dart-sass.
    assert_parity("a { b: -a-calc(/**/ c); }\n");
    assert_parity("a { b: -a-calc(c /**/); }\n");
    assert_parity("a {\n  b: -a-calc(//\n    c);\n}\n");
    assert_parity("a {\n  b: -a-calc(c //\n    );\n}\n");
    assert_parity("a { b: element(c d); }\n");
    assert_parity("a { b: expression(a=b); }\n");
    assert_parity("a { b: expression(opacity=80); }\n");
    assert_parity("a { b: TYPE(0); }\n");
    assert_parity("a { b: type(@#$%^&*({[]})_-+=); }\n");
    assert_parity("a { b: -A-CALC(0); }\n");
    assert_parity("a { b: -C-ELEMENT(0); }\n");
    assert_parity("a { b: -C-EXPRESSION(#{1 + 1}); }\n");
    assert_parity("a { b: -a-calc(  c   d  ); }\n");
}

#[test]
fn css_custom_function_mixin_passthrough() {
    // A `@function`/`@mixin` whose name begins with `--`, or any non-lowercase
    // spelling of the keyword, is a plain CSS custom function/mixin: emitted
    // verbatim. Top-level declaration values stay literal (`$b`, `1 + 1`,
    // arbitrary characters) with whitespace collapsed; interpolated properties
    // evaluate as SassScript; `#{}` resolves. Byte-matched to dart-sass.
    assert_parity("@function --a(--b <color>) {result: c}\n");
    assert_parity("@function --a() returns <ident> {result: b}\n");
    assert_parity("@function --#{a}() {result: b}\n");
    assert_parity("@function --a() {\n  result: $b;\n}\n");
    assert_parity("@function --a() {\n  result: 1 + 1;\n}\n");
    assert_parity("@function --a() {\n  result: #{1 + 1};\n}\n");
    assert_parity("@function --a() {\n  result: {}#&%^*;\n}\n");
    assert_parity("@function --a() {\n  RESULT: {b: c};\n}\n");
    assert_parity("@function --a() {\n  #{result}: 1 + 1;\n}\n");
    assert_parity("@FUNCTION --a() {\n  result: $b;\n}\n");
    assert_parity("@FUNCTION foo() {\n  result: $b;\n}\n");
    assert_parity("@MIXIN foo {}\n");
    assert_parity("@MIXIN --a {}\n");
    // A non-custom lowercase `@function`/`@mixin` is still a Sass definition.
    assert_parity("@function foo() { @return 1px * 2; }\na { b: foo(); }\n");
    assert_parity("@mixin foo { x: y; }\na { @include foo; }\n");
}

#[test]
fn special_url_function_passthrough() {
    // url() is recognised case-insensitively and with an optional vendor
    // prefix. A plain unquoted URL is emitted as a bare lower-cased `url(...)`
    // (the vendor prefix is dropped), tolerating `!` and other url-safe
    // characters and resolving `#{}` (including inside quoted strings). When
    // the contents are SassScript (a `$variable`) the call falls back to a
    // normal function so its arguments evaluate, keeping the original name.
    assert_parity("a { b: url(!); }\n");
    assert_parity("a { b: URL(!); }\n");
    assert_parity("a { b: URL(http://c.d/e!f); }\n");
    assert_parity("a { b: -c-url(0); }\n");
    assert_parity("a { b: -c-url(http://d.e/f!g); }\n");
    assert_parity("a { b: -c-url(#{0}); }\n");
    assert_parity("a { b: url(c, d); }\n");
    assert_parity("$a: b;\nc { d: url($a); }\n");
    assert_parity("$a: b;\nc { d: -e-url($a); }\n");
    assert_parity("$f: bar;\na {\n  foo: url($f);\n  foo: url(#{$f});\n  foo: url(\"x?v=#{$f}\");\n}\n");
}

#[test]
fn extended_named_colors() {
    // Every one of the 148 CSS named colors must resolve and feed color
    // functions; previously extended names like `plum` errored as "not a
    // color". Each rule round-trips through rgba() so the exact channel
    // values are byte-matched to dart-sass.
    assert_parity("a { b: rgba(plum, 0.5); }\n");
    assert_parity("a { b: rgba(rebeccapurple, 0.5); }\n");
    assert_parity("a { b: rgba(darkslategray, 0.5); }\n");
    assert_parity("a { b: desaturate(plum, 14%); }\n");
    assert_parity("a { b: rgba(cornflowerblue, 0.25); }\n");
    assert_parity("a { b: rgba(mediumspringgreen, 0.75); }\n");
    assert_parity("a { b: rgba(lightgoldenrodyellow, 1); }\n");
}

#[test]
fn legacy_color_argument_forms() {
    // The single-argument `$channels` list form, the `rgb($color, $alpha)`
    // two-argument form (positional and named), and the `none`-channel
    // verbatim spelling (a bare hue gains `deg`) — all byte-matched.
    assert_parity("a { b: hsl($channels: 0 100% 50%); }\n");
    assert_parity("a { b: rgb($channels: 1 2 3); }\n");
    assert_parity("a { b: rgb($color: #123, $alpha: 0.5); }\n");
    assert_parity("a { b: rgb($alpha: 0.5, $color: blue); }\n");
    assert_parity("a { b: rgb(red, 0.5); }\n");
    assert_parity("a { b: hsl(0 none 50%); }\n");
    assert_parity("a { b: hsl(0 100% none); }\n");
    assert_parity("a { b: hsl(none 100% 50%); }\n");
}

#[test]
fn hsl_degenerate_calc_channels() {
    // A degenerate calc() channel keeps the hsl() spelling, coercing each
    // channel like dart-sass: hue reduces mod 360 to calc(NaN); saturation
    // and lightness gain `* 1%`, with saturation clamping non-positive/NaN
    // to 0%. Byte-matched to dart-sass.
    assert_parity("a { b: hsl(calc(infinity), 100%, 50%); }\n");
    assert_parity("a { b: hsl(calc(-infinity), 100%, 50%); }\n");
    assert_parity("a { b: hsl(calc(NaN), 100%, 50%); }\n");
    assert_parity("a { b: hsl(0, calc(infinity), 50%); }\n");
    assert_parity("a { b: hsl(0, calc(-infinity), 50%); }\n");
    assert_parity("a { b: hsl(0, calc(NaN), 50%); }\n");
    assert_parity("a { b: hsl(0, 100%, calc(infinity)); }\n");
    assert_parity("a { b: hsl(0, 100%, calc(-infinity)); }\n");
    assert_parity("a { b: hsl(0, 100%, calc(NaN)); }\n");
}

#[test]
fn mix_srgb_method_matches_legacy() {
    // The `srgb`/`rgb` interpolation methods reproduce the legacy mix this
    // build computes, and must byte-match dart-sass (other spaces require
    // full color-space interpolation and are validated elsewhere).
    assert_parity("a { b: mix(red, blue, $method: srgb); }\n");
    assert_parity("a { b: mix(red, blue, $method: rgb); }\n");
    assert_parity("a { b: mix(red, blue, 25%, $method: srgb); }\n");
}

#[test]
fn relative_color_from_is_preserved() {
    // A relative-color `rgb(from … )`/`hsl(from … )` call is kept verbatim
    // rather than computed or rejected by the channel-count check.
    assert_parity("a { b: rgb(from red r g b); }\n");
    assert_parity("a { b: hsl(from red h s l); }\n");
    assert_parity("a { b: rgb(from var(--c) r g b); }\n");
}

#[test]
fn slash_with_special_value_forms_slash() {
    // dart-sass: `/` between non-number operands (a calc()/var()/unquoted
    // string/list, or a number divided by a non-number) does not divide — it
    // forms a slash-separated unquoted string `left/right`. A `calc()` that
    // folds to a number keeps the slash spelling too. A color on the *left*
    // of `/` is the one case that still errors ("Undefined operation").
    assert_parity("a { b: calc(1)/2; }\n");
    assert_parity("a { b: 1/calc(2); }\n");
    assert_parity("a { b: calc(1)/calc(2); }\n");
    assert_parity("a { b: calc(2px)/calc(4px); }\n");
    assert_parity("a { b: calc(1px + 1%)/2; }\n");
    assert_parity("a { b: 2/calc(1px + 1%); }\n");
    assert_parity("a { b: calc(1px + 1%)/calc(2px + 2%); }\n");
    assert_parity("a { b: foo / 2; }\n");
    assert_parity("a { b: var(--x) / 2; }\n");
    assert_parity("a { b: 2 / var(--x); }\n");
    assert_parity("a { b: (1 2) / 3; }\n");
    assert_parity("a { b: 2 / red; }\n");
}

#[test]
fn calc_infinity_nan_constants() {
    // `infinity`/`-infinity`/`nan` are calc() numeric constants (like
    // `pi`/`e`), resolved case-insensitively. They fold through arithmetic
    // (`infinity * 2` -> `infinity`) and canonicalize their spelling; a
    // unit-carrying non-finite renders (and parenthesizes) as `infinity * 1px`.
    assert_parity("a { b: calc(infinity * 2); }\n");
    assert_parity("a { b: calc(-infinity * 2); }\n");
    assert_parity("a { b: calc(NAN * 2); }\n");
    assert_parity("a { b: calc(InFiNiTy); }\n");
    assert_parity("a { b: calc(nan); }\n");
    assert_parity("a { b: calc(infinity * (1% + 1px)); }\n");
    assert_parity("a { b: calc((1/0) * (1% + 1px)); }\n");
    assert_parity("a { b: calc(infinity * 1px); }\n");
    assert_parity("a { b: calc(2 * infinity * 1px); }\n");
    assert_parity("a { b: calc(var(--c) / (infinity * 1px)); }\n");
    assert_parity("a { b: calc(var(--c) - (infinity * 1px)); }\n");
    // The degenerate-constant color channels still resolve (the calc value
    // keeps the spelling the color builtins inspect).
    assert_parity("a { b: rgb(calc(infinity), 0, 0, 0.5); }\n");
    assert_parity("a { b: rgb(calc(NaN), 0, 0, 0.5); }\n");
}

#[test]
fn calc_wrapping_complete_calculation_flattens() {
    // `calc()` wrapping a single already-complete calculation drops the
    // redundant outer `calc()` (dart-sass): `calc(min(1%, 2px))` -> `min(…)`.
    // A real operation inside keeps the wrapper, and a non-calculation leaf
    // (`var()`, unknown function) keeps its `calc()`.
    assert_parity("a { b: calc(min(1%, 2px)); }\n");
    assert_parity("a { b: calc(max(1%, 2px)); }\n");
    assert_parity("a { b: calc(clamp(1%, 2px, 3%)); }\n");
    assert_parity("a { b: calc(round(1%, 2px)); }\n");
    assert_parity("a { b: calc(calc-size(1%, 2px)); }\n");
    assert_parity("a { b: calc(min(1%, 2px) + 1px); }\n");
    assert_parity("a { b: calc(var(--x)); }\n");
    assert_parity("a { b: calc(unknownfn(1%, 2px)); }\n");
}

#[test]
fn calc_relative_length_cross_dimension_errors() {
    // A relative length (`em`, `ch`, `vw`, …) is a known *length*, so mixing
    // it with another dimension in calc() `+`/`-` is incompatible (dart-sass
    // errors), even though it is not convertible to an absolute length.
    for src in [
        "a {b: calc(1ch + 1deg)}\n",
        "a {b: calc(1em + 1s)}\n",
        "a {b: calc(1vw + 1hz)}\n",
        "a {b: calc(1rem + 1dpi)}\n",
        "a {b: calc(1vmax - 1khz)}\n",
        "a {b: calc(1ex + 1grad)}\n",
    ] {
        assert!(
            compile(src, &Options::default()).is_err(),
            "expected error for {src}"
        );
    }
    // Two lengths (even when one is relative and not convertible) are
    // compatible and preserved; `%`, `fr`, and unknown units never error.
    assert_parity("a { b: calc(1px + 1vw); }\n");
    assert_parity("a { b: calc(1em + 1px); }\n");
    assert_parity("a { b: calc(1ch + 1em); }\n");
    assert_parity("a { b: calc(1fr + 1px); }\n");
    assert_parity("a { b: calc(1% + 1deg); }\n");
    assert_parity("a { b: calc(1foo + 1deg); }\n");
}

#[test]
fn calc_value_plus_strictness() {
    // A calculation may only be `+`-concatenated with a string; against any
    // other operand (number, color, bool, list, another calculation)
    // dart-sass raises "Undefined operation".
    for src in [
        "a {b: calc(var(--c)) + 1}\n",
        "a {b: 1 + calc(var(--c))}\n",
        "a {b: calc(var(--c)) + calc(var(--d))}\n",
        "a {b: calc(var(--c)) + true}\n",
        "a {b: calc(var(--c)) + red}\n",
        "a {b: red + calc(var(--c))}\n",
    ] {
        assert!(
            compile(src, &Options::default()).is_err(),
            "expected error for {src}"
        );
    }
    // Concatenation with a string is allowed; a calc on the left inherits the
    // right string's quotedness.
    assert_parity("a { b: calc(var(--c)) + foo; }\n");
    assert_parity("a { b: foo + calc(var(--c)); }\n");
    assert_parity("a { b: calc(var(--c)) + \"x\"; }\n");
    assert_parity("a { b: \"x\" + calc(var(--c)); }\n");
}

#[test]
fn calc_operand_value_strictness() {
    // A value resolved into a calc() that is not a number, calculation, or
    // unquoted special string — a null, bool, color, list, map, or quoted
    // string (typically via a `$variable`) — is rejected.
    for src in [
        "$a: null;\nb {c: calc($a)}\n",
        "$a: true;\nb {c: calc($a)}\n",
        "$a: blue;\nb {c: calc($a)}\n",
        "$a: 1 2 3;\nb {c: calc($a)}\n",
        "$a: (1, 2);\nb {c: calc($a)}\n",
        "$a: (b: c);\nb {c: calc($a)}\n",
        "$a: \"foo\";\nb {c: calc($a)}\n",
    ] {
        assert!(
            compile(src, &Options::default()).is_err(),
            "expected error for {src}"
        );
    }
    // A number, var(), interpolation, or plain ident operand is fine.
    assert_parity("a { b: calc(foo); }\n");
    assert_parity("a { b: calc(var(--x)); }\n");
    assert_parity("a { b: calc(#{foo}); }\n");
}

#[test]
fn calc_space_list_grammar() {
    // A space-separated run inside calc() is only legal when it carries a
    // var()/env() substitution or interpolation (spliced verbatim); a run of
    // ordinary operands has no operator between adjacent terms ("Missing math
    // operator.").
    assert_parity("a { b: calc(var(--c) 1); }\n");
    assert_parity("a { b: calc(1 var(--c)); }\n");
    assert_parity("a { b: calc(1 var(--c) 2); }\n");
    assert_parity("a { b: calc(#{\"1 +\"} 2); }\n");
    assert_parity("a { b: calc(1 #{\"+ 2\"}); }\n");
    assert_parity("a { b: calc(1 #{\"+ 2 +\"} 3); }\n");
    for src in [
        "a {b: calc(1 2)}\n",
        "a {b: calc(c 1 2)}\n",
        "a {b: calc(1 2 c)}\n",
        "a {b: calc(1 (3))}\n",
        "a {b: calc(1 calc(1px + 1%))}\n",
        "$c: 1;\n$d: 2;\na {b: calc($c $d)}\n",
    ] {
        assert!(
            compile(src, &Options::default()).is_err(),
            "expected error for {src}"
        );
    }
}

#[test]
fn calc_rejects_non_arithmetic_operators() {
    // Only `+`/`-`/`*`/`/` are valid in a calculation; modulo, comparisons,
    // and `and`/`or` are rejected ("This operation can't be used in a
    // calculation.").
    for src in [
        "a {b: calc(1px % 2px)}\n",
        "a {b: calc(1 > 2)}\n",
        "a {b: calc(1 == 2)}\n",
        "a {b: calc(1 and 2)}\n",
    ] {
        assert!(
            compile(src, &Options::default()).is_err(),
            "expected error for {src}"
        );
    }
}

#[test]
fn calc_name_is_case_insensitive() {
    // `calc` is recognized case-insensitively and its interior simplified;
    // a vendor-prefixed form stays a verbatim special function.
    assert_parity("a { b: CaLc(1px); }\n");
    assert_parity("a { b: Calc(2); }\n");
    assert_parity("a { b: CALC(1px + 1%); }\n");
    assert_parity("a { b: -webkit-calc(1px + 1%); }\n");
}

#[test]
fn slash_chain_keeps_spelling_through_special_value() {
    // A slash-division operand keeps its chained spelling when the other side
    // of a `/` is a special value: `1 / 2 / foo()` -> `1/2/foo()`, not the
    // collapsed quotient `0.5/foo()`.
    assert_parity("a { b: 1 / 2 / foo(); }\n");
    assert_parity("a { b: 1/2/foo(); }\n");
}

#[test]
fn progid_long_filter_syntax_is_preserved() {
    // IE `progid:Name.Name(...)` long-filter syntax (with `:`, `.`, `=`, and
    // `#hex` inside the arg list) is preserved verbatim; the `progid` keyword
    // and any vendor prefix are lower-cased while the `.Name` chain keeps its
    // case. Interpolation resolves; a backslash escapes the next character so
    // an escaped `\(`/`\)` does not affect parenthesis nesting.
    assert_parity(
        "foo { filter: progid:DXImageTransform.Microsoft.gradient(GradientType=1, startColorstr=#c0ff3300, endColorstr=#ff000000); }\n",
    );
    assert_parity("a { b: -C-PROGID:D(#{0}); }\n");
    assert_parity("a { b: progid:c(/**/ d); }\n");
    assert_parity("a { b: progid:foo.bar(x=1), progid:baz.qux(y=2); }\n");
    assert_parity("a { b: progid:c(opacity=20\\)); }\n");
}

#[test]
fn lone_percent_is_a_value_token() {
    // A `%` with no left operand is a standalone unquoted-string value (not the
    // modulo operator), so the IE/CSS `attr(c, %)` placeholder round-trips and
    // a bare `%` survives in any argument position. A whitespace-surrounded `%`
    // remains the modulo operator.
    assert_parity("a { b: %; }\n");
    assert_parity("a { b: attr(c, %); }\n");
    assert_parity("a { b: rgb(attr(c, %), 2, 3); }\n");
    assert_parity("a { b: rgb(1, 2, attr(c, %)); }\n");
    assert_parity("a { b: foo(1, %, 2); }\n");
    assert_parity("a { b: 7 % 3; }\n");
}

#[test]
fn quoted_string_line_continuation_is_removed() {
    // A backslash immediately followed by a CSS newline inside a quoted string
    // is a line continuation: both the backslash and the newline are dropped,
    // joining the two physical lines (leading whitespace on the next line is
    // preserved). Byte-matched to dart-sass.
    assert_parity("a { b: \"line1 \\\n      line2\"; }\n");
    assert_parity("a { b: \"x\\\ny\"; }\n");
    assert_parity("a { b: 'a\\\nb\\\nc'; }\n");
}

#[test]
fn parent_selector_as_value() {
    // `&` in value position resolves to the current selector: a single
    // selector, a comma list, and a nested/descendant selector. At the
    // document root `&` is `null` (interpolates to empty). A content block
    // passed to a mixin without `@content` is an error.
    assert_eq!(ours("a {\n  b: &;\n}\n"), "a {\n  b: a;\n}\n");
    assert_eq!(ours(".x, .y {\n  c: &;\n}\n"), ".x, .y {\n  c: .x, .y;\n}\n");
    assert_eq!(
        ours(".foo {\n  .bar {\n    d: &;\n  }\n}\n"),
        ".foo .bar {\n  d: .foo .bar;\n}\n"
    );
    // `&` is always a comma list, so `nth(&, 1)` is the first complex
    // selector and a descendant selector reports two space-separated items.
    assert_eq!(ours(".x, .y {\n  c: nth(&, 1);\n}\n"), ".x, .y {\n  c: .x;\n}\n");
    assert_eq!(
        ours(".a .b {\n  c: length(nth(&, 1));\n}\n"),
        ".a .b {\n  c: 2;\n}\n"
    );
    // Interpolation of the root `&` (null) yields the empty string.
    assert_eq!(ours("a {\n  c: \"#{&}!\";\n}\n"), "a {\n  c: \"a!\";\n}\n");
    // A selector that resolves to nothing (`#{&}` at the root) is rejected.
    assert!(compile("#{&} {\n  foo {\n    bar: baz;\n  }\n}\n", &Options::default()).is_err());
    // A content block for a mixin that never uses `@content` is an error.
    assert!(compile(
        "@mixin m() { x: 1; }\na { @include m { y: 2; } }\n",
        &Options::default()
    )
    .is_err());
}

#[test]
fn parent_selector_placement_strictness() {
    // `&` must begin a compound selector and a top-level `&` may not carry an
    // identifier suffix — matching dart-sass's parser rules. These run offline.
    // Non-initial `&` is always an error (parent or not).
    assert!(compile("p {\n  b& {c: d}\n}\n", &Options::default()).is_err());
    assert!(compile("p {\n  [b]& {c: d}\n}\n", &Options::default()).is_err());
    assert!(compile("p {\n  .x& {c: d}\n}\n", &Options::default()).is_err());
    assert!(compile(":not(a > b)& {c: d}\n", &Options::default()).is_err());
    // A top-level `&` with an identifier suffix is an error.
    assert!(compile("&a {b: c}\n", &Options::default()).is_err());
    assert!(compile("&-x {b: c}\n", &Options::default()).is_err());
    assert!(compile("@at-rule {\n  &b {c: d}\n}\n", &Options::default()).is_err());
    // But a suffix under a real parent is allowed (it concatenates).
    assert_eq!(ours(".x {\n  &a {c: d}\n}\n"), ".xa {\n  c: d;\n}\n");
    // And these valid placements still compile (each `&` begins a compound).
    assert!(compile("p {\n  &.foo {c: d}\n}\n", &Options::default()).is_ok());
    assert!(compile("p {\n  &:hover {c: d}\n}\n", &Options::default()).is_ok());
    assert!(compile("p {\n  & > & {c: d}\n}\n", &Options::default()).is_ok());
    assert!(compile("p {\n  &[a~=b] {c: d}\n}\n", &Options::default()).is_ok());
}

#[test]
fn placeholder_selector_must_be_named() {
    // A bare `%` (or `%` not followed by an identifier name-start char) is
    // "Expected identifier." in dart-sass. Keyframe percentage selectors
    // (`10%`, `1e2%`) are not placeholders and must still compile.
    assert!(compile("% {\n  a: b;\n}\n", &Options::default()).is_err());
    assert!(compile("%.bar {\n  a: b;\n}\n", &Options::default()).is_err());
    assert!(compile(".a % {\n  c: d;\n}\n", &Options::default()).is_err());
    assert_eq!(
        ours("@keyframes a {\n  10% {\n    c: d;\n  }\n}\n"),
        "@keyframes a {\n  10% {\n    c: d;\n  }\n}\n"
    );
    assert_eq!(
        ours("@keyframes a {\n  from, 15%, to {\n    c: d;\n  }\n}\n"),
        "@keyframes a {\n  from, 15%, to {\n    c: d;\n  }\n}\n"
    );
}

#[test]
fn attribute_selector_modifier_strictness() {
    // An attribute modifier must be a single ASCII letter directly before the
    // closing `]`. Invalid forms (no operator, too long, non-letter, trailing
    // space) error with `expected "]"`; valid forms still compile. Offline.
    assert!(compile("[a b] {c: d}\n", &Options::default()).is_err());
    assert!(compile("[a=b cd] {c: d}\n", &Options::default()).is_err());
    assert!(compile("[a=b _] {c: d}\n", &Options::default()).is_err());
    assert!(compile("[a=b 1] {c: d}\n", &Options::default()).is_err());
    assert!(compile("[a=b i ] {c: d}\n", &Options::default()).is_err());
    assert!(compile("[charset i] {c: d}\n", &Options::default()).is_err());
    // Valid attribute selectors compile, including single-letter modifiers,
    // a modifier glued to a quoted value, namespaces, and `]` inside a value.
    assert!(compile("[a] {c: d}\n", &Options::default()).is_ok());
    assert!(compile("[a=b i] {c: d}\n", &Options::default()).is_ok());
    assert!(compile("[a=b I] {c: d}\n", &Options::default()).is_ok());
    assert!(compile("[a=b c] {c: d}\n", &Options::default()).is_ok());
    assert!(compile("[a=\"b\"i] {c: d}\n", &Options::default()).is_ok());
    assert!(compile("[*|a=b i] {c: d}\n", &Options::default()).is_ok());
    assert!(compile("[a=\"]\"] {c: d}\n", &Options::default()).is_ok());
}

#[test]
fn attribute_selector_emit_normalization() {
    // Expanded-mode attribute selectors serialize canonically: whitespace
    // around the operator and at the edges is removed, a quoted value that is
    // a plain CSS identifier is unquoted, and a trailing modifier is preceded
    // by a single space — byte-matched to dart-sass.
    assert_eq!(ours("a[\n  b]\n  {c: d}\n"), "a[b] {\n  c: d;\n}\n");
    assert_eq!(ours("a[b=\n  c]\n  {d: e}\n"), "a[b=c] {\n  d: e;\n}\n");
    assert_eq!(ours("a[b\n  =c]\n  {d: e}\n"), "a[b=c] {\n  d: e;\n}\n");
    assert_eq!(ours("[a=\"b\"i] {c: d}\n"), "[a=b i] {\n  c: d;\n}\n");
    assert_eq!(ours("[a=\"b\"] {c: d}\n"), "[a=b] {\n  c: d;\n}\n");
    // Non-identifier values stay quoted; `]` inside a value is preserved.
    assert_eq!(ours("[a=\"b c\"] {d: e}\n"), "[a=\"b c\"] {\n  d: e;\n}\n");
    assert_eq!(ours("[a=\"--b\"] {d: e}\n"), "[a=\"--b\"] {\n  d: e;\n}\n");
    assert_eq!(ours("[a=\"]\"] {d: e}\n"), "[a=\"]\"] {\n  d: e;\n}\n");
}

#[test]
fn math_unit_rules_and_arity() {
    // atan2/hypot preserve their call verbatim when an operand is a `%`
    // (context-dependent) or an unknown/relative unit can't be combined; an
    // all-compatible call still folds. Byte-matched to dart-sass. Offline.
    assert_eq!(ours("a {b: atan2(1%, 2%)}\n"), "a {\n  b: atan2(1%, 2%);\n}\n");
    assert_eq!(
        ours("a {b: atan2(1px, 10%)}\n"),
        "a {\n  b: atan2(1px, 10%);\n}\n"
    );
    assert_eq!(
        ours("a {b: atan2(1foo, 2bar)}\n"),
        "a {\n  b: atan2(1foo, 2bar);\n}\n"
    );
    assert_eq!(
        ours("a {b: atan2(1foo, 2foo)}\n"),
        "a {\n  b: 26.5650511771deg;\n}\n"
    );
    assert_eq!(ours("a {b: hypot(1%, 2%)}\n"), "a {\n  b: hypot(1%, 2%);\n}\n");
    assert_eq!(
        ours("a {b: hypot(1foo, 2foo)}\n"),
        "a {\n  b: 2.2360679775foo;\n}\n"
    );
    // mod/rem fold equal unknown units but preserve a real+unknown mix.
    assert_eq!(ours("a {b: mod(1%, 2%)}\n"), "a {\n  b: 1%;\n}\n");
    assert_eq!(ours("a {b: mod(5px, 3%)}\n"), "a {\n  b: mod(5px, 3%);\n}\n");
    // Calc-style math names fold case-insensitively.
    assert_eq!(ours("a {b: SiN(1deg)}\n"), "a {\n  b: 0.0174524064;\n}\n");
    assert_eq!(ours("a {b: AbS(-2)}\n"), "a {\n  b: 2;\n}\n");
    // A known cross-dimension or unitless/real mix is an error.
    assert!(compile("a {b: atan2(1deg, 1px)}\n", &Options::default()).is_err());
    assert!(compile("a {b: atan2(1, 1px)}\n", &Options::default()).is_err());
    assert!(compile("a {b: mod(16px, 5deg)}\n", &Options::default()).is_err());
    // Too many arguments to a fixed-arity function is an error.
    assert!(compile("a {b: sin(0, 0)}\n", &Options::default()).is_err());
    assert!(compile("a {b: abs(1, 2)}\n", &Options::default()).is_err());
    assert!(compile("a {b: pow(1, 2, 3)}\n", &Options::default()).is_err());
}

#[test]
fn math_random_in_range() {
    // random() is a unitless float in [0, 1); random($limit) is an integer in
    // [1, $limit]. The draw is nondeterministic, so assert range membership
    // rather than an exact value. Offline.
    for _ in 0..200 {
        let css = ours("a {b: random()}\n");
        let v: f64 = css
            .trim()
            .trim_start_matches("a {")
            .trim()
            .trim_start_matches("b:")
            .trim()
            .trim_end_matches('}')
            .trim()
            .trim_end_matches(';')
            .trim()
            .parse()
            .expect("random() should emit a bare number");
        assert!((0.0..1.0).contains(&v), "random() out of range: {v}");

        let css = ours("a {b: random(5)}\n");
        let v: f64 = css
            .trim()
            .trim_start_matches("a {")
            .trim()
            .trim_start_matches("b:")
            .trim()
            .trim_end_matches('}')
            .trim()
            .trim_end_matches(';')
            .trim()
            .parse()
            .expect("random(5) should emit a bare integer");
        assert!((1.0..=5.0).contains(&v), "random(5) out of range: {v}");
        assert_eq!(v, v.round(), "random(5) must be an integer: {v}");
    }
    // A non-positive or non-integer limit errors.
    assert!(compile("a {b: random(0)}\n", &Options::default()).is_err());
    assert!(compile("a {b: random(-1)}\n", &Options::default()).is_err());
    assert!(compile("a {b: random(1.5)}\n", &Options::default()).is_err());
}

#[test]
fn math_min_max_clamp_unit_rules() {
    // min/max fold compatible/convertible units to the winning argument's own
    // unit, preserve mutually-incomparable clusters, and error on a known
    // cross-dimension pair. Byte-matched to dart-sass. Offline.
    assert_eq!(ours("a {b: min(1px, 1in, 1cm)}\n"), "a {\n  b: 1px;\n}\n");
    assert_eq!(ours("a {b: max(1px, 1in, 1cm)}\n"), "a {\n  b: 1in;\n}\n");
    assert_eq!(ours("a {b: min(3d, 2, 1e)}\n"), "a {\n  b: 1e;\n}\n");
    assert_eq!(ours("a {b: min(1px, 2vw)}\n"), "a {\n  b: min(1px, 2vw);\n}\n");
    assert_eq!(ours("a {b: min(1c, 2d)}\n"), "a {\n  b: min(1c, 2d);\n}\n");
    assert_eq!(ours("a {b: min(1%, 2%)}\n"), "a {\n  b: 1%;\n}\n");
    assert!(compile("a {b: min(1s, 2px)}\n", &Options::default()).is_err());
    assert!(compile("a {b: max(1px, 2px, 3s)}\n", &Options::default()).is_err());
    // clamp checks `min` first (so `clamp(3, 5, 1)` is `1`), keeps the winning
    // argument's unit, errors on a known cross-dimension pair, and preserves a
    // lone non-number argument.
    assert_eq!(ours("a {b: clamp(1px, 1in, 1cm)}\n"), "a {\n  b: 1cm;\n}\n");
    assert_eq!(ours("a {b: clamp(3, 5, 1)}\n"), "a {\n  b: 1;\n}\n");
    assert_eq!(ours("a {b: clamp(5, 1, 3)}\n"), "a {\n  b: 5;\n}\n");
    assert_eq!(
        ours("a {b: clamp(1px, 2vw, 3px)}\n"),
        "a {\n  b: clamp(1px, 2vw, 3px);\n}\n"
    );
    assert_eq!(
        ours("a {b: clamp(var(--c))}\n"),
        "a {\n  b: clamp(var(--c));\n}\n"
    );
    assert!(compile("a {b: clamp(1s, 2px, 3px)}\n", &Options::default()).is_err());
    assert!(compile("a {b: clamp(1px)}\n", &Options::default()).is_err());
}

#[test]
fn math_round_strategy_preservation() {
    // An explicit three-argument round() preserves its strategy keyword when
    // the units keep it from simplifying (round(nearest, 1px, 10%) keeps
    // `nearest`), while the implicit two-argument form does not. A strategy
    // with an unsimplifiable value but no step preserves rather than erroring.
    // Byte-matched to dart-sass. Offline.
    assert_eq!(
        ours("a {b: round(nearest, 1px, 10%)}\n"),
        "a {\n  b: round(nearest, 1px, 10%);\n}\n"
    );
    assert_eq!(
        ours("a {b: round(1px, 10%)}\n"),
        "a {\n  b: round(1px, 10%);\n}\n"
    );
    assert_eq!(
        ours("a {b: round(up, 1px, 10%)}\n"),
        "a {\n  b: round(up, 1px, 10%);\n}\n"
    );
    assert_eq!(
        ours("a {c: round(up, var(--c))}\n"),
        "a {\n  c: round(up, var(--c));\n}\n"
    );
    // A strategy with a real number but no step is still an error.
    assert!(compile("a {b: round(nearest, 5)}\n", &Options::default()).is_err());
    assert!(compile("a {b: round(up, 5)}\n", &Options::default()).is_err());
}

#[test]
fn selector_comment_stripping() {
    // dart-sass treats a loud `/* */` or silent `//` comment inside a selector
    // as whitespace: it is dropped and a separator is left, so the selector
    // normaliser collapses it. Byte-matched to dart-sass. Offline.
    assert_eq!(ours("a /**/ {b: c}\n"), "a {\n  b: c;\n}\n");
    assert_eq!(ours("a /**/ b {x: y}\n"), "a b {\n  x: y;\n}\n");
    assert_eq!(ours("a/**/b {x: y}\n"), "a b {\n  x: y;\n}\n");
    assert_eq!(ours("a /***/ b {x: y}\n"), "a b {\n  x: y;\n}\n");
    assert_eq!(ours("a //\n  {b: c}\n"), "a {\n  b: c;\n}\n");
    // A loud comment that is a standalone statement is still emitted.
    assert_eq!(
        ours("a {\n  /* keep */\n  b: c;\n}\n"),
        "a {\n  /* keep */\n  b: c;\n}\n"
    );
}

#[test]
fn declaration_property_comment_stripping() {
    // A loud or silent comment between a declaration's property name and the
    // `:` is dropped (the property template strips it as whitespace, and the
    // emitter trims). Byte-matched to dart-sass. Offline.
    assert_eq!(ours("a {b /**/ : c}\n"), "a {\n  b: c;\n}\n");
    assert_eq!(ours("a {b //\n  : c}\n"), "a {\n  b: c;\n}\n");
    assert_eq!(ours("a { color : red ; }\n"), "a {\n  color: red;\n}\n");
}

#[test]
fn at_rule_prelude_comment_stripping() {
    // `@supports` and `@-moz-document` use structured grammars: top-level
    // trivia comments are dropped, but comments inside parentheses are kept.
    assert_eq!(
        ours("@supports (a: b) /**/ {c {d: e}}\n"),
        "@supports (a: b) {\n  c {\n    d: e;\n  }\n}\n"
    );
    assert_eq!(
        ours("@supports (a: b) //\n  {c {d: e}}\n"),
        "@supports (a: b) {\n  c {\n    d: e;\n  }\n}\n"
    );
    assert_eq!(
        ours("@supports (a /**/ b) {c {d: e}}\n"),
        "@supports (a /**/ b) {\n  c {\n    d: e;\n  }\n}\n"
    );
    // Unknown directives keep a loud comment verbatim but drop a silent one.
    assert_eq!(ours("@a b /**/\n"), "@a b /**/;\n");
    assert_eq!(ours("@a b //\n"), "@a b;\n");
    assert_eq!(ours("@a /**/ b\n"), "@a b;\n");
    assert_eq!(ours("@a b /**/ {}\n"), "@a b /**/ {}\n");
    assert_eq!(ours("@a b //\n  {}\n"), "@a b {}\n");
}

#[test]
fn legacy_channels_special_slash_alpha() {
    // When the trailing `channel / alpha` slash crosses a special value
    // (var/calc/attr) or a `none`, it evaluates to an unquoted `X/Y` string
    // rather than a numeric slash. The one-argument channels form must still
    // peel off the alpha and emit dart-sass's normalized spelling. Byte-matched
    // to `npx sass`. Offline.
    // Three plain-or-special channels with a special channel or alpha → the
    // legacy comma form (the alpha becomes the fourth comma item).
    assert_eq!(
        ours("a{x: rgb(1 2 var(--f) / 0.4)}\n"),
        "a {\n  x: rgb(1, 2, var(--f), 0.4);\n}\n"
    );
    assert_eq!(
        ours("a{x: rgb(1 2 3 / var(--f))}\n"),
        "a {\n  x: rgb(1, 2, 3, var(--f));\n}\n"
    );
    assert_eq!(
        ours("a{x: hsl(1 2% 3%/var(--a))}\n"),
        "a {\n  x: hsl(1, 2%, 3%, var(--a));\n}\n"
    );
    // A wrong channel count with a special value keeps the original spelling
    // verbatim (the `/` alpha separator stays glued).
    assert_eq!(
        ours("a{x: rgb(var(--f) 2 / 0.4)}\n"),
        "a {\n  x: rgb(var(--f) 2/0.4);\n}\n"
    );
    assert_eq!(
        ours("a{x: rgb(var(--f) / 0.4)}\n"),
        "a {\n  x: rgb(var(--f)/0.4);\n}\n"
    );
    // A `none` keyword (no special function) keeps the space/slash spelling;
    // hsl gives a bare-number hue an explicit `deg`.
    assert_eq!(
        ours("a{x: rgb(0 255 127 / none)}\n"),
        "a {\n  x: rgb(0 255 127 / none);\n}\n"
    );
    assert_eq!(
        ours("a{x: rgb(0 none 127 / 0.5)}\n"),
        "a {\n  x: rgb(0 none 127 / 0.5);\n}\n"
    );
    assert_eq!(
        ours("a{x: hsl(180 none 50% / 0.5)}\n"),
        "a {\n  x: hsl(180deg none 50% / 0.5);\n}\n"
    );
    // hwb: a special function keeps the verbatim glued spelling; a `none`
    // keeps the spaced `deg`/slash spelling.
    assert_eq!(
        ours("a{x: hwb(0 30% 40% / none)}\n"),
        "a {\n  x: hwb(0deg 30% 40% / none);\n}\n"
    );
    assert_eq!(
        ours("a{x: hwb(0 30% 40% / var(--a))}\n"),
        "a {\n  x: hwb(0 30% 40%/var(--a));\n}\n"
    );
}

#[test]
fn color_function_degenerate_calc() {
    // A degenerate `calc()` (`NaN`/`infinity`/`-infinity`) in a `color()` call
    // is folded the way dart-sass folds it, and the result serializes in the
    // modern (space-around-`/`) form. A degenerate channel is preserved; a
    // degenerate alpha folds to a number (`infinity` = opaque/omitted,
    // `-infinity`/`NaN` = 0). Byte-matched to `npx sass`. Offline.
    assert_eq!(
        ours("a{x: color(srgb 0 0 calc(infinity) / 0.5)}\n"),
        "a {\n  x: color(srgb 0 0 calc(infinity) / 0.5);\n}\n"
    );
    assert_eq!(
        ours("a{x: color(srgb 0 0 calc(NaN) / 0.5)}\n"),
        "a {\n  x: color(srgb 0 0 calc(NaN) / 0.5);\n}\n"
    );
    assert_eq!(
        ours("a{x: color(srgb 0 0 0 / calc(infinity))}\n"),
        "a {\n  x: color(srgb 0 0 0);\n}\n"
    );
    assert_eq!(
        ours("a{x: color(srgb 0 0 0 / calc(-infinity))}\n"),
        "a {\n  x: color(srgb 0 0 0 / 0);\n}\n"
    );
    assert_eq!(
        ours("a{x: color(srgb 0 0 0 / calc(NaN))}\n"),
        "a {\n  x: color(srgb 0 0 0 / 0);\n}\n"
    );
    // A non-degenerate special channel/alpha keeps the original glued spelling.
    assert_eq!(
        ours("a{x: color(srgb 0 0 var(--x) / 0.5)}\n"),
        "a {\n  x: color(srgb 0 0 var(--x)/0.5);\n}\n"
    );
    // A degenerate channel with no alpha is unchanged.
    assert_eq!(
        ours("a{x: color(srgb calc(infinity) 0 0)}\n"),
        "a {\n  x: color(srgb calc(infinity) 0 0);\n}\n"
    );
}

#[test]
fn legacy_channels_non_number_channel_error() {
    // A one-argument channels list whose first channel is a non-`from`,
    // non-number value (a quoted `"from"` or a bare keyword like `c`) reports
    // a per-channel error before the channel-count check, matching dart-sass.
    // The channel name is per-space (`red`/`hue`) for the first three and
    // `channel <N>` beyond. Offline.
    let err = |scss: &str| {
        compile(scss, &Options::default())
            .err()
            .map(|e| e.to_string())
            .unwrap_or_default()
    };
    assert!(err("a{b: rgb(\"from\" #aaa r g b)}\n")
        .contains("$channels: Expected red channel to be a number, was \"from\"."));
    assert!(
        err("a{b: rgb(c #aaa r g b)}\n").contains("$channels: Expected red channel to be a number, was c.")
    );
    assert!(
        err("a{b: hsl(c #aaa h s l)}\n").contains("$channels: Expected hue channel to be a number, was c.")
    );
    assert!(
        err("a{b: hwb(c #aaa h w b)}\n").contains("$channels: Expected hue channel to be a number, was c.")
    );
    assert!(err("a{b: rgb(1 c d)}\n").contains("$channels: Expected green channel to be a number, was c."));
    assert!(err("a{b: rgb(1 2 3 c d)}\n").contains("$channels: Expected channel 4 to be a number, was c."));
    // A `from`-relative call (even with a var() base and a slash alpha) is kept
    // verbatim, not reported as an error.
    assert_eq!(
        ours("a{x: rgb(from var(--c) r g b / 25%)}\n"),
        "a {\n  x: rgb(from var(--c) r g b/25%);\n}\n"
    );
}

#[test]
fn non_finite_number_serializes_as_calc() {
    // A bare non-finite number value serializes like dart-sass: a unitless
    // infinity/-infinity/NaN prints as `calc(infinity)`/`calc(-infinity)`/
    // `calc(NaN)`, and a unit-bearing one as `calc(infinity * 1px)`.
    // `1e400` overflows the f64 literal to +Infinity, `-1e400` to -Infinity.
    assert_eq!(ours("a {b: 1e400}\n"), "a {\n  b: calc(infinity);\n}\n");
    assert_eq!(ours("a {b: -1e400}\n"), "a {\n  b: calc(-infinity);\n}\n");
    // Unit-bearing non-finite values keep their unit as a `* 1<unit>` operand.
    assert_eq!(
        ours("a {b: (1px / 0) * 1}\n"),
        "a {\n  b: calc(infinity * 1px);\n}\n"
    );
    assert_eq!(ours("a {b: (0px / 0) * 1}\n"), "a {\n  b: calc(NaN * 1px);\n}\n");
    // Interpolation produces the same calc form (no longer a bare `Infinity`).
    assert_eq!(ours("a {b: #{1e400}}\n"), "a {\n  b: calc(infinity);\n}\n");
}

#[test]
fn list_is_bracketed_and_zip() {
    // `is-bracketed` reports the bracket flag; a bare value, an empty list,
    // and a plain space/comma list are all `false`.
    assert_eq!(ours("a {b: is-bracketed([a b c])}\n"), "a {\n  b: true;\n}\n");
    assert_eq!(ours("a {b: is-bracketed(a b c)}\n"), "a {\n  b: false;\n}\n");
    assert_eq!(ours("a {b: is-bracketed(())}\n"), "a {\n  b: false;\n}\n");
    // `zip` interleaves corresponding elements into a comma list of space
    // lists, truncating to the shortest input.
    assert_eq!(
        ours("a {b: zip(1px 2px, solid dashed, red blue)}\n"),
        "a {\n  b: 1px solid red, 2px dashed blue;\n}\n"
    );
    assert_eq!(ours("a {b: zip(1 2 3, c d)}\n"), "a {\n  b: 1 c, 2 d;\n}\n");
    // A single list zips each element into its own one-element row.
    assert_eq!(ours("a {b: zip(a b c)}\n"), "a {\n  b: a, b, c;\n}\n");
}

#[test]
fn meta_feature_exists_known_set() {
    // `feature-exists` is `true` for dart-sass's fixed feature set (quoted or
    // unquoted), and `false` for any other name.
    assert_eq!(ours("a {b: feature-exists(at-error)}\n"), "a {\n  b: true;\n}\n");
    assert_eq!(
        ours("a {b: feature-exists(global-variable-shadowing)}\n"),
        "a {\n  b: true;\n}\n"
    );
    assert_eq!(
        ours("a {b: feature-exists(\"custom-property\")}\n"),
        "a {\n  b: true;\n}\n"
    );
    assert_eq!(
        ours("a {b: feature-exists(units-level-3)}\n"),
        "a {\n  b: true;\n}\n"
    );
    assert_eq!(
        ours("a {b: feature-exists(extend-selector-pseudoclass)}\n"),
        "a {\n  b: true;\n}\n"
    );
    assert_eq!(ours("a {b: feature-exists(nope)}\n"), "a {\n  b: false;\n}\n");
}

#[test]
fn equality_is_unit_and_format_aware() {
    // Numbers compare across convertible units (`1in == 96px`), but unitless
    // vs unit-bearing and incompatible units stay unequal. Units are
    // case-sensitive in `==` (`1PX != 1px`).
    assert_eq!(ours("a {b: 1in == 96px}\n"), "a {\n  b: true;\n}\n");
    assert_eq!(ours("a {b: 1cm == 10mm}\n"), "a {\n  b: true;\n}\n");
    assert_eq!(ours("a {b: 100grad == 90deg}\n"), "a {\n  b: true;\n}\n");
    assert_eq!(ours("a {b: 1s == 1000ms}\n"), "a {\n  b: true;\n}\n");
    assert_eq!(ours("a {b: 1 == 1px}\n"), "a {\n  b: false;\n}\n");
    assert_eq!(ours("a {b: 1px == 1em}\n"), "a {\n  b: false;\n}\n");
    assert_eq!(ours("a {b: 1PX == 1px}\n"), "a {\n  b: false;\n}\n");
    // Colors compare resolved channels fuzzily: a named color equals an HSL
    // color that resolves to the same sRGB channels within epsilon.
    assert_eq!(
        ours("a {b: purple == hsl(300, 100%, 25.098039215686%)}\n"),
        "a {\n  b: true;\n}\n"
    );
    // Genuinely different colors stay unequal.
    assert_eq!(ours("a {b: red == hsl(0, 0%, 50%)}\n"), "a {\n  b: false;\n}\n");
}

#[test]
fn extend_basic_and_placeholders() {
    // A class extend adds the extender as an alternative selector.
    assert_eq!(
        ours(".foo {a: b}\n.bar {@extend .foo}\n"),
        ".foo, .bar {\n  a: b;\n}\n"
    );
    // A placeholder rule emits nothing on its own, but its body surfaces under
    // the extending selector(s).
    assert_eq!(ours("%p {color: red}\n"), "");
    assert_eq!(
        ours("%p {color: red}\n.a {@extend %p}\n"),
        ".a {\n  color: red;\n}\n"
    );
    // Nested target: the extender replaces the matched compound in place.
    assert_eq!(
        ours(".foo .bar {a: b}\n.baz {@extend .bar}\n"),
        ".foo .bar, .foo .baz {\n  a: b;\n}\n"
    );
    // Compound unification across two extends, with the within-compound product.
    assert_eq!(
        ours(".foo.bar {a: b}\n.baz {@extend .foo}\n.bang {@extend .bar}\n"),
        ".foo.bar, .foo.bang, .bar.baz, .baz.bang {\n  a: b;\n}\n"
    );
    // !optional suppresses the "target not found" error.
    assert_eq!(
        ours(".a {x: y; @extend .missing !optional}\n"),
        ".a {\n  x: y;\n}\n"
    );
}

#[test]
fn extend_trim_and_chain_order() {
    // Redundant subselectors are trimmed: `.baz` supersedes `.foo.baz` etc.
    assert_eq!(
        ours(".foo.bar {a: b}\n.baz {@extend .foo; @extend .bar}\n"),
        ".foo.bar, .baz {\n  a: b;\n}\n"
    );
    // The universal selector supersedes the bare class, so only `-a *` remains.
    assert_eq!(
        ours("%-a .foo {a: b}\n* {@extend .foo} -a {@extend %-a}\n"),
        "-a * {\n  a: b;\n}\n"
    );
    // Chained extends keep dart-sass's reverse-registration ordering of
    // same-target extenders.
    assert_eq!(
        ours(".foo {a: b}\n.bar {@extend .foo}\n.baz {@extend .bar}\n.bip {@extend .bar}\n"),
        ".foo, .bar, .bip, .baz {\n  a: b;\n}\n"
    );
    // Two direct extenders of the same target also come out reversed.
    assert_eq!(
        ours(".foo {a: b}\n.bar {@extend .foo}\n.baz {@extend .foo}\n"),
        ".foo, .baz, .bar {\n  a: b;\n}\n"
    );
}

#[test]
fn extend_weaves_multi_component_extenders() {
    // A multi-component extender interweaves its parents with the matched
    // selector's parents in all order-preserving ways (dart-sass `weave`).
    assert_eq!(
        ours(".baz .bip .foo {a: b}\nfoo .grank bar {@extend .foo}\n"),
        ".baz .bip .foo, .baz .bip foo .grank bar, foo .grank .baz .bip bar {\n  a: b;\n}\n"
    );
    // Identical parent prefixes unify to a single woven selector.
    assert_eq!(
        ours(".baz .bip .foo {a: b}\n.baz .bip bar {@extend .foo}\n"),
        ".baz .bip .foo, .baz .bip bar {\n  a: b;\n}\n"
    );
}

#[test]
fn extend_universal_and_element_unification() {
    // `*|*` unified into a compound with a class drops the universal entirely.
    assert_eq!(
        ours("%-a .foo.bar {a: b}\n*|* {@extend .foo} -a {@extend %-a}\n"),
        "-a .bar {\n  a: b;\n}\n"
    );
    // A namespaced universal target keeps its namespace where it can't unify away.
    assert_eq!(
        ours("%-a ns|*.foo {a: b}\n* {@extend .foo} -a {@extend %-a}\n"),
        "-a ns|*.foo {\n  a: b;\n}\n"
    );
    // A namespaced type extender unifies with `*` to the concrete element.
    assert_eq!(
        ours("%-a *.foo {a: b}\n*|a {@extend .foo} -a {@extend %-a}\n"),
        "-a *.foo, -a a {\n  a: b;\n}\n"
    );
}

#[test]
fn extend_pseudo_class_and_element_ordering() {
    // A unified pseudo-class keeps its order after an existing pseudo-class.
    assert_eq!(
        ours("%-a :foo.baz {a: b}\n:bar {@extend .baz} -a {@extend %-a}\n"),
        "-a :foo.baz, -a :foo:bar {\n  a: b;\n}\n"
    );
    // Pseudo-classes always sort before a pseudo-element in the result.
    assert_eq!(
        ours(".foo:bar {a: b}\n.baz::bang {@extend .foo}\n"),
        ".foo:bar, .baz:bar::bang {\n  a: b;\n}\n"
    );
    // `:not()` unifies as an ordinary pseudo-class.
    assert_eq!(
        ours("%-a :not(.foo).baz {a: b}\n:not(.bar) {@extend .baz} -a {@extend %-a}\n"),
        "-a :not(.foo).baz, -a :not(.foo):not(.bar) {\n  a: b;\n}\n"
    );
}

#[test]
fn extend_across_media_is_an_error() {
    // An `@extend` inside `@media` may not extend a selector defined at the
    // document root.
    assert!(compile(
        ".foo { a: b }\n@media print { .bar { @extend .foo } }\n",
        &Options::default()
    )
    .is_err());
    // Both target and extender inside the same media context is fine.
    assert_eq!(
        ours("@media print { .a { x: y } .b { @extend .a } }\n"),
        "@media print {\n  .a, .b {\n    x: y;\n  }\n}\n"
    );
    // A bare `@extend` directly inside `@at-root` (no enclosing rule) errors.
    assert!(compile(
        ".a { x: y }\n.b { @at-root (with: media) { @extend .a } }\n",
        &Options::default()
    )
    .is_err());
}

#[test]
fn placeholder_inside_pseudo_arguments() {
    // A nonexistent `%placeholder` is removed from `:is()`/`:not()` arguments.
    assert_eq!(ours("a:not(%b) {x: y}\n"), "a {\n  x: y;\n}\n");
    assert_eq!(ours(":not(%b) {x: y}\n"), "* {\n  x: y;\n}\n");
    assert_eq!(ours("a:is(%b, c) {x: y}\n"), "a:is(c) {\n  x: y;\n}\n");
    assert_eq!(ours("a:not(%b, c) {x: y}\n"), "a:not(c) {\n  x: y;\n}\n");
    // A solo `%placeholder` in a matches-any pseudo removes the whole rule.
    assert_eq!(ours("a:is(%b) {x: y}\n"), "");
}

#[test]
fn unquoted_value_escapes_are_canonicalized() {
    // A CSS escape in an unquoted value decodes to its code point and then
    // re-serializes per dart-sass's identifier rules: printable name chars
    // become literal, control chars use the `\<hex> ` form, and other
    // punctuation is backslash-escaped.
    assert_eq!(ours("a {b: \\41}\n"), "a {\n  b: A;\n}\n");
    assert_eq!(ours("a {b: \\41 BC}\n"), "a {\n  b: ABC;\n}\n");
    assert_eq!(ours("a {b: \\9}\n"), "a {\n  b: \\9 ;\n}\n");
    assert_eq!(ours("a {b: \\0}\n"), "a {\n  b: \\0 ;\n}\n");
    // A leading digit (or one right after a leading hyphen) is hex-escaped;
    // the same digit mid-identifier stays literal.
    assert_eq!(ours("a {b: \\30 x}\n"), "a {\n  b: \\30 x;\n}\n");
    assert_eq!(ours("a {b: q\\30 x}\n"), "a {\n  b: q0x;\n}\n");
    assert_eq!(ours("a {b: -\\30 x}\n"), "a {\n  b: -\\30 x;\n}\n");
    // A `-` produced by an escape at identifier start is backslash-escaped,
    // but a literal leading `-` stays bare.
    assert_eq!(ours("a {b: \\2d a}\n"), "a {\n  b: \\-a;\n}\n");
    assert_eq!(ours("a {b: \\2d\\2d}\n"), "a {\n  b: \\--;\n}\n");
    assert_eq!(ours("a {b: -\\2d}\n"), "a {\n  b: -\\-;\n}\n");
    assert_eq!(ours("a {b: a\\2d}\n"), "a {\n  b: a-;\n}\n");
    // Printable punctuation gets a literal backslash; a literal backslash
    // round-trips.
    assert_eq!(ours("a {b: \\21}\n"), "a {\n  b: \\!;\n}\n");
    assert_eq!(ours("a {b: \\7f}\n"), "a {\n  b: \\7f ;\n}\n");
    assert_eq!(ours("a {b: \\\\}\n"), "a {\n  b: \\\\;\n}\n");
}

#[test]
fn quoted_string_escapes_are_normalized() {
    // Quoted strings decode escapes to code points and re-serialize per
    // dart-sass: printable chars pass through, `\#{` becomes a literal `#{`,
    // and only the quote char, backslash, and control chars are re-escaped.
    assert_eq!(ours("a {b: \"\\41\"}\n"), "a {\n  b: \"A\";\n}\n");
    assert_eq!(ours("a {b: \"x\\#{y}\"}\n"), "a {\n  b: \"x#{y}\";\n}\n");
    assert_eq!(ours("a {b: \"\\#{y}\"}\n"), "a {\n  b: \"#{y}\";\n}\n");
    // Tab (0x09) stays literal inside quotes; DEL is hex-escaped.
    assert_eq!(ours("a {b: \"\\9\"}\n"), "a {\n  b: \"\t\";\n}\n");
    assert_eq!(ours("a {b: \"\\7f\"}\n"), "a {\n  b: \"\\7f\";\n}\n");
    // A control escape gets a trailing space only when the next char would
    // extend the escape (a hex digit).
    assert_eq!(ours("a {b: \"\\1 0\"}\n"), "a {\n  b: \"\\1 0\";\n}\n");
    assert_eq!(ours("a {b: \"\\1 a\"}\n"), "a {\n  b: \"\\1 a\";\n}\n");
    // A string containing `\"` but no `'` is rewrapped in single quotes; one
    // containing both keeps double quotes and escapes the inner `"`.
    assert_eq!(ours("a {b: \"a\\\"b\"}\n"), "a {\n  b: 'a\"b';\n}\n");
    assert_eq!(ours("a {b: \"a'b\\\"c\"}\n"), "a {\n  b: \"a'b\\\"c\";\n}\n");
    // A literal backslash round-trips as `\\`.
    assert_eq!(ours("a {b: \"\\\\\"}\n"), "a {\n  b: \"\\\\\";\n}\n");
}

#[test]
fn unquoted_url_contents_escapes_are_canonicalized() {
    // CSS escapes inside plain (unquoted) `url(...)` contents decode and
    // re-serialize with the identifier body rules: name chars (including a
    // leading digit or `-`) stay literal, control chars use `\<hex> `, and
    // other punctuation is backslash-escaped. `\#{` stays a literal `#{`.
    assert_eq!(ours("a {b: url(\\41)}\n"), "a {\n  b: url(A);\n}\n");
    assert_eq!(ours("a {b: url(\\41 bc)}\n"), "a {\n  b: url(Abc);\n}\n");
    assert_eq!(ours("a {b: url(\\30)}\n"), "a {\n  b: url(0);\n}\n");
    assert_eq!(ours("a {b: url(\\2d)}\n"), "a {\n  b: url(-);\n}\n");
    assert_eq!(ours("a {b: url(\\9)}\n"), "a {\n  b: url(\\9 );\n}\n");
    assert_eq!(ours("a {b: url(\\7f)}\n"), "a {\n  b: url(\\7f );\n}\n");
    assert_eq!(ours("a {b: url(\\21)}\n"), "a {\n  b: url(\\!);\n}\n");
    assert_eq!(ours("a {b: url(\\))}\n"), "a {\n  b: url(\\));\n}\n");
    assert_eq!(ours("a {b: url(\\#{})}\n"), "a {\n  b: url(\\#{});\n}\n");
}

#[test]
fn non_ascii_output_declares_utf8_charset() {
    use sasso::OutputStyle;
    // Non-ASCII output (here produced by a unicode escape) gets a leading
    // `@charset "UTF-8";` in expanded output and a UTF-8 BOM in compressed
    // output, matching dart-sass. Pure-ASCII output gets neither.
    assert_eq!(
        ours("a {b: url(\\2603)}\n"),
        "@charset \"UTF-8\";\na {\n  b: url(\u{2603});\n}\n"
    );
    assert_eq!(ours("a {b: c}\n"), "a {\n  b: c;\n}\n");
    let compressed = compile(
        "a {b: url(\\2603)}\n",
        &Options::default().with_style(OutputStyle::Compressed),
    )
    .expect("compile failed");
    assert_eq!(compressed, "\u{FEFF}a{b:url(\u{2603})}");
}

#[test]
fn leading_utf8_bom_is_stripped() {
    // A leading UTF-8 BOM in the source is dropped before parsing, so it never
    // appears in the output and never triggers a spurious `@charset`.
    assert_eq!(ours("\u{FEFF}foo {bar: baz}\n"), "foo {\n  bar: baz;\n}\n");
}

#[test]
fn unicode_range_tokens_parse_and_preserve() {
    // CSS unicode-range values: plain ranges, `-end` ranges, and `?`
    // wildcards. The original case is preserved (`u+1a2b` stays lowercase).
    assert_eq!(ours("a {b: U+1}\n"), "a {\n  b: U+1;\n}\n");
    assert_eq!(ours("a {b: U+123456}\n"), "a {\n  b: U+123456;\n}\n");
    assert_eq!(ours("a {b: u+1a2b}\n"), "a {\n  b: u+1a2b;\n}\n");
    assert_eq!(ours("a {b: U+4??}\n"), "a {\n  b: U+4??;\n}\n");
    assert_eq!(ours("a {b: U+0-7F}\n"), "a {\n  b: U+0-7F;\n}\n");
    assert_eq!(
        ours("a {b: U+1A2B3C-10FFFF}\n"),
        "a {\n  b: U+1A2B3C-10FFFF;\n}\n"
    );
    // A `?`-wildcard token is terminal: a directly-following identifier
    // becomes a fresh space-list element (`U+A?BCDE` -> `U+A? BCDE`,
    // `U+A?-BCDE` -> `U+A? -BCDE`), while `-<digit>` continues as a
    // subtraction whose unquoted-string join keeps the source spelling.
    assert_eq!(ours("a {b: U+A?BCDE}\n"), "a {\n  b: U+A? BCDE;\n}\n");
    assert_eq!(ours("a {b: U+A?-BCDE}\n"), "a {\n  b: U+A? -BCDE;\n}\n");
    assert_eq!(ours("a {b: U+A?-1234}\n"), "a {\n  b: U+A?-1234;\n}\n");
    // Malformed ranges still error like dart-sass.
    assert!(compile("a {b: U+}\n", &Options::default()).is_err());
    assert!(compile("a {b: U+1234567}\n", &Options::default()).is_err());
    assert!(compile("a {b: U+123-456-ABC}\n", &Options::default()).is_err());
}

#[test]
fn minus_operator_string_joins_non_numbers() {
    // dart-sass's `-` operator subtracts two numbers but otherwise produces an
    // unquoted string join `<left>-<right>`, with each side keeping its own
    // serialization (quoted strings keep their quotes).
    assert_eq!(ours("a {b: foo - 1}\n"), "a {\n  b: foo-1;\n}\n");
    assert_eq!(ours("a {b: 1 - foo}\n"), "a {\n  b: 1-foo;\n}\n");
    assert_eq!(ours("a {b: foo - bar}\n"), "a {\n  b: foo-bar;\n}\n");
    assert_eq!(ours("a {b: \"q\" - 1}\n"), "a {\n  b: \"q\"-1;\n}\n");
    assert_eq!(ours("a {b: 1 - \"q\"}\n"), "a {\n  b: 1-\"q\";\n}\n");
    assert_eq!(ours("a {b: red - foo}\n"), "a {\n  b: red-foo;\n}\n");
    // Two numbers still subtract numerically.
    assert_eq!(ours("a {b: 10 - 20}\n"), "a {\n  b: -10;\n}\n");
    assert_eq!(ours("a {b: 1px - 2px}\n"), "a {\n  b: -1px;\n}\n");
}

#[test]
fn single_equals_ms_filter_operator_in_args() {
    // A lone `=` inside a function argument list is the lowest-precedence
    // Microsoft-filter operator: both sides are evaluated and joined with `=`
    // (no spaces) into an unquoted string. Surrounding whitespace is dropped.
    assert_eq!(ours("a {b: foo(a=b)}\n"), "a {\n  b: foo(a=b);\n}\n");
    assert_eq!(ours("a {b: foo(1=2)}\n"), "a {\n  b: foo(1=2);\n}\n");
    assert_eq!(ours("a {b: foo(a = b)}\n"), "a {\n  b: foo(a=b);\n}\n");
    assert_eq!(ours("a {b: foo(a=b=c)}\n"), "a {\n  b: foo(a=b=c);\n}\n");
    assert_eq!(ours("a {b: foo((1 + 2)=3)}\n"), "a {\n  b: foo(3=3);\n}\n");
    assert_eq!(ours("a {b: foo(a b = c d)}\n"), "a {\n  b: foo(a b=c d);\n}\n");
    assert_eq!(ours("a {b: foo(1 + 2 = 3 + 4)}\n"), "a {\n  b: foo(3=7);\n}\n");
    // `==` stays the equality operator inside arguments.
    assert_eq!(ours("a {b: foo(1 == 1)}\n"), "a {\n  b: foo(true);\n}\n");
}

#[test]
fn global_alpha_microsoft_filter_overload() {
    // The global `alpha()` with unquoted `name=value` arguments is the
    // proprietary IE filter overload: it passes through verbatim as a CSS
    // function rather than being treated as a color accessor.
    assert_eq!(
        ours("a {b: alpha(opacity=80)}\n"),
        "a {\n  b: alpha(opacity=80);\n}\n"
    );
    assert_eq!(ours("a {b: alpha(c=d)}\n"), "a {\n  b: alpha(c=d);\n}\n");
    assert_eq!(
        ours("a {b: alpha(c=d, e=f, g=h)}\n"),
        "a {\n  b: alpha(c=d, e=f, g=h);\n}\n"
    );
    // A real color argument still routes to the normal alpha accessor.
    assert_eq!(ours("a {b: alpha(red)}\n"), "a {\n  b: 1;\n}\n");
}

#[test]
fn unary_plus_and_minus_on_non_numbers() {
    // Unary `-`/`+` negate / identity a number, but on any other operand they
    // prepend the sign as an unquoted string (dart-sass `unaryMinus`/
    // `unaryPlus`). Whitespace may separate the operator from its operand.
    assert_eq!(ours("a {b: (- red)}\n"), "a {\n  b: -red;\n}\n");
    assert_eq!(ours("a {b: (+ red)}\n"), "a {\n  b: +red;\n}\n");
    assert_eq!(ours("a {b: +hello}\n"), "a {\n  b: +hello;\n}\n");
    assert_eq!(ours("a {b: + hello}\n"), "a {\n  b: +hello;\n}\n");
    assert_eq!(ours("a {b: (+- red)}\n"), "a {\n  b: +-red;\n}\n");
    assert_eq!(ours("a {b: (- \"q\")}\n"), "a {\n  b: -\"q\";\n}\n");
    assert_eq!(ours("a {b: (+ \"q\")}\n"), "a {\n  b: +\"q\";\n}\n");
    // Numbers keep arithmetic semantics.
    assert_eq!(ours("a {b: +10}\n"), "a {\n  b: 10;\n}\n");
    assert_eq!(ours("a {b: (- 5)}\n"), "a {\n  b: -5;\n}\n");
    assert_eq!(ours("a {b: -10px + 10px}\n"), "a {\n  b: 0px;\n}\n");
    // A `-` glued to an identifier stays part of the identifier.
    assert_eq!(ours("a {b: -webkit-box}\n"), "a {\n  b: -webkit-box;\n}\n");
}

#[test]
fn number_rounds_half_away_from_zero_at_tenth_place() {
    // dart-sass rounds to 10 decimal places half away from zero, not half to
    // even. `1.5e-10` -> `0.0000000002`, `0.99999999995` -> `1`, and
    // `0.30000000005` -> `0.3000000001`. Verified byte-for-byte against
    // `npx sass --no-source-map --stdin`.
    assert_eq!(ours("a {b: 0.00000000015}\n"), "a {\n  b: 0.0000000002;\n}\n");
    assert_eq!(ours("a {b: 0.00000000035}\n"), "a {\n  b: 0.0000000004;\n}\n");
    assert_eq!(ours("a {b: 0.99999999995}\n"), "a {\n  b: 1;\n}\n");
    assert_eq!(ours("a {b: 1.99999999995}\n"), "a {\n  b: 2;\n}\n");
    assert_eq!(ours("a {b: 0.30000000005}\n"), "a {\n  b: 0.3000000001;\n}\n");
    // Values that already round-tripped correctly must stay unchanged.
    assert_eq!(ours("a {b: 0.0000000001}\n"), "a {\n  b: 0.0000000001;\n}\n");
    assert_eq!(ours("a {b: 0.1}\n"), "a {\n  b: 0.1;\n}\n");
    assert_eq!(
        ours("a {b: 123456789.12345678905}\n"),
        "a {\n  b: 123456789.12345679;\n}\n"
    );
}

#[test]
fn opaque_hex_literals_preserve_authored_spelling() {
    // dart-sass emits an opaque 3-/6-digit hex literal exactly as authored,
    // keeping its length and case. The 4-/8-digit alpha forms, by contrast,
    // are canonicalized. Verified byte-for-byte against
    // `npx sass --no-source-map --stdin`.
    assert_eq!(ours("a {b: #fff}\n"), "a {\n  b: #fff;\n}\n");
    assert_eq!(ours("a {b: #FFF}\n"), "a {\n  b: #FFF;\n}\n");
    assert_eq!(ours("a {b: #aaa}\n"), "a {\n  b: #aaa;\n}\n");
    assert_eq!(ours("a {b: #FFAA00}\n"), "a {\n  b: #FFAA00;\n}\n");
    assert_eq!(ours("a {b: #ABCABC}\n"), "a {\n  b: #ABCABC;\n}\n");
    assert_eq!(ours("a {b: #aaaaaa}\n"), "a {\n  b: #aaaaaa;\n}\n");
    // 4-/8-digit opaque alpha forms canonicalize to lowercase 6-digit hex.
    assert_eq!(ours("a {b: #369f}\n"), "a {\n  b: #336699;\n}\n");
    assert_eq!(ours("a {b: #112233ff}\n"), "a {\n  b: #112233;\n}\n");
    // Compressed output ignores the authored spelling and shortens.
    use sasso::OutputStyle;
    let compressed = compile(
        "a {b: #FFAA00}\n",
        &Options::default().with_style(OutputStyle::Compressed),
    )
    .expect("compile failed");
    assert_eq!(compressed, "a{b:#fa0}");
}

#[test]
fn selector_nest_and_append() {
    // `selector-nest` joins selectors as descendants and resolves `&`; the
    // result is a comma list of space lists rendered as the usual selector
    // string. `selector-append` joins with no descendant combinator (the
    // leading compound of each suffix merges onto the prefix's trailing one).
    // All outputs byte-verified against dart-sass.
    assert_eq!(ours("a {b: selector-nest(c, d)}\n"), "a {\n  b: c d;\n}\n");
    assert_eq!(
        ours("a {b: selector-nest(\".a, .b\", \".c, .d\")}\n"),
        "a {\n  b: .a .c, .a .d, .b .c, .b .d;\n}\n"
    );
    assert_eq!(
        ours("a {b: selector-nest(\".a .b\", \"&:hover\")}\n"),
        "a {\n  b: .a .b:hover;\n}\n"
    );
    assert_eq!(
        ours("a {b: selector-nest(\".p1 .p2\", \".x & .y\")}\n"),
        "a {\n  b: .x .p1 .p2 .y;\n}\n"
    );
    assert_eq!(ours("a {b: selector-append(c, d)}\n"), "a {\n  b: cd;\n}\n");
    assert_eq!(
        ours("a {b: selector-append(\".a .b\", \".c .d\")}\n"),
        "a {\n  b: .a .b.c .d;\n}\n"
    );
    assert_eq!(
        ours("a {b: selector-append(\".a, .b\", \".c .d, .e\")}\n"),
        "a {\n  b: .a.c .d, .a.e, .b.c .d, .b.e;\n}\n"
    );
}

#[test]
fn selector_extend_and_replace() {
    // `selector-extend` adds the extender wherever the (compound) extendee
    // matches, keeping the original; `selector-replace` drops the matched
    // original. Compound extendees (`.a.b`) match a compound only when all
    // their simples are present. All outputs byte-verified against dart-sass.
    assert_eq!(ours("a {b: selector-extend(c, c, d)}\n"), "a {\n  b: c, d;\n}\n");
    assert_eq!(
        ours("a {b: selector-extend(\".a .b\", \".b\", \".c\")}\n"),
        "a {\n  b: .a .b, .a .c;\n}\n"
    );
    assert_eq!(
        ours("a {b: selector-extend(\".a.b .c\", \".a.b\", \".x\")}\n"),
        "a {\n  b: .a.b .c, .x .c;\n}\n"
    );
    // `.a .c` does NOT match the compound `.a.b`, so it is unchanged.
    assert_eq!(
        ours("a {b: selector-extend(\".a .c\", \".a.b\", \".x\")}\n"),
        "a {\n  b: .a .c;\n}\n"
    );
    assert_eq!(ours("a {b: selector-replace(c, c, d)}\n"), "a {\n  b: d;\n}\n");
    assert_eq!(
        ours("a {b: selector-replace(\".a.b.c\", \".a.b\", \".x\")}\n"),
        "a {\n  b: .c.x;\n}\n"
    );
    assert_eq!(
        ours("a {b: selector-replace(\".x\", \".c\", \".d\")}\n"),
        "a {\n  b: .x;\n}\n"
    );
}

#[test]
fn selector_unify_superselector_simple_and_parse() {
    // `selector-unify` yields the selectors matching both inputs (or `null`
    // when nothing unifies — the declaration is then dropped). `is-superselector`
    // tests selector-list containment. `simple-selectors` splits one compound
    // into its simples; `selector-parse` round-trips a selector string. All
    // outputs byte-verified against dart-sass.
    assert_eq!(
        ours("a {b: selector-unify(\".c\", \".d\")}\n"),
        "a {\n  b: .c.d;\n}\n"
    );
    assert_eq!(
        ours("a {b: selector-unify(\".a .b\", \".c .d\")}\n"),
        "a {\n  b: .a .c .b.d, .c .a .b.d;\n}\n"
    );
    // Incompatible type selectors don't unify; `null` drops the declaration,
    // leaving the rule empty and therefore omitted entirely (as dart-sass does).
    assert_eq!(ours("a {b: selector-unify(a, b)}\n"), "");
    assert_eq!(ours("a {b: is-superselector(c, d)}\n"), "a {\n  b: false;\n}\n");
    assert_eq!(
        ours("a {b: is-superselector(\".a\", \".a.b\")}\n"),
        "a {\n  b: true;\n}\n"
    );
    assert_eq!(
        ours("a {b: simple-selectors(\".c.d\")}\n"),
        "a {\n  b: .c, .d;\n}\n"
    );
    assert_eq!(
        ours("a {b: simple-selectors(\"a.b.c:hover\")}\n"),
        "a {\n  b: a, .b, .c, :hover;\n}\n"
    );
    assert_eq!(
        ours("a {b: selector-parse(\".c, .d\")}\n"),
        "a {\n  b: .c, .d;\n}\n"
    );
    assert_eq!(
        ours("a {b: selector-parse(\".a > .b, .c + .d\")}\n"),
        "a {\n  b: .a > .b, .c + .d;\n}\n"
    );
}

#[test]
fn function_exists_recognizes_builtins() {
    // `function-exists` reports `true` for a built-in function and `false`
    // for an unknown name (dart-sass `function-exists`).
    assert_eq!(ours("a {b: function-exists(rgb)}\n"), "a {\n  b: true;\n}\n");
    assert_eq!(ours("a {b: function-exists(\"rgb\")}\n"), "a {\n  b: true;\n}\n");
    assert_eq!(ours("a {b: function-exists(c)}\n"), "a {\n  b: false;\n}\n");
    // Arity and type validation matches dart-sass.
    assert!(compile("a {b: function-exists()}\n", &Options::default()).is_err());
    assert!(compile("a {b: function-exists(a, b, c)}\n", &Options::default()).is_err());
    assert!(compile("a {b: function-exists(2px)}\n", &Options::default()).is_err());
}

#[test]
fn get_function_validates_arity_and_type() {
    // `get-function` raises dart-sass's arity / type errors before resolution.
    assert!(compile("a {b: get-function()}\n", &Options::default()).is_err());
    assert!(compile("a {b: get-function(c, true, d, e)}\n", &Options::default()).is_err());
    assert!(compile("a {b: get-function(2px)}\n", &Options::default()).is_err());
    // A well-formed call has no function-reference value at this layer, so it
    // is preserved verbatim as a plain CSS function.
    assert_eq!(
        ours("a {b: get-function(rgb)}\n"),
        "a {\n  b: get-function(rgb);\n}\n"
    );
}

#[test]
fn nested_property_sets() {
    // A bare property set (`prop: { … }`) namespaces each child as
    // `prop-<child>`; an empty value needs no whitespace after the colon.
    assert_eq!(ours("a { b: { c: d } }\n"), "a {\n  b-c: d;\n}\n");
    assert_eq!(ours("a { b:{ c: d } }\n"), "a {\n  b-c: d;\n}\n");
    assert_eq!(ours("a { b: { -c: d } }\n"), "a {\n  b--c: d;\n}\n");
    // The value-plus-block form (`prop: value { … }`) emits the value first,
    // then the namespaced children — but only when whitespace follows the
    // colon; `b:c { … }` is a style rule, not a property set.
    assert_eq!(ours("a { b: c { d: e } }\n"), "a {\n  b: c;\n  b-d: e;\n}\n");
    assert_eq!(ours("a { b:c { d: e } }\n"), "a b:c {\n  d: e;\n}\n");
    // Property sets nest, joining each level with `-`, and interleave with a
    // following sibling separated by `;`.
    assert_eq!(
        ours("a { b: { c: { d: e }; f: g } }\n"),
        "a {\n  b-c-d: e;\n  b-f: g;\n}\n"
    );
    // A custom-property child (literal `--`) may not be nested.
    assert!(compile("a { b: { --d: e } }\n", &Options::default()).is_err());
}

#[test]
fn custom_property_declarations() {
    // A literal `--` name takes a verbatim value: SassScript is not evaluated,
    // only `#{…}` interpolation resolves.
    assert_eq!(ours("a { --x: 1 + 2; }\n"), "a {\n  --x: 1 + 2;\n}\n");
    assert_eq!(ours("a { --x: #{1 + 2}; }\n"), "a {\n  --x: 3;\n}\n");
    // Interpolation resolves even inside a quoted string in the value.
    assert_eq!(ours("a { --x: \"c#{1 + 2}d\"; }\n"), "a {\n  --x: \"c3d\";\n}\n");
    // A partially interpolated name beginning literally with `--` keeps the
    // raw value, while a fully/initially interpolated name is real SassScript.
    assert_eq!(
        ours("a { --#{only}-name: 1 + 2; }\n"),
        "a {\n  --only-name: 1 + 2;\n}\n"
    );
    assert_eq!(ours("a { #{--entire}: 1 + 2; }\n"), "a {\n  --entire: 3;\n}\n");
    // `!important` in a custom-property value is a literal value character.
    assert_eq!(
        ours("a { --x: value !important; }\n"),
        "a {\n  --x: value !important;\n}\n"
    );
}

#[test]
fn var_env_argument_evaluation() {
    // `var()`/`env()` are plain CSS functions whose arguments are real
    // SassScript: the fallback evaluates, whitespace normalises, and a
    // `var($args...)` splat expands — matching dart-sass byte for byte.
    assert_eq!(ours("a {b: var()}\n"), "a {\n  b: var();\n}\n");
    assert_eq!(ours("a {b: var(--c)}\n"), "a {\n  b: var(--c);\n}\n");
    // The fallback argument is evaluated.
    assert_eq!(ours("a {b: var(--c, 1 + 2)}\n"), "a {\n  b: var(--c, 3);\n}\n");
    assert_eq!(
        ours("a {b: var(--c, \"d\" + \"e\")}\n"),
        "a {\n  b: var(--c, \"de\");\n}\n"
    );
    // Surrounding whitespace around the trailing comma normalises.
    assert_eq!(ours("a {b: var(--c , )}\n"), "a {\n  b: var(--c, );\n}\n");
    assert_eq!(ours("a {b: var(--c ,)}\n"), "a {\n  b: var(--c, );\n}\n");
    // `allowEmptySecondArg`: a trailing comma after exactly the first argument
    // keeps an empty second argument — but only for the name `var`
    // (case-insensitively), never for `env`.
    assert_eq!(ours("a {b: var(--c,)}\n"), "a {\n  b: var(--c, );\n}\n");
    assert_eq!(ours("a {b: VaR(--c,)}\n"), "a {\n  b: VaR(--c, );\n}\n");
    assert_eq!(ours("a {b: env(--c, )}\n"), "a {\n  b: env(--c);\n}\n");
    // A trailing comma after the *second* argument is an ordinary ignorable
    // trailing comma (no empty arg added).
    assert_eq!(ours("a {b: var(--c, d,)}\n"), "a {\n  b: var(--c, d);\n}\n");
    // A parenthesised comma list spreads its items as separate arguments.
    assert_eq!(
        ours("a {b: var(--c, (1, 2))}\n"),
        "a {\n  b: var(--c, 1, 2);\n}\n"
    );
    // `var($args...)` splat expands list/single rest arguments.
    assert_eq!(
        ours("$name: --c; a {b: var($name...)}\n"),
        "a {\n  b: var(--c);\n}\n"
    );
    assert_eq!(
        ours("$args: --c, d; a {b: var($args...)}\n"),
        "a {\n  b: var(--c, d);\n}\n"
    );
    // A trailing comma after a rest argument is normal (no empty arg).
    assert_eq!(ours("$n: --c; a {b: var($n...,)}\n"), "a {\n  b: var(--c);\n}\n");
}

#[test]
fn calc_nested_parenthesization() {
    // A nested calc() whose interior is an unresolved interpolation that is
    // not a clean single token is parenthesized when spliced into the
    // surrounding calculation (whitespace / `*` / `/` are ambiguous).
    assert_eq!(
        ours("a {b: calc(calc(#{\"c*\"}))}\n"),
        "a {\n  b: calc((c*));\n}\n"
    );
    assert_eq!(
        ours("a {b: calc(calc(#{\"c/\"}))}\n"),
        "a {\n  b: calc((c/));\n}\n"
    );
    assert_eq!(
        ours("a {b: calc(calc(#{\"c \"}))}\n"),
        "a {\n  b: calc((c ));\n}\n"
    );
    // A `var()` substitution inside a nested calc is always grouped.
    assert_eq!(
        ours("a {b: calc(1 + calc(var(--c)))}\n"),
        "a {\n  b: calc(1 + (var(--c)));\n}\n"
    );
    // A clean identifier, a hyphenated token, a number, an operation, and a
    // complete sub-calculation all flatten without extra parentheses.
    assert_eq!(ours("a {b: calc(calc(#{c}))}\n"), "a {\n  b: calc(c);\n}\n");
    assert_eq!(ours("a {b: calc(calc(c-d))}\n"), "a {\n  b: calc(c-d);\n}\n");
    assert_eq!(ours("a {b: calc(calc(c + d))}\n"), "a {\n  b: calc(c + d);\n}\n");
    assert_eq!(
        ours("a {b: calc(1 + calc(min(1%, 2px)))}\n"),
        "a {\n  b: calc(1 + min(1%, 2px));\n}\n"
    );
    // A top-level (non-nested) calc keeps its own interpolation bare.
    assert_eq!(ours("a {b: calc(#{\"c*\"})}\n"), "a {\n  b: calc(c*);\n}\n");
}

#[test]
fn adjacent_quoted_string_schema() {
    // A quoted string that abuts an adjacent atom with no whitespace forms an
    // implicit space-separated list — including same-type quotes nested inside
    // interpolation, which dart-sass parses as a "string schema".
    assert_eq!(
        ours("a {b: \"[\"'foo'\"]\"}\n"),
        "a {\n  b: \"[\" \"foo\" \"]\";\n}\n"
    );
    assert_eq!(ours("a {b: \"x\"\"y\"}\n"), "a {\n  b: \"x\" \"y\";\n}\n");
    // A string followed by, or preceded by, a non-string atom also lists.
    assert_eq!(ours("a {b: \"x\"foo}\n"), "a {\n  b: \"x\" foo;\n}\n");
    assert_eq!(ours("a {b: foo\"x\"}\n"), "a {\n  b: foo \"x\";\n}\n");
    assert_eq!(ours("a {b: 1\"x\"}\n"), "a {\n  b: 1 \"x\";\n}\n");
    assert_eq!(
        ours("a {b: gamme \"x\"delta}\n"),
        "a {\n  b: gamme \"x\" delta;\n}\n"
    );
    // A same-type quote nested inside interpolation: the inner string schema
    // resolves and re-quotes around the surrounding string.
    assert_eq!(
        ours("a {b: \"[#{\"[\"'foo'\"]\"}]\"}\n"),
        "a {\n  b: \"[[ foo ]]\";\n}\n"
    );
    assert_eq!(ours("a {b: #{\"[\"'foo'\"]\"}}\n"), "a {\n  b: [ foo ];\n}\n");
    // Adjacency must not swallow a map key-value colon: a quoted-string map
    // key still parses (`("a": 1)`).
    assert_eq!(
        ours("$m: (\"a\": 1); a {b: map-get($m, \"a\")}\n"),
        "a {\n  b: 1;\n}\n"
    );
}

#[test]
fn supports_condition_serialization() {
    // A declaration condition normalizes spacing (`(a:b)` -> `(a: b)`) and
    // evaluates SassScript on both sides.
    assert_eq!(ours("@supports (a:b) {@c}\n"), "@supports (a: b) {\n  @c;\n}\n");
    assert_eq!(
        ours("@supports (1 + 1: b) {@c}\n"),
        "@supports (2: b) {\n  @c;\n}\n"
    );
    assert_eq!(
        ours("@supports (a: 1 + 1) {@c}\n"),
        "@supports (a: 2) {\n  @c;\n}\n"
    );
    // Redundant nested parentheses around a declaration collapse.
    assert_eq!(
        ours("@supports ((((a: b)))) {@c}\n"),
        "@supports (a: b) {\n  @c;\n}\n"
    );
    // `and`/`or`/`not` operators and grouping parentheses.
    assert_eq!(
        ours("@supports (a: b) and ((c: d) or (e: f)) {@g}\n"),
        "@supports (a: b) and ((c: d) or (e: f)) {\n  @g;\n}\n"
    );
    assert_eq!(
        ours("@supports not (a: b) {@c}\n"),
        "@supports not (a: b) {\n  @c;\n}\n"
    );
    // A custom-property value keeps no space after the colon and stays verbatim.
    assert_eq!(
        ours("@supports (--a: b) {@c}\n"),
        "@supports (--a: b) {\n  @c;\n}\n"
    );
    // A `calc()` (and `min`/`clamp`/…) is kept unsimplified inside a `@supports`
    // declaration, while a `#{…}` interpolation simplifies as usual.
    assert_eq!(
        ours("@supports (a: calc(1 + 2)) {@d}\n"),
        "@supports (a: calc(1 + 2)) {\n  @d;\n}\n"
    );
    assert_eq!(
        ours("@supports (a: clamp(0, 1, 2)) {@d}\n"),
        "@supports (a: clamp(0, 1, 2)) {\n  @d;\n}\n"
    );
    assert_eq!(
        ours("@supports (a: #{calc(1 + 2)}) {@d}\n"),
        "@supports (a: 3) {\n  @d;\n}\n"
    );
    // Trivia comments inside the condition are stripped; loud comments in an
    // "anything" value are preserved.
    assert_eq!(
        ours("@supports (a /**/: b) {c {d: e}}\n"),
        "@supports (a: b) {\n  c {\n    d: e;\n  }\n}\n"
    );
    // A `supports()`-style function call and an arbitrary "anything" value pass
    // through verbatim.
    assert_eq!(ours("@supports a(b) {@c}\n"), "@supports a(b) {\n  @c;\n}\n");
    assert_eq!(ours("@supports (a b) {@c}\n"), "@supports (a b) {\n  @c;\n}\n");
    // A lone interpolation is spliced in unquoted.
    assert_eq!(
        ours("@supports #{\"(a: b)\"} and (c: 1 + 1) {@d}\n"),
        "@supports (a: b) and (c: 2) {\n  @d;\n}\n"
    );
    // An empty/placeholder-only body produces no output.
    assert_eq!(ours("@supports (a: b) {}\n"), "");
    assert_eq!(ours("@supports (a: b) { %c {d: e} }\n"), "");
    // Malformed conditions are rejected.
    assert!(compile("@supports a {@b}\n", &Options::default()).is_err());
    assert!(compile("@supports (not a) {@b}\n", &Options::default()).is_err());
    assert!(compile(
        "@supports (a: b) and (c: d) or (e: f) {@g}\n",
        &Options::default()
    )
    .is_err());

    // Live parity for the same constructs.
    assert_parity("@supports (a:b) {@c}\n");
    assert_parity("@supports (a: calc(1 + 2)) and #{\"(c: d)\"} {x {y: z}}\n");
    assert_parity("@supports (--a: b //\n  ) {c {d: e}}\n");
}
