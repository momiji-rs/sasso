use super::*;

impl<'a> Evaluator<'a> {
    /// Evaluate a generic at-rule. The prelude template is resolved to a
    /// string; the body (when present) is executed so that nested rules carry
    /// the enclosing selectors INSIDE the at-rule, and the whole node hoists to
    /// the document root (bubbling).
    pub(super) fn eval_at_rule(
        &mut self,
        name: &str,
        prelude: &[TplPiece],
        body: Option<&[Stmt]>,
        lines: SrcLines,
        parents: &[String],
        sink: &mut Sink<'_>,
    ) -> Result<(), Error> {
        let prelude = self.eval_template(prelude)?;
        let Some(stmts) = body else {
            // dart-sass strips a top-level (or bubbled-out) `@charset` entirely,
            // but keeps one that appears inside a style rule's block.
            if name == "charset" && !sink.is_rule() {
                return Ok(());
            }
            sink.push_childless_at_rule(name.to_string(), prelude, lines);
            return Ok(());
        };
        // `@font-face` (exactly, case-sensitively, unprefixed) holds plain
        // declarations: dart-sass does NOT carry the enclosing style-rule
        // selector into its body — `a { @font-face { d: e } }` emits a bare
        // `@font-face { d: e }`. Every other at-rule (including `@page`,
        // `@-moz-font-face`, and unknown directives) wraps its body in the
        // enclosing selector — UNLESS we're directly inside `@at-root`,
        // where there is no style rule to wrap with (dart's _styleRule is
        // null even though `&` still resolves).
        let body_parents: &[String] = if name == "font-face" || self.at_root_excluding_style_rule {
            &[]
        } else {
            parents
        };
        let out_body = self.eval_at_body(stmts, body_parents)?;
        sink.push_at_rule(OutNode::AtRule {
            name: name.to_string(),
            prelude,
            body: out_body,
            has_block: true,
            lines,
        });
        Ok(())
    }

    /// Evaluate a plain CSS custom `@function`/`@mixin`: resolve the prelude
    /// and each body declaration (verbatim values keep their literal text;
    /// interpolated-property declarations evaluate as SassScript), then emit the
    /// whole construct verbatim as a generic at-rule.
    pub(super) fn eval_css_custom_at_rule(
        &mut self,
        name: &str,
        prelude: &[TplPiece],
        body: &[CssCustomItem],
        sink: &mut Sink<'_>,
    ) -> Result<(), Error> {
        let prelude = self.eval_template(prelude)?;
        let mut out_body: Vec<OutNode> = Vec::new();
        for item in body {
            let prop = self.eval_template(&item.property)?;
            match &item.value {
                CssCustomValue::Raw(tpl) => {
                    let raw = self.eval_template(tpl)?;
                    out_body.push(OutNode::Raw(format!("{prop}:{raw};")));
                }
                CssCustomValue::Script(expr) => {
                    let value = self.eval_expr(expr)?.to_css(self.compressed());
                    out_body.push(OutNode::Raw(format!("{prop}: {value};")));
                }
                // A nested property set on an interpolated property: each
                // child emits as `property-suffix: value`.
                CssCustomValue::Set(children) => {
                    for (suffix, expr) in children {
                        let sfx = self.eval_template(suffix)?;
                        let value = self.eval_expr(expr)?.to_css(self.compressed());
                        out_body.push(OutNode::Raw(format!("{prop}-{sfx}: {value};")));
                    }
                }
            }
        }
        sink.push_at_rule(OutNode::AtRule {
            name: name.to_string(),
            prelude,
            body: out_body,
            has_block: true,
            lines: SrcLines::default(),
        });
        Ok(())
    }

