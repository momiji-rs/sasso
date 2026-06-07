//! The evaluator: walks the AST, resolving variables, nesting (`&` and
//! the parent×child selector product), interpolation and arithmetic, and
//! flattens the result into a list of output rules.
//!
//! Like dart-sass (and unlike grass), a rule's own declarations are
//! gathered into a single block emitted *before* its nested rules bubble
//! out after it.

use std::collections::HashMap;
use std::rc::Rc;

use crate::ast::{
    BinOp, CallArg, Callable, Conjunction, CssCustomItem, CssCustomValue, Declaration, Expr, IfClause,
    IfCond, ImportArg, MediaFeature, MediaInParens, MediaQuery, MediaQueryList, ParamList, Rule, Stmt,
    Stylesheet, TplPiece, UnOp, VarDecl,
};
use crate::error::Error;
use crate::scanner::Pos;
use crate::value::{CalcNode, CalcOp, List, ListSep, Map, Number, SassStr, Value};
use crate::{Importer, OutputStyle};

/// A call's evaluated arguments, split into positional values and named
/// `(name, value)` keyword pairs (after splat expansion).
type EvaledArgs = (Vec<Value>, Vec<(String, Value)>);

/// A flattened output node.
pub(crate) enum OutNode {
    Rule {
        selectors: Vec<String>,
        items: Vec<OutItem>,
    },
    Comment(String),
    /// A verbatim line (e.g. a passed-through CSS `@import`).
    Raw(String),
    /// A blank-line separator between top-level groups (expanded only).
    Blank,
    /// An at-rule: `@name prelude { body }` (when `has_block`) or
    /// `@name prelude;` (when not). The body holds the bubbled-out child
    /// nodes; bare declarations appear as [`OutNode::AtDecl`].
    AtRule {
        name: String,
        prelude: String,
        body: Vec<OutNode>,
        has_block: bool,
    },
    /// A bare declaration emitted directly inside an at-rule body (e.g.
    /// `@font-face { font-family: x; }`).
    AtDecl {
        prop: String,
        value: String,
        important: bool,
    },
}

/// A media query resolved to its final string components, ready to serialize
/// and to merge with nested queries (dart-sass `CssMediaQuery`).
#[derive(Clone, PartialEq, Eq)]
struct ResolvedQuery {
    /// `not`/`only`, already lowercased; `None` for a condition-only query.
    modifier: Option<String>,
    /// The media type (e.g. `screen`); `None` for a condition-only query.
    mtype: Option<String>,
    /// Serialized condition strings (e.g. `(a)`, `not (b)`, `((a) or (b))`).
    conditions: Vec<String>,
    /// Whether the conditions are joined by `and` (true) or `or` (false).
    conjunction_and: bool,
}

/// The result of merging two media queries (dart-sass `MediaQueryMergeResult`).
enum MergeResult {
    /// Mutually exclusive — the merged query selects nothing.
    Empty,
    /// The merge can't be represented as a single query; keep them nested.
    Unrepresentable,
    /// A single merged query.
    Query(ResolvedQuery),
}

/// An item inside a rule block.
pub(crate) enum OutItem {
    Decl {
        prop: String,
        value: String,
        important: bool,
    },
    Comment(String),
}

/// Where evaluated statements deposit their output. At the top level each
/// statement forms its own blank-separated group; inside a style rule,
/// declarations join the rule's block and nested rules bubble out after it.
/// This is the seam that lets one block executor serve the top level, rule
/// bodies, and every nested-block construct (conditionals, loops, mixins).
enum Sink<'a> {
    Top(&'a mut Vec<OutNode>),
    Rule {
        items: &'a mut Vec<OutItem>,
        nested: &'a mut Vec<OutNode>,
    },
    /// The body of a top-level at-rule (no enclosing selector): bare
    /// declarations land here directly as [`OutNode::AtDecl`], interleaved
    /// in source order with bubbled child rules and nested at-rules.
    AtRoot(&'a mut Vec<OutNode>),
}

impl Sink<'_> {
    fn is_top(&self) -> bool {
        matches!(self, Sink::Top(_))
    }

    fn push_comment(&mut self, text: String) {
        match self {
            Sink::Top(out) => {
                let out = &mut **out;
                push_group(out, vec![OutNode::Comment(text)]);
            }
            Sink::Rule { items, .. } => items.push(OutItem::Comment(text)),
            Sink::AtRoot(body) => body.push(OutNode::Comment(text)),
        }
    }

    fn push_item(&mut self, item: OutItem) {
        match self {
            Sink::Rule { items, .. } => items.push(item),
            Sink::AtRoot(body) => match item {
                OutItem::Decl {
                    prop,
                    value,
                    important,
                } => body.push(OutNode::AtDecl {
                    prop,
                    value,
                    important,
                }),
                OutItem::Comment(text) => body.push(OutNode::Comment(text)),
            },
            Sink::Top(_) => {}
        }
    }

    /// Emit a produced style rule — its own declaration block (when
    /// non-empty) plus the rules that bubbled out of it.
    fn emit_style_rule(&mut self, block: Option<OutNode>, nested: Vec<OutNode>) {
        match self {
            Sink::Top(out) => {
                let mut group = Vec::new();
                if let Some(b) = block {
                    group.push(b);
                }
                group.extend(nested);
                let out = &mut **out;
                push_group(out, group);
            }
            Sink::Rule { nested: parent, .. } => {
                if let Some(b) = block {
                    parent.push(b);
                }
                parent.extend(nested);
            }
            Sink::AtRoot(body) => {
                if let Some(b) = block {
                    body.push(b);
                }
                body.extend(nested);
            }
        }
    }

    /// Deposit a produced at-rule (or `@at-root` output). At the top level it
    /// forms its own group; inside a style rule it joins the rules that bubble
    /// out to the document root; inside another at-rule's body it nests.
    fn push_at_rule(&mut self, node: OutNode) {
        match self {
            Sink::Top(out) => {
                let out = &mut **out;
                push_group(out, vec![node]);
            }
            Sink::Rule { nested, .. } => nested.push(node),
            Sink::AtRoot(body) => body.push(node),
        }
    }
}

/// Options visible to the evaluator (subset of the public `Options`).
pub(crate) struct EvalOptions<'a> {
    pub style: OutputStyle,
    pub importer: Option<&'a dyn Importer>,
}

pub(crate) struct Evaluator<'a> {
    scopes: Vec<HashMap<String, Value>>,
    options: EvalOptions<'a>,
    /// Import paths currently being loaded, deepest last. Re-entering one is a
    /// load cycle (dart-sass "This file is already being loaded."); a path that
    /// has finished loading may be imported again (`@import` re-evaluates).
    loading: Vec<String>,
    functions: HashMap<String, Rc<Callable>>,
    mixins: HashMap<String, Rc<Callable>>,
    /// Stack of `@content` blocks, one per active `@include`.
    content_stack: Vec<Option<Rc<Vec<Stmt>>>>,
    /// The resolved query list of the enclosing `@media` context (empty at the
    /// document root). Used to merge nested `@media` queries.
    media_queries: Vec<ResolvedQuery>,
}

impl<'a> Evaluator<'a> {
    pub(crate) fn new(options: EvalOptions<'a>) -> Self {
        Evaluator {
            scopes: vec![HashMap::new()],
            options,
            loading: Vec::new(),
            functions: HashMap::new(),
            mixins: HashMap::new(),
            content_stack: Vec::new(),
            media_queries: Vec::new(),
        }
    }

    pub(crate) fn eval_sheet(&mut self, sheet: &Stylesheet, out: &mut Vec<OutNode>) -> Result<(), Error> {
        let mut sink = Sink::Top(out);
        self.exec(&sheet.stmts, &[], &mut sink)
    }

    fn compressed(&self) -> bool {
        matches!(self.options.style, OutputStyle::Compressed)
    }

    // ---- scopes ------------------------------------------------------

    fn lookup(&self, name: &str) -> Option<&Value> {
        for scope in self.scopes.iter().rev() {
            if let Some(v) = scope.get(name) {
                return Some(v);
            }
        }
        None
    }

    fn assign(&mut self, name: &str, val: Value) {
        for scope in self.scopes.iter_mut().rev() {
            if scope.contains_key(name) {
                scope.insert(name.to_string(), val);
                return;
            }
        }
        if let Some(cur) = self.scopes.last_mut() {
            cur.insert(name.to_string(), val);
        }
    }

    fn apply_var(&mut self, v: &VarDecl) -> Result<(), Error> {
        let val = self.eval_expr(&v.value)?;
        if v.is_default {
            if let Some(existing) = self.lookup(&v.name) {
                if !matches!(existing, Value::Null) {
                    return Ok(());
                }
            }
        }
        if v.is_global {
            if let Some(g) = self.scopes.first_mut() {
                g.insert(v.name.clone(), val);
            }
        } else {
            self.assign(&v.name, val);
        }
        Ok(())
    }

    // ---- loop helpers ------------------------------------------------

    /// Set a variable in the innermost scope (loop variables live there,
    /// alongside the surroundings — flow control adds no scope).
    fn set_local(&mut self, name: &str, val: Value) {
        if let Some(sc) = self.scopes.last_mut() {
            sc.insert(name.to_string(), val);
        }
    }

    /// Restore (or clear) a loop variable to its value before the loop.
    fn restore_local(&mut self, name: &str, saved: Option<Value>) {
        if let Some(sc) = self.scopes.last_mut() {
            match saved {
                Some(v) => {
                    sc.insert(name.to_string(), v);
                }
                None => {
                    sc.remove(name);
                }
            }
        }
    }

    fn eval_index(&mut self, e: &Expr) -> Result<i64, Error> {
        match self.eval_expr(e)? {
            Value::Number(n) => Ok(n.value.round() as i64),
            other => Err(Error::unpositioned(format!(
                "{} is not a number.",
                other.type_name()
            ))),
        }
    }

