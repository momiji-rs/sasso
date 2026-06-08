//! Source-span diagnostic rendering — a hand-rolled, dependency-free
//! re-implementation of dart-sass's `SourceSpanHighlighter` snippet block.
//!
//! This module is **pure**: every public entry point is a function over an
//! explicit `(source, line, col, length, …)` description and returns a
//! `String`. Nothing here touches the evaluator, the parser, or any global
//! state, and there is no I/O — the integration step (which wires real
//! [`crate::Error`] spans through here) lives elsewhere. Keeping it isolated
//! makes it trivial to test against the real `dart-sass` binary byte-for-byte.
//!
//! # What dart-sass renders
//!
//! For a single-line span (`a {\n  b: $undefined;\n}` → the `$undefined`
//! token), dart-sass emits this exact block to stderr (Unicode glyph set):
//!
//! ```text
//!   ╷
//! 2 │   b: $undefined;
//!   │      ^^^^^^^^^^
//!   ╵
//!   path/to/input.scss 2:6  root stylesheet
//! ```
//!
//! The structure is:
//!
//! * a *top* gutter line: right-aligned blank line-number column, a space, the
//!   top glyph `╷` (`U+2577`);
//! * one *source* line per spanned source line: the right-aligned line number,
//!   a space, the mid glyph `│` (`U+2502`), a space, then the source text with
//!   **every TAB expanded to exactly four spaces** (this is byte-load-bearing;
//!   dart-sass does *not* use tab stops, each `\t` becomes `"    "`);
//! * for a single-line span, a *caret* line: blank gutter, the mid glyph, a
//!   space, padding equal to the display-column offset of the span start, then
//!   `^` (`U+005E`) repeated for the display width of the spanned text (at
//!   least one caret, even for a zero-length span);
//! * a *bottom* gutter line: blank gutter, a space, the bottom glyph `╵`
//!   (`U+2575`);
//! * a *location* line: two spaces, the file URL, a space, `line:col`, two
//!   spaces, then the frame name (the outermost frame is literally
//!   `root stylesheet`).
//!
//! The line/column numbers in the location line are **1-based**, matching
//! dart-sass and [`crate::Error`].
//!
//! With `--no-unicode`, dart-sass swaps the glyph set:
//! `╷│╵` → `,|'` and (for multi-line spans) `┌│└─` → `,|'-`.

// This is a self-contained, not-yet-wired deliverable: the renderer's public
// API is exercised by the unit tests below and is consumed by the later
// integration step that attaches it to `crate::Error`. Until then the non-test
// `cargo build` sees the items as unused, so we silence `dead_code` here; the
// integration step that calls these functions removes this allow.
#![allow(dead_code)]

/// The glyph set used to draw the gutter and span decorations.
///
/// dart-sass picks [`GlyphSet::Unicode`] by default and [`GlyphSet::Ascii`]
/// under `--no-unicode` (or a non-Unicode terminal). The two sets are
/// byte-for-byte what dart-sass writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GlyphSet {
    /// Box-drawing glyphs: `╷ │ ╵ ┌ └ ─` and the ASCII caret `^`.
    Unicode,
    /// Pure-ASCII fallback: `, | ' , ' -` and the caret `^`.
    Ascii,
}

impl GlyphSet {
    /// Top of a single-column gutter (`╷` / `,`).
    const fn top(self) -> &'static str {
        match self {
            GlyphSet::Unicode => "\u{2577}",
            GlyphSet::Ascii => ",",
        }
    }

    /// Vertical bar of a gutter (`│` / `|`).
    const fn vertical(self) -> &'static str {
        match self {
            GlyphSet::Unicode => "\u{2502}",
            GlyphSet::Ascii => "|",
        }
    }

    /// Bottom of a single-column gutter (`╵` / `'`).
    const fn bottom(self) -> &'static str {
        match self {
            GlyphSet::Unicode => "\u{2575}",
            GlyphSet::Ascii => "'",
        }
    }

    /// Top-left corner that opens a multi-line span (`┌` / `,`).
    const fn top_left(self) -> &'static str {
        match self {
            GlyphSet::Unicode => "\u{250c}",
            GlyphSet::Ascii => ",",
        }
    }

    /// Bottom-left corner that closes a multi-line span (`└` / `'`).
    const fn bottom_left(self) -> &'static str {
        match self {
            GlyphSet::Unicode => "\u{2514}",
            GlyphSet::Ascii => "'",
        }
    }

    /// Horizontal rule used by the multi-line span arms (`─` / `-`).
    const fn horizontal(self) -> &'static str {
        match self {
            GlyphSet::Unicode => "\u{2500}",
            GlyphSet::Ascii => "-",
        }
    }
}

