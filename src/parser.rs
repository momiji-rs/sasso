//! The SCSS parser: a character-level recursive-descent parser.
//!
//! SCSS is context-sensitive — a leading `:` can begin a declaration
//! value or a pseudo-class selector — so statements are disambiguated by
//! a bounded lookahead ([`Parser::classify`]) that finds whether a
//! top-level `{` (a rule) or `;`/`}` (a declaration) comes first.

use std::rc::Rc;

use crate::ast::{
    BinOp, CallArg, Callable, Conjunction, CssCustomItem, CssCustomValue, Declaration, Expr, IfBranch,
    IfClause, IfCond, ImportArg, MediaFeature, MediaInParens, MediaQuery, MediaQueryList, Param, ParamList,
    PropertySet, Rule, Stmt, Stylesheet, TplPiece, UnOp, VarDecl,
};
use crate::error::Error;
use crate::scanner::{Pos, Scanner};
use crate::value::{named_color, Color, ListSep};

enum NextKind {
    Rule,
    Declaration,
}

/// How [`Parser::parse_template_mode`] treats `/* */` (loud) and `//` (silent)
/// comments encountered while scanning a selector / prelude / property.
#[derive(Clone, Copy, PartialEq)]
enum CommentMode {
    /// Keep every comment verbatim in the literal (legacy behaviour: values
    /// and grammars that capture text exactly).
    Keep,
    /// Drop every comment, replacing it with a single space, at any nesting.
    /// dart-sass normalises selectors and structured grammars this way, so a
    /// comment behaves purely as whitespace (`a/**/b` → `a b`).
    Strip,
    /// Strip only *top-level* (outside `()`/`[]`) comments, replacing them with
    /// a single space; comments nested inside parentheses are kept verbatim.
    /// dart-sass parses `@supports` and `@-moz-document` preludes with a
    /// structured grammar that drops trivia comments between tokens while the
    /// parenthesised content is captured raw (`(a /**/ b)` is preserved).
    StripTopLevel,
    /// Unknown / interpolation-prelude directives (`@page`, `@font-face`,
    /// `@layer`, any unknown `@foo`, …): a `//` silent comment is dropped to
    /// end of line (it acts as whitespace); a `/* */` loud comment is *kept*
    /// verbatim. A leading comment is already removed by `skip_ws_inline`
    /// before the prelude template starts. Applied only at the top level;
    /// nested content is kept verbatim.
    UnknownPrelude,
}

enum MessageKind {
    Warn,
    Debug,
    Error,
}

/// Trim leading/trailing whitespace from a parsed prelude template, dropping
/// any whitespace-only literals at the ends. Interior interpolation is kept.
fn trim_prelude(pieces: Vec<TplPiece>) -> Vec<TplPiece> {
    let mut pieces = pieces;
    if let Some(TplPiece::Lit(first)) = pieces.first_mut() {
        let trimmed = first.trim_start().to_string();
        *first = trimmed;
        if first.is_empty() {
            pieces.remove(0);
        }
    }
    if let Some(TplPiece::Lit(last)) = pieces.last_mut() {
        let trimmed = last.trim_end().to_string();
        *last = trimmed;
        if last.is_empty() {
            pieces.pop();
        }
    }
    pieces
}

/// Whether a parsed media identifier template is exactly the plain keyword
/// `kw` (case-insensitively). Interpolation never matches a keyword.
fn media_ident_is(pieces: &[TplPiece], kw: &str) -> bool {
    match media_ident_plain(pieces) {
        Some(s) => s.eq_ignore_ascii_case(kw),
        None => false,
    }
}

/// The plain text of a media identifier template, or `None` if it contains
/// interpolation.
fn media_ident_plain(pieces: &[TplPiece]) -> Option<&str> {
    match pieces {
        [TplPiece::Lit(s)] => Some(s),
        [] => Some(""),
        _ => None,
    }
}

/// Whether two range comparison operators form a valid range: both must point
/// the same direction (`<`/`<=` then `<`/`<=`, or `>`/`>=` then `>`/`>=`), and
/// neither may be `=`.
fn range_ops_compatible(op1: &str, op2: &str) -> bool {
    let dir = |op: &str| match op {
        "<" | "<=" => Some(true),
        ">" | ">=" => Some(false),
        _ => None,
    };
    match (dir(op1), dir(op2)) {
        (Some(a), Some(b)) => a == b,
        _ => false,
    }
}

struct Parser {
    sc: Scanner,
    /// Depth of enclosing `calc()`/math-function contexts. Inside one, `/`
    /// is always real division (never a slash separator) and `+`/`-` must be
    /// surrounded by whitespace.
    calc_depth: u32,
    /// Set by `parse_unicode_range` when a `?`-wildcard range token is
    /// immediately followed (no whitespace) by an identifier. dart-sass treats
    /// the wildcard as terminal and starts a new space-list element with an
    /// implicit separator (`U+A?BCDE` -> `U+A? BCDE`); `space_list` consumes
    /// the flag to continue without requiring whitespace.
    pending_unicode_split: bool,
}

/// Parse a complete stylesheet.
pub(crate) fn parse(src: &str) -> Result<Stylesheet, Error> {
    let mut p = Parser {
        sc: Scanner::new(src),
        calc_depth: 0,
        pending_unicode_split: false,
    };
    let stmts = p.parse_statements(true)?;
    Ok(Stylesheet { stmts })
}

fn is_ident_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '-' || c == '_'
}

/// Whether `c` may begin a CSS identifier *without* escaping: an ASCII letter,
/// `_`, or any non-ASCII code point (matches dart-sass `isNameStart`).
fn is_name_start_codepoint(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_' || (c as u32) >= 0x80
}

/// Whether `c` may appear in an identifier body without escaping (matches
/// dart-sass `isName`): a name-start char, an ASCII digit, or `-`.
fn is_name_codepoint(c: char) -> bool {
    is_name_start_codepoint(c) || c.is_ascii_digit() || c == '-'
}

/// Append the canonical spelling of an *escaped* identifier code point to `out`,
/// matching dart-sass's `escape()`. `identifier_start` is true when this escape
/// is the first code point of the identifier (or the code point right after a
/// single leading `-`), in which case digits and `-` are escaped and only a
/// name-start char passes through literally.
fn push_ident_escape(out: &mut String, c: char, identifier_start: bool) {
    let cp = c as u32;
    let literal_ok = if identifier_start {
        is_name_start_codepoint(c)
    } else {
        is_name_codepoint(c)
    };
    if literal_ok {
        out.push(c);
    } else if cp <= 0x1F || cp == 0x7F || (identifier_start && c.is_ascii_digit()) {
        // Control characters (and a leading digit) serialize as a hex escape
        // with a trailing space, e.g. `\9 ` for a tab or `\30 ` for a leading 0.
        out.push('\\');
        out.push_str(&format!("{cp:x}"));
        out.push(' ');
    } else {
        // Other printable ASCII punctuation: a backslash followed by the literal
        // character (e.g. `\:`, `\@`, `\-`).
        out.push('\\');
        out.push(c);
    }
}

/// Classify a function name (the identifier immediately before a `(`) as a
/// "special" CSS function whose argument list must be preserved verbatim
/// rather than parsed as SassScript. Matching is case-insensitive and the
/// returned name is the dart-sass canonical (lower-cased) spelling that is
/// emitted.
///
/// `calc`, `element`, and `expression` are special with or without a single
/// vendor prefix (`-x-`). `type` is special only when *un*prefixed. `url`,
/// `var`, and `env` are handled separately (they have their own raw paths).
fn special_function_name(name: &str) -> Option<String> {
    let lower = name.to_ascii_lowercase();
    // Strip a single leading vendor prefix: `-<vendor>-`.
    let unprefixed = strip_vendor_prefix(&lower);
    match unprefixed {
        "calc" | "element" | "expression" => Some(lower),
        // `type` is special only with no vendor prefix.
        "type" if unprefixed == lower => Some(lower),
        _ => None,
    }
}

/// Strip a single leading `-<vendor>-` prefix from an already-lower-cased
/// identifier, returning the remainder. A vendor prefix is `-`, one or more
/// identifier characters, then `-` (e.g. `-webkit-`). If no such prefix is
/// present the whole string is returned unchanged.
fn strip_vendor_prefix(lower: &str) -> &str {
    let bytes = lower.as_bytes();
    if bytes.first() != Some(&b'-') {
        return lower;
    }
    // Find the second `-` after at least one inner character.
    if let Some(rel) = lower[1..].find('-') {
        if rel >= 1 {
            return &lower[1 + rel + 1..];
        }
    }
    lower
}

/// Whether `name` is a `url(` function — `url` itself or any vendor-prefixed
/// `-x-url`, case-insensitively. dart-sass parses these with its special URL
/// grammar (a plain, unquoted URL is preserved verbatim and the call is
/// emitted as a bare `url(...)`).
fn is_url_function(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    strip_vendor_prefix(&lower) == "url"
}

/// Whether `name` (the identifier immediately before a `:`) introduces an IE
/// `progid:` special function — `progid` itself or any vendor-prefixed
/// `-x-progid`, case-insensitively. dart-sass recognises a value-position
/// token of the form `[-vendor-]progid:Name(...)` and preserves its argument
/// list verbatim. The whole `[-vendor-]progid` prefix is lower-cased on emit
/// (the `Name` after the `:` keeps its original case).
fn is_progid_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    strip_vendor_prefix(&lower) == "progid"
}

/// Validate a modern `if()` condition: an evaluated `sass()` may not coexist
/// in an `and`/`or` chain with an *unparenthesised* multi-token "arbitrary
/// substitution". Parenthesising the substitution shields it (a `sass()`
/// nested in parens is NOT shielded). Returns `(sass_anywhere, direct_multi)`
/// where `sass_anywhere` is true if any `sass()` exists in the subtree
/// (descending through parens), and `direct_multi` is true if an
/// unparenthesised multi-token raw exists at this boolean level.
fn validate_if_cond(cond: &IfCond) -> Result<(bool, bool), Error> {
    let (sass, multi) = match cond {
        IfCond::Sass(_) => (true, false),
        IfCond::Raw { multi, .. } => (false, *multi),
        IfCond::Not(inner) => validate_if_cond(inner)?,
        // A paren shields a multi-token substitution from the parent level,
        // but a `sass()` inside still propagates.
        IfCond::Paren(inner) => {
            let (s, _) = validate_if_cond(inner)?;
            (s, false)
        }
        IfCond::And(items) | IfCond::Or(items) => {
            let mut sass = false;
            let mut multi = false;
            for it in items {
                let (s, m) = validate_if_cond(it)?;
                sass |= s;
                multi |= m;
            }
            (sass, multi)
        }
    };
    if sass && multi {
        return Err(Error::unpositioned(
            "if() conditions with arbitrary substitutions may not contain sass() expressions.",
        ));
    }
    Ok((sass, multi))
}

/// Whether a quoted `@import` URL (parsed into template pieces) is a *plain
/// CSS* URL — one ending in `.css`, or beginning with a protocol/`//`. Such
/// URLs are emitted verbatim rather than inlined as Sass partials. Only the
/// leading/trailing literal text is inspected (interpolated URLs are dynamic
/// Sass paths and handled elsewhere).
fn import_url_is_css(pieces: &[TplPiece]) -> bool {
    let mut head = "";
    if let Some(TplPiece::Lit(s)) = pieces.first() {
        head = s.as_str();
    }
    let mut tail = String::new();
    if let Some(TplPiece::Lit(s)) = pieces.last() {
        tail = s.clone();
    }
    tail.ends_with(".css")
        || head.starts_with("http://")
        || head.starts_with("https://")
        || head.starts_with("//")
}

/// Drop trailing whitespace-only literal pieces and trim the last literal.
fn trim_trailing_ws(mut pieces: Vec<TplPiece>) -> Vec<TplPiece> {
    if let Some(TplPiece::Lit(s)) = pieces.last_mut() {
        *s = strip_trailing_trivia(s);
    }
    while let Some(TplPiece::Lit(s)) = pieces.last() {
        if s.is_empty() {
            pieces.pop();
        } else {
            break;
        }
    }
    pieces
}