    /// The values `@each` iterates: a list yields its items, `null` yields
    /// nothing, and any other value is iterated once.
    fn eval_each_items(&mut self, e: &Expr) -> Result<Vec<Value>, Error> {
        match self.eval_expr(e)? {
            Value::List(l) => Ok(l.items),
            // `@each` over a map yields each `key value` pair as a two-element
            // space list, so `@each $k, $v in $map` destructures correctly.
            Value::Map(m) => Ok(m
                .entries
                .into_iter()
                .map(|(k, v)| {
                    Value::List(List {
                        items: vec![k, v],
                        sep: ListSep::Space,
                        bracketed: false,
                    })
                })
                .collect()),
            Value::Null => Ok(Vec::new()),
            other => Ok(vec![other]),
        }
    }

    /// Bind `@each` variables to an item, destructuring a list across
    /// multiple variables (missing elements become `null`).
    fn bind_each(&mut self, vars: &[String], item: Value) {
        if vars.len() == 1 {
            self.set_local(&vars[0], item);
            return;
        }
        let elems: Vec<Value> = match item {
            Value::List(l) => l.items,
            other => vec![other],
        };
        for (i, v) in vars.iter().enumerate() {
            let val = elems.get(i).cloned().unwrap_or(Value::Null);
            self.set_local(v, val);
        }
    }

    fn restore_each(&mut self, vars: &[String], saved: &[Option<Value>]) {
        for (v, sv) in vars.iter().zip(saved) {
            self.restore_local(v, sv.clone());
        }
    }

    // ---- callables ---------------------------------------------------

    /// Evaluate call arguments and bind them to a parameter list, returning
    /// the call frame: positional args fill params in order, then keyword
    /// args by name, then declared defaults; extra positionals collect into
    /// a `$rest...` parameter or are an error.
    /// Evaluate a call's argument list into separate positional and keyword
    /// vectors, expanding any `...` splat (a list spreads into positional
    /// args, a map into keyword args). Duplicate keyword names (after
    /// hyphen/underscore normalization) are rejected, and a positional arg
    /// after a keyword arg is an error — matching dart-sass.
    fn eval_call_args(&mut self, args: &[CallArg]) -> Result<EvaledArgs, Error> {
        // Explicit positional args are gathered first; positionals spread from
        // a `...` splat are appended after them, so `f([1, 2]..., 3)` binds
        // `3` before `1, 2` (matching dart-sass's misplaced-rest behaviour).
        let mut explicit_pos = Vec::new();
        let mut splat_pos = Vec::new();
        let mut keyword: Vec<(String, Value)> = Vec::new();
        let mut seen_named = false;
        let push_named = |keyword: &mut Vec<(String, Value)>, name: String, v: Value| -> Result<(), Error> {
            let norm = normalize_arg_name(&name);
            if keyword.iter().any(|(n, _)| normalize_arg_name(n) == norm) {
                return Err(Error::unpositioned("Duplicate argument."));
            }
            keyword.push((name, v));
            Ok(())
        };
        for a in args {
            let v = self.eval_expr(&a.value)?;
            if a.splat {
                // A splat list spreads into positional args; a map spreads
                // into keyword args (string keys only). A single non-list/map
                // value acts as one positional arg; `null` spreads to nothing.
                match v {
                    Value::Map(m) => {
                        for (k, val) in m.entries {
                            let key = match &k {
                                Value::Str(s) => s.text.clone(),
                                other => {
                                    return Err(Error::unpositioned(format!(
                                        "{} is not a string in $args.",
                                        other.to_css(false)
                                    )))
                                }
                            };
                            push_named(&mut keyword, key, val)?;
                        }
                    }
                    Value::List(l) => splat_pos.extend(l.items),
                    Value::Null => {}
                    other => splat_pos.push(other),
                }
                continue;
            }
            match &a.name {
                Some(n) => {
                    push_named(&mut keyword, n.clone(), v)?;
                    seen_named = true;
                }
                None => {
                    // A plain positional arg may not follow a keyword arg.
                    if seen_named {
                        return Err(Error::unpositioned(
                            "Positional arguments must come before keyword arguments.",
                        ));
                    }
                    explicit_pos.push(v);
                }
            }
        }
        explicit_pos.extend(splat_pos);
        Ok((explicit_pos, keyword))
    }

    fn bind_args(
        &mut self,
        params: &ParamList,
        args: &[CallArg],
        name: &str,
    ) -> Result<HashMap<String, Value>, Error> {
        let (positional, keyword_vec) = self.eval_call_args(args)?;
        let mut keyword: HashMap<String, Value> = HashMap::new();
        for (n, v) in keyword_vec {
            keyword.insert(normalize_arg_name(&n), v);
        }
        let mut frame = HashMap::new();
        let mut pos_iter = positional.into_iter();
        for param in &params.params {
            let val = if let Some(v) = pos_iter.next() {
                v
            } else if let Some(v) = keyword.remove(&normalize_arg_name(&param.name)) {
                v
            } else if let Some(def) = &param.default {
                self.eval_expr(def)?
            } else {
                return Err(Error::unpositioned(format!(
                    "Missing argument ${} for {name}.",
                    param.name
                )));
            };
            frame.insert(param.name.clone(), val);
        }
        if let Some(rest) = &params.rest {
            let remaining: Vec<Value> = pos_iter.collect();
            frame.insert(
                rest.clone(),
                Value::List(List {
                    items: remaining,
                    sep: ListSep::Comma,
                    bracketed: false,
                }),
            );
        } else if pos_iter.next().is_some() {
            return Err(Error::unpositioned(format!(
                "{name} was passed too many arguments."
            )));
        }
        Ok(frame)
    }

    /// Call a user-defined `@function`, returning its `@return` value.
    fn call_function(&mut self, func: &Rc<Callable>, args: &[CallArg]) -> Result<Value, Error> {
        let frame = self.bind_args(&func.params, args, &func.name)?;
        self.scopes.push(frame);
        let result = self.run_fn_body(&func.body);
        self.scopes.pop();
        match result? {
            // A bare slash-division returned from a function collapses to
            // its number (dart-sass `withoutSlash`); slashes nested in a
            // returned list are preserved.
            Some(v) => Ok(v.without_slash()),
            None => Err(Error::unpositioned(format!(
                "Function {}() did not @return a value.",
                func.name
            ))),
        }
    }

    /// Run a function body, propagating the first `@return` (including from
    /// nested control flow). Functions emit no CSS, so a returned value
    /// short-circuits the whole call.
    fn run_fn_body(&mut self, stmts: &[Stmt]) -> Result<Option<Value>, Error> {
        for stmt in stmts {
            match stmt {
                Stmt::VarDecl(v) => self.apply_var(v)?,
                Stmt::Comment(_) => {}
                Stmt::Return(e) => return Ok(Some(self.eval_expr(e)?)),
                Stmt::FunctionDef(c) => {
                    self.functions.insert(c.name.clone(), Rc::clone(c));
                }
                Stmt::If(branches) => {
                    for branch in branches {
                        let take = match &branch.cond {
                            None => true,
                            Some(c) => self.eval_expr(c)?.is_truthy(),
                        };
                        if take {
                            if let Some(v) = self.run_fn_body(&branch.body)? {
                                return Ok(Some(v));
                            }
                            break;
                        }
                    }
                }
                Stmt::For {
                    var,
                    from,
                    to,
                    inclusive,
                    body,
                } => {
                    let start = self.eval_index(from)?;
                    let end = self.eval_index(to)?;
                    let saved = self.scopes.last().and_then(|sc| sc.get(var).cloned());
                    for i in for_indices(start, end, *inclusive) {
                        self.set_local(
                            var,
                            Value::Number(Number {
                                value: i as f64,
                                unit: String::new(),
                            }),
                        );
                        if let Some(v) = self.run_fn_body(body)? {
                            self.restore_local(var, saved);
                            return Ok(Some(v));
                        }
                    }
                    self.restore_local(var, saved);
                }
                Stmt::Each { vars, list, body } => {
                    let items = self.eval_each_items(list)?;
                    let saved: Vec<Option<Value>> = vars
                        .iter()
                        .map(|v| self.scopes.last().and_then(|sc| sc.get(v).cloned()))
                        .collect();
                    for item in items {
                        self.bind_each(vars, item);
                        if let Some(v) = self.run_fn_body(body)? {
                            self.restore_each(vars, &saved);
                            return Ok(Some(v));
                        }
                    }
                    self.restore_each(vars, &saved);
                }
                Stmt::While { cond, body } => {
                    let mut guard = 0u32;
                    while self.eval_expr(cond)?.is_truthy() {
                        if let Some(v) = self.run_fn_body(body)? {
                            return Ok(Some(v));
                        }
                        guard += 1;
                        if guard >= 100_000 {
                            return Err(Error::unpositioned("@while exceeded 100000 iterations"));
                        }
                    }
                }
                Stmt::Warn(e) => {
                    let v = self.eval_expr(e)?;
                    eprintln!("WARNING: {}", v.to_interp());
                }
                Stmt::Debug(e) => {
                    let v = self.eval_expr(e)?;
                    eprintln!("DEBUG: {}", v.to_interp());
                }
                Stmt::Error(e) => {
                    let v = self.eval_expr(e)?;
                    return Err(Error::unpositioned(v.to_interp()));
                }
                _ => {
                    return Err(Error::unpositioned(
                        "only variable assignments, control flow and @return are allowed in a function.",
                    ));
                }
            }
        }
        Ok(None)
    }

    /// Execute an `@include`: bind args into a call frame, make the content
    /// block available, and run the mixin body into the current sink.
    fn exec_include(
        &mut self,
        name: &str,
        args: &[CallArg],
        content: Option<Rc<Vec<Stmt>>>,
        parents: &[String],
        sink: &mut Sink<'_>,
    ) -> Result<(), Error> {
        let mixin = self
            .mixins
            .get(name)
            .cloned()
            .ok_or_else(|| Error::unpositioned(format!("Undefined mixin {name}.")))?;
        let frame = self.bind_args(&mixin.params, args, &mixin.name)?;
        self.scopes.push(frame);
        self.content_stack.push(content);
        let result = self.exec(&mixin.body, parents, sink);
        self.content_stack.pop();
        self.scopes.pop();
        result
    }

