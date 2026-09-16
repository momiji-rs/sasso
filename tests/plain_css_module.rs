//! Plain-CSS files reached through `@use`/`@import` are emitted the way
//! dart-sass emits them: nothing of the file's own `@charset` survives (the
//! output's `@charset` is re-derived from its content), and nested rules keep
//! the selector list's source line structure.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use sasso::{compile, FsImporter, Options, WarnEvent};

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("sasso_plaincss_{tag}_{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("mkdir");
    dir
}

/// Compile `src` as an entry in `dir`, swallowing warnings (the `@import`
/// deprecation), and return the CSS.
fn compile_in(dir: &std::path::Path, name: &str, src: &str) -> String {
    let entry = dir.join(name);
    std::fs::write(&entry, src).unwrap();
    let url = entry.to_string_lossy().into_owned();
    let imp = FsImporter::new(Vec::new());
    let sink: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
    let seen = Rc::clone(&sink);
    let opts = Options::default()
        .with_importer(&imp)
        .with_url(&url)
        .with_warn_handler(Rc::new(move |ev: &WarnEvent<'_>| {
            seen.borrow_mut().push(ev.message.to_string());
        }));
    compile(src, &opts).expect("compile")
}

#[test]
fn a_loaded_files_charset_is_dropped() {
    // dart: the `@charset` of a loaded `.css` (or `.scss`) file never appears
    // in the output; the output's own `@charset "UTF-8";` comes from its
    // non-ASCII content, and an all-ASCII output has none.
    let dir = scratch("charset");
    std::fs::write(
        dir.join("_theme.css"),
        "@charset \"utf-8\";\n.t { color: red; }\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("_lib.scss"),
        "@charset \"utf-8\";\n.s { content: \"é\"; }\n",
    )
    .unwrap();
    assert_eq!(
        compile_in(&dir, "use.scss", "@use \"theme\";\n@use \"lib\";\na { b: c }\n"),
        "@charset \"UTF-8\";\n.t {\n  color: red;\n}\n\n.s {\n  content: \"é\";\n}\n\na {\n  b: c;\n}"
    );
    assert_eq!(
        compile_in(
            &dir,
            "imp.scss",
            "a { b: c }\n@import \"theme\";\n@import \"lib\";\n"
        ),
        "@charset \"UTF-8\";\na {\n  b: c;\n}\n\n.t {\n  color: red;\n}\n\n.s {\n  content: \"é\";\n}"
    );
    assert_eq!(
        compile_in(&dir, "only.scss", "@import \"theme\";\n"),
        ".t {\n  color: red;\n}"
    );
    // Only the file's top-level `@charset` goes: one inside an at-rule or a
    // style rule is kept verbatim, as dart keeps it.
    std::fs::write(
        dir.join("_nested.css"),
        "@media (min-width: 1px) {\n  @charset \"utf-8\";\n  .t { color: red; }\n}\n.u { @charset \"utf-8\"; color: blue; }\n",
    )
    .unwrap();
    assert_eq!(
        compile_in(&dir, "usenested.scss", "@use \"nested\";\n"),
        "@media (min-width: 1px) {\n  @charset \"utf-8\";\n  .t {\n    color: red;\n  }\n}\n.u {\n  @charset \"utf-8\";\n  color: blue;\n}"
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn nested_rules_keep_their_selector_lines() {
    // dart keeps a plain-CSS file's selector lists as written — a complex
    // selector that started on its own line still does, re-indented, and
    // runs of spaces collapse — for nested rules just like top-level ones
    // (the Lichess `recap` bundle, via the swiper stylesheet).
    let dir = scratch("sel");
    std::fs::write(
        dir.join("_sel.css"),
        "a,\nb {\n  x: 1;\n}\nc, d {\n  x: 2;\n}\n.p {\n  q: 3;\n  e,\n  f {\n    y: 1;\n  }\n  g, h {\n    y: 2;\n  }\n  i,   j,\n    k {\n    y: 3;\n  }\n}\n",
    )
    .unwrap();
    let expected = "a,\nb {\n  x: 1;\n}\n\nc, d {\n  x: 2;\n}\n\n.p {\n  q: 3;\n  e,\n  f {\n    y: 1;\n  }\n  g, h {\n    y: 2;\n  }\n  i, j,\n  k {\n    y: 3;\n  }\n}";
    assert_eq!(compile_in(&dir, "use.scss", "@use \"sel\";\n"), expected);
    assert_eq!(compile_in(&dir, "imp.scss", "@import \"sel\";\n"), expected);
    // A `&` part is no different: dart keeps the line it was written on
    // (`.child,\n  & {`), whether the module stands alone or is imported
    // under a Sass parent (its own top level joins the parent; the nested
    // rules stay native).
    std::fs::write(
        dir.join("_amp.css"),
        ".p {\n  .child,\n  & {\n    y: 1;\n  }\n  &,\n  .kid {\n    y: 2;\n  }\n  .a, &.b,\n  .c {\n    y: 3;\n  }\n}\n",
    )
    .unwrap();
    let body = "\n  .child,\n  & {\n    y: 1;\n  }\n  &,\n  .kid {\n    y: 2;\n  }\n  .a, &.b,\n  .c {\n    y: 3;\n  }\n}";
    assert_eq!(
        compile_in(&dir, "useamp.scss", "@use \"amp\";\n"),
        format!(".p {{{body}")
    );
    assert_eq!(
        compile_in(&dir, "nestamp.scss", "x {\n  @import \"amp\";\n}\n"),
        format!("x .p {{{body}")
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// The same, in COMPRESSED style — which is the only thing that tells a value
/// dart kept TYPED from one it kept as text.
fn compile_compressed_in(dir: &std::path::Path, name: &str, src: &str) -> String {
    let entry = dir.join(name);
    std::fs::write(&entry, src).unwrap();
    let url = entry.to_string_lossy().into_owned();
    let imp = FsImporter::new(Vec::new());
    let opts = Options::default()
        .with_importer(&imp)
        .with_url(&url)
        .with_style(sasso::OutputStyle::Compressed);
    compile(src, &opts).expect("compile")
}

/// A value in a loaded `.css` file is a VALUE, not frozen text: dart parses it
/// and re-serializes it for the output style, so compressing shortens its
/// numbers, its hex colours and its list separators. Every expectation below
/// was measured against dart-sass 1.103.1.
#[test]
fn a_loaded_files_values_serialize_for_the_output_style() {
    let dir = scratch("values");
    let case = |file: &str, decl: &str, expanded: &str, compressed: &str| {
        std::fs::write(dir.join(format!("_{file}.css")), format!(".a {{ b: {decl}; }}\n")).unwrap();
        let src = format!("@use \"{file}\";\n");
        assert_eq!(
            compile_in(&dir, &format!("entry_{file}.scss"), &src),
            format!(".a {{\n  b: {expanded};\n}}"),
            "expanded {decl}"
        );
        assert_eq!(
            compile_compressed_in(&dir, &format!("entry_{file}_c.scss"), &src),
            format!(".a{{b:{compressed}}}"),
            "compressed {decl}"
        );
    };
    // Numbers shorten, in a list and behind `!important` as well.
    case("num", "0.5px", "0.5px", ".5px");
    case("neg", "-0.5px", "-0.5px", "-0.5px");
    case("list", "1px 0.5px", "1px 0.5px", "1px .5px");
    case("clist", "1px, 0.5px", "1px, 0.5px", "1px,.5px");
    // Hex colours take their shortest form.
    case("hex6", "#cccccc", "#cccccc", "#ccc");
    case("hex6u", "#CCCCCC", "#CCCCCC", "#ccc");
    case("hex8", "#ccccccff", "#cccccc", "#ccc");
    case("hexname", "#ff0000", "#ff0000", "red");
    case(
        "shorthand",
        "1px solid #cccccc",
        "1px solid #cccccc",
        "1px solid #ccc",
    );
    // A calculation is a calculation.
    case(
        "calc",
        "calc(100% - 2 * var(--x))",
        "calc(100% - 2 * var(--x))",
        "calc(100% - 2*var(--x))",
    );
}

/// A colour KEYWORD is a Sass value, not a CSS one — dart's CSS parser leaves
/// `white` an identifier, so it keeps its own spelling (and its case) instead
/// of compressing to `#fff`. Measured against dart-sass 1.103.1.
#[test]
fn a_loaded_files_colour_keywords_stay_identifiers() {
    let dir = scratch("keywords");
    let case = |file: &str, decl: &str, want: &str| {
        std::fs::write(dir.join(format!("_{file}.css")), format!(".a {{ b: {decl}; }}\n")).unwrap();
        let src = format!("@use \"{file}\";\n");
        assert_eq!(
            compile_compressed_in(&dir, &format!("entry_{file}.scss"), &src),
            format!(".a{{b:{want}}}"),
            "{decl}"
        );
    };
    // The keyword survives; only the LIST around it compresses.
    for (i, (decl, want)) in [
        ("white", "white"),
        ("black", "black"),
        ("magenta", "magenta"),
        ("rebeccapurple", "rebeccapurple"),
        ("transparent", "transparent"),
        ("WHITE", "WHITE"),
        ("1px solid white", "1px solid white"),
        ("white, black", "white,black"),
    ]
    .iter()
    .enumerate()
    {
        case(&format!("kw{i}"), decl, want);
    }
    // Written as SCSS the same keyword IS a colour, and compresses.
    let dir2 = scratch("keywords_scss");
    assert_eq!(
        compile_compressed_in(&dir2, "s.scss", ".a { b: white; }"),
        ".a{b:#fff}"
    );
}

/// A function call in a loaded `.css` file becomes a STRING, and dart builds
/// that string with the DEFAULT style whatever the output style is — so its
/// arguments keep their leading zeros and their `, ` even when compressing,
/// while the separator is normalised from the source. Measured against
/// dart-sass 1.103.1.
#[test]
fn a_loaded_files_function_call_serializes_in_the_default_style() {
    let dir = scratch("calls");
    let case = |file: &str, decl: &str, want: &str| {
        std::fs::write(dir.join(format!("_{file}.css")), format!(".a {{ b: {decl}; }}\n")).unwrap();
        let src = format!("@use \"{file}\";\n");
        assert_eq!(
            compile_compressed_in(&dir, &format!("entry_{file}.scss"), &src),
            format!(".a{{b:{want}}}"),
            "{decl}"
        );
    };
    case("rgb", "rgb(255,255,255)", "rgb(255, 255, 255)");
    case("rgba", "rgba(0, 0, 0, 0.15)", "rgba(0, 0, 0, 0.15)");
    case("unk", "unknownfn(0.5px,2px)", "unknownfn(0.5px, 2px)");
    case("nest", "nested(inner(0.5), 2)", "nested(inner(0.5), 2)");
    case("tr", "translate(0.5px,-0.5px)", "translate(0.5px, -0.5px)");
}

/// A nested plain-CSS rule holding nothing that survives compression — a
/// comment, or another such rule — is not written at all, and neither is the
/// rule left empty around it. (`swiper-bundle.css` ships exactly this.)
/// Measured against dart-sass 1.103.1.
#[test]
fn a_loaded_files_comment_only_rule_vanishes_when_compressed() {
    let dir = scratch("emptyrule");
    let case = |file: &str, css: &str, want: &str| {
        std::fs::write(dir.join(format!("_{file}.css")), css).unwrap();
        assert_eq!(
            compile_compressed_in(
                &dir,
                &format!("entry_{file}.scss"),
                &format!("@use \"{file}\";\n")
            ),
            want,
            "{css}"
        );
    };
    case("only", ".a {\n  .b {\n    /* c */\n  }\n}\n", "");
    case(
        "deep",
        ".a {\n  .b {\n    .c {\n      /* c */\n    }\n  }\n}\n",
        "",
    );
    case("at", ".a {\n  @media x {\n    /* c */\n  }\n}\n", "");
    // A sibling that DOES write keeps its rule.
    case(
        "sibling",
        ".a {\n  c: 1;\n  .b {\n    /* c */\n  }\n}\n",
        ".a{c:1}",
    );
    // And a LOUD comment writes, so everything around it stays.
    case("loud", ".a {\n  .b {\n    /*! c */\n  }\n}\n", ".a{.b{/*! c */}}");
}
