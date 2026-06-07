//! The SCSS parser: a character-level recursive-descent parser.
//!
//! SCSS is context-sensitive — a leading `:` can begin a declaration
//! value or a pseudo-class selector — so statements are disambiguated by
//! a bounded lookahead ([`Parser::classify`]) that finds whether a
//! top-level `{` (a rule) or `;`/`}` (a declaration) comes first.

use std::rc::Rc;

use crate::ast::{
    BinOp, CallArg, Callable, Declaration, Expr, IfBranch, Param, ParamList, Rule, Stmt, Stylesheet,
    TplPiece, UnOp, VarDecl,
};
use crate::error::Error;
use crate::scanner::Scanner;
use crate::value::{named_color, Color, ListSep};

enum NextKind {
    Rule,
    Declaration,
}

struct Parser {
    sc: Scanner,
    /// Depth of enclosing `calc()`/math-function contexts. Inside one, `/`
    /// is always real division (never a slash separator) and `+`/`-` must be
    /// surrounded by whitespace.
    calc_depth: u32,
}

/// Parse a complete stylesheet.
pub(crate) fn parse(src: &str) -> Result<Stylesheet, Error> {
    let mut p = Parser {
        sc: Scanner::new(src),
        calc_depth: 0,
    };
    let stmts = p.parse_statements(true)?;
    Ok(Stylesheet { stmts })
}

fn is_ident_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '-' || c == '_'
}

/// Whether `expr` is eligible to keep the deprecated `/` slash spelling.
/// dart-sass keeps the slash only between number literals (including a
/// unary-signed literal) and chains of such slash divisions; variables,
/// function calls, parentheses, and other operations force real division.
fn is_slash_operand(expr: &Expr) -> bool {
    match expr {
        Expr::Number(_, _) => true,
        Expr::Div { slash, .. } => *slash,
        Expr::Unary {
            op: UnOp::Neg,
            operand,
        } => is_slash_operand(operand),
        _ => false,
    }
}

impl Parser {
    fn parse_statements(&mut self, top: bool) -> Result<Vec<Stmt>, Error> {
        let mut stmts = Vec::new();
        loop {
            self.skip_trivia(&mut stmts);
            match self.sc.peek() {
                None => {
                    if top {
                        break;
                    }
                    return Err(Error::at(
                        "unexpected end of input, expected \"}\"",
                        self.sc.position(),
                    ));
                }
                Some('}') => {
                    if top {
                        return Err(Error::at("unexpected \"}\"", self.sc.position()));
                    }
                    break;
                }
                Some('$') => stmts.push(self.parse_var_decl()?),
                Some('@') => stmts.push(self.parse_at_rule()?),
                _ => match self.classify() {
                    NextKind::Rule => stmts.push(self.parse_rule()?),
                    NextKind::Declaration => stmts.push(self.parse_declaration()?),
                },
            }
        }
        Ok(stmts)
    }

    /// Skip whitespace and comments, collecting loud `/* */` comments into
    /// the statement stream so they emit in source order.
    fn skip_trivia(&mut self, out: &mut Vec<Stmt>) {
        loop {
            match self.sc.peek() {
                Some(c) if c.is_whitespace() => {
                    self.sc.bump();
                }
                Some('/') if self.sc.peek_at(1) == Some('/') => {
                    while let Some(c) = self.sc.peek() {
                        if c == '\n' {
                            break;
                        }
                        self.sc.bump();
                    }
                }
                Some('/') if self.sc.peek_at(1) == Some('*') => {
                    self.sc.bump();
                    self.sc.bump();
                    let mut inner = String::new();
                    loop {
                        match self.sc.peek() {
                            None => break,
                            Some('*') if self.sc.peek_at(1) == Some('/') => {
                                self.sc.bump();
                                self.sc.bump();
                                break;
                            }
                            Some(c) => {
                                inner.push(c);
                                self.sc.bump();
                            }
                        }
                    }
                    out.push(Stmt::Comment(inner));
                }
                _ => break,
            }
        }
    }

    /// Skip inline whitespace; report whether any was consumed.
    fn skip_ws_inline(&mut self) -> bool {
        let mut any = false;
        while matches!(self.sc.peek(), Some(c) if c.is_whitespace()) {
            self.sc.bump();
            any = true;
        }
        any
    }

