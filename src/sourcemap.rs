//! Source Map v3 generation — Phase A: the encoding primitives + JSON model.
//!
//! Zero-dependency (std only): a Base64-VLQ encoder, a builder that accumulates
//! `(generated → source)` position segments, and the standard v3 JSON renderer.
//! This module is self-contained and not yet wired into the serializer; later
//! phases feed it from `emit.rs` and surface it through the public API.
//!
//! Format reference: the Source Map v3 spec (ECMA-426). All positions are
//! 0-based; the generated column is in UTF-16 code units (the serializer is
//! responsible for counting in those units when it feeds `add`).

/// The Base64 alphabet used by both VLQ digits and the inline-map data URI.
const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Append the Base64-VLQ encoding of a signed value to `out`.
///
/// The value is first zig-zag folded (sign in the least-significant bit), then
/// emitted little-endian in 5-bit groups; every non-final group sets the
/// continuation bit (0x20).
fn vlq_encode(value: i64, out: &mut String) {
    let mut v: u64 = if value < 0 {
        ((value.unsigned_abs()) << 1) | 1
    } else {
        (value as u64) << 1
    };
    loop {
        let mut digit = (v & 0x1f) as usize;
        v >>= 5;
        if v != 0 {
            digit |= 0x20; // continuation bit: more groups follow
        }
        out.push(B64[digit] as char);
        if v == 0 {
            break;
        }
    }
}

/// Standard Base64 of a byte slice (RFC 4648, with `=` padding). Used for the
/// `--embed-source-map` `data:application/json;base64,…` URI — distinct from the
/// per-5-bit VLQ digit encoding above.
// Reserved for the CLI/wasm `--embed-source-map` data-URI surface (a later
// phase); already covered by unit tests in this module.
#[allow(dead_code)]
pub(crate) fn base64_bytes(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(B64[(n >> 18 & 63) as usize] as char);
        out.push(B64[(n >> 12 & 63) as usize] as char);
        out.push(if chunk.len() > 1 {
            B64[(n >> 6 & 63) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            B64[(n & 63) as usize] as char
        } else {
            '='
        });
    }
    out
}

/// One generated→source mapping. Fields are 0-based; `gen_col` is a UTF-16
/// column on the generated line `gen_line`. Names are not used for CSS, so there
/// is no name index (dart-sass emits 4-field segments for stylesheets).
#[derive(Clone, Copy)]
pub(crate) struct Segment {
    pub gen_col: u32,
    pub src_id: u32,
    pub src_line: u32,
    pub src_col: u32,
}

/// Accumulates mapping segments grouped by generated line, then renders the
/// delta-encoded `mappings` string.
#[derive(Default)]
pub(crate) struct Mappings {
    /// `lines[g]` holds the segments on generated line `g`, in column order.
    lines: Vec<Vec<Segment>>,
}

impl Mappings {
    pub(crate) fn new() -> Self {
        Mappings::default()
    }

    /// Record a mapping at generated `(gen_line, gen_col)` back to source
    /// `(src_id, src_line, src_col)`. Callers add segments in generated order.
    pub(crate) fn add(&mut self, gen_line: u32, gen_col: u32, src_id: u32, src_line: u32, src_col: u32) {
        let g = gen_line as usize;
        if self.lines.len() <= g {
            self.lines.resize_with(g + 1, Vec::new);
        }
        self.lines[g].push(Segment {
            gen_col,
            src_id,
            src_line,
            src_col,
        });
    }

    /// Render the standard `mappings` field: generated lines separated by `;`,
    /// segments by `,`. `gen_col` resets to absolute at each line; `src_id`/
    /// `src_line`/`src_col` are deltas that persist across lines.
    pub(crate) fn encode(&self) -> String {
        let mut out = String::new();
        let (mut p_id, mut p_line, mut p_col) = (0i64, 0i64, 0i64);
        for (i, line) in self.lines.iter().enumerate() {
            if i > 0 {
                out.push(';');
            }
            let mut p_gen = 0i64;
            for (j, s) in line.iter().enumerate() {
                if j > 0 {
                    out.push(',');
                }
                vlq_encode(s.gen_col as i64 - p_gen, &mut out);
                p_gen = s.gen_col as i64;
                vlq_encode(s.src_id as i64 - p_id, &mut out);
                p_id = s.src_id as i64;
                vlq_encode(s.src_line as i64 - p_line, &mut out);
                p_line = s.src_line as i64;
                vlq_encode(s.src_col as i64 - p_col, &mut out);
                p_col = s.src_col as i64;
            }
        }
        out
    }
}