/// Strip trailing whitespace and a single trailing `/* */` or `//…` comment
/// from `s` (an `@import` modifier run). A comment in the *middle* of the run
/// (e.g. inside `b(/**/ c)`) is preserved because it is not at the very end.
fn strip_trailing_trivia(s: &str) -> String {
    let mut out = s.trim_end().to_string();
    loop {
        if out.ends_with("*/") {
            if let Some(start) = out.rfind("/*") {
                out.truncate(start);
                out = out.trim_end().to_string();
                continue;
            }
        }
        // A line comment runs to end-of-text once captured (no newline left).
        if let Some(start) = find_trailing_line_comment(&out) {
            out.truncate(start);
            out = out.trim_end().to_string();
            continue;
        }
        break;
    }
    out
}

/// If the final line of `s` begins (after optional whitespace) a `//` line
/// comment, return the byte index where the comment's whitespace run starts.
fn find_trailing_line_comment(s: &str) -> Option<usize> {
    let last_line_start = s.rfind('\n').map(|i| i + 1).unwrap_or(0);
    let line = &s[last_line_start..];
    // Find `//` that is not inside a string; the modifier run never contains a
    // bare `//` except as a comment, so a simple search suffices.
    let idx = line.find("//")?;
    Some(last_line_start + idx)
}

/// Whether `expr` is eligible to keep the deprecated `/` slash spelling.
/// dart-sass keeps the slash only between number literals (including a
/// unary-signed literal) and chains of such slash divisions; variables,
/// function calls, parentheses, and other operations force real division.
fn is_slash_operand(expr: &Expr) -> bool {
    match expr {
        Expr::Number(_, _) => true,
        // dart-sass also keeps the slash spelling when an operand is a
        // `calc()` (and the math-function family parsed as calculations):
        // `calc(1)/2` -> `1/2`, `calc(2px)/calc(4px)` -> `2px/4px`. The calc
        // operand folds to a number at eval time, so the slash repr is its
        // serialized value.
        Expr::Calc { .. } => true,
        Expr::Div { slash, .. } => *slash,
        Expr::Unary {
            op: UnOp::Neg,
            operand,
        } => is_slash_operand(operand),
        _ => false,
    }
}

