//! Serialize the flattened output tree to CSS.

use crate::ast::SrcLines;
use crate::eval::{OutItem, OutNode};
use crate::OutputStyle;

pub(crate) fn emit(nodes: &[OutNode], style: OutputStyle) -> String {
    let body = match style {
        OutputStyle::Expanded => emit_expanded(nodes),
        OutputStyle::Compressed => emit_compressed(nodes),
    };
    // dart-sass declares UTF-8 when the output contains any non-ASCII code
    // point: expanded output gets a leading `@charset "UTF-8";`, compressed
    // output gets a UTF-8 byte-order mark instead.
    if body.is_ascii() {
        return body;
    }
    match style {
        OutputStyle::Expanded => format!("@charset \"UTF-8\";\n{body}"),
        OutputStyle::Compressed => format!("\u{FEFF}{body}"),
    }
}

fn emit_expanded(nodes: &[OutNode]) -> String {
    let mut out = String::new();
    let mut prev = SrcLines::default();
    for node in nodes {
        emit_node_expanded(&mut out, node, 0, &mut prev);
    }
    out
}

/// dart `_isTrailingComment` (expanded style only): a comment joins the
/// previous line when it starts on the line the previous construct ended —
/// or, for a block's first child, on the parent's opening-brace line — and
/// both come from the same source file. `prev.end` always holds the
/// comparison line; a zero file id disables the rule.
///
/// A `previous` whose span is IDENTICAL to the comment's is a clone of the
/// same comment (the same file imported twice): dart's span-containment
/// branch then walks back for a `{`, finds `searchFrom < 0`, and rejects it.
fn is_trailing(comment: SrcLines, prev: SrcLines) -> bool {
    comment.file != 0 && comment.file == prev.file && comment.start == prev.end && comment != prev
}

/// The previous-sibling seed for a block's children: the first child compares
/// against the block's opening-brace line (dart walks back to the `{`).
fn block_start(lines: SrcLines) -> SrcLines {
    SrcLines {
        file: lines.file,
        start: lines.start,
        end: lines.start,
        col: 0,
    }
}

/// Append a trailing comment to the line already in `out`: drop the pending
/// newline(s) — at the root a group blank line may sit between — then write
/// ` /*…*/` with continuation lines at indentation 0 (dart saves and zeroes
/// `_indentation` for trailing comments).
fn push_trailing_comment(out: &mut String, text: &str) {
    while out.ends_with('\n') {
        out.pop();
    }
    out.push_str(" /*");
    push_comment_text(out, text, "");
    out.push_str("*/\n");
}

/// Close an expanded block opened with `" {\n"`. When its only child was a
/// trailing comment the whole block stays on one line (dart: ` }`, e.g.
/// `@font-face { /**/ }`); otherwise the `}` gets its own indented line.
fn close_block(out: &mut String, indent: &str, children: usize, last_joined: bool) {
    if children == 1 && last_joined {
        out.pop(); // the trailing comment's newline
        out.push_str(" }\n");
    } else {
        out.push_str(indent);
        out.push_str("}\n");
    }
}

/// Indentation for a nesting depth (two spaces per level) without allocating:
/// `"  ".repeat(depth)` ran once per emitted node/item, which is tens of
/// thousands of short-lived Strings on a large output. Depths beyond the
/// precomputed pad (rare) fall back to an owned String.
fn indent_for(depth: usize) -> std::borrow::Cow<'static, str> {
    const PAD: &str = "                                                                "; // 32 levels
    match PAD.get(..depth * 2) {
        Some(s) => std::borrow::Cow::Borrowed(s),
        None => std::borrow::Cow::Owned("  ".repeat(depth)),
    }
}

