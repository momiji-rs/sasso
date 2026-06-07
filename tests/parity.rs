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