    /// Run an at-rule body, producing its output node list. When the at-rule
    /// is nested under a style rule, bare declarations are wrapped in the
    /// enclosing selectors; at the document root they emit directly.
    fn eval_at_body(&mut self, stmts: &[Stmt], parents: &[String]) -> Result<Vec<OutNode>, Error> {
        self.push_scope(false);
        let mut body: Vec<OutNode> = Vec::new();
        let result = if parents.is_empty() {
            let mut child = Sink::AtRoot(&mut body);
            self.exec(stmts, &[], &mut child)
        } else {
            let mut items: Vec<OutItem> = Vec::new();
            let mut nested: Vec<OutNode> = Vec::new();
            let mut flushed: Option<usize> = None;
            let at_depth = self.at_rule_ctx.len();
            let res = {
                let mut child = Sink::Rule {
                    selectors: parents,
                    linebreaks: &[],
                    // The wrap re-uses the enclosing selectors, so it has no
                    // source rule of its own: `file`/`start`/`end` stay 0 to keep
                    // the trailing-comment rule disabled. For SOURCE MAPS only it
                    // carries the enclosing rule's selector position in the
                    // map-override fields, so the bubbled parent selector maps
                    // back to the ORIGINAL rule's span (dart parity). No CSS effect.
                    lines: SrcLines {
                        map_file: self.cur_rule_lines.file,
                        map_line: self.cur_rule_lines.start,
                        start_col: self.cur_rule_lines.start_col,
                        ..SrcLines::default()
                    },
                    items: &mut items,
                    nested: &mut nested,
                    at_depth,
                    flushed: &mut flushed,
                    extend_base: usize::MAX,
                };
                let r = self.exec(stmts, parents, &mut child);
                if r.is_ok() {
                    child.flush_rule_block();
                }
                r
            };
            if res.is_ok() {
                body.extend(nested);
            }
            res
        };
        self.pop_scope();
        result?;
        Ok(body)
    }

    /// Evaluate `@media`: resolve the query list (SassScript inside feature
    /// values is evaluated), merge with any enclosing `@media`, run the body
    /// carrying enclosing selectors inside, then emit the at-rule (which bubbles
    /// to the document root). An empty body produces no output.
    pub(super) fn eval_media(
        &mut self,
        query: &MediaQueryList,
        body: &[Stmt],
        lines: SrcLines,
        parents: &[String],
        sink: &mut Sink<'_>,
    ) -> Result<(), Error> {
        // Without an enclosing style rule, a bare declaration directly inside a
        // media block is invalid (dart-sass: "expected \"{\".") — only rules and
        // at-rules may appear there. With a style rule, declarations belong to
        // its selector and are allowed.
        if parents.is_empty() {
            for stmt in body {
                if matches!(stmt, Stmt::Decl(_)) {
                    return Err(Error::unpositioned("expected \"{\"."));
                }
            }
        }

        let queries = self.resolve_media_queries(query)?;

        // Inside a keyframe block an at-rule nests verbatim: the frame is not
        // a style rule in dart-sass, so there is no bubbling/wrapping.
        if self.in_keyframes && sink.is_rule() {
            let prelude = serialize_media_queries(&queries, self.compressed());
            let out_body = self.eval_at_body(body, &[])?;
            sink.push_item(OutItem::NestedAtRule {
                name: "media".to_string(),
                prelude,
                items: at_body_to_items(out_body),
            });
            return Ok(());
        }

        // Merge with the enclosing media context (dart-sass `_mergeMediaQueries`).
        let merged = if self.media_queries.is_empty() {
            None
        } else {
            match merge_media_query_lists(&self.media_queries, &queries) {
                // Mutually exclusive everywhere — emit nothing.
                Some(m) if m.is_empty() => return Ok(()),
                other => other,
            }
        };

        // Children see the merged queries when mergeable, else just our own.
        let child_queries = merged.clone().unwrap_or_else(|| queries.clone());
        // The emitted node carries the merged queries (when mergeable) and
        // bubbles past the enclosing media; otherwise it stays nested.
        let bubble_out = merged.is_some();
        let node_queries = if bubble_out { &child_queries } else { &queries };
        let prelude = serialize_media_queries(node_queries, self.compressed());

        let enclosing = !self.media_queries.is_empty();
        let saved = std::mem::replace(&mut self.media_queries, child_queries);
        let saved_hoist = std::mem::take(&mut self.media_hoist);
        let own_depth = self.at_rule_ctx.len();
        self.at_rule_ctx.push(AtCtx::Media {
            prelude: prelude.clone(),
        });
        let out_body = self.eval_at_body(body, parents);
        self.at_rule_ctx.pop();
        self.media_queries = saved;
        let mut hoisted = std::mem::replace(&mut self.media_hoist, saved_hoist);
        let out_body = out_body?;

        // Split the body at the hoist markers nested mergeable media rules
        // left behind: each marker interleaves the bubbled rule at its source
        // position, slicing this rule's own children into segments around it
        // (dart-sass#453 keeps source order).
        let mut result: Vec<OutNode> = Vec::new();
        let mut segment: Vec<OutNode> = Vec::new();
        let mut hoist_iter = hoisted.drain(..);
        let flush = |segment: &mut Vec<OutNode>, result: &mut Vec<OutNode>, prelude: &str| {
            if !segment.is_empty() {
                result.push(OutNode::AtRule {
                    name: "media".to_string(),
                    prelude: prelude.to_string(),
                    body: std::mem::take(segment),
                    has_block: true,
                    lines,
                });
            }
        };
        for n in out_body {
            let at_root_target = match &n {
                OutNode::AtRootHoist { target } => Some(*target),
                _ => None,
            };
            if matches!(&n, OutNode::MediaHoist) {
                flush(&mut segment, &mut result, &prelude);
                if let Some(batch) = hoist_iter.next() {
                    result.extend(batch);
                }
            } else if let Some(t) = at_root_target {
                if t == own_depth + 1 {
                    // The batch grafts INTO this rule's own body (dart adds
                    // it to the existing node at its current end): it joins
                    // the current segment in place, splitting nothing.
                    if let Some(b) = self.at_root_hoist.pop_front() {
                        debug_assert_eq!(b.target, t);
                        segment.extend(b.nodes);
                    }
                } else if own_depth == 0 {
                    // A root-bound batch: split around it, placing it just
                    // after this rule's current segment at the root.
                    flush(&mut segment, &mut result, &prelude);
                    if let Some(b) = self.at_root_hoist.pop_front() {
                        debug_assert_eq!(b.target, t);
                        result.extend(b.nodes);
                        result.push(if b.group_end {
                            OutNode::GroupEnd
                        } else {
                            OutNode::AtRootPackTight
                        });
                    }
                } else {
                    // Bound further out: split and pass the marker outward.
                    flush(&mut segment, &mut result, &prelude);
                    result.push(n);
                }
            } else {
                segment.push(n);
            }
        }
        drop(hoist_iter);
        flush(&mut segment, &mut result, &prelude);
        if result.is_empty() {
            return Ok(());
        }

        // A mergeable rule nested in another media bubbles the whole batch
        // out through the enclosing rule (leaving a marker at this source
        // position); otherwise emit in place.
        if bubble_out && enclosing {
            sink.push_at_rule(OutNode::MediaHoist);
            self.media_hoist.push(result);
        } else {
            for n in result {
                sink.push_at_rule(n);
            }
        }
        Ok(())
    }