/// Render one node at the given nesting `depth` (0 = document root). Each
/// extra level adds two spaces of indentation. `prev` carries the previous
/// sibling's source lines for the trailing-comment rule (its `end` is the
/// comparison line); returns whether THIS node was emitted as a trailing
/// comment joined onto the previous line.
fn emit_node_expanded(out: &mut String, node: &OutNode, depth: usize, prev: &mut SrcLines) -> bool {
    let indent = indent_for(depth);
    let indent = indent.as_ref();
    match node {
        // A module-scope wrapper is transparent: emit its contents in place
        // (the previous-sibling line state flows through the boundary).
        OutNode::ModuleScope { nodes, .. } => {
            let mut joined = false;
            for n in nodes {
                joined = emit_node_expanded(out, n, depth, prev);
            }
            return joined;
        }
        OutNode::Rule {
            selectors,
            linebreaks,
            items,
            lines,
            ..
        } => {
            out.push_str(indent);
            // A complex selector flagged with a source line break starts on its
            // own line (aligned to the rule's indent); others are `, `-joined.
            for (i, sel) in selectors.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                    if linebreaks.get(i).copied().unwrap_or(false) {
                        out.push('\n');
                        out.push_str(indent);
                    } else {
                        out.push(' ');
                    }
                }
                out.push_str(sel);
            }
            out.push_str(" {\n");
            let mut inner = block_start(*lines);
            let mut joined = false;
            for item in items {
                joined = emit_item_expanded(out, item, depth + 1, &mut inner);
            }
            close_block(out, indent, items.len(), joined);
            *prev = *lines;
        }
        OutNode::Comment(text, lines) => {
            if is_trailing(*lines, *prev) {
                push_trailing_comment(out, text);
                *prev = *lines;
                return true;
            }
            out.push_str(indent);
            out.push_str("/*");
            push_comment_text(out, text, indent);
            out.push_str("*/\n");
            *prev = *lines;
        }
        OutNode::Raw(s) => {
            // Internal sentinels (style-group-end) never reach the output —
            // and don't disturb the previous-sibling line state.
            if s.starts_with('\u{0}') {
                return false;
            }
            out.push_str(indent);
            out.push_str(s);
            out.push('\n');
            *prev = SrcLines::default();
        }
        OutNode::Blank => {
            // A synthetic group separator: dart has no such node, so it leaves
            // the previous-sibling line state alone (a trailing comment after
            // it joins across, swallowing the blank like dart does).
            out.push('\n');
        }
        OutNode::AtDecl {
            prop,
            value,
            important,
            custom,
            lines,
        } => {
            out.push_str(indent);
            out.push_str(prop);
            emit_decl_value_expanded(out, value, *important, *custom, lines.col as usize, indent);
            out.push_str(";\n");
            *prev = *lines;
        }
        OutNode::AtRule {
            name,
            prelude,
            body,
            has_block,
            lines,
        } => {
            out.push_str(indent);
            out.push('@');
            out.push_str(name);
            if !prelude.is_empty() {
                out.push(' ');
                out.push_str(prelude);
            }
            *prev = *lines;
            if !has_block {
                out.push_str(";\n");
                return false;
            }
            if body.is_empty() {
                out.push_str(" {}\n");
                return false;
            }
            out.push_str(" {\n");
            let mut inner = block_start(*lines);
            let mut children = 0usize;
            let mut joined = false;
            for child in body {
                let before = out.len();
                let j = emit_node_expanded(out, child, depth + 1, &mut inner);
                // Sentinels emit nothing and don't count as children.
                if out.len() > before {
                    children += 1;
                    joined = j;
                }
            }
            close_block(out, indent, children, joined);
        }
    }
    false
}