    /// Look ahead to decide whether the next statement is a rule (a
    /// top-level `{` comes first) or a declaration (`;`/`}` comes first),
    /// skipping over strings, comments, interpolation and bracket pairs.
    fn classify(&self) -> NextKind {
        let cs = self.sc.rest();
        let mut i = 0;
        let mut paren = 0i32;
        let mut bracket = 0i32;
        while i < cs.len() {
            let c = cs[i];
            match c {
                '"' | '\'' => {
                    let q = c;
                    i += 1;
                    while i < cs.len() && cs[i] != q {
                        if cs[i] == '\\' {
                            i += 1;
                        }
                        i += 1;
                    }
                    i += 1;
                    continue;
                }
                '/' if cs.get(i + 1) == Some(&'/') => {
                    while i < cs.len() && cs[i] != '\n' {
                        i += 1;
                    }
                    continue;
                }
                '/' if cs.get(i + 1) == Some(&'*') => {
                    i += 2;
                    while i + 1 < cs.len() && !(cs[i] == '*' && cs[i + 1] == '/') {
                        i += 1;
                    }
                    i += 2;
                    continue;
                }
                '#' if cs.get(i + 1) == Some(&'{') => {
                    i += 2;
                    let mut depth = 1;
                    while i < cs.len() && depth > 0 {
                        match cs[i] {
                            '{' => depth += 1,
                            '}' => depth -= 1,
                            _ => {}
                        }
                        i += 1;
                    }
                    continue;
                }
                '(' => paren += 1,
                ')' => paren -= 1,
                '[' => bracket += 1,
                ']' => bracket -= 1,
                '{' if paren == 0 && bracket == 0 => return NextKind::Rule,
                ';' if paren == 0 && bracket == 0 => return NextKind::Declaration,
                '}' if paren == 0 && bracket == 0 => return NextKind::Declaration,
                _ => {}
            }
            i += 1;
        }
        NextKind::Declaration
    }

    fn parse_rule(&mut self) -> Result<Stmt, Error> {
        let selector = self.parse_template(&['{'])?;
        if !self.sc.eat('{') {
            return Err(Error::at("expected \"{\"", self.sc.position()));
        }
        let body = self.parse_statements(false)?;
        if !self.sc.eat('}') {
            return Err(Error::at("expected \"}\"", self.sc.position()));
        }
        Ok(Stmt::Rule(Rule { selector, body }))
    }

    fn parse_declaration(&mut self) -> Result<Stmt, Error> {
        let pos = self.sc.position();
        let property = self.parse_template(&[':'])?;
        if !self.sc.eat(':') {
            return Err(Error::at("expected \":\" in declaration", self.sc.position()));
        }
        self.skip_ws_inline();
        let value = self.parse_value()?;
        self.skip_ws_inline();
        let mut important = false;
        if self.sc.peek() == Some('!') {
            let mark = self.sc.mark();
            self.sc.bump();
            self.skip_ws_inline();
            let flag = self.read_ident_name().unwrap_or_default();
            if flag.eq_ignore_ascii_case("important") {
                important = true;
            } else {
                self.sc.reset(mark);
            }
        }
        self.skip_ws_inline();
        self.sc.eat(';');
        Ok(Stmt::Decl(Declaration {
            property,
            value,
            important,
            pos,
        }))
    }

    fn parse_var_decl(&mut self) -> Result<Stmt, Error> {
        let pos = self.sc.position();
        self.sc.bump(); // '$'
        let name = self.read_ident_name()?;
        self.skip_ws_inline();
        if !self.sc.eat(':') {
            return Err(Error::at(
                "expected \":\" after variable name",
                self.sc.position(),
            ));
        }
        self.skip_ws_inline();
        let value = self.parse_value()?;
        let mut is_default = false;
        let mut is_global = false;
        loop {
            self.skip_ws_inline();
            if self.sc.peek() == Some('!') {
                self.sc.bump();
                let flag = self.read_ident_name()?;
                match flag.as_str() {
                    "default" => is_default = true,
                    "global" => is_global = true,
                    other => return Err(Error::at(format!("invalid flag !{other}"), pos)),
                }
            } else {
                break;
            }
        }
        self.skip_ws_inline();
        self.sc.eat(';');
        Ok(Stmt::VarDecl(VarDecl {
            name,
            value,
            is_default,
            is_global,
        }))
    }

    fn parse_at_rule(&mut self) -> Result<Stmt, Error> {
        let pos = self.sc.position();
        self.sc.bump(); // '@'
        let name = self.read_ident_name()?;
        match name.as_str() {
            "import" => {
                let mut args = Vec::new();
                loop {
                    self.skip_ws_inline();
                    match self.sc.peek() {
                        Some('"') | Some('\'') => {
                            let pieces = self.parse_quoted_string()?;
                            let mut path = String::new();
                            for p in &pieces {
                                match p {
                                    TplPiece::Lit(s) => path.push_str(s),
                                    TplPiece::Interp(_) => {
                                        return Err(Error::at(
                                            "dynamic @import paths are not supported",
                                            pos,
                                        ));
                                    }
                                }
                            }
                            args.push(path);
                        }
                        _ => return Err(Error::at("expected a string after @import", self.sc.position())),
                    }
                    self.skip_ws_inline();
                    if self.sc.eat(',') {
                        continue;
                    }
                    break;
                }
                self.skip_ws_inline();
                self.sc.eat(';');
                Ok(Stmt::Import(args))
            }
            "if" => self.parse_if(),
            "for" => self.parse_for(),
            "each" => self.parse_each(),
            "while" => self.parse_while(),
            "function" => self.parse_callable_def(true),
            "mixin" => self.parse_callable_def(false),
            "return" => self.parse_return(),
            "include" => self.parse_include(),
            "content" => {
                self.skip_ws_inline();
                self.sc.eat(';');
                Ok(Stmt::Content)
            }
            other => Err(Error::at(format!("@{other} is not supported in this build"), pos)),
        }
    }