    /// Resolve a parsed media query list to its final string components,
    /// evaluating SassScript inside feature values.
    pub(super) fn resolve_media_queries(
        &mut self,
        list: &MediaQueryList,
    ) -> Result<Vec<ResolvedQuery>, Error> {
        let mut out = Vec::with_capacity(list.queries.len());
        for q in &list.queries {
            out.push(self.resolve_media_query(q)?);
        }
        // dart-sass re-parses the RESOLVED prelude text (CssMediaQuery
        // .parseList), so interpolation may span query boundaries
        // (`scr#{"een, pri"}nt` splits into two queries). Only a prelude that
        // actually contained interpolation needs the round-trip.
        if list.queries.iter().any(media_query_has_interp) {
            let text = serialize_media_queries(&out, self.compressed());
            return css_media_parse_list(&text);
        }
        Ok(out)
    }

    fn resolve_media_query(&mut self, q: &MediaQuery) -> Result<ResolvedQuery, Error> {
        match q {
            MediaQuery::Type {
                modifier,
                mtype,
                conditions,
            } => {
                let mtype = self.eval_template(mtype)?;
                let modifier = match modifier {
                    Some(t) => Some(self.eval_template(t)?),
                    None => None,
                };
                let conditions = self.resolve_conditions(conditions)?;
                Ok(ResolvedQuery {
                    modifier,
                    mtype: Some(mtype),
                    conditions,
                    conjunction_and: true,
                })
            }
            MediaQuery::Condition {
                conditions,
                conjunction,
            } => Ok(ResolvedQuery {
                modifier: None,
                mtype: None,
                conditions: self.resolve_conditions(conditions)?,
                conjunction_and: matches!(conjunction, Conjunction::And),
            }),
        }
    }

    fn resolve_conditions(&mut self, conds: &[MediaInParens]) -> Result<Vec<String>, Error> {
        let mut out = Vec::with_capacity(conds.len());
        for c in conds {
            out.push(self.serialize_media_in_parens(c)?);
        }
        Ok(out)
    }

