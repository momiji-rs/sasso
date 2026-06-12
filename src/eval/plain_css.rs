use super::*;

impl<'a> Evaluator<'a> {
    /// Emit a plain-CSS (`.css`) module's statements, preserving nesting (no
    /// Sass flattening), keeping `&` parent references literal, and resolving
    /// only `#{…}` interpolation. The parser has already rejected Sass-only
    /// constructs, so the remaining statements are plain CSS.
    pub(super) fn exec_css(
        &mut self,
        stmts: &[Stmt],
        parents: &[String],
        sink: &mut Sink<'_>,
    ) -> Result<(), Error> {
        let saved = std::mem::replace(&mut self.in_plain_css, true);
        let result = self.exec_css_inner(stmts, parents, sink);
        self.in_plain_css = saved;
        result
    }

    fn exec_css_inner(
        &mut self,
        stmts: &[Stmt],
        parents: &[String],
        sink: &mut Sink<'_>,
    ) -> Result<(), Error> {
        // When the plain-CSS sheet is imported inside a style rule, top-level
        // rules whose selector contains a parent reference `&` keep native
        // CSS-nesting semantics: they are emitted VERBATIM as nested children
        // of one leading parent-selector shell (dart `nestWithin` with
        // `preserveParentSelectors`), while `&`-less rules get the descendant
        // join below.
        if !parents.is_empty() {
            let mut preserved: Vec<OutItem> = Vec::new();
            for stmt in stmts {
                if let Stmt::Rule(r) = stmt {
                    let own = self.css_selectors(&r.selector, true)?;
                    if own.iter().any(|s| part_has_parent_ref(s)) {
                        let inner = self.css_body(&r.body)?;
                        if !inner.is_empty() {
                            preserved.push(OutItem::NestedRule {
                                selectors: own,
                                items: inner,
                            });
                        }
                    }
                }
            }
            if !preserved.is_empty() {
                sink.push_at_rule(OutNode::Rule {
                    selectors: parents.to_vec(),
                    linebreaks: Vec::new(),
                    items: preserved,
                    lines: SrcLines::default(),
                    extend_base: usize::MAX,
                });
            }
        }
        for stmt in stmts {
            match stmt {
                Stmt::Rule(r) => {
                    // When the plain-CSS sheet is imported inside a style rule,
                    // its outermost rules nest under the Sass parent (descendant
                    // join); inner nesting stays native (dart-sass `nestWithin`
                    // with `preserveParentSelectors`). The sheet's own top level
                    // always rejects leading combinators — also when merged
                    // under a Sass parent (dart checks in the merge branch).
                    let own = self.css_selectors(&r.selector, true)?;
                    // A `&`-bearing rule was already emitted in the leading
                    // parent shell above.
                    if !parents.is_empty() && own.iter().any(|s| part_has_parent_ref(s)) {
                        continue;
                    }
                    let selectors: Vec<String> = if parents.is_empty() {
                        own
                    } else {
                        parents
                            .iter()
                            .flat_map(|p| own.iter().map(move |s| format!("{p} {s}")))
                            .collect()
                    };
                    let (items, bubbled) = self.css_rule_children(&r.body, &selectors)?;
                    // A childless rule is invisible (dart-sass skips it when
                    // serializing) — e.g. when its whole body bubbled out.
                    if !items.is_empty() {
                        sink.push_at_rule(OutNode::Rule {
                            selectors,
                            linebreaks: Vec::new(),
                            items,
                            lines: self.stamp(SrcLines {
                                file: 0,
                                start: r.brace_line,
                                end: r.end_line,
                                col: 0,
                            }),
                            extend_base: usize::MAX,
                        });
                    }
                    for node in bubbled {
                        sink.push_at_rule(node);
                    }
                }
                Stmt::Comment(c, lines) => {
                    let text = self.eval_template(c)?;
                    let lines = self.stamp(*lines);
                    sink.push_at_rule(OutNode::Comment(text, lines));
                }
                // A plain CSS file never inlines an `@import`; every entry is
                // emitted verbatim (`@import "x";` / `@import url(x);`), matching
                // dart-sass loading a `.css` stylesheet.
                Stmt::Import(args) => {
                    for arg in args {
                        let text = match arg {
                            ImportArg::Css { url, modifiers } => self.serialize_css_import(url, modifiers)?,
                            ImportArg::Sass { path, .. } => format!("\"{path}\""),
                        };
                        sink.push_at_rule(OutNode::Raw(format!("@import {text};")));
                    }
                }
                Stmt::Media { query, body, lines } => {
                    let queries = self.resolve_media_queries(query)?;
                    let prelude = serialize_media_queries(&queries);
                    let out_body = self.css_at_body(body)?;
                    if !out_body.is_empty() {
                        let lines = self.stamp(*lines);
                        sink.push_at_rule(OutNode::AtRule {
                            name: "media".to_string(),
                            prelude,
                            body: out_body,
                            has_block: true,
                            lines,
                        });
                    }
                }
                Stmt::Supports { condition, body } => {
                    let prelude = self.serialize_supports_condition(condition)?;
                    let out_body = self.css_at_body(body)?;
                    if !out_body.is_empty() {
                        sink.push_at_rule(OutNode::AtRule {
                            name: "supports".to_string(),
                            prelude,
                            body: out_body,
                            has_block: true,
                            lines: SrcLines::default(),
                        });
                    }
                }
                Stmt::AtRule {
                    name,
                    prelude,
                    body,
                    lines,
                } => {
                    let prelude_s = self.eval_template(prelude)?.trim().to_string();
                    let lines = self.stamp(*lines);
                    match body {
                        None => sink.push_at_rule(OutNode::AtRule {
                            name: name.clone(),
                            prelude: prelude_s,
                            body: Vec::new(),
                            has_block: false,
                            lines,
                        }),
                        Some(b) => {
                            let out_body = self.css_at_body(b)?;
                            sink.push_at_rule(OutNode::AtRule {
                                name: name.clone(),
                                prelude: prelude_s,
                                body: out_body,
                                has_block: true,
                                lines,
                            });
                        }
                    }
                }
                Stmt::Keyframes {
                    name,
                    prelude,
                    body,
                    lines,
                } => {
                    let prelude_s = self.eval_template(prelude)?.trim().to_string();
                    let out_body = self.css_at_body(body)?;
                    let lines = self.stamp(*lines);
                    sink.push_at_rule(OutNode::AtRule {
                        name: name.clone(),
                        prelude: prelude_s,
                        body: out_body,
                        has_block: true,
                        lines,
                    });
                }
                // A plain-CSS custom `@function --x` is emitted verbatim, same
                // as in an SCSS sheet.
                Stmt::CssCustomAtRule { name, prelude, body } => {
                    self.eval_css_custom_at_rule(name, prelude, body, sink)?;
                }
                _ => {}
            }
        }
        Ok(())
    }