    // ---- statements --------------------------------------------------

    /// Execute a block of statements, routing each into `sink`. One executor
    /// serves the top level (each statement is its own group), rule bodies
    /// (declarations join the block, nested rules bubble out), and every
    /// nested-block construct that reuses it.
    fn exec(&mut self, stmts: &[Stmt], parents: &[String], sink: &mut Sink<'_>) -> Result<(), Error> {
        for stmt in stmts {
            match stmt {
                Stmt::VarDecl(v) => self.apply_var(v)?,
                Stmt::Comment(c) => sink.push_comment(c.clone()),
                Stmt::Decl(d) => {
                    if sink.is_top() {
                        return Err(Error::at("top-level declarations aren't allowed", d.pos));
                    }
                    if let Some(oi) = self.eval_decl(d)? {
                        sink.push_item(oi);
                    }
                }
                Stmt::Rule(r) => self.eval_style_rule(r, parents, sink)?,
                Stmt::If(branches) => {
                    // Evaluate conditions top to bottom; run the first match's
                    // body into the same sink. Flow control adds no scope, so
                    // its assignments are visible to the surroundings (Sass).
                    for branch in branches {
                        let take = match &branch.cond {
                            None => true,
                            Some(c) => self.eval_expr(c)?.is_truthy(),
                        };
                        if take {
                            self.exec(&branch.body, parents, sink)?;
                            break;
                        }
                    }
                }
                Stmt::For {
                    var,
                    from,
                    to,
                    inclusive,
                    body,
                } => {
                    let start = self.eval_index(from)?;
                    let end = self.eval_index(to)?;
                    let saved = self.scopes.last().and_then(|sc| sc.get(var).cloned());
                    for i in for_indices(start, end, *inclusive) {
                        self.set_local(
                            var,
                            Value::Number(Number {
                                value: i as f64,
                                unit: String::new(),
                            }),
                        );
                        self.exec(body, parents, sink)?;
                    }
                    self.restore_local(var, saved);
                }
                Stmt::Each { vars, list, body } => {
                    let items = self.eval_each_items(list)?;
                    let saved: Vec<Option<Value>> = vars
                        .iter()
                        .map(|v| self.scopes.last().and_then(|sc| sc.get(v).cloned()))
                        .collect();
                    for item in items {
                        self.bind_each(vars, item);
                        self.exec(body, parents, sink)?;
                    }
                    for (v, sv) in vars.iter().zip(saved) {
                        self.restore_local(v, sv);
                    }
                }
                Stmt::While { cond, body } => {
                    let mut guard = 0u32;
                    while self.eval_expr(cond)?.is_truthy() {
                        self.exec(body, parents, sink)?;
                        guard += 1;
                        if guard >= 100_000 {
                            return Err(Error::unpositioned("@while exceeded 100000 iterations"));
                        }
                    }
                }
                Stmt::FunctionDef(callable) => {
                    self.functions.insert(callable.name.clone(), Rc::clone(callable));
                }
                Stmt::MixinDef(callable) => {
                    self.mixins.insert(callable.name.clone(), Rc::clone(callable));
                }
                Stmt::Return(_) => {
                    return Err(Error::unpositioned("@return is only allowed inside a function."));
                }
                Stmt::Include { name, args, content } => {
                    self.exec_include(name, args, content.clone(), parents, sink)?;
                }
                Stmt::Content => {
                    if let Some(Some(block)) = self.content_stack.last().cloned() {
                        self.exec(&block, parents, sink)?;
                    }
                }
                Stmt::Import(args) => self.eval_imports(args, parents, sink)?,
                Stmt::AtRule { name, prelude, body } => {
                    self.eval_at_rule(name, prelude, body.as_deref(), parents, sink)?;
                }
                Stmt::CssCustomAtRule { name, prelude, body } => {
                    self.eval_css_custom_at_rule(name, prelude, body, sink)?;
                }
                Stmt::Media { query, body } => {
                    self.eval_media(query, body, parents, sink)?;
                }
                Stmt::AtRoot { query, body } => {
                    self.eval_at_root(query.as_deref(), body, sink)?;
                }
                Stmt::Keyframes { name, prelude, body } => {
                    self.eval_keyframes(name, prelude, body, sink)?;
                }
                Stmt::Warn(e) => {
                    let v = self.eval_expr(e)?;
                    eprintln!("WARNING: {}", v.to_interp());
                }
                Stmt::Debug(e) => {
                    let v = self.eval_expr(e)?;
                    eprintln!("DEBUG: {}", v.to_interp());
                }
                Stmt::Error(e) => {
                    let v = self.eval_expr(e)?;
                    return Err(Error::unpositioned(v.to_interp()));
                }
            }
        }
        Ok(())
    }

    /// Evaluate a style rule: resolve its selector against `parents`, run its
    /// body into a fresh rule sink, then hand the produced block and the
    /// rules that bubbled out of it to the enclosing `sink`.
    fn eval_style_rule(&mut self, rule: &Rule, parents: &[String], sink: &mut Sink<'_>) -> Result<(), Error> {
        let sel_str = self.eval_template(&rule.selector)?;
        let current = resolve_selectors(&sel_str, parents);
        self.scopes.push(HashMap::new());
        let mut items: Vec<OutItem> = Vec::new();
        let mut nested: Vec<OutNode> = Vec::new();
        {
            let mut child = Sink::Rule {
                items: &mut items,
                nested: &mut nested,
            };
            self.exec(&rule.body, &current, &mut child)?;
        }
        self.scopes.pop();
        let block = if items.is_empty() {
            None
        } else {
            Some(OutNode::Rule {
                selectors: current,
                items,
            })
        };
        sink.emit_style_rule(block, nested);
        Ok(())
    }

    /// Evaluate a generic at-rule. The prelude template is resolved to a
    /// string; the body (when present) is executed so that nested rules carry
    /// the enclosing selectors INSIDE the at-rule, and the whole node hoists to
    /// the document root (bubbling).
    fn eval_at_rule(
        &mut self,
        name: &str,
        prelude: &[TplPiece],
        body: Option<&[Stmt]>,
        parents: &[String],
        sink: &mut Sink<'_>,
    ) -> Result<(), Error> {
        let prelude = self.eval_template(prelude)?;
        // dart-sass strips a leading `@charset "utf-8";` entirely.
        if name == "charset" && body.is_none() {
            return Ok(());
        }
        let Some(stmts) = body else {
            sink.push_at_rule(OutNode::AtRule {
                name: name.to_string(),
                prelude,
                body: Vec::new(),
                has_block: false,
            });
            return Ok(());
        };
        let out_body = self.eval_at_body(stmts, parents)?;
        sink.push_at_rule(OutNode::AtRule {
            name: name.to_string(),
            prelude,
            body: out_body,
            has_block: true,
        });
        Ok(())
    }