    fn serialize_media_in_parens(&mut self, c: &MediaInParens) -> Result<String, Error> {
        match c {
            MediaInParens::Feature(f) => {
                let inner = self.serialize_media_feature(f)?;
                Ok(format!("({inner})"))
            }
            MediaInParens::Not(inner) => Ok(format!("not {}", self.serialize_media_in_parens(inner)?)),
            MediaInParens::Group {
                conditions,
                conjunction,
            } => {
                let parts = self.resolve_conditions(conditions)?;
                let sep = if matches!(conjunction, Conjunction::And) {
                    " and "
                } else {
                    " or "
                };
                Ok(format!("({})", parts.join(sep)))
            }
            MediaInParens::Interp(e) => {
                let v = self.eval_expr(e)?;
                interp_checked(&v)
            }
        }
    }

    fn serialize_media_feature(&mut self, f: &MediaFeature) -> Result<String, Error> {
        match f {
            MediaFeature::Decl { name, value } => {
                // Media-feature names and values serialize in interpolation
                // context: a quoted string unquotes (`("min-width:#{$w}")`
                // emits `(min-width:20px)`), numbers are unchanged.
                let n = interp_checked(&self.eval_expr(name)?)?;
                match value {
                    Some(v) => {
                        let val = interp_checked(&self.eval_expr(v)?)?;
                        Ok(format!("{n}: {val}"))
                    }
                    None => Ok(n),
                }
            }
            MediaFeature::Range {
                first,
                op1,
                second,
                rest,
            } => {
                let a = self.eval_expr(first)?.to_css(self.compressed());
                let b = self.eval_expr(second)?.to_css(self.compressed());
                let mut s = format!("{a} {op1} {b}");
                if let Some((op2, third)) = rest {
                    let c = self.eval_expr(third)?.to_css(self.compressed());
                    s.push_str(&format!(" {op2} {c}"));
                }
                Ok(s)
            }
        }
    }

    /// Evaluate `@supports <condition> { body }`: serialize the structured
    /// condition canonically, run the body (bubbling like any at-rule), and emit
    /// the node — skipping emission entirely when the body produces nothing
    /// (dart-sass drops an empty/invisible `@supports`).
    pub(super) fn eval_supports(
        &mut self,
        condition: &SupportsCondition,
        body: &[Stmt],
        lines: SrcLines,
        parents: &[String],
        sink: &mut Sink<'_>,
    ) -> Result<(), Error> {
        let prelude = self.serialize_supports_condition(condition)?;
        let own_depth = self.at_rule_ctx.len();
        self.at_rule_ctx.push(AtCtx::Supports {
            prelude: prelude.clone(),
        });
        let out_body = self.eval_at_body(body, parents);
        self.at_rule_ctx.pop();
        let out_body = out_body?;
        if out_body.is_empty() {
            return Ok(());
        }
        // Split around any escaping @at-root batches (markers), wrapping each
        // segment in its own @supports copy like dart's tree rebuild.
        let mut result: Vec<OutNode> = Vec::new();
        let mut segment: Vec<OutNode> = Vec::new();
        let flush = |segment: &mut Vec<OutNode>, result: &mut Vec<OutNode>| {
            if !segment.is_empty() {
                result.push(OutNode::AtRule {
                    name: "supports".to_string(),
                    prelude: prelude.clone(),
                    body: std::mem::take(segment),
                    has_block: true,
                    lines,
                });
            }
        };
        for n in out_body {
            let at_root_target = match &n {
                OutNode::AtRootHoist { target } => Some(*target),
                _ => None,
            };
            if let Some(t) = at_root_target {
                if t == own_depth + 1 {
                    // Grafts INTO this rule's own body: joins the current
                    // segment in place (dart appends to the existing node).
                    if let Some(b) = self.at_root_hoist.pop_front() {
                        segment.extend(b.nodes);
                    }
                } else if own_depth == 0 {
                    flush(&mut segment, &mut result);
                    if let Some(b) = self.at_root_hoist.pop_front() {
                        result.extend(b.nodes);
                        result.push(if b.group_end {
                            OutNode::GroupEnd
                        } else {
                            OutNode::AtRootPackTight
                        });
                    }
                } else {
                    flush(&mut segment, &mut result);
                    result.push(n);
                }
            } else {
                segment.push(n);
            }
        }
        flush(&mut segment, &mut result);
        for n in result {
            sink.push_at_rule(n);
        }
        Ok(())
    }