    /// Build the body of a top-level plain-CSS at-rule: style rules (with their
    /// own first-level bubbling), bare declarations, comments, and nested
    /// at-rules.
    fn css_at_body(&mut self, stmts: &[Stmt]) -> Result<Vec<OutNode>, Error> {
        let mut out: Vec<OutNode> = Vec::new();
        for stmt in stmts {
            match stmt {
                Stmt::Rule(r) => {
                    let selectors = self.css_selectors(&r.selector, false)?;
                    let (items, bubbled) = self.css_rule_children(&r.body, &selectors)?;
                    if !items.is_empty() {
                        out.push(OutNode::Rule {
                            selectors,
                            linebreaks: Vec::new(),
                            items,
                            lines: self.stamp(SrcLines {
                                file: 0,
                                start: r.brace_line,
                                end: r.end_line,
                                col: 0,
                            }),
                            extend_base: usize::MAX,
                        });
                    }
                    out.extend(bubbled);
                }
                Stmt::Decl(d) => {
                    let prop = self.eval_template(&d.property)?.trim().to_string();
                    let value = self.eval_expr(&d.value)?.to_css(false);
                    out.push(OutNode::AtDecl {
                        prop,
                        value,
                        important: d.important,
                        custom: false,
                        lines: self.stamp(SrcLines {
                            file: 0,
                            start: d.pos.line as u32,
                            end: d.end_line,
                            col: 0,
                        }),
                    });
                }
                Stmt::CustomDecl(d) => {
                    let prop = self.eval_template(&d.property)?.trim().to_string();
                    let value = self.eval_template(&d.value)?;
                    out.push(OutNode::AtDecl {
                        prop,
                        value,
                        important: false,
                        custom: true,
                        lines: self.stamp(SrcLines {
                            file: 0,
                            start: d.pos.line as u32,
                            end: d.end_line,
                            col: 0,
                        }),
                    });
                }
                Stmt::Comment(c, lines) => {
                    let text = self.eval_template(c)?;
                    let lines = self.stamp(*lines);
                    out.push(OutNode::Comment(text, lines));
                }
                Stmt::Media { query, body, lines } => {
                    let queries = self.resolve_media_queries(query)?;
                    let prelude = serialize_media_queries(&queries);
                    let inner = self.css_at_body(body)?;
                    if !inner.is_empty() {
                        let lines = self.stamp(*lines);
                        out.push(OutNode::AtRule {
                            name: "media".to_string(),
                            prelude,
                            body: inner,
                            has_block: true,
                            lines,
                        });
                    }
                }
                Stmt::Supports { condition, body } => {
                    let prelude = self.serialize_supports_condition(condition)?;
                    let inner = self.css_at_body(body)?;
                    if !inner.is_empty() {
                        out.push(OutNode::AtRule {
                            name: "supports".to_string(),
                            prelude,
                            body: inner,
                            has_block: true,
                            lines: SrcLines::default(),
                        });
                    }
                }
                Stmt::AtRule {
                    name,
                    prelude,
                    body,
                    lines,
                } => {
                    let prelude_s = self.eval_template(prelude)?.trim().to_string();
                    let lines = self.stamp(*lines);
                    match body {
                        None => out.push(OutNode::AtRule {
                            name: name.clone(),
                            prelude: prelude_s,
                            body: Vec::new(),
                            has_block: false,
                            lines,
                        }),
                        Some(b) => {
                            let inner = self.css_at_body(b)?;
                            out.push(OutNode::AtRule {
                                name: name.clone(),
                                prelude: prelude_s,
                                body: inner,
                                has_block: true,
                                lines,
                            });
                        }
                    }
                }
                Stmt::Import(args) => {
                    for arg in args {
                        let text = match arg {
                            ImportArg::Css { url, modifiers } => self.serialize_css_import(url, modifiers)?,
                            ImportArg::Sass { path, .. } => format!("\"{path}\""),
                        };
                        out.push(OutNode::Raw(format!("@import {text};")));
                    }
                }
                _ => {}
            }
        }
        Ok(out)
    }

