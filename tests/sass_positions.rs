//! Indented-syntax (`.sass`) positions: every line and column a diagnostic or
//! a source map reports is a position in the `.sass` file itself.
//!
//! The front-end reconstructs SCSS from the indentation-structured source and
//! hands it to the shared parser, so what it reconstructs decides what every
//! `Pos` means. The reconstruction is position-preserving — one output line per
//! source line, the source indentation kept, a block's `}` riding on its last
//! line — and the two constructs that cannot be rewritten without moving
//! columns (the `=`/`+` mixin shorthands, unquoted `@import` urls) are read by
//! the parser in place instead.
//!
//! Every expectation here was taken from dart-sass 1.103.1 on the same input.

use std::cell::RefCell;
use std::rc::Rc;

use sasso::{compile, compile_with_source_map, Options, OutputStyle, Syntax, WarnEvent};

/// Compile indented source, with diagnostics enabled under `url`.
fn sass(src: &str, url: &str) -> Result<String, sasso::Error> {
    compile(src, &Options::default().with_syntax(Syntax::Sass).with_url(url))
}

/// The rendered diagnostic block of the error `src` raises.
fn sass_err(src: &str, url: &str) -> String {
    sass(src, url).expect_err("expected a compile error").to_string()
}

/// Every `formatted` warning block `src` produces.
fn sass_warnings(src: &str, url: &str) -> Vec<String> {
    let seen: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
    let sink = Rc::clone(&seen);
    let opts = Options::default()
        .with_syntax(Syntax::Sass)
        .with_url(url)
        .with_warn_handler(Rc::new(move |ev: &WarnEvent<'_>| {
            sink.borrow_mut().push(ev.formatted.to_string());
        }));
    compile(src, &opts).expect("compile");
    let out = seen.borrow().clone();
    out
}

/// The `mappings` of an indented source compiled under `url`.
fn sass_mappings(src: &str, url: &str) -> String {
    let opts = Options::default().with_syntax(Syntax::Sass).with_url(url);
    compile_with_source_map(src, &opts)
        .expect("compile")
        .source_map
        .mappings
}

#[test]
fn an_error_points_at_its_sass_line_and_column() {
    // The column is the one in the `.sass` file: the reconstruction keeps each
    // statement's own indentation rather than re-indenting by nesting depth.
    assert_eq!(
        sass_err(".a\n  .b\n    color: $undefined\n", "a.sass"),
        "Error: Undefined variable.\n  \u{2577}\n3 \u{2502}     color: $undefined\n  \
         \u{2502}            ^^^^^^^^^^\n  \u{2575}\n  a.sass 3:12  root stylesheet"
    );
}

#[test]
fn closed_blocks_and_comments_do_not_shift_later_lines() {
    // Each source line maps to one output line: a finished block, a blank line
    // and a multi-line comment all leave the lines after them where they were.
    let cases = [
        // A nested block, then a blank line, then the error.
        (
            ".a\n  color: red\n\n.b\n  .c\n    color: blue\n\n.d\n  color: $u\n",
            (9, 10),
        ),
        // A multi-line selector list.
        (".a,\n.b\n  color: red\n.d\n  color: $u\n", (5, 10)),
        // A multi-line loud comment.
        ("/* one\n   two\n   three */\n.d\n  color: $u\n", (5, 10)),
        // The simplest shape of all.
        (".a\n  color: red\n.d\n  color: $u\n", (4, 10)),
    ];
    for (src, (line, col)) in cases {
        let e = sass(src, "in.sass").expect_err("expected an error");
        assert_eq!((e.line, e.col), (line, col), "for {src:?}");
    }
}

#[test]
fn a_warning_points_at_its_sass_column() {
    let w = sass_warnings(".a\n  .b\n    @warn \"hi\"\n", "b.sass");
    assert_eq!(w.len(), 1);
    assert!(w[0].contains("b.sass 3:5"), "{}", w[0]);
}