/// dart-sass expands a literal TAB to exactly this many spaces when rendering a
/// source line (it is **not** tab-stop alignment — every `\t` is four spaces,
/// wherever it sits on the line).
const TAB_WIDTH: usize = 4;

/// The caret glyph that underlines a span (`^`, `U+005E`) — identical in both
/// glyph sets.
const CARET: char = '^';

/// A located span to highlight, described in dart-sass / [`crate::Error`]
/// terms: 1-based `line`/`col` of the span start and a byte `length`.
///
/// `length` is measured in **bytes of the original source** (UTF-8); the
/// renderer slices the affected source text out and measures its *display*
/// width (with tabs expanded) to size the caret underline. A `length` of `0`
/// describes a point span and still draws a single caret, exactly as
/// dart-sass does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    /// 1-based line of the span start.
    pub line: usize,
    /// 1-based column of the span start, counted in source characters (a TAB
    /// counts as one column, matching dart-sass's reported position).
    pub col: usize,
    /// Length of the span in **bytes** of the original source text.
    pub length: usize,
}

/// One frame of the rendered stack trace.
///
/// The location line under the snippet is a single [`Frame`]; deeper traces
/// (e.g. a function invocation) stack several, outermost last and literally
/// named `root stylesheet`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame<'a> {
    /// The file URL/path as dart-sass prints it (e.g. the absolute path, or
    /// `-` for stdin).
    pub url: &'a str,
    /// 1-based line of this frame's span.
    pub line: usize,
    /// 1-based column of this frame's span.
    pub col: usize,
    /// The member name for this frame, or `root stylesheet` for the outermost.
    pub name: &'a str,
}

impl Frame<'_> {
    /// Render a single frame line: `<url> <line>:<col>  <name>` (two spaces
    /// before the name), with no leading indentation. Callers that emit a full
    /// trace prepend two spaces per line; see [`render_frames`].
    fn render_inner(&self) -> String {
        format!("{} {}:{}  {}", self.url, self.line, self.col, self.name)
    }
}

/// Render a stack trace exactly as dart-sass appends it under the snippet:
/// each frame on its own line, prefixed with two spaces.
///
/// ```text
///   path 2:6  some-mixin
///   path 9:3  root stylesheet
/// ```
#[must_use]
pub fn render_frames(frames: &[Frame<'_>]) -> String {
    let mut out = String::new();
    for (i, f) in frames.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        out.push_str("  ");
        out.push_str(&f.render_inner());
    }
    out
}

/// Split `source` into lines the way dart-sass's `SourceFile` does: on `\n`,
/// `\r\n`, and bare `\r`, *dropping* the terminator. A trailing newline yields
/// a final empty line index that is simply never addressed by a 1-based line
/// number, so we do not special-case it.
fn split_lines(source: &str) -> Vec<&str> {
    let mut lines = Vec::new();
    let bytes = source.as_bytes();
    let mut start = 0usize;
    let mut i = 0usize;
    while i < bytes.len() {
        match bytes[i] {
            b'\n' => {
                lines.push(&source[start..i]);
                i += 1;
                start = i;
            }
            b'\r' => {
                lines.push(&source[start..i]);
                i += 1;
                if i < bytes.len() && bytes[i] == b'\n' {
                    i += 1;
                }
                start = i;
            }
            _ => i += 1,
        }
    }
    lines.push(&source[start..]);
    lines
}

