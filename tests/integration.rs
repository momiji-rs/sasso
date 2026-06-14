//! Library-level golden tests.
//!
//! Every expected string here was produced by dart-sass 1.100 (expanded,
//! unless noted) and verified byte-for-byte. They run without any external
//! tool, so they gate the parser/evaluator/emitter on every `cargo test`.

use std::collections::HashMap;

use sasso::{compile, Importer, Options, OutputStyle};

fn css(input: &str) -> String {
    compile(input, &Options::default()).expect("compile should succeed")
}

fn css_compressed(input: &str) -> String {
    compile(input, &Options::default().with_style(OutputStyle::Compressed)).expect("compile should succeed")
}

/// An in-memory importer for `@import` tests.
struct MemImporter(HashMap<String, String>);

impl Importer for MemImporter {
    fn resolve(&self, path: &str) -> Option<String> {
        self.0.get(path).cloned()
    }
}

#[test]
fn variables_nesting_and_colors() {
    let out = css("$c: #336699;\n.a {\n  color: $c;\n  .b { color: lighten($c, 10%); }\n  &:hover { color: mix($c, white, 50%); }\n}\n");
    assert_eq!(
        out,
        ".a {\n  color: #336699;\n}\n.a .b {\n  color: rgb(63.75, 127.5, 191.25);\n}\n.a:hover {\n  color: rgb(153, 178.5, 204);\n}\n"
    );
}

#[test]
fn color_function_set() {
    let out = css("$brand: #2a7ae2;\n.x {\n  color: rgba($brand, 0.5);\n  background: darken($brand, 15%);\n  border-color: hsl(120, 50%, 40%);\n  width: percentage(0.25);\n}\n");
    assert_eq!(
        out,
        ".x {\n  color: rgba(42, 122, 226, 0.5);\n  background: rgb(22.9483471074, 86.2541322314, 168.5516528926);\n  border-color: hsl(120, 50%, 40%);\n  width: 25%;\n}\n"
    );
}

#[test]
fn rgb_and_hsl_literals_preserve_form() {
    assert_eq!(
        css(".x { color: rgb(51, 153, 51); }"),
        ".x {\n  color: rgb(51, 153, 51);\n}\n"
    );
    assert_eq!(
        css(".x { color: hsl(120, 50%, 40%); }"),
        ".x {\n  color: hsl(120, 50%, 40%);\n}\n"
    );
}

#[test]
fn unknown_identifiers_pass_through() {
    let out = css(".x { color: red; border-color: rebeccapurple; display: block; }");
    assert_eq!(
        out,
        ".x {\n  color: red;\n  border-color: rebeccapurple;\n  display: block;\n}\n"
    );
}

#[test]
fn nesting_combinators_and_parent_ref() {
    let out = css(".a, .b {\n  margin: 0;\n  > .c { padding: 1px; }\n  &.active { color: red; }\n  .d & { color: blue; }\n}\n");
    assert_eq!(
        out,
        ".a, .b {\n  margin: 0;\n}\n.a > .c, .b > .c {\n  padding: 1px;\n}\n.a.active, .b.active {\n  color: red;\n}\n.d .a, .d .b {\n  color: blue;\n}\n"
    );
}

#[test]
fn blank_line_between_top_level_groups() {
    // Separate top-level rules are blank-separated; a parent and its bubbled
    // children are not.
    assert_eq!(
        css(".a{color:red} .b{color:blue}"),
        ".a {\n  color: red;\n}\n\n.b {\n  color: blue;\n}\n"
    );
    assert_eq!(
        css(".a{color:red; .b{color:blue}}"),
        ".a {\n  color: red;\n}\n.a .b {\n  color: blue;\n}\n"
    );
}

#[test]
fn interpolation_in_selectors_and_values() {
    let out = css("$name: warning;\n$i: 3;\n.icon-#{$name} { content: \"#{$name}-#{$i}\"; }\n.col-#{$i} { width: 10px * $i; }\n");
    assert_eq!(
        out,
        ".icon-warning {\n  content: \"warning-3\";\n}\n\n.col-3 {\n  width: 30px;\n}\n"
    );
}

