//! Verification harness for Source Map v3 output — built VERIFICATION-FIRST,
//! before the emit/API phases produce maps, and committed so it runs in CI.
//!
//! Three layers (see the module fns):
//!   1. `decode_mappings` — an independent Base64-VLQ + `mappings` decoder (the
//!      inverse of `src/sourcemap.rs`), used as the oracle machinery.
//!   2. `check_declaration_invariants` — asserts each declaration mapping points
//!      at the SAME leading CSS identifier in the source as appears at the
//!      generated position (a self-checking semantic invariant, no dart needed).
//!   3. The dart-sass differential lives in `tests/parity.rs`-style gated tests
//!      added as the feature lands; here we validate the MACHINERY itself against
//!      a known-good dart-sass map fixture, so later phases can trust it.
//!
//! As Phase B/C/E land, the sasso-output tests call `check_declaration_invariants`
//! on sasso's own (css, map) and decode-diff against dart.

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn b64_val(c: u8) -> i64 {
    B64.iter().position(|&b| b == c).expect("valid base64 digit") as i64
}

/// Decode every Base64-VLQ value packed in one segment string.
fn vlq_decode_all(seg: &str) -> Vec<i64> {
    let bytes = seg.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let mut result: i64 = 0;
        let mut shift = 0;
        loop {
            let d = b64_val(bytes[i]);
            i += 1;
            result |= (d & 0x1f) << shift;
            shift += 5;
            if d & 0x20 == 0 {
                break;
            }
        }
        let mag = result >> 1;
        out.push(if result & 1 == 1 { -mag } else { mag });
    }
    out
}

/// One absolute mapping (all 0-based): generated (line,col) -> source (id,line,col).
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
struct Mapping {
    gen_line: u32,
    gen_col: u32,
    src_id: u32,
    src_line: u32,
    src_col: u32,
}

/// Decode a v3 `mappings` string into absolute mappings (the inverse of
/// `sourcemap::Mappings::encode`). Segments with only a generated column (no
/// source) are skipped — they carry no source position to verify.
fn decode_mappings(mappings: &str) -> Vec<Mapping> {
    let mut out = Vec::new();
    let (mut s_id, mut s_line, mut s_col) = (0i64, 0i64, 0i64);
    for (gen_line, line) in mappings.split(';').enumerate() {
        let mut gen_col = 0i64;
        for seg in line.split(',') {
            if seg.is_empty() {
                continue;
            }
            let f = vlq_decode_all(seg);
            gen_col += f[0];
            if f.len() >= 4 {
                s_id += f[1];
                s_line += f[2];
                s_col += f[3];
                out.push(Mapping {
                    gen_line: gen_line as u32,
                    gen_col: gen_col as u32,
                    src_id: s_id as u32,
                    src_line: s_line as u32,
                    src_col: s_col as u32,
                });
            }
        }
    }
    out
}

/// The text on `line` starting at column `col`. NOTE: for ASCII fixtures the
/// UTF-16 source-map column equals the byte column; non-ASCII verification needs
/// a UTF-16->byte conversion (added when those fixtures appear).
fn at(text: &str, line: u32, col: u32) -> Option<&str> {
    text.lines()
        .nth(line as usize)
        .and_then(|l| l.get(col as usize..))
}

/// The leading CSS identifier (property name / keyword) at the start of `s`.
fn leading_ident(s: &str) -> String {
    s.chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect()
}

/// Self-checking invariant: for every mapping whose GENERATED position begins
/// with an identifier (a declaration name or at-rule keyword — not a selector,
/// which starts with `.`/`#`/`&`/etc.), the SOURCE position it points to must
/// begin with the same identifier. Selector mappings (non-ident start) are only
/// bounds-checked, since source flattening (`.a .b` <- `.b`) means the generated
/// token is not a literal prefix of the source. Returns the count of strictly
/// verified (identifier-matched) mappings.
fn check_declaration_invariants(sources: &[(&str, &str)], output_css: &str, mappings: &str) -> usize {
    let maps = decode_mappings(mappings);
    let mut strict = 0;
    for m in &maps {
        let src_text = sources
            .get(m.src_id as usize)
            .unwrap_or_else(|| panic!("mapping references missing source id {}", m.src_id))
            .1;
        let gen = at(output_css, m.gen_line, m.gen_col)
            .unwrap_or_else(|| panic!("generated pos {:?} out of bounds", m));
        let src =
            at(src_text, m.src_line, m.src_col).unwrap_or_else(|| panic!("source pos {:?} out of bounds", m));
        let gen_id = leading_ident(gen);
        if gen_id.is_empty() {
            continue; // selector / symbol-leading token: bounds-checked only
        }
        assert_eq!(
            gen_id,
            leading_ident(src),
            "mapping {m:?}: generated {gen_id:?} != source {:?}",
            leading_ident(src)
        );
        strict += 1;
    }
    strict
}

// --- The dart-sass fixture: ground truth that validates the machinery above. ---
// Produced by: dart-sass 1.101.0 `sass --source-map sm_in.scss sm_out.css`.
const DART_INPUT: &str = ".a {\n  color: red;\n  .b { width: 10px; }\n}\n";
const DART_OUTPUT_CSS: &str = ".a {\n  color: red;\n}\n.a .b {\n  width: 10px;\n}\n";
const DART_MAPPINGS: &str = "AAAA;EACE;;AACA;EAAK";

#[test]
fn decoder_matches_known_dart_map() {
    // Hand-verified decode of dart's mappings for DART_INPUT.
    let got = decode_mappings(DART_MAPPINGS);
    let expect = vec![
        Mapping {
            gen_line: 0,
            gen_col: 0,
            src_id: 0,
            src_line: 0,
            src_col: 0,
        }, // `.a` selector
        Mapping {
            gen_line: 1,
            gen_col: 2,
            src_id: 0,
            src_line: 1,
            src_col: 2,
        }, // `color` decl name
        Mapping {
            gen_line: 3,
            gen_col: 0,
            src_id: 0,
            src_line: 2,
            src_col: 2,
        }, // flattened `.a .b` <- `.b`
        Mapping {
            gen_line: 4,
            gen_col: 2,
            src_id: 0,
            src_line: 2,
            src_col: 7,
        }, // `width` decl name
    ];
    assert_eq!(got, expect);
}

#[test]
fn invariant_checker_accepts_known_good_dart_map() {
    // The machinery must ACCEPT a known-correct map (and strictly verify the two
    // declaration names `color` and `width`).
    let strict = check_declaration_invariants(&[("sm_in.scss", DART_INPUT)], DART_OUTPUT_CSS, DART_MAPPINGS);
    assert_eq!(
        strict, 2,
        "expected to strictly verify the 2 declaration-name mappings"
    );
}

#[test]
fn invariant_checker_rejects_a_corrupted_map() {
    // Sanity: corrupting the source column of the `color` mapping (point it at
    // the wrong place) must make the checker FAIL — proving it actually checks.
    // "EACE" (gen+2,src0,line+1,col+2) -> change col delta so `color` maps to col 0.
    let bad = "AAAA;EACA;;AACA;EAAK"; // second segment col delta 2 -> 0
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {})); // silence the expected-panic noise
    let result = std::panic::catch_unwind(|| {
        check_declaration_invariants(&[("sm_in.scss", DART_INPUT)], DART_OUTPUT_CSS, bad)
    });
    std::panic::set_hook(prev);
    assert!(
        result.is_err(),
        "checker must reject a map whose source column is wrong"
    );
}
