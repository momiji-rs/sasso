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
    /// The text of the trailing line, e.g.
    /// `More info and automated migrator: https://sass-lang.com/d/import` or
    /// `call-string`'s `Recommendation: …`, or `None` for the ids dart-sass
    /// prints without one.
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

    /// The `global-builtin` deprecation: a global function that has a
    /// `sass:*` module equivalent. `replacement` is the member dart names
    /// (`map.get`, `color.adjust`, `math.is-unitless`), and the info line is
    /// the migrator's — dart prints the `@import` URL here, not one of its own.
    pub(crate) fn global_builtin(replacement: &str) -> Self {
        Deprecation {
            id: "global-builtin",
            message: format!(
                "Global built-in functions are deprecated and will be removed in Dart Sass 3.0.0.\nUse {replacement} instead."
            ),
            more_info: Some("More info and automated migrator: https://sass-lang.com/d/import".to_string()),
        }
    }

    /// The `feature-exists` deprecation: the function itself is going away, in
    /// its global spelling and as `meta.feature-exists` alike.
    pub(crate) fn feature_exists() -> Self {
        Deprecation {
            id: "feature-exists",
            message: "The feature-exists() function is deprecated.".to_string(),
            more_info: Some("More info: https://sass-lang.com/d/feature-exists".to_string()),
        }
    }

    /// The `call-string` deprecation: `call("name")` looks a function up by
    /// name instead of taking a reference. The trailing line is a
    /// `Recommendation:` rather than a `More info:`, in the same slot.
    pub(crate) fn call_string(name: &str) -> Self {
        Deprecation {
            id: "call-string",
            message: "Passing a string to call() is deprecated and will be illegal in Dart Sass 2.0.0."
                .to_string(),
            // The name is a STRING in the suggested code, so it is serialized
            // as one — dart writes `call(get-function('a\\"b'))` for a name
            // that holds a quote.
            more_info: Some(format!(
                "Recommendation: call(get-function({}))",
                crate::value::serialize_quoted(name)
            )),
        }
    }

    /// The `color-functions` deprecation: a legacy `sass:color` member that
    /// Color 4 replaced. `qualified` is the name dart prints — `red()` for the
    /// global spelling, `color.red()` for one reached through the module — and
    /// `suggestions` is the replacement code, one line each, computed from the
    /// call's own arguments. dart labels one "Suggestion" and several
    /// "Suggestions".
    pub(crate) fn color_functions(qualified: &str, suggestions: &[String]) -> Self {
        let label = if suggestions.len() == 1 {
            "Suggestion"
        } else {
            "Suggestions"
        };
        Deprecation {
            id: "color-functions",
            message: format!(
                "{qualified}() is deprecated. {label}:\n\n{}",
                suggestions.join("\n")
            ),
            more_info: Some("More info: https://sass-lang.com/d/color-functions".to_string()),
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