/// Expand every TAB in `text` to [`TAB_WIDTH`] spaces. Returns the displayable
/// string. This is the exact transform dart-sass applies before measuring
/// column widths, so it is shared by both the source line and the caret math.
fn expand_tabs(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        if ch == '\t' {
            for _ in 0..TAB_WIDTH {
                out.push(' ');
            }
        } else {
            out.push(ch);
        }
    }
    out
}

/// Display width of the first `cols` *source columns* of `line`, where each TAB
/// counts as [`TAB_WIDTH`] and every other character counts as one. `cols` is a
/// 0-based character count from the start of the line.
fn display_width_of_prefix(line: &str, cols: usize) -> usize {
    let mut width = 0usize;
    for ch in line.chars().take(cols) {
        width += if ch == '\t' { TAB_WIDTH } else { 1 };
    }
    width
}

/// Number of decimal digits in `n` (at least 1, so `0` → 1).
fn digit_count(n: usize) -> usize {
    let mut n = n;
    let mut digits = 1;
    while n >= 10 {
        n /= 10;
        digits += 1;
    }
    digits
}

/// Build the blank gutter prefix used by decoration lines: `width` spaces (for
/// the line-number column) plus one trailing space, e.g. `"  "` for a
/// single-digit file or `"   "` once line numbers reach 10.
fn blank_gutter(width: usize) -> String {
    let mut s = String::with_capacity(width + 1);
    for _ in 0..width + 1 {
        s.push(' ');
    }
    s
}

/// Build the numbered gutter prefix for a source line: the right-aligned line
/// number padded to `width`, then a space, e.g. `"2 "` or `" 2 "`.
fn numbered_gutter(line_no: usize, width: usize) -> String {
    let digits = digit_count(line_no);
    let mut s = String::with_capacity(width + 1);
    for _ in 0..width.saturating_sub(digits) {
        s.push(' ');
    }
    push_usize(&mut s, line_no);
    s.push(' ');
    s
}

/// Append the decimal rendering of `n` to `out` without allocating.
fn push_usize(out: &mut String, n: usize) {
    if n >= 10 {
        push_usize(out, n / 10);
    }
    // 0..=9 always maps to a valid ASCII digit.
    let digit = (n % 10) as u8 + b'0';
    out.push(digit as char);
}