/// A scratch directory holding `_foo.sass`, for the import tests.
fn scratch(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("sasso_sass_pos_{tag}_{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("mkdir");
    std::fs::write(dir.join("_foo.sass"), "l\n  m: 1\n").expect("write");
    dir
}

#[test]
fn an_unquoted_import_url_carries_its_own_span() {
    // dart reads an unquoted url with the whole-value reader, so the
    // deprecation carets exactly the url token — `foo`, not the quoted
    // rewrite the front-end used to produce (two bytes longer).
    let dir = scratch("span");
    let src = "@import foo\n.a\n  color: red\n";
    let entry = dir.join("c.sass");
    std::fs::write(&entry, src).unwrap();
    let url = entry.to_string_lossy().into_owned();
    let imp = sasso::FsImporter::new(Vec::new());
    let seen: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
    let sink = Rc::clone(&seen);
    let opts = Options::default()
        .with_syntax(Syntax::Sass)
        .with_importer(&imp)
        .with_url(&url)
        .with_warn_handler(Rc::new(move |ev: &WarnEvent<'_>| {
            sink.borrow_mut().push(ev.formatted.to_string());
        }));
    let css = compile(src, &opts).expect("compile");
    assert_eq!(css, "l {\n  m: 1;\n}\n\n.a {\n  color: red;\n}");
    let w = seen.borrow();
    assert_eq!(w.len(), 1);
    assert!(
        w[0].contains("1 \u{2502} @import foo\n  \u{2502}         ^^^\n"),
        "{}",
        w[0]
    );
    assert!(w[0].contains("c.sass 1:9"), "{}", w[0]);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn an_unquoted_import_url_runs_to_the_comma() {
    // dart's `SassParser.importArgument` reads to the next top-level comma,
    // spaces included: `@import foo screen` is ONE url named `foo screen`
    // (dart carets all ten characters), not a url plus a media modifier.
    // (sasso's message still names the url where dart's is bare — a general
    // gap in the missing-import diagnostic, not an indented-syntax one.)
    let dir = scratch("comma");
    let src = "@import foo screen\n";
    let entry = dir.join("e.sass");
    std::fs::write(&entry, src).unwrap();
    let url = entry.to_string_lossy().into_owned();
    let imp = sasso::FsImporter::new(Vec::new());
    let opts = Options::default()
        .with_syntax(Syntax::Sass)
        .with_importer(&imp)
        .with_url(&url)
        .with_warn_handler(Rc::new(|_: &WarnEvent<'_>| {}));
    let e = compile(src, &opts).expect_err("expected an error");
    // The whole token is the url, so that is what cannot be found. (dart also
    // carets it at 1:9; sasso's missing-import error carries no span yet —
    // a general gap, in `.scss` as much as here.)
    assert!(e.message.contains("foo screen"), "{}", e.message);
    // Two comma-separated urls are two imports.
    std::fs::write(dir.join("_bar.sass"), "n\n  o: 2\n").unwrap();
    let src = "@import foo, bar\n";
    std::fs::write(&entry, src).unwrap();
    let css = compile(src, &opts).expect("compile");
    assert_eq!(css, "l {\n  m: 1;\n}\n\nn {\n  o: 2;\n}");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_css_import_url_is_written_back_quoted() {
    // A `.css`/protocol url is a plain-CSS import, emitted with its text
    // verbatim inside quotes, as dart emits it.
    assert_eq!(
        sass("@import other.css\n", "c.sass").unwrap(),
        "@import \"other.css\";"
    );
    assert_eq!(
        sass("@import http://x/y.css\n", "d.sass").unwrap(),
        "@import \"http://x/y.css\";"
    );
    assert_eq!(
        sass("@import url(http://x/y.css)\n", "d.sass").unwrap(),
        "@import url(http://x/y.css);"
    );
}

#[test]
fn a_double_slash_inside_a_url_is_not_a_comment() {
    // dart scans `url(` and its contents as one token; only the exact `url`
    // function qualifies, so `my-url(//y)` really does start a comment.
    assert_eq!(
        sass(".a\n  b: url(http://x/y)\n", "a.sass").unwrap(),
        ".a {\n  b: url(http://x/y);\n}"
    );
    assert_eq!(
        sass(".a\n  background: url(//cdn/x.png)\n", "a.sass").unwrap(),
        ".a {\n  background: url(//cdn/x.png);\n}"
    );
    assert_eq!(
        sass(".a\n  b: URL(//y)\n", "a.sass").unwrap(),
        ".a {\n  b: url(//y);\n}"
    );
    assert!(sass(".a\n  b: my-url(//y)\n", "a.sass").is_err());
    // A VENDOR-PREFIXED url is a url token too — the shared value parser reads
    // `-c-url(` as one and emits it bare — while `my-url(` above is not.
    assert_eq!(
        sass("a\n  b: -c-url(//cdn/x)\n  d: red\n", "a.sass").unwrap(),
        "a {\n  b: url(//cdn/x);\n  d: red;\n}"
    );
    // The name is matched the way the value parser matches it, escapes and
    // all: `u\\72l(` is `url(`, so its `//` is url text too.
    assert_eq!(
        sass("a\n  b: u\\72l(//cdn/x)\n  c: red\n", "a.sass").unwrap(),
        "a {\n  b: url(//cdn/x);\n  c: red;\n}"
    );
    // An escaped `)` is url CONTENT and does not close the token.
    assert_eq!(
        sass("a\n  b: url(foo\\)//cdn)\n", "a.sass").unwrap(),
        "a {\n  b: url(foo\\)//cdn);\n}"
    );
    // A declaration AFTER one is still its own statement: the line scanners
    // that decide where a logical line ends must skip the url token too, or
    // `url(http://x/y)` reads as an unterminated paren and swallows the next
    // line (`b: url(http://x/y) c;` / `: red;`).
    assert_eq!(
        sass(".a\n  b: url(http://x/y)\n  c: red\n", "a.sass").unwrap(),
        ".a {\n  b: url(http://x/y);\n  c: red;\n}"
    );
    assert_eq!(
        sass(".a\n  b: url(//x/y)\n  c: red\n", "a.sass").unwrap(),
        ".a {\n  b: url(//x/y);\n  c: red;\n}"
    );
    // A `url(` may be left OPEN at the end of its line — the indented syntax
    // continues it — and its contents stay verbatim across the join, so the
    // `//` in a protocol url is still not a comment (four sass-spec cases).
    assert_eq!(
        sass("a\n  b: url(\n    c)\n", "a.sass").unwrap(),
        "a {\n  b: url(c);\n}"
    );
    assert_eq!(
        sass("a\n  b: url(c\n    )\n", "a.sass").unwrap(),
        "a {\n  b: url(c);\n}"
    );
    assert_eq!(
        sass("a\n  b: url(\n    http://x/y)\n  d: red\n", "a.sass").unwrap(),
        "a {\n  b: url(http://x/y);\n  d: red;\n}"
    );
    // A trailing `//` is still a comment, inside and outside a string.
    assert_eq!(sass(".a\n  b: c // t\n", "a.sass").unwrap(), ".a {\n  b: c;\n}");
    assert_eq!(
        sass(".a\n  b: \"http://x\" // t\n", "a.sass").unwrap(),
        ".a {\n  b: \"http://x\";\n}"
    );
    assert_eq!(
        sass(".a\n  b: url(x) // t\n", "a.sass").unwrap(),
        ".a {\n  b: url(x);\n}"
    );
}

#[test]
fn source_maps_point_into_the_sass_file() {
    // Every `mappings` string here is dart-sass 1.103.1's for the same input.
    let cases = [
        (".a\n  color: red\n", "AAAA;EACE"),
        // A silent comment, a blank line, a multi-line selector list and a
        // loud comment — none of them shift a later mapping.
        (
            "// silent\n.a\n  color: red\n\n  .b,\n  .c\n    /* loud */\n    width: 1px\n",
            "AACA;EACE;;AAEA;AAAA;AAEE;EACA",
        ),
        // The mixin shorthands: `+mx(1)`'s ARGUMENT maps to its own column,
        // which rewriting `+mx` to `@include mx` would have moved by eight.
        ("=mx($x)\n  m: $x\n.d\n  +mx(1)\n", "AAEA;EADE,GAEI"),
        // A parent reference and a bubbled `@media`.
        (
            ".a\n  &:hover\n    x: 1\n@media screen\n  .b\n    y: 2\n",
            "AACE;EACE;;;AACJ;EACE;IACE",
        ),
        // A custom property.
        (".a\n  --v: 1px\n  b: c\n", "AAAA;EACE;EACA"),
        // A top-level multi-line comment.
        ("/* one\n   two */\n.a\n  b: c\n", "AAAA;AAAA;AAEA;EACE"),
        // The legacy escaped-selector marker is not part of the selector:
        // dart maps `\:hover` to the `:`, one column past the `\`.
        (".a\n  \\:hover\n    b: c\n", "AACG;EACC"),
        (".a\n  :hover\n    b: c\n", "AACE;EACE"),
    ];
    for (src, expected) in cases {
        assert_eq!(sass_mappings(src, "in.sass"), expected, "for {src:?}");
    }
}

#[test]
fn the_shorthands_still_compile_to_what_they_stand_for() {
    // Reading `=`/`+` in the parser rather than rewriting them upstream must
    // not change what they mean.
    assert_eq!(
        sass("=a\n  b: c\nd\n  +a\n", "x.sass").unwrap(),
        "d {\n  b: c;\n}"
    );
    assert_eq!(
        sass("=a($x)\n  b: $x\nd\n  +a(1)\n", "x.sass").unwrap(),
        "d {\n  b: 1;\n}"
    );
    assert_eq!(
        sass("=a\n  @content\nd\n  +a\n    e: f\n", "x.sass").unwrap(),
        "d {\n  e: f;\n}"
    );
    // A bare `+` is the next-sibling combinator, not an include.
    assert_eq!(
        sass("d\n  +\n    a\n      x: y\n", "x.sass").unwrap(),
        "d + a {\n  x: y;\n}"
    );
    // A `+name` include with `using`, and a namespaced one.
    assert_eq!(
        sass("=a\n  @content(1)\nd\n  +a using ($v)\n    e: $v\n", "x.sass").unwrap(),
        "d {\n  e: 1;\n}"
    );
}

#[test]
fn the_shorthand_keeps_the_rules_the_keyword_has() {
    // `=--name` is the plain-CSS mixin spelling dart reserves: rejected here
    // exactly as `@mixin --name` is, pointing at the name (dart: 1:2).
    let e = sass("=--a\n  b: c\n", "b.sass").expect_err("expected an error");
    assert_eq!((e.line, e.col), (1, 2));
    assert!(
        e.message.contains("beginning with -- are forbidden"),
        "{}",
        e.message
    );
    // A mixin whose NAME is the keyword the shorthand stands for still works:
    // the prelude is everything after the sigil, with no keyword to strip.
    assert_eq!(
        sass("=mixin\n  b: c\n.d\n  +mixin\n", "x.sass").unwrap(),
        ".d {\n  b: c;\n}"
    );
    assert_eq!(
        sass("=include\n  b: c\n.d\n  +include\n", "x.sass").unwrap(),
        ".d {\n  b: c;\n}"
    );
}

#[test]
fn a_multi_line_directive_prelude_keeps_its_own_lines() {
    // A prelude continuation is joined ON ITS OWN LINE, not with a space, so a
    // diagnostic inside it points at the line it was written on, as dart does.
    let cases = [
        "@each $a in\n  $undef\n  .x\n    y: 1\n",
        "$v:\n  $undef\n",
        "@if 1 ==\n  $undef\n  .x\n    y: 1\n",
    ];
    for src in cases {
        let e = sass(src, "in.sass").expect_err("expected an error");
        assert_eq!((e.line, e.col), (2, 3), "for {src:?}");
        assert!(e.message.contains("Undefined variable"), "{}", e.message);
    }
    // The prelude still reads the same text: a trailing comma does NOT
    // continue it (`@each $a in b,` iterates the one-element list `(b,)` and
    // the deeper lines are its body), exactly as dart-sass 1.103.1 reads it.
    assert_eq!(
        sass("@each $a in b,\n c\n  .#{$a}\n    d: $a\n", "in.sass").unwrap(),
        "c .b {\n  d: b;\n}"
    );
}

#[test]
fn a_custom_property_keeps_its_own_spacing() {
    // A custom property's value is emitted verbatim, so the whitespace after
    // the colon is part of it: dart writes `--v:1px` with no space, and
    // collapses a run of them to one. The front-end used to normalize every
    // spelling to `: `, which changed the CSS.
    assert_eq!(sass(".a\n  --v:1px\n", "a.sass").unwrap(), ".a {\n  --v:1px;\n}");
    assert_eq!(
        sass(".a\n  --v: 1px\n", "a.sass").unwrap(),
        ".a {\n  --v: 1px;\n}"
    );
    assert_eq!(
        sass(".a\n  --v:  1px\n", "a.sass").unwrap(),
        ".a {\n  --v: 1px;\n}"
    );
    assert_eq!(
        sass(".a\n  --v:   1px\n", "a.sass").unwrap(),
        ".a {\n  --v: 1px;\n}"
    );
    assert_eq!(
        sass(".a\n  --v:1px 2px\n", "a.sass").unwrap(),
        ".a {\n  --v:1px 2px;\n}"
    );
    assert_eq!(
        sass(".a\n  --v:#{1 + 1}\n", "a.sass").unwrap(),
        ".a {\n  --v:2;\n}"
    );
}

#[test]
fn compressed_output_is_unaffected_by_the_line_padding() {
    // The reconstruction pads with blank lines; compressed output has none.
    let opts = Options::default()
        .with_syntax(Syntax::Sass)
        .with_style(OutputStyle::Compressed)
        .with_url("in.sass");
    let css = compile("// c\n.a\n  b: c\n\n.d\n  e: f\n", &opts).expect("compile");
    assert_eq!(css, ".a{b:c}.d{e:f}");
}