    /// Build the children of a *top-level* plain-CSS style rule: declarations
    /// and nested rules stay in the block; a block at-rule (`@media` etc.)
    /// bubbles out wrapping a copy of the parent rule (dart-sass's standard
    /// at-rule bubbling — `a {@media b {c: d}}` → `@media b { a { c: d } }`).
    /// Deeper levels never bubble (see [`Evaluator::css_body`]).
    #[allow(clippy::type_complexity)]
    fn css_rule_children(
        &mut self,
        stmts: &[Stmt],
        parent_selectors: &[String],
    ) -> Result<(Vec<OutItem>, Vec<OutNode>), Error> {
        let mut items = Vec::new();
        let mut bubbled: Vec<OutNode> = Vec::new();
        let bubble = |name: &str, prelude: String, inner: Vec<OutItem>, bubbled: &mut Vec<OutNode>| {
            if inner.is_empty() {
                return;
            }
            bubbled.push(OutNode::AtRule {
                name: name.to_string(),
                prelude,
                body: vec![OutNode::Rule {
                    selectors: parent_selectors.to_vec(),
                    linebreaks: Vec::new(),
                    items: inner,
                    lines: SrcLines::default(),
                    extend_base: usize::MAX,
                }],
                has_block: true,
                lines: SrcLines::default(),
            });
        };
        for stmt in stmts {
            match stmt {
                Stmt::Media {
                    query,
                    body,
                    lines: _,
                } => {
                    let queries = self.resolve_media_queries(query)?;
                    let prelude = serialize_media_queries(&queries);
                    let inner = self.css_body(body)?;
                    bubble("media", prelude, inner, &mut bubbled);
                }
                Stmt::Supports { condition, body } => {
                    let prelude = self.serialize_supports_condition(condition)?;
                    let inner = self.css_body(body)?;
                    bubble("supports", prelude, inner, &mut bubbled);
                }
                Stmt::AtRule {
                    name,
                    prelude,
                    body: Some(b),
                    ..
                } => {
                    let prelude_s = self.eval_template(prelude)?.trim().to_string();
                    let inner = self.css_body(b)?;
                    bubble(name, prelude_s, inner, &mut bubbled);
                }
                other => self.css_body_stmt(other, &mut items)?,
            }
        }
        Ok((items, bubbled))
    }

    /// Resolve a plain-CSS selector to its comma-separated parts, keeping `&`
    /// and combinators verbatim (no parent resolution), and rejecting the
    /// Sass-only selector forms that plain CSS forbids.
    fn css_selectors(&mut self, sel: &[crate::ast::TplPiece], top_level: bool) -> Result<Vec<String>, Error> {
        let s = self.eval_template(sel)?;
        let parts: Vec<String> = split_commas(&s)
            .into_iter()
            .map(|p| p.trim().to_string())
            .filter(|p| !p.is_empty())
            .collect();
        for p in &parts {
            validate_plain_css_selector(p, top_level)?;
        }
        Ok(parts)
    }

