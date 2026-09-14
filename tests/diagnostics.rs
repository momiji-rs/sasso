//! Byte-exact stderr diagnostics parity against the dart-sass 1.100 fixtures.
//!
//! For each `tests/fixtures/diagnostics/<name>.scss` that ships a captured
//! `<name>.ascii.stderr` (dart-sass `--no-unicode`), run the `sasso` binary
//! from inside that directory with the bare basename and `--no-unicode` (the
//! exact way the fixtures were captured) and assert sasso's stderr is
//! byte-identical.
//!
//! The set of fixtures that currently match is gated by an allow-list so the
//! test stays green while later sub-steps (deprecations, the multi-span
//! renderer, `@import` source-swap) grow it. Each non-matching fixture is
//! listed with the reason it is skipped.

use std::path::Path;
use std::process::Command;

use sasso::{compile, Options};

/// Fixtures whose `--no-unicode` stderr sasso reproduces byte-for-byte today.
const MATCHING: &[&str] = &[
    // @debug — single line, no snippet/frames.
    "debug-string",
    "debug-values",
    // @warn — WARNING: + 4-space frame trace + blank line.
    "warn-plain",
    "warn-interpolated",
    "warn-in-mixin",
    // @error — snippet at the call site, 2-space frame trace.
    "error-plain",
    "error-interpolated",
    "error-in-mixin",
    "error-stack-nested",
    "error-in-function",
    "error-cross-file",
    // Compile errors with a positioned span (undefined variable).
    "compile-undefined-variable",
    "compile-undefined-variable-stack",
    "compile-tab-expansion",
    "compile-gutter-alignment",
    // Deprecations (registry sub-step): the fully-static `@import` warning.
    "deprecation-import",
    // The global-built-in family, and the two that ride along with it.
    "deprecation-global-builtin",
    "deprecation-feature-exists",
    "deprecation-call-string",
];

fn fixtures_dir() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/diagnostics")
}

/// Run sasso from inside the fixtures dir with the bare basename + --no-unicode,
/// returning captured stderr verbatim.
fn run_sasso_stderr(name: &str) -> String {
    let bin = env!("CARGO_BIN_EXE_sasso");
    let out = Command::new(bin)
        .current_dir(fixtures_dir())
        .arg("--no-unicode")
        .arg(format!("{name}.scss"))
        .output()
        .expect("run sasso");
    String::from_utf8_lossy(&out.stderr).into_owned()
}

#[test]
fn diagnostics_match_dart_ascii_fixtures() {
    let dir = fixtures_dir();
    for name in MATCHING {
        let expected = std::fs::read_to_string(dir.join(format!("{name}.ascii.stderr")))
            .unwrap_or_else(|e| panic!("read {name}.ascii.stderr: {e}"));
        let got = run_sasso_stderr(name);
        assert_eq!(got, expected, "stderr mismatch for fixture {name}");
    }
}

/// Sanity: an `@error` exits 65 and a `@warn` exits 0, matching dart-sass.
#[test]
fn diagnostics_exit_codes() {
    let bin = env!("CARGO_BIN_EXE_sasso");
    let dir = fixtures_dir();
    let error_code = Command::new(bin)
        .current_dir(&dir)
        .arg("--no-unicode")
        .arg("error-plain.scss")
        .output()
        .expect("run")
        .status
        .code();
    assert_eq!(error_code, Some(65), "@error must exit 65");

    let warn_code = Command::new(bin)
        .current_dir(&dir)
        .arg("--no-unicode")
        .arg("warn-plain.scss")
        .output()
        .expect("run")
        .status
        .code();
    assert_eq!(warn_code, Some(0), "@warn must exit 0");
}