    /// Parse a `@function name(params) { … }` or `@mixin name(params) { … }`.
    fn parse_callable_def(&mut self, is_function: bool) -> Result<Stmt, Error> {
        self.skip_ws_inline();
        let name = self.read_ident_name()?;
        let params = self.parse_param_list()?;
        let body = self.parse_braced_body()?;
        let callable = Rc::new(Callable { name, params, body });
        Ok(if is_function {
            Stmt::FunctionDef(callable)
        } else {
            Stmt::MixinDef(callable)
        })
    }

    /// Parse a declared parameter list `($a, $b: default, $rest...)`.
    /// Missing parentheses means no parameters.
    fn parse_param_list(&mut self) -> Result<ParamList, Error> {
        let mut params = Vec::new();
        let mut rest = None;
        self.skip_ws_inline();
        if self.sc.peek() != Some('(') {
            return Ok(ParamList { params, rest });
        }
        self.sc.bump(); // '('
        self.skip_ws_inline();
        if self.sc.peek() == Some(')') {
            self.sc.bump();
            return Ok(ParamList { params, rest });
        }
        loop {
            self.skip_ws_inline();
            if !self.sc.eat('$') {
                return Err(Error::at("expected a parameter", self.sc.position()));
            }
            let name = self.read_ident_name()?;
            self.skip_ws_inline();
            if self.sc.peek() == Some('.')
                && self.sc.peek_at(1) == Some('.')
                && self.sc.peek_at(2) == Some('.')
            {
                self.sc.bump();
                self.sc.bump();
                self.sc.bump();
                rest = Some(name);
                self.skip_ws_inline();
                break;
            }
            let default = if self.sc.peek() == Some(':') {
                self.sc.bump();
                self.skip_ws_inline();
                Some(self.space_list()?)
            } else {
                None
            };
            params.push(Param { name, default });
            self.skip_ws_inline();
            if self.sc.eat(',') {
                continue;
            }
            break;
        }
        self.skip_ws_inline();
        if !self.sc.eat(')') {
            return Err(Error::at("expected \")\"", self.sc.position()));
        }
        Ok(ParamList { params, rest })
    }

    /// Parse `@return <expr>;`.
    fn parse_return(&mut self) -> Result<Stmt, Error> {
        self.skip_ws_inline();
        let value = self.parse_value()?;
        self.skip_ws_inline();
        self.sc.eat(';');
        Ok(Stmt::Return(value))
    }

    /// Parse `@include name[(args)] [{ content }];`.
    fn parse_include(&mut self) -> Result<Stmt, Error> {
        self.skip_ws_inline();
        let name = self.read_ident_name()?;
        self.skip_ws_inline();
        let args = if self.sc.peek() == Some('(') {
            self.sc.bump();
            self.parse_args_after_paren()?
        } else {
            Vec::new()
        };
        self.skip_ws_inline();
        let content = if self.sc.peek() == Some('{') {
            Some(Rc::new(self.parse_braced_body()?))
        } else {
            self.sc.eat(';');
            None
        };
        Ok(Stmt::Include { name, args, content })
    }

    /// `@for $i from <start> through|to <end> { … }`. Bounds are parsed at
    /// the additive level so the `through`/`to` keywords are not swallowed
    /// into a space list.
    fn parse_for(&mut self) -> Result<Stmt, Error> {
        self.skip_ws_inline();
        if !self.sc.eat('$') {
            return Err(Error::at("expected a variable after @for", self.sc.position()));
        }
        let var = self.read_ident_name()?;
        if !self.try_keyword("from") {
            return Err(Error::at("expected \"from\"", self.sc.position()));
        }
        self.skip_ws_inline();
        let from = self.additive()?;
        let inclusive = if self.try_keyword("through") {
            true
        } else if self.try_keyword("to") {
            false
        } else {
            return Err(Error::at("expected \"through\" or \"to\"", self.sc.position()));
        };
        self.skip_ws_inline();
        let to = self.additive()?;
        let body = self.parse_braced_body()?;
        Ok(Stmt::For {
            var,
            from,
            to,
            inclusive,
            body,
        })
    }

    /// `@each $v[, $k…] in <list> { … }`.
    fn parse_each(&mut self) -> Result<Stmt, Error> {
        let mut vars = Vec::new();
        loop {
            self.skip_ws_inline();
            if !self.sc.eat('$') {
                return Err(Error::at("expected a variable after @each", self.sc.position()));
            }
            vars.push(self.read_ident_name()?);
            self.skip_ws_inline();
            if self.sc.eat(',') {
                continue;
            }
            break;
        }
        if !self.try_keyword("in") {
            return Err(Error::at("expected \"in\"", self.sc.position()));
        }
        self.skip_ws_inline();
        let list = self.parse_value()?;
        let body = self.parse_braced_body()?;
        Ok(Stmt::Each { vars, list, body })
    }