    /// Build a plain-CSS rule body below the first nesting level: declarations
    /// and nested style rules with nesting preserved (`OutItem::NestedRule`),
    /// and block at-rules kept in place (`OutItem::NestedAtRule`) — dart-sass
    /// `_hasCssNesting` skips bubbling once nesting is already native.
    fn css_body(&mut self, stmts: &[Stmt]) -> Result<Vec<OutItem>, Error> {
        let mut items = Vec::new();
        for stmt in stmts {
            self.css_body_stmt(stmt, &mut items)?;
        }
        Ok(items)
    }

    /// Process one plain-CSS statement into rule-body items (the shared body of
    /// [`Evaluator::css_body`] and the non-bubbling arm of
    /// [`Evaluator::css_rule_children`]).
    fn css_body_stmt(&mut self, stmt: &Stmt, items: &mut Vec<OutItem>) -> Result<(), Error> {
        match stmt {
            Stmt::Decl(d) => {
                let prop = self.eval_template(&d.property)?.trim().to_string();
                let value = self.eval_expr(&d.value)?.to_css(false);
                items.push(OutItem::Decl {
                    prop,
                    value,
                    important: d.important,
                    custom: false,
                    lines: self.stamp(SrcLines {
                        file: 0,
                        start: d.pos.line as u32,
                        end: d.end_line,
                        col: 0,
                    }),
                });
            }
            Stmt::CustomDecl(d) => {
                let prop = self.eval_template(&d.property)?.trim().to_string();
                let value = self.eval_template(&d.value)?;
                items.push(OutItem::Decl {
                    prop,
                    value,
                    important: false,
                    custom: true,
                    lines: self.stamp(SrcLines {
                        file: 0,
                        start: d.pos.line as u32,
                        end: d.end_line,
                        col: 0,
                    }),
                });
            }
            Stmt::Rule(r) => {
                let selectors = self.css_selectors(&r.selector, false)?;
                let inner = self.css_body(&r.body)?;
                // An (recursively) empty nested rule is invisible (dart-sass
                // skips childless rules when serializing).
                if !inner.is_empty() {
                    items.push(OutItem::NestedRule {
                        selectors,
                        items: inner,
                    });
                }
            }
            Stmt::Comment(c, lines) => {
                let text = self.eval_template(c)?;
                let lines = self.stamp(*lines);
                items.push(OutItem::Comment(text, lines));
            }
            // A nested `@import` inside a plain-CSS rule is preserved
            // verbatim, like a top-level one (see `exec_css`).
            Stmt::Import(args) => {
                for arg in args {
                    let prelude = match arg {
                        ImportArg::Css { url, modifiers } => self.serialize_css_import(url, modifiers)?,
                        ImportArg::Sass { path, .. } => format!("\"{path}\""),
                    };
                    items.push(OutItem::ChildlessAtRule {
                        name: "import".to_string(),
                        prelude,
                        lines: SrcLines::default(),
                    });
                }
            }
            Stmt::Media {
                query,
                body,
                lines: _,
            } => {
                let queries = self.resolve_media_queries(query)?;
                let prelude = serialize_media_queries(&queries);
                let inner = self.css_body(body)?;
                if !inner.is_empty() {
                    items.push(OutItem::NestedAtRule {
                        name: "media".to_string(),
                        prelude,
                        items: inner,
                    });
                }
            }
            Stmt::Supports { condition, body } => {
                let prelude = self.serialize_supports_condition(condition)?;
                let inner = self.css_body(body)?;
                if !inner.is_empty() {
                    items.push(OutItem::NestedAtRule {
                        name: "supports".to_string(),
                        prelude,
                        items: inner,
                    });
                }
            }
            Stmt::AtRule {
                name,
                prelude,
                body,
                lines,
            } => {
                let prelude_s = self.eval_template(prelude)?.trim().to_string();
                match body {
                    None => {
                        let lines = self.stamp(*lines);
                        items.push(OutItem::ChildlessAtRule {
                            name: name.clone(),
                            prelude: prelude_s,
                            lines,
                        });
                    }
                    Some(b) => {
                        let inner = self.css_body(b)?;
                        if !inner.is_empty() {
                            items.push(OutItem::NestedAtRule {
                                name: name.clone(),
                                prelude: prelude_s,
                                items: inner,
                            });
                        }
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }
}
