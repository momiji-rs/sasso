//! The evaluator: walks the AST, resolving variables, nesting (`&` and
//! the parent×child selector product), interpolation and arithmetic, and
//! flattens the result into a list of output rules.
//!
//! Like dart-sass (and unlike grass), a rule's own declarations are
//! gathered into a single block emitted *before* its nested rules bubble
//! out after it.

use std::collections::{HashMap, HashSet};

use crate::ast::{BinOp, CallArg, Declaration, Expr, Rule, Stmt, Stylesheet, TplPiece, UnOp, VarDecl};
use crate::error::Error;
use crate::scanner::Pos;
use crate::value::{List, Number, SassStr, Value};
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

/// Options visible to the evaluator (subset of the public `Options`).
pub(crate) struct EvalOptions<'a> {
    pub style: OutputStyle,
    pub importer: Option<&'a dyn Importer>,
}

pub(crate) struct Evaluator<'a> {
    scopes: Vec<HashMap<String, Value>>,
    options: EvalOptions<'a>,
    imported: HashSet<String>,
}

impl<'a> Evaluator<'a> {
    pub(crate) fn new(options: EvalOptions<'a>) -> Self {
        Evaluator {
            scopes: vec![HashMap::new()],
            options,
            imported: HashSet::new(),
        }
    }

    pub(crate) fn eval_sheet(&mut self, sheet: &Stylesheet, out: &mut Vec<OutNode>) -> Result<(), Error> {
        self.eval_top_stmts(&sheet.stmts, out)
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

    // ---- statements --------------------------------------------------

    /// Evaluate top-level statements, grouping each statement's output so a
    /// blank line separates consecutive groups (dart-sass expanded style).
    fn eval_top_stmts(&mut self, stmts: &[Stmt], out: &mut Vec<OutNode>) -> Result<(), Error> {
        let no_parents: Vec<String> = Vec::new();
        for stmt in stmts {
            match stmt {
                Stmt::VarDecl(v) => self.apply_var(v)?,
                Stmt::Decl(d) => {
                    return Err(Error::at("top-level declarations aren't allowed", d.pos));
                }
                Stmt::Import(args) => self.eval_import_top(args, out)?,
                Stmt::Rule(r) => {
                    let mut group: Vec<OutNode> = Vec::new();
                    self.eval_rule(r, &no_parents, &mut group)?;
                    push_group(out, group);
                }
                Stmt::Comment(c) => push_group(out, vec![OutNode::Comment(c.clone())]),
            }
        }
        Ok(())
    }

    fn eval_rule(&mut self, rule: &Rule, parents: &[String], out: &mut Vec<OutNode>) -> Result<(), Error> {
        let sel_str = self.eval_template(&rule.selector)?;
        let current = resolve_selectors(&sel_str, parents);
        self.scopes.push(HashMap::new());
        let mut items: Vec<OutItem> = Vec::new();
        let mut nested: Vec<OutNode> = Vec::new();
        for stmt in &rule.body {
            match stmt {
                Stmt::VarDecl(v) => self.apply_var(v)?,
                Stmt::Decl(d) => {
                    if let Some(oi) = self.eval_decl(d)? {
                        items.push(oi);
                    }
                }
                Stmt::Rule(r) => self.eval_rule(r, &current, &mut nested)?,
                Stmt::Comment(c) => items.push(OutItem::Comment(c.clone())),
                Stmt::Import(_) => {
                    return Err(Error::at(
                        "nested @import is not supported in this build",
                        rule.pos,
                    ));
                }
            }
        }
        self.scopes.pop();
        if !items.is_empty() {
            out.push(OutNode::Rule {
                selectors: current,
                items,
            });
        }
        out.append(&mut nested);
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

    /// Inline a top-level `@import`. Each imported top-level statement
    /// becomes its own group (so it blank-separates like a local rule),
    /// while genuine CSS imports pass through verbatim.
    fn eval_import_top(&mut self, args: &[String], out: &mut Vec<OutNode>) -> Result<(), Error> {
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
                    self.eval_top_stmts(&sheet.stmts, out)?;
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