    /// `@while <cond> { … }`.
    fn parse_while(&mut self) -> Result<Stmt, Error> {
        self.skip_ws_inline();
        let cond = self.parse_value()?;
        let body = self.parse_braced_body()?;
        Ok(Stmt::While { cond, body })
    }

    /// Parse an `@if <cond> { … }` with its `@else if` / `@else` chain.
    fn parse_if(&mut self) -> Result<Stmt, Error> {
        let mut branches = Vec::new();
        self.skip_ws_inline();
        let cond = self.parse_value()?;
        let body = self.parse_braced_body()?;
        branches.push(IfBranch {
            cond: Some(cond),
            body,
        });
        loop {
            let mark = self.sc.mark();
            let mut discard = Vec::new();
            self.skip_trivia(&mut discard);
            if self.sc.peek() != Some('@') {
                self.sc.reset(mark);
                break;
            }
            self.sc.bump(); // '@'
            if self.read_ident_name().unwrap_or_default() != "else" {
                self.sc.reset(mark);
                break;
            }
            if self.try_keyword("if") {
                self.skip_ws_inline();
                let cond = self.parse_value()?;
                let body = self.parse_braced_body()?;
                branches.push(IfBranch {
                    cond: Some(cond),
                    body,
                });
            } else {
                let body = self.parse_braced_body()?;
                branches.push(IfBranch { cond: None, body });
                break;
            }
        }
        Ok(Stmt::If(branches))
    }

    /// Parse a `{ … }` statement block.
    fn parse_braced_body(&mut self) -> Result<Vec<Stmt>, Error> {
        self.skip_ws_inline();
        if !self.sc.eat('{') {
            return Err(Error::at("expected \"{\"", self.sc.position()));
        }
        let body = self.parse_statements(false)?;
        if !self.sc.eat('}') {
            return Err(Error::at("expected \"}\"", self.sc.position()));
        }
        Ok(body)
    }

    /// Parse an interpolated template (selector or property name) up to,
    /// but not including, one of `stops` at bracket depth 0.
    fn parse_template(&mut self, stops: &[char]) -> Result<Vec<TplPiece>, Error> {
        let mut pieces = Vec::new();
        let mut lit = String::new();
        let mut paren = 0i32;
        let mut bracket = 0i32;
        while let Some(c) = self.sc.peek() {
            if paren == 0 && bracket == 0 && stops.contains(&c) {
                break;
            }
            match c {
                '#' if self.sc.peek_at(1) == Some('{') => {
                    if !lit.is_empty() {
                        pieces.push(TplPiece::Lit(std::mem::take(&mut lit)));
                    }
                    self.sc.bump();
                    self.sc.bump();
                    let e = self.parse_value()?;
                    self.skip_ws_inline();
                    if !self.sc.eat('}') {
                        return Err(Error::at("expected \"}\"", self.sc.position()));
                    }
                    pieces.push(TplPiece::Interp(e));
                }
                '"' | '\'' => {
                    lit.push(c);
                    self.sc.bump();
                    while let Some(ch) = self.sc.peek() {
                        lit.push(ch);
                        self.sc.bump();
                        if ch == '\\' {
                            if let Some(n) = self.sc.bump() {
                                lit.push(n);
                            }
                            continue;
                        }
                        if ch == c {
                            break;
                        }
                    }
                }
                '(' => {
                    paren += 1;
                    lit.push(c);
                    self.sc.bump();
                }
                ')' => {
                    paren -= 1;
                    lit.push(c);
                    self.sc.bump();
                }
                '[' => {
                    bracket += 1;
                    lit.push(c);
                    self.sc.bump();
                }
                ']' => {
                    bracket -= 1;
                    lit.push(c);
                    self.sc.bump();
                }
                _ => {
                    lit.push(c);
                    self.sc.bump();
                }
            }
        }
        if !lit.is_empty() {
            pieces.push(TplPiece::Lit(lit));
        }
        Ok(pieces)
    }

    fn read_ident_name(&mut self) -> Result<String, Error> {
        let mut s = String::new();
        while matches!(self.sc.peek(), Some(c) if is_ident_char(c)) {
            if let Some(c) = self.sc.bump() {
                s.push(c);
            }
        }
        if s.is_empty() {
            return Err(Error::at("expected an identifier", self.sc.position()));
        }
        Ok(s)
    }

    // ---- value expressions -------------------------------------------

    fn parse_value(&mut self) -> Result<Expr, Error> {
        self.comma_list()
    }

    fn at_value_terminator(&self) -> bool {
        matches!(
            self.sc.peek(),
            None | Some(',') | Some(';') | Some('}') | Some(')') | Some(']') | Some('{') | Some('!')
        )
    }