#[test]
fn invalid_utf8_input_exits_65() {
    let bin = env!("CARGO_BIN_EXE_sasso");
    let dir = std::env::temp_dir().join(format!("sasso_invalid_utf8_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create temp dir");
    let input = dir.join("input.scss");
    std::fs::write(&input, b"foo{;\xF6\xFC").expect("write invalid utf8");

    let out = Command::new(bin).arg(&input).output().expect("run");
    let _ = std::fs::remove_dir_all(&dir);

    assert_eq!(out.status.code(), Some(65));
    assert_eq!(String::from_utf8_lossy(&out.stderr), "Error: Invalid UTF-8.\n");
}

/// The rendered diagnostic block for `src`, compiled under `url` so snippets
/// and frames are produced.
fn err_block(src: &str, url: &str) -> String {
    compile(src, &Options::default().with_url(url))
        .expect_err("expected a compile error")
        .to_string()
}

/// The caret line of a rendered block, trimmed — so a span that is one
/// character too long fails instead of passing a substring check.
fn caret_line(block: &str) -> String {
    block
        .lines()
        .find(|l| l.contains('^'))
        .unwrap_or_else(|| panic!("no caret line in:\n{block}"))
        // Drop the gutter (`  \u{2502} `) and the indentation before the run.
        .rsplit('\u{2502}')
        .next()
        .unwrap_or_default()
        .trim()
        .to_string()
}

#[test]
fn a_module_diagnostic_carets_the_construct_it_is_about() {
    // dart spans the whole rule, call or reference a diagnostic is about;
    // sasso drew a single caret (or none at all, leaving the error with no
    // snippet). Every span below was measured against dart-sass 1.103.1.
    let cases = [
        // a failed load carets the whole `@use`/`@forward`
        ("@use \"nope\";\n", "1 \u{2502} @use \"nope\";\n", "^^^^^^^^^^^\n"),
        (
            "@forward \"nope\";\n",
            "1 \u{2502} @forward \"nope\";\n",
            "^^^^^^^^^^^^^^^\n",
        ),
        // a misplaced module rule carets all of itself, trailing space and all
        (
            ".a { b: c }\n@use \"nope\"   ;\n",
            "2 \u{2502} @use \"nope\"   ;\n",
            "^^^^^^^^^^^^^^\n",
        ),
        // an undefined mixin carets the `@include`, args and all
        (
            ".a { @include nope; }\n",
            "1 \u{2502} .a { @include nope; }\n",
            "^^^^^^^^^^^^^\n",
        ),
        (
            ".a { @include nope(1); }\n",
            "1 \u{2502} .a { @include nope(1); }\n",
            "^^^^^^^^^^^^^^^^\n",
        ),
        // an error about the RULE carets all of it — `using` clause and
        // content block included (dart's `span`, not `spanWithoutContent`)
        (
            ".a { @include nope using ($x) { c: $x; } }\n",
            "1 \u{2502} .a { @include nope using ($x) { c: $x; } }\n",
            "^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^\n",
        ),
        (
            ".a { @include nope { c: d; } }\n",
            "1 \u{2502} .a { @include nope { c: d; } }\n",
            "^^^^^^^^^^^^^^^^^^^^^^^\n",
        ),
        // a module rule's span ends at the `;`, the whitespace before it
        // included, `with` clause or not
        (
            "@use \"nope\" with ($x: 1)   ;\n",
            "1 \u{2502} @use \"nope\" with ($x: 1)   ;\n",
            "^^^^^^^^^^^^^^^^^^^^^^^^^^^\n",
        ),
        (
            "@forward \"nope\" with ($x: 1)   ;\n",
            "1 \u{2502} @forward \"nope\" with ($x: 1)   ;\n",
            "^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^\n",
        ),
        // a built-in namespace, and a namespace that is not bound at all
        (
            "@use \"sass:math\";\n.a { b: math.nope(1); }\n",
            "2 \u{2502} .a { b: math.nope(1); }\n",
            "^^^^^^^^^^^^\n",
        ),
        (
            ".a { b: nope.foo(1); }\n",
            "1 \u{2502} .a { b: nope.foo(1); }\n",
            "^^^^^^^^^^^\n",
        ),
        // an unbound namespace on an `@include` carets the whole rule
        (
            ".a { @include nope.pub; }\n",
            "1 \u{2502} .a { @include nope.pub; }\n",
            "^^^^^^^^^^^^^^^^^\n",
        ),
        // a built-in mixin's own failure carets the invocation
        (
            "@use \"sass:meta\";\n.a { @include meta.load-css(\"nope\"); }\n",
            "2 \u{2502} .a { @include meta.load-css(\"nope\"); }\n",
            "^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^\n",
        ),
    ];
    for (src, line, caret) in cases {
        let block = err_block(src, "in.scss");
        assert!(block.contains(line), "{src:?}\n{block}");
        assert_eq!(caret_line(&block), caret.trim(), "{src:?}\n{block}");
    }
    // The mixin is not named in the message — the span says which one it is.
    assert!(
        err_block(".a { @include nope; }\n", "in.scss").starts_with("Error: Undefined mixin.\n"),
        "{}",
        err_block(".a { @include nope; }\n", "in.scss")
    );
    // A missing stylesheet names no url either.
    assert!(
        err_block("@import \"nope\";\n", "in.scss").contains("Error: Can't find stylesheet to import.\n"),
        "{}",
        err_block("@import \"nope\";\n", "in.scss")
    );
}

#[test]
fn a_namespaced_member_diagnostic_points_at_the_reference() {
    // `ns.$var` carried no position at all, so its "Undefined variable." was
    // reported against line 1 column 1 — the `@use` line, not the reference.
    let dir = std::env::temp_dir().join(format!("sasso_ns_diag_{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("mkdir");
    std::fs::write(
        dir.join("_lib.scss"),
        "@mixin -priv { a: b; }\n$pub: 1;\n$-pv: 1;\n@mixin -pr\\69 v { a: b; }\n",
    )
    .expect("write");
    let entry = dir.join("in.scss");
    let url = entry.to_string_lossy().into_owned();
    let imp = sasso::FsImporter::new(Vec::new());
    let run = |src: &str| {
        std::fs::write(&entry, src).unwrap();
        compile(src, &Options::default().with_importer(&imp).with_url(&url))
            .expect_err("expected a compile error")
            .to_string()
    };
    let block = run("@use \"lib\";\n.a { b: lib.$nope; }\n");
    assert!(block.contains("2 \u{2502} .a { b: lib.$nope; }\n"), "{block}");
    assert_eq!(caret_line(&block), "^^^^^^^^^", "{block}");
    assert!(block.contains("in.scss 2:9"), "{block}");
    // A namespaced call spans the call; a private member spans its name.
    let block = run("@use \"lib\";\n.a { b: lib.nope(1); }\n");
    assert_eq!(caret_line(&block), "^^^^^^^^^^^", "{block}");
    let block = run("@use \"lib\";\n.a { @include lib.-priv; }\n");
    assert_eq!(caret_line(&block), "^^^^^", "{block}");
    assert!(block.contains("in.scss 2:19"), "{block}");
    // The caret covers the member AS WRITTEN, so an escape inside it counts
    // its source bytes rather than the decoded ones.
    let block = run("@use \"lib\";\n.a { @include lib.-pr\\69 v; }\n");
    assert_eq!(caret_line(&block), "^^^^^^^^", "{block}");
    // A private VARIABLE reference carets the whole `ns.$name`.
    let block = run("@use \"lib\";\n.a { b: lib.$-pv; }\n");
    assert_eq!(caret_line(&block), "^^^^^^^^", "{block}");
    assert!(block.contains("in.scss 2:9"), "{block}");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_span_that_crosses_lines_ends_where_dart_ends_it() {
    // A CRLF terminator is two bytes; counting it as one walked the end of a
    // multi-line span a line too far. dart ends this one on the `}` line.
    let block = err_block(
        ".a {\r\n  @include nope {\r\n    c: d;\r\n  }\r\n}\r\n",
        "in.scss",
    );
    assert!(block.contains("4 \u{2502} \u{2514}   }\n"), "{block}");
    assert!(!block.contains("5 \u{2502}"), "{block}");
    // The same file with LF terminators ends in the same place.
    let lf = err_block(".a {\n  @include nope {\n    c: d;\n  }\n}\n", "in.scss");
    assert_eq!(block.replace("\r", ""), lf);
}

#[test]
fn an_indented_include_carets_its_own_line() {
    // The braces around an indented child block are written by the front end,
    // so the text they enclose is not a source span: the caret stays on the
    // call. (dart spans the children; that needs a reconstruction-to-source
    // mapping the front end does not have.)
    let block = compile(
        ".a\n  @include nope\n    c: d\n",
        &Options::default()
            .with_syntax(sasso::Syntax::Sass)
            .with_url("in.sass"),
    )
    .expect_err("expected a compile error")
    .to_string();
    assert_eq!(caret_line(&block), "^^^^^^^^^^^^^", "{block}");
    assert!(block.contains("in.sass 2:3"), "{block}");
}

#[test]
fn an_escaped_member_name_is_not_private() {
    // dart reads privacy from the LITERAL spelling: `ns.-priv` is private,
    // `ns.\2d priv` is an ordinary member it then fails to find. A private
    // member is not in a module's public view, so the failure is "not found",
    // not "private".
    let dir = std::env::temp_dir().join(format!("sasso_esc_priv_{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("mkdir");
    std::fs::write(
        dir.join("_lib.scss"),
        "@mixin -priv { a: b; }\n@function -pf() { @return 1; }\n$-pv: 1;\n",
    )
    .expect("write");
    let entry = dir.join("in.scss");
    let url = entry.to_string_lossy().into_owned();
    let imp = sasso::FsImporter::new(Vec::new());
    let run = |src: &str| {
        std::fs::write(&entry, src).unwrap();
        compile(src, &Options::default().with_importer(&imp).with_url(&url))
            .expect_err("expected a compile error")
            .to_string()
    };
    let block = run("@use \"lib\";\n.a { @include lib.\\2d priv; }\n");
    assert!(block.starts_with("Error: Undefined mixin.\n"), "{block}");
    assert_eq!(caret_line(&block), "^^^^^^^^^^^^^^^^^^^^^", "{block}");
    let block = run("@use \"lib\";\n.a { b: lib.$\\2d pv; }\n");
    assert!(block.starts_with("Error: Undefined variable.\n"), "{block}");
    assert_eq!(caret_line(&block), "^^^^^^^^^^^", "{block}");
    let block = run("@use \"lib\";\n.a { b: lib.\\2d pf(); }\n");
    assert!(block.starts_with("Error: Undefined function.\n"), "{block}");
    // A LITERAL private member keeps dart's privacy error.
    let block = run("@use \"lib\";\n.a { @include lib.-priv; }\n");
    assert!(
        block.starts_with("Error: Private members can't be accessed from outside their modules.\n"),
        "{block}"
    );
    // The by-name API reports it missing, with the name it was asked for.
    let block = run(
        "@use \"sass:meta\";\n@use \"lib\";\n.a { b: meta.call(meta.get-function(\"-pf\", $module: \"lib\")); }\n",
    );
    assert!(
        block.starts_with("Error: Function not found: \"-pf\"\n"),
        "{block}"
    );
    assert_eq!(caret_line(&block), "^".repeat(40), "{block}");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_missing_argument_points_at_the_invocation() {
    // dart reports a missing argument against the CALL — its primary span —
    // whichever path made the call, and reads the snippet from the CALLER's
    // file even though the callee's file is the current one by then.
    let dir = std::env::temp_dir().join(format!("sasso_missing_arg_{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("mkdir");
    std::fs::write(
        dir.join("_lib.scss"),
        "@function f($x) { @return $x; }\n@mixin m($x) { a: $x; }\n",
    )
    .expect("write");
    let entry = dir.join("in.scss");
    let url = entry.to_string_lossy().into_owned();
    let imp = sasso::FsImporter::new(Vec::new());
    let run = |src: &str| {
        std::fs::write(&entry, src).unwrap();
        compile(src, &Options::default().with_importer(&imp).with_url(&url))
            .expect_err("expected a compile error")
            .to_string()
    };
    // A function in the same file.
    let block = run("@function f($x) { @return $x; }\n.a { b: f(); }\n");
    assert!(block.starts_with("Error: Missing argument $x.\n"), "{block}");
    assert!(block.contains("2 \u{2502} .a { b: f(); }\n"), "{block}");
    assert_eq!(caret_line(&block), "^^^", "{block}");
    // A function in ANOTHER file: the snippet is the caller's line, not the
    // definition's.
    let block = run("@use \"lib\";\n.a { b: lib.f(); }\n");
    assert!(block.contains("2 \u{2502} .a { b: lib.f(); }\n"), "{block}");
    assert_eq!(caret_line(&block), "^^^^^^^", "{block}");
    assert!(block.contains("in.scss 2:9  f()"), "{block}");
    // The same for a mixin.
    let block = run("@use \"lib\";\n.a { @include lib.m; }\n");
    assert!(block.contains("2 \u{2502} .a { @include lib.m; }\n"), "{block}");
    assert_eq!(caret_line(&block), "^^^^^^^^^^^^^^", "{block}");
    // And through `meta.call`, which invokes a reference.
    let block = run(
        "@use \"sass:meta\";\n@function f($x) { @return $x; }\n.a { b: meta.call(meta.get-function(\"f\")); }\n",
    );
    assert_eq!(caret_line(&block), "^".repeat(33), "{block}");
    // A built-in re-exported through `@forward` reports like a direct one.
    std::fs::write(dir.join("_fwd.scss"), "@forward \"sass:math\";\n").expect("write");
    let block = run("@use \"fwd\";\n.a { b: fwd.div(1); }\n");
    assert_eq!(caret_line(&block), "^^^^^^^^^^", "{block}");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_multi_line_span_uses_the_gutter_at_a_line_edge() {
    // dart (source_span) writes the arm glyph in the GUTTER when the span
    // begins at its line's first non-whitespace character, and likewise when it
    // ends at its line's last; it draws an arrow row only for an end that
    // starts or stops mid-line. The two ends are decided separately.
    let both = err_block(".a {\n  @include nope {\n    c: d;\n  }\n}\n", "in.scss");
    assert!(
        both.contains(
            "2 \u{2502} \u{250c}   @include nope {\n3 \u{2502} \u{2502}     c: d;\n4 \u{2502} \u{2514}   }\n"
        ),
        "{both}"
    );
    assert!(!both.contains('^'), "{both}");
    // Starts mid-line, ends at the line's last character: an opening arrow row
    // and a closing gutter glyph.
    let open = err_block(".a { @include nope {\n    c: d;\n  }\n}\n", "in.scss");
    assert!(
        open.contains("\u{2502} \u{250c}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}^\n"),
        "{open}"
    );
    assert!(open.contains("3 \u{2502} \u{2514}   }\n"), "{open}");
    // Starts at the line's first character, ends mid-line: the mirror image.
    let close = err_block(".a {\n  @include nope {\n    c: d;\n  } x: y;\n}\n", "in.scss");
    assert!(
        close.contains("2 \u{2502} \u{250c}   @include nope {\n"),
        "{close}"
    );
    assert!(
        close.contains("\u{2502} \u{2514}\u{2500}\u{2500}\u{2500}^\n"),
        "{close}"
    );
}

#[test]
fn an_error_at_the_end_of_a_file_points_at_the_last_line_with_content() {
    // dart's scanner never advances into a file's trailing whitespace, so an
    // "expected …" at the end of one points at the end of the last line that
    // says something — trailing spaces on that line included.
    for (src, line, caret_col) in [
        (".a { b: c\n", 1, 10),
        (".a {\n", 1, 5),
        ("@media screen {\n  .a { b: c }\n", 2, 14),
        (".a { b: c; \n\n\n", 1, 12),
    ] {
        let block = err_block(src, "in.scss");
        assert!(block.starts_with("Error: expected \"}\".\n"), "{src:?}\n{block}");
        assert!(
            block.contains(&format!("in.scss {line}:{caret_col}")),
            "{src:?}\n{block}"
        );
        // The snippet shows that line, not the blank one after it.
        assert!(block.contains(&format!("{line} \u{2502} ")), "{src:?}\n{block}");
    }
}

#[test]
fn a_value_that_cannot_start_reports_what_was_expected() {
    // dart names what it WANTED — an expression — whatever it found there: a
    // character that cannot start one, or the end of the file.
    for src in [
        ".a { b: ; }\n",
        ".a { b: 1 + ; }\n",
        ".a { b: ) }\n",
        "@if  { a: b; }\n",
        ".a { b: \n",
        ".a { b: 1 +\n",
    ] {
        let block = err_block(src, "in.scss");
        assert!(
            block.starts_with("Error: Expected expression.\n"),
            "{src:?}\n{block}"
        );
    }
}

/// Every `formatted` warning a compile of `src` produces.
fn warnings(src: &str, url: &str) -> Vec<String> {
    use std::cell::RefCell;
    use std::rc::Rc;
    let seen: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
    let sink = Rc::clone(&seen);
    let opts =
        Options::default()
            .with_url(url)
            .with_warn_handler(Rc::new(move |ev: &sasso::WarnEvent<'_>| {
                sink.borrow_mut().push(ev.formatted.to_string());
            }));
    let _ = compile(src, &opts);
    let out = seen.borrow().clone();
    out
}

#[test]
fn a_global_builtin_with_a_module_form_is_deprecated() {
    // dart names the member to use and carets the whole call. The mapping is
    // not mechanical — every entry below was measured against dart-sass
    // 1.103.1 — so the table is checked, not just the machinery.
    for (src, replacement, caret) in [
        (".a { b: map-get((x: 1), x); }\n", "map.get", "^^^^^^^^^^^^^^^^^^"),
        (".a { b: nth(1 2 3, 1); }\n", "list.nth", "^^^^^^^^^^^^^"),
        (
            ".a { b: percentage(0.5); }\n",
            "math.percentage",
            "^^^^^^^^^^^^^^^",
        ),
        (
            ".a { b: lighten(#fff, 10%); }\n",
            "color.adjust",
            "^^^^^^^^^^^^^^^^^^",
        ),
        (".a { b: unitless(1); }\n", "math.is-unitless", "^^^^^^^^^^^"),
        (
            ".a { b: comparable(1px, 2px); }\n",
            "math.compatible",
            "^^^^^^^^^^^^^^^^^^^^",
        ),
        (
            ".a { b: list-separator(1 2); }\n",
            "list.separator",
            "^^^^^^^^^^^^^^^^^^^",
        ),
        (
            ".a { b: str-length(\"abc\"); }\n",
            "string.length",
            "^^^^^^^^^^^^^^^^^",
        ),
        (".a { b: type-of(1); }\n", "meta.type-of", "^^^^^^^^^^"),
        (
            ".a { b: selector-parse(\"a\"); }\n",
            "selector.parse",
            "^^^^^^^^^^^^^^^^^^",
        ),
    ] {
        let w = warnings(src, "in.scss");
        assert_eq!(w.len(), 1, "{src:?} -> {w:?}");
        assert!(
            w[0].starts_with(&format!(
                "DEPRECATION WARNING [global-builtin]: Global built-in functions are deprecated and will be removed in Dart Sass 3.0.0.\nUse {replacement} instead.\n\nMore info and automated migrator: https://sass-lang.com/d/import\n"
            )),
            "{src:?}\n{}",
            w[0]
        );
        assert!(w[0].contains(caret), "{src:?}\n{}", w[0]);
    }
    // A global dart KEEPS is not deprecated: a CSS function it shares a name
    // with, or one with no module form.
    for src in [
        ".a { b: abs(-1); }\n",
        ".a { b: round(1.5); }\n",
        ".a { b: min(1, 2); }\n",
        ".a { b: rgba(0, 0, 0, 0.5); }\n",
        ".a { b: ie-hex-str(#fff); }\n",
    ] {
        assert!(warnings(src, "in.scss").is_empty(), "{src:?}");
    }
    // The module form itself is never deprecated.
    assert!(warnings(
        "@use \"sass:math\";\n.a { b: math.percentage(0.5); }\n",
        "in.scss"
    )
    .is_empty());
    // The `sass:meta` predicates resolve against evaluator state and return
    // before the generic built-in dispatch: they are deprecated too.
    for (src, replacement) in [
        (
            "$x: 1;\n.a { b: variable-exists(\"x\"); }\n",
            "meta.variable-exists",
        ),
        (
            "$x: 1;\n.a { b: global-variable-exists(\"x\"); }\n",
            "meta.global-variable-exists",
        ),
        (
            "@mixin m {}\n.a { b: mixin-exists(\"m\"); }\n",
            "meta.mixin-exists",
        ),
        (
            ".a { b: function-exists(\"percentage\"); }\n",
            "meta.function-exists",
        ),
    ] {
        let w = warnings(src, "in.scss");
        assert!(
            w.iter()
                .any(|x| x.contains(&format!("Use {replacement} instead."))),
            "{src:?}\n{w:?}"
        );
    }
    // The name is matched EXACTLY: dart resolves these case-sensitively, so an
    // upper-case spelling is plain CSS to it and carries no warning.
    for src in [
        ".a { b: MAP-GET((x: 1), x); }\n",
        ".a { b: Lighten(#fff, 10%); }\n",
        ".a { b: PERCENTAGE(0.5); }\n",
    ] {
        assert!(warnings(src, "in.scss").is_empty(), "{src:?}");
    }
    // The indented syntax reports it too, at its own position.
    let seen: std::rc::Rc<std::cell::RefCell<Vec<String>>> =
        std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let sink = std::rc::Rc::clone(&seen);
    let opts = Options::default()
        .with_syntax(sasso::Syntax::Sass)
        .with_url("in.sass")
        .with_warn_handler(std::rc::Rc::new(move |ev: &sasso::WarnEvent<'_>| {
            sink.borrow_mut().push(ev.formatted.to_string());
        }));
    let _ = compile(".a\n  b: nth(1 2, 1)\n", &opts);
    let w = seen.borrow().clone();
    assert_eq!(w.len(), 1, "{w:?}");
    assert!(w[0].contains("in.sass 2:6"), "{}", w[0]);
}

#[test]
fn a_deprecated_function_reached_indirectly_still_warns() {
    // dart reports the global built-in a `call()` reaches — by name or through
    // a reference — against the INVOCATION, on top of the warnings for `call`
    // and `get-function` themselves. Two `[global-builtin]` warnings can
    // therefore share one span, saying different things.
    let w = warnings(".a { b: call(get-function(\"percentage\"), 0.5); }\n", "in.scss");
    assert_eq!(w.len(), 3, "{w:?}");
    assert!(w[0].contains("Use meta.get-function instead."), "{}", w[0]);
    assert!(w[1].contains("Use meta.call instead."), "{}", w[1]);
    assert!(w[2].contains("Use math.percentage instead."), "{}", w[2]);
    assert!(
        w[1].contains("in.scss 1:9") && w[2].contains("in.scss 1:9"),
        "{w:?}"
    );
    // The string form adds `[call-string]`, with the name it was given.
    let w = warnings(
        "@function foo() { @return 1; }\n.a { b: call(\"foo\"); }\n",
        "in.scss",
    );
    assert_eq!(w.len(), 2, "{w:?}");
    assert!(w[0].contains("Use meta.call instead."), "{}", w[0]);
    assert!(
        w[1].starts_with("DEPRECATION WARNING [call-string]: Passing a string to call() is deprecated and will be illegal in Dart Sass 2.0.0.\n\nRecommendation: call(get-function(\"foo\"))\n"),
        "{}",
        w[1]
    );
    // `feature-exists` is deprecated whichever way it is spelled.
    let w = warnings(".a { b: feature-exists(\"at-error\"); }\n", "in.scss");
    assert_eq!(w.len(), 2, "{w:?}");
    assert!(w[1].starts_with("DEPRECATION WARNING [feature-exists]: The feature-exists() function is deprecated.\n\nMore info: https://sass-lang.com/d/feature-exists\n"), "{}", w[1]);
    let w = warnings(
        "@use \"sass:meta\";\n.a { b: meta.feature-exists(\"at-error\"); }\n",
        "in.scss",
    );
    assert_eq!(w.len(), 1, "{w:?}");
    assert!(w[0].contains("[feature-exists]"), "{}", w[0]);
}

#[test]
fn a_deprecation_follows_the_call_that_is_actually_made() {
    // Whether a call is deprecated depends on what it RESOLVES to, not on how
    // it is spelled. Every expectation here was measured against dart-sass
    // 1.103.1.
    let dir = std::env::temp_dir().join(format!("sasso_dep_resolve_{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("mkdir");
    std::fs::write(dir.join("_fwdmeta.scss"), "@forward \"sass:meta\";\n").expect("write");
    let entry = dir.join("in.scss");
    let url = entry.to_string_lossy().into_owned();
    let imp = sasso::FsImporter::new(Vec::new());
    let run = |src: &str| {
        std::fs::write(&entry, src).unwrap();
        let seen: std::rc::Rc<std::cell::RefCell<Vec<String>>> =
            std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let sink = std::rc::Rc::clone(&seen);
        let opts = Options::default()
            .with_importer(&imp)
            .with_url(&url)
            .with_warn_handler(std::rc::Rc::new(move |ev: &sasso::WarnEvent<'_>| {
                sink.borrow_mut().push(ev.formatted.to_string());
            }));
        let _ = compile(src, &opts);
        let out = seen.borrow().clone();
        out
    };
    // A built-in module bound to an alias is still that module.
    let w = run("@use \"sass:meta\" as m;\n.a { b: m.feature-exists(\"at-error\"); }\n");
    assert_eq!(w.len(), 1, "{w:?}");
    assert!(w[0].contains("[feature-exists]"), "{}", w[0]);
    // So is one reached through a `@forward`.
    let w = run("@use \"fwdmeta\" as m;\n.a { b: m.feature-exists(\"at-error\"); }\n");
    assert_eq!(w.len(), 1, "{w:?}");
    assert!(w[0].contains("[feature-exists]"), "{}", w[0]);
    // A member exposed by `@use … as *` is that module's, not a global: the
    // function's own deprecation fires, the global-built-in one does not.
    let w = run("@use \"sass:meta\" as *;\n.a { b: feature-exists(\"at-error\"); }\n");
    assert_eq!(w.len(), 1, "{w:?}");
    assert!(w[0].contains("[feature-exists]"), "{}", w[0]);
    // A user `@function` wins over the global, so nothing is deprecated.
    assert!(run("@function type-of($x) { @return 1; }\n.a { b: type-of(2); }\n").is_empty());
    // A deprecated call INSIDE a deprecated one is reported first, as dart
    // reports it.
    let w = run("@use \"sass:meta\";\n.a { b: meta.feature-exists(inspect(\"at-error\")); }\n");
    assert_eq!(w.len(), 2, "{w:?}");
    assert!(w[0].contains("Use meta.inspect instead."), "{}", w[0]);
    assert!(w[1].contains("[feature-exists]"), "{}", w[1]);
    // A plain-CSS reference invokes no Sass built-in.
    assert!(run(
        "@use \"sass:meta\";\n.a { b: meta.call(meta.get-function(\"percentage\", $css: true), 1); }\n"
    )
    .is_empty());
    // `call()`'s recommendation quotes the name as Sass would write it.
    let w = run("@function a\\\"b() { @return 1; }\n.a { b: call(\"a\\\\\\\"b\"); }\n");
    assert!(
        w.iter()
            .any(|x| x.contains("Recommendation: call(get-function('a\\\\\"b'))")),
        "{w:?}"
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn what_a_call_resolves_to_decides_what_is_deprecated() {
    // The underscore spelling reaches the same built-in, so it carries the same
    // deprecation; a reference taken from a module is not the global. Every
    // expectation measured against dart-sass 1.103.1.
    let w = warnings("a { b: map_get((x: 1), x); }\n", "in.scss");
    assert_eq!(w.len(), 1, "{w:?}");
    assert!(w[0].contains("Use map.get instead."), "{}", w[0]);
    // The caret covers the call as written, underscore and all.
    assert!(w[0].contains("^^^^^^^^^^^^^^^^^^"), "{}", w[0]);
    let w = warnings(
        "@use \"sass:meta\";\na { b: meta.feature_exists(\"at-error\"); }\n",
        "in.scss",
    );
    assert_eq!(w.len(), 1, "{w:?}");
    assert!(w[0].contains("[feature-exists]"), "{}", w[0]);
    let w = warnings(
        "@use \"sass:meta\" as *;\na { b: feature_exists(\"at-error\"); }\n",
        "in.scss",
    );
    assert_eq!(w.len(), 1, "{w:?}");
    assert!(w[0].contains("[feature-exists]"), "{}", w[0]);
    // A reference taken THROUGH a module is that module's member: the global
    // spelling is what is deprecated, and this is not it.
    assert!(warnings(
        "@use \"sass:meta\"; @use \"sass:math\";\na { b: meta.call(meta.get-function(\"percentage\", $module: \"math\"), 1); }\n",
        "in.scss",
    )
    .is_empty());
    // The function's OWN deprecation still fires for a module-derived one.
    let w = warnings(
        "@use \"sass:meta\";\na { b: meta.call(meta.get-function(\"feature-exists\", $module: \"meta\"), \"at-error\"); }\n",
        "in.scss",
    );
    assert_eq!(w.len(), 1, "{w:?}");
    assert!(w[0].contains("[feature-exists]"), "{}", w[0]);
    // Taken globally, it is deprecated for being global.
    let w = warnings(
        "@use \"sass:meta\";\na { b: meta.call(meta.get-function(\"percentage\"), 1); }\n",
        "in.scss",
    );
    assert_eq!(w.len(), 1, "{w:?}");
    assert!(w[0].contains("Use math.percentage instead."), "{}", w[0]);
}

#[test]
fn a_host_function_does_not_exempt_a_global_builtin() {
    // dart's `functions` do not shadow a built-in global at all: the built-in
    // runs and still warns (measured against 1.103.1's JS API with a
    // `type-of($v)` host function, which never runs).
    use std::rc::Rc;
    let cb: sasso::HostFunction = Rc::new(|_args: &[u8]| Ok(Vec::new()));
    let seen: Rc<std::cell::RefCell<Vec<String>>> = Rc::new(std::cell::RefCell::new(Vec::new()));
    let sink = Rc::clone(&seen);
    let opts = Options::default()
        .with_url("in.scss")
        .with_function("type-of($v)", cb)
        .with_warn_handler(Rc::new(move |ev: &sasso::WarnEvent<'_>| {
            sink.borrow_mut().push(ev.formatted.to_string());
        }));
    let _ = compile(".a { b: type-of(1); }\n", &opts);
    let w = seen.borrow().clone();
    assert_eq!(w.len(), 1, "{w:?}");
    assert!(w[0].contains("Use meta.type-of instead."), "{}", w[0]);
}