    /// Evaluate a plain CSS custom `@function`/`@mixin`: resolve the prelude
    /// and each body declaration (verbatim values keep their literal text;
    /// interpolated-property declarations evaluate as SassScript), then emit the
    /// whole construct verbatim as a generic at-rule.
    fn eval_css_custom_at_rule(
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
            let line = match &item.value {
                CssCustomValue::Raw(tpl) => {
                    let raw = self.eval_template(tpl)?;
                    format!("{prop}:{raw};")
                }
                CssCustomValue::Script(expr) => {
                    let value = self.eval_expr(expr)?.to_css(self.compressed());
                    format!("{prop}: {value};")
                }
            };
            out_body.push(OutNode::Raw(line));
        }
        sink.push_at_rule(OutNode::AtRule {
            name: name.to_string(),
            prelude,
            body: out_body,
            has_block: true,
        });
        Ok(())
    }

    /// Run an at-rule body, producing its output node list. When the at-rule
    /// is nested under a style rule, bare declarations are wrapped in the
    /// enclosing selectors; at the document root they emit directly.
    fn eval_at_body(&mut self, stmts: &[Stmt], parents: &[String]) -> Result<Vec<OutNode>, Error> {
        self.scopes.push(HashMap::new());
        let mut body: Vec<OutNode> = Vec::new();
        let result = if parents.is_empty() {
            let mut child = Sink::AtRoot(&mut body);
            self.exec(stmts, &[], &mut child)
        } else {
            let mut items: Vec<OutItem> = Vec::new();
            let mut nested: Vec<OutNode> = Vec::new();
            let res = {
                let mut child = Sink::Rule {
                    items: &mut items,
                    nested: &mut nested,
                };
                self.exec(stmts, parents, &mut child)
            };
            if res.is_ok() {
                if !items.is_empty() {
                    body.push(OutNode::Rule {
                        selectors: parents.to_vec(),
                        items,
                    });
                }
                body.extend(nested);
            }
            res
        };
        self.scopes.pop();
        result?;
        Ok(body)
    }

    /// Evaluate `@media`: resolve the query list (SassScript inside feature
    /// values is evaluated), merge with any enclosing `@media`, run the body
    /// carrying enclosing selectors inside, then emit the at-rule (which bubbles
    /// to the document root). An empty body produces no output.
    fn eval_media(
        &mut self,
        query: &MediaQueryList,
        body: &[Stmt],
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
        let prelude = serialize_media_queries(node_queries);

        let saved = std::mem::replace(&mut self.media_queries, child_queries);
        let out_body = self.eval_at_body(body, parents);
        self.media_queries = saved;
        let out_body = out_body?;

        // An empty body produces no output.
        if out_body.is_empty() {
            return Ok(());
        }

        sink.push_at_rule(OutNode::AtRule {
            name: "media".to_string(),
            prelude,
            body: out_body,
            has_block: true,
        });
        Ok(())
    }

    /// Resolve a parsed media query list to its final string components,
    /// evaluating SassScript inside feature values.
    fn resolve_media_queries(&mut self, list: &MediaQueryList) -> Result<Vec<ResolvedQuery>, Error> {
        let mut out = Vec::with_capacity(list.queries.len());
        for q in &list.queries {
            out.push(self.resolve_media_query(q)?);
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
                let conditions = self.resolve_conditions(conditions)?;
                Ok(ResolvedQuery {
                    modifier: modifier.clone(),
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
                Ok(v.to_interp())
            }
        }
    }

    fn serialize_media_feature(&mut self, f: &MediaFeature) -> Result<String, Error> {
        match f {
            MediaFeature::Decl { name, value } => {
                let n = self.eval_expr(name)?.to_css(self.compressed());
                match value {
                    Some(v) => {
                        let val = self.eval_expr(v)?.to_css(self.compressed());
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

    /// Evaluate `@keyframes`. The frame selectors are keyframe selectors, not
    /// CSS selectors: no `&`/parent resolution. We run the body with the parent
    /// context reset to root (empty parents), so frame blocks emit verbatim.
    /// The whole node bubbles to the document root like any other at-rule.
    fn eval_keyframes(
        &mut self,
        name: &str,
        prelude: &[TplPiece],
        body: &[Stmt],
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
        let out_body = self.eval_at_body(body, &[])?;
        sink.push_at_rule(OutNode::AtRule {
            name: name.to_string(),
            prelude,
            body: out_body,
            has_block: true,
        });
        Ok(())
    }

    /// Evaluate `@at-root`: run the body with the parent-selector context reset
    /// to the document root, then hoist its output. The optional query is
    /// accepted but not yet honoured (the common no-query case is supported).
    fn eval_at_root(
        &mut self,
        _query: Option<&[TplPiece]>,
        body: &[Stmt],
        sink: &mut Sink<'_>,
    ) -> Result<(), Error> {
        self.scopes.push(HashMap::new());
        let mut out: Vec<OutNode> = Vec::new();
        let res = {
            let mut child = Sink::AtRoot(&mut out);
            self.exec(body, &[], &mut child)
        };
        self.scopes.pop();
        res?;
        for node in out {
            sink.push_at_rule(node);
        }
        Ok(())
    }

    fn eval_decl(&mut self, d: &Declaration) -> Result<Option<OutItem>, Error> {
        let prop = self.eval_template(&d.property)?.trim().to_string();
        let value = self.eval_expr(&d.value)?;
        if matches!(value, Value::Null) {
            return Ok(None);
        }
        // A map is not a valid CSS value (even when nested inside a list).
        if let Some(m) = find_map(&value) {
            return Err(Error::at(
                format!("{} isn't a valid CSS value.", m.to_css(false)),
                d.pos,
            ));
        }
        let vstr = value.to_css(self.compressed());
        Ok(Some(OutItem::Decl {
            prop,
            value: vstr,
            important: d.important,
        }))
    }

    /// Evaluate an `@import` statement into `sink`. Sass arguments are parsed
    /// and inlined under the current `parents` (so a nested `@import` bubbles
    /// like an inline block); plain CSS arguments are emitted verbatim as
    /// `@import …;` in source order.
    fn eval_imports(
        &mut self,
        args: &[ImportArg],
        parents: &[String],
        sink: &mut Sink<'_>,
    ) -> Result<(), Error> {
        let importer = self.options.importer;
        for arg in args {
            match arg {
                ImportArg::Css(tpl) => {
                    let text = self.eval_template(tpl)?;
                    sink.push_at_rule(OutNode::Raw(format!("@import {text};")));
                }
                ImportArg::Sass(path) => {
                    if is_css_import(path) {
                        sink.push_at_rule(OutNode::Raw(format!("@import \"{path}\";")));
                        continue;
                    }
                    match importer.and_then(|imp| imp.resolve(path)) {
                        Some(src) => {
                            if self.loading.iter().any(|p| p == path) {
                                return Err(Error::unpositioned("This file is already being loaded."));
                            }
                            let sheet = crate::parser::parse(&src)?;
                            self.loading.push(path.clone());
                            let result = self.exec(&sheet.stmts, parents, sink);
                            self.loading.pop();
                            result?;
                        }
                        None => {
                            return Err(Error::unpositioned(format!(
                                "Can't find stylesheet to import: {path}"
                            )));
                        }
                    }
                }
            }
        }
        Ok(())
    }

    // ---- templates & expressions ------------------------------------

    fn eval_template(&mut self, pieces: &[TplPiece]) -> Result<String, Error> {
        let mut s = String::new();
        for piece in pieces {
            match piece {
                TplPiece::Lit(t) => s.push_str(t),
                TplPiece::Interp(e) => {
                    let v = self.eval_expr(e)?;
                    s.push_str(&v.to_interp());
                }
            }
        }
        Ok(s)
    }

    fn eval_expr(&mut self, expr: &Expr) -> Result<Value, Error> {
        match expr {
            Expr::Number(v, unit) => Ok(Value::Number(Number {
                value: *v,
                unit: unit.clone(),
            })),
            Expr::Color(c) => Ok(Value::Color(c.clone())),
            Expr::Bool(b) => Ok(Value::Bool(*b)),
            Expr::Null => Ok(Value::Null),
            // Reading a variable drops a bare slash-division's spelling
            // (dart-sass `withoutSlash`): `$x: 1/2; a {b: $x}` is `0.5`.
            // Slashes nested inside a stored list are preserved.
            Expr::Var(name) => match self.lookup(name) {
                Some(v) => Ok(v.clone().without_slash()),
                None => Err(Error::unpositioned(format!("Undefined variable ${name}."))),
            },
            Expr::QuotedString(pieces) => {
                let text = self.eval_template(pieces)?;
                Ok(Value::Str(SassStr { text, quoted: true }))
            }
            Expr::Ident(pieces) => {
                let text = self.eval_template(pieces)?;
                Ok(Value::Str(SassStr { text, quoted: false }))
            }
            Expr::Interp(inner) => {
                let v = self.eval_expr(inner)?;
                Ok(Value::Str(SassStr {
                    text: v.to_interp(),
                    quoted: false,
                }))
            }
            Expr::ModernIf(clauses) => self.eval_modern_if(clauses),
            // Parentheses force the deprecated slash to perform real
            // division: `(1/2)` is `0.5`, not the slash value `1/2`.
            Expr::Paren(inner) => Ok(self.eval_expr(inner)?.without_slash()),
            Expr::List {
                items,
                sep,
                bracketed,
            } => {
                let mut vals = Vec::with_capacity(items.len());
                for it in items {
                    vals.push(self.eval_expr(it)?);
                }
                Ok(Value::List(List {
                    items: vals,
                    sep: *sep,
                    bracketed: *bracketed,
                }))
            }
            Expr::Map(entries) => {
                let mut map = Map { entries: Vec::new() };
                for (k, v) in entries {
                    let key = self.eval_expr(k)?.without_slash();
                    let val = self.eval_expr(v)?;
                    // A duplicate literal key is an error in dart-sass.
                    if map.get(&key).is_some() {
                        return Err(Error::unpositioned("Duplicate key."));
                    }
                    map.insert(key, val);
                }
                Ok(Value::Map(map))
            }
            Expr::Unary { op, operand } => {
                let v = self.eval_expr(operand)?.without_slash();
                match op {
                    UnOp::Neg => match v {
                        Value::Number(n) => Ok(Value::Number(Number {
                            value: -n.value,
                            unit: n.unit,
                        })),
                        other => Err(Error::unpositioned(format!(
                            "-{} is not a number",
                            other.type_name()
                        ))),
                    },
                    UnOp::Not => Ok(Value::Bool(!v.is_truthy())),
                }
            }
            Expr::Binary { op, lhs, rhs, pos } => {
                // `and`/`or` short-circuit and yield a value, so the
                // right operand is only evaluated when needed.
                let l = self.eval_expr(lhs)?;
                match op {
                    BinOp::And => {
                        if l.is_truthy() {
                            self.eval_expr(rhs)
                        } else {
                            Ok(l)
                        }
                    }
                    BinOp::Or => {
                        if l.is_truthy() {
                            Ok(l)
                        } else {
                            self.eval_expr(rhs)
                        }
                    }
                    _ => {
                        let r = self.eval_expr(rhs)?;
                        eval_binary(*op, l.without_slash(), r.without_slash(), *pos)
                    }
                }
            }
            Expr::Div { lhs, rhs, slash, pos } => {
                let l = self.eval_expr(lhs)?;
                let r = self.eval_expr(rhs)?;
                eval_div(l, r, *slash, *pos)
            }
            Expr::Calc { inner, .. } => {
                let node = self.eval_calc(inner)?;
                // A calculation that reduces to a single finite number unwraps
                // to it; a non-finite result (infinity/NaN) stays a
                // calculation so it serializes as `calc(infinity)` etc., and
                // anything still containing an operation stays a calculation.
                match node {
                    CalcNode::Number(n) if n.value.is_finite() => Ok(Value::Number(n)),
                    // A bare unitless non-finite result canonicalizes to its
                    // constant spelling (`infinity`/`-infinity`/`NaN`). This is
                    // also the form the color builtins inspect for degenerate
                    // `calc()` channels (`rgb(calc(infinity), …)`).
                    CalcNode::Number(n) if n.unit.is_empty() => {
                        let spelling = if n.value.is_nan() {
                            "NaN"
                        } else if n.value > 0.0 {
                            "infinity"
                        } else {
                            "-infinity"
                        };
                        Ok(Value::Calc(CalcNode::Str(spelling.to_string())))
                    }
                    // `calc()` wrapping a single already-complete calculation
                    // (`calc(min(1%, 2px))`, `calc(clamp(…))`, etc.) is
                    // redundant: dart-sass drops the outer `calc()` and emits
                    // the inner calculation directly. (A non-calculation leaf
                    // such as `calc(var(--x))` keeps its wrapper.)
                    CalcNode::Str(s) if is_complete_calculation(&s) => Ok(Value::Str(SassStr {
                        text: s,
                        quoted: false,
                    })),
                    other => Ok(Value::Calc(other)),
                }
            }
            Expr::Func { name, args, pos } => {
                // if() is lazy: only the selected branch is evaluated.
                if name == "if" {
                    return self.eval_if_function(args, *pos);
                }
                // User-defined @function takes precedence over builtins.
                if let Some(func) = self.functions.get(name).cloned() {
                    return self.call_function(&func, args);
                }
                // The pure CSS-calculation functions are parsed as
                // calculations, which cannot take a `...` rest argument.
                if is_calc_function(name) && args.iter().any(|a| a.splat) {
                    return Err(Error::at("Rest arguments can't be used with calculations.", *pos));
                }
                // Evaluate args, expanding any `...` splat into positional /
                // keyword arguments.
                let (mut pos_args, mut named) = self.eval_call_args(args)?;
                // A bare slash-division argument collapses to its number when
                // passed to a real Sass function (dart-sass `withoutSlash`);
                // plain CSS functions (`foo(1/2)`) keep the slash verbatim.
                if crate::builtins::is_builtin(name) {
                    for v in &mut pos_args {
                        *v = std::mem::replace(v, Value::Null).without_slash();
                    }
                    for (_, v) in &mut named {
                        *v = std::mem::replace(v, Value::Null).without_slash();
                    }
                }
                crate::builtins::call(name, &pos_args, &named, *pos)
            }
        }
    }

    /// The lazy `if($condition, $if-true, $if-false)` function: evaluates
    /// the condition, then only the selected branch.
    fn eval_if_function(&mut self, args: &[CallArg], pos: Pos) -> Result<Value, Error> {
        let mut by_pos: Vec<&Expr> = Vec::new();
        let mut cond = None;
        let mut t_val = None;
        let mut f_val = None;
        for a in args {
            match a.name.as_deref() {
                Some("condition") => cond = Some(&a.value),
                Some("if-true") => t_val = Some(&a.value),
                Some("if-false") => f_val = Some(&a.value),
                Some(other) => {
                    return Err(Error::at(format!("if() has no argument named ${other}."), pos));
                }
                None => by_pos.push(&a.value),
            }
        }
        let cond = cond.or_else(|| by_pos.first().copied());
        let t_val = t_val.or_else(|| by_pos.get(1).copied());
        let f_val = f_val.or_else(|| by_pos.get(2).copied());
        match (cond, t_val, f_val) {
            (Some(c), Some(t), Some(f)) => {
                // if() is a function boundary: a bare slash-division branch
                // collapses to its number (dart-sass `withoutSlash`).
                let branch = if self.eval_expr(c)?.is_truthy() { t } else { f };
                Ok(self.eval_expr(branch)?.without_slash())
            }
            _ => Err(Error::at(
                "if() requires arguments $condition, $if-true, $if-false.",
                pos,
            )),
        }
    }

    /// Evaluate a modern CSS `if()`: a `;`-separated list of clauses, each
    /// `<condition>: <value>` (or `else: <value>`). Conditions mix evaluated
    /// `sass(<expr>)` with non-evaluable `css(...)` / arbitrary substitution
    /// pieces. If every reachable condition resolves statically, the matching
    /// value is returned; otherwise the whole `if()` is re-serialized
    /// verbatim (with statically-true/false conditions folded away) as an
    /// unquoted string.
    fn eval_modern_if(&mut self, clauses: &[IfClause]) -> Result<Value, Error> {
        let mut verbatim: Option<Vec<String>> = None;
        for clause in clauses {
            // The `else` clause has no condition: it always matches.
            let result = match &clause.condition {
                None => CondEval::Bool(true),
                Some(cond) => self.eval_if_cond(cond)?,
            };
            match (&mut verbatim, result) {
                // Not yet verbatim: a static-true (or `else`) clause wins.
                (None, CondEval::Bool(true)) => {
                    return Ok(self.eval_expr(&clause.value)?.without_slash());
                }
                // Not yet verbatim: a static-false clause is skipped.
                (None, CondEval::Bool(false)) => {}
                // First non-evaluable condition: enter verbatim mode.
                (None, CondEval::Css(rc)) => {
                    let value = self.eval_if_value(&clause.value)?;
                    verbatim = Some(vec![format!("{}: {}", rc.to_css(), value)]);
                }
                // Already verbatim: fold each remaining clause.
                (Some(out), CondEval::Bool(true)) => {
                    let value = self.eval_if_value(&clause.value)?;
                    out.push(format!("else: {value}"));
                }
                (Some(_), CondEval::Bool(false)) => {}
                (Some(out), CondEval::Css(rc)) => {
                    let value = self.eval_if_value(&clause.value)?;
                    out.push(format!("{}: {}", rc.to_css(), value));
                }
            }
        }
        match verbatim {
            Some(parts) => Ok(Value::Str(SassStr {
                text: format!("if({})", parts.join("; ")),
                quoted: false,
            })),
            // No clause matched and no `else`: the modern `if()` is null.
            None => Ok(Value::Null),
        }
    }

    /// Evaluate an `if()` clause value. dart-sass serializes the value in a
    /// parenthesized-expression context, so lists are wrapped in parens and
    /// a bare slash-division collapses to its number.
    fn eval_if_value(&mut self, expr: &Expr) -> Result<String, Error> {
        let v = self.eval_expr(expr)?.without_slash();
        Ok(serialize_if_value(&v))
    }

    /// Evaluate a modern `if()` condition into a tri-state result: a static
    /// boolean (from `sass(...)` atoms) or a residual non-evaluable CSS
    /// condition that must be re-serialized verbatim.
    fn eval_if_cond(&mut self, cond: &IfCond) -> Result<CondEval, Error> {
        match cond {
            IfCond::Sass(expr) => Ok(CondEval::Bool(self.eval_expr(expr)?.is_truthy())),
            IfCond::Raw { pieces, .. } => {
                let text = self.eval_template(pieces)?;
                Ok(CondEval::Css(RCond::Css(text)))
            }
            IfCond::Not(inner) => match self.eval_if_cond(inner)? {
                CondEval::Bool(b) => Ok(CondEval::Bool(!b)),
                CondEval::Css(rc) => Ok(CondEval::Css(RCond::Not(Box::new(rc)))),
            },
            IfCond::Paren(inner) => match self.eval_if_cond(inner)? {
                CondEval::Bool(b) => Ok(CondEval::Bool(b)),
                CondEval::Css(rc) => Ok(CondEval::Css(RCond::Paren(Box::new(rc)))),
            },
            IfCond::And(items) => {
                let mut residuals: Vec<RCond> = Vec::new();
                for item in items {
                    match self.eval_if_cond(item)? {
                        // A statically-false operand makes the whole `and`
                        // false and short-circuits the rest.
                        CondEval::Bool(false) => return Ok(CondEval::Bool(false)),
                        // A statically-true operand drops out of the `and`.
                        CondEval::Bool(true) => {}
                        CondEval::Css(rc) => residuals.push(rc),
                    }
                }
                Ok(combine_residuals(residuals, true))
            }
            IfCond::Or(items) => {
                let mut residuals: Vec<RCond> = Vec::new();
                for item in items {
                    match self.eval_if_cond(item)? {
                        // A statically-true operand makes the whole `or`
                        // true and short-circuits the rest.
                        CondEval::Bool(true) => return Ok(CondEval::Bool(true)),
                        // A statically-false operand drops out of the `or`.
                        CondEval::Bool(false) => {}
                        CondEval::Css(rc) => residuals.push(rc),
                    }
                }
                Ok(combine_residuals(residuals, false))
            }
        }
    }

    /// Evaluate the interior of a `calc()` into a simplified node tree.
    /// Numeric `number <op> number` subtrees with compatible units fold;
    /// everything else (variables, interpolations, incompatible units) is
    /// preserved for canonical serialization, mirroring dart-sass's
    /// "only simplify number+number" rule.
    fn eval_calc(&mut self, expr: &Expr) -> Result<CalcNode, Error> {
        match expr {
            Expr::Binary { op, lhs, rhs, pos } => {
                let calc_op = match op {
                    BinOp::Add => CalcOp::Add,
                    BinOp::Sub => CalcOp::Sub,
                    BinOp::Mul => CalcOp::Mul,
                    _ => {
                        // Non-arithmetic operators are not valid in calc.
                        let v = self.eval_expr(expr)?;
                        return Ok(value_to_calc_node(v));
                    }
                };
                let l = self.eval_calc(lhs)?;
                let r = self.eval_calc(rhs)?;
                fold_calc(calc_op, l, r, *pos)
            }
            Expr::Div { lhs, rhs, pos, .. } => {
                let l = self.eval_calc(lhs)?;
                let r = self.eval_calc(rhs)?;
                fold_calc(CalcOp::Div, l, r, *pos)
            }
            Expr::Unary {
                op: UnOp::Neg,
                operand,
            } => {
                let node = self.eval_calc(operand)?;
                match node {
                    CalcNode::Number(n) => Ok(CalcNode::Number(Number {
                        value: -n.value,
                        unit: n.unit,
                    })),
                    other => Ok(CalcNode::Op {
                        op: CalcOp::Mul,
                        left: Box::new(CalcNode::Number(Number {
                            value: -1.0,
                            unit: String::new(),
                        })),
                        right: Box::new(other),
                    }),
                }
            }
            // Parentheses around a single opaque operand (a `var()`,
            // identifier, or interpolation) are preserved verbatim; around a
            // number or an operation they are redundant and dropped (operator
            // precedence reintroduces them where needed).
            Expr::Paren(inner) => {
                let node = self.eval_calc(inner)?;
                match node {
                    CalcNode::Str(s) => Ok(CalcNode::Str(format!("({s})"))),
                    other => Ok(other),
                }
            }
            // A nested calc() flattens into the surrounding calculation.
            Expr::Calc { inner, .. } => self.eval_calc(inner),
            // Any leaf (number, var(), interpolation, ident) evaluates to a
            // value and becomes a calc operand.
            other => {
                let v = self.eval_expr(other)?;
                // A map is not a valid calculation operand.
                if let Value::Map(m) = &v {
                    return Err(Error::unpositioned(format!(
                        "Value {} can't be used in a calculation.",
                        m.to_css(false)
                    )));
                }
                // The calc constants `pi`/`e`/`infinity`/`-infinity`/`nan`
                // (case-insensitive) resolve to their numeric values inside a
                // calculation, so `calc(infinity * 2)` folds to `calc(infinity)`
                // and `calc(NaN)` canonicalizes its spelling.
                if let Value::Str(s) = &v {
                    if !s.quoted {
                        if let Some(value) = calc_constant(&s.text) {
                            return Ok(CalcNode::Number(Number {
                                value,
                                unit: String::new(),
                            }));
                        }
                    }
                }
                Ok(value_to_calc_node(v))
            }
        }
    }
}

/// Resolve a calc() numeric constant from its (unquoted) ident spelling,
/// case-insensitively: `pi`, `e`, `infinity`/`-infinity`, and `nan`. Returns
/// `None` for any other identifier, which is then kept verbatim.
fn calc_constant(text: &str) -> Option<f64> {
    match text.to_ascii_lowercase().as_str() {
        "pi" => Some(std::f64::consts::PI),
        "e" => Some(std::f64::consts::E),
        "infinity" => Some(f64::INFINITY),
        "-infinity" => Some(f64::NEG_INFINITY),
        "nan" => Some(f64::NAN),
        _ => None,
    }
}

/// Whether `s` is exactly one complete CSS-calculation function call —
/// `name(...)` for a recognized calculation function, with the closing paren
/// at the very end (balanced, nothing trailing). Used so that a `calc()`
/// wrapping a single already-complete calculation (`calc(min(1%, 2px))`)
/// drops its redundant outer `calc()`, matching dart-sass. A non-calculation
/// leaf (`var(--x)`, an unknown function) keeps its wrapper.
fn is_complete_calculation(s: &str) -> bool {
    let s = s.trim();
    let Some(open) = s.find('(') else { return false };
    if !s.ends_with(')') {
        return false;
    }
    let name = s[..open].trim().to_ascii_lowercase();
    let is_calc_name = matches!(
        name.as_str(),
        "calc"
            | "min"
            | "max"
            | "clamp"
            | "round"
            | "mod"
            | "rem"
            | "sin"
            | "cos"
            | "tan"
            | "asin"
            | "acos"
            | "atan"
            | "atan2"
            | "pow"
            | "sqrt"
            | "exp"
            | "log"
            | "hypot"
            | "abs"
            | "sign"
            | "calc-size"
    );
    if !is_calc_name {
        return false;
    }
    // The opening paren must match the final paren (one balanced call that
    // spans the whole string), so `min(1%, 2px)` qualifies but
    // `min(1%, 2px) + min(…)` (extra trailing content) does not.
    let mut depth = 0u32;
    for (i, c) in s.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return i == s.len() - 1;
                }
            }
            _ => {}
        }
    }
    false
}

/// Convert an evaluated value into a calc operand node. Numbers stay numeric
/// (and can fold); everything else becomes an opaque string token preserved
/// verbatim.
fn value_to_calc_node(v: Value) -> CalcNode {
    match v.without_slash() {
        Value::Number(n) => CalcNode::Number(n),
        Value::Calc(node) => node,
        other => CalcNode::Str(other.to_css(false)),
    }
}

/// Fold a calc operation: combine two compatible numbers into one; raise
/// dart-sass's incompatible-units error for two known-but-cross-dimension
/// operands of `+`/`-`; otherwise keep the operation for canonical
/// serialization. Only the immediate numeric operands are considered, like
/// dart-sass's limited simplification.
fn fold_calc(op: CalcOp, left: CalcNode, right: CalcNode, pos: Pos) -> Result<CalcNode, Error> {
    if let (CalcNode::Number(a), CalcNode::Number(b)) = (&left, &right) {
        if let Some(n) = fold_numbers(op, a, b, pos)? {
            return Ok(CalcNode::Number(n));
        }
    }
    Ok(CalcNode::Op {
        op,
        left: Box::new(left),
        right: Box::new(right),
    })
}

/// Try to combine two numbers under a calc operator.
///
/// `Ok(Some(n))` folds them; `Ok(None)` preserves the operation verbatim;
/// `Err` is dart-sass's "<a> and <b> are incompatible." rejection.
///
/// For `+`/`-` dart-sass folds equal units and convertible units (converting
/// the right into the left), but — unlike Sass arithmetic — treats a unitless
/// operand mixed with any real unit as an error, and rejects two known
/// absolute units of different dimensions (`1px + 1s`). Two distinct units
/// where at least one is relative/unknown (`1px + 1vw`, `100% - 10px`) or
/// that share a class but are not convertible (`1khz + 1hz`) are preserved.
///
/// `*`/`/` fold when one operand is unitless (or, for `/`, the units cancel
/// after conversion); compound results (`6px * 1s`) are out of scope and
/// preserved.
fn fold_numbers(op: CalcOp, a: &Number, b: &Number, pos: Pos) -> Result<Option<Number>, Error> {
    match op {
        CalcOp::Add | CalcOp::Sub => {
            let apply = |x: f64, y: f64| if op == CalcOp::Add { x + y } else { x - y };
            // Equal units (incl. `%`, relative units, both unitless) fold.
            if a.unit.eq_ignore_ascii_case(&b.unit) {
                return Ok(Some(Number {
                    value: apply(a.value, b.value),
                    unit: a.unit.clone(),
                }));
            }
            // A unitless operand mixed with a real unit is an error in calc.
            if a.unit.is_empty() || b.unit.is_empty() {
                return Err(calc_incompatible(a, b, pos));
            }
            // Two distinct real units: convert when in the same convertible
            // group; error when both are known but cross-dimension; otherwise
            // preserve (a relative/unknown unit is involved).
            if let Some(factor) = crate::value::convert_factor(&b.unit, &a.unit) {
                Ok(Some(Number {
                    value: apply(a.value, b.value * factor),
                    unit: a.unit.clone(),
                }))
            } else if crate::value::calc_units_incompatible(&a.unit, &b.unit) {
                Err(calc_incompatible(a, b, pos))
            } else {
                Ok(None)
            }
        }
        CalcOp::Mul => {
            let unit = if a.unit.is_empty() {
                b.unit.clone()
            } else if b.unit.is_empty() {
                a.unit.clone()
            } else {
                // Compound units (`px * s`) are out of scope; preserve.
                return Ok(None);
            };
            Ok(Some(Number {
                value: a.value * b.value,
                unit,
            }))
        }
        CalcOp::Div => {
            if b.unit.is_empty() {
                return Ok(Some(Number {
                    value: a.value / b.value,
                    unit: a.unit.clone(),
                }));
            }
            if a.unit.eq_ignore_ascii_case(&b.unit) {
                return Ok(Some(Number {
                    value: a.value / b.value,
                    unit: String::new(),
                }));
            }
            // Convertible units cancel to a unitless quotient; anything else
            // (inverse or compound units) is out of scope and preserved.
            match crate::value::convert_factor(&b.unit, &a.unit) {
                Some(factor) => Ok(Some(Number {
                    value: a.value / (b.value * factor),
                    unit: String::new(),
                })),
                None => Ok(None),
            }
        }
    }
}

/// dart-sass's `calc()` incompatible-units error (note: "are incompatible.",
/// distinct from the arithmetic "have incompatible units." wording).
fn calc_incompatible(a: &Number, b: &Number, pos: Pos) -> Error {
    Error::at(
        format!("{} and {} are incompatible.", a.to_css(false), b.to_css(false)),
        pos,
    )
}

/// Evaluate the `/` operator. When `slash` is set and both operands are
/// numbers, produce a slash-separated value that serializes as `a/b` but
/// behaves numerically as the quotient; otherwise perform real division.
fn eval_div(l: Value, r: Value, slash: bool, pos: Pos) -> Result<Value, Error> {
    // The parser only sets `slash` when both operands are numeric literals
    // (or themselves slash divisions), so they are always numbers here. A
    // slash-separated value is kept only when the two units are compatible
    // (equal, or at most one carries a unit), which covers every slash form
    // dart-sass preserves in practice (`1/2`, `16px/1.5`, `10px/2px`,
    // `0.3/0.4px`); a genuinely incompatible pair (`3deg/0.4px`) instead
    // performs real division so its incompatible-units error still fires.
    if let (true, Value::Number(a), Value::Number(b)) =
        (slash, l.clone().without_slash(), r.clone().without_slash())
    {
        let units_compatible = a.unit == b.unit || a.unit.is_empty() || b.unit.is_empty();
        if units_compatible {
            let repr = format!("{}/{}", slash_repr(&l), slash_repr(&r));
            // The carried numeric quotient is only used if the slash is later
            // forced into arithmetic: matching units cancel (`px/px` ->
            // unitless), otherwise the lone unit is kept.
            let unit = if !a.unit.is_empty() && a.unit == b.unit {
                String::new()
            } else if a.unit.is_empty() {
                b.unit.clone()
            } else {
                a.unit.clone()
            };
            return Ok(Value::Slash(
                Number {
                    value: a.value / b.value,
                    unit,
                },
                repr,
            ));
        }
    }
    match (l.without_slash(), r.without_slash()) {
        (Value::Number(a), Value::Number(b)) => divide_numbers(&a, &b, pos),
        // dart-sass: `SassColor.dividedBy` throws "Undefined operation"; a
        // color on the *left* of `/` is the one error case here.
        (l @ Value::Color(_), r) => Err(undefined_op(&l, "/", &r, pos)),
        // Every other left/right pair (a calculation, `var()`, unquoted
        // string, list, `true`/`null`, or a number divided by a non-number)
        // forms a slash-separated unquoted string `left/right`, mirroring
        // dart-sass's default `Value.dividedBy`. This is what lets a `/` next
        // to a `calc()`/`var()` special value survive (and what carries the
        // alpha slash through `rgb(1 2 var(--x) / 0.4)`).
        (l, r) => Ok(Value::Str(SassStr {
            text: format!("{}/{}", l.to_css(false), r.to_css(false)),
            quoted: false,
        })),
    }
}

/// Real division of two numbers with dart-sass unit semantics: two units in
/// the same dimension cancel (the divisor is converted into the dividend's
/// unit first), a unitless divisor keeps the dividend's unit, and a unitless
/// dividend over a real divisor yields the (unrepresentable) inverse unit —
/// kept as the bare divisor unit, matching prior behaviour. Cross-dimension
/// real units error.
fn divide_numbers(a: &Number, b: &Number, pos: Pos) -> Result<Value, Error> {
    let (value, unit) = if b.unit.is_empty() {
        (a.value / b.value, a.unit.clone())
    } else if a.unit.is_empty() {
        // Inverse units are out of scope; preserve the prior result shape.
        (a.value / b.value, b.unit.clone())
    } else if a.unit.eq_ignore_ascii_case(&b.unit) {
        (a.value / b.value, String::new())
    } else {
        match crate::value::convert_factor(&b.unit, &a.unit) {
            Some(factor) => (a.value / (b.value * factor), String::new()),
            None => {
                return Err(Error::at(
                    format!(
                        "{}{} and {}{} have incompatible units.",
                        crate::value::fmt_num(a.value, false),
                        a.unit,
                        crate::value::fmt_num(b.value, false),
                        b.unit
                    ),
                    pos,
                ))
            }
        }
    };
    Ok(Value::Number(Number { value, unit }))
}

/// The slash-spelling text of an operand: a slash value keeps its chained
/// `a/b` text; any other value uses its plain CSS form.
fn slash_repr(v: &Value) -> String {
    match v {
        Value::Slash(_, repr) => repr.clone(),
        other => other.to_css(false),
    }
}

fn eval_binary(op: BinOp, l: Value, r: Value, pos: Pos) -> Result<Value, Error> {
    match op {
        BinOp::Add => binary_add(l, r, pos),
        BinOp::Sub => num_binop(l, r, pos, "-", |a, b| a - b),
        BinOp::Mod => num_binop(l, r, pos, "%", sass_modulo),
        BinOp::Mul => binary_mul(l, r, pos),
        BinOp::Eq => Ok(Value::Bool(l.sass_eq(&r))),
        BinOp::Neq => Ok(Value::Bool(!l.sass_eq(&r))),
        BinOp::Lt => num_compare(l, r, pos, "<", |a, b| a < b),
        BinOp::Gt => num_compare(l, r, pos, ">", |a, b| a > b),
        BinOp::Le => num_compare(l, r, pos, "<=", |a, b| a <= b),
        BinOp::Ge => num_compare(l, r, pos, ">=", |a, b| a >= b),
        BinOp::And | BinOp::Or => Err(Error::unpositioned(
            "internal: and/or are short-circuited in eval_expr",
        )),
    }
}

fn num_compare(
    l: Value,
    r: Value,
    pos: Pos,
    sym: &str,
    f: impl Fn(f64, f64) -> bool,
) -> Result<Value, Error> {
    match (l, r) {
        (Value::Number(a), Value::Number(b)) => {
            let (av, bv, _) = coerce_pair(&a, &b, pos)?;
            Ok(Value::Bool(f(av, bv)))
        }
        (l, r) => Err(undefined_op(&l, sym, &r, pos)),
    }
}

fn binary_add(l: Value, r: Value, pos: Pos) -> Result<Value, Error> {
    if let (Value::Number(a), Value::Number(b)) = (&l, &r) {
        let (av, bv, unit) = coerce_pair(a, b, pos)?;
        return Ok(Value::Number(Number { value: av + bv, unit }));
    }
    // A map cannot be serialized for string concatenation, so `map + x`
    // errors like dart-sass with "(…) isn't a valid CSS value.".
    if let Some(m) = find_map(&l).or_else(|| find_map(&r)) {
        return Err(Error::at(
            format!("{} isn't a valid CSS value.", m.to_css(false)),
            pos,
        ));
    }
    let quoted = matches!(&l, Value::Str(s) if s.quoted);
    let text = format!("{}{}", concat_str(&l), concat_str(&r));
    Ok(Value::Str(SassStr { text, quoted }))
}

fn binary_mul(l: Value, r: Value, pos: Pos) -> Result<Value, Error> {
    match (l, r) {
        (Value::Number(a), Value::Number(b)) => {
            let unit = if a.unit.is_empty() {
                b.unit
            } else if b.unit.is_empty() {
                a.unit
            } else {
                return Err(Error::at(
                    format!(
                        "Multiplication of two units ({} * {}) is not supported.",
                        a.unit, b.unit
                    ),
                    pos,
                ));
            };
            Ok(Value::Number(Number {
                value: a.value * b.value,
                unit,
            }))
        }
        (l, r) => Err(undefined_op(&l, "*", &r, pos)),
    }
}

/// Sass modulo: a floored modulo whose result takes the divisor's sign
/// (matching dart-sass). `1.2 % -4.7 == -3.5`, `-1.2 % 4.7 == 3.5`.
/// Division by zero yields NaN.
fn sass_modulo(a: f64, b: f64) -> f64 {
    if b == 0.0 {
        return f64::NAN;
    }
    a - b * (a / b).floor()
}

fn num_binop(l: Value, r: Value, pos: Pos, sym: &str, f: impl Fn(f64, f64) -> f64) -> Result<Value, Error> {
    match (l, r) {
        (Value::Number(a), Value::Number(b)) => {
            let (av, bv, unit) = coerce_pair(&a, &b, pos)?;
            Ok(Value::Number(Number {
                value: f(av, bv),
                unit,
            }))
        }
        (l, r) => Err(undefined_op(&l, sym, &r, pos)),
    }
}

/// Coerce two numbers onto a common unit for `+`, `-`, `%`, `/`, and
/// comparison. The result keeps the LEFT operand's unit; the right operand
/// is converted into it (`1in + 1cm` → both in inches, result `in`). When
/// exactly one operand is unitless the other's unit is adopted with no
/// rescaling (`5 + 1px` → `6px`). Incompatible real units error, matching
/// dart-sass's `<a> and <b> have incompatible units.`
///
/// Returns `(left_value, right_value, result_unit)` with both values
/// expressed in `result_unit`.
fn coerce_pair(a: &Number, b: &Number, pos: Pos) -> Result<(f64, f64, String), Error> {
    // Equal units (case-insensitively) or a unitless operand never need a
    // numeric conversion.
    if a.unit.eq_ignore_ascii_case(&b.unit) || b.unit.is_empty() {
        return Ok((a.value, b.value, a.unit.clone()));
    }
    if a.unit.is_empty() {
        return Ok((a.value, b.value, b.unit.clone()));
    }
    // Two distinct real units: convert the right into the left's unit.
    match crate::value::convert_factor(&b.unit, &a.unit) {
        Some(factor) => Ok((a.value, b.value * factor, a.unit.clone())),
        None => Err(Error::at(
            format!(
                "{}{} and {}{} have incompatible units.",
                crate::value::fmt_num(a.value, false),
                a.unit,
                crate::value::fmt_num(b.value, false),
                b.unit
            ),
            pos,
        )),
    }
}

fn concat_str(v: &Value) -> String {
    match v {
        Value::Str(s) => s.text.clone(),
        other => other.to_css(false),
    }
}

fn undefined_op(l: &Value, sym: &str, r: &Value, pos: Pos) -> Error {
    Error::at(
        format!(
            "Undefined operation \"{} {} {}\".",
            l.to_css(false),
            sym,
            r.to_css(false)
        ),
        pos,
    )
}

fn is_css_import(arg: &str) -> bool {
    arg.ends_with(".css")
        || arg.starts_with("http://")
        || arg.starts_with("https://")
        || arg.starts_with("//")
}

/// Append a top-level group's output, prefixing a blank-line separator
/// when there is already prior output (and the group is non-empty).
/// dart-sass omits the blank line after an at-rule, so two adjacent at-rules
/// (or an at-rule followed by a style rule) pack together with no gap.
fn push_group(out: &mut Vec<OutNode>, mut group: Vec<OutNode>) {
    if group.is_empty() {
        return;
    }
    // dart-sass packs a passed-through CSS `@import` (a `Raw` at-rule) tight
    // with the following group, just like a real `@rule`.
    let prev_is_at_rule = matches!(out.last(), Some(OutNode::AtRule { .. }) | Some(OutNode::Raw(_)));
    if !out.is_empty() && !prev_is_at_rule {
        out.push(OutNode::Blank);
    }
    out.append(&mut group);
}

/// The integer indices a `@for` iterates: ascending or descending, with the
/// end included (`through`) or excluded (`to`).
/// Normalize a Sass argument/parameter name: hyphens and underscores are
/// interchangeable, so `$b-c` and `$b_c` refer to the same parameter.
fn normalize_arg_name(name: &str) -> String {
    name.replace('_', "-")
}

/// Whether `name` is a global CSS-calculation function that dart-sass parses
/// as a calculation expression, and so cannot accept a `...` rest argument
/// (`clamp`, `hypot`, the exponent/trig functions). `min`/`max` are excluded:
/// they are variadic Sass functions that also accept a splat.
fn is_calc_function(name: &str) -> bool {
    matches!(name, "clamp" | "hypot" | "atan2" | "log" | "pow")
}

/// Find a map anywhere in a value (including nested in a list), returning a
/// reference to the first one. Used to reject maps in CSS output positions,
/// where dart-sass errors with "(…) isn't a valid CSS value.".
fn find_map(v: &Value) -> Option<&Map> {
    match v {
        Value::Map(m) => Some(m),
        Value::List(l) => l.items.iter().find_map(find_map),
        _ => None,
    }
}

fn for_indices(start: i64, end: i64, inclusive: bool) -> Vec<i64> {
    let mut out = Vec::new();
    if start <= end {
        let last = if inclusive { end } else { end - 1 };
        let mut i = start;
        while i <= last {
            out.push(i);
            i += 1;
        }
    } else {
        let last = if inclusive { end } else { end + 1 };
        let mut i = start;
        while i >= last {
            out.push(i);
            i -= 1;
        }
    }
    out
}

/// Resolve a (possibly comma-separated) selector against the parent
/// selector list: substitute `&`, or prepend the parent as a descendant.
fn resolve_selectors(sel: &str, parents: &[String]) -> Vec<String> {
    let parts: Vec<String> = split_commas(sel)
        .into_iter()
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty())
        .collect();
    let mut result = Vec::new();
    if parents.is_empty() {
        for part in &parts {
            let combined = if part.contains('&') {
                part.replace('&', "")
            } else {
                part.clone()
            };
            result.push(normalize_selector(&combined));
        }
    } else {
        for parent in parents {
            for part in &parts {
                let combined = if part.contains('&') {
                    part.replace('&', parent)
                } else {
                    format!("{parent} {part}")
                };
                result.push(normalize_selector(&combined));
            }
        }
    }
    result
}