    fn comma_list(&mut self) -> Result<Expr, Error> {
        let first = self.space_list()?;
        let mut rest = Vec::new();
        loop {
            let mark = self.sc.mark();
            self.skip_ws_inline();
            if self.sc.peek() == Some(',') {
                self.sc.bump();
                self.skip_ws_inline();
                if self.at_value_terminator() {
                    break;
                }
                rest.push(self.space_list()?);
            } else {
                self.sc.reset(mark);
                break;
            }
        }
        if rest.is_empty() {
            Ok(first)
        } else {
            let mut items = Vec::with_capacity(rest.len() + 1);
            items.push(first);
            items.extend(rest);
            Ok(Expr::List {
                items,
                sep: ListSep::Comma,
            })
        }
    }

    fn space_list(&mut self) -> Result<Expr, Error> {
        let first = self.or_expr()?;
        let mut rest = Vec::new();
        loop {
            let mark = self.sc.mark();
            let had_ws = self.skip_ws_inline();
            if !had_ws || self.at_value_terminator() {
                self.sc.reset(mark);
                break;
            }
            rest.push(self.or_expr()?);
        }
        if rest.is_empty() {
            Ok(first)
        } else {
            let mut items = Vec::with_capacity(rest.len() + 1);
            items.push(first);
            items.extend(rest);
            Ok(Expr::List {
                items,
                sep: ListSep::Space,
            })
        }
    }

    // Operator precedence, lowest to highest: `or`, `and`, `not`, equality
    // (== !=), relational (< > <= >=), then additive (below). The logical
    // keywords are bare identifiers recognized only in operator position.

    fn or_expr(&mut self) -> Result<Expr, Error> {
        let mut lhs = self.and_expr()?;
        while self.try_keyword("or") {
            self.skip_ws_inline();
            let pos = self.sc.position();
            let rhs = self.and_expr()?;
            lhs = Expr::Binary {
                op: BinOp::Or,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
                pos,
            };
        }
        Ok(lhs)
    }

    fn and_expr(&mut self) -> Result<Expr, Error> {
        let mut lhs = self.not_expr()?;
        while self.try_keyword("and") {
            self.skip_ws_inline();
            let pos = self.sc.position();
            let rhs = self.not_expr()?;
            lhs = Expr::Binary {
                op: BinOp::And,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
                pos,
            };
        }
        Ok(lhs)
    }

    fn not_expr(&mut self) -> Result<Expr, Error> {
        if self.try_keyword("not") {
            self.skip_ws_inline();
            let operand = self.not_expr()?;
            return Ok(Expr::Unary {
                op: UnOp::Not,
                operand: Box::new(operand),
            });
        }
        self.equality()
    }

    fn equality(&mut self) -> Result<Expr, Error> {
        let mut lhs = self.relational()?;
        loop {
            let mark = self.sc.mark();
            self.skip_ws_inline();
            let op = if self.sc.peek() == Some('=') && self.sc.peek_at(1) == Some('=') {
                self.sc.bump();
                self.sc.bump();
                BinOp::Eq
            } else if self.sc.peek() == Some('!') && self.sc.peek_at(1) == Some('=') {
                self.sc.bump();
                self.sc.bump();
                BinOp::Neq
            } else {
                self.sc.reset(mark);
                break;
            };
            let pos = self.sc.position();
            self.skip_ws_inline();
            let rhs = self.relational()?;
            lhs = Expr::Binary {
                op,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
                pos,
            };
        }
        Ok(lhs)
    }

    fn relational(&mut self) -> Result<Expr, Error> {
        let mut lhs = self.additive()?;
        loop {
            let mark = self.sc.mark();
            self.skip_ws_inline();
            let op = match (self.sc.peek(), self.sc.peek_at(1)) {
                (Some('<'), Some('=')) => {
                    self.sc.bump();
                    self.sc.bump();
                    BinOp::Le
                }
                (Some('>'), Some('=')) => {
                    self.sc.bump();
                    self.sc.bump();
                    BinOp::Ge
                }
                (Some('<'), _) => {
                    self.sc.bump();
                    BinOp::Lt
                }
                (Some('>'), _) => {
                    self.sc.bump();
                    BinOp::Gt
                }
                _ => {
                    self.sc.reset(mark);
                    break;
                }
            };
            let pos = self.sc.position();
            self.skip_ws_inline();
            let rhs = self.additive()?;
            lhs = Expr::Binary {
                op,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
                pos,
            };
        }
        Ok(lhs)
    }

    /// Consume the bare keyword `kw` (`and`/`or`/`not`) if it appears next
    /// in operator position (after optional whitespace), matched as a whole
    /// identifier. Restores the cursor and returns false otherwise.
    fn try_keyword(&mut self, kw: &str) -> bool {
        let mark = self.sc.mark();
        self.skip_ws_inline();
        if !matches!(self.sc.peek(), Some(c) if c.is_ascii_alphabetic()) {
            self.sc.reset(mark);
            return false;
        }
        let mut word = String::new();
        while matches!(self.sc.peek(), Some(c) if is_ident_char(c)) {
            if let Some(c) = self.sc.bump() {
                word.push(c);
            }
        }
        if word == kw {
            true
        } else {
            self.sc.reset(mark);
            false
        }
    }

