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
    assert!(block.contains("4 \u{2502} \u{2502}   }\n"), "{block}");
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