fn split_commas(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut paren = 0i32;
    let mut bracket = 0i32;
    for c in s.chars() {
        match c {
            '(' => {
                paren += 1;
                cur.push(c);
            }
            ')' => {
                paren -= 1;
                cur.push(c);
            }
            '[' => {
                bracket += 1;
                cur.push(c);
            }
            ']' => {
                bracket -= 1;
                cur.push(c);
            }
            ',' if paren == 0 && bracket == 0 => out.push(std::mem::take(&mut cur)),
            _ => cur.push(c),
        }
    }
    out.push(cur);
    out
}

/// Collapse whitespace and put single spaces around `>`/`+`/`~`
/// combinators (at bracket depth 0), matching dart-sass's selector
/// serialization.
fn normalize_selector(s: &str) -> String {
    let collapsed = s.split_whitespace().collect::<Vec<_>>().join(" ");
    let chars: Vec<char> = collapsed.chars().collect();
    let mut out = String::new();
    let mut paren = 0i32;
    let mut bracket = 0i32;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        match c {
            '(' => {
                paren += 1;
                out.push(c);
            }
            ')' => {
                paren -= 1;
                out.push(c);
            }
            '[' => {
                bracket += 1;
                out.push(c);
            }
            ']' => {
                bracket -= 1;
                out.push(c);
            }
            '>' | '~' | '+' if paren == 0 && bracket == 0 => {
                while out.ends_with(' ') {
                    out.pop();
                }
                out.push(' ');
                out.push(c);
                out.push(' ');
                i += 1;
                while i < chars.len() && chars[i] == ' ' {
                    i += 1;
                }
                continue;
            }
            _ => out.push(c),
        }
        i += 1;
    }
    out.trim().to_string()
}