#[test]
fn unit_arithmetic() {
    assert_eq!(css(".a { width: 8px * 2; }"), ".a {\n  width: 16px;\n}\n");
    assert_eq!(css(".a { width: 10px + 5px; }"), ".a {\n  width: 15px;\n}\n");
    assert_eq!(css(".a { margin: 2 * 3em; }"), ".a {\n  margin: 6em;\n}\n");
}

#[test]
fn lists_round_trip() {
    let out = css("$stack: \"Helvetica Neue\", Arial, sans-serif;\n.t { font-family: $stack; margin: 1px 2px 3px 4px; }\n");
    assert_eq!(
        out,
        ".t {\n  font-family: \"Helvetica Neue\", Arial, sans-serif;\n  margin: 1px 2px 3px 4px;\n}\n"
    );
}

#[test]
fn default_and_important_flags() {
    // !default does not override an existing binding.
    let out = css("$c: red;\n$c: blue !default;\n.a { color: $c; background: red !important; }");
    assert_eq!(out, ".a {\n  color: red;\n  background: red !important;\n}\n");
}

#[test]
fn comments_loud_preserved_silent_dropped() {
    // A loud comment starting on the line the previous declaration ends joins
    // that line (dart-sass's trailing-comment serializer rule).
    let out = css("// silent\n.a { color: red; /* inline */ }\n/* trailing */");
    assert_eq!(out, ".a {\n  color: red; /* inline */\n}\n\n/* trailing */\n");
}

#[test]
fn null_value_omits_declaration() {
    assert_eq!(css(".a { color: null; width: 1px; }"), ".a {\n  width: 1px;\n}\n");
}

#[test]
fn import_inlining() {
    let mut files = HashMap::new();
    files.insert(
        "parts/base".to_string(),
        "$pad: 8px;\nbody { margin: 0; padding: $pad * 2; }".to_string(),
    );
    let importer = MemImporter(files);
    let out = compile(
        "@import \"parts/base\";\n.wrap { padding: 4px; }",
        &Options::default().with_importer(&importer),
    )
    .expect("compile");
    assert_eq!(
        out,
        "body {\n  margin: 0;\n  padding: 16px;\n}\n\n.wrap {\n  padding: 4px;\n}\n"
    );
}

#[test]
fn css_import_passes_through() {
    // dart-sass packs a passed-through CSS `@import` tight against the
    // following rule, with no blank-line separator.
    let out = css("@import \"https://fonts.example/x.css\";\n.a { color: red; }");
    assert_eq!(
        out,
        "@import \"https://fonts.example/x.css\";\n.a {\n  color: red;\n}\n"
    );
}

#[test]
fn preserves_css_functions_verbatim() {
    let out = css(".a { width: calc(100% - 20px); transform: translateX(10px); }");
    assert_eq!(
        out,
        ".a {\n  width: calc(100% - 20px);\n  transform: translateX(10px);\n}\n"
    );
}

#[test]
fn compressed_output() {
    let out = css_compressed(".a { color: #336699; width: 10px; .b { color: #2a7ae2; } }");
    assert_eq!(out, ".a{color:#369;width:10px}.a .b{color:#2a7ae2}");
}

#[test]
fn comparison_and_logical_operators() {
    assert_eq!(css(".a { x: if(3 > 2, big, small); }"), ".a {\n  x: big;\n}\n");
    assert_eq!(css(".a { x: 1 + 2 == 3; }"), ".a {\n  x: true;\n}\n");
    assert_eq!(css(".a { x: not false; }"), ".a {\n  x: true;\n}\n");
    assert_eq!(css(".a { x: 1 == 1px; }"), ".a {\n  x: false;\n}\n");
    assert_eq!(css(".a { x: if(true and false, y, n); }"), ".a {\n  x: n;\n}\n");
    assert_eq!(css(".a { x: if(2 <= 2 or false, y, n); }"), ".a {\n  x: y;\n}\n");
}