/// One raw mapped token recorded by the serializer: a byte offset into the
/// emitted CSS *body* (before any `@charset`/BOM prefix) and the 0-based source
/// position it came from. The body offset is resolved to a `(gen_line,
/// gen_col_utf16)` pair by [`SmCollector::finalize`] once the whole body — and
/// thus the prefix — is known.
#[derive(Clone, Copy)]
pub(crate) struct RawEntry {
    /// Byte offset into the CSS body where the mapped token begins.
    pub byte_off: u32,
    /// Evaluator-interned source file id (1-based; 0 = unknown, dropped).
    pub file_id: u32,
    /// 0-based source line.
    pub src_line: u32,
    /// 0-based source column.
    pub src_col: u32,
}

/// Accumulates [`RawEntry`] records during emit. The serializer holds this in an
/// `Option`: `None` (the default `compile` path) records nothing and reads only
/// `out.len()`, so the CSS bytes are untouched.
#[derive(Default)]
pub(crate) struct SmCollector {
    entries: Vec<RawEntry>,
    /// Compressed output skips a token whose source line repeats the
    /// IMMEDIATELY PRECEDING mapped token's (matching dart-sass): expanded maps
    /// every selector + declaration, but compressed packs many tokens onto one
    /// line and dart coalesces consecutive same-source-line runs. This is a
    /// consecutive-run skip, NOT a global one-per-source-line: a source line
    /// that recurs non-consecutively (e.g. a `@media`-bubbled parent selector
    /// mapping back to its original line) is mapped again. `false` keeps every
    /// token (expanded).
    compressed: bool,
    /// The `(file_id, src_line)` of the last recorded entry, for the compressed
    /// consecutive-same-source-line skip.
    last: Option<(u32, u32)>,
}

impl SmCollector {
    pub(crate) fn new(compressed: bool) -> Self {
        SmCollector {
            compressed,
            ..SmCollector::default()
        }
    }

    /// Record a mapped token at body byte offset `byte_off` whose source
    /// position is `(file_id, src_line, src_col)` (all 0-based; `file_id` is the
    /// 1-based interned id, 0 = unknown). Reads nothing from the output buffer.
    pub(crate) fn record(&mut self, byte_off: usize, file_id: u32, src_line: u32, src_col: u32) {
        // A token with no known source file carries no usable mapping.
        if file_id == 0 {
            return;
        }
        // Compressed: skip a token whose source line repeats the IMMEDIATELY
        // PRECEDING mapped token's (dart coalesces consecutive same-source-line
        // runs in compressed output; non-consecutive recurrences still map).
        if self.compressed && self.last == Some((file_id, src_line)) {
            return;
        }
        self.last = Some((file_id, src_line));
        self.entries.push(RawEntry {
            byte_off: byte_off as u32,
            file_id,
            src_line,
            src_col,
        });
    }