// ---- media queries -----------------------------------------------------

impl ResolvedQuery {
    /// Serialize one query (dart-sass `CssMediaQuery.toString`).
    fn render(&self) -> String {
        let mut s = String::new();
        if let Some(m) = &self.modifier {
            s.push_str(m);
            s.push(' ');
        }
        if let Some(t) = &self.mtype {
            s.push_str(t);
            if !self.conditions.is_empty() {
                s.push_str(" and ");
            }
        }
        let sep = if self.conjunction_and { " and " } else { " or " };
        s.push_str(&self.conditions.join(sep));
        s
    }
}

/// Serialize a comma list of media queries.
fn serialize_media_queries(queries: &[ResolvedQuery]) -> String {
    queries
        .iter()
        .map(ResolvedQuery::render)
        .collect::<Vec<_>>()
        .join(", ")
}

/// Merge an enclosing query list with a nested query list (dart-sass
/// `_mergeMediaQueries`). Returns `None` if any pair is unrepresentable (keep
/// the nested rule in place); otherwise the merged list, which is empty when
/// every pair is mutually exclusive (the rule is dropped).
fn merge_media_query_lists(outer: &[ResolvedQuery], inner: &[ResolvedQuery]) -> Option<Vec<ResolvedQuery>> {
    let mut merged = Vec::new();
    for a in outer {
        for b in inner {
            match merge_media_query(a, b) {
                MergeResult::Empty => continue,
                MergeResult::Unrepresentable => return None,
                MergeResult::Query(q) => merged.push(q),
            }
        }
    }
    Some(merged)
}

