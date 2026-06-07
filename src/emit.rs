//! Serialize the flattened output tree to CSS.

use crate::eval::{OutItem, OutNode};
use crate::OutputStyle;

pub(crate) fn emit(nodes: &[OutNode], style: OutputStyle) -> String {
    match style {
        OutputStyle::Expanded => emit_expanded(nodes),
        OutputStyle::Compressed => emit_compressed(nodes),
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
        OutNode::Rule { selectors, items } => {
            out.push_str(&indent);
            out.push_str(&selectors.join(", "));
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
        } => {
            out.push_str(&indent);
            out.push_str(prop);
            out.push_str(": ");
            out.push_str(value);
            if *important {
                out.push_str(" !important");
            }
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
        } => {
            out.push_str(&indent);
            out.push_str(prop);
            out.push_str(": ");
            out.push_str(value);
            if *important {
                out.push_str(" !important");
            }
            out.push_str(";\n");
        }
        OutItem::Comment(text) => {
            out.push_str(&indent);
            out.push_str("/*");
            out.push_str(text);
            out.push_str("*/\n");
        }
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

fn emit_node_compressed(out: &mut String, node: &OutNode) {
    match node {
        OutNode::Rule { selectors, items } => {
            let decls: Vec<String> = items
                .iter()
                .filter_map(|it| match it {
                    OutItem::Decl {
                        prop,
                        value,
                        important,
                    } => {
                        let imp = if *important { "!important" } else { "" };
                        Some(format!("{prop}:{value}{imp}"))
                    }
                    OutItem::Comment(_) => None,
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
        } => {
            let imp = if *important { "!important" } else { "" };
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
                out.push(' ');
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