    fn additive(&mut self) -> Result<Expr, Error> {
        let mut lhs = self.multiplicative()?;
        loop {
            let mark = self.sc.mark();
            let had_ws = self.skip_ws_inline();
            let op = match self.sc.peek() {
                Some('+') => Some(BinOp::Add),
                Some('-') => Some(BinOp::Sub),
                _ => None,
            };
            match op {
                Some(op) => {
                    let ws_after = matches!(self.sc.peek_at(1), Some(c) if c.is_whitespace());
                    if had_ws && ws_after {
                        let pos = self.sc.position();
                        self.sc.bump();
                        self.skip_ws_inline();
                        let rhs = self.multiplicative()?;
                        lhs = Expr::Binary {
                            op,
                            lhs: Box::new(lhs),
                            rhs: Box::new(rhs),
                            pos,
                        };
                    } else if self.calc_depth > 0 {
                        // Inside calc(), `+`/`-` must be whitespace-surrounded.
                        return Err(Error::at(
                            "\"+\" and \"-\" must be surrounded by whitespace in calculations.",
                            self.sc.position(),
                        ));
                    } else {
                        self.sc.reset(mark);
                        break;
                    }
                }
                None => {
                    self.sc.reset(mark);
                    break;
                }
            }
        }
        Ok(lhs)
    }

    fn multiplicative(&mut self) -> Result<Expr, Error> {
        let mut lhs = self.unary()?;
        loop {
            let mark = self.sc.mark();
            self.skip_ws_inline();
            let op = match self.sc.peek() {
                Some('*') => Some(BinOp::Mul),
                Some('%') => Some(BinOp::Mod),
                _ => None,
            };
            // `/` is the deprecated slash operator (handled specially), but
            // never treat `*/` or a `/` opening a comment as an operator.
            if op.is_none()
                && self.sc.peek() == Some('/')
                && self.sc.peek_at(1) != Some('/')
                && self.sc.peek_at(1) != Some('*')
            {
                let pos = self.sc.position();
                self.sc.bump();
                self.skip_ws_inline();
                let rhs = self.unary()?;
                // Inside calc() `/` is always real division; elsewhere it
                // keeps the slash spelling between number literals.
                let slash = self.calc_depth == 0 && is_slash_operand(&lhs) && is_slash_operand(&rhs);
                lhs = Expr::Div {
                    lhs: Box::new(lhs),
                    rhs: Box::new(rhs),
                    slash,
                    pos,
                };
                continue;
            }
            match op {
                Some(op) => {
                    let pos = self.sc.position();
                    self.sc.bump();
                    self.skip_ws_inline();
                    let rhs = self.unary()?;
                    lhs = Expr::Binary {
                        op,
                        lhs: Box::new(lhs),
                        rhs: Box::new(rhs),
                        pos,
                    };
                }
                None => {
                    self.sc.reset(mark);
                    break;
                }
            }
        }
        Ok(lhs)
    }

    fn unary(&mut self) -> Result<Expr, Error> {
        match self.sc.peek() {
            Some('-') => {
                if matches!(self.sc.peek_at(1), Some(c) if c.is_ascii_digit() || c == '.' || c == '$' || c == '(')
                {
                    self.sc.bump();
                    let operand = self.unary()?;
                    return Ok(Expr::Unary {
                        op: UnOp::Neg,
                        operand: Box::new(operand),
                    });
                }
            }
            Some('+') => {
                if matches!(self.sc.peek_at(1), Some(c) if c.is_ascii_digit() || c == '.' || c == '$' || c == '(')
                {
                    self.sc.bump();
                    return self.unary();
                }
            }
            _ => {}
        }
        self.primary()
    }

    fn primary(&mut self) -> Result<Expr, Error> {
        match self.sc.peek() {
            Some(c) if c.is_ascii_digit() => self.parse_number(),
            Some('.') if matches!(self.sc.peek_at(1), Some(d) if d.is_ascii_digit()) => self.parse_number(),
            Some('$') => {
                self.sc.bump();
                let name = self.read_ident_name()?;
                Ok(Expr::Var(name))
            }
            Some('#') if self.sc.peek_at(1) == Some('{') => {
                self.sc.bump();
                self.sc.bump();
                let e = self.parse_value()?;
                self.skip_ws_inline();
                if !self.sc.eat('}') {
                    return Err(Error::at("expected \"}\"", self.sc.position()));
                }
                Ok(Expr::Interp(Box::new(e)))
            }
            Some('#') => self.parse_hex(),
            Some('"') | Some('\'') => {
                let pieces = self.parse_quoted_string()?;
                Ok(Expr::QuotedString(pieces))
            }
            Some('(') => {
                self.sc.bump();
                self.skip_ws_inline();
                if self.sc.peek() == Some(')') {
                    self.sc.bump();
                    return Ok(Expr::List {
                        items: Vec::new(),
                        sep: ListSep::Space,
                    });
                }
                let e = self.parse_value()?;
                self.skip_ws_inline();
                if !self.sc.eat(')') {
                    return Err(Error::at("expected \")\"", self.sc.position()));
                }
                Ok(Expr::Paren(Box::new(e)))
            }
            Some(c) if c.is_ascii_alphabetic() || c == '-' || c == '_' => self.parse_ident_or_call(),
            Some(c) => Err(Error::at(
                format!("unexpected character {c:?} in value"),
                self.sc.position(),
            )),
            None => Err(Error::at("unexpected end of input in value", self.sc.position())),
        }
    }