#[test]
fn if_function_is_lazy() {
    // The branch not taken is never evaluated — referencing an undefined
    // variable there must not error.
    assert_eq!(css(".a { x: if(true, ok, $undefined); }"), ".a {\n  x: ok;\n}\n");
    // Named arguments.
    assert_eq!(
        css(".a { x: if($condition: false, $if-true: a, $if-false: b); }"),
        ".a {\n  x: b;\n}\n"
    );
}

#[test]
fn at_if_else_chain() {
    // Inside a rule the matched branch's declarations join the block.
    assert_eq!(
        css("$t: dark;\n.a { @if $t == dark { color: white; } @else { color: black; } padding: 1px; }"),
        ".a {\n  color: white;\n  padding: 1px;\n}\n"
    );
    // @else if.
    assert_eq!(
        css("$n: 2;\n.a { @if $n == 1 { x: a; } @else if $n == 2 { x: b; } @else { x: c; } }"),
        ".a {\n  x: b;\n}\n"
    );
    // A top-level @if yields a top-level group.
    assert_eq!(css("@if 2 > 1 { .b { y: 1; } }"), ".b {\n  y: 1;\n}\n");
    // A false branch contributes nothing.
    assert_eq!(
        css(".a { @if false { x: 1; } color: red; }"),
        ".a {\n  color: red;\n}\n"
    );
}

#[test]
fn at_for_loop() {
    assert_eq!(
        css("@for $i from 1 through 3 { .c#{$i} { w: $i * 10px; } }"),
        ".c1 {\n  w: 10px;\n}\n\n.c2 {\n  w: 20px;\n}\n\n.c3 {\n  w: 30px;\n}\n"
    );
    // Exclusive `to` stops one short.
    assert_eq!(
        css("@for $i from 1 to 3 { .c#{$i} { x: $i; } }"),
        ".c1 {\n  x: 1;\n}\n\n.c2 {\n  x: 2;\n}\n"
    );
}

#[test]
fn at_each_loop() {
    assert_eq!(
        css("@each $n in a, b { .i-#{$n} { content: \"#{$n}\"; } }"),
        ".i-a {\n  content: \"a\";\n}\n\n.i-b {\n  content: \"b\";\n}\n"
    );
    // Destructuring across nested lists.
    assert_eq!(
        css("@each $k, $v in (a 1), (b 2) { .#{$k} { order: $v; } }"),
        ".a {\n  order: 1;\n}\n\n.b {\n  order: 2;\n}\n"
    );
}

#[test]
fn at_while_loop() {
    assert_eq!(
        css(".x { $i: 0; @while $i < 3 { p-#{$i}: $i; $i: $i + 1; } }"),
        ".x {\n  p-0: 0;\n  p-1: 1;\n  p-2: 2;\n}\n"
    );
}

#[test]
fn at_function_and_return() {
    assert_eq!(
        css("@function double($n) { @return $n * 2; }\n.a { width: double(8px); }"),
        ".a {\n  width: 16px;\n}\n"
    );
    // Control flow + @return, keyword args, defaults.
    assert_eq!(
        css("@function cap($v, $max: 100) { @if $v > $max { @return $max; } @return $v; }\n.a { x: cap(150); y: cap(50, $max: 60); }"),
        ".a {\n  x: 100;\n  y: 50;\n}\n"
    );
    // Rest parameter + @each accumulation.
    assert_eq!(
        css("@function sum($n...) { $t: 0; @each $x in $n { $t: $t + $x; } @return $t; }\n.a { order: sum(1, 2, 3, 4); }"),
        ".a {\n  order: 10;\n}\n"
    );
}

#[test]
fn at_mixin_include_content() {
    assert_eq!(
        css("@mixin box($pad, $color: blue) { padding: $pad; color: $color; }\n.a { @include box(4px); }\n.b { @include box(8px, red); }"),
        ".a {\n  padding: 4px;\n  color: blue;\n}\n\n.b {\n  padding: 8px;\n  color: red;\n}\n"
    );
    // @content injects the include's block into the mixin body.
    assert_eq!(
        css("@mixin surround { border: 1px; @content; margin: 0; }\n.a { @include surround { background: yellow; } }"),
        ".a {\n  border: 1px;\n  background: yellow;\n  margin: 0;\n}\n"
    );
}