/// Merge two media queries (dart-sass `CssMediaQuery.merge`).
fn merge_media_query(this: &ResolvedQuery, other: &ResolvedQuery) -> MergeResult {
    if !this.conjunction_and || !other.conjunction_and {
        return MergeResult::Unrepresentable;
    }
    let our_modifier = this.modifier.as_ref().map(|s| s.to_ascii_lowercase());
    let our_type = this.mtype.as_ref().map(|s| s.to_ascii_lowercase());
    let their_modifier = other.modifier.as_ref().map(|s| s.to_ascii_lowercase());
    let their_type = other.mtype.as_ref().map(|s| s.to_ascii_lowercase());

    if our_type.is_none() && their_type.is_none() {
        let mut conditions = this.conditions.clone();
        conditions.extend(other.conditions.iter().cloned());
        return MergeResult::Query(ResolvedQuery {
            modifier: None,
            mtype: None,
            conditions,
            conjunction_and: true,
        });
    }

    let our_not = our_modifier.as_deref() == Some("not");
    let their_not = their_modifier.as_deref() == Some("not");
    let is_all = |t: &Option<String>| t.as_deref() == Some("all");

    let (modifier, mtype, conditions): (Option<String>, Option<String>, Vec<String>);
    if our_not != their_not {
        if our_type == their_type {
            let (neg, pos) = if our_not {
                (&this.conditions, &other.conditions)
            } else {
                (&other.conditions, &this.conditions)
            };
            if neg.iter().all(|c| pos.contains(c)) {
                return MergeResult::Empty;
            }
            return MergeResult::Unrepresentable;
        } else if our_type.is_none() || is_all(&our_type) || their_type.is_none() || is_all(&their_type) {
            return MergeResult::Unrepresentable;
        }
        if our_not {
            modifier = their_modifier.clone();
            mtype = their_type.clone();
            conditions = other.conditions.clone();
        } else {
            modifier = our_modifier.clone();
            mtype = our_type.clone();
            conditions = this.conditions.clone();
        }
    } else if our_not {
        if our_type != their_type {
            return MergeResult::Unrepresentable;
        }
        let (more, fewer) = if this.conditions.len() > other.conditions.len() {
            (&this.conditions, &other.conditions)
        } else {
            (&other.conditions, &this.conditions)
        };
        if !fewer.iter().all(|c| more.contains(c)) {
            return MergeResult::Unrepresentable;
        }
        modifier = our_modifier.clone();
        mtype = our_type.clone();
        conditions = more.clone();
    } else if our_type.is_none() || is_all(&our_type) {
        mtype = if (their_type.is_none() || is_all(&their_type)) && our_type.is_none() {
            None
        } else {
            their_type.clone()
        };
        let mut c = this.conditions.clone();
        c.extend(other.conditions.iter().cloned());
        conditions = c;
        modifier = their_modifier.clone();
    } else if their_type.is_none() || is_all(&their_type) {
        let mut c = this.conditions.clone();
        c.extend(other.conditions.iter().cloned());
        conditions = c;
        modifier = our_modifier.clone();
        mtype = our_type.clone();
    } else if our_type != their_type {
        return MergeResult::Empty;
    } else {
        modifier = our_modifier.clone().or_else(|| their_modifier.clone());
        let mut c = this.conditions.clone();
        c.extend(other.conditions.iter().cloned());
        conditions = c;
        mtype = our_type.clone();
    }

    // dart-sass keeps the raw (original-case) type of whichever query
    // contributed it.
    let final_type = match &mtype {
        None => None,
        Some(_) if mtype == our_type => this.mtype.clone(),
        Some(_) => other.mtype.clone(),
    };
    MergeResult::Query(ResolvedQuery {
        modifier,
        mtype: final_type,
        conditions,
        conjunction_and: true,
    })
}