    fn parse_number(&mut self) -> Result<Expr, Error> {
        let mut s = String::new();
        while matches!(self.sc.peek(), Some(c) if c.is_ascii_digit()) {
            if let Some(c) = self.sc.bump() {
                s.push(c);
            }
        }
        if self.sc.peek() == Some('.') && matches!(self.sc.peek_at(1), Some(c) if c.is_ascii_digit()) {
            if let Some(c) = self.sc.bump() {
                s.push(c);
            }
            while matches!(self.sc.peek(), Some(c) if c.is_ascii_digit()) {
                if let Some(c) = self.sc.bump() {
                    s.push(c);
                }
            }
        }
        // Scientific notation: `e`/`E` is an exponent only when followed by
        // (an optionally-signed) digit; otherwise it begins a unit (`1em`).
        if matches!(self.sc.peek(), Some('e' | 'E')) {
            let after = self.sc.peek_at(1);
            let exp_digit = matches!(after, Some(c) if c.is_ascii_digit())
                || (matches!(after, Some('+' | '-'))
                    && matches!(self.sc.peek_at(2), Some(c) if c.is_ascii_digit()));
            if exp_digit {
                if let Some(c) = self.sc.bump() {
                    s.push(c);
                }
                if matches!(self.sc.peek(), Some('+' | '-')) {
                    if let Some(c) = self.sc.bump() {
                        s.push(c);
                    }
                }
                while matches!(self.sc.peek(), Some(c) if c.is_ascii_digit()) {
                    if let Some(c) = self.sc.bump() {
                        s.push(c);
                    }
                }
            }
        }
        let value: f64 = s
            .parse()
            .map_err(|_| Error::at(format!("invalid number {s:?}"), self.sc.position()))?;
        let mut unit = String::new();
        if self.sc.peek() == Some('%') {
            self.sc.bump();
            unit.push('%');
        } else {
            while matches!(self.sc.peek(), Some(c) if c.is_ascii_alphabetic()) {
                if let Some(c) = self.sc.bump() {
                    unit.push(c);
                }
            }
        }
        Ok(Expr::Number(value, unit))
    }

    fn parse_hex(&mut self) -> Result<Expr, Error> {
        let pos = self.sc.position();
        self.sc.bump(); // '#'
        let mut hex = String::new();
        while matches!(self.sc.peek(), Some(c) if c.is_ascii_hexdigit()) {
            if let Some(c) = self.sc.bump() {
                hex.push(c);
            }
        }
        match Color::from_hex(&hex) {
            Some(c) => Ok(Expr::Color(c)),
            None => Err(Error::at(format!("invalid hex color #{hex}"), pos)),
        }
    }

    fn parse_quoted_string(&mut self) -> Result<Vec<TplPiece>, Error> {
        let q = match self.sc.bump() {
            Some(c) => c,
            None => return Err(Error::at("expected a string", self.sc.position())),
        };
        let mut pieces = Vec::new();
        let mut lit = String::new();
        loop {
            match self.sc.peek() {
                None => return Err(Error::at("unterminated string", self.sc.position())),
                Some(c) if c == q => {
                    self.sc.bump();
                    break;
                }
                Some('#') if self.sc.peek_at(1) == Some('{') => {
                    if !lit.is_empty() {
                        pieces.push(TplPiece::Lit(std::mem::take(&mut lit)));
                    }
                    self.sc.bump();
                    self.sc.bump();
                    let e = self.parse_value()?;
                    self.skip_ws_inline();
                    if !self.sc.eat('}') {
                        return Err(Error::at("expected \"}\"", self.sc.position()));
                    }
                    pieces.push(TplPiece::Interp(e));
                }
                Some('\\') => {
                    if let Some(c) = self.sc.bump() {
                        lit.push(c);
                    }
                    if let Some(c) = self.sc.bump() {
                        lit.push(c);
                    }
                }
                Some(c) => {
                    lit.push(c);
                    self.sc.bump();
                }
            }
        }
        if !lit.is_empty() {
            pieces.push(TplPiece::Lit(lit));
        }
        Ok(pieces)
    }

    fn parse_ident_or_call(&mut self) -> Result<Expr, Error> {
        let pieces = self.parse_ident_template()?;
        if pieces.len() == 1 {
            if let Some(TplPiece::Lit(name)) = pieces.first() {
                let name = name.clone();
                if self.sc.peek() == Some('(') {
                    return self.parse_call(name);
                }
                match name.as_str() {
                    "true" => return Ok(Expr::Bool(true)),
                    "false" => return Ok(Expr::Bool(false)),
                    "null" => return Ok(Expr::Null),
                    _ => {}
                }
                if let Some(color) = named_color(&name) {
                    return Ok(Expr::Color(color));
                }
            }
        }
        Ok(Expr::Ident(pieces))
    }