#[test]
fn undefined_variable_is_an_error() {
    let err = compile(".a { color: $missing; }", &Options::default()).unwrap_err();
    assert!(err.message.contains("Undefined variable"));
}

#[test]
fn incompatible_units_error() {
    // dart-sass wording: "<a> and <b> have incompatible units." Mixing a
    // known unit (px) with an unknown/relative one (em) is incompatible.
    let err = compile(".a { width: 1px + 1em; }", &Options::default()).unwrap_err();
    assert!(err.message.contains("incompatible units"));
}

#[test]
fn hex_color_validation_matches_dart() {
    // A `#` followed by a digit is a hex color or an error — never a silent
    // hash-identifier. These all match dart-sass byte-for-byte.

    // Valid 3/4/6/8-digit forms (digit- and letter-start alike).
    assert_eq!(css("a{color:#000}"), "a {\n  color: #000;\n}\n");
    assert_eq!(css("a{color:#abc}"), "a {\n  color: #abc;\n}\n");
    assert_eq!(css("a{color:#000000}"), "a {\n  color: #000000;\n}\n");
    assert_eq!(css("a{color:#abcd12}"), "a {\n  color: #abcd12;\n}\n");
    assert_eq!(css("a{color:#0000}"), "a {\n  color: rgba(0, 0, 0, 0);\n}\n");
    assert_eq!(css("a{color:#00000000}"), "a {\n  color: rgba(0, 0, 0, 0);\n}\n");

    // A digit-start run of an invalid length (or a non-hex char before a valid
    // length) is "Expected hex digit." — sasso used to accept these verbatim.
    for bad in [
        "a{color:#0}",
        "a{color:#00}",
        "a{color:#00000}",
        "a{color:#0000000}",
        "a{color:#0g}",
        "a{color:#00g}",
        "a{color:#12g}",
    ] {
        let err = compile(bad, &Options::default()).unwrap_err();
        assert!(
            err.message.contains("Expected hex digit"),
            "{bad} should error, got {}",
            err.message
        );
    }

    // A valid digit-start color followed by a name char keeps the color and
    // leaves the rest as a trailing token (`#000g` -> `#000` + `g`).
    assert_eq!(css("a{color:#000g}"), "a {\n  color: #000 g;\n}\n");
    assert_eq!(css("a{color:#000000g}"), "a {\n  color: #000000 g;\n}\n");

    // A name-start `#` that isn't a whole valid hex is a `#…` identifier string.
    assert_eq!(css("a{color:#abcde}"), "a {\n  color: #abcde;\n}\n");
    assert_eq!(css("a{color:#abcg}"), "a {\n  color: #abcg;\n}\n");
    assert_eq!(css("a{color:#xyz}"), "a {\n  color: #xyz;\n}\n");
}

#[test]
fn rejects_lenient_parser_forms_like_dart() {
    let err = |src: &str| compile(src, &Options::default()).unwrap_err().message;

    // Duplicate @mixin/@function parameter (dart treats `-`/`_` as identical).
    assert_eq!(
        err("@mixin m($a,$a){x:$a}a{@include m(1,2)}"),
        "Duplicate parameter."
    );
    assert_eq!(
        err("@function f($a,$a){@return $a}a{x:f(1,2)}"),
        "Duplicate parameter."
    );
    assert_eq!(
        err("@mixin m($a-b,$a_b){x:$a-b}c{@include m(1,2)}"),
        "Duplicate parameter."
    );
    assert!(compile("@mixin ok($a,$b){x:$a}a{@include ok(1,2)}", &Options::default()).is_ok());

    // A committed exponent (`e` then a sign or digit) requires a digit.
    for bad in ["a{b:1e-}", "a{b:1e-x}", "a{b:1e++5}", "a{b:1e--5}"] {
        assert_eq!(err(bad), "Expected digit.", "{bad}");
    }
    assert_eq!(css("a{b:1e5}"), "a {\n  b: 100000;\n}\n");
    assert_eq!(css("a{b:1e+2}"), "a {\n  b: 100;\n}\n");
    assert_eq!(css("a{b:1em}"), "a {\n  b: 1em;\n}\n"); // `e` + letter is a unit

    // A module namespace must be a real identifier (not digit-leading).
    assert_eq!(err("@use \"sass:math\" as 0;a{b:1}"), "Expected identifier.");
    assert_eq!(err("@forward \"sass:math\" as 9-*;"), "Expected identifier.");
}

