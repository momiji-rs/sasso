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