/// Render one rule-block item; same `prev`/return contract as
/// [`emit_node_expanded`].
fn emit_item_expanded(out: &mut String, item: &OutItem, depth: usize, prev: &mut SrcLines) -> bool {
    let indent = indent_for(depth);
    let indent = indent.as_ref();
    match item {
        OutItem::Decl {
            prop,
            value,
            important,
            custom,
            lines,
        } => {
            out.push_str(indent);
            out.push_str(prop);
            emit_decl_value_expanded(out, value, *important, *custom, lines.col as usize, indent);
            out.push_str(";\n");
            *prev = *lines;
        }
        OutItem::Comment(text, lines) => {
            if is_trailing(*lines, *prev) {
                push_trailing_comment(out, text);
                *prev = *lines;
                return true;
            }
            out.push_str(indent);
            out.push_str("/*");
            push_comment_text(out, text, indent);
            out.push_str("*/\n");
            *prev = *lines;
        }
        OutItem::ChildlessAtRule { name, prelude, lines } => {
            out.push_str(indent);
            out.push('@');
            out.push_str(name);
            if !prelude.is_empty() {
                out.push(' ');
                out.push_str(prelude);
            }
            out.push_str(";\n");
            *prev = *lines;
        }
        OutItem::NestedRule { selectors, items } => {
            out.push_str(indent);
            out.push_str(&selectors.join(", "));
            out.push_str(" {\n");
            let mut inner = SrcLines::default();
            let mut joined = false;
            for child in items {
                joined = emit_item_expanded(out, child, depth + 1, &mut inner);
            }
            close_block(out, indent, items.len(), joined);
            *prev = SrcLines::default();
        }
        OutItem::NestedAtRule { name, prelude, items } => {
            out.push_str(indent);
            out.push('@');
            out.push_str(name);
            if !prelude.is_empty() {
                out.push(' ');
                out.push_str(prelude);
            }
            out.push_str(" {\n");
            let mut inner = SrcLines::default();
            let mut joined = false;
            for child in items {
                joined = emit_item_expanded(out, child, depth + 1, &mut inner);
            }
            close_block(out, indent, items.len(), joined);
            *prev = SrcLines::default();
        }
    }
    false
}

/// Append the `: value [!important]` portion of an expanded declaration. A
/// custom property emits its value verbatim right after the colon (its leading
/// whitespace is part of `value`, dart-sass adds no space) and never appends an
/// `!important` flag; a normal declaration uses the canonical `: ` separator.
/// A multi-line custom value is re-indented (dart `_writeReindentedValue`):
/// `name_col` is the declaration name's 0-based source column and `indent`
/// the current output indentation.
fn emit_decl_value_expanded(
    out: &mut String,
    value: &str,
    important: bool,
    custom: bool,
    name_col: usize,
    indent: &str,
) {
    if custom {
        out.push(':');
        match minimum_indentation(value) {
            MinIndent::SingleLine => out.push_str(value),
            MinIndent::Trailing => {
                out.push_str(trim_ascii_right_exclude_escape(value));
                out.push(' ');
            }
            MinIndent::Min(m) => write_with_indent(out, value, m.min(name_col), indent),
        }
        return;
    }
    out.push_str(": ");
    out.push_str(value);
    if important {
        out.push_str(" !important");
    }
}

/// dart `_minimumIndentation`: the minimum indentation of `text`'s
/// continuation lines, skipping blank lines.
enum MinIndent {
    /// `text` has no newline — emit verbatim.
    SingleLine,
    /// Every continuation line is blank (dart's `-1`) — trim right + space.
    Trailing,
    /// The least indented non-blank continuation line starts at this column.
    Min(usize),
}

fn minimum_indentation(text: &str) -> MinIndent {
    let bytes = text.as_bytes();
    let mut i = match text.find('\n') {
        None => return MinIndent::SingleLine,
        Some(p) => p + 1,
    };
    if i >= bytes.len() {
        return MinIndent::Trailing;
    }
    let mut min: Option<usize> = None;
    while i < bytes.len() {
        let start = i;
        while i < bytes.len() && (bytes[i] == b' ' || bytes[i] == b'\t') {
            i += 1;
        }
        if i >= bytes.len() {
            break; // a trailing all-whitespace line is not counted
        }
        if bytes[i] == b'\n' {
            i += 1; // blank line: not counted
            continue;
        }
        let col = i - start;
        min = Some(min.map_or(col, |m| m.min(col)));
        while i < bytes.len() && bytes[i] != b'\n' {
            i += 1;
        }
        i += 1;
    }
    match min {
        None => MinIndent::Trailing,
        Some(m) => MinIndent::Min(m),
    }
}