/// Whether the span between a `property:` colon and the following `{` is empty
/// of any value — only whitespace and `/* */` / `//` comments. Such a span
/// makes `property: { … }` a bare nested property set (no leading value).
fn value_is_only_comments(span: &[char]) -> bool {
    let mut i = 0;
    while i < span.len() {
        let c = span[i];
        if c.is_whitespace() {
            i += 1;
        } else if c == '/' && span.get(i + 1) == Some(&'*') {
            i += 2;
            while i + 1 < span.len() && !(span[i] == '*' && span[i + 1] == '/') {
                i += 1;
            }
            i += 2;
        } else if c == '/' && span.get(i + 1) == Some(&'/') {
            while i < span.len() && span[i] != '\n' {
                i += 1;
            }
        } else {
            return false;
        }
    }
    true
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
        loop {
            match self.sc.peek() {
                Some(c) if c.is_whitespace() => {
                    self.sc.bump();
                    any = true;
                }
                // `/* ... */` loud and `// ...` silent comments act as
                // whitespace between value tokens (`c /* d */ e`, `c // d`).
                Some('/') if self.sc.peek_at(1) == Some('*') => {
                    self.sc.bump();
                    self.sc.bump();
                    while let Some(c) = self.sc.peek() {
                        if c == '*' && self.sc.peek_at(1) == Some('/') {
                            self.sc.bump();
                            self.sc.bump();
                            break;
                        }
                        self.sc.bump();
                    }
                    any = true;
                }
                Some('/') if self.sc.peek_at(1) == Some('/') => {
                    while let Some(c) = self.sc.peek() {
                        if c == '\n' {
                            break;
                        }
                        self.sc.bump();
                    }
                    any = true;
                }
                _ => break,
            }
        }
        any
    }

    /// Look ahead to decide whether the next statement is a rule (a
    /// top-level `{` comes first) or a declaration (`;`/`}` comes first),
    /// skipping over strings, comments, interpolation and bracket pairs.
    ///
    /// A top-level `{` after a `property:` is a *nested property set* (a
    /// declaration with a block) rather than a style rule when the value is
    /// empty (`b: { … }` / `b:{ … }`) or whitespace/comment immediately
    /// follows the colon (`b: c { … }`). Otherwise (`a:hover { … }`) the `{`
    /// opens a style rule — matching dart-sass declaration disambiguation.
    fn classify(&self) -> NextKind {
        let cs = self.sc.rest();
        let mut i = 0;
        let mut paren = 0i32;
        let mut bracket = 0i32;
        // Index just past the first top-level `:`, plus whether whitespace or a
        // comment immediately follows it. `None` until the colon is seen.
        let mut after_colon: Option<usize> = None;
        let mut ws_after_colon = false;
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
                ':' if paren == 0 && bracket == 0 && after_colon.is_none() => {
                    after_colon = Some(i + 1);
                    ws_after_colon = matches!(
                        cs.get(i + 1),
                        Some(c) if c.is_whitespace())
                        || matches!(
                            (cs.get(i + 1), cs.get(i + 2)),
                            (Some('/'), Some('*')) | (Some('/'), Some('/'))
                        );
                }
                '{' if paren == 0 && bracket == 0 => {
                    return match after_colon {
                        // No `property:` before this `{` — an ordinary rule.
                        None => NextKind::Rule,
                        // `property:` then a block: a nested property set if the
                        // value is empty, or whitespace/comment followed the
                        // colon; otherwise (`a:hover {`) a style rule.
                        Some(start) => {
                            let empty_value = cs[start..i].iter().all(|c| c.is_whitespace())
                                || value_is_only_comments(&cs[start..i]);
                            if empty_value || ws_after_colon {
                                NextKind::Declaration
                            } else {
                                NextKind::Rule
                            }
                        }
                    };
                }
                ';' if paren == 0 && bracket == 0 => return NextKind::Declaration,
                '}' if paren == 0 && bracket == 0 => return NextKind::Declaration,
                _ => {}
            }
            i += 1;
        }
        NextKind::Declaration
    }

    fn parse_rule(&mut self) -> Result<Stmt, Error> {
        let selector = self.parse_template_mode(&['{'], CommentMode::Strip)?;
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
        let property = self.parse_template_mode(&[':'], CommentMode::Strip)?;
        if !self.sc.eat(':') {
            return Err(Error::at("expected \":\" in declaration", self.sc.position()));
        }
        let ws_after_colon = self.skip_ws_inline();
        // Bare nested property set: `prop: { … }` / `prop:{ … }` (no value).
        if self.sc.peek() == Some('{') {
            let body = self.parse_property_set_body()?;
            return Ok(Stmt::PropertySet(PropertySet {
                property,
                value: None,
                important: false,
                body,
                pos,
            }));
        }
        let value = self.parse_value()?;
        let mut important = false;
        self.skip_ws_inline();
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
        // Value-plus-block nested property set: `prop: value [!important] { … }`.
        // Only a value separated from the colon by whitespace (or comment)
        // qualifies — `prop:value { … }` is a style rule.
        if ws_after_colon {
            let mark = self.sc.mark();
            self.skip_ws_inline();
            if self.sc.peek() == Some('{') {
                let body = self.parse_property_set_body()?;
                return Ok(Stmt::PropertySet(PropertySet {
                    property,
                    value: Some(value),
                    important,
                    body,
                    pos,
                }));
            }
            self.sc.reset(mark);
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

    /// Parse the `{ … }` block of a nested property set (the cursor is at `{`),
    /// consuming an optional trailing `;` so a following sibling parses cleanly
    /// (`b: { c: { d: e }; f: g }`).
    fn parse_property_set_body(&mut self) -> Result<Vec<Stmt>, Error> {
        self.sc.bump(); // '{'
        let body = self.parse_statements(false)?;
        if !self.sc.eat('}') {
            return Err(Error::at("expected \"}\".", self.sc.position()));
        }
        let mark = self.sc.mark();
        self.skip_ws_inline();
        if !self.sc.eat(';') {
            self.sc.reset(mark);
        }
        Ok(body)
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
            "import" => self.parse_import(pos),
            "if" => self.parse_if(),
            "for" => self.parse_for(),
            "each" => self.parse_each(),
            "while" => self.parse_while(),
            // Lowercase `@function`/`@mixin` define Sass callables — UNLESS the
            // name begins with `--`, which dart-sass reserves for plain CSS
            // custom functions/mixins (a function then passes through verbatim;
            // a mixin is a hard error).
            "function" if self.peek_callable_name_is_custom() => self.parse_css_custom_callable(name),
            "mixin" if self.peek_callable_name_is_custom() => Err(Error::at(
                "Sass @mixin names beginning with -- are forbidden for \
                     forward-compatibility with plain CSS mixins.",
                pos,
            )),
            "function" => self.parse_callable_def(true),
            "mixin" => self.parse_callable_def(false),
            "return" => self.parse_return(),
            "include" => self.parse_include(),
            "content" => {
                self.skip_ws_inline();
                self.sc.eat(';');
                Ok(Stmt::Content)
            }
            "warn" => self.parse_message(MessageKind::Warn),
            "debug" => self.parse_message(MessageKind::Debug),
            "error" => self.parse_message(MessageKind::Error),
            "at-root" => self.parse_at_root(),
            "media" => self.parse_media(),
            "keyframes" | "-webkit-keyframes" | "-moz-keyframes" | "-o-keyframes" | "-ms-keyframes" => {
                self.parse_keyframes(name)
            }
            "extend" => self.parse_extend(pos),
            // Known Sass features that are deliberately unimplemented in this
            // build: keep erroring (the generic passthrough would silently
            // accept them and lose their error specs).
            "use" | "forward" => Err(Error::at(format!("@{name} is not supported in this build"), pos)),
            // A non-lowercase spelling of `@function`/`@mixin` (e.g. `@FUNCTION`,
            // `@Mixin`) is never a Sass definition; dart-sass parses it as a
            // plain CSS custom function/mixin (verbatim body), regardless of
            // whether the name begins with `--`.
            _ if name.eq_ignore_ascii_case("function") || name.eq_ignore_ascii_case("mixin") => {
                self.parse_css_custom_callable(name)
            }
            _ => self.parse_generic_at_rule(name),
        }
    }

    /// Parse `@import <arg> [, <arg>]* ;`. Each argument is either a Sass
    /// import (a bare quoted string with no modifiers and a non-CSS URL,
    /// which is inlined) or a plain CSS `@import` (a `url(...)` URL, a `.css`/
    /// protocol URL, or a URL followed by media-query/`supports()` modifiers,
    /// which is emitted verbatim).
    fn parse_import(&mut self, pos: Pos) -> Result<Stmt, Error> {
        let mut args = Vec::new();
        loop {
            self.skip_ws_trivia();
            let arg = self.parse_import_arg(pos)?;
            args.push(arg);
            self.skip_ws_trivia();
            if self.sc.eat(',') {
                continue;
            }
            break;
        }
        self.skip_ws_trivia();
        self.sc.eat(';');
        Ok(Stmt::Import(args))
    }

    /// Skip whitespace and `/* */` / `//` comments (discarding them); used
    /// between the `@import` keyword, URLs, modifiers, and commas.
    fn skip_ws_trivia(&mut self) {
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
                    loop {
                        match self.sc.peek() {
                            None => break,
                            Some('*') if self.sc.peek_at(1) == Some('/') => {
                                self.sc.bump();
                                self.sc.bump();
                                break;
                            }
                            Some(_) => {
                                self.sc.bump();
                            }
                        }
                    }
                }
                _ => break,
            }
        }
    }

    /// Parse one `@import` argument and classify it as Sass or plain CSS.
    fn parse_import_arg(&mut self, pos: Pos) -> Result<ImportArg, Error> {
        // `url(...)` form — always a plain CSS import.
        if self.peek_is_url_func() {
            let mut tpl = self.parse_import_url_func()?;
            self.skip_ws_trivia();
            let modifiers = self.parse_import_modifiers()?;
            if !modifiers.is_empty() {
                tpl.push(TplPiece::Lit(" ".to_string()));
                tpl.extend(modifiers);
            }
            return Ok(ImportArg::Css(tpl));
        }
        // Quoted-string form.
        match self.sc.peek() {
            Some('"') | Some('\'') => {
                let mark = self.sc.mark();
                let pieces = self.parse_quoted_string()?;
                let raw_url = self.sc.slice_from(mark);
                self.skip_ws_trivia();
                let modifiers = self.parse_import_modifiers()?;
                let css_url = import_url_is_css(&pieces);
                if modifiers.is_empty() && !css_url {
                    // A bare Sass path. Reject interpolation (dynamic paths).
                    let mut path = String::new();
                    for p in &pieces {
                        match p {
                            TplPiece::Lit(s) => path.push_str(s),
                            TplPiece::Interp(_) => {
                                return Err(Error::at("dynamic @import paths are not supported", pos));
                            }
                        }
                    }
                    Ok(ImportArg::Sass(path))
                } else {
                    let mut tpl = vec![TplPiece::Lit(raw_url)];
                    if !modifiers.is_empty() {
                        tpl.push(TplPiece::Lit(" ".to_string()));
                        tpl.extend(modifiers);
                    }
                    Ok(ImportArg::Css(tpl))
                }
            }
            _ => Err(Error::at("expected a string after @import", self.sc.position())),
        }
    }

    /// Whether the cursor is at a `url(` (case-insensitive) function call.
    fn peek_is_url_func(&self) -> bool {
        let cs = self.sc.rest();
        if cs.len() < 4 {
            return false;
        }
        cs[0].eq_ignore_ascii_case(&'u')
            && cs[1].eq_ignore_ascii_case(&'r')
            && cs[2].eq_ignore_ascii_case(&'l')
            && cs[3] == '('
    }

    /// Capture a `url(...)` argument verbatim (parens may nest; quoted
    /// strings inside are passed through). Interpolation is not expanded here
    /// (none of the spec URLs use it); the run is emitted as a single literal.
    fn parse_import_url_func(&mut self) -> Result<Vec<TplPiece>, Error> {
        let mark = self.sc.mark();
        self.sc.bump(); // u
        self.sc.bump(); // r
        self.sc.bump(); // l
        self.sc.bump(); // (
        let mut depth = 1i32;
        while let Some(c) = self.sc.peek() {
            match c {
                '"' | '\'' => {
                    let q = c;
                    self.sc.bump();
                    while let Some(ch) = self.sc.peek() {
                        self.sc.bump();
                        if ch == '\\' {
                            self.sc.bump();
                            continue;
                        }
                        if ch == q {
                            break;
                        }
                    }
                }
                '(' => {
                    depth += 1;
                    self.sc.bump();
                }
                ')' => {
                    depth -= 1;
                    self.sc.bump();
                    if depth == 0 {
                        break;
                    }
                }
                _ => {
                    self.sc.bump();
                }
            }
        }
        if depth != 0 {
            return Err(Error::at("expected \")\"", self.sc.position()));
        }
        Ok(vec![TplPiece::Lit(self.sc.slice_from(mark))])
    }

    /// Parse the optional `supports(...)` and media-query-list modifiers that
    /// follow an `@import` URL, captured verbatim. Returns empty when there
    /// are none (the next char is `,`, `;`, `}`, or EOF).
    ///
    /// A media query *list* consumes following top-level commas as part of the
    /// same import. dart-sass enters list mode only when the first modifier
    /// token is a media query (a bare identifier media type, or a parenthesised
    /// `(feature)` group) — not a `supports(...)` or a `name(...)` URL-modifier
    /// function, after which a top-level comma starts a fresh import argument.
    fn parse_import_modifiers(&mut self) -> Result<Vec<TplPiece>, Error> {
        match self.sc.peek() {
            None | Some(',') | Some(';') | Some('}') => return Ok(Vec::new()),
            _ => {}
        }
        let media_list_mode = self.import_modifier_starts_media_list();
        let mut pieces = self.parse_template(&[',', ';', '}'])?;
        if media_list_mode {
            while self.sc.peek() == Some(',') {
                self.sc.bump();
                pieces.push(TplPiece::Lit(", ".to_string()));
                self.skip_ws_inline();
                let more = self.parse_template(&[',', ';', '}'])?;
                pieces.extend(more);
            }
        }
        Ok(trim_trailing_ws(pieces))
    }

    /// Whether the modifier at the cursor begins a media query list (so that a
    /// following top-level comma continues the same `@import`). True for a bare
    /// identifier not immediately followed by `(` (a media type) and for a `(`
    /// (a media feature); false for `supports(...)` and `name(...)`.
    fn import_modifier_starts_media_list(&self) -> bool {
        match self.sc.peek() {
            Some('(') => true,
            Some(c) if is_ident_char(c) && !c.is_ascii_digit() => {
                // Read the identifier and check what follows.
                let cs = self.sc.rest();
                let mut i = 0;
                while i < cs.len() && is_ident_char(cs[i]) {
                    i += 1;
                }
                let ident: String = cs[..i].iter().collect();
                if ident.eq_ignore_ascii_case("supports") {
                    return false;
                }
                // A function call (`name(`) is a URL modifier, not a media
                // type; a bare identifier is a media type.
                cs.get(i) != Some(&'(')
            }
            _ => false,
        }
    }

    /// Parse `@warn`/`@debug`/`@error <expr>;`.
    fn parse_message(&mut self, kind: MessageKind) -> Result<Stmt, Error> {
        self.skip_ws_inline();
        let value = self.parse_value()?;
        self.skip_ws_inline();
        self.sc.eat(';');
        Ok(match kind {
            MessageKind::Warn => Stmt::Warn(value),
            MessageKind::Debug => Stmt::Debug(value),
            MessageKind::Error => Stmt::Error(value),
        })
    }

    /// Parse `@extend <selector> [!optional];`. The selector is captured as a
    /// template (resolving `#{...}` at eval time); a trailing `!optional`
    /// suppresses the "didn't match" error.
    fn parse_extend(&mut self, pos: Pos) -> Result<Stmt, Error> {
        self.skip_ws_inline();
        let selector = trim_prelude(self.parse_template_mode(&['!', ';', '}', '{'], CommentMode::Strip)?);
        let mut optional = false;
        if self.sc.peek() == Some('!') {
            self.sc.bump();
            self.skip_ws_inline();
            let flag = self.read_ident_name()?;
            if !flag.eq_ignore_ascii_case("optional") {
                return Err(Error::at(format!("Invalid flag name: !{flag}"), pos));
            }
            optional = true;
        }
        self.skip_ws_inline();
        self.sc.eat(';');
        Ok(Stmt::Extend {
            selector,
            optional,
            pos,
        })
    }

    /// Parse `@at-root [query] { body }`. The optional query is the
    /// parenthesised `(with: …)` / `(without: …)` form; an inline selector
    /// (`@at-root .x { … }`) is desugared into a single rule inside the body.
    fn parse_at_root(&mut self) -> Result<Stmt, Error> {
        self.skip_ws_inline();
        let query = if self.sc.peek() == Some('(') {
            let q = self.parse_template(&['{'])?;
            Some(trim_prelude(q))
        } else if self.sc.peek() == Some('{') {
            None
        } else {
            let selector = self.parse_template(&['{'])?;
            let body = self.parse_braced_body()?;
            return Ok(Stmt::AtRoot {
                query: None,
                body: vec![Stmt::Rule(Rule { selector, body })],
            });
        };
        let body = self.parse_braced_body()?;
        Ok(Stmt::AtRoot { query, body })
    }

    /// Parse `@keyframes <name> { from {…} 50% {…} … }`. The body is parsed as
    /// ordinary statements; each frame block classifies as a rule (its keyframe
    /// selector is terminated by `{`). Parent resolution is suppressed at eval
    /// time so the frame selectors emit verbatim.
    fn parse_keyframes(&mut self, name: String) -> Result<Stmt, Error> {
        self.skip_ws_inline();
        let prelude = trim_prelude(self.parse_template(&['{'])?);
        let body = self.parse_braced_body()?;
        Ok(Stmt::Keyframes { name, prelude, body })
    }

    /// Parse a generic/unknown at-rule: `@name <prelude up to { ; or }>` then
    /// either a `{ … }` body or a terminating `;` (or an immediate `}` closing
    /// the enclosing block, as in `@supports … {@g}`). Covers `@font-face`,
    /// `@page`, `@charset`, `@supports`, vendor `@foo`, and unknown directives.
    fn parse_generic_at_rule(&mut self, name: String) -> Result<Stmt, Error> {
        self.skip_ws_inline();
        // dart-sass parses `@supports` and `@-moz-document` (only the exact
        // lowercase spellings) with structured grammars that strip trivia
        // comments between tokens; every other at-rule keeps loud comments
        // verbatim and treats silent comments as whitespace.
        let comment_mode = if name == "supports" || name == "-moz-document" {
            CommentMode::StripTopLevel
        } else {
            CommentMode::UnknownPrelude
        };
        let prelude = self.parse_template_mode(&['{', ';', '}'], comment_mode)?;
        let prelude = trim_prelude(prelude);
        self.skip_ws_inline();
        let body = if self.sc.peek() == Some('{') {
            Some(self.parse_braced_body()?)
        } else {
            self.sc.eat(';');
            None
        };
        Ok(Stmt::AtRule { name, prelude, body })
    }

    // ---- @media -----------------------------------------------------

    /// Parse `@media <media-query-list> { body }`. The query is parsed into a
    /// structured form: a comma list of queries, each a media-type form
    /// (`[not|only]? <type> (and <cond>)*`) or a condition form (one or more
    /// parenthesised conditions joined by `and`/`or`). SassScript expressions
    /// inside feature values are kept as `Expr`s for eval-time resolution.
    /// Malformed queries are rejected, matching dart-sass.
    fn parse_media(&mut self) -> Result<Stmt, Error> {
        let query = self.parse_media_query_list()?;
        let body = self.parse_braced_body()?;
        Ok(Stmt::Media { query, body })
    }

    /// Skip whitespace and `/* */` / `//` comments (allowed between media
    /// query tokens). Reports whether any whitespace or comment was consumed.
    fn skip_media_ws(&mut self) -> bool {
        let mut any = false;
        loop {
            match self.sc.peek() {
                Some(c) if c.is_whitespace() => {
                    self.sc.bump();
                    any = true;
                }
                Some('/') if self.sc.peek_at(1) == Some('*') => {
                    self.sc.bump();
                    self.sc.bump();
                    while let Some(c) = self.sc.peek() {
                        if c == '*' && self.sc.peek_at(1) == Some('/') {
                            self.sc.bump();
                            self.sc.bump();
                            break;
                        }
                        self.sc.bump();
                    }
                    any = true;
                }
                Some('/') if self.sc.peek_at(1) == Some('/') => {
                    while let Some(c) = self.sc.peek() {
                        if c == '\n' {
                            break;
                        }
                        self.sc.bump();
                    }
                    any = true;
                }
                _ => break,
            }
        }
        any
    }

    fn parse_media_query_list(&mut self) -> Result<MediaQueryList, Error> {
        let mut queries = Vec::new();
        loop {
            self.skip_media_ws();
            queries.push(self.parse_media_query()?);
            self.skip_media_ws();
            if self.sc.eat(',') {
                continue;
            }
            break;
        }
        Ok(MediaQueryList { queries })
    }

    /// Parse one media query (dart-sass `_mediaQuery`).
    fn parse_media_query(&mut self) -> Result<MediaQuery, Error> {
        // Condition-only form: a leading `(` (a media-in-parens).
        if self.sc.peek() == Some('(') {
            let first = self.parse_media_in_parens()?;
            let (conditions, conjunction) = self.parse_media_logic_sequence(first)?;
            return Ok(MediaQuery::Condition {
                conditions,
                conjunction,
            });
        }

        let ident1 = self.parse_media_identifier()?;
        // `not (...)` → condition form (only when `not` is a raw keyword and a
        // media-in-parens follows rather than another identifier). dart-sass
        // requires whitespace after `not`.
        if media_ident_is(&ident1, "not") {
            self.parse_media_keyword_whitespace()?;
            if !self.looking_at_media_identifier() {
                let inner = self.parse_media_or_interp()?;
                let first = MediaInParens::Not(Box::new(inner));
                let (conditions, conjunction) = self.parse_media_logic_sequence(first)?;
                return Ok(MediaQuery::Condition {
                    conditions,
                    conjunction,
                });
            }
        }
        self.skip_media_ws();
        if !self.looking_at_media_identifier() {
            // `@media screen { … }` — bare media type.
            return Ok(MediaQuery::Type {
                modifier: None,
                mtype: ident1,
                conditions: Vec::new(),
            });
        }
        let ident2 = self.parse_media_identifier()?;
        let (modifier, mtype) = if media_ident_is(&ident2, "and") {
            // `@media screen and …` — ident1 is the type, "and" begins the
            // condition sequence.
            (None, ident1)
        } else {
            self.skip_media_ws();
            // `@media only screen [and …]` — ident1 is the modifier.
            let modifier = media_ident_plain(&ident1).map(|s| s.to_ascii_lowercase());
            if !self.try_media_keyword("and") {
                return Ok(MediaQuery::Type {
                    modifier,
                    mtype: ident2,
                    conditions: Vec::new(),
                });
            }
            (modifier, ident2)
        };
        // We have consumed `and`; parse the condition sequence (and-only).
        let conditions = self.parse_media_and_conditions()?;
        Ok(MediaQuery::Type {
            modifier,
            mtype,
            conditions,
        })
    }

    /// After a leading condition, parse the rest of an `and`/`or` sequence.
    /// All conjunctions in one condition must match (no mixing `and`/`or`).
    fn parse_media_logic_sequence(
        &mut self,
        first: MediaInParens,
    ) -> Result<(Vec<MediaInParens>, Conjunction), Error> {
        let mut conditions = vec![first];
        let mut conjunction = Conjunction::And;
        let mut chosen: Option<Conjunction> = None;
        loop {
            let mark = self.sc.mark();
            self.skip_media_ws();
            let next = if self.try_media_keyword("and") {
                Conjunction::And
            } else if self.try_media_keyword("or") {
                Conjunction::Or
            } else {
                self.sc.reset(mark);
                break;
            };
            if let Some(prev) = chosen {
                if prev != next {
                    return Err(Error::at("expected \"{\".", self.sc.position()));
                }
            }
            chosen = Some(next);
            conjunction = next;
            self.parse_media_keyword_whitespace()?;
            conditions.push(self.parse_media_or_interp()?);
        }
        Ok((conditions, conjunction))
    }

    /// Parse one or more `and`-separated media-in-parens (used after a media
    /// type's `and`). `or` is not allowed here. The first operand may be a
    /// `not <media-in-parens>`, which terminates the query (no more conditions).
    fn parse_media_and_conditions(&mut self) -> Result<Vec<MediaInParens>, Error> {
        self.parse_media_keyword_whitespace()?;
        // `<type> and not (<feature>)` — a single negated condition, nothing
        // may follow it (matching dart-sass).
        if self.try_media_keyword("not") {
            self.parse_media_keyword_whitespace()?;
            let inner = self.parse_media_or_interp()?;
            return Ok(vec![MediaInParens::Not(Box::new(inner))]);
        }
        let mut conditions = vec![self.parse_media_or_interp()?];
        loop {
            let mark = self.sc.mark();
            self.skip_media_ws();
            if self.try_media_keyword("and") {
                self.parse_media_keyword_whitespace()?;
                conditions.push(self.parse_media_or_interp()?);
            } else if self.try_media_keyword("or") {
                // `or` after a media type's `and` chain is invalid.
                return Err(Error::at("expected \"{\".", self.sc.position()));
            } else {
                self.sc.reset(mark);
                break;
            }
        }
        Ok(conditions)
    }

    /// Parse a `_mediaOrInterp`: a raw `#{…}` interpolation operand or a single
    /// `(...)` media-in-parens.
    fn parse_media_or_interp(&mut self) -> Result<MediaInParens, Error> {
        self.skip_media_ws();
        if self.sc.peek() == Some('#') && self.sc.peek_at(1) == Some('{') {
            self.sc.bump();
            self.sc.bump();
            let e = self.parse_value()?;
            self.skip_ws_inline();
            if !self.sc.eat('}') {
                return Err(Error::at("expected \"}\"", self.sc.position()));
            }
            return Ok(MediaInParens::Interp(e));
        }
        self.parse_media_in_parens()
    }

    /// Parse a single media-in-parens (dart-sass `_mediaInParens`): `(feature)`,
    /// `(not <in-parens>)` → `not <in-parens>`, `((cond) and/or (cond)…)` group,
    /// or a raw `#{…}` interpolation operand.
    fn parse_media_in_parens(&mut self) -> Result<MediaInParens, Error> {
        self.skip_media_ws();
        // A raw interpolation operand is spliced verbatim.
        if self.sc.peek() == Some('#') && self.sc.peek_at(1) == Some('{') {
            self.sc.bump();
            self.sc.bump();
            let e = self.parse_value()?;
            self.skip_ws_inline();
            if !self.sc.eat('}') {
                return Err(Error::at("expected \"}\"", self.sc.position()));
            }
            return Ok(MediaInParens::Interp(e));
        }
        if !self.sc.eat('(') {
            return Err(Error::at(
                "expected media condition in parentheses.",
                self.sc.position(),
            ));
        }
        self.skip_media_ws();
        // `(not (...))` → `not (...)`: the wrapping parens are dropped.
        if self.media_at_not_keyword() {
            self.parse_media_identifier()?; // consume "not"
            self.parse_media_keyword_whitespace()?;
            let inner = self.parse_media_in_parens()?;
            self.skip_media_ws();
            if !self.sc.eat(')') {
                return Err(Error::at("expected \")\".", self.sc.position()));
            }
            return Ok(MediaInParens::Not(Box::new(inner)));
        }
        // Nested parens → a sub-condition group, kept wrapped.
        if self.sc.peek() == Some('(') {
            let first = self.parse_media_in_parens()?;
            let (conditions, conjunction) = self.parse_media_logic_sequence(first)?;
            self.skip_media_ws();
            if !self.sc.eat(')') {
                return Err(Error::at("expected \")\".", self.sc.position()));
            }
            return Ok(MediaInParens::Group {
                conditions,
                conjunction,
            });
        }
        // Otherwise a media feature: `<expr> [: <expr> | <op> <expr> [<op> <expr>]]`.
        let feature = self.parse_media_feature()?;
        self.skip_media_ws();
        if !self.sc.eat(')') {
            return Err(Error::at("expected \")\".", self.sc.position()));
        }
        Ok(MediaInParens::Feature(feature))
    }

    /// Whether the cursor is at a raw `not` keyword (used to start a nested
    /// `(not (...))` group).
    fn media_at_not_keyword(&self) -> bool {
        let mut i = 0;
        let mut word = String::new();
        while let Some(c) = self.sc.peek_at(i) {
            if is_ident_char(c) {
                word.push(c);
                i += 1;
            } else {
                break;
            }
        }
        word.eq_ignore_ascii_case("not")
    }

    /// Parse the interior of a single `(...)` media feature.
    fn parse_media_feature(&mut self) -> Result<MediaFeature, Error> {
        let first = self.media_expression()?;
        self.skip_media_ws();
        match self.sc.peek() {
            Some(':') => {
                self.sc.bump();
                self.skip_media_ws();
                let value = self.parse_value()?;
                Ok(MediaFeature::Decl {
                    name: first,
                    value: Some(value),
                })
            }
            Some('<') | Some('>') | Some('=') => {
                let op1 = self.parse_media_comparison()?;
                self.skip_media_ws();
                let second = self.media_expression()?;
                self.skip_media_ws();
                let rest = match self.sc.peek() {
                    Some('<') | Some('>') | Some('=') => {
                        let op2 = self.parse_media_comparison()?;
                        // A range must use a consistent direction. `=` may not
                        // be the second operator either.
                        if !range_ops_compatible(&op1, &op2) {
                            return Err(Error::at("expected \")\".", self.sc.position()));
                        }
                        self.skip_media_ws();
                        let third = self.media_expression()?;
                        self.skip_media_ws();
                        // A third comparison operator is invalid.
                        if matches!(self.sc.peek(), Some('<') | Some('>') | Some('=')) {
                            return Err(Error::at("expected \")\".", self.sc.position()));
                        }
                        Some((op2, third))
                    }
                    _ => None,
                };
                Ok(MediaFeature::Range {
                    first,
                    op1,
                    second,
                    rest,
                })
            }
            _ => Ok(MediaFeature::Decl {
                name: first,
                value: None,
            }),
        }
    }

    /// Parse a comparison operator (`<`, `<=`, `>`, `>=`, `=`). The two-char
    /// forms may not contain whitespace (`< =` is rejected).
    fn parse_media_comparison(&mut self) -> Result<String, Error> {
        match self.sc.peek() {
            Some('=') => {
                self.sc.bump();
                Ok("=".to_string())
            }
            Some('<') | Some('>') => {
                let c = self.sc.bump().unwrap_or('<');
                if self.sc.peek() == Some('=') {
                    self.sc.bump();
                    Ok(format!("{c}="))
                } else {
                    Ok(c.to_string())
                }
            }
            _ => Err(Error::at("Expected expression.", self.sc.position())),
        }
    }

    /// Parse a media-feature expression (a SassScript expression that stops
    /// before a top-level comparison operator: the additive level).
    fn media_expression(&mut self) -> Result<Expr, Error> {
        self.skip_media_ws();
        // `=` directly here (e.g. after `<` with a space) is invalid.
        if matches!(self.sc.peek(), Some('=')) {
            return Err(Error::at("Expected expression.", self.sc.position()));
        }
        self.additive()
    }

    /// Parse an identifier/template that may include interpolation, used for a
    /// media modifier or type. Stops at whitespace, `,`, `(`, `{`, or `;`.
    fn parse_media_identifier(&mut self) -> Result<Vec<TplPiece>, Error> {
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
        if pieces.is_empty() {
            return Err(Error::at("Expected identifier.", self.sc.position()));
        }
        Ok(pieces)
    }

    /// Whether the cursor is positioned at the start of a media identifier
    /// (a letter, `-`, `_`, or interpolation) — used to distinguish a type
    /// from a following condition.
    fn looking_at_media_identifier(&self) -> bool {
        match self.sc.peek() {
            Some('#') if self.sc.peek_at(1) == Some('{') => true,
            Some(c) => c.is_ascii_alphabetic() || c == '-' || c == '_',
            None => false,
        }
    }

    /// Consume the bare keyword `kw` (`and`/`or`) if it is next as a whole
    /// identifier; restores the cursor and returns false otherwise. Matched
    /// case-insensitively.
    fn try_media_keyword(&mut self, kw: &str) -> bool {
        let mark = self.sc.mark();
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
        if word.eq_ignore_ascii_case(kw) {
            true
        } else {
            self.sc.reset(mark);
            false
        }
    }

    /// After a media `and`/`or`/`not` keyword, dart-sass requires whitespace
    /// (or a comment) before the next operand: `and(b)` is an error.
    fn parse_media_keyword_whitespace(&mut self) -> Result<(), Error> {
        if !self.skip_media_ws() {
            // An interpolation operand needs no space; `(` does.
            if !(self.sc.peek() == Some('#') && self.sc.peek_at(1) == Some('{')) {
                return Err(Error::at("Expected whitespace.", self.sc.position()));
            }
        }
        Ok(())
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

    /// After a lowercase `@function`/`@mixin` keyword, peek whether the name
    /// that follows begins with `--` (a plain CSS custom function/mixin).
    fn peek_callable_name_is_custom(&self) -> bool {
        let cs = self.sc.rest();
        let mut i = 0;
        while i < cs.len() && cs[i].is_whitespace() {
            i += 1;
        }
        cs.get(i) == Some(&'-') && cs.get(i + 1) == Some(&'-')
    }

    /// Parse a plain CSS custom `@function`/`@mixin` (`keyword` is the literal
    /// at-rule name, e.g. `function`/`FUNCTION`/`mixin`). The prelude (the name,
    /// optional parameter list, and optional `returns <type>` clause) is
    /// captured as a template up to `{`; the body's top-level declarations keep
    /// their values verbatim. Only `#{...}` interpolation is resolved.
    fn parse_css_custom_callable(&mut self, keyword: String) -> Result<Stmt, Error> {
        self.skip_ws_inline();
        let prelude = trim_prelude(self.parse_template(&['{', ';', '}'])?);
        self.skip_ws_inline();
        if self.sc.peek() != Some('{') {
            // A bodyless form (`@mixin --a;`) is not valid CSS here.
            return Err(Error::at("expected \"{\".", self.sc.position()));
        }
        self.sc.bump(); // '{'
        let body = self.parse_css_custom_body()?;
        Ok(Stmt::CssCustomAtRule {
            name: keyword,
            prelude,
            body,
        })
    }

    /// Parse the `{ … }` body of a plain CSS custom `@function`/`@mixin` after
    /// the `{` has been consumed, up to and including the matching `}`. Each
    /// top-level item is a declaration `property: value`. When the property is
    /// a plain literal the value is captured verbatim (a template); when it
    /// contains interpolation the value is parsed as SassScript.
    fn parse_css_custom_body(&mut self) -> Result<Vec<CssCustomItem>, Error> {
        let mut items = Vec::new();
        loop {
            self.skip_ws_inline();
            match self.sc.peek() {
                None => return Err(Error::at("expected \"}\".", self.sc.position())),
                Some('}') => {
                    self.sc.bump();
                    break;
                }
                Some(';') => {
                    self.sc.bump();
                    continue;
                }
                _ => {}
            }
            // Property name up to `:` (interpolation allowed).
            let property = trim_prelude(self.parse_template(&[':', '{', ';', '}'])?);
            if !self.sc.eat(':') {
                return Err(Error::at("expected \":\".", self.sc.position()));
            }
            let has_interp = property.iter().any(|p| matches!(p, TplPiece::Interp(_)));
            let value = if has_interp {
                self.skip_ws_inline();
                let expr = self.parse_value()?;
                self.skip_ws_inline();
                self.sc.eat(';');
                CssCustomValue::Script(expr)
            } else {
                let raw = self.parse_css_custom_value()?;
                CssCustomValue::Raw(raw)
            };
            items.push(CssCustomItem { property, value });
        }
        Ok(items)
    }

    /// Capture a verbatim CSS custom declaration value after the `:`, up to the
    /// terminating top-level `;` or `}`. Whitespace runs collapse to a single
    /// space (matching dart-sass serialization); `#{...}` interpolation is
    /// resolved; nested `()`/`[]`/`{}` are balanced so a braced value such as
    /// `{b: c}` is captured whole. The terminating `;` is consumed; a `}` is
    /// left for the body loop.
    fn parse_css_custom_value(&mut self) -> Result<Vec<TplPiece>, Error> {
        let mut pieces: Vec<TplPiece> = Vec::new();
        let mut lit = String::new();
        let mut depth = 0i32;
        loop {
            match self.sc.peek() {
                None => break,
                Some(';') if depth == 0 => {
                    self.sc.bump();
                    break;
                }
                Some('}') if depth == 0 => break,
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
                Some(q @ ('"' | '\'')) => {
                    lit.push(q);
                    self.sc.bump();
                    while let Some(ch) = self.sc.peek() {
                        lit.push(ch);
                        self.sc.bump();
                        if ch == '\\' {
                            if let Some(esc) = self.sc.peek() {
                                lit.push(esc);
                                self.sc.bump();
                            }
                            continue;
                        }
                        if ch == q {
                            break;
                        }
                    }
                }
                Some(c) if c.is_whitespace() => {
                    while matches!(self.sc.peek(), Some(c) if c.is_whitespace()) {
                        self.sc.bump();
                    }
                    lit.push(' ');
                }
                Some(c @ ('(' | '[' | '{')) => {
                    depth += 1;
                    lit.push(c);
                    self.sc.bump();
                }
                Some(c @ (')' | ']' | '}')) => {
                    depth -= 1;
                    lit.push(c);
                    self.sc.bump();
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
                // An optional trailing comma is allowed after `$rest...`.
                self.sc.eat(',');
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
                self.skip_ws_inline();
                // A trailing comma before `)` ends the list.
                if self.sc.peek() == Some(')') {
                    break;
                }
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
        self.parse_template_mode(stops, CommentMode::Keep)
    }

    /// Consume the `/* ... */` loud comment at the cursor (the leading `/*`
    /// must already be confirmed by the caller) and return its inner text
    /// including the surrounding delimiters, i.e. the full `/* ... */`.
    fn consume_loud_comment(&mut self) -> String {
        let mut s = String::from("/*");
        self.sc.bump();
        self.sc.bump();
        loop {
            match self.sc.peek() {
                None => break,
                Some('*') if self.sc.peek_at(1) == Some('/') => {
                    self.sc.bump();
                    self.sc.bump();
                    s.push_str("*/");
                    break;
                }
                Some(c) => {
                    s.push(c);
                    self.sc.bump();
                }
            }
        }
        s
    }

    /// Consume the `// ...` silent comment at the cursor up to (but not
    /// including) the newline; the leading `//` must already be confirmed.
    fn consume_silent_comment(&mut self) {
        while let Some(c) = self.sc.peek() {
            if c == '\n' {
                break;
            }
            self.sc.bump();
        }
    }

    fn parse_template_mode(&mut self, stops: &[char], comments: CommentMode) -> Result<Vec<TplPiece>, Error> {
        let mut pieces = Vec::new();
        let mut lit = String::new();
        let mut paren = 0i32;
        let mut bracket = 0i32;
        while let Some(c) = self.sc.peek() {
            if paren == 0 && bracket == 0 && stops.contains(&c) {
                break;
            }
            // Comment handling depends on the mode and nesting depth.
            if c == '/' && comments != CommentMode::Keep {
                let top = paren == 0 && bracket == 0;
                let strip_here = match comments {
                    CommentMode::Strip => true,
                    CommentMode::StripTopLevel | CommentMode::UnknownPrelude => top,
                    CommentMode::Keep => false,
                };
                if strip_here {
                    match self.sc.peek_at(1) {
                        Some('*') => {
                            // A loud comment is kept verbatim for unknown
                            // preludes (dart-sass preserves it); otherwise it
                            // collapses to whitespace.
                            if comments == CommentMode::UnknownPrelude {
                                let text = self.consume_loud_comment();
                                lit.push_str(&text);
                            } else {
                                let _ = self.consume_loud_comment();
                                lit.push(' ');
                            }
                            continue;
                        }
                        Some('/') => {
                            // A silent comment always acts as whitespace. For
                            // unknown preludes nothing is inserted (the trailing
                            // newline left in the source provides the gap and
                            // edges are trimmed); otherwise a single space.
                            self.consume_silent_comment();
                            if comments != CommentMode::UnknownPrelude {
                                lit.push(' ');
                            }
                            continue;
                        }
                        _ => {}
                    }
                }
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

    /// Consume a CSS escape sequence. The opening `\` must be the next
    /// character; it is consumed here. Returns the decoded code point, or `None`
    /// for a line continuation (`\` immediately before a newline), which yields
    /// no character. A backslash at end-of-input decodes to U+FFFD, matching
    /// dart-sass. Errors on an out-of-range Unicode code point.
    fn consume_escape(&mut self) -> Result<Option<char>, Error> {
        let pos = self.sc.position();
        self.sc.bump(); // the leading backslash
        match self.sc.peek() {
            // `\` before a CSS newline is a line continuation: the pair is
            // dropped entirely.
            Some('\n') => {
                self.sc.bump();
                Ok(None)
            }
            Some('\r') => {
                self.sc.bump();
                self.sc.eat('\n'); // CRLF
                Ok(None)
            }
            Some('\u{c}') => {
                self.sc.bump();
                Ok(None)
            }
            Some(c) if c.is_ascii_hexdigit() => {
                let mut value: u32 = 0;
                let mut digits = 0;
                while digits < 6 {
                    match self.sc.peek() {
                        Some(h) if h.is_ascii_hexdigit() => {
                            value = value * 16 + h.to_digit(16).unwrap_or(0);
                            self.sc.bump();
                            digits += 1;
                        }
                        _ => break,
                    }
                }
                // A single trailing whitespace character terminates the escape
                // and is consumed.
                match self.sc.peek() {
                    Some(' ' | '\t' | '\n' | '\u{c}') => {
                        self.sc.bump();
                    }
                    Some('\r') => {
                        self.sc.bump();
                        self.sc.eat('\n');
                    }
                    _ => {}
                }
                if value > 0x10_FFFF {
                    return Err(Error::at("Invalid Unicode code point.", pos));
                }
                // Surrogate code points cannot be represented and become the
                // replacement char; NUL is kept (it serializes as `\0 `).
                match char::from_u32(value) {
                    Some(ch) => Ok(Some(ch)),
                    None => Ok(Some('\u{FFFD}')),
                }
            }
            // Any other character escapes to itself literally.
            Some(c) => {
                self.sc.bump();
                Ok(Some(c))
            }
            None => Ok(Some('\u{FFFD}')),
        }
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
                bracketed: false,
            })
        }
    }

    fn space_list(&mut self) -> Result<Expr, Error> {
        let first = self.or_expr()?;
        let mut rest = Vec::new();
        loop {
            // A `?`-wildcard unicode-range token immediately followed by an
            // identifier inserts an implicit space separator (`U+A?BCDE` ->
            // `U+A? BCDE`), so continue without consuming whitespace.
            if std::mem::take(&mut self.pending_unicode_split) {
                rest.push(self.or_expr()?);
                continue;
            }
            let mark = self.sc.mark();
            let had_ws = self.skip_ws_inline();
            // A lone `=` (not `==`) ends the space-list so an enclosing
            // argument list can apply the single-`=` Microsoft-filter operator
            // (`foo(a = b)`); `==` stays the equality operator, parsed above.
            if !had_ws
                || self.at_value_terminator()
                || (self.sc.peek() == Some('=') && self.sc.peek_at(1) != Some('='))
            {
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
                bracketed: false,
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
                    // Whitespace OR a comment (`/* */`, `//`) immediately
                    // after the operator counts as separation, matching
                    // dart-sass's `1 /**/+/**/ 2` handling.
                    let ws_after = matches!(self.sc.peek_at(1), Some(c) if c.is_whitespace())
                        || (self.sc.peek_at(1) == Some('/')
                            && matches!(self.sc.peek_at(2), Some('*') | Some('/')));
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
                // `-` directly before a number/paren/variable is numeric
                // negation (`-5`, `-(1)`, `-$x`); when separated by whitespace
                // it is the unary-minus operator over any value (`- red` ->
                // `-red`). A `-` immediately followed by an identifier char is
                // instead part of an identifier (`-webkit-foo`, `-red`) and
                // falls through to `primary`.
                if matches!(self.sc.peek_at(1), Some(c) if c.is_ascii_digit() || c == '.' || c == '$' || c == '(')
                {
                    self.sc.bump();
                    let operand = self.unary()?;
                    return Ok(Expr::Unary {
                        op: UnOp::Neg,
                        operand: Box::new(operand),
                    });
                }
                if matches!(self.sc.peek_at(1), Some(c) if c.is_whitespace()) {
                    self.sc.bump();
                    self.skip_ws_inline();
                    let operand = self.unary()?;
                    return Ok(Expr::Unary {
                        op: UnOp::Neg,
                        operand: Box::new(operand),
                    });
                }
            }
            Some('+') => {
                // `+` is never part of an identifier, so a leading `+` in value
                // position is always the unary-plus operator (numeric identity
                // for a number, otherwise an unquoted `+<value>` string).
                // Optional whitespace separates it from its operand. A `+` with
                // no following operand falls through to `primary` (which reports
                // the error).
                let next = self.sc.peek_at(1);
                let starts_operand = matches!(next, Some(c)
                    if c.is_ascii_digit()
                        || c == '.'
                        || c == '$'
                        || c == '('
                        || c == '"'
                        || c == '\''
                        || c == '#'
                        || c == '+'
                        || c == '-'
                        || is_ident_char(c));
                if starts_operand || matches!(next, Some(c) if c.is_whitespace()) {
                    self.sc.bump();
                    self.skip_ws_inline();
                    let operand = self.unary()?;
                    return Ok(Expr::Unary {
                        op: UnOp::Plus,
                        operand: Box::new(operand),
                    });
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
                        bracketed: false,
                    });
                }
                // Parse the first sub-expression at the space-list level. A
                // following `:` makes this a map literal `(k: v, …)`; anything
                // else is an ordinary parenthesised expression / list.
                let first = self.space_list()?;
                self.skip_ws_inline();
                if self.sc.peek() == Some(':') && self.sc.peek_at(1) != Some(':') {
                    return self.parse_map_after_first_key(first);
                }
                let e = self.finish_paren_list(first)?;
                self.skip_ws_inline();
                if !self.sc.eat(')') {
                    return Err(Error::at("expected \")\"", self.sc.position()));
                }
                Ok(Expr::Paren(Box::new(e)))
            }
            Some('[') => self.parse_bracketed_list(),
            Some('&') => {
                self.sc.bump();
                Ok(Expr::Parent)
            }
            // CSS unicode-range token: `u`/`U` immediately followed by `+`
            // (no whitespace) commits to the unicode-range grammar, matching
            // dart-sass. `u + 1` (with whitespace) is ordinary concatenation
            // and falls through to the identifier branch below.
            Some('u') | Some('U') if self.sc.peek_at(1) == Some('+') => self.parse_unicode_range(),
            Some(c) if c.is_ascii_alphabetic() || c == '-' || c == '_' => self.parse_ident_or_call(),
            // A value beginning with a CSS escape (`\41`, `\9`, …) is an
            // identifier; let `parse_ident_or_call` consume the escape run.
            Some('\\') => self.parse_ident_or_call(),
            // A lone `%` in value position (no left operand, so it is not the
            // modulo operator) is a standalone unquoted-string token, as in
            // dart-sass: `attr(c, %)` keeps the `%`, and `%foo` parses as the
            // two space-list elements `%` and `foo`. Only a single `%` is
            // consumed here.
            Some('%') => {
                self.sc.bump();
                Ok(Expr::Ident(vec![TplPiece::Lit("%".to_string())]))
            }
            Some(c) => Err(Error::at(
                format!("unexpected character {c:?} in value"),
                self.sc.position(),
            )),
            None => Err(Error::at("unexpected end of input in value", self.sc.position())),
        }
    }

    /// Parse a CSS unicode-range token (`U+1A2B`, `U+4??`, `U+0-7F`, …). The
    /// leading `u`/`U` and `+` are not yet consumed; the caller guarantees the
    /// next two characters are `[uU]` and `+`. The original case is preserved
    /// verbatim and the token is lowered to an unquoted [`Expr::Ident`].
    ///
    /// Grammar (mirrors dart-sass `_unicodeRange`):
    ///   - 1–6 hex digits, optionally followed by `?` wildcards (total ≤ 6);
    ///   - if no `?` was seen, an optional `-` then 1–6 hex end digits;
    ///   - the token must end at a non-identifier boundary.
    fn parse_unicode_range(&mut self) -> Result<Expr, Error> {
        let start = self.sc.position();
        let mut s = String::new();
        if let Some(c) = self.sc.bump() {
            s.push(c); // `u` / `U`
        }
        if let Some(c) = self.sc.bump() {
            s.push(c); // `+`
        }
        // First range: up to 6 hex digits, then `?` wildcards (total ≤ 6).
        let mut count = 0usize;
        while count < 6 && matches!(self.sc.peek(), Some(c) if c.is_ascii_hexdigit()) {
            if let Some(c) = self.sc.bump() {
                s.push(c);
            }
            count += 1;
        }
        let mut saw_question = false;
        while count < 6 && self.sc.peek() == Some('?') {
            self.sc.bump();
            s.push('?');
            saw_question = true;
            count += 1;
        }
        if count == 0 {
            return Err(Error::at("Expected hex digit or \"?\".", self.sc.position()));
        }
        // After a wildcard form, no `-end` range is allowed.
        if !saw_question && self.sc.peek() == Some('-') {
            self.sc.bump();
            s.push('-');
            let mut end_count = 0usize;
            while end_count < 6 && matches!(self.sc.peek(), Some(c) if c.is_ascii_hexdigit()) {
                if let Some(c) = self.sc.bump() {
                    s.push(c);
                }
                end_count += 1;
            }
            if end_count == 0 {
                return Err(Error::at("Expected hex digit.", self.sc.position()));
            }
        }
        // When the 6-digit cap is reached but more hex digits or `?`
        // wildcards immediately follow, dart-sass reports "Expected at most 6
        // digits." (spanning the whole token). Below the cap the token simply
        // ends and any following identifier becomes the next list element.
        if count == 6 && matches!(self.sc.peek(), Some(c) if c.is_ascii_hexdigit() || c == '?') {
            return Err(Error::at("Expected at most 6 digits.", start));
        }
        let token = Expr::Ident(vec![TplPiece::Lit(s)]);
        if !saw_question {
            // A plain or `-end` range is terminal: any directly-following
            // identifier char (a stray `-name` chain like `U+123-456-ABC`, or
            // a non-hex letter like `U+1234GH`) is "Expected end of
            // identifier." in dart-sass.
            if matches!(self.sc.peek(), Some(c) if is_ident_char(c))
                || (self.sc.peek() == Some('#') && self.sc.peek_at(1) == Some('{'))
            {
                return Err(Error::at("Expected end of identifier.", self.sc.position()));
            }
            return Ok(token);
        }
        // After a `?` wildcard form, dart-sass does not allow a `-end` range,
        // so the wildcard token is terminal. A directly-following `-<digit>`
        // (no whitespace) continues as a subtraction whose unquoted-string
        // join yields `U+A?-1234`. A directly-following identifier
        // (`U+A?BCDE`, `U+A?-BCDE`) instead begins a fresh space-list element
        // with an implicit separator, handled by `space_list`.
        if self.sc.peek() == Some('-')
            && matches!(self.sc.peek_at(1), Some(c) if c.is_ascii_digit() || c == '.')
        {
            let pos = self.sc.position();
            self.sc.bump(); // '-'
            let rhs = self.multiplicative()?;
            return Ok(Expr::Binary {
                op: BinOp::Sub,
                lhs: Box::new(token),
                rhs: Box::new(rhs),
                pos,
            });
        }
        if matches!(self.sc.peek(), Some(c) if is_ident_char(c) || c == '.')
            || (self.sc.peek() == Some('#') && self.sc.peek_at(1) == Some('{'))
        {
            // Signal `space_list` to continue without requiring whitespace.
            self.pending_unicode_split = true;
        }
        Ok(token)
    }

    /// Continue a comma list whose first element (`first`) has already been
    /// parsed at the space-list level, stopping before the closing `)`. Used
    /// when the leading element of a parenthesised group turned out not to be
    /// a map key. With no following commas this is just `first`.
    fn finish_paren_list(&mut self, first: Expr) -> Result<Expr, Error> {
        let mut rest = Vec::new();
        let mut trailing = false;
        loop {
            let mark = self.sc.mark();
            self.skip_ws_inline();
            if self.sc.peek() == Some(',') {
                self.sc.bump();
                self.skip_ws_inline();
                if self.sc.peek() == Some(')') {
                    trailing = true;
                    break;
                }
                rest.push(self.space_list()?);
            } else {
                self.sc.reset(mark);
                break;
            }
        }
        if rest.is_empty() && !trailing {
            return Ok(first);
        }
        let mut items = Vec::with_capacity(rest.len() + 1);
        items.push(first);
        items.extend(rest);
        Ok(Expr::List {
            items,
            sep: ListSep::Comma,
            bracketed: false,
        })
    }

    /// Parse the remaining entries of a map literal after the first key was
    /// parsed and the `:` separator confirmed (but not yet consumed). Each
    /// entry is `key: value` at the space-list level, comma-separated, with an
    /// optional trailing comma before `)`.
    fn parse_map_after_first_key(&mut self, first_key: Expr) -> Result<Expr, Error> {
        let mut entries = Vec::new();
        // Consume the `:` for the first entry and read its value.
        self.sc.bump();
        self.skip_ws_inline();
        let first_val = self.space_list()?;
        entries.push((first_key, first_val));
        loop {
            self.skip_ws_inline();
            if !self.sc.eat(',') {
                break;
            }
            self.skip_ws_inline();
            if self.sc.peek() == Some(')') {
                break; // trailing comma
            }
            let key = self.space_list()?;
            self.skip_ws_inline();
            if !(self.sc.peek() == Some(':') && self.sc.peek_at(1) != Some(':')) {
                return Err(Error::at("expected \":\".", self.sc.position()));
            }
            self.sc.bump();
            self.skip_ws_inline();
            let val = self.space_list()?;
            entries.push((key, val));
        }
        self.skip_ws_inline();
        if !self.sc.eat(')') {
            return Err(Error::at("expected \")\"", self.sc.position()));
        }
        Ok(Expr::Map(entries))
    }

    /// Parse a bracketed list literal `[ ... ]`. An empty `[]` is a bracketed
    /// empty space list; otherwise the interior is parsed as an ordinary value
    /// (which may itself be a comma/space list) and re-marked as bracketed.
    fn parse_bracketed_list(&mut self) -> Result<Expr, Error> {
        self.sc.bump(); // '['
        self.skip_ws_inline();
        if self.sc.peek() == Some(']') {
            self.sc.bump();
            return Ok(Expr::List {
                items: Vec::new(),
                sep: ListSep::Space,
                bracketed: true,
            });
        }
        let inner = self.parse_value()?;
        self.skip_ws_inline();
        if !self.sc.eat(']') {
            return Err(Error::at("expected \"]\"", self.sc.position()));
        }
        // An *unbracketed* list interior (the comma/space list produced by
        // parsing several elements) keeps its separator and is simply marked
        // bracketed. A scalar — or a single nested bracketed list like
        // `[[c]]` — becomes a one-item bracketed list instead of being
        // unwrapped.
        match inner {
            Expr::List {
                items,
                sep,
                bracketed: false,
            } => Ok(Expr::List {
                items,
                sep,
                bracketed: true,
            }),
            other => Ok(Expr::List {
                items: vec![other],
                sep: ListSep::Space,
                bracketed: true,
            }),
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
                    // Decode the escape to its code point and store it raw; the
                    // string serializer re-escapes only what it must. A line
                    // continuation (`\` before a CSS newline) yields no
                    // character. `\#{...}` decodes the `#` literally, so the
                    // sequence becomes a plain `#{` rather than interpolation.
                    // Inside a quoted string a NUL escape becomes the Unicode
                    // replacement character (unlike an identifier, where it
                    // serializes as `\0 `).
                    if let Some(c) = self.consume_escape()? {
                        lit.push(if c == '\0' { '\u{FFFD}' } else { c });
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
                // IE `progid:` special function: `[-vendor-]progid:Name(...)`.
                // The identifier `progid` (or a vendor-prefixed `-x-progid`) is
                // recognised only when immediately followed by a `:`. The
                // argument list (and any further `.Name` chain) is preserved
                // verbatim with only `#{...}` interpolation resolved.
                if self.sc.peek() == Some(':') && is_progid_name(&name) {
                    return self.parse_progid(&name);
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
        // `emitted` counts code points written to the identifier so far (across
        // both literal and interpolation pieces) so the leading-digit / first
        // -char escaping rules can be applied. `first_hyphen` records whether
        // the identifier begins with `-` (a digit right after it is escaped).
        let mut emitted = 0usize;
        let mut first_hyphen = false;
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
                    emitted += 1;
                }
                Some('\\') => {
                    if let Some(c) = self.consume_escape()? {
                        // The escape is at "identifier start" when it is the very
                        // first code point, or the code point right after a single
                        // leading literal/escaped `-`.
                        let identifier_start = emitted == 0 || (emitted == 1 && first_hyphen);
                        push_ident_escape(&mut lit, c, identifier_start);
                        emitted += 1;
                    }
                }
                Some(c) if is_ident_char(c) => {
                    lit.push(c);
                    self.sc.bump();
                    if emitted == 0 && c == '-' {
                        first_hyphen = true;
                    }
                    emitted += 1;
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
                        // expression and simplified at evaluation time. The
                        // name is matched case-insensitively (`CaLc(1px)` ->
                        // `1px`); a vendor-prefixed `-webkit-calc(…)` does not
                        // match and stays a verbatim special function.
        if name.eq_ignore_ascii_case("calc") {
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
        // any `#{...}` interpolation inside them. `min`/`max`/`clamp` are
        // NOT here: they route to the math builtins, which evaluate their
        // arguments as Sass values and reduce when every argument is a
        // compatible-unit number, otherwise fall back to a preserved CSS
        // `min()`/`max()`/`clamp()` form (so `min(1px, 2vw)` round-trips).
        // A `url(` function (case-insensitively, with an optional vendor
        // prefix): dart-sass tries to read a plain unquoted URL verbatim. If
        // that succeeds the call is emitted as a bare lower-cased `url(...)`
        // (the vendor prefix is dropped); if the contents contain SassScript
        // (a `$variable`), the trial fails and the call is parsed as an
        // ordinary function so its arguments evaluate (keeping the original
        // name, e.g. `-e-url($a)` -> `-e-url(b)`).
        if is_url_function(&name) {
            let mark = self.sc.mark();
            if let Some(pieces) = self.try_plain_url_contents()? {
                return Ok(Expr::Ident(pieces));
            }
            self.sc.reset(mark);
            let args = self.parse_args_after_paren()?;
            return Ok(Expr::Func { name, args, pos });
        }
        if matches!(name.as_str(), "var" | "env") {
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
        // Special CSS functions (`calc()`, `element()`, `expression()` with or
        // without a vendor prefix; `type()` unprefixed) preserve their
        // arguments verbatim — they may contain `%`, `@`, `=`, IE-hack syntax,
        // comments, and other non-SassScript characters. Only `#{...}`
        // interpolation is resolved; the function name is lower-cased.
        if let Some(canonical) = special_function_name(&name) {
            return self.parse_special_function(&canonical);
        }
        // The modern CSS `if()` uses `:`/`;` clause syntax and `css()`/
        // `sass()` wrappers, which the comma-arg parser cannot handle. Try
        // the modern grammar first; if it does not match, reset and fall
        // back to the legacy `if($cond, $t, $f)` builtin.
        if name == "if" {
            let mark = self.sc.mark();
            match self.try_parse_modern_if() {
                Ok(Some(clauses)) => return Ok(Expr::ModernIf(clauses)),
                Ok(None) => self.sc.reset(mark),
                Err(_) => self.sc.reset(mark),
            }
        }
        let args = self.parse_args_after_paren()?;
        Ok(Expr::Func { name, args, pos })
    }

    /// Capture the argument list of a special CSS function verbatim after the
    /// opening `(` has been consumed. `canonical` is the lower-cased function
    /// name to emit. The contents are preserved literally except that:
    ///   - `#{...}` interpolation is resolved;
    ///   - runs of whitespace collapse to a single space;
    ///   - silent `//` comments are dropped (the whitespace around them stays);
    ///   - loud `/* */` comments, quoted strings, and all other characters
    ///     (`%`, `@`, `=`, punctuation, …) are emitted verbatim.
    ///
    /// Nested parentheses are balanced.
    fn parse_special_function(&mut self, canonical: &str) -> Result<Expr, Error> {
        let pieces: Vec<TplPiece> = Vec::new();
        let lit = format!("{canonical}(");
        self.capture_verbatim_args(pieces, lit)
    }

    /// Parse an IE `progid:` special function whose `[-vendor-]progid` prefix
    /// (passed as `name`, with the `:` not yet consumed) has been recognised.
    /// The prefix is lower-cased on emit; a `.`-separated chain of ASCII-letter
    /// name segments follows the `:` (kept verbatim, case preserved), then an
    /// opening `(` is required and the argument list is captured verbatim
    /// (resolving only `#{...}` interpolation). Mirrors dart-sass's `progid`
    /// branch of `_trySpecialFunction`.
    fn parse_progid(&mut self, name: &str) -> Result<Expr, Error> {
        let mut lit = name.to_ascii_lowercase();
        self.sc.bump(); // ':'
        lit.push(':');
        // The name chain: ASCII letters and `.` only (e.g.
        // `DXImageTransform.Microsoft.gradient`). Anything else stops the chain
        // and the required `(` check below produces dart-sass's error.
        while let Some(c) = self.sc.peek() {
            if c.is_ascii_alphabetic() || c == '.' {
                lit.push(c);
                self.sc.bump();
            } else {
                break;
            }
        }
        if !self.sc.eat('(') {
            return Err(Error::at("expected \"(\".", self.sc.position()));
        }
        lit.push('(');
        self.capture_verbatim_args(Vec::new(), lit)
    }

    /// Continue capturing a verbatim special-function argument list, given the
    /// already-accumulated template `pieces`/`lit` (which must end with the
    /// opening `(` that has just been consumed). The closing `)` is consumed.
    /// Used by both the named special functions (`calc()`, `element()`, …) and
    /// the IE `progid:` form, which share dart-sass's interpolated-declaration
    /// value grammar.
    fn capture_verbatim_args(&mut self, mut pieces: Vec<TplPiece>, mut lit: String) -> Result<Expr, Error> {
        let mut depth = 1i32;
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
                // Quoted strings: copy verbatim (parens inside do not nest).
                Some(q @ ('"' | '\'')) => {
                    lit.push(q);
                    self.sc.bump();
                    while let Some(ch) = self.sc.peek() {
                        lit.push(ch);
                        self.sc.bump();
                        if ch == '\\' {
                            if let Some(esc) = self.sc.peek() {
                                lit.push(esc);
                                self.sc.bump();
                            }
                            continue;
                        }
                        if ch == q {
                            break;
                        }
                    }
                }
                // Loud comment: emit verbatim.
                Some('/') if self.sc.peek_at(1) == Some('*') => {
                    lit.push('/');
                    lit.push('*');
                    self.sc.bump();
                    self.sc.bump();
                    loop {
                        match self.sc.peek() {
                            None => break,
                            Some('*') if self.sc.peek_at(1) == Some('/') => {
                                lit.push('*');
                                lit.push('/');
                                self.sc.bump();
                                self.sc.bump();
                                break;
                            }
                            Some(c) => {
                                lit.push(c);
                                self.sc.bump();
                            }
                        }
                    }
                }
                // Silent comment: drop it (surrounding whitespace is kept).
                Some('/') if self.sc.peek_at(1) == Some('/') => {
                    while let Some(c) = self.sc.peek() {
                        if c == '\n' {
                            break;
                        }
                        self.sc.bump();
                    }
                }
                // A backslash escapes the next character: both are emitted
                // verbatim, and the escaped character (e.g. `\(` or `\)`) does
                // NOT affect parenthesis nesting. Mirrors dart-sass's
                // `_interpolatedDeclarationValue` escape handling.
                Some('\\') => {
                    lit.push('\\');
                    self.sc.bump();
                    if let Some(c) = self.sc.bump() {
                        lit.push(c);
                    }
                }
                // Whitespace run collapses to a single space.
                Some(c) if c.is_whitespace() => {
                    while matches!(self.sc.peek(), Some(c) if c.is_whitespace()) {
                        self.sc.bump();
                    }
                    lit.push(' ');
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
        if depth != 0 {
            return Err(Error::at("expected \")\"", self.sc.position()));
        }
        if !lit.is_empty() {
            pieces.push(TplPiece::Lit(lit));
        }
        Ok(Expr::Ident(pieces))
    }

    /// Try to read the contents of a `url(` as a plain unquoted URL (the `(`
    /// is already consumed). Returns `Some(pieces)` emitting a canonical
    /// `url(<contents>)` (vendor prefix dropped, name lower-cased) when the
    /// contents are a plain URL — arbitrary url-safe characters plus `#{...}`
    /// interpolation, balanced parentheses, and quoted strings. Returns `None`
    /// (without committing the cursor — the caller resets) when a top-level
    /// `$variable` appears, signalling that the call must instead be parsed as
    /// an ordinary function so its arguments evaluate.
    fn try_plain_url_contents(&mut self) -> Result<Option<Vec<TplPiece>>, Error> {
        let mut pieces: Vec<TplPiece> = Vec::new();
        let mut lit = String::from("url(");
        let mut depth = 1i32;
        loop {
            match self.sc.peek() {
                None => return Ok(None),
                // A top-level `$variable` is SassScript, not a plain URL.
                Some('$') => return Ok(None),
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
                // A quoted string: copy verbatim but still resolve any `#{...}`
                // interpolation inside it (matching dart-sass, which evaluates
                // interpolation within URL string contents).
                Some(q @ ('"' | '\'')) => {
                    lit.push(q);
                    self.sc.bump();
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
                            Some('\\') => {
                                if let Some(c) = self.sc.bump() {
                                    lit.push(c);
                                }
                                if let Some(c) = self.sc.bump() {
                                    lit.push(c);
                                }
                            }
                            Some(ch) => {
                                lit.push(ch);
                                self.sc.bump();
                                if ch == q {
                                    break;
                                }
                            }
                        }
                    }
                }
                // A CSS escape in unquoted URL contents is decoded and
                // re-serialized with the identifier rules (always in body
                // position, so a leading digit or `-` stays literal). This also
                // makes `\#{}` a literal `#{}` rather than interpolation.
                Some('\\') => {
                    if let Some(c) = self.consume_escape()? {
                        push_ident_escape(&mut lit, c, false);
                    }
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
        if depth != 0 {
            return Ok(None);
        }
        if !lit.is_empty() {
            pieces.push(TplPiece::Lit(lit));
        }
        Ok(Some(pieces))
    }

    /// Attempt to parse the modern `if()` grammar after the opening `(` was
    /// consumed. Returns `Ok(Some(_))` if the input matches the modern
    /// clause syntax (consuming through the closing `)`), `Ok(None)` if it
    /// is clearly the legacy comma-arg form, or `Err` on a genuine modern
    /// syntax error (which surfaces to the caller as a hard error after the
    /// trial succeeds in committing to modern).
    fn try_parse_modern_if(&mut self) -> Result<Option<Vec<IfClause>>, Error> {
        self.skip_ws_inline();
        // Recognise the first clause to decide between modern and legacy.
        // A modern clause starts with `else` or a condition atom; if the
        // first token cannot begin a condition, this is the legacy form.
        let first = match self.parse_if_clause(true)? {
            Some(c) => c,
            None => return Ok(None),
        };
        let mut clauses = vec![first];
        loop {
            self.skip_ws_inline();
            if self.sc.eat(';') {
                self.skip_ws_inline();
                if self.sc.peek() == Some(')') {
                    break; // trailing semicolon
                }
                match self.parse_if_clause(false)? {
                    Some(c) => clauses.push(c),
                    None => return Err(Error::at("Expected identifier.", self.sc.position())),
                }
                continue;
            }
            break;
        }
        self.skip_ws_inline();
        if !self.sc.eat(')') {
            return Err(Error::at("expected \")\".", self.sc.position()));
        }
        Ok(Some(clauses))
    }

    /// Parse one `if()` clause: `<condition>: <value>` or `else: <value>`.
    /// When `probe` is true a failure to recognise the leading condition
    /// returns `Ok(None)` (signalling a fall-back to legacy) rather than an
    /// error.
    fn parse_if_clause(&mut self, probe: bool) -> Result<Option<IfClause>, Error> {
        self.skip_ws_inline();
        // `else` clause: bare keyword, no condition.
        let else_mark = self.sc.mark();
        if self.try_keyword("else") {
            self.skip_ws_inline();
            if self.sc.peek() == Some(':') {
                self.sc.bump();
                self.skip_ws_inline();
                let value = self.space_list()?;
                return Ok(Some(IfClause {
                    condition: None,
                    value,
                }));
            }
            // `else` not followed by `:` — not a modern else clause.
            self.sc.reset(else_mark);
        }
        let condition = match self.parse_if_cond(probe)? {
            Some(c) => c,
            None => return Ok(None),
        };
        // A condition may not mix an evaluated `sass()` with a multi-token
        // "arbitrary substitution" raw sequence at the same boolean level.
        validate_if_cond(&condition)?;
        self.skip_ws_inline();
        if !self.sc.eat(':') {
            return Err(Error::at("expected \":\".", self.sc.position()));
        }
        self.skip_ws_inline();
        let value = self.space_list()?;
        Ok(Some(IfClause {
            condition: Some(condition),
            value,
        }))
    }

    /// Parse a full `if()` condition. dart-sass's grammar:
    ///   `not <operand>`                        (a single negated operand)
    ///   `<operand> (and <operand>)+`           (an `and` chain)
    ///   `<operand> (or <operand>)+`            (an `or` chain)
    ///   `<operand>`                            (a lone operand)
    /// Mixing `and` and `or` at the same level is an error; `not` only
    /// applies to a single non-multi-token operand and cannot be chained.
    /// Keywords (`not`/`and`/`or`) are matched case-insensitively.
    fn parse_if_cond(&mut self, probe: bool) -> Result<Option<IfCond>, Error> {
        self.skip_ws_inline();
        // Leading `not`: a single operand, no following `and`/`or`.
        if self.eat_if_keyword("not") {
            self.require_paren_whitespace_after("not")?;
            self.skip_ws_inline();
            let operand = self.parse_if_operand(false, true)?;
            return Ok(Some(IfCond::Not(Box::new(self.expect_operand(operand)?))));
        }
        let first = match self.parse_if_operand(probe, false)? {
            Some(c) => c,
            None => return Ok(None),
        };
        // Peek for a conjunction (case-insensitive).
        let mark = self.sc.mark();
        self.skip_ws_inline();
        let conj = self.peek_if_conjunction();
        self.sc.reset(mark);
        match conj {
            Some(is_and) => {
                let kw = if is_and { "and" } else { "or" };
                let other = if is_and { "or" } else { "and" };
                let mut items = vec![first];
                loop {
                    let mark = self.sc.mark();
                    self.skip_ws_inline();
                    if self.eat_if_keyword(kw) {
                        self.require_paren_whitespace_after(kw)?;
                        self.skip_ws_inline();
                        let operand = self.parse_if_operand(false, false)?;
                        items.push(self.expect_operand(operand)?);
                        continue;
                    }
                    // Mixing the other conjunction is an error.
                    if self.peek_word_is_ci(other) {
                        return Err(Error::at("expected \":\".", self.sc.position()));
                    }
                    self.sc.reset(mark);
                    break;
                }
                if is_and {
                    Ok(Some(IfCond::And(items)))
                } else {
                    Ok(Some(IfCond::Or(items)))
                }
            }
            None => Ok(Some(first)),
        }
    }

    /// Unwrap an operand that may have failed to parse.
    fn expect_operand(&self, operand: Option<IfCond>) -> Result<IfCond, Error> {
        operand.ok_or_else(|| Error::at("expected \"(\".", self.sc.position()))
    }

    /// Parse one condition operand: a parenthesised condition, `sass(<expr>)`,
    /// or a raw substitution sequence. When `single` is true (the operand of
    /// `not`), only a single raw token is consumed (a paren or one function
    /// call), not a multi-token sequence. Returns `Ok(None)` (when probing)
    /// if the position cannot begin an operand.
    fn parse_if_operand(&mut self, probe: bool, single: bool) -> Result<Option<IfCond>, Error> {
        self.skip_ws_inline();
        match self.sc.peek() {
            Some('(') => {
                self.sc.bump();
                self.skip_ws_inline();
                let inner = match self.parse_if_cond(false)? {
                    Some(c) => c,
                    None => return Err(Error::at("Expected identifier.", self.sc.position())),
                };
                self.skip_ws_inline();
                if !self.sc.eat(')') {
                    return Err(Error::at("expected \")\".", self.sc.position()));
                }
                Ok(Some(IfCond::Paren(Box::new(inner))))
            }
            _ if self.peek_keyword_paren_ci("sass") => {
                for _ in 0.."sass".chars().count() {
                    self.sc.bump();
                }
                self.sc.bump(); // '('
                self.skip_ws_inline();
                let expr = self.space_list()?;
                self.skip_ws_inline();
                if !self.sc.eat(')') {
                    return Err(Error::at("expected \")\".", self.sc.position()));
                }
                // A `sass()` atom may not be space-adjacent to a raw token
                // ("arbitrary substitution"): `sass(true) var(--x)` is illegal.
                let mark = self.sc.mark();
                let had_ws = self.skip_ws_inline();
                let next_is_raw = had_ws && self.peek_starts_raw_token();
                self.sc.reset(mark);
                if next_is_raw {
                    return Err(Error::at(
                        "if() conditions with arbitrary substitutions may not contain sass() expressions.",
                        self.sc.position(),
                    ));
                }
                Ok(Some(IfCond::Sass(Box::new(expr))))
            }
            _ => self.parse_if_raw_sequence(probe, single),
        }
    }

    /// Parse a sequence of raw substitution tokens (`css(...)`,
    /// `<ident>(...)`, `#{...}`) joined by spaces into a single non-evaluable
    /// condition. When `single` is true, at most one token is read. A
    /// `sass()` token inside the sequence is the "arbitrary substitution"
    /// error. Returns `Ok(None)` (when probing) if the input cannot begin a
    /// raw token.
    fn parse_if_raw_sequence(&mut self, probe: bool, single: bool) -> Result<Option<IfCond>, Error> {
        let mut pieces: Vec<TplPiece> = Vec::new();
        if !self.read_if_raw_token(&mut pieces)? {
            if probe {
                return Ok(None);
            }
            return Err(Error::at("expected \"(\".", self.sc.position()));
        }
        let mut token_count = 1usize;
        if !single {
            loop {
                let mark = self.sc.mark();
                let had_ws = self.skip_ws_inline();
                if !had_ws || !self.peek_starts_raw_token() {
                    self.sc.reset(mark);
                    break;
                }
                // A `sass()` token adjacent to raw substitution is illegal.
                if self.peek_keyword_paren_ci("sass") {
                    return Err(Error::at(
                        "if() conditions with arbitrary substitutions may not contain sass() expressions.",
                        self.sc.position(),
                    ));
                }
                pieces.push(TplPiece::Lit(" ".to_string()));
                if !self.read_if_raw_token(&mut pieces)? {
                    self.sc.reset(mark);
                    break;
                }
                token_count += 1;
            }
        }
        Ok(Some(IfCond::Raw {
            pieces,
            multi: token_count > 1,
        }))
    }

    /// Whether the next token starts a raw substitution token: an
    /// interpolation or an identifier (which must be followed by `(`).
    fn peek_starts_raw_token(&self) -> bool {
        match self.sc.peek() {
            Some('#') if self.sc.peek_at(1) == Some('{') => true,
            Some(c) if is_ident_char(c) => {
                // A bare keyword (`and`/`or`/`not`) is not a raw token.
                !(self.peek_word_is_ci("and") || self.peek_word_is_ci("or") || self.peek_word_is_ci("not"))
            }
            _ => false,
        }
    }

    /// Read one raw substitution token (`<ident>(...)` or `#{...}`),
    /// appending its serialized pieces. A bare interpolation token is valid;
    /// a bare identifier (not followed by `(`) is not. Returns false if the
    /// position does not start such a token.
    fn read_if_raw_token(&mut self, pieces: &mut Vec<TplPiece>) -> Result<bool, Error> {
        match self.sc.peek() {
            Some('#') if self.sc.peek_at(1) == Some('{') => {
                self.sc.bump();
                self.sc.bump();
                let e = self.parse_value()?;
                self.skip_ws_inline();
                if !self.sc.eat('}') {
                    return Err(Error::at("expected \"}\"", self.sc.position()));
                }
                pieces.push(TplPiece::Interp(e));
                // An interpolation followed immediately by `(` is a function.
                if self.sc.peek() == Some('(') {
                    self.read_if_raw_parens(pieces)?;
                }
                Ok(true)
            }
            // `not`/`and`/`or` are reserved: written as `not(`/`and(`/`or(`
            // (no space) they are the "whitespace required" error, not a
            // function call.
            Some(c)
                if (c.is_ascii_alphabetic())
                    && (self.peek_keyword_paren_ci("not")
                        || self.peek_keyword_paren_ci("and")
                        || self.peek_keyword_paren_ci("or")) =>
            {
                let kw = if self.peek_keyword_paren_ci("not") {
                    "not"
                } else if self.peek_keyword_paren_ci("and") {
                    "and"
                } else {
                    "or"
                };
                // Skip the keyword to point the error at the `(`.
                for _ in 0..kw.chars().count() {
                    self.sc.bump();
                }
                Err(Error::at(
                    format!("Whitespace is required between \"{kw}\" and \"(\""),
                    self.sc.position(),
                ))
            }
            Some(c) if is_ident_char(c) => {
                let mark = self.sc.mark();
                let name_pieces = self.read_ident_template()?;
                if self.sc.peek() != Some('(') {
                    // Bare identifier — not a raw token unless it contained
                    // interpolation (e.g. `#{"x"}`), which is a valid token.
                    if name_pieces.iter().all(|p| matches!(p, TplPiece::Lit(_))) {
                        self.sc.reset(mark);
                        return Ok(false);
                    }
                    pieces.extend(name_pieces);
                    return Ok(true);
                }
                pieces.extend(name_pieces);
                self.read_if_raw_parens(pieces)?;
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    /// Read a balanced `( ... )` group verbatim (resolving interpolation),
    /// appending to `pieces`. Assumes the next char is `(`.
    fn read_if_raw_parens(&mut self, pieces: &mut Vec<TplPiece>) -> Result<(), Error> {
        let mut lit = String::new();
        let mut depth = 0u32;
        loop {
            match self.sc.peek() {
                None => return Err(Error::at("expected \")\".", self.sc.position())),
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
        Ok(())
    }

    /// Read an identifier that may contain leading/embedded `#{...}`
    /// interpolation (e.g. `#{css}`), as a template.
    fn read_ident_template(&mut self) -> Result<Vec<TplPiece>, Error> {
        let mut pieces: Vec<TplPiece> = Vec::new();
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
        if pieces.is_empty() {
            return Err(Error::at("Expected identifier.", self.sc.position()));
        }
        Ok(pieces)
    }

    /// Peek whether the next word equals `kw` case-insensitively (followed
    /// by a non-ident char). `if()` keywords are case-insensitive.
    fn peek_word_is_ci(&self, kw: &str) -> bool {
        let rest = self.sc.rest();
        let kw_chars: Vec<char> = kw.chars().collect();
        if rest.len() < kw_chars.len() {
            return false;
        }
        for (i, &kc) in kw_chars.iter().enumerate() {
            if !rest[i].eq_ignore_ascii_case(&kc) {
                return false;
            }
        }
        match rest.get(kw_chars.len()) {
            Some(&c) => !is_ident_char(c),
            None => true,
        }
    }

    /// At the current position, peek whether a conjunction follows.
    /// `Some(true)` for `and`, `Some(false)` for `or`, `None` otherwise.
    fn peek_if_conjunction(&self) -> Option<bool> {
        if self.peek_word_is_ci("and") {
            Some(true)
        } else if self.peek_word_is_ci("or") {
            Some(false)
        } else {
            None
        }
    }

    /// Consume the keyword `kw` (case-insensitively) if it is the next word
    /// (followed by a non-ident char). Does not skip leading whitespace.
    fn eat_if_keyword(&mut self, kw: &str) -> bool {
        if self.peek_word_is_ci(kw) {
            for _ in 0..kw.chars().count() {
                self.sc.bump();
            }
            true
        } else {
            false
        }
    }

    /// After consuming `not`/`and`/`or`, dart-sass requires whitespace
    /// before a following `(` (otherwise `and(` etc. is a function call,
    /// which is an error in condition position).
    fn require_paren_whitespace_after(&mut self, kw: &str) -> Result<(), Error> {
        if self.sc.peek() == Some('(') {
            return Err(Error::at(
                format!("Whitespace is required between \"{kw}\" and \"(\""),
                self.sc.position(),
            ));
        }
        Ok(())
    }

    /// Peek whether the next word is `kw` (case-insensitive) immediately
    /// followed by `(`.
    fn peek_keyword_paren_ci(&self, kw: &str) -> bool {
        let rest = self.sc.rest();
        let kw_chars: Vec<char> = kw.chars().collect();
        if rest.len() <= kw_chars.len() {
            return false;
        }
        for (i, &kc) in kw_chars.iter().enumerate() {
            if !rest[i].eq_ignore_ascii_case(&kc) {
                return false;
            }
        }
        rest.get(kw_chars.len()) == Some(&'(')
    }

    /// Parse a call's argument list, assuming the opening `(` was already
    /// consumed, through the closing `)`. Args are positional or
    /// `$name: value`. Shared by function calls and `@include`.
    /// A function-argument value: a space-list optionally followed by one or
    /// more single-`=` Microsoft-filter operators (`alpha(opacity=80)`). The
    /// `=` is the lowest-precedence value operator and is recognised only here,
    /// inside an argument list (a lone `=` is a syntax error elsewhere). A
    /// `==` is the equality operator and is handled at its own precedence, so
    /// only a `=` not followed by `=` is consumed.
    fn arg_value(&mut self) -> Result<Expr, Error> {
        let mut lhs = self.space_list()?;
        loop {
            let mark = self.sc.mark();
            self.skip_ws_inline();
            if self.sc.peek() == Some('=') && self.sc.peek_at(1) != Some('=') {
                let pos = self.sc.position();
                self.sc.bump();
                self.skip_ws_inline();
                let rhs = self.space_list()?;
                lhs = Expr::Binary {
                    op: BinOp::SingleEq,
                    lhs: Box::new(lhs),
                    rhs: Box::new(rhs),
                    pos,
                };
            } else {
                self.sc.reset(mark);
                break;
            }
        }
        Ok(lhs)
    }

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
                let value = self.arg_value()?;
                // A trailing `...` marks a splat argument: a list spreads into
                // positional args and a map into keyword args. A named arg may
                // not be a splat.
                let splat = name_opt.is_none()
                    && self.sc.peek() == Some('.')
                    && self.sc.peek_at(1) == Some('.')
                    && self.sc.peek_at(2) == Some('.');
                if splat {
                    self.sc.bump();
                    self.sc.bump();
                    self.sc.bump();
                }
                args.push(CallArg {
                    name: name_opt,
                    value,
                    splat,
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
