//! The deprecation-warning registry.
//!
//! Each deprecation dart-sass 1.100 fires has a stable `[id]` tag, an optional
//! `More info` URL line, and a (possibly multi-line, possibly dynamic) message
//! body. This module models the ids the evaluator emits and renders the header
//! block (everything above the snippet), byte-for-byte from the captured
//! fixtures in `tests/fixtures/diagnostics/deprecation-*`.
//!
//! The snippet + 4-space-indented stack trace are appended by the evaluator
//! (it owns the source/url/glyph context); this module only produces the
//! `DEPRECATION WARNING [id]: …` header and any `More info` lines.

/// A single deprecation occurrence: its id tag, the message body (which may be
/// several lines and carry dynamic content), and the optional `More info` line.
pub(crate) struct Deprecation {
    /// The `[id]` tag printed in the header.
    pub id: &'static str,
    /// The message body, printed right after `DEPRECATION WARNING [id]: `. May
    /// contain embedded newlines for multi-line messages.
    pub message: String,
    /// The text of the trailing info line, e.g.
    /// `More info and automated migrator: https://sass-lang.com/d/import`, or
    /// `None` for the ids dart-sass prints without one.
    pub more_info: Option<String>,
}

impl Deprecation {
    /// The `@import` deprecation — a fully static message.
    pub(crate) fn import() -> Self {
        Deprecation {
            id: "import",
            message: "Sass @import rules are deprecated and will be removed in Dart Sass 3.0.0.".to_string(),
            more_info: Some("More info and automated migrator: https://sass-lang.com/d/import".to_string()),
        }
    }

    /// Render the header block: `DEPRECATION WARNING [id]: <message>` followed
    /// by a blank line and the `More info` line (when present), then a blank
    /// line and the top snippet-gutter is left to the caller. Returns the lines
    /// from the header down to (and including) the blank line that precedes the
    /// snippet — i.e. everything before the `  ,` gutter row.
    pub(crate) fn render_header(&self) -> String {
        let mut out = format!("DEPRECATION WARNING [{}]: {}\n", self.id, self.message);
        if let Some(info) = &self.more_info {
            out.push('\n');
            out.push_str(info);
            out.push('\n');
        }
        out.push('\n');
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn import_header_matches_fixture_prefix() {
        let d = Deprecation::import();
        let expected = "\
DEPRECATION WARNING [import]: Sass @import rules are deprecated and will be removed in Dart Sass 3.0.0.

More info and automated migrator: https://sass-lang.com/d/import

";
        assert_eq!(d.render_header(), expected);
    }
}