/// dart `trimAsciiRight(value, excludeEscape: true)`: strip trailing ASCII
/// whitespace, but keep one character after a terminating backslash so an
/// escaped trailing space survives.
fn trim_ascii_right_exclude_escape(s: &str) -> &str {
    let bytes = s.as_bytes();
    let mut end = bytes.len();
    while end > 0 && matches!(bytes[end - 1], b' ' | b'\t' | b'\n' | b'\r' | b'\x0c') {
        end -= 1;
    }
    if end != 0 && end != bytes.len() && bytes[end - 1] == b'\\' {
        end += 1;
    }
    &s[..end]
}

/// dart `_writeWithIndent`: write the first line verbatim, then each
/// continuation line stripped of `min_indent` characters and prefixed with
/// the current output indentation. Blank lines stay blank; a trailing
/// all-whitespace line folds to a single space.
fn write_with_indent(out: &mut String, text: &str, min_indent: usize, indent: &str) {
    let first_end = text.find('\n').unwrap_or(text.len());
    out.push_str(&text[..first_end]);
    if first_end == text.len() {
        return;
    }
    let mut i = first_end + 1;
    loop {
        let mut line_start = i;
        let mut newlines = 1usize;
        // Scan the whitespace run, counting blank lines; `i` settles on the
        // first non-whitespace character (the run's indentation is ASCII, so
        // byte stepping stays on char boundaries).
        loop {
            if i >= text.len() {
                out.push(' ');
                return;
            }
            match text.as_bytes()[i] {
                b' ' | b'\t' => i += 1,
                b'\n' => {
                    i += 1;
                    line_start = i;
                    newlines += 1;
                }
                _ => break,
            }
        }
        for _ in 0..newlines {
            out.push('\n');
        }
        out.push_str(indent);
        let line_end = text[i..].find('\n').map(|p| i + p).unwrap_or(text.len());
        out.push_str(&text[line_start + min_indent..line_end]);
        if line_end == text.len() {
            return;
        }
        i = line_end + 1;
    }
}

fn emit_compressed(nodes: &[OutNode]) -> String {
    let mut out = String::new();
    for node in nodes {
        emit_node_compressed(&mut out, node);
    }
    out
}

/// Render `nodes` joined for compressed output. A declaration is terminated by
/// `;` before whatever follows it, but a preceding rule/at-rule `}` is its own
/// separator, so no `;` is inserted after it (matching dart-sass).
fn emit_compressed_body(out: &mut String, nodes: &[OutNode]) {
    let mut prev_was_decl = false;
    for node in nodes {
        // Comments and blanks produce no compressed output; don't let them
        // reset the separator state.
        if matches!(node, OutNode::Comment(..) | OutNode::Blank) {
            continue;
        }
        if prev_was_decl {
            out.push(';');
        }
        emit_node_compressed(out, node);
        prev_was_decl = matches!(node, OutNode::AtDecl { .. });
    }
}

/// Serialize a plain-CSS nested rule for compressed output (a rare, untested
/// path — plain CSS is normally emitted expanded).
fn compressed_nested_rule(selectors: &[String], items: &[OutItem]) -> String {
    let inner: Vec<String> = items
        .iter()
        .filter_map(|it| match it {
            OutItem::Decl {
                prop,
                value,
                important,
                custom,
                ..
            } => {
                let imp = if *important && !*custom { "!important" } else { "" };
                let value = fold_value_compressed(value, *custom);
                Some(format!("{prop}:{value}{imp}"))
            }
            OutItem::Comment(..) => None,
            OutItem::ChildlessAtRule { name, prelude, .. } if prelude.is_empty() => Some(format!("@{name}")),
            OutItem::ChildlessAtRule { name, prelude, .. } => Some(format!("@{name} {prelude}")),
            OutItem::NestedRule { selectors, items } => Some(compressed_nested_rule(selectors, items)),
            OutItem::NestedAtRule { name, prelude, items } => {
                Some(compressed_nested_at_rule(name, prelude, items))
            }
        })
        .collect();
    format!("{}{{{}}}", selectors.join(","), inner.join(";"))
}