    /// Serialize a `@supports` condition to its canonical CSS string
    /// (dart-sass `_visitSupportsCondition`).
    pub(super) fn serialize_supports_condition(
        &mut self,
        condition: &SupportsCondition,
    ) -> Result<String, Error> {
        match condition {
            SupportsCondition::Operation { left, right, op } => {
                let l = self.parenthesize_supports(left, Some(*op))?;
                let r = self.parenthesize_supports(right, Some(*op))?;
                let word = if matches!(op, Conjunction::And) {
                    "and"
                } else {
                    "or"
                };
                Ok(format!("{l} {word} {r}"))
            }
            SupportsCondition::Negation(inner) => {
                Ok(format!("not {}", self.parenthesize_supports(inner, None)?))
            }
            SupportsCondition::Interpolation(expr) => interp_checked(&self.eval_expr(expr)?),
            SupportsCondition::Declaration { name, value, custom } => {
                // dart-sass evaluates BOTH the name and the value with
                // `_inSupportsDeclaration` set, so a calc in the name
                // (`(calc(0): a)`) is also kept unsimplified.
                let saved = self.in_supports_declaration;
                self.in_supports_declaration = true;
                let result = (|| {
                    let n = self.eval_expr(name)?.to_css(self.compressed());
                    let v = match value.as_ref() {
                        SupportsValue::Expr(e) => self.eval_expr(e)?.to_css(self.compressed()),
                        // A custom-property value is an unquoted string: resolve
                        // its interpolation, then apply unquoted-string
                        // serialization (`\n` -> space, post-newline spaces
                        // dropped), matching dart-sass `_visitUnquotedString`.
                        SupportsValue::Raw(tpl) => unquoted_string_css(&self.eval_template(tpl)?),
                    };
                    Ok::<_, Error>((n, v))
                })();
                self.in_supports_declaration = saved;
                let (n, v) = result?;
                let sep = if *custom { "" } else { " " };
                Ok(format!("({n}:{sep}{v})"))
            }
            SupportsCondition::Function { name, arguments } => {
                let n = self.eval_template(name)?;
                let args = self.eval_template(arguments)?;
                Ok(format!("{n}({args})"))
            }
            SupportsCondition::Anything(contents) => {
                let inner = self.eval_template(contents)?;
                Ok(format!("({inner})"))
            }
        }
    }

    /// Serialize a plain-CSS `@import` argument: the URL template followed by
    /// its canonical modifiers. Raw identifier/function runs join with single
    /// spaces; a `supports(<query>)` re-serializes its parsed condition (a
    /// declaration's own parens double as the call parens); the terminal media
    /// query list joins with `, ` when it continued a bare-identifier query.
    pub(super) fn serialize_css_import(
        &mut self,
        url: &[TplPiece],
        modifiers: &[ImportModifier],
    ) -> Result<String, Error> {
        let mut out = self.eval_template(url)?;
        for m in modifiers {
            match m {
                ImportModifier::Raw(tpl) => {
                    out.push(' ');
                    out.push_str(&self.eval_template(tpl)?);
                }
                ImportModifier::Supports {
                    condition,
                    declaration,
                } => {
                    out.push(' ');
                    // The condition is an *expression* part of the modifiers
                    // interpolation in dart-sass (a `SupportsExpression` whose
                    // value is an unquoted string), so its serialized text gets
                    // the unquoted-string newline collapse — unlike a Raw
                    // modifier, which is verbatim buffer text.
                    let inner = unquoted_string_css(&self.serialize_supports_condition(condition)?);
                    if *declaration {
                        out.push_str(&format!("supports{inner}"));
                    } else {
                        out.push_str(&format!("supports({inner})"));
                    }
                }
                ImportModifier::Media { list, comma_before } => {
                    out.push_str(if *comma_before { ", " } else { " " });
                    let queries = self.resolve_media_queries(list)?;
                    out.push_str(&serialize_media_queries(&queries, self.compressed()));
                }
            }
        }
        Ok(out)
    }

    /// dart-sass `_parenthesize`: wrap a sub-condition in parentheses when it is
    /// a negation, or an operation whose operator differs from the surrounding
    /// one (or there is no surrounding operator).
    fn parenthesize_supports(
        &mut self,
        condition: &SupportsCondition,
        operator: Option<Conjunction>,
    ) -> Result<String, Error> {
        let needs_parens = match condition {
            SupportsCondition::Negation(_) => true,
            SupportsCondition::Operation { op, .. } => match operator {
                None => true,
                Some(outer) => outer != *op,
            },
            _ => false,
        };
        let inner = self.serialize_supports_condition(condition)?;
        if needs_parens {
            Ok(format!("({inner})"))
        } else {
            Ok(inner)
        }
    }