    fn parse_ident_template(&mut self) -> Result<Vec<TplPiece>, Error> {
        let mut pieces = Vec::new();
        let mut lit = String::new();
        loop {
            match self.sc.peek() {
                Some('#') if self.sc.peek_at(1) == Some('{') => {
                    if !lit.is_empty() {
                        pieces.push(TplPiece::Lit(std::mem::take(&mut lit)));
                    }
                    self.sc.bump();
                    self.sc.bump();
                    let e = self.parse_value()?;
                    self.skip_ws_inline();
                    if !self.sc.eat('}') {
                        return Err(Error::at("expected \"}\"", self.sc.position()));
                    }
                    pieces.push(TplPiece::Interp(e));
                }
                Some(c) if is_ident_char(c) => {
                    lit.push(c);
                    self.sc.bump();
                }
                _ => break,
            }
        }
        if !lit.is_empty() {
            pieces.push(TplPiece::Lit(lit));
        }
        Ok(pieces)
    }

    fn parse_call(&mut self, name: String) -> Result<Expr, Error> {
        let pos = self.sc.position();
        self.sc.bump(); // '('
                        // `calc()` interior is parsed as a real arithmetic
                        // expression and simplified at evaluation time.
        if name == "calc" {
            self.calc_depth += 1;
            self.skip_ws_inline();
            let inner = self.parse_value();
            self.calc_depth -= 1;
            let inner = inner?;
            self.skip_ws_inline();
            if !self.sc.eat(')') {
                return Err(Error::at("expected \")\"", self.sc.position()));
            }
            return Ok(Expr::Calc {
                inner: Box::new(inner),
            });
        }
        // CSS functions whose contents must be preserved verbatim (they may
        // contain arithmetic that is not Sass math), while still resolving
        // any `#{...}` interpolation inside them.
        if matches!(name.as_str(), "url" | "clamp" | "var" | "env" | "min" | "max") {
            let mut pieces: Vec<TplPiece> = Vec::new();
            let mut lit = format!("{name}(");
            let mut depth = 1;
            loop {
                match self.sc.peek() {
                    None => break,
                    Some('#') if self.sc.peek_at(1) == Some('{') => {
                        if !lit.is_empty() {
                            pieces.push(TplPiece::Lit(std::mem::take(&mut lit)));
                        }
                        self.sc.bump();
                        self.sc.bump();
                        let e = self.parse_value()?;
                        self.skip_ws_inline();
                        if !self.sc.eat('}') {
                            return Err(Error::at("expected \"}\"", self.sc.position()));
                        }
                        pieces.push(TplPiece::Interp(e));
                    }
                    Some('(') => {
                        depth += 1;
                        lit.push('(');
                        self.sc.bump();
                    }
                    Some(')') => {
                        depth -= 1;
                        lit.push(')');
                        self.sc.bump();
                        if depth == 0 {
                            break;
                        }
                    }
                    Some(c) => {
                        lit.push(c);
                        self.sc.bump();
                    }
                }
            }
            if !lit.is_empty() {
                pieces.push(TplPiece::Lit(lit));
            }
            return Ok(Expr::Ident(pieces));
        }
        let args = self.parse_args_after_paren()?;
        Ok(Expr::Func { name, args, pos })
    }

    /// Parse a call's argument list, assuming the opening `(` was already
    /// consumed, through the closing `)`. Args are positional or
    /// `$name: value`. Shared by function calls and `@include`.
    fn parse_args_after_paren(&mut self) -> Result<Vec<CallArg>, Error> {
        let mut args = Vec::new();
        self.skip_ws_inline();
        if self.sc.peek() != Some(')') {
            loop {
                self.skip_ws_inline();
                let mut name_opt = None;
                if self.sc.peek() == Some('$') {
                    let mark = self.sc.mark();
                    self.sc.bump();
                    let argname = self.read_ident_name()?;
                    self.skip_ws_inline();
                    if self.sc.peek() == Some(':') && self.sc.peek_at(1) != Some(':') {
                        self.sc.bump();
                        self.skip_ws_inline();
                        name_opt = Some(argname);
                    } else {
                        self.sc.reset(mark);
                    }
                }
                let value = self.space_list()?;
                args.push(CallArg {
                    name: name_opt,
                    value,
                });
                self.skip_ws_inline();
                if self.sc.eat(',') {
                    self.skip_ws_inline();
                    if self.sc.peek() == Some(')') {
                        break;
                    }
                    continue;
                }
                break;
            }
        }
        self.skip_ws_inline();
        if !self.sc.eat(')') {
            return Err(Error::at("expected \")\"", self.sc.position()));
        }
        Ok(args)
    }
}