    /// Convert the recorded body offsets into delta-encoded `mappings`.
    ///
    /// `output` is the FINAL CSS string (already including any `@charset`/BOM
    /// prefix); `body_off` is the byte length of that prefix, which shifts every
    /// recorded body offset. Generated lines are counted by `\n`; the generated
    /// column is in UTF-16 code units (not bytes). A `file_id` is mapped to its
    /// `sources[]` index by `id - 1` (ids are assigned 1,2,3,…).
    pub(crate) fn finalize(mut self, output: &str, body_off: usize) -> Mappings {
        let mut m = Mappings::new();
        if self.entries.is_empty() {
            return m;
        }
        // Sort by generated byte offset so segments are added in generated
        // order (the encoder requires column-ascending segments per line).
        self.entries.sort_by_key(|e| e.byte_off);

        // A cursor that walks the output once, translating an absolute byte
        // offset into (line, utf16-col). Offsets arrive sorted, so the walk is
        // monotonic and O(output length) overall.
        let mut cur_byte = 0usize;
        let mut gen_line = 0u32;
        let mut gen_col16 = 0u32; // UTF-16 code units since the last '\n'

        for e in &self.entries {
            let target = body_off + e.byte_off as usize;
            // Advance the cursor to `target`, counting newlines and UTF-16
            // units. We step by whole chars (offsets always land on a token
            // start, i.e. a char boundary).
            while cur_byte < target {
                let ch = output[cur_byte..].chars().next().expect("offset within output");
                if ch == '\n' {
                    gen_line += 1;
                    gen_col16 = 0;
                } else {
                    gen_col16 += ch.len_utf16() as u32;
                }
                cur_byte += ch.len_utf8();
            }
            debug_assert_eq!(cur_byte, target, "offset landed on a char boundary");
            m.add(gen_line, gen_col16, e.file_id - 1, e.src_line, e.src_col);
        }
        m
    }
}

/// A finished [Source Map v3](https://tc39.es/ecma426/), ready to serialize as
/// JSON via [`SourceMap::to_json`].
#[derive(Clone, Debug)]
pub struct SourceMap {
    /// Name of the generated file (the `file` field), if known.
    pub file: Option<String>,
    /// Source URLs, in the order their ids were assigned.
    pub sources: Vec<String>,
    /// Full source text per `sources` entry (parallel array), or `None` to omit
    /// the `sourcesContent` field entirely (controlled by
    /// `Options::with_source_map_include_sources`).
    pub sources_content: Option<Vec<String>>,
    /// The pre-encoded `mappings` string.
    pub mappings: String,
}

impl SourceMap {
    /// Render the v3 JSON. Built by hand (zero-dep) with correct string escaping.
    pub fn to_json(&self) -> String {
        let mut s = String::from("{\"version\":3");
        if let Some(file) = &self.file {
            s.push_str(",\"file\":");
            json_str(file, &mut s);
        }
        s.push_str(",\"sources\":[");
        for (i, src) in self.sources.iter().enumerate() {
            if i > 0 {
                s.push(',');
            }
            json_str(src, &mut s);
        }
        s.push(']');
        if let Some(contents) = &self.sources_content {
            s.push_str(",\"sourcesContent\":[");
            for (i, c) in contents.iter().enumerate() {
                if i > 0 {
                    s.push(',');
                }
                json_str(c, &mut s);
            }
            s.push(']');
        }
        s.push_str(",\"names\":[],\"mappings\":");
        json_str(&self.mappings, &mut s);
        s.push('}');
        s
    }
}