#[test]
fn rgb_hsl_argument_validation_matches_dart() {
    let err = |src: &str| compile(src, &Options::default()).unwrap_err().message;

    // Each rgb channel must be unitless or `%` (dart names the offending param).
    assert_eq!(
        err("a{b:rgb(1px,2,3)}"),
        "$red: Expected 1px to have unit \"%\" or no units."
    );
    assert_eq!(
        err("a{b:rgb(2,1px,3)}"),
        "$green: Expected 1px to have unit \"%\" or no units."
    );
    assert_eq!(
        err("a{b:rgb(1,2,3px)}"),
        "$blue: Expected 3px to have unit \"%\" or no units."
    );

    // A 2-arg comma call is the legacy `rgb($color, $alpha)` — $color must be a
    // color, so a space-list (modern channels shape) is rejected.
    assert_eq!(err("a{color:rgb(1 2 3, 0.5)}"), "$color: (1 2 3) is not a color.");
    assert_eq!(err("a{color:hsl(1 2% 3%, 0.5)}"), "Missing argument $lightness.");

    // Valid forms still compile (legacy, modern space-list, slash-alpha, var()).
    for ok in [
        "a{color:rgb(255 0 0)}",
        "a{color:rgb(1,2,3)}",
        "a{color:rgb(1 2 3 / 0.5)}",
        "a{color:hsl(120, 50%, 40%)}",
        "a{color:rgb(1, var(--foo))}",
    ] {
        assert!(compile(ok, &Options::default()).is_ok(), "{ok}");
    }
}

#[test]
fn static_placement_and_serialization_match_dart() {
    let err = |src: &str| compile(src, &Options::default()).unwrap_err().message;

    // @content only inside a @mixin declaration (caught even in dead branches).
    assert_eq!(
        err("@content;"),
        "@content is only allowed within mixin declarations."
    );
    assert_eq!(
        err("@if true{@content}"),
        "@content is only allowed within mixin declarations."
    );
    assert!(compile("@mixin m{@content}\na{@include m{x:y}}", &Options::default()).is_ok());

    // @function bodies forbid style rules / declarations / @extend statically.
    assert_eq!(
        err("@function f(){ @if false { a { color:red } } @return 1 } x{y:f()}"),
        "@function rules may not contain style rules."
    );
    assert_eq!(
        err("@function f(){ @if false { color: red } @return 1 } x{y:f()}"),
        "@function rules may not contain declarations."
    );

    // @extend must be lexically within a style rule (dead branches caught too).
    assert_eq!(
        err("@if false { @extend .foo; }"),
        "@extend may only be used within style rules."
    );
    assert!(compile("a{x:1}b{@extend a}", &Options::default()).is_ok());

    // A map or empty list is not a valid CSS value in any serialization context.
    assert_eq!(err("a{b: -(a:1)}"), "(a: 1) isn't a valid CSS value.");
    assert_eq!(err("a{b: #{(a:1)}}"), "(a: 1) isn't a valid CSS value.");
    assert_eq!(err("a{b: #{()}}"), "() isn't a valid CSS value.");
    assert_eq!(err("a{b: 1 + ()}"), "() isn't a valid CSS value.");
    assert_eq!(css("a{b: #{1 2 3}}"), "a {\n  b: 1 2 3;\n}\n"); // a non-empty list is fine
}