    /// Evaluate `@keyframes`. The frame selectors are keyframe selectors, not
    /// CSS selectors: no `&`/parent resolution. We run the body with the parent
    /// context reset to root (empty parents), so frame blocks emit verbatim.
    /// The whole node bubbles to the document root like any other at-rule.
    pub(super) fn eval_keyframes(
        &mut self,
        name: &str,
        prelude: &[TplPiece],
        body: &[Stmt],
        lines: SrcLines,
        sink: &mut Sink<'_>,
    ) -> Result<(), Error> {
        // A style rule nested inside a keyframe block is invalid; each frame
        // (a top-level rule in the body) may only hold declarations.
        for stmt in body {
            if let Stmt::Rule(frame) = stmt {
                for inner in &frame.body {
                    if matches!(inner, Stmt::Rule(_)) {
                        return Err(Error::unpositioned(
                            "Style rules may not be used within keyframe blocks.",
                        ));
                    }
                }
            }
        }
        let prelude = self.eval_template(prelude)?;
        let saved_kf = std::mem::replace(&mut self.in_keyframes, true);
        let own_depth = self.at_rule_ctx.len();
        self.at_rule_ctx.push(AtCtx::Keyframes {
            name: name.to_string(),
            prelude: prelude.clone(),
        });
        let out_body = self.eval_at_body(body, &[]);
        self.at_rule_ctx.pop();
        self.in_keyframes = saved_kf;
        let out_body = out_body?;
        // Pull any escaping @at-root batches out (the keyframes shell stays,
        // even empty: `@keyframes a {}` + the hoisted rules after it).
        let mut shell: Vec<OutNode> = Vec::new();
        let mut after: Vec<OutNode> = Vec::new();
        for n in out_body {
            let at_root_target = match &n {
                OutNode::AtRootHoist { target } => Some(*target),
                _ => None,
            };
            if let Some(t) = at_root_target {
                if t == own_depth + 1 {
                    if let Some(b) = self.at_root_hoist.pop_front() {
                        shell.extend(b.nodes);
                    }
                } else if own_depth == 0 {
                    if let Some(b) = self.at_root_hoist.pop_front() {
                        after.extend(b.nodes);
                        after.push(if b.group_end {
                            OutNode::GroupEnd
                        } else {
                            OutNode::AtRootPackTight
                        });
                    }
                } else {
                    after.push(n);
                }
            } else {
                shell.push(n);
            }
        }
        sink.push_at_rule(OutNode::AtRule {
            name: name.to_string(),
            prelude,
            body: shell,
            has_block: true,
            lines,
        });
        for n in after {
            sink.push_at_rule(n);
        }
        Ok(())
    }