// ---- modern if() condition evaluation ----------------------------------

/// The tri-state outcome of evaluating a modern `if()` condition: a static
/// boolean, or a residual non-evaluable CSS condition kept for verbatim
/// serialization.
enum CondEval {
    Bool(bool),
    Css(RCond),
}

/// A residual (non-evaluable) modern `if()` condition tree. The raw text of
/// each `Css` leaf already has interpolation resolved; the tree is preserved
/// so `and`/`or`/`not`/parentheses serialize canonically.
enum RCond {
    /// A serialized raw substitution sequence (`css(...)`, `var(...)`, ...).
    Css(String),
    Not(Box<RCond>),
    And(Vec<RCond>),
    Or(Vec<RCond>),
    Paren(Box<RCond>),
}

impl RCond {
    fn to_css(&self) -> String {
        match self {
            RCond::Css(s) => s.clone(),
            RCond::Not(c) => format!("not {}", c.to_css()),
            RCond::And(items) => items.iter().map(RCond::to_css).collect::<Vec<_>>().join(" and "),
            RCond::Or(items) => items.iter().map(RCond::to_css).collect::<Vec<_>>().join(" or "),
            RCond::Paren(c) => format!("({})", c.to_css()),
        }
    }
}

/// Combine the residual operands of an `and`/`or` whose statically-known
/// operands were already folded away. When a single residual remains, the
/// operation collapses to it (and a redundant outer paren is dropped, as in
/// dart-sass); otherwise it stays an `and`/`or` chain.
fn combine_residuals(mut residuals: Vec<RCond>, is_and: bool) -> CondEval {
    match residuals.len() {
        // No residuals: every operand was statically known. An `and` that
        // reached here had no false operand (all true) -> true; an `or`
        // that reached here had no true operand (all false) -> false.
        0 => CondEval::Bool(is_and),
        1 => {
            let single = residuals.pop().unwrap_or(RCond::Css(String::new()));
            // A single surviving operand drops a redundant outer paren.
            let unwrapped = match single {
                RCond::Paren(inner) => *inner,
                other => other,
            };
            CondEval::Css(unwrapped)
        }
        _ => {
            if is_and {
                CondEval::Css(RCond::And(residuals))
            } else {
                CondEval::Css(RCond::Or(residuals))
            }
        }
    }
}

/// Serialize a value for the modern `if()` value position, where dart-sass
/// uses a parenthesized-expression context: lists (including the empty list)
/// are wrapped in parentheses; other values serialize as usual.
fn serialize_if_value(v: &Value) -> String {
    match v {
        Value::List(_) => format!("({})", v.to_css(false)),
        Value::Null => "null".to_string(),
        other => other.to_css(false),
    }
}