#[test]
fn selector_pseudo_grammar_matches_dart() {
    let err = |src: &str| compile(src, &Options::default()).unwrap_err().message;

    // Empty/garbage functional-pseudo and An+B arguments, and bare colon runs.
    assert_eq!(err("a:not(){x:y}"), "expected selector.");
    assert_eq!(err("a:nth-child(2n+3 foo){x:y}"), "Expected \"of\".");
    assert_eq!(err("a:::before{x:y}"), "Expected identifier.");
    for bad in [
        "a:nth-child(2n+){x:y}",
        "a:nth-child(of){x:y}",
        "a:nth-child(2x){x:y}",
    ] {
        assert!(compile(bad, &Options::default()).is_err(), "{bad}");
    }

    // Valid pseudos / An+B / interpolation / unknown-pseudo args still compile.
    for ok in [
        "a:nth-child(2n+1){x:y}",
        "a:nth-child(odd){x:y}",
        "a:nth-child(-n+3){x:y}",
        "a:nth-child(2n of .a){x:y}",
        "a:not(.a, .b){x:y}",
        "a:is(h1, h2){x:y}",
        "a:has(> .x){x:y}",
        "a::before{x:y}",
        "a:lang(en){x:y}",
        "$n: 3;\na:nth-child(#{$n}){x:y}",
        "a:nth-of-type(){x:y}", // dart accepts this; sasso no longer over-rejects
    ] {
        assert!(compile(ok, &Options::default()).is_ok(), "{ok}");
    }
}

#[test]
fn selector_bang_and_extend_leading_comma_match_dart() {
    let err = |src: &str| compile(src, &Options::default()).unwrap_err().message;

    // A top-level `!` is not valid in a selector — dart stops there and fails
    // to find the `{`. A `!` inside an attribute value or string is fine.
    assert_eq!(err("a !important {b:c}"), "expected \"{\".");
    assert_eq!(err("div !default {color:red}"), "expected \"{\".");
    assert!(compile("[data-x=\"a!b\"]{c:d}", &Options::default()).is_ok());
    assert_eq!(
        css("a{color:red !important}"),
        "a {\n  color: red !important;\n}\n"
    );

    // @extend rejects a leading empty component but allows a trailing comma.
    assert_eq!(err("a{x:1}.x{@extend ,a}"), "expected selector.");
    assert!(compile("a{x:1}.x{@extend a,}", &Options::default()).is_ok());
}

#[test]
fn at_charset_and_at_root_query_match_dart() {
    let err = |src: &str| compile(src, &Options::default()).unwrap_err().message;

    // @charset takes exactly one quoted string.
    assert_eq!(err("@charset utf-8;a{b:1}"), "Expected string.");
    assert_eq!(err("@charset;a{b:1}"), "Expected string.");
    assert!(compile("@charset \"utf-8\";a{b:1}", &Options::default()).is_ok());
    assert!(compile("@charset \"utf-8\" \"extra\";a{b:1}", &Options::default()).is_err());

    // @at-root (...) query grammar: with|without : <expr>.
    assert_eq!(
        err("@at-root (foo) {a{b:c}}"),
        "Expected \"with\" or \"without\"."
    );
    assert_eq!(err("@at-root (with) {a{b:c}}"), "expected \":\".");
    assert_eq!(err("@at-root (with:) {a{b:c}}"), "Expected expression.");
    assert_eq!(err("@at-root (with: rule) junk {a{b:c}}"), "expected \"{\".");
    for ok in [
        "@at-root (with: rule) {a{b:c}}",
        "@at-root (without: media) {a{b:c}}",
        "@at-root (with: a b) {a{b:c}}",
        "@at-root {a{b:c}}",
        "@at-root .x {a{b:c}}",
    ] {
        assert!(compile(ok, &Options::default()).is_ok(), "{ok}");
    }
}

