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
        match node {
            OutNode::Rule { selectors, items } => {
                out.push_str(&selectors.join(", "));
                out.push_str(" {\n");
                for item in items {
                    match item {
                        OutItem::Decl {
                            prop,
                            value,
                            important,
                        } => {
                            out.push_str("  ");
                            out.push_str(prop);
                            out.push_str(": ");
                            out.push_str(value);
                            if *important {
                                out.push_str(" !important");
                            }
                            out.push_str(";\n");
                        }
                        OutItem::Comment(text) => {
                            out.push_str("  /*");
                            out.push_str(text);
                            out.push_str("*/\n");
                        }
                    }
                }
                out.push_str("}\n");
            }
            OutNode::Comment(text) => {
                out.push_str("/*");
                out.push_str(text);
                out.push_str("*/\n");
            }
            OutNode::Raw(s) => {
                out.push_str(s);
                out.push('\n');
            }
            OutNode::Blank => out.push('\n'),
        }
    }
    out
}

fn emit_compressed(nodes: &[OutNode]) -> String {
    let mut out = String::new();
    for node in nodes {
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
                    continue;
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
        }
    }
    out
}
