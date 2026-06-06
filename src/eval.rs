//! The evaluator: walks the AST, resolving variables, nesting (`&` and
//! the parent×child selector product), interpolation and arithmetic, and
//! flattens the result into a list of output rules.
//!
//! Like dart-sass (and unlike grass), a rule's own declarations are
//! gathered into a single block emitted *before* its nested rules bubble
//! out after it.

use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use crate::ast::{
    BinOp, CallArg, Callable, Declaration, Expr, ParamList, Rule, Stmt, Stylesheet, TplPiece, UnOp, VarDecl,
};
use crate::error::Error;
use crate::scanner::Pos;
use crate::value::{List, ListSep, Number, SassStr, Value};
use crate::{Importer, OutputStyle};

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
        }
    }

    fn push_item(&mut self, item: OutItem) {
        if let Sink::Rule { items, .. } = self {
            items.push(item);
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
    imported: HashSet<String>,
    functions: HashMap<String, Rc<Callable>>,
    mixins: HashMap<String, Rc<Callable>>,
    /// Stack of `@content` blocks, one per active `@include`.
    content_stack: Vec<Option<Rc<Vec<Stmt>>>>,
}

impl<'a> Evaluator<'a> {
    pub(crate) fn new(options: EvalOptions<'a>) -> Self {
        Evaluator {
            scopes: vec![HashMap::new()],
            options,
            imported: HashSet::new(),
            functions: HashMap::new(),
            mixins: HashMap::new(),
            content_stack: Vec::new(),
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
    fn bind_args(
        &mut self,
        params: &ParamList,
        args: &[CallArg],
        name: &str,
    ) -> Result<HashMap<String, Value>, Error> {
        let mut positional = Vec::new();
        let mut keyword: HashMap<String, Value> = HashMap::new();
        for a in args {
            let v = self.eval_expr(&a.value)?;
            match &a.name {
                Some(n) => {
                    keyword.insert(n.clone(), v);
                }
                None => positional.push(v),
            }
        }
        let mut frame = HashMap::new();
        let mut pos_iter = positional.into_iter();
        for param in &params.params {
            let val = if let Some(v) = pos_iter.next() {
                v
            } else if let Some(v) = keyword.remove(&param.name) {
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
            Some(v) => Ok(v),
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
                Stmt::Import(args) => match sink {
                    Sink::Top(out) => {
                        let out = &mut **out;
                        self.eval_imports(args, out)?;
                    }
                    Sink::Rule { .. } => {
                        return Err(Error::unpositioned(
                            "nested @import is not supported in this build",
                        ));
                    }
                },
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

    fn eval_decl(&mut self, d: &Declaration) -> Result<Option<OutItem>, Error> {
        let prop = self.eval_template(&d.property)?.trim().to_string();
        let value = self.eval_expr(&d.value)?;
        if matches!(value, Value::Null) {
            return Ok(None);
        }
        let vstr = value.to_css(self.compressed());
        Ok(Some(OutItem::Decl {
            prop,
            value: vstr,
            important: d.important,
        }))
    }

    /// Inline `@import`s into the top-level output. Each imported top-level
    /// statement becomes its own group; genuine CSS imports pass through.
    fn eval_imports(&mut self, args: &[String], out: &mut Vec<OutNode>) -> Result<(), Error> {
        let importer = self.options.importer;
        for arg in args {
            if is_css_import(arg) {
                push_group(out, vec![OutNode::Raw(format!("@import \"{arg}\";"))]);
                continue;
            }
            match importer.and_then(|imp| imp.resolve(arg)) {
                Some(src) => {
                    if !self.imported.insert(arg.clone()) {
                        continue;
                    }
                    let sheet = crate::parser::parse(&src)?;
                    let mut sink = Sink::Top(&mut *out);
                    self.exec(&sheet.stmts, &[], &mut sink)?;
                }
                None => {
                    return Err(Error::unpositioned(format!(
                        "Can't find stylesheet to import: {arg}"
                    )));
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
            Expr::Var(name) => match self.lookup(name) {
                Some(v) => Ok(v.clone()),
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
            Expr::Paren(inner) => self.eval_expr(inner),
            Expr::List { items, sep } => {
                let mut vals = Vec::with_capacity(items.len());
                for it in items {
                    vals.push(self.eval_expr(it)?);
                }
                Ok(Value::List(List {
                    items: vals,
                    sep: *sep,
                }))
            }
            Expr::Unary { op, operand } => {
                let v = self.eval_expr(operand)?;
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
                        eval_binary(*op, l, r, *pos)
                    }
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
                let mut pos_args = Vec::new();
                let mut named = Vec::new();
                for a in args {
                    let v = self.eval_expr(&a.value)?;
                    match &a.name {
                        Some(n) => named.push((n.clone(), v)),
                        None => pos_args.push(v),
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
                if self.eval_expr(c)?.is_truthy() {
                    self.eval_expr(t)
                } else {
                    self.eval_expr(f)
                }
            }
            _ => Err(Error::at(
                "if() requires arguments $condition, $if-true, $if-false.",
                pos,
            )),
        }
    }
}

fn eval_binary(op: BinOp, l: Value, r: Value, pos: Pos) -> Result<Value, Error> {
    match op {
        BinOp::Add => binary_add(l, r, pos),
        BinOp::Sub => num_binop(l, r, pos, "-", |a, b| a - b),
        BinOp::Mod => num_binop(l, r, pos, "%", |a, b| a.rem_euclid(b)),
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
            if a.unit == b.unit || a.unit.is_empty() || b.unit.is_empty() {
                Ok(Value::Bool(f(a.value, b.value)))
            } else {
                Err(Error::at(
                    format!("Incompatible units {} and {}.", a.unit, b.unit),
                    pos,
                ))
            }
        }
        (l, r) => Err(undefined_op(&l, sym, &r, pos)),
    }
}

fn binary_add(l: Value, r: Value, pos: Pos) -> Result<Value, Error> {
    if let (Value::Number(a), Value::Number(b)) = (&l, &r) {
        let unit = unify_units(&a.unit, &b.unit, pos)?;
        return Ok(Value::Number(Number {
            value: a.value + b.value,
            unit,
        }));
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

fn num_binop(l: Value, r: Value, pos: Pos, sym: &str, f: impl Fn(f64, f64) -> f64) -> Result<Value, Error> {
    match (l, r) {
        (Value::Number(a), Value::Number(b)) => {
            let unit = unify_units(&a.unit, &b.unit, pos)?;
            Ok(Value::Number(Number {
                value: f(a.value, b.value),
                unit,
            }))
        }
        (l, r) => Err(undefined_op(&l, sym, &r, pos)),
    }
}

fn unify_units(a: &str, b: &str, pos: Pos) -> Result<String, Error> {
    if a == b || b.is_empty() {
        Ok(a.to_string())
    } else if a.is_empty() {
        Ok(b.to_string())
    } else {
        Err(Error::at(format!("Incompatible units {a} and {b}."), pos))
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
fn push_group(out: &mut Vec<OutNode>, mut group: Vec<OutNode>) {
    if group.is_empty() {
        return;
    }
    if !out.is_empty() {
        out.push(OutNode::Blank);
    }
    out.append(&mut group);
}

/// The integer indices a `@for` iterates: ascending or descending, with the
/// end included (`through`) or excluded (`to`).
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