// --- scoped-arena escape safety (perf #5) ----------------------------------
//
// `compile` brackets its work in a bump-arena scope (when `ScopedAlloc` is the
// global allocator) and resets the arena on return. A caller's `Importer` runs
// inside that scope, so if it stashed the passed `&str` path or otherwise kept
// allocations made during the call, those would dangle after the reset. The
// evaluator therefore `pause()`s the arena around each importer callback so the
// importer's own allocations go to the system allocator and survive the compile.
//
// This integration test exercises that boundary: a caching importer copies every
// requested path into a `RefCell<Vec<String>>` *it owns* (a `path.to_string()`
// — an allocation made during the importer callback). After `compile` returns we
// assert those cached strings are still readable and correct. Under `ScopedAlloc`
// this proves they were NOT arena-allocated (an arena allocation would have been
// reclaimed by the post-compile reset); under the default allocator it is still a
// useful correctness regression guard for the pause/resume wiring.

use std::cell::RefCell;

/// An importer that caches every path it is asked to resolve, owning the cached
/// `String`s itself. Serves both `@import` and `@use`/`@forward`.
struct CachingImporter {
    files: HashMap<String, String>,
    /// Paths requested, copied into importer-owned storage during the callback.
    requested: RefCell<Vec<String>>,
}

impl Importer for CachingImporter {
    fn resolve(&self, path: &str) -> Option<String> {
        // Record the request in importer-owned state. This allocation happens
        // *inside* the importer callback; the pause/resume boundary must keep it
        // on the system allocator so it outlives the compile's arena reset.
        self.requested.borrow_mut().push(path.to_string());
        self.files.get(path).cloned()
    }

    fn resolve_module(&self, path: &str) -> Option<(String, String)> {
        self.requested.borrow_mut().push(path.to_string());
        self.files.get(path).map(|src| (path.to_string(), src.clone()))
    }
}

#[test]
fn importer_cached_strings_survive_compile_reset() {
    let mut files = HashMap::new();
    files.insert(
        "partial".to_string(),
        "$pad: 8px;\nbody { padding: $pad; }".to_string(),
    );
    files.insert("mod".to_string(), "$gap: 4px;".to_string());
    let importer = CachingImporter {
        files,
        requested: RefCell::new(Vec::new()),
    };

    // Drive both importer entry points: `@use` -> resolve_module_with_syntax,
    // `@import` -> resolve_with_syntax.
    let out = compile(
        "@use \"mod\";\n@import \"partial\";\n.a { margin: mod.$gap; }",
        &Options::default().with_importer(&importer),
    )
    .expect("compile should succeed");

    assert_eq!(out, "body {\n  padding: 8px;\n}\n\n.a {\n  margin: 4px;\n}\n");

    // After the compile returns (and, under ScopedAlloc, the arena has been
    // reset) the importer-owned cache must still be intact and correct. If the
    // `path.to_string()` allocations had landed in the arena, this would read
    // freed/reused memory.
    let requested = importer.requested.borrow();
    assert!(
        requested.iter().any(|p| p == "mod"),
        "expected `mod` to have been requested; got {requested:?}"
    );
    assert!(
        requested.iter().any(|p| p == "partial"),
        "expected `partial` to have been requested; got {requested:?}"
    );
    // Every cached string is still valid UTF-8 with its original content.
    for p in requested.iter() {
        assert!(!p.is_empty());
        assert!(p == "mod" || p == "partial", "unexpected cached path {p:?}");
    }
}

#[test]
fn default_assignment_does_not_eval_rhs_when_already_set() {
    // A guarded (`!default`) declaration whose target already holds a non-null
    // value must NOT evaluate its right-hand side. dart-sass short-circuits
    // before evaluation, so an otherwise-erroring expression is fine here.
    // This mirrors Bootstrap's `$form-check-padding-start: $w + .5em !default`
    // after a Shopware-style override sets `$w: 1rem` and the var to `1.5rem`,
    // where `1rem + .5em` would be an "incompatible units" error if evaluated.
    let out = css(concat!(
        "$w: 1rem !default;\n",
        "$p: 1.5rem !default;\n",
        "$w: 1em !default;\n",
        "$p: $w + .5em !default;\n",
        ".a { width: $w; padding: $p; }\n",
    ));
    assert_eq!(out, ".a {\n  width: 1rem;\n  padding: 1.5rem;\n}\n");
}
