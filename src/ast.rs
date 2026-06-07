//! The parsed syntax tree.
//!
//! Selectors and property names are templates ([`TplPiece`]) because they
//! may contain `#{...}` interpolation that is only resolved at eval time.

use std::rc::Rc;

use crate::scanner::Pos;
use crate::value::{Color, ListSep};

/// A parsed stylesheet: an ordered list of top-level statements.
pub(crate) struct Stylesheet {
    pub stmts: Vec<Stmt>,
}

/// A statement, valid at the top level or inside a rule body.
pub(crate) enum Stmt {
    /// `$name: value [!default] [!global];`
    VarDecl(VarDecl),
    /// `selector { ... }`
    Rule(Rule),
    /// `property: value [!important];`
    Decl(Declaration),
    /// `@import "a", "b";` — the args are the raw (unquoted) paths.
    Import(Vec<String>),
    /// `/* ... */` loud comment (inner text, without the delimiters).
    Comment(String),
    /// `@if`/`@else if`/`@else` — evaluated top to bottom, first match wins.
    If(Vec<IfBranch>),
    /// `@for $i from A through|to B { … }`.
    For {
        var: String,
        from: Expr,
        to: Expr,
        inclusive: bool,
        body: Vec<Stmt>,
    },
    /// `@each $v[, $k…] in <list> { … }`.
    Each {
        vars: Vec<String>,
        list: Expr,
        body: Vec<Stmt>,
    },
    /// `@while <cond> { … }`.
    While { cond: Expr, body: Vec<Stmt> },
    /// `@function name(params) { … @return … }`. Shared in an `Rc` so the
    /// definition survives in the environment after its source drops.
    FunctionDef(Rc<Callable>),
    /// `@return <expr>;`
    Return(Expr),
    /// `@mixin name(params) { … }`.
    MixinDef(Rc<Callable>),
    /// `@include name(args) [{ content }];`
    Include {
        name: String,
        args: Vec<CallArg>,
        content: Option<Rc<Vec<Stmt>>>,
    },
    /// `@content;` — runs the `@include`'s content block.
    Content,
    /// A generic at-rule: `@name <prelude> { body }` or `@name <prelude>;`.
    /// `body == None` is the statement (`;`) form. Used for `@font-face`,
    /// `@page`, `@charset`, `@supports`, vendor `@foo`, and unknown
    /// directives alike.
    AtRule {
        name: String,
        prelude: Vec<TplPiece>,
        body: Option<Vec<Stmt>>,
    },
    /// `@warn <expr>;` — writes to stderr, emits no CSS.
    Warn(Expr),
    /// `@debug <expr>;` — writes to stderr, emits no CSS.
    Debug(Expr),
    /// `@error <expr>;` — aborts compilation with the message.
    Error(Expr),
}

/// One arm of an `@if` chain. `cond == None` is the trailing `@else`.
pub(crate) struct IfBranch {
    pub cond: Option<Expr>,
    pub body: Vec<Stmt>,
}

/// A `@function` or `@mixin` definition (same shape).
pub(crate) struct Callable {
    pub name: String,
    pub params: ParamList,
    pub body: Vec<Stmt>,
}

/// A declared parameter list: positional/defaulted params plus an optional
/// trailing rest parameter (`$args...`).
pub(crate) struct ParamList {
    pub params: Vec<Param>,
    pub rest: Option<String>,
}

/// One declared parameter, with an optional default expression.
pub(crate) struct Param {
    pub name: String,
    pub default: Option<Expr>,
}

pub(crate) struct VarDecl {
    pub name: String,
    pub value: Expr,
    pub is_default: bool,
    pub is_global: bool,
}

pub(crate) struct Rule {
    pub selector: Vec<TplPiece>,
    pub body: Vec<Stmt>,
}

pub(crate) struct Declaration {
    pub property: Vec<TplPiece>,
    pub value: Expr,
    pub important: bool,
    pub pos: Pos,
}

/// One piece of an interpolated template: literal text or an embedded
/// expression (`#{...}`).
pub(crate) enum TplPiece {
    Lit(String),
    Interp(Expr),
}

/// A value expression.
pub(crate) enum Expr {
    /// Numeric literal: value + unit (`""` for unitless).
    Number(f64, String),
    /// Color literal (hex or named), parsed eagerly.
    Color(Color),
    /// Quoted string, possibly with interpolation.
    QuotedString(Vec<TplPiece>),
    /// Unquoted identifier/string, possibly with interpolation
    /// (e.g. `solid`, `sans-serif`, `col-#{$n}`).
    Ident(Vec<TplPiece>),
    /// `true` / `false`.
    Bool(bool),
    /// `null`.
    Null,
    /// `$name` variable reference.
    Var(String),
    /// Binary arithmetic / string concatenation.
    Binary {
        op: BinOp,
        lhs: Box<Expr>,
        rhs: Box<Expr>,
        pos: Pos,
    },
    /// The `a / b` slash operator. When `slash` is true (both operands are
    /// number literals or themselves slash divisions), it produces a
    /// slash-separated value that serializes as `a/b`; otherwise it performs
    /// real division.
    Div {
        lhs: Box<Expr>,
        rhs: Box<Expr>,
        slash: bool,
        pos: Pos,
    },
    /// A `calc()` calculation whose interior is the wrapped expression.
    /// Evaluated with calc simplification rules (numeric subtrees fold,
    /// everything else is preserved verbatim).
    Calc { inner: Box<Expr> },
    /// Unary negation.
    Unary { op: UnOp, operand: Box<Expr> },
    /// Function call.
    Func {
        name: String,
        args: Vec<CallArg>,
        pos: Pos,
    },
    /// A space- or comma-separated list.
    List { items: Vec<Expr>, sep: ListSep },
    /// `( expr )`.
    Paren(Box<Expr>),
    /// `#{ expr }` used in value position — always yields an unquoted
    /// string.
    Interp(Box<Expr>),
}

/// A call argument, optionally named (`$name: value`).
pub(crate) struct CallArg {
    pub name: Option<String>,
    pub value: Expr,
}

#[derive(Clone, Copy)]
pub(crate) enum BinOp {
    Add,
    Sub,
    Mul,
    Mod,
    Eq,
    Neq,
    Lt,
    Gt,
    Le,
    Ge,
    And,
    Or,
}

#[derive(Clone, Copy)]
pub(crate) enum UnOp {
    Neg,
    Not,
}