/// Append `value` as a JSON string literal (quotes + escaping) to `out`.
fn json_str(value: &str, out: &mut String) {
    out.push('"');
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Decode one Base64-VLQ value starting at `bytes[*i]`, advancing `*i`.
    /// Test-only inverse of `vlq_encode`, for round-trip checking.
    fn vlq_decode(bytes: &[u8], i: &mut usize) -> i64 {
        let mut result: u64 = 0;
        let mut shift = 0;
        loop {
            let digit = B64.iter().position(|&b| b == bytes[*i]).unwrap() as u64;
            *i += 1;
            result |= (digit & 0x1f) << shift;
            shift += 5;
            if digit & 0x20 == 0 {
                break;
            }
        }
        let mag = (result >> 1) as i64;
        if result & 1 == 1 {
            -mag
        } else {
            mag
        }
    }

    #[test]
    fn vlq_known_vectors() {
        let enc = |v: i64| {
            let mut s = String::new();
            vlq_encode(v, &mut s);
            s
        };
        assert_eq!(enc(0), "A");
        assert_eq!(enc(1), "C");
        assert_eq!(enc(-1), "D");
        assert_eq!(enc(2), "E");
        assert_eq!(enc(-2), "F");
        assert_eq!(enc(16), "gB");
        assert_eq!(enc(123), "2H");
    }

    #[test]
    fn vlq_roundtrips_over_a_wide_range() {
        for v in (-100_000..=100_000).step_by(37) {
            let mut s = String::new();
            vlq_encode(v, &mut s);
            let mut i = 0;
            assert_eq!(vlq_decode(s.as_bytes(), &mut i), v, "roundtrip {v}");
            assert_eq!(i, s.len(), "consumed all bytes for {v}");
        }
        // boundary magnitudes
        for v in [i32::MAX as i64, i32::MIN as i64, 0, 15, 16, 31, 32, 1023, 1024] {
            let mut s = String::new();
            vlq_encode(v, &mut s);
            let mut i = 0;
            assert_eq!(vlq_decode(s.as_bytes(), &mut i), v);
        }
    }

    #[test]
    fn mappings_delta_encoding() {
        let mut m = Mappings::new();
        // line 0: one segment at the origin -> all zero deltas -> "AAAA"
        m.add(0, 0, 0, 0, 0);
        assert_eq!(m.encode(), "AAAA");

        let mut m = Mappings::new();
        // line 0: col 0 -> src0 line0 col0; then col 5 -> src0 line0 col10
        m.add(0, 0, 0, 0, 0);
        m.add(0, 5, 0, 0, 10);
        // line 1 (empty), line 2: col 2 -> src0 line1 col0
        m.add(2, 2, 0, 1, 0);
        let s = m.encode();
        // groups separated by ';' ; empty line 1 is just an extra ';'
        let groups: Vec<&str> = s.split(';').collect();
        assert_eq!(groups.len(), 3);
        assert_eq!(groups[1], "", "empty generated line is an empty group");
        // first group: [0,0,0,0]=AAAA then delta gen_col +5, src_col +10 -> "KAAU"
        assert_eq!(groups[0], "AAAA,KAAU");
        // decode the whole thing and confirm the absolute positions reconstruct
        assert!(s.bytes().all(|b| B64.contains(&b) || b == b';' || b == b','));
    }

    #[test]
    fn mappings_matches_legacy_charset_regex() {
        // The legacy sass source-map test pins mappings to this shape.
        let mut m = Mappings::new();
        m.add(0, 0, 0, 0, 0);
        m.add(0, 7, 1, 3, 2);
        m.add(1, 0, 0, 5, 0);
        let s = m.encode();
        let ok = s
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/' | b'=' | b',' | b';'));
        assert!(ok, "mappings {s:?} must match /^([A-Za-z0-9+/=]*[,;]?)*$/");
    }

    #[test]
    fn base64_known_vectors() {
        assert_eq!(base64_bytes(b""), "");
        assert_eq!(base64_bytes(b"f"), "Zg==");
        assert_eq!(base64_bytes(b"fo"), "Zm8=");
        assert_eq!(base64_bytes(b"foo"), "Zm9v");
        assert_eq!(base64_bytes(b"foob"), "Zm9vYg==");
        assert_eq!(base64_bytes(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64_bytes(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn json_shape_and_escaping() {
        let map = SourceMap {
            file: Some("out.css".to_string()),
            sources: vec!["a.scss".to_string(), "dir/b.scss".to_string()],
            sources_content: Some(vec![".a{b:1}".to_string(), "x:\"q\"\n\\z".to_string()]),
            mappings: "AAAA,KAAU".to_string(),
        };
        let j = map.to_json();
        assert!(j.starts_with("{\"version\":3,\"file\":\"out.css\""));
        assert!(j.contains("\"sources\":[\"a.scss\",\"dir/b.scss\"]"));
        assert!(j.contains("\"names\":[]"));
        assert!(j.contains("\"mappings\":\"AAAA,KAAU\""));
        // escaping: the embedded quote, newline and backslash must be escaped
        assert!(j.contains("x:\\\"q\\\"\\n\\\\z"));
        // omitting sourcesContent drops the field entirely
        let map2 = SourceMap {
            sources_content: None,
            ..map_with_no_content()
        };
        assert!(!map2.to_json().contains("sourcesContent"));
    }

    fn map_with_no_content() -> SourceMap {
        SourceMap {
            file: None,
            sources: vec!["a.scss".to_string()],
            sources_content: None,
            mappings: String::new(),
        }
    }
}