    /// Evaluate `@at-root`: hoist the body's output to the document root. The
    /// parent-selector context is KEPT — dart resolves `&` against the
    /// enclosing rule but disables the implicit parent join
    /// (`implicitParent: !_atRootExcludingStyleRule`), so `@at-root & {…}`
    /// re-emits the parent at the root while `@at-root .x {…}` stays `.x`.
    /// The optional query is accepted but not yet honoured (the common
    /// no-query case is supported).
    pub(super) fn eval_at_root(
        &mut self,
        query: Option<&[TplPiece]>,
        body: &[Stmt],
        parents: &[String],
        sink: &mut Sink<'_>,
    ) -> Result<(), Error> {
        let query_text = match query {
            Some(tpl) => Some(self.eval_template(tpl)?),
            None => None,
        };
        let q = AtRootQuery::parse(query_text.as_deref());
        // Which enclosing at-rule layers the query keeps vs excludes (dart
        // `AtRootQuery.excludes`). The topmost excluded layer is the graft
        // target (dart `_trimIncluded`): kept layers ABOVE it stay in place
        // (the batch re-enters inside the innermost of them), kept layers
        // BELOW it are copied around the hoisted body.
        let excluded: Vec<bool> = self
            .at_rule_ctx
            .iter()
            .map(|c| q.excludes_name(c.query_name()))
            .collect();
        let first_excluded = excluded.iter().position(|&e| e);
        let any_excluded_layer = first_excluded.is_some();
        // `(with: all)`-style queries that exclude nothing run in place.
        if !any_excluded_layer && !q.excludes_style_rules() {
            return self.exec(body, parents, sink);
        }

        self.push_scope(false);
        let saved = self.at_root_excluding_style_rule;
        self.at_root_excluding_style_rule = q.excludes_style_rules();
        // An excluded media layer also stops feeding the body's own @media
        // merging; an excluded keyframes layer drops the keyframe context.
        let saved_media = if q.excludes_name("media") {
            Some(std::mem::take(&mut self.media_queries))
        } else {
            None
        };
        let saved_kf = if q.excludes_name("keyframes") {
            Some(std::mem::replace(&mut self.in_keyframes, false))
        } else {
            None
        };
        let mut out: Vec<OutNode> = Vec::new();
        let res = {
            let mut child = Sink::AtRoot(&mut out);
            self.exec(body, parents, &mut child)
        };
        self.at_root_excluding_style_rule = saved;
        if let Some(m) = saved_media {
            self.media_queries = m;
        }
        if let Some(k) = saved_kf {
            self.in_keyframes = k;
        }
        self.pop_scope();
        res?;

        // A declaration inside `@at-root` that no style rule wraps lands at
        // the document root, which dart rejects ("Declarations may only be
        // used within style rules." — issue_1585's `@at-root { @content }`).
        if (q.excludes_style_rules() || parents.is_empty())
            && out.iter().any(|n| matches!(n, OutNode::AtDecl { .. }))
        {
            return Err(Error::unpositioned(
                "Declarations may only be used within style rules.",
            ));
        }

        // When the query KEEPS style rules, bare declarations re-wrap in the
        // enclosing selectors (dart's included CssStyleRule copy):
        // `a { @at-root (without: media) { b: c } }` emits `a { b: c }`.
        let out = if !q.excludes_style_rules() && !parents.is_empty() {
            let mut wrapped: Vec<OutNode> = Vec::new();
            let mut decls: Vec<OutItem> = Vec::new();
            let flush = |decls: &mut Vec<OutItem>, wrapped: &mut Vec<OutNode>| {
                if !decls.is_empty() {
                    wrapped.push(OutNode::plain_rule(
                        parents.to_vec(),
                        std::mem::take(decls),
                        SrcLines::default(),
                    ));
                }
            };
            for n in out {
                match n {
                    OutNode::AtDecl {
                        prop,
                        value,
                        important,
                        custom,
                        lines,
                    } => decls.push(OutItem::Decl {
                        prop,
                        value,
                        important,
                        custom,
                        lines,
                    }),
                    other => {
                        flush(&mut decls, &mut wrapped);
                        wrapped.push(other);
                    }
                }
            }
            flush(&mut decls, &mut wrapped);
            wrapped
        } else {
            out
        };

        // Hoisted root-level nodes separate with a blank line only after a
        // completed style rule (dart's isGroupEnd: `#inc {…}` → blank →
        // `@supports`, but `@supports {…}` → `@foo` packs tight).
        let mut spaced: Vec<OutNode> = Vec::new();
        let mut prev_was_rule = false;
        for node in out {
            if prev_was_rule {
                spaced.push(OutNode::Blank);
            }
            prev_was_rule = matches!(node, OutNode::Rule { .. });
            spaced.push(node);
        }
        if spaced.is_empty() {
            return Ok(());
        }
        if let Some(te) = first_excluded {
            // Escape the excluded at-rules: re-wrap the body in the kept
            // layers BELOW the graft target (innermost-last) and leave a
            // marker; each enclosing layer below the target splits around it
            // and passes it outward until the target layer consumes it.
            let mut batch = spaced;
            for (i, layer) in self.at_rule_ctx.iter().enumerate().rev() {
                if i > te && !excluded[i] {
                    batch = vec![layer.wrap(batch)];
                }
            }
            self.at_root_hoist.push_back(AtRootBatch {
                target: te,
                group_end: parents.is_empty(),
                nodes: batch,
            });
            sink.push_at_rule(OutNode::AtRootHoist { target: te });
        } else {
            // No layer escaped: the body stays inside the enclosing at-rules
            // (only the style-rule join was disabled), so emit in place.
            for node in spaced {
                sink.push_at_rule(node);
            }
        }
        Ok(())
    }
}