/// Render the snippet block (gutter + source + caret/arms + location frames)
/// for a span, byte-for-byte like dart-sass.
///
/// This does **not** emit the `Error: <message>` header — the caller owns the
/// message line — but it does emit everything from the top gutter glyph down to
/// (and including) the trailing frame lines, with no trailing newline.
///
/// `source` is the full text of the file the span points into; `span` is the
/// 1-based start position and byte length; `frames` is the stack trace to print
/// under the snippet (use a single [`Frame`] named `root stylesheet` for the
/// common case). `glyphs` selects the Unicode or ASCII decoration set.
///
/// The function is total: out-of-range line numbers, a `length` that runs past
/// the file, and empty sources all degrade gracefully (clamping rather than
/// panicking), so it satisfies the crate's panic-free discipline.
#[must_use]
pub fn render_snippet(source: &str, span: Span, frames: &[Frame<'_>], glyphs: GlyphSet) -> String {
    let lines = split_lines(source);

    // Resolve the 0-based start line, clamped into range.
    let start_idx = span.line.saturating_sub(1).min(lines.len().saturating_sub(1));
    let start_col0 = span.col.saturating_sub(1);

    // Walk the byte length across lines to find the end line/col. dart-sass
    // counts the terminator between lines as one byte; we mirror that so a
    // span that crosses a newline lands on the right line.
    let (end_idx, end_col0) = resolve_end(&lines, start_idx, start_col0, span.length);

    // Gutter width is sized to the widest line number we will print.
    let max_line_no = end_idx + 1;
    let width = digit_count(max_line_no);

    let mut out = String::new();

    if start_idx == end_idx {
        render_single_line(&mut out, &lines, start_idx, start_col0, end_col0, width, glyphs);
    } else {
        render_multi_line(
            &mut out, &lines, start_idx, start_col0, end_idx, end_col0, width, glyphs,
        );
    }

    // Bottom gutter glyph.
    out.push('\n');
    out.push_str(&blank_gutter(width));
    out.push_str(glyphs.bottom());

    // Location / stack-trace lines.
    if !frames.is_empty() {
        out.push('\n');
        out.push_str(&render_frames(frames));
    }

    out
}

/// Resolve the (0-based line, 0-based col) just past the end of a byte span,
/// starting from `(start_idx, start_col0)`. Tabs and multibyte characters are
/// handled by walking characters and decrementing the remaining byte budget by
/// each character's UTF-8 length; the inter-line terminator costs one byte.
fn resolve_end(lines: &[&str], start_idx: usize, start_col0: usize, length: usize) -> (usize, usize) {
    let mut idx = start_idx;
    let mut col = start_col0;
    let mut remaining = length;

    loop {
        let line = lines.get(idx).copied().unwrap_or("");
        // Characters available from `col` to end of this line.
        let mut consumed_cols = 0usize;
        for ch in line.chars().skip(col) {
            let blen = ch.len_utf8();
            if remaining < blen {
                return (idx, col + consumed_cols);
            }
            remaining -= blen;
            consumed_cols += 1;
        }
        // Reached end of this line. The terminator costs one byte if there is
        // a following line.
        if remaining == 0 || idx + 1 >= lines.len() {
            return (idx, col + consumed_cols);
        }
        // Spend the newline byte and move on.
        remaining = remaining.saturating_sub(1);
        idx += 1;
        col = 0;
        if remaining == 0 {
            return (idx, 0);
        }
    }
}

/// Render the top gutter line and the (single) source + caret lines.
fn render_single_line(
    out: &mut String,
    lines: &[&str],
    idx: usize,
    start_col0: usize,
    end_col0: usize,
    width: usize,
    glyphs: GlyphSet,
) {
    let line = lines.get(idx).copied().unwrap_or("");
    let v = glyphs.vertical();

    // Top gutter glyph.
    out.push_str(&blank_gutter(width));
    out.push_str(glyphs.top());

    // Source line.
    out.push('\n');
    out.push_str(&numbered_gutter(idx + 1, width));
    out.push_str(v);
    out.push(' ');
    out.push_str(&expand_tabs(line));

    // Caret line.
    out.push('\n');
    out.push_str(&blank_gutter(width));
    out.push_str(v);
    out.push(' ');
    let pad = display_width_of_prefix(line, start_col0);
    for _ in 0..pad {
        out.push(' ');
    }
    // At least one caret, even for a zero-length (point) span — matching
    // dart-sass; `display_width_of_prefix_range` already clamps to >= 1.
    let caret_w = display_width_of_prefix_range(line, start_col0, end_col0);
    for _ in 0..caret_w {
        out.push(CARET);
    }
}

/// Display width of the characters in `line` from 0-based column `from` up to
/// (not including) 0-based column `to`. Used to size a single-line caret run.
fn display_width_of_prefix_range(line: &str, from: usize, to: usize) -> usize {
    if to <= from {
        return 1;
    }
    let mut width = 0usize;
    for ch in line.chars().skip(from).take(to - from) {
        width += if ch == '\t' { TAB_WIDTH } else { 1 };
    }
    width.max(1)
}

/// Render a multi-line span: the opening arm under the first line, each
/// intermediate source line prefixed with the vertical arm, and the closing arm
/// under the last line. Mirrors dart-sass's `┌─…^` / `│` / `└─^` decorations.
#[allow(clippy::too_many_arguments)]
fn render_multi_line(
    out: &mut String,
    lines: &[&str],
    start_idx: usize,
    start_col0: usize,
    end_idx: usize,
    end_col0: usize,
    width: usize,
    glyphs: GlyphSet,
) {
    let v = glyphs.vertical();
    let h = glyphs.horizontal();

    // Top gutter glyph.
    out.push_str(&blank_gutter(width));
    out.push_str(glyphs.top());

    // First source line. The arm column (where the `┌`/`│`/`└` go on later
    // rows) is a blank slot here, so the layout after the gutter `│` is:
    // `<space><arm-slot><space><content>` → three spaces before the content.
    let first = lines.get(start_idx).copied().unwrap_or("");
    out.push('\n');
    out.push_str(&numbered_gutter(start_idx + 1, width));
    out.push_str(v);
    out.push(' ');
    out.push(' '); // empty arm slot for the opening source row
    out.push(' ');
    out.push_str(&expand_tabs(first));

    // Opening arm: `┌─…─^` whose caret sits under the span start. The `┌`
    // occupies the arm slot; the content baseline is two columns to its right,
    // so the caret offset is the span-start display column + 1.
    out.push('\n');
    out.push_str(&blank_gutter(width));
    out.push_str(v);
    out.push(' ');
    out.push_str(glyphs.top_left());
    let lead = display_width_of_prefix(first, start_col0) + 1;
    for _ in 0..lead {
        out.push_str(h);
    }
    out.push(CARET);

    // Intermediate + final source lines, each carrying a `│` arm:
    // `<space>│<space><content>`.
    for li in (start_idx + 1)..=end_idx {
        let text = lines.get(li).copied().unwrap_or("");
        out.push('\n');
        out.push_str(&numbered_gutter(li + 1, width));
        out.push_str(v);
        out.push(' ');
        out.push_str(v);
        out.push(' ');
        out.push_str(&expand_tabs(text));
    }

    // Closing arm: `└─…─^` whose caret sits under the last spanned character
    // (one column left of the span end), with the same +1 arm offset.
    let last = lines.get(end_idx).copied().unwrap_or("");
    out.push('\n');
    out.push_str(&blank_gutter(width));
    out.push_str(v);
    out.push(' ');
    out.push_str(glyphs.bottom_left());
    let tail = display_width_of_prefix(last, end_col0);
    for _ in 0..tail {
        out.push_str(h);
    }
    out.push(CARET);
}

/// Convenience: render a full diagnostic (`Error:` header + snippet) for the
/// common single-frame `root stylesheet` case. Returns the complete block with
/// no trailing newline, exactly as dart-sass would write it to stderr.
#[must_use]
pub fn render_error(message: &str, source: &str, url: &str, span: Span, glyphs: GlyphSet) -> String {
    let frame = Frame {
        url,
        line: span.line,
        col: span.col,
        name: "root stylesheet",
    };
    let mut out = format!("Error: {message}\n");
    out.push_str(&render_snippet(source, span, &[frame], glyphs));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // ----- Offline, hard-coded expectations (always run) -----

    #[test]
    fn split_lines_handles_all_terminators() {
        assert_eq!(split_lines("a\nb\r\nc\rd"), vec!["a", "b", "c", "d"]);
        assert_eq!(split_lines(""), vec![""]);
        assert_eq!(split_lines("a\n"), vec!["a", ""]);
    }

    #[test]
    fn tabs_expand_to_four_spaces_everywhere() {
        assert_eq!(expand_tabs("\tb"), "    b");
        assert_eq!(expand_tabs("a\tb"), "a    b");
        assert_eq!(expand_tabs("\t\tb"), "        b");
        assert_eq!(expand_tabs(" \tb"), "     b");
    }

    #[test]
    fn digit_count_basic() {
        assert_eq!(digit_count(0), 1);
        assert_eq!(digit_count(9), 1);
        assert_eq!(digit_count(10), 2);
        assert_eq!(digit_count(123), 3);
    }

    /// dart-sass 1.100.0, `a {\n  b: $undefined;\n}\n`, span at 2:6 len 10.
    #[test]
    fn undefined_variable_unicode() {
        let src = "a {\n  b: $undefined;\n}\n";
        let span = Span {
            line: 2,
            col: 6,
            length: "$undefined".len(),
        };
        let got = render_error(
            "Undefined variable.",
            src,
            "/tmp/input.scss",
            span,
            GlyphSet::Unicode,
        );
        let expected = "\
Error: Undefined variable.
  \u{2577}
2 \u{2502}   b: $undefined;
  \u{2502}      ^^^^^^^^^^
  \u{2575}
  /tmp/input.scss 2:6  root stylesheet";
        assert_eq!(got, expected);
    }

    /// Same input, ASCII glyph set (dart-sass `--no-unicode`).
    #[test]
    fn undefined_variable_ascii() {
        let src = "a {\n  b: $undefined;\n}\n";
        let span = Span {
            line: 2,
            col: 6,
            length: "$undefined".len(),
        };
        let got = render_error(
            "Undefined variable.",
            src,
            "/tmp/input.scss",
            span,
            GlyphSet::Ascii,
        );
        let expected = "\
Error: Undefined variable.
  ,
2 |   b: $undefined;
  |      ^^^^^^^^^^
  '
  /tmp/input.scss 2:6  root stylesheet";
        assert_eq!(got, expected);
    }

    /// dart-sass `@error "boom #{1 + 1}";` → message `"boom 2"`, span 1:1 len 22.
    #[test]
    fn at_error_span_at_line_one() {
        let src = "@error \"boom #{1 + 1}\";\n";
        let span = Span {
            line: 1,
            col: 1,
            length: "@error \"boom #{1 + 1}\"".len(),
        };
        let got = render_error("\"boom 2\"", src, "/tmp/input.scss", span, GlyphSet::Unicode);
        let expected = "\
Error: \"boom 2\"
  \u{2577}
1 \u{2502} @error \"boom #{1 + 1}\";
  \u{2502} ^^^^^^^^^^^^^^^^^^^^^^
  \u{2575}
  /tmp/input.scss 1:1  root stylesheet";
        assert_eq!(got, expected);
    }

    /// Gutter widens once the line number reaches double digits (dart-sass:
    /// 11 blank lines then `a { b: $x; }`, span 12:8 len 2).
    #[test]
    fn wide_gutter_double_digit_line() {
        let mut src = String::new();
        for _ in 0..11 {
            src.push('\n');
        }
        src.push_str("a { b: $x; }\n");
        let span = Span {
            line: 12,
            col: 8,
            length: "$x".len(),
        };
        let got = render_error(
            "Undefined variable.",
            &src,
            "/tmp/input.scss",
            span,
            GlyphSet::Unicode,
        );
        let expected = "\
Error: Undefined variable.
   \u{2577}
12 \u{2502} a { b: $x; }
   \u{2502}        ^^
   \u{2575}
  /tmp/input.scss 12:8  root stylesheet";
        assert_eq!(got, expected);
    }

    /// TAB expansion is byte-load-bearing: dart-sass renders `a {\n\tb: $x;\n}`
    /// with the leading tab as four spaces, span at 2:5 len 2.
    #[test]
    fn tab_indent_expands_in_source_and_caret() {
        let src = "a {\n\tb: $x;\n}\n";
        let span = Span {
            line: 2,
            col: 5,
            length: "$x".len(),
        };
        let got = render_error(
            "Undefined variable.",
            src,
            "/tmp/input.scss",
            span,
            GlyphSet::Unicode,
        );
        // Source: `\tb: $x;` → `    b: $x;`. `$` is source col 5 → display col 8
        // (4 for the tab + `b`,`:`,` ` = 3 → 7? dart reports col 5, display pad 7).
        let expected = "\
Error: Undefined variable.
  \u{2577}
2 \u{2502}     b: $x;
  \u{2502}        ^^
  \u{2575}
  /tmp/input.scss 2:5  root stylesheet";
        assert_eq!(got, expected);
    }

    /// Multi-line single span (dart-sass `a{b: (1px +\n2s)}` →
    /// "incompatible units", span 1:7 across two lines). This is a clean
    /// single span (no secondary labels), captured verbatim from dart-sass
    /// 1.100.0:
    /// ```text
    ///   ╷
    /// 1 │   a{b: (1px +
    ///   │ ┌───────^
    /// 2 │ │ 2s)}
    ///   │ └──^
    ///   ╵
    /// ```
    #[test]
    fn multi_line_span_unicode_arms() {
        let src = "a{b: (1px +\n2s)}\n";
        // Span starts at the `1` of `1px` (1:7) and runs through `2s` on line 2.
        let start_byte = byte_index(src, 1, 7);
        let end_byte = byte_index(src, 2, 3); // just past `2s`
        let span = Span {
            line: 1,
            col: 7,
            length: end_byte - start_byte,
        };
        let frame = Frame {
            url: "/tmp/input.scss",
            line: 1,
            col: 7,
            name: "root stylesheet",
        };
        let got = render_snippet(src, span, &[frame], GlyphSet::Unicode);
        let expected = concat!(
            "  \u{2577}\n",
            "1 \u{2502}   a{b: (1px +\n",
            "  \u{2502} \u{250c}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}^\n",
            "2 \u{2502} \u{2502} 2s)}\n",
            "  \u{2502} \u{2514}\u{2500}\u{2500}^\n",
            "  \u{2575}\n",
            "  /tmp/input.scss 1:7  root stylesheet",
        );
        assert_eq!(got, expected);
    }

    /// ASCII multi-line arms (`--no-unicode`), same `1px + 2s` input.
    #[test]
    fn multi_line_span_ascii_arms() {
        let src = "a{b: (1px +\n2s)}\n";
        let start_byte = byte_index(src, 1, 7);
        let end_byte = byte_index(src, 2, 3);
        let span = Span {
            line: 1,
            col: 7,
            length: end_byte - start_byte,
        };
        let frame = Frame {
            url: "/tmp/input.scss",
            line: 1,
            col: 7,
            name: "root stylesheet",
        };
        let got = render_snippet(src, span, &[frame], GlyphSet::Ascii);
        let expected = concat!(
            "  ,\n",
            "1 |   a{b: (1px +\n",
            "  | ,-------^\n",
            "2 | | 2s)}\n",
            "  | '--^\n",
            "  '\n",
            "  /tmp/input.scss 1:7  root stylesheet",
        );
        assert_eq!(got, expected);
    }

    /// A point (zero-length) span still draws exactly one caret.
    #[test]
    fn zero_length_span_one_caret() {
        let src = "a {\n  b: 1 +\n";
        let span = Span {
            line: 2,
            col: 9,
            length: 0,
        };
        let got = render_error(
            "Expected expression.",
            src,
            "/tmp/input.scss",
            span,
            GlyphSet::Unicode,
        );
        let expected = "\
Error: Expected expression.
  \u{2577}
2 \u{2502}   b: 1 +
  \u{2502}         ^
  \u{2575}
  /tmp/input.scss 2:9  root stylesheet";
        assert_eq!(got, expected);
    }

    #[test]
    fn frames_stack_outermost_root() {
        let frames = [
            Frame {
                url: "/tmp/input.scss",
                line: 2,
                col: 7,
                name: "f()",
            },
            Frame {
                url: "/tmp/input.scss",
                line: 2,
                col: 7,
                name: "root stylesheet",
            },
        ];
        let got = render_frames(&frames);
        let expected = "  /tmp/input.scss 2:7  f()\n  /tmp/input.scss 2:7  root stylesheet";
        assert_eq!(got, expected);
    }

    #[test]
    fn out_of_range_does_not_panic() {
        let src = "a {}\n";
        let span = Span {
            line: 99,
            col: 99,
            length: 99,
        };
        // Just assert it produces *something* without panicking.
        let got = render_error("x", src, "-", span, GlyphSet::Unicode);
        assert!(got.starts_with("Error: x"));
    }

    /// Test-only: byte offset of 1-based (line, col) in `src`.
    fn byte_index(src: &str, line: usize, col: usize) -> usize {
        let mut cur_line = 1usize;
        let mut cur_col = 1usize;
        for (i, ch) in src.char_indices() {
            if cur_line == line && cur_col == col {
                return i;
            }
            if ch == '\n' {
                cur_line += 1;
                cur_col = 1;
            } else {
                cur_col += 1;
            }
        }
        src.len()
    }

    // ----- Live dart-sass parity (gated behind SASSO_DIAG_LIVE) -----

    /// When `SASSO_DIAG_LIVE=1` and a `sass` binary is reachable
    /// (`SASS_BIN=/path/to/sass`, else `sass` on PATH), drive the real
    /// compiler and assert our snippet block is byte-identical to dart's.
    #[test]
    fn live_dart_parity() {
        if std::env::var("SASSO_DIAG_LIVE").as_deref() != Ok("1") {
            return;
        }
        let bin = std::env::var("SASS_BIN").unwrap_or_else(|_| "sass".to_string());

        // (source, message, span, glyphs, no_unicode_flag). The `length` for
        // the multi-line case is computed from `byte_index` so the fixture
        // stays readable.
        let ml_src = "a{b: (1px +\n2s)}\n";
        let ml_len = byte_index(ml_src, 2, 3) - byte_index(ml_src, 1, 7);
        let cases: &[(&str, &str, Span, GlyphSet, bool)] = &[
            (
                "a {\n  b: $undefined;\n}\n",
                "Undefined variable.",
                Span {
                    line: 2,
                    col: 6,
                    length: 10,
                },
                GlyphSet::Unicode,
                false,
            ),
            (
                "a {\n  b: $undefined;\n}\n",
                "Undefined variable.",
                Span {
                    line: 2,
                    col: 6,
                    length: 10,
                },
                GlyphSet::Ascii,
                true,
            ),
            (
                "@error \"x\";\n",
                "\"x\"",
                Span {
                    line: 1,
                    col: 1,
                    length: 10,
                },
                GlyphSet::Unicode,
                false,
            ),
            (
                "a {\n\tb: $x;\n}\n",
                "Undefined variable.",
                Span {
                    line: 2,
                    col: 5,
                    length: 2,
                },
                GlyphSet::Unicode,
                false,
            ),
            (
                ml_src,
                "1px and 2s have incompatible units.",
                Span {
                    line: 1,
                    col: 7,
                    length: ml_len,
                },
                GlyphSet::Unicode,
                false,
            ),
            (
                ml_src,
                "1px and 2s have incompatible units.",
                Span {
                    line: 1,
                    col: 7,
                    length: ml_len,
                },
                GlyphSet::Ascii,
                true,
            ),
        ];

        for (i, (src, msg, span, glyphs, no_unicode)) in cases.iter().enumerate() {
            let dir = std::env::temp_dir().join(format!("sasso-diag-{}-{}", std::process::id(), i));
            let _ = std::fs::create_dir_all(&dir);
            let path = dir.join("input.scss");
            std::fs::write(&path, src).expect("write fixture");

            let mut cmd = std::process::Command::new(&bin);
            cmd.arg(&path).arg("--no-color");
            if *no_unicode {
                cmd.arg("--no-unicode");
            }
            let output = match cmd.output() {
                Ok(o) => o,
                Err(_) => return, // sass not runnable; skip silently.
            };
            let stderr = String::from_utf8_lossy(&output.stderr);
            let path_str = path.to_string_lossy().to_string();

            let ours = render_error(msg, src, &path_str, *span, *glyphs);

            // Compare the snippet block: from `Error:` through the location line.
            // dart-sass appends a trailing newline; trim a single one.
            let dart = stderr.trim_end_matches('\n');
            assert_eq!(ours, dart, "\n--- ours ---\n{ours}\n--- dart ---\n{dart}\n");
        }
    }
}
