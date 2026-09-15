//! Library-level golden tests.
//!
//! Every expected string here was produced by dart-sass 1.100 (expanded,
//! unless noted) and verified byte-for-byte. They run without any external
//! tool, so they gate the parser/evaluator/emitter on every `cargo test`.

use std::collections::HashMap;

use sasso::{
    compile, CanonicalUrl, CanonicalizeContext, Importer, ImporterError, ImporterResult, Options,
    OutputStyle, Syntax,
};

// `compile` returns the stylesheet WITHOUT a trailing newline (byte-for-byte
// what dart-sass's library API returns). The expanded goldens below were
// captured from the dart-sass CLI, which appends one to NON-empty output (empty
// output stays empty), so mirror that here. The library's no-trailing-newline
// contract is pinned by `library_api_omits_trailing_newline`.
fn css(input: &str) -> String {
    let out = compile(input, &Options::default()).expect("compile should succeed");
    if out.is_empty() {
        out
    } else {
        out + "\n"
    }
}

// Compressed goldens were captured from the library API directly (no trailing
// newline), so this helper compares against `compile` verbatim.
fn css_compressed(input: &str) -> String {
    compile(input, &Options::default().with_style(OutputStyle::Compressed)).expect("compile should succeed")
}

/// The library API (`compile`) must not emit a trailing newline in either
/// style — that matches dart-sass's library API (`compileString().css`) and is
/// what the wasm `compileString().css` and the Ruby gem return. The single
/// newline the dart-sass CLI appends is re-added only by the CLI front-ends
/// (`src/main.rs`, `wasm/npm/cli.mjs`), guarded by the CLI tests.
#[test]
fn library_api_omits_trailing_newline() {
    for input in [
        ".a { color: red; }",
        ".a { color: red; }\n",
        "$c: blue;\n.a {\n  color: $c;\n  .b { width: 10px; }\n}\n",
        ".a { color: red; }\n.b { color: blue; }\n",
        // non-ASCII -> expanded prepends `@charset`, which must not reintroduce
        // a trailing newline at the end.
        ".a { content: \"café\"; }",
    ] {
        let expanded = compile(input, &Options::default()).expect("compile");
        assert!(
            !expanded.ends_with('\n'),
            "expanded library output must not end with a newline: {expanded:?}"
        );
        let compressed =
            compile(input, &Options::default().with_style(OutputStyle::Compressed)).expect("compile");
        assert!(
            !compressed.ends_with('\n'),
            "compressed library output must not end with a newline: {compressed:?}"
        );
    }
}

/// An in-memory importer for `@import` tests.
struct MemImporter(HashMap<String, String>);

impl Importer for MemImporter {
    fn canonicalize(
        &self,
        url: &str,
        _ctx: &CanonicalizeContext<'_>,
    ) -> Result<Option<CanonicalUrl>, ImporterError> {
        Ok(self.0.contains_key(url).then(|| CanonicalUrl::new(url)))
    }

    fn load(&self, canonical: &CanonicalUrl) -> Result<Option<ImporterResult>, ImporterError> {
        Ok(self.0.get(canonical.as_str()).map(|c| ImporterResult {
            contents: c.clone(),
            syntax: Syntax::Scss,
            source_map_url: None,
        }))
    }
}

#[test]
fn variables_nesting_and_colors() {
    let out = css("$c: #336699;\n.a {\n  color: $c;\n  .b { color: lighten($c, 10%); }\n  &:hover { color: mix($c, white, 50%); }\n}\n");
    assert_eq!(
        out,
        ".a {\n  color: #336699;\n}\n.a .b {\n  color: rgb(25%, 50%, 75%);\n}\n.a:hover {\n  color: rgb(60%, 70%, 80%);\n}\n"
    );
}

