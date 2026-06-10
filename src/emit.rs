//! Serialize the flattened output tree to CSS.

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
    for node in nodes {
        emit_node_expanded(&mut out, node, 0);
    }
    out
}

/// Render one node at the given nesting `depth` (0 = document root). Each
/// extra level adds two spaces of indentation.
fn emit_node_expanded(out: &mut String, node: &OutNode, depth: usize) {
    let indent = "  ".repeat(depth);
    match node {
        // A module-scope wrapper is transparent: emit its contents in place.
        OutNode::ModuleScope { nodes, .. } => {
            for n in nodes {
                emit_node_expanded(out, n, depth);
            }
        }
        OutNode::Rule {
            selectors,
            linebreaks,
            items,
        } => {
            out.push_str(&indent);
            // A complex selector flagged with a source line break starts on its
            // own line (aligned to the rule's indent); others are `, `-joined.
            for (i, sel) in selectors.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                    if linebreaks.get(i).copied().unwrap_or(false) {
                        out.push('\n');
                        out.push_str(&indent);
                    } else {
                        out.push(' ');
                    }
                }
                out.push_str(sel);
            }
            out.push_str(" {\n");
            for item in items {
                emit_item_expanded(out, item, depth + 1);
            }
            out.push_str(&indent);
            out.push_str("}\n");
        }
        OutNode::Comment(text) => {
            out.push_str(&indent);
            out.push_str("/*");
            out.push_str(text);
            out.push_str("*/\n");
        }
        OutNode::Raw(s) => {
            out.push_str(&indent);
            out.push_str(s);
            out.push('\n');
        }
        OutNode::Blank => out.push('\n'),
        OutNode::AtDecl {
            prop,
            value,
            important,
            custom,
        } => {
            out.push_str(&indent);
            out.push_str(prop);
            emit_decl_value_expanded(out, value, *important, *custom, depth);
            out.push_str(";\n");
        }
        OutNode::AtRule {
            name,
            prelude,
            body,
            has_block,
        } => {
            out.push_str(&indent);
            out.push('@');
            out.push_str(name);
            if !prelude.is_empty() {
                out.push(' ');
                out.push_str(prelude);
            }
            if !has_block {
                out.push_str(";\n");
                return;
            }
            if body.is_empty() {
                out.push_str(" {}\n");
                return;
            }
            out.push_str(" {\n");
            for child in body {
                emit_node_expanded(out, child, depth + 1);
            }
            out.push_str(&indent);
            out.push_str("}\n");
        }
    }
}

fn emit_item_expanded(out: &mut String, item: &OutItem, depth: usize) {
    let indent = "  ".repeat(depth);
    match item {
        OutItem::Decl {
            prop,
            value,
            important,
            custom,
        } => {
            out.push_str(&indent);
            out.push_str(prop);
            emit_decl_value_expanded(out, value, *important, *custom, depth);
            out.push_str(";\n");
        }
        OutItem::Comment(text) => {
            out.push_str(&indent);
            out.push_str("/*");
            out.push_str(text);
            out.push_str("*/\n");
        }
        OutItem::ChildlessAtRule { name, prelude } => {
            out.push_str(&indent);
            out.push('@');
            out.push_str(name);
            if !prelude.is_empty() {
                out.push(' ');
                out.push_str(prelude);
            }
            out.push_str(";\n");
        }
        OutItem::NestedRule { selectors, items } => {
            out.push_str(&indent);
            out.push_str(&selectors.join(", "));
            out.push_str(" {\n");
            for child in items {
                emit_item_expanded(out, child, depth + 1);
            }
            out.push_str(&indent);
            out.push_str("}\n");
        }
        OutItem::NestedAtRule { name, prelude, items } => {
            out.push_str(&indent);
            out.push('@');
            out.push_str(name);
            if !prelude.is_empty() {
                out.push(' ');
                out.push_str(prelude);
            }
            out.push_str(" {\n");
            for child in items {
                emit_item_expanded(out, child, depth + 1);
            }
            out.push_str(&indent);
            out.push_str("}\n");
        }
    }
}

/// Append the `: value [!important]` portion of an expanded declaration. A
/// custom property emits its value verbatim right after the colon (its leading
/// whitespace is part of `value`, dart-sass adds no space) and never appends an
/// `!important` flag; a normal declaration uses the canonical `: ` separator.
fn emit_decl_value_expanded(out: &mut String, value: &str, important: bool, custom: bool, _depth: usize) {
    if custom {
        out.push(':');
        out.push_str(value);
        return;
    }
    out.push_str(": ");
    out.push_str(value);
    if important {
        out.push_str(" !important");
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
        if matches!(node, OutNode::Comment(_) | OutNode::Blank) {
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
            } => {
                let imp = if *important && !*custom { "!important" } else { "" };
                Some(format!("{prop}:{value}{imp}"))
            }
            OutItem::Comment(_) => None,
            OutItem::ChildlessAtRule { name, prelude } if prelude.is_empty() => Some(format!("@{name}")),
            OutItem::ChildlessAtRule { name, prelude } => Some(format!("@{name} {prelude}")),
            OutItem::NestedRule { selectors, items } => Some(compressed_nested_rule(selectors, items)),
            OutItem::NestedAtRule { name, prelude, items } => {
                Some(compressed_nested_at_rule(name, prelude, items))
            }
        })
        .collect();
    format!("{}{{{}}}", selectors.join(","), inner.join(";"))
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
        } => {
            let decls: Vec<String> = items
                .iter()
                .filter_map(|it| match it {
                    OutItem::Decl {
                        prop,
                        value,
                        important,
                        custom,
                    } => {
                        // A custom property emits its value verbatim (its
                        // leading whitespace is part of `value`) and never gains
                        // an `!important` flag.
                        let imp = if *important && !*custom { "!important" } else { "" };
                        Some(format!("{prop}:{value}{imp}"))
                    }
                    OutItem::Comment(_) => None,
                    OutItem::ChildlessAtRule { name, prelude } => {
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
        OutNode::Comment(_) => {}
        OutNode::Raw(s) => out.push_str(s),
        OutNode::Blank => {}
        OutNode::AtDecl {
            prop,
            value,
            important,
            custom,
        } => {
            let imp = if *important && !*custom { "!important" } else { "" };
            out.push_str(prop);
            out.push(':');
            out.push_str(value);
            out.push_str(imp);
        }
        OutNode::AtRule {
            name,
            prelude,
            body,
            has_block,
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
