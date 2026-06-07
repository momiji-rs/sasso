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
    let out = css("// silent\n.a { color: red; /* inline */ }\n/* trailing */");
    assert_eq!(out, ".a {\n  color: red;\n  /* inline */\n}\n\n/* trailing */\n");
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
    let out = css("@import \"https://fonts.example/x.css\";\n.a { color: red; }");
    assert_eq!(
        out,
        "@import \"https://fonts.example/x.css\";\n\n.a {\n  color: red;\n}\n"
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