#[test]
fn color_function_set() {
    let out = css("$brand: #2a7ae2;\n.x {\n  color: rgba($brand, 0.5);\n  background: darken($brand, 15%);\n  border-color: hsl(120, 50%, 40%);\n  width: percentage(0.25);\n}\n");
    assert_eq!(
        out,
        ".x {\n  color: rgba(42, 122, 226, 0.5);\n  background: rgb(8.9993518068%, 33.8251498947%, 66.0986874088%);\n  border-color: hsl(120, 50%, 40%);\n  width: 25%;\n}\n"
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
        "body {\n  margin: 0;\n  padding: 16px;\n}\n\n.wrap {\n  padding: 4px;\n}"
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

/// A directory-relative in-memory importer: resolves a relative URL against the
/// directory of `ctx.containing_url` (the root for the entry), with dart's
/// partial (`_name`) fallback. Unlike [`MemImporter`] (which keys by the
/// verbatim URL) this models the *directory-relative* resolution that
/// `containing_url` drives — the axis exercised by issue #8.
struct DirImporter(HashMap<String, String>);

impl DirImporter {
    fn dirname(p: &str) -> &str {
        match p.rfind('/') {
            Some(0) => "/",
            Some(i) => &p[..i],
            None => "",
        }
    }
    /// The partial spelling: `/sub/mod` -> `/sub/_mod`, `/dep` -> `/_dep`.
    fn as_partial(p: &str) -> String {
        match p.rfind('/') {
            Some(i) => format!("{}/_{}", &p[..i], &p[i + 1..]),
            None => format!("_{p}"),
        }
    }
}

impl Importer for DirImporter {
    fn canonicalize(
        &self,
        url: &str,
        ctx: &CanonicalizeContext<'_>,
    ) -> Result<Option<CanonicalUrl>, ImporterError> {
        let base = ctx
            .containing_url
            .map(|c| Self::dirname(c.as_str()))
            .unwrap_or("");
        let joined = format!("{}/{}", base.trim_end_matches('/'), url);
        for cand in [joined.clone(), Self::as_partial(&joined)] {
            if self.0.contains_key(&cand) {
                return Ok(Some(CanonicalUrl::new(&cand)));
            }
        }
        Ok(None)
    }

    fn load(&self, canonical: &CanonicalUrl) -> Result<Option<ImporterResult>, ImporterError> {
        Ok(self.0.get(canonical.as_str()).map(|c| ImporterResult {
            contents: c.clone(),
            syntax: Syntax::Scss,
            source_map_url: None,
        }))
    }
}

#[test]
fn first_class_mixin_load_css_resolves_against_defining_module() {
    // Regression test for issue #8: a relative `meta.load-css` inside a mixin
    // captured via `meta.get-mixin` and invoked with `meta.apply` from another
    // file must resolve the URL against the *defining* module's directory, not
    // the caller's. Two `_dep` partials exist — `/sub/_dep` (correct, next to
    // the mixin) and `/_dep` (wrong, next to the entry) — so a caller-relative
    // resolution picks the wrong file and is observable in the output.
    let mut files = HashMap::new();
    files.insert(
        "/sub/_mod".to_string(),
        "@use \"sass:meta\";\n@mixin go { @include meta.load-css(\"dep\"); }\n\
         $m: meta.get-mixin(\"go\");\n"
            .to_string(),
    );
    files.insert(
        "/sub/_dep".to_string(),
        ".loaded { x: from-sub-dep; }\n".to_string(),
    );
    files.insert(
        "/_dep".to_string(),
        ".loaded { x: from-ROOT-WRONG; }\n".to_string(),
    );
    let importer = DirImporter(files);
    let out = compile(
        "@use \"sub/mod\";\n@use \"sass:meta\";\n.out { @include meta.apply(mod.$m); }\n",
        &Options::default().with_url("/entry").with_importer(&importer),
    )
    .expect("compile");
    // Byte-for-byte dart-sass 1.101 (resolves "dep" against /sub). The library
    // API omits the trailing newline the CLI adds.
    assert_eq!(out, ".out .loaded {\n  x: from-sub-dep;\n}");
}

#[test]
fn compressed_output() {
    let out = css_compressed(".a { color: #336699; width: 10px; .b { color: #2a7ae2; } }");
    assert_eq!(out, ".a{color:#369;width:10px}.a .b{color:#2a7ae2}");
}

/// Compressed legacy colors are emitted in the SHORTEST equivalent form
/// (hex / name / rgb() / hsl()), matching dart-sass 1.101.0. Every expected
/// string below was produced by dart-sass 1.101.0 (`--style=compressed`).
/// This is the offline regression gate for the color-serialization fix; the
/// live cross-check lives in tests/parity.rs (compressed parity battery).
#[test]
fn compressed_color_picks_shortest_form() {
    let case = |scss: &str| css_compressed(&format!("a{{x:{scss}}}"));

    // --- fractional triples: percentages, with hsl only when it wins by more
    // --- than the two-character handicap dart gives it -----------------------
    assert_eq!(case("darken(#336699,10%)"), "a{x:rgb(15%,30%,45%)}");
    assert_eq!(case("lighten(#336699,10%)"), "a{x:rgb(25%,50%,75%)}");
    assert_eq!(case("saturate(#336699,10%)"), "a{x:rgb(16%,40%,64%)}");
    assert_eq!(case("grayscale(#ff6600)"), "a{x:hsl(0,0%,50%)}");
    assert_eq!(
        css_compressed("@use 'sass:color';a{x:color.mix(#ff6600,#fff,30%)}"),
        "a{x:rgb(100%,82%,70%)}"
    );
    assert_eq!(
        css_compressed("@use 'sass:color';a{x:color.adjust(#336699,$lightness:-10%)}"),
        "a{x:rgb(15%,30%,45%)}"
    );
    // Non-opaque: rgba() vs hsla(); the percentage rgba form wins.
    assert_eq!(case("rgba(darken(#336699,10%),.5)"), "a{x:rgba(15%,30%,45%,.5)}");

    // --- results whose rgb form is shorter (or equal) -> stays rgb/hex ------
    // The percentage form wins under dart's two-character hsl handicap.
    assert_eq!(case("saturate(#888,20%)"), "a{x:rgb(62.6666666667%,44%,44%)}");
    // A hue rotation that lands on integers collapses to hex.
    assert_eq!(case("adjust-hue(#336699,90deg)"), "a{x:#939}");

    // --- hsl-space literals also pick the shortest form --------------------
    // Integer-rgb-equivalent hsl collapses to hex.
    assert_eq!(case("hsl(210,50%,40%)"), "a{x:#369}");
    // A fractional-rgb hsl literal serializes through rgb like every legacy
    // color (dart `_writeLegacyColor`): the percent rgb form wins under the
    // two-character hsl handicap.
    assert_eq!(case("hsl(210,50%,30%)"), "a{x:rgb(15%,30%,45%)}");
    assert_eq!(case("hsl(30,100%,50%)"), "a{x:rgb(100%,50%,0%)}");
    // A powerless (zero-saturation) hue collapses to the rgb round-trip's 0:
    // compressed serialization derives the hsl candidate from the rgb triple,
    // so the authored hue does not participate.
    assert_eq!(case("hsl(30,0%,50%)"), "a{x:hsl(0,0%,50%)}");

    // --- regression guards: unchanged cases -------------------------------
    assert_eq!(case("#336699"), "a{x:#369}");
    assert_eq!(case("red"), "a{x:red}");
    assert_eq!(case("rgba(0,0,0,.5)"), "a{x:rgba(0,0,0,.5)}");
    assert_eq!(case("hsl(0,0%,50%)"), "a{x:hsl(0,0%,50%)}");
}

/// Compressed style drops a leading zero from a POSITIVE number only — dart
/// looks for a literal `0.` prefix on the rendered string, which a minus sign
/// has already pushed out of the way. Measured against dart-sass 1.103.1.
#[test]
fn compressed_keeps_the_zero_on_a_negative_decimal() {
    let v = |scss: &str| css_compressed(&format!("a{{x:{scss}}}"));
    assert_eq!(v("0.5px"), "a{x:.5px}");
    assert_eq!(v("-0.5px"), "a{x:-0.5px}");
    assert_eq!(v("-0.25%"), "a{x:-0.25%}");
    assert_eq!(v("-0.5"), "a{x:-0.5}");
    // Computed, not just written that way.
    assert_eq!(v("-1px * 0.1"), "a{x:-0.1px}");
    assert_eq!(v("1px -0.5px"), "a{x:1px -0.5px}");
    // Zero itself has no fraction to shorten, either way round.
    assert_eq!(v("0px"), "a{x:0px}");
    assert_eq!(v("-0.0px"), "a{x:0px}");
}

/// Compressed style drops comments — except the LOUD ones, which open `/*!`
/// and are how a stylesheet keeps its licence header. Measured against
/// dart-sass 1.103.1.
#[test]
fn compressed_keeps_loud_comments() {
    // Written verbatim, newlines and all, with no separator of its own.
    assert_eq!(css_compressed("/*! head */\n.a { b: 1; }"), "/*! head */.a{b:1}");
    assert_eq!(
        css_compressed("/*!\n * line\n */\n.a { b: 1; }"),
        "/*!\n * line\n */.a{b:1}"
    );
    assert_eq!(
        css_compressed("/*! one */\n/*! two */\n.a { b: 1; }"),
        "/*! one *//*! two */.a{b:1}"
    );
    assert_eq!(css_compressed(".a { b: 1; }\n/*! tail */"), ".a{b:1}/*! tail */");
    assert_eq!(
        css_compressed(".a { b: 1; }\n/*! mid */\n.c { d: 1; }"),
        ".a{b:1}/*! mid */.c{d:1}"
    );
    // Inside a rule it takes the pending `;` and needs none of its own.
    assert_eq!(
        css_compressed(".a { b: 1; /*! c */ d: 2; }"),
        ".a{b:1;/*! c */d:2}"
    );
    assert_eq!(css_compressed(".a { /*! c */ b: 1; }"), ".a{/*! c */b:1}");
    assert_eq!(css_compressed(".a { b: 1; /*! c */ }"), ".a{b:1;/*! c */}");
    // A rule that holds nothing else is still emitted around it — but one
    // holding only a QUIET comment is not.
    assert_eq!(css_compressed(".a { /*! c */ }"), ".a{/*! c */}");
    assert_eq!(css_compressed(".a { /* c */ }"), "");
    assert_eq!(
        css_compressed(".a { /*! c */ .b { d: 1; } }"),
        ".a{/*! c */}.a .b{d:1}"
    );
    // And inside an at-rule body.
    assert_eq!(
        css_compressed("@media (a: 1) { /*! c */ .a { b: 1; } }"),
        "@media(a: 1){/*! c */.a{b:1}}"
    );
    // Interpolation resolves first, as in expanded output.
    assert_eq!(
        css_compressed("$x: 1;\n/*! v#{$x} */\n.a { b: 1; }"),
        "/*! v1 */.a{b:1}"
    );
}

/// A CSS escape is a TOKEN, and the whitespace that terminates a numeric one
/// belongs to it: `.\31  .b` is the class `1` and then a descendant combinator,
/// so compressing either space away changes which selector it is. Measured
/// against dart-sass 1.103.1.
#[test]
fn compressed_selectors_keep_an_escapes_terminator() {
    let sel = |scss: &str| css_compressed(&format!("{scss}{{a:1}}"));
    // The terminator survives; the structural space beside it is what goes.
    assert_eq!(sel(".\\31  > .b"), ".\\31 >.b{a:1}");
    assert_eq!(sel(".\\31  .b"), ".\\31  .b{a:1}");
    assert_eq!(sel(".\\31 .b"), ".\\31 .b{a:1}");
    assert_eq!(sel(".\\31 "), ".\\31 {a:1}");
    // Including at the end of a selector-list component, where trimming the
    // part would have eaten it.
    assert_eq!(sel(":not(.\\31 , .b)"), ":not(.\\31 ,.b){a:1}");
    assert_eq!(sel(":not(.a, .\\31 )"), ":not(.a,.\\31 ){a:1}");
    assert_eq!(sel(":is(.\\31 , .b) > .c"), ":is(.\\31 ,.b)>.c{a:1}");
    // A non-hex escape is one character and carries no terminator.
    assert_eq!(sel(".a\\ b > .c"), ".a\\ b>.c{a:1}");
    assert_eq!(sel(".a\\9 b .c"), ".a\\9 b .c{a:1}");
    // A hex digit after the terminator still belongs to the next token.
    assert_eq!(sel(".\\31 a .b"), ".\\31 a .b{a:1}");
}

/// `::slotted()` takes a selector list like `:not()` and friends — and the
/// dispatch is CASE-SENSITIVE, which is dart's own behaviour: `:NOT(.a, .b)`
/// keeps its comma space. Both measured against dart-sass 1.103.1.
#[test]
fn compressed_selector_pseudo_dispatch_matches_dart() {
    let sel = |scss: &str| css_compressed(&format!("{scss}{{a:1}}"));
    assert_eq!(sel("::slotted(.b, .c)"), "::slotted(.b,.c){a:1}");
    assert_eq!(sel(".x:-moz-any(.b, .c)"), ".x:-moz-any(.b,.c){a:1}");
    // dart compares the unvendored name verbatim, so an upper-case spelling is
    // opaque to it and keeps the space. Mirrored, not tidied.
    assert_eq!(sel(".x:NOT(.b, .c)"), ".x:NOT(.b, .c){a:1}");
    assert_eq!(sel(".x:Where(.b, .c)"), ".x:Where(.b, .c){a:1}");
    // An opaque argument keeps its space whatever the case.
    assert_eq!(sel(".x:LANG(en, fr)"), ".x:LANG(en, fr){a:1}");
}

/// A comment is loud by what it SAYS, not by how it was spelled: dart resolves
/// interpolation first, so `/*#{"!"} x */` is kept when compressing. Measured
/// against dart-sass 1.103.1.
#[test]
fn compressed_loudness_is_decided_after_interpolation() {
    assert_eq!(
        css_compressed("/*#{\"!\"} normal */\n.a { b: 1; }"),
        "/*! normal */.a{b:1}"
    );
    // A `!` that is not the first character is not loud.
    assert_eq!(css_compressed("/* !late */\n.a { b: 1; }"), ".a{b:1}");
}

/// The CSS `@import` writes no space before its url when compressing, and a
/// `url(…)` wrapper is unwrapped to save its four bytes. The modifiers keep the
/// spaces they hold between themselves. Measured against dart-sass 1.103.1.
#[test]
fn compressed_css_import_loses_its_prelude_space() {
    assert_eq!(
        css_compressed("@import \"x.css\";\n.a { b: 1; }"),
        "@import\"x.css\";.a{b:1}"
    );
    assert_eq!(
        css_compressed("@import url(x.css);\n.a { b: 1; }"),
        "@import\"x.css\";.a{b:1}"
    );
    assert_eq!(
        css_compressed("@import url(\"x.css\");\n.a { b: 1; }"),
        "@import\"x.css\";.a{b:1}"
    );
    // A plain quoted url is written exactly as it was spelled.
    assert_eq!(
        css_compressed("@import 'x.css';\n.a { b: 1; }"),
        "@import'x.css';.a{b:1}"
    );
    // Only the ONE separator before the modifiers goes.
    assert_eq!(
        css_compressed("@import \"x.css\" screen, print;\n.a { b: 1; }"),
        "@import\"x.css\"screen, print;.a{b:1}"
    );
    assert_eq!(
        css_compressed("@import \"x.css\" layer(a) supports(display: grid) screen;\n.a { b: 1; }"),
        "@import\"x.css\"layer(a) supports(display: grid) screen;.a{b:1}"
    );
    // Nested in a rule and in an at-rule body.
    assert_eq!(
        css_compressed(".a { @import url(x.css); }"),
        ".a{@import\"x.css\"}"
    );
    assert_eq!(
        css_compressed("@media screen { @import \"x.css\"; }"),
        "@media screen{@import\"x.css\"}"
    );
    // Expanded output is untouched: the source form survives.
    let expanded = |scss: &str| compile(scss, &Options::default()).expect("compile");
    assert_eq!(
        expanded("@import url(x.css);\n.a { b: 1; }"),
        "@import url(x.css);\n.a {\n  b: 1;\n}"
    );
}

/// A `@supports` DECLARATION is not a value: dart writes its calculations
/// verbatim, spaces and all, in both styles — `calc-size` included. Measured
/// against dart-sass 1.103.1.
#[test]
fn compressed_supports_declarations_keep_their_calculation_spaces() {
    for (scss, want) in [
        (
            "@supports (width: calc-size(auto, var(--y))) { .a { b: 1; } }",
            "@supports(width: calc-size(auto, var(--y))){.a{b:1}}",
        ),
        (
            "@supports (width: clamp(1px, var(--y), 2px)) { .a { b: 1; } }",
            "@supports(width: clamp(1px, var(--y), 2px)){.a{b:1}}",
        ),
        (
            "@supports (width: min(1px, var(--y))) { .a { b: 1; } }",
            "@supports(width: min(1px, var(--y))){.a{b:1}}",
        ),
    ] {
        assert_eq!(css_compressed(scss), want, "{scss}");
    }
    // The same calculations as VALUES do lose the space.
    assert_eq!(
        css_compressed(".a { b: calc-size(auto, var(--y)); }"),
        ".a{b:calc-size(auto,var(--y))}"
    );
}

/// A value is verbatim text and can end in a `;` of its own, which dart keeps
/// — so what may be dropped is decided by the NODE that wrote the last byte,
/// never by the byte. Measured against dart-sass 1.103.1.
#[test]
fn compressed_keeps_a_semicolon_that_belongs_to_a_value() {
    let v = "$v: \";\";\n";
    assert_eq!(
        css_compressed(&format!("{v}.a {{ --x: #{{$v}}; }}")),
        ".a{--x: ;}"
    );
    assert_eq!(
        css_compressed(&format!("{v}@font-face {{ --x: #{{$v}}; }}")),
        "@font-face{--x: ;}"
    );
    assert_eq!(
        css_compressed(&format!("{v}@font-face {{ src: #{{$v}}; }}")),
        "@font-face{src:;}"
    );
    assert_eq!(
        css_compressed(&format!("{v}@font-face {{ a: 1; --x: #{{$v}}; }}")),
        "@font-face{a:1;--x: ;}"
    );
    assert_eq!(
        css_compressed(&format!("{v}@media a {{ @font-face {{ --x: #{{$v}}; }} }}")),
        "@media a{@font-face{--x: ;}}"
    );
}

/// dart writes a statement's `;` as a SEPARATOR, so compressed output never
/// ends with one — at the end of the stylesheet or before a `}`. Measured
/// against dart-sass 1.103.1.
#[test]
fn compressed_output_never_ends_with_a_semicolon() {
    assert_eq!(css_compressed("@import \"x.css\";"), "@import\"x.css\"");
    assert_eq!(
        css_compressed("@import \"x.css\" screen;"),
        "@import\"x.css\"screen"
    );
    assert_eq!(css_compressed("@namespace \"x\";"), "@namespace \"x\"");
    assert_eq!(css_compressed("@unknown foo;"), "@unknown foo");
    assert_eq!(
        css_compressed("@media a { @unknown foo; }"),
        "@media a{@unknown foo}"
    );
    assert_eq!(css_compressed(".a { b: 1; }"), ".a{b:1}");
    assert_eq!(
        css_compressed("@font-face { src: url(x); }"),
        "@font-face{src:url(x)}"
    );
}

/// A private-use character is escaped in expanded output and written RAW when
/// compressing — dart trades the escape for the character once bytes are what
/// matter. Measured against dart-sass 1.103.1.
#[test]
fn compressed_writes_private_use_characters_raw() {
    let expanded = |scss: &str| compile(scss, &Options::default()).expect("compile");
    // U+E028 is private use: escaped when expanded, raw when compressed — and
    // the raw character makes the output non-ASCII, which brings the BOM that
    // compressed style writes in place of `@charset`.
    assert_eq!(
        expanded(".a::before { content: \"\\e028\"; }"),
        ".a::before {\n  content: \"\\e028\";\n}"
    );
    assert_eq!(
        css_compressed(".a::before { content: \"\\e028\"; }"),
        "\u{feff}.a::before{content:\"\u{e028}\"}"
    );
    // Written raw in the source, it comes out the same way round.
    assert_eq!(
        expanded(".a::before { content: \"\u{e028}\"; }"),
        ".a::before {\n  content: \"\\e028\";\n}"
    );
    assert_eq!(
        css_compressed(".a::before { content: \"\u{e028}\"; }"),
        "\u{feff}.a::before{content:\"\u{e028}\"}"
    );
    // Both edges of the BMP range, and the SUPPLEMENTARY private-use planes
    // (U+F0000-U+10FFFF), whose characters are four UTF-8 bytes rather than
    // three — the branch the escape used to hide.
    assert_eq!(
        css_compressed(".a::before { content: \"\\e000\"; }"),
        "\u{feff}.a::before{content:\"\u{e000}\"}"
    );
    assert_eq!(
        css_compressed(".a::before { content: \"\\f8ff\"; }"),
        "\u{feff}.a::before{content:\"\u{f8ff}\"}"
    );
    assert_eq!(
        css_compressed(".a::before { content: \"\\f0000\"; }"),
        "\u{feff}.a::before{content:\"\u{f0000}\"}"
    );
    assert_eq!(
        css_compressed(".a::before { content: \"\\10fffd\"; }"),
        "\u{feff}.a::before{content:\"\u{10fffd}\"}"
    );
    // A character that is NOT private use is raw in both styles already, and a
    // control character stays escaped in both.
    assert_eq!(
        css_compressed(".a::before { content: \"\\4e2d\"; }"),
        "\u{feff}.a::before{content:\"\u{4e2d}\"}"
    );
    assert_eq!(
        css_compressed(".a::before { content: \"\\1\"; }"),
        ".a::before{content:\"\\1\"}"
    );
    // `inspect` and error messages keep the escape whatever the style, because
    // they are not CSS output.
    assert_eq!(
        css_compressed(".a { b: inspect(\"\\e028\"); }"),
        ".a{b:\"\\e028\"}"
    );
}

/// Compressed style drops the whitespace AROUND A COMBINATOR and the space
/// after a SELECTOR LIST's comma — and nothing else. Every expectation below
/// was measured against dart-sass 1.103.1 (`--style=compressed`).
#[test]
fn compressed_selectors_lose_only_structural_whitespace() {
    let sel = |scss: &str| css_compressed(&format!("{scss}{{a:1}}"));
    // The three combinators, on both sides.
    assert_eq!(sel(".a > .b"), ".a>.b{a:1}");
    assert_eq!(sel(".a + .b"), ".a+.b{a:1}");
    assert_eq!(sel(".a ~ .b"), ".a~.b{a:1}");
    assert_eq!(sel(".a .b > .c + .d ~ .e"), ".a .b>.c+.d~.e{a:1}");
    assert_eq!(sel("* > *"), "*>*{a:1}");
    // A descendant combinator IS a space; it stays.
    assert_eq!(sel(".a .b"), ".a .b{a:1}");
    // A combinator that opens a relative selector loses its trailing space.
    assert_eq!(sel(":has(+ .b)"), ":has(+.b){a:1}");
    assert_eq!(sel(":has(> .a, + .b)"), ":has(>.a,+.b){a:1}");
    // A SELECTOR-list comma loses its space; an opaque argument keeps it.
    assert_eq!(sel(":not(.b, .c)"), ":not(.b,.c){a:1}");
    assert_eq!(sel(":where(.a, .b) .c"), ":where(.a,.b) .c{a:1}");
    assert_eq!(sel(":is(:not(.a, .b), .c) > .d"), ":is(:not(.a,.b),.c)>.d{a:1}");
    assert_eq!(sel(":host-context(.a, .b)"), ":host-context(.a,.b){a:1}");
    assert_eq!(sel(":lang(en, fr)"), ":lang(en, fr){a:1}");
    // `:nth-child()` carries an An+B, and only its `of` tail is a list.
    assert_eq!(sel(":nth-child(2n + 1)"), ":nth-child(2n+1){a:1}");
    assert_eq!(
        sel(":nth-child(2n + 1 of .a, .b)"),
        ":nth-child(2n+1 of .a,.b){a:1}"
    );
    // Quoted and escaped text is not selector structure.
    assert_eq!(sel("[a=\"x > y\"]"), "[a=\"x > y\"]{a:1}");
    assert_eq!(sel(":not([a=\"x, y\"], .b)"), ":not([a=\"x, y\"],.b){a:1}");
    assert_eq!(sel(".a\\+b"), ".a\\+b{a:1}");
    assert_eq!(sel(".a\\:b > .c"), ".a\\:b>.c{a:1}");
    // A nested rule and an `@extend` rewrite go through the same writer.
    assert_eq!(css_compressed(".a { > .b { c: 1; } }"), ".a>.b{c:1}");
    assert_eq!(
        css_compressed("%p { a: 1; }\n.x > .y { @extend %p; }"),
        ".x>.y{a:1}"
    );
}

/// A preserved CSS calculation — one that keeps a `var()` or `env()` and so
/// cannot fold to a number — separates its arguments with a bare comma when
/// compressing, like any other value. Measured against dart-sass 1.103.1.
#[test]
fn compressed_preserved_calculations_drop_the_argument_space() {
    let v = |scss: &str| css_compressed(&format!("a{{x:{scss}}}"));
    assert_eq!(v("clamp(0.5px, var(--y), 2px)"), "a{x:clamp(.5px,var(--y),2px)}");
    assert_eq!(
        v("clamp(1px, env(safe-area), 2px)"),
        "a{x:clamp(1px,env(safe-area),2px)}"
    );
    assert_eq!(v("min(1px, var(--y))"), "a{x:min(1px,var(--y))}");
    assert_eq!(v("mod(var(--y), 2px)"), "a{x:mod(var(--y),2px)}");
    assert_eq!(v("pow(var(--y), 2)"), "a{x:pow(var(--y),2)}");
    assert_eq!(v("calc-size(auto, var(--y))"), "a{x:calc-size(auto,var(--y))}");
    assert_eq!(
        v("calc(1px + clamp(1px, var(--y), 2px))"),
        "a{x:calc(1px + clamp(1px,var(--y),2px))}"
    );
    // A `@supports` declaration is not a value: dart writes it verbatim, space
    // and all, in both styles.
    assert_eq!(
        css_compressed("@supports (width: clamp(1px, var(--y), 2px)) { .a { b: 1; } }"),
        "@supports(width: clamp(1px, var(--y), 2px)){.a{b:1}}"
    );
}

#[test]
fn compressed_at_rule_prelude_spacing() {
    // dart-sass 1.101 compressed: `@media`/`@supports` drop the space before a
    // prelude that begins with `(`; within a `@media` query the space before
    // `and`/`or` is dropped after a `)` and the comma between queries loses its
    // space — but `@supports` conditions and other at-rules (`@container`) keep
    // their spaces, and an identifier media type keeps its `and` space.
    assert_eq!(
        css_compressed("@media (min-width: 1px) { a { x: 1 } }"),
        "@media(min-width: 1px){a{x:1}}"
    );
    assert_eq!(
        css_compressed("@media (a: 1) and (b: 2) { a { x: 1 } }"),
        "@media(a: 1)and (b: 2){a{x:1}}"
    );
    assert_eq!(
        css_compressed("@media (a: 1), (b: 2) { a { x: 1 } }"),
        "@media(a: 1),(b: 2){a{x:1}}"
    );
    assert_eq!(
        css_compressed("@media screen and (a: 1) { a { x: 1 } }"),
        "@media screen and (a: 1){a{x:1}}"
    );
    // @supports drops the leading `(` space but does NOT tighten `and`/`or`.
    assert_eq!(
        css_compressed("@supports (display: grid) { a { x: 1 } }"),
        "@supports(display: grid){a{x:1}}"
    );
    assert_eq!(
        css_compressed("@supports (a: 1) and (b: 2) { a { x: 1 } }"),
        "@supports(a: 1) and (b: 2){a{x:1}}"
    );
    // @container (and other at-rules) keep the space even before `(`.
    assert_eq!(
        css_compressed("@container (min-width: 1px) { a { x: 1 } }"),
        "@container (min-width: 1px){a{x:1}}"
    );
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
fn unknown_channel_errors_render_the_color_with_inspect() {
    let err = |src: &str| compile(src, &Options::default()).unwrap_err().message;
    let call = |src: &str| format!("@use \"sass:color\";a{{b:{src}}}");

    // dart builds these messages by interpolating the color into a string,
    // which routes through `Value.toString()` => `serializeValue(inspect: true)`
    // (lib/src/value.dart:439). So the color renders with INSPECT semantics, not
    // CSS-output semantics: `hwb(...)` keeps its own form instead of collapsing
    // to the `hsl(...)` it would be written as in a declaration.
    // All expectations below were produced by running dart-sass 1.101.6.
    assert_eq!(
        err(&call("color.is-missing(hwb(200 20% 30%), \"red\")")),
        "$channel: Color hwb(200 20% 30%) doesn't have a channel named \"red\"."
    );
    assert_eq!(
        err(&call("color.is-powerless(hwb(200 20% 30%), \"red\")")),
        "$channel: Color hwb(200 20% 30%) doesn't have a channel named \"red\"."
    );
    // `color.channel()` words its message differently (unquoted channel, "has no
    // channel named"), but renders the color the same way.
    assert_eq!(
        err(&call("color.channel(hwb(200 20% 30% / 0.5), \"zzz\")")),
        "$channel: Color hwb(200 20% 30% / 0.5) has no channel named zzz."
    );

    // A non-legacy space is unaffected by the reroute but must keep its own
    // canonical inspect form (percent lightness, `deg` hue).
    assert_eq!(
        err(&call("color.is-missing(oklch(0.5 0.1 200), \"red\")")),
        "$channel: Color oklch(50% 0.1 200deg) doesn't have a channel named \"red\"."
    );

    // A plain legacy sRGB color serializes identically under both renderers,
    // including its authored hex spelling.
    assert_eq!(
        err(&call("color.is-missing(#336699, \"zzz\")")),
        "$channel: Color #336699 doesn't have a channel named \"zzz\"."
    );
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

#[test]
fn at_root_group_separation_matches_dart() {
    // dart-sass treats an @at-root-hoisted chunk as its own top-level group and
    // separates the RESUMED parent rule with one blank line (isGroupEnd), but
    // ONLY when the chunk ends in a style rule. Bare-@at-root siblings separate;
    // a nested-@at-root chain and a rule + its OWN bubbled @media stay
    // contiguous. Every expected string is byte-exact dart-sass 1.101.

    // Resume after a single @at-root rule -> one blank before the resumed rule.
    assert_eq!(
        css(".a {\n  x: 1;\n  @at-root .b {\n    y: 2;\n  }\n  z: 3;\n}\n"),
        ".a {\n  x: 1;\n}\n.b {\n  y: 2;\n}\n\n.a {\n  z: 3;\n}\n"
    );
    // Bare @at-root with multiple rules: blank between siblings AND before resume.
    assert_eq!(
        css(".a {\n  x: 1;\n  @at-root {\n    .b { y: 2; }\n    .c { w: 4; }\n  }\n  z: 3;\n}\n"),
        ".a {\n  x: 1;\n}\n.b {\n  y: 2;\n}\n\n.c {\n  w: 4;\n}\n\n.a {\n  z: 3;\n}\n"
    );
    // Nested @at-root chain stays contiguous: NO blank between .b and .c.
    assert_eq!(
        css(".a {\n  x: 1;\n  @at-root .b {\n    y: 2;\n    @at-root .c { w: 4; }\n  }\n  z: 3;\n}\n"),
        ".a {\n  x: 1;\n}\n.b {\n  y: 2;\n}\n.c {\n  w: 4;\n}\n\n.a {\n  z: 3;\n}\n"
    );
    // A rule + its OWN bubbled @media stays contiguous: NO blank between them.
    assert_eq!(
        css(".parent {\n  color: red;\n  @at-root .top {\n    color: green;\n    @media screen { color: blue; }\n  }\n}\n"),
        ".parent {\n  color: red;\n}\n.top {\n  color: green;\n}\n@media screen {\n  .top {\n    color: blue;\n  }\n}\n"
    );
    // No resume after the @at-root -> no trailing blank.
    assert_eq!(
        css(".a {\n  x: 1;\n  @at-root .b { y: 2; }\n}\n"),
        ".a {\n  x: 1;\n}\n.b {\n  y: 2;\n}\n"
    );
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
    fn canonicalize(
        &self,
        url: &str,
        _ctx: &CanonicalizeContext<'_>,
    ) -> Result<Option<CanonicalUrl>, ImporterError> {
        // Record the request in importer-owned state. This allocation happens
        // *inside* the importer callback; the pause/resume boundary must keep it
        // on the system allocator so it outlives the compile's arena reset.
        self.requested.borrow_mut().push(url.to_string());
        Ok(self.files.contains_key(url).then(|| CanonicalUrl::new(url)))
    }

    fn load(&self, canonical: &CanonicalUrl) -> Result<Option<ImporterResult>, ImporterError> {
        Ok(self.files.get(canonical.as_str()).map(|src| ImporterResult {
            contents: src.clone(),
            syntax: Syntax::Scss,
            source_map_url: None,
        }))
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

    assert_eq!(out, "body {\n  padding: 8px;\n}\n\n.a {\n  margin: 4px;\n}");

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

#[test]
fn rgba_hsla_special_value_passthrough_keeps_name() {
    // When an rgb()/hsl() call can't resolve to a concrete color because a
    // channel is a CSS `var()`, dart-sass preserves the call AND the exact
    // function name the caller wrote. We previously normalized `rgba`/`hsla`
    // down to `rgb`/`hsl`. (Bootstrap relies on `rgba(var(--x), …)`.)
    assert_eq!(
        css(".a { color: rgba(var(--bs-body-color-rgb), 0.65); }\n"),
        ".a {\n  color: rgba(var(--bs-body-color-rgb), 0.65);\n}\n"
    );
    assert_eq!(
        css(".a { color: hsla(var(--h), 50%, 50%, 0.5); }\n"),
        ".a {\n  color: hsla(var(--h), 50%, 50%, 0.5);\n}\n"
    );
    // A genuine `rgb()`/`hsl()` call keeps its name too (unchanged behavior).
    assert_eq!(
        css(".a { color: rgb(var(--y), 0.5); }\n"),
        ".a {\n  color: rgb(var(--y), 0.5);\n}\n"
    );
    // A `none`-only call normalizes to the canonical space name, not the alias.
    assert_eq!(
        css(".a { color: rgba(none none none); }\n"),
        ".a {\n  color: rgb(none none none);\n}\n"
    );
}

#[test]
fn a_custom_property_value_reads_an_escape_as_one_token() {
    // dart-sass captures a custom-property value with
    // `_interpolatedDeclarationValue`, which consumes a `\` escape whole and
    // re-serializes it canonically (`escape(identifierStart: true)`). An
    // escaped delimiter is therefore literal text: it neither opens a bracket
    // nor a string, and it never terminates the declaration.
    assert_eq!(
        css(".a { --x: \\{; }\n.b { c: d; }\n"),
        ".a {\n  --x: \\{;\n}\n\n.b {\n  c: d;\n}\n"
    );
    assert_eq!(css(".a { --x: \\\"; }\n"), ".a {\n  --x: \\\";\n}\n");
    assert_eq!(css(".a { --x: a\\;b; }\n"), ".a {\n  --x: a\\;b;\n}\n");
    // Canonical re-serialization: a name-start char loses the escape, a digit
    // and a control character keep the hex form, `-` and `{` take the short
    // form, and an invalid code point becomes U+FFFD.
    assert_eq!(css(".a { --x: \\61 b; }\n"), ".a {\n  --x: ab;\n}\n");
    assert_eq!(css(".a { --x: \\7b; }\n"), ".a {\n  --x: \\{;\n}\n");
    assert_eq!(css(".a { --x: \\30 z; }\n"), ".a {\n  --x: \\30 z;\n}\n");
    assert_eq!(css(".a { --x: \\9 z; }\n"), ".a {\n  --x: \\9 z;\n}\n");
    assert_eq!(css(".a { --x: \\2d z; }\n"), ".a {\n  --x: \\-z;\n}\n");
    assert_eq!(
        css(".a { --x: \\d800 z; }\n"),
        "@charset \"UTF-8\";\n.a {\n  --x: \u{fffd}z;\n}\n"
    );
    // The same reader serves a `@supports` custom declaration and the body of
    // a plain-CSS custom `@function`.
    assert_eq!(
        css("@supports (--x: \\61 b) { a { b: c } }\n"),
        "@supports (--x: ab) {\n  a {\n    b: c;\n  }\n}\n"
    );
    assert_eq!(
        css("@function --f() { result: \\{; }\n"),
        "@function --f() {\n  result: \\{;\n}\n"
    );
}

#[test]
fn a_custom_property_value_matches_its_brackets() {
    // dart-sass matches each closer against the bracket it opened, so `(]` is
    // an error rather than a pair that cancels out; a closer with no opener
    // ends the value, and the declaration then wants its `;`.
    let err = |src: &str| {
        let e = compile(src, &Options::default()).expect_err("expected a compile error");
        (e.to_string(), e.line, e.col)
    };
    let (msg, line, col) = err(".a { --x: (] ; }\n");
    assert!(msg.contains("expected \")\"."), "{msg}");
    assert_eq!((line, col), (1, 12));
    let (msg, line, col) = err(".a { --x: ]; }\n");
    assert!(msg.contains("expected \";\"."), "{msg}");
    assert_eq!((line, col), (1, 11));
    // An escape after the backslash is required, as in an identifier.
    let (msg, line, col) = err(".a { --x: a\\\n b; }\n");
    assert!(msg.contains("Expected escape sequence."), "{msg}");
    assert_eq!((line, col), (1, 13));
}

#[test]
fn a_plain_css_custom_at_rule_body_matches_its_brackets() {
    // The body of a plain-CSS custom `@function`/`@mixin` captures each value
    // the way a custom property does, so a mismatched closer is an error, an
    // unclosed opener names the bracket it wanted, and a closer with no opener
    // ends the value (the body then wants its `;`).
    let err = |src: &str| {
        let e = compile(src, &Options::default()).expect_err("expected a compile error");
        (e.to_string(), e.line, e.col)
    };
    let (msg, line, col) = err("@function --f() { result: (]; }\n");
    assert!(msg.contains("expected \")\"."), "{msg}");
    assert_eq!((line, col), (1, 28));
    let (msg, line, col) = err("@function --f() { result: (; }\n");
    assert!(msg.contains("expected \")\"."), "{msg}");
    assert_eq!((line, col), (1, 30));
    let (msg, line, col) = err("@function --f() { result: ]; }\n");
    assert!(msg.contains("expected \";\"."), "{msg}");
    assert_eq!((line, col), (1, 27));
    // A balanced value still captures whole, `;` and all.
    assert_eq!(
        css("@function --f() { result: (a; b); other: c; }\n"),
        "@function --f() {\n  result: (a; b);\n  other: c;\n}\n"
    );
}

#[test]
fn an_import_url_token_drops_its_padding_and_decodes_escapes() {
    // dart reads `url(…)` in an `@import` with `_tryUrlContents`: the
    // whitespace after the `(` and before the `)` is not part of the token,
    // and a `\` escape is consumed whole and written back canonically
    // (`escape()`, so a name character loses its backslash). The value reader
    // already did both; the import reader kept the text verbatim.
    assert_eq!(css("@import url(  x.css  );\n"), "@import url(x.css);\n");
    assert_eq!(
        css("@import url(\n  http://x/y.css\n);\n"),
        "@import url(http://x/y.css);\n"
    );
    assert_eq!(css("@import url(\\61 b.css);\n"), "@import url(ab.css);\n");
    assert_eq!(css("@import url(\\2d x.css);\n"), "@import url(-x.css);\n");
    assert_eq!(css("@import url(\\30 x.css);\n"), "@import url(0x.css);\n");
    // A control character keeps its hex form, and an escaped space keeps its
    // backslash — neither is a name character.
    assert_eq!(css("@import url(\\9 x.css);\n"), "@import url(\\9 x.css);\n");
    assert_eq!(css("@import url(\\ x.css);\n"), "@import url(\\ x.css);\n");
    // An escaped paren is still url content, and a quoted url keeps its
    // padding (it is a string, not a url token).
    assert_eq!(
        css("@import url(foo\\)bar.css);\n"),
        "@import url(foo\\)bar.css);\n"
    );
    assert_eq!(
        css("@import url(\"  x.css  \");\n"),
        "@import url(\"  x.css  \");\n"
    );
    // Several imports on one line keep their own padding rules.
    assert_eq!(
        css("@import url(x.css ), url(y.css);\n"),
        "@import url(x.css);\n@import url(y.css);\n"
    );
}

#[test]
fn a_backslash_before_a_newline_is_only_an_escape_inside_a_string() {
    // dart's `escape()` fails on a newline, so a backslash "line continuation"
    // is an error everywhere a CSS escape may appear — a value, a selector, a
    // property name, an at-rule prelude. Only the string reader drops the pair
    // first, which is what makes it legal inside quotes.
    let err = |src: &str| {
        let e = compile(src, &Options::default()).expect_err("expected a compile error");
        (e.to_string(), e.line, e.col)
    };
    for (src, line, col) in [
        (".a { b: c\\\n  d; }\n", 1, 11),
        (".a,\\\n.b { c: d; }\n", 1, 5),
        (".a { b\\\nc: d; }\n", 1, 8),
        ("@media screen\\\nand (min-width: 0) { .a { b: c } }\n", 1, 15),
        (".a { b: url(foo\\\nbar.css); }\n", 1, 17),
    ] {
        let (msg, l, c) = err(src);
        assert!(msg.contains("Expected escape sequence."), "{src:?}: {msg}");
        assert_eq!((l, c), (line, col), "for {src:?}");
    }
    // Inside a quoted string the pair IS a line continuation: it vanishes, and
    // the next line's indentation stays content.
    assert_eq!(css(".a { b: \"x\\\ny\"; }\n"), ".a {\n  b: \"xy\";\n}\n");
    assert_eq!(css("[a=\"x\\\ny\"] { c: d; }\n"), "[a=xy] {\n  c: d;\n}\n");
    assert_eq!(
        css("@media (min-width: 0) and (x: \"a\\\nb\") { .a { b: c } }\n"),
        "@media (min-width: 0) and (x: ab) {\n  .a {\n    b: c;\n  }\n}\n"
    );
}

#[test]
fn an_import_url_that_is_not_a_url_token_is_a_function_call() {
    // dart `dynamicUrl`: when `url(…)` does not read as a plain url token, the
    // call is an ordinary function whose arguments EVALUATE. sasso emitted the
    // SassScript verbatim, so a variable reached the CSS.
    assert_eq!(css("@import url(foo + bar);\n"), "@import url(foobar);\n");
    assert_eq!(
        css("$v: x;\n@import url($v + \".css\");\n"),
        "@import url(x.css);\n"
    );
    // A quoted argument is a function call too, so it keeps its own text.
    assert_eq!(
        css("@import url(\"  x.css  \");\n"),
        "@import url(\"  x.css  \");\n"
    );
    // Interpolation inside that string still resolves.
    assert_eq!(
        css("$p: http;\n@import url(\"#{$p}://x/y.css\");\n"),
        "@import url(\"http://x/y.css\");\n"
    );
    // A plain url token still takes the token path (no evaluation, padding
    // dropped, escapes decoded).
    assert_eq!(css("@import url(  x.css  );\n"), "@import url(x.css);\n");
}

#[test]
fn interpolation_resolves_inside_a_quoted_verbatim_value() {
    // A verbatim value's TEXT is copied, but `#{…}` is not part of that text —
    // dart resolves it inside a quoted string as well as outside one. The
    // custom-property reader already did; the `@supports` and plain-CSS custom
    // callable readers copied the string whole.
    assert_eq!(
        css("$v: x;\n@supports (--a: \"#{$v}\") { .a { b: c } }\n"),
        "@supports (--a: \"x\") {\n  .a {\n    b: c;\n  }\n}\n"
    );
    assert_eq!(
        css("$v: x;\n@function --f() { result: \"#{$v}\"; }\n"),
        "@function --f() {\n  result: \"x\";\n}\n"
    );
    assert_eq!(
        css("$v: x;\n.a { --x: \"#{$v}\"; }\n"),
        ".a {\n  --x: \"x\";\n}\n"
    );
    // The string's own escapes stay verbatim, line continuation included — a
    // verbatim value is not re-serialized the way a SassScript string is.
    assert_eq!(
        css(".a { --x: \"a\\\nb\"; }\n"),
        ".a {\n  --x: \"a\\\n  b\";\n}\n"
    );
    assert_eq!(
        css("@supports (--a: \"x\\\ny\") { .a { b: c } }\n"),
        "@supports (--a: \"x\\ y\") {\n  .a {\n    b: c;\n  }\n}\n"
    );
}
#[test]
fn a_form_feed_is_a_newline_to_the_escape_reader() {
    // dart's `isNewline` counts U+000C, so a backslash cannot escape it — in a
    // verbatim value as in an ordinary one.
    let err = |src: &str| {
        let e = compile(src, &Options::default()).expect_err("expected a compile error");
        (e.to_string(), e.line, e.col)
    };
    let (msg, line, col) = err(".a { --x: c\\\u{c}d; }\n");
    assert!(msg.contains("Expected escape sequence."), "{msg}");
    assert_eq!((line, col), (1, 13));
    let (msg, _, _) = err(".a { b: c\\\u{c}d; }\n");
    assert!(msg.contains("Expected escape sequence."), "{msg}");
    // Inside a string it is a line continuation, and the pair vanishes.
    assert_eq!(css(".a { b: \"c\\\u{c}d\"; }\n"), ".a {\n  b: \"cd\";\n}\n");
}

#[test]
fn a_hex_escape_terminator_is_one_line_break() {
    // One whitespace character terminates a hex escape. dart takes a CRLF
    // whole where the text is captured VERBATIM — `--x: \61` + CRLF + `b` is
    // `ab`, with no line break left in the value — and only the `\r` where the
    // text is parsed as SassScript, so the `\n` still separates two
    // identifiers.
    assert_eq!(css(".a { --x: \\61\r\nb; }\n"), ".a {\n  --x: ab;\n}\n");
    assert_eq!(css(".a { b: \\61\r\nb; }\n"), ".a {\n  b: a b;\n}\n");
    // A lone LF or CR terminates the escape in both.
    assert_eq!(css(".a { --x: \\61\nb; }\n"), ".a {\n  --x: ab;\n}\n");
    assert_eq!(css(".a { --x: \\61\rb; }\n"), ".a {\n  --x: ab;\n}\n");
    assert_eq!(css(".a { b: \\61\nb; }\n"), ".a {\n  b: ab;\n}\n");
    assert_eq!(css(".a { b: \\61\rb; }\n"), ".a {\n  b: ab;\n}\n");
}

#[test]
fn a_quoted_string_is_serialized_with_the_quote_it_needs() {
    // dart `_visitQuotedString`: single quotes when the text contains a `"`
    // and no `'`, and the chosen quote and every backslash escaped. `inspect`
    // wrapped the text in `"` unconditionally, so a string containing a quote
    // or a backslash came out as INVALID CSS (`b: "a"b";`).
    assert_eq!(
        css("@use \"sass:meta\";\n.a { b: meta.inspect(\"a\\\"b\"); }\n"),
        ".a {\n  b: 'a\"b';\n}\n"
    );
    assert_eq!(
        css("@use \"sass:meta\";\n.a { b: meta.inspect(\"a\\\\b\"); }\n"),
        ".a {\n  b: \"a\\\\b\";\n}\n"
    );
    assert_eq!(
        css("@use \"sass:meta\";\n.a { b: meta.inspect(\"it \\\"broke\\\"\"); }\n"),
        ".a {\n  b: 'it \"broke\"';\n}\n"
    );
    // Inside a collection, and for a string that needs both quote kinds.
    assert_eq!(
        css("@use \"sass:meta\";\n.a { b: meta.inspect((x: \"a\\\"b\")); }\n"),
        ".a {\n  b: (x: 'a\"b');\n}\n"
    );
    // `@error` renders its value through the same serializer.
    let e = compile("@error \"a\\\"b\";\n", &Options::default()).expect_err("an @error");
    assert!(e.to_string().contains("'a\"b'"), "{e}");
    let e = compile("@error 'it \"broke\"';\n", &Options::default()).expect_err("an @error");
    assert!(e.to_string().contains("'it \"broke\"'"), "{e}");
}

#[test]
fn an_unquoted_import_url_written_back_is_quoted_like_a_string() {
    // The indented syntax writes a plain-CSS import's bare url back QUOTED.
    // The url is text, so its backslashes are escaped and a url containing a
    // `"` takes single quotes — dart serializes it as any other string.
    let sass = |src: &str| compile(src, &Options::default().with_syntax(Syntax::Sass)).expect("compile");
    assert_eq!(
        sass("@import h\\74 tps://x/y.css\n"),
        "@import \"h\\\\74 tps://x/y.css\";"
    );
    assert_eq!(sass("@import foo\\\"bar.css\n"), "@import 'foo\\\\\"bar.css';");
    assert_eq!(sass("@import \\\\x.css\n"), "@import \"\\\\\\\\x.css\";");
    // A url with neither stays as it was.
    assert_eq!(sass("@import foo.css\n"), "@import \"foo.css\";");
}

#[test]
fn a_builtin_answers_to_its_underscore_spelling() {
    // `_` and `-` are one character in a Sass identifier, so a global built-in
    // is reached either way — as it already is for user members and module
    // members. Measured against dart-sass 1.103.1.
    assert_eq!(css("a { b: str_index(\"abc\", \"b\"); }"), "a {\n  b: 2;\n}\n");
    assert_eq!(css("a { b: to_upper_case(\"abc\"); }"), "a {\n  b: \"ABC\";\n}\n");
    assert_eq!(css("a { b: type_of(1); }"), "a {\n  b: number;\n}\n");
    assert_eq!(css("a { b: map_get((x: 1), x); }"), "a {\n  b: 1;\n}\n");
    // The stateful `sass:meta` members resolve the same way, globally and
    // through the module.
    assert_eq!(
        css("$x: 1; a { b: variable_exists(\"x\"); }"),
        "a {\n  b: true;\n}\n"
    );
    assert_eq!(
        css("@use \"sass:meta\"; a { b: meta.function_exists(\"rgb\"); }"),
        "a {\n  b: true;\n}\n"
    );
    // A name that is no built-in keeps the spelling it was written with: it is
    // a plain CSS function, not a Sass one.
    assert_eq!(css("a { b: my_own_fn(1); }"), "a {\n  b: my_own_fn(1);\n}\n");
    // A reference is stored canonically, as dart stores it.
    assert_eq!(
        css("@use \"sass:meta\"; a { b: meta.inspect(meta.get-function(\"map_get\")); }"),
        "a {\n  b: get-function(\"map-get\");\n}\n"
    );
}

#[test]
fn a_module_member_reference_is_not_its_global_alias() {
    // `meta.get-function($module:)` yields the MODULE's function: dart keeps
    // the member's own name, does not compare it equal to the global alias,
    // and dispatches it through the module. Measured against dart-sass 1.103.1.
    assert_eq!(
        css("@use \"sass:meta\"; @use \"sass:map\";\na { b: meta.inspect(meta.get-function(\"get\", $module: \"map\")); }"),
        "a {\n  b: get-function(\"get\");\n}\n"
    );
    assert_eq!(
        css("@use \"sass:meta\"; @use \"sass:map\";\na { b: meta.get-function(\"get\", $module: \"map\") == meta.get-function(\"map-get\"); }"),
        "a {\n  b: false;\n}\n"
    );
    assert_eq!(
        css("@use \"sass:meta\"; @use \"sass:map\";\na { b: meta.call(meta.get-function(\"get\", $module: \"map\"), (x: 1), x); }"),
        "a {\n  b: 1;\n}\n"
    );
    // `color.scale` is not the global `scale-color`, so the reference has to
    // carry the module to reach the right function at all.
    assert_eq!(
        css("@use \"sass:meta\"; @use \"sass:color\";\na { b: meta.inspect(meta.get-function(\"scale\", $module: \"color\")); }"),
        "a {\n  b: get-function(\"scale\");\n}\n"
    );
    assert_eq!(
        css("@use \"sass:meta\"; @use \"sass:color\";\na { b: meta.call(meta.get-function(\"scale\", $module: \"color\"), #abcdef, $lightness: 50%); }"),
        "a {\n  b: #d5e6f7;\n}\n"
    );
    // A member a `@use "sass:…" as *` exposes unprefixed is that module's too.
    assert_eq!(
        css(
            "@use \"sass:map\" as *; @use \"sass:meta\";\na { b: meta.inspect(meta.get-function(\"get\")); }"
        ),
        "a {\n  b: get-function(\"get\");\n}\n"
    );
}

#[test]
fn every_global_is_referenceable_by_name() {
    // The `sass:meta` predicates that resolve against evaluator state are
    // globals like any other: `get-function` finds them, and invoking the
    // reference runs them. Measured against dart-sass 1.103.1.
    assert_eq!(
        css(
            "$x: 1;\n@use \"sass:meta\";\na { b: meta.call(meta.get-function(\"variable-exists\"), \"x\"); }"
        ),
        "a {\n  b: true;\n}\n"
    );
    assert_eq!(
        css("@use \"sass:meta\";\na { b: meta.call(meta.get-function(\"call\"), meta.get-function(\"rgb\"), 1, 2, 3); }"),
        "a {\n  b: rgb(1, 2, 3);\n}\n"
    );
    for name in ["keywords", "content-exists", "get-function", "mixin-exists"] {
        assert_eq!(
            css(&format!(
                "@use \"sass:meta\";\na {{ b: meta.inspect(meta.get-function(\"{name}\")); }}"
            )),
            format!("a {{\n  b: get-function(\"{name}\");\n}}\n")
        );
    }
    // A name that is no function at all is reported as the string it was asked
    // for, quoting and all.
    let err = compile(
        "@use \"sass:meta\"; a { b: meta.get-function(\"a\\\"b\"); }",
        &Options::default(),
    )
    .unwrap_err()
    .to_string();
    assert!(err.contains("Function not found: 'a\"b'"), "{err}");
}

#[test]
fn a_starred_builtin_member_shadows_the_global_of_that_name() {
    // `@use "sass:…" as *` exposes the module's members unprefixed, and they
    // WIN over the global of the same name: `index` is `string.index` after
    // `@use "sass:string" as *`, not the list one. Measured against dart-sass
    // 1.103.1.
    assert_eq!(
        css("@use \"sass:string\" as *; a { b: index(\"abc\", \"b\"); }"),
        "a {\n  b: 2;\n}\n"
    );
    assert_eq!(
        css("@use \"sass:list\" as *; a { b: index(\"abc\" \"b\", \"b\"); }"),
        "a {\n  b: 2;\n}\n"
    );
    assert!(compile(
        "@use \"sass:string\" as *; a { b: index(\"abc\" \"b\", \"b\"); }",
        &Options::default()
    )
    .unwrap_err()
    .to_string()
    .contains("is not a string."));
    // Exposure from two starred modules is ambiguous, however it is reached.
    for src in [
        "@use \"sass:list\" as *; @use \"sass:string\" as *; a { b: index(\"abc\", \"b\"); }",
        "@use \"sass:list\" as *; @use \"sass:string\" as *; @use \"sass:meta\";\na { b: meta.get-function(\"index\"); }",
        "@use \"sass:list\" as *; @use \"sass:string\" as *; @use \"sass:meta\";\na { b: meta.function-exists(\"index\"); }",
    ] {
        let err = compile(src, &Options::default()).unwrap_err().to_string();
        assert!(
            err.contains("This function is available from multiple global modules."),
            "{src}: {err}"
        );
    }
    // A module that does not have the member leaves the global alone.
    assert_eq!(
        css("@use \"sass:map\" as *; @use \"sass:meta\";\na { b: meta.function-exists(\"get\"); }"),
        "a {\n  b: true;\n}\n"
    );
}

#[test]
fn a_reference_invoked_by_name_is_looked_up_canonically() {
    // `call("string")` never goes through `get-function`, so the name arrives
    // exactly as written — and `_` is `-` in a Sass identifier, including for
    // the `sass:meta` members the evaluator owns. Measured against dart-sass
    // 1.103.1.
    assert_eq!(
        css("$x: 1; a { b: call(\"variable_exists\", \"x\"); }"),
        "a {\n  b: true;\n}\n"
    );
    assert_eq!(
        css("a { b: call(\"str_index\", \"abc\", \"b\"); }"),
        "a {\n  b: 2;\n}\n"
    );
    assert_eq!(
        css("@use \"sass:meta\"; $x: 1; a { b: meta.call(\"variable_exists\", \"x\"); }"),
        "a {\n  b: true;\n}\n"
    );
    // A starred module's member is still stored canonically.
    assert_eq!(
        css("@use \"sass:string\" as *; @use \"sass:meta\";\na { b: meta.inspect(meta.get-function(\"to_upper_case\")); }"),
        "a {\n  b: get-function(\"to-upper-case\");\n}\n"
    );
}