/// dart `_writeFoldedValue` (compressed custom properties): each newline
/// becomes a single space and the whitespace run following it is dropped.
/// Non-custom values pass through untouched.
fn fold_value_compressed<'v>(value: &'v str, custom: bool) -> std::borrow::Cow<'v, str> {
    if !custom || !value.contains('\n') {
        return std::borrow::Cow::Borrowed(value);
    }
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\n' {
            out.push(c);
            continue;
        }
        out.push(' ');
        while matches!(chars.peek(), Some(' ' | '\t' | '\n' | '\r' | '\x0c')) {
            chars.next();
        }
    }
    std::borrow::Cow::Owned(out)
}

/// Serialize a plain-CSS nested at-rule for compressed output (rare path; see
/// [`compressed_nested_rule`]).
fn compressed_nested_at_rule(name: &str, prelude: &str, items: &[OutItem]) -> String {
    let body = compressed_nested_rule(&[], items);
    // `compressed_nested_rule` with no selectors renders `{...}`; reuse its body.
    if prelude.is_empty() {
        format!("@{name}{body}")
    } else {
        format!("@{name} {prelude}{body}")
    }
}

fn emit_node_compressed(out: &mut String, node: &OutNode) {
    match node {
        OutNode::ModuleScope { nodes, .. } => {
            for n in nodes {
                emit_node_compressed(out, n);
            }
        }
        OutNode::Rule {
            selectors,
            linebreaks: _,
            items,
            ..
        } => {
            let decls: Vec<String> = items
                .iter()
                .filter_map(|it| match it {
                    OutItem::Decl {
                        prop,
                        value,
                        important,
                        custom,
                        ..
                    } => {
                        // A custom property emits its value verbatim (its
                        // leading whitespace is part of `value`) and never gains
                        // an `!important` flag.
                        let imp = if *important && !*custom { "!important" } else { "" };
                        let value = fold_value_compressed(value, *custom);
                        Some(format!("{prop}:{value}{imp}"))
                    }
                    OutItem::Comment(..) => None,
                    OutItem::ChildlessAtRule { name, prelude, .. } => {
                        if prelude.is_empty() {
                            Some(format!("@{name}"))
                        } else {
                            Some(format!("@{name} {prelude}"))
                        }
                    }
                    OutItem::NestedRule { selectors, items } => {
                        Some(compressed_nested_rule(selectors, items))
                    }
                    OutItem::NestedAtRule { name, prelude, items } => {
                        Some(compressed_nested_at_rule(name, prelude, items))
                    }
                })
                .collect();
            if decls.is_empty() {
                return;
            }
            out.push_str(&selectors.join(","));
            out.push('{');
            out.push_str(&decls.join(";"));
            out.push('}');
        }
        // Loud comments are dropped in compressed output (the slice does
        // not yet special-case `/*!` important comments).
        OutNode::Comment(..) => {}
        OutNode::Raw(s) => {
            if !s.starts_with('\u{0}') {
                out.push_str(s)
            }
        }
        OutNode::Blank => {}
        OutNode::AtDecl {
            prop,
            value,
            important,
            custom,
            ..
        } => {
            let imp = if *important && !*custom { "!important" } else { "" };
            out.push_str(prop);
            out.push(':');
            out.push_str(&fold_value_compressed(value, *custom));
            out.push_str(imp);
        }
        OutNode::AtRule {
            name,
            prelude,
            body,
            has_block,
            ..
        } => {
            out.push('@');
            out.push_str(name);
            if !prelude.is_empty() {
                // Compressed `@supports` omits the space before a prelude that
                // begins with `(` (dart-sass `visitCssSupportsRule`).
                let omit_space = name == "supports" && prelude.starts_with('(');
                if !omit_space {
                    out.push(' ');
                }
                out.push_str(prelude);
            }
            if !has_block {
                out.push(';');
                return;
            }
            out.push('{');
            emit_compressed_body(out, body);
            out.push('}');
        }
    }
}

/// Write a loud comment's text with dart's continuation-line handling: every
/// line after the first gains the CURRENT output indentation on top of its
/// own source indentation.
fn push_comment_text(out: &mut String, text: &str, indent: &str) {
    let mut first = true;
    for line in text.split('\n') {
        if !first {
            out.push('\n');
            if !line.is_empty() {
                out.push_str(indent);
            }
        }
        out.push_str(line);
        first = false;
    }
}
