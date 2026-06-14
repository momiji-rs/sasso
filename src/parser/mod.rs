//! The SCSS parser: a character-level recursive-descent parser.
//!
//! SCSS is context-sensitive — a leading `:` can begin a declaration
//! value or a pseudo-class selector — so statements are disambiguated by
//! a bounded lookahead ([`Parser::classify`]) that finds whether a
//! top-level `{` (a rule) or `;`/`}` (a declaration) comes first.

use std::rc::Rc;

use crate::ast::{
    BinOp, CallArg, Callable, ConfigEntry, Conjunction, CssCustomItem, CssCustomValue, CustomDecl,
    Declaration, Expr, ForwardMember, IfBranch, IfClause, IfCond, ImportArg, ImportModifier, MediaFeature,
    MediaInParens, MediaQuery, MediaQueryList, Param, ParamList, PropertySet, Rule, SrcLines, Stmt,
    Stylesheet, SupportsCondition, SupportsValue, TplPiece, UnOp, VarDecl,
};
use crate::error::Error;
use crate::scanner::{Mark, Pos, Scanner};
use crate::value::{named_color, Color, ListSep};

mod value;

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
    /// A declaration's property name: like `Strip`, except that ONE loud
    /// comment directly glued to the name (no whitespace between) joins it
    /// verbatim — dart `_declarationOrBuffer` appends `rawText(loudComment)`
    /// to the name buffer when `/*` immediately follows the identifier
    /// (issue_1422: `foo/*c*/: bar` keeps the comment, `foo /*c*/ : bar`
    /// drops it).
    DeclName,
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

/// The plain (non-interpolated) text of a template, or `None` if it contains
/// any `#{…}` interpolation (dart-sass `Interpolation.asPlain`).
fn tpl_plain(pieces: &[TplPiece]) -> Option<String> {
    let mut s = String::new();
    for p in pieces {
        match p {
            TplPiece::Lit(t) => s.push_str(t),
            TplPiece::Interp(_) => return None,
        }
    }
    Some(s)
}

/// If `pieces` is exactly one `#{…}` interpolation (no surrounding literal),
/// return its expression; otherwise `None`.
fn tpl_single_interp(pieces: Vec<TplPiece>) -> Option<Expr> {
    if pieces.len() == 1 {
        if let Some(TplPiece::Interp(e)) = pieces.into_iter().next() {
            return Some(e);
        }
    }
    None
}

/// Whether a `@supports` declaration name is a CSS custom property: an unquoted
/// identifier whose plain text begins with `--` (dart-sass
/// `SupportsDeclaration.isCustomProperty`).
fn expr_is_custom_property(name: &Expr) -> bool {
    match name {
        Expr::Ident(pieces) => match pieces.first() {
            Some(TplPiece::Lit(s)) => s.starts_with("--"),
            _ => false,
        },
        _ => false,
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
    /// Depth of enclosing `{ … }` blocks. `@use`/`@forward` are only valid at
    /// the top level (depth 0); inside any block they are "This at-rule is not
    /// allowed here.".
    block_depth: u32,
    /// When set, `parse_template_mode` records each top-level `#{…}`
    /// interpolation's expression span (line, start col, col of `}`) here —
    /// used by rule selectors for the dual-span "error in interpolated
    /// output" diagnostic. Nested template parses (inside an interpolation's
    /// expression) suspend collection.
    collect_interp_spans: bool,
    interp_spans: Vec<(u32, u32, u32)>,
    /// Set once a top-level statement that is *not* a variable declaration,
    /// comment, `@charset`, `@use`, or `@forward` has been parsed. A later
    /// `@use`/`@forward` then errors ("@use rules must be written before any
    /// other rules.").
    seen_non_module_stmt: bool,
    /// Plain-CSS mode (a `.css` file loaded via `@use`/`@forward`). SassScript
    /// and Sass-only statements are rejected: this is the analogue of dart-sass's
    /// `CssParser`. Nesting is still parsed (CSS nesting is preserved in output);
    /// the difference is that Sass features become errors.
    plain_css: bool,
}

/// Parse a complete stylesheet (SCSS).
pub(crate) fn parse(src: &str) -> Result<Stylesheet, Error> {
    parse_inner(src, false)
}

/// Parse a plain-CSS stylesheet (a loaded `.css` file): the same brace/semicolon
/// grammar, but Sass features are rejected.
pub(crate) fn parse_plain_css(src: &str) -> Result<Stylesheet, Error> {
    parse_inner(src, true)
}

fn parse_inner(src: &str, plain_css: bool) -> Result<Stylesheet, Error> {
    let mut p = Parser {
        sc: Scanner::new(src),
        calc_depth: 0,
        pending_unicode_split: false,
        block_depth: 0,
        collect_interp_spans: false,
        interp_spans: Vec::new(),
        seen_non_module_stmt: false,
        plain_css,
    };
    let stmts = p.parse_statements(true)?;
    Ok(Stylesheet { stmts })
}

fn is_ident_char(c: char) -> bool {
    // dart `isName`: any non-ASCII code point is a valid identifier char
    // (`$vär`, `föö` need no escaping).
    c.is_ascii_alphanumeric() || c == '-' || c == '_' || (c as u32) >= 0x80
}

/// dart `_minimumIndentation` + `_writeWithIndent`: a loud comment's
/// continuation lines drop `min(<their minimum indentation>, <the comment's
/// own start column>)` leading spaces at PARSE time; the emitter then adds
/// the current output indentation back. Only applied to interpolation-free
/// comments (the common case).
fn strip_comment_indent(pieces: &mut [TplPiece], start_col: usize) {
    if pieces.len() != 1 {
        return;
    }
    let TplPiece::Lit(text) = &mut pieces[0] else {
        return;
    };
    if !text.contains('\n') {
        return;
    }
    let mut min_indent: Option<usize> = None;
    for line in text.split('\n').skip(1) {
        if line.trim().is_empty() {
            continue;
        }
        let ind = line.len() - line.trim_start_matches(' ').len();
        min_indent = Some(min_indent.map_or(ind, |m| m.min(ind)));
    }
    let Some(min_indent) = min_indent else { return };
    let strip = min_indent.min(start_col);
    if strip == 0 {
        return;
    }
    let mut out = String::with_capacity(text.len());
    for (i, line) in text.split('\n').enumerate() {
        if i > 0 {
            out.push('\n');
            let avail = line.len() - line.trim_start_matches(' ').len();
            out.push_str(&line[strip.min(avail)..]);
        } else {
            out.push_str(line);
        }
    }
    *text = out;
}

/// Reject generic/unknown at-rules in a context that forbids them (function
/// bodies, nested property sets), recursing into control-flow bodies.
fn reject_at_rules_in(stmts: &[Stmt]) -> Result<(), Error> {
    for s in stmts {
        match s {
            Stmt::AtRule { .. } | Stmt::InterpAtRule { .. } => {
                return Err(Error::unpositioned("This at-rule is not allowed here."));
            }
            Stmt::If(branches) => {
                for b in branches {
                    reject_at_rules_in(&b.body)?;
                }
            }
            Stmt::For { body, .. } | Stmt::Each { body, .. } | Stmt::While { body, .. } => {
                reject_at_rules_in(body)?;
            }
            _ => {}
        }
    }
    Ok(())
}

/// Whether `name` is a global Sass function with no plain-CSS meaning, so a
/// `.css` file calling it is an error (the CSS color/math functions — `rgb`,
/// `hsl`, `grayscale`, `saturate`, `min`, `calc`, … — are deliberately absent,
/// since plain CSS preserves those verbatim).
fn is_sass_only_function(name: &str) -> bool {
    matches!(
        name,
        // list
        "index" | "nth" | "set-nth" | "join" | "append" | "zip" | "list-separator"
            | "is-bracketed" | "length"
            // map
            | "map-get" | "map-merge" | "map-remove" | "map-keys" | "map-values" | "map-has-key"
            // meta
            | "type-of" | "unit" | "unitless" | "comparable" | "inspect" | "keywords"
            | "feature-exists" | "variable-exists" | "global-variable-exists" | "function-exists"
            | "mixin-exists" | "content-exists" | "get-function" | "call" | "get-mixin"
            // string
            | "str-length" | "str-insert" | "str-index" | "str-slice" | "to-upper-case"
            | "to-lower-case" | "unique-id"
            // color (Sass-only adjusters / getters, not CSS functions)
            | "mix" | "adjust-hue" | "lighten" | "darken" | "desaturate" | "opacify"
            | "transparentize" | "fade-in" | "fade-out" | "scale-color" | "adjust-color"
            | "change-color" | "ie-hex-str"
            // selector
            | "selector-nest" | "selector-append" | "selector-replace" | "selector-unify"
            | "is-superselector" | "simple-selectors" | "selector-parse" | "selector-extend"
    )
}

/// Whether a module member name is private (dart-sass: begins with `-` or
/// `_`). Private members can't be accessed across module boundaries.
fn is_private_member(name: &str) -> bool {
    name.starts_with('-') || name.starts_with('_')
}

/// Whether a top-level statement may legally appear *before* a `@use` rule.
/// dart-sass permits variable declarations, loud comments, `@charset`, `@use`,
/// and `@forward`; everything else means a following `@use` is too late.
fn stmt_allowed_before_use(stmt: &Stmt) -> bool {
    match stmt {
        Stmt::VarDecl(_) | Stmt::Comment(..) | Stmt::Use { .. } | Stmt::Forward { .. } => true,
        Stmt::AtRule { name, .. } => name.eq_ignore_ascii_case("charset"),
        _ => false,
    }
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
/// Append a single literal character to a template, merging into a trailing
/// `Lit` piece when possible.
fn push_lit(pieces: &mut Vec<TplPiece>, c: char) {
    if let Some(TplPiece::Lit(s)) = pieces.last_mut() {
        s.push(c);
    } else {
        pieces.push(TplPiece::Lit(c.to_string()));
    }
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

/// Whether `expr` is a bare quoted-string atom. dart-sass forms an implicit
/// space-separated list when a quoted string abuts an adjacent value atom with
/// no whitespace, so the space-list parser uses this to decide whether a
/// no-whitespace boundary continues the list (`"["'foo'"]"` -> a three-element
/// list) rather than ending it.
fn expr_is_quoted_string(expr: &Expr) -> bool {
    matches!(expr, Expr::QuotedString(_))
}

/// Strip a `-foo-` vendor prefix (a `-`, ASCII letters, then `-`). `--x`,
/// `-moz_x` (underscore), and an unprefixed name are returned unchanged.
fn unvendor(name: &str) -> &str {
    let bytes = name.as_bytes();
    if bytes.first() != Some(&b'-') || bytes.get(1) == Some(&b'-') {
        return name;
    }
    for (i, &c) in bytes.iter().enumerate().skip(1) {
        if c == b'-' {
            return &name[i + 1..];
        }
        if !c.is_ascii_alphabetic() {
            return name;
        }
    }
    name
}

/// Whether `name` is reserved and may not be a user `@function` name (dart-sass
/// "Invalid function name."). The SassScript operators and the raw-content CSS
/// functions `url`/`expression` match exactly (so `-a-and`, `AND`, `-a-url` are
/// allowed); `element` matches after stripping a vendor prefix; `type` matches
/// case-insensitively.
fn is_reserved_function_name(name: &str) -> bool {
    matches!(name, "and" | "or" | "not" | "url" | "expression")
        || unvendor(name) == "element"
        || name.eq_ignore_ascii_case("type")
}

/// Whether a property-name template begins, *literally*, with `--` (a custom
/// property). A name whose first piece is `#{…}` interpolation is not literal,
/// so `#{--b}` namespaces normally while a written `--b` is a custom property.
fn property_is_literal_custom(property: &[TplPiece]) -> bool {
    match property.first() {
        Some(TplPiece::Lit(s)) => s.trim_start().starts_with("--"),
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
            self.skip_trivia(&mut stmts)?;
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
                // A stray `;` (e.g. after a `}` rule block) is an empty
                // statement: dart-sass accepts and ignores it.
                Some(';') => {
                    self.sc.bump();
                    continue;
                }
                Some('$') => stmts.push(self.parse_var_decl()?),
                Some('@') => stmts.push(self.parse_at_rule()?),
                // A namespaced variable assignment `ns.$name: value`.
                _ if self.peek_namespaced_var_decl() => stmts.push(self.parse_var_decl()?),
                _ => match self.classify() {
                    NextKind::Rule => stmts.push(self.parse_rule()?),
                    NextKind::Declaration => stmts.push(self.parse_declaration()?),
                },
            }
            // Track, at the top level, whether anything that must follow `@use`
            // has been seen. A variable declaration, `@charset`, `@use`, and
            // `@forward` (and comments, which `skip_trivia` collects) are
            // permitted before `@use`; anything else "uses up" the position.
            if top {
                if let Some(last) = stmts.last() {
                    if !stmt_allowed_before_use(last) {
                        self.seen_non_module_stmt = true;
                    }
                }
            }
        }
        Ok(stmts)
    }

    /// Skip whitespace and comments, collecting loud `/* */` comments into
    /// the statement stream so they emit in source order.
    fn skip_trivia(&mut self, out: &mut Vec<Stmt>) -> Result<(), Error> {
        loop {
            match self.sc.peek() {
                Some(c) if c.is_whitespace() => {
                    self.sc.bump();
                }
                Some('/') if self.sc.peek_at(1) == Some('/') => {
                    if self.plain_css {
                        return Err(Error::at(
                            "Silent comments aren't allowed in plain CSS.",
                            self.sc.position(),
                        ));
                    }
                    while let Some(c) = self.sc.peek() {
                        if c == '\n' {
                            break;
                        }
                        self.sc.bump();
                    }
                }
                Some('/') if self.sc.peek_at(1) == Some('*') => {
                    let start = self.sc.position();
                    let col0 = start.col - 1;
                    self.sc.bump();
                    self.sc.bump();
                    let mut pieces = self.parse_loud_comment_body()?;
                    strip_comment_indent(&mut pieces, col0);
                    let end_line = self.sc.position().line as u32;
                    out.push(Stmt::Comment(
                        pieces,
                        SrcLines {
                            file: 0,
                            start: start.line as u32,
                            end: end_line,
                            col: start.col as u32,
                        },
                    ));
                }
                _ => break,
            }
        }
        Ok(())
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
                // In plain CSS `//` is not a comment: each `/` is a value token
                // (`1///bar` round-trips), so it must not be skipped.
                Some('/') if self.sc.peek_at(1) == Some('/') && !self.plain_css => {
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
        // Whether the statement's first non-whitespace characters are `--`: a
        // custom-property name. With a top-level `:` it is always a custom
        // declaration (`--ambiguous:foo {…}`), never a style rule.
        let starts_custom = {
            let first = cs.iter().position(|c| !c.is_whitespace());
            matches!(first, Some(p) if cs.get(p) == Some(&'-') && cs.get(p + 1) == Some(&'-'))
        };
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
                // A CSS escape: the next character (`something\:`) is part of
                // the identifier, never a declaration colon.
                '\\' => {
                    i += 2;
                    continue;
                }
                '(' => paren += 1,
                ')' => paren -= 1,
                '[' => bracket += 1,
                ']' => bracket -= 1,
                ':' if paren == 0 && bracket == 0 && after_colon.is_none() => {
                    // A custom-property name with a `:` is always a declaration,
                    // even when a `{` follows (`--ambiguous:foo {…}`).
                    if starts_custom {
                        return NextKind::Declaration;
                    }
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
        let selector_pos = self.sc.position();
        let saved_spans = std::mem::take(&mut self.interp_spans);
        let saved_collect = std::mem::replace(&mut self.collect_interp_spans, true);
        // A top-level `!` is not valid in a selector: dart-sass stops the
        // selector there and then fails to find the `{` (`a !important {…}` →
        // `expected "{".`). It is only a stop at depth 0 — a `!` inside an
        // attribute selector (`[a="x!y"]`) or a string is consumed normally.
        let selector = self.parse_template_mode(&['{', '!'], CommentMode::Strip);
        self.collect_interp_spans = saved_collect;
        let selector_interp_spans = std::mem::replace(&mut self.interp_spans, saved_spans);
        let selector = selector?;
        let brace_line = self.sc.position().line as u32;
        if !self.sc.eat('{') {
            return Err(Error::at("expected \"{\".", self.sc.position()));
        }
        self.block_depth += 1;
        let body = self.parse_statements(false);
        self.block_depth -= 1;
        let body = body?;
        if !self.sc.eat('}') {
            return Err(Error::at("expected \"}\"", self.sc.position()));
        }
        let end_line = self.sc.position().line as u32;
        Ok(Stmt::Rule(Rule {
            selector,
            body,
            selector_pos,
            selector_interp_spans,
            brace_line,
            end_line,
        }))
    }

    fn parse_declaration(&mut self) -> Result<Stmt, Error> {
        let pos = self.sc.position();
        // An IE property hack may start with a single punctuation character;
        // `*x`/`.x`/`#x` fall out of template parsing naturally, but a leading
        // `:` (`:x: y`) must be consumed up front so the property name doesn't
        // terminate at it (dart-sass `_declarationOrBuffer` reads one leading
        // `:`/`*`/`.`/`#` before the identifier).
        let colon_hack = self.sc.peek() == Some(':')
            && matches!(self.sc.peek_at(1), Some(c) if c.is_alphanumeric() || c == '-' || c == '_' || c == '\\');
        if colon_hack {
            self.sc.bump();
        }
        let mut property = self.parse_template_mode(&[':'], CommentMode::DeclName)?;
        if colon_hack {
            match property.first_mut() {
                Some(TplPiece::Lit(lit)) => lit.insert(0, ':'),
                _ => property.insert(0, TplPiece::Lit(":".to_string())),
            }
        }
        if !self.sc.eat(':') {
            return Err(Error::at("expected \":\" in declaration", self.sc.position()));
        }
        // A declaration whose name *literally* begins with `--` is a custom
        // property: its value is captured verbatim (only `#{…}` interpolation
        // resolves, no SassScript), and a trailing `{` is part of the value —
        // never a nested property set.
        if property_is_literal_custom(&property) {
            let value = self.parse_custom_property_value()?;
            let end_line = self.sc.position().line as u32;
            self.sc.eat(';');
            return Ok(Stmt::CustomDecl(CustomDecl {
                property,
                value,
                pos,
                end_line,
            }));
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
        // The line where the declaration's span ends, for the serializer's
        // trailing-comment rule — captured before whitespace is skipped (an
        // `!important` flag extends the span).
        let mut end_line = self.sc.position().line as u32;
        let mut important = false;
        self.skip_ws_inline();
        if self.sc.peek() == Some('!') {
            // `!important` is consumed by the expression layer as a value
            // term, so a `!` here is a stray flag (`!default` after a plain
            // declaration) — dart stops the declaration at the `!` and fails
            // wanting the terminator.
            let bang_pos = self.sc.position();
            if !self.looking_at_important() {
                return Err(Error::at("expected \";\".", bang_pos));
            }
            self.sc.bump();
            self.skip_ws_inline();
            let _ = self.read_ident_name();
            important = true;
            end_line = self.sc.position().line as u32;
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
            end_line,
        }))
    }

    /// Parse the `{ … }` block of a nested property set (the cursor is at `{`),
    /// consuming an optional trailing `;` so a following sibling parses cleanly
    /// (`b: { c: { d: e }; f: g }`).
    fn parse_property_set_body(&mut self) -> Result<Vec<Stmt>, Error> {
        if self.plain_css {
            return Err(Error::at(
                "Nested declarations aren't allowed in plain CSS.",
                self.sc.position(),
            ));
        }
        self.sc.bump(); // '{'
        self.block_depth += 1;
        let body = self.parse_statements(false);
        self.block_depth -= 1;
        let body = body?;
        // Unknown at-rules aren't allowed in a nested property set.
        reject_at_rules_in(&body)?;
        // dart's `_declarationChild` parses every non-`@` child of a nested
        // property set as a declaration, so a style rule there is its
        // `expected ":".` (the selector reads as a property name).
        fn reject_rules_in(stmts: &[Stmt]) -> Result<(), Error> {
            for s in stmts {
                match s {
                    Stmt::Rule(_) => {
                        return Err(Error::unpositioned("expected \":\"."));
                    }
                    Stmt::If(branches) => {
                        for b in branches {
                            reject_rules_in(&b.body)?;
                        }
                    }
                    Stmt::For { body, .. } | Stmt::Each { body, .. } | Stmt::While { body, .. } => {
                        reject_rules_in(body)?;
                    }
                    _ => {}
                }
            }
            Ok(())
        }
        reject_rules_in(&body)?;
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
        if self.plain_css {
            return Err(Error::at("Sass variables aren't allowed in plain CSS.", pos));
        }
        // An optional `ns.` prefix: `ns.$name: value` assigns to a module
        // variable. `peek_namespaced_var_decl` guarantees the shape.
        let mut namespace = None;
        if self.sc.peek() != Some('$') {
            namespace = Some(self.read_ident_name()?);
            self.sc.eat('.');
        }
        self.sc.bump(); // '$'
        let name = self.read_variable_name()?;
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
            namespace,
        }))
    }

    /// Whether the scanner is at the start of a namespaced variable assignment
    /// `ns.$name` (an identifier, then `.`, then `$`). Does not consume input.
    fn peek_namespaced_var_decl(&self) -> bool {
        let mut i = 0;
        // Leading identifier.
        match self.sc.peek_at(0) {
            Some(c) if c == '-' || c == '_' || c.is_ascii_alphabetic() || !c.is_ascii() => {}
            _ => return false,
        }
        while matches!(self.sc.peek_at(i), Some(c) if is_ident_char(c)) {
            i += 1;
        }
        if i == 0 || self.sc.peek_at(i) != Some('.') {
            return false;
        }
        self.sc.peek_at(i + 1) == Some('$')
    }

    fn parse_at_rule(&mut self) -> Result<Stmt, Error> {
        let pos = self.sc.position();
        // Mark at the `@` so span-carrying at-rules (`@error`, `@include`) can
        // measure their byte length for the diagnostic caret.
        let start_mark = self.sc.mark();
        self.sc.bump(); // '@'
                        // An interpolated NAME makes this a generic (unknown) at-rule with no
                        // Sass parse-time behavior (`@#{"media"} …`).
        if self.sc.peek() == Some('#') && self.sc.peek_at(1) == Some('{') {
            return self.parse_interp_at_rule(Vec::new());
        }
        let name = self.read_ident_name()?;
        if self.sc.peek() == Some('#') && self.sc.peek_at(1) == Some('{') {
            return self.parse_interp_at_rule(vec![TplPiece::Lit(name)]);
        }
        // In plain CSS the Sass control/definition at-rules are rejected; only
        // genuine CSS at-rules (`@media`, `@supports`, `@font-face`,
        // `@keyframes`, `@import`, `@charset`, `@page`, and unknown vendor
        // at-rules emitted verbatim) are allowed.
        if self.plain_css {
            // `@function --x`/`@mixin --x` are plain-CSS custom callables (the
            // `--` prefix), which CSS allows; a bare `@function`/`@mixin` is the
            // Sass definition and is rejected like the other control at-rules.
            let sass_callable =
                matches!(name.as_str(), "function" | "mixin") && !self.peek_callable_name_is_custom();
            if sass_callable
                || matches!(
                    name.as_str(),
                    "if" | "else"
                        | "each"
                        | "for"
                        | "while"
                        | "include"
                        | "content"
                        | "return"
                        | "warn"
                        | "debug"
                        | "error"
                        | "extend"
                        | "at-root"
                        | "use"
                        | "forward"
                )
            {
                return Err(Error::at("This at-rule isn't allowed in plain CSS.", pos));
            }
        }
        match name.as_str() {
            "import" => self.parse_import(pos),
            "if" => self.parse_if(),
            // A stray `@else` (one not consumed as part of an `@if` chain by
            // `parse_if`) is never valid on its own.
            "else" => Err(Error::at("This at-rule is not allowed here.", pos)),
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
            "include" => self.parse_include(pos, start_mark),
            "content" => {
                self.skip_ws_inline();
                let args = if self.sc.peek() == Some('(') {
                    self.sc.bump();
                    self.parse_args_after_paren()?
                } else {
                    Vec::new()
                };
                self.skip_ws_inline();
                self.sc.eat(';');
                Ok(Stmt::Content(args))
            }
            "warn" => self.parse_message(MessageKind::Warn, pos, start_mark),
            "debug" => self.parse_message(MessageKind::Debug, pos, start_mark),
            "error" => self.parse_message(MessageKind::Error, pos, start_mark),
            "at-root" => self.parse_at_root(),
            "media" => self.parse_media(),
            "keyframes" | "-webkit-keyframes" | "-moz-keyframes" | "-o-keyframes" | "-ms-keyframes" => {
                self.parse_keyframes(name)
            }
            "extend" => self.parse_extend(pos),
            // `@charset` takes exactly one quoted string. A non-string (or
            // missing) argument is dart's "Expected string."; anything after
            // the string is left for the next statement (so `@charset "a" "b"`
            // fails as a malformed following rule, `expected "{".`). The value
            // itself is not emitted — the serializer re-derives `@charset` from
            // the output's own non-ASCII content.
            "charset" => {
                self.skip_ws_inline();
                let line = self.sc.position().line as u32;
                if !matches!(self.sc.peek(), Some('"' | '\'')) {
                    return Err(Error::at("Expected string.", self.sc.position()));
                }
                let mut prelude = vec![TplPiece::Lit("\"".to_string())];
                prelude.extend(self.parse_quoted_string()?);
                prelude.push(TplPiece::Lit("\"".to_string()));
                self.skip_ws_inline();
                self.sc.eat(';');
                Ok(Stmt::AtRule {
                    name,
                    prelude,
                    body: None,
                    lines: SrcLines {
                        file: 0,
                        start: line,
                        end: line,
                        col: 0,
                    },
                })
            }
            "supports" => self.parse_supports(),
            "use" => self.parse_use(pos),
            // @forward is parked for the module-system epic.
            "forward" => self.parse_forward(pos),
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
            if self.sc.peek() == Some(',') {
                // Plain CSS `@import` takes a single URL — a comma-separated
                // list is a Sass-only feature.
                if self.plain_css {
                    return Err(Error::at("expected \";\".", self.sc.position()));
                }
                self.sc.bump();
                continue;
            }
            break;
        }
        self.skip_ws_trivia();
        // After the last argument only `;`, `}`, or EOF may follow; anything
        // else (e.g. a supports()/identifier after a media query list, the
        // `wrong_order` cases) is a syntax error like dart-sass.
        match self.sc.peek() {
            None | Some(';') | Some('}') => {}
            _ => return Err(Error::at("expected \";\".", self.sc.position())),
        }
        self.sc.eat(';');
        Ok(Stmt::Import(args))
    }

    /// Parse `@use "<url>" [as <namespace>|as *];`. The URL is a quoted string
    /// without interpolation; an explicit `as ns` / `as *` overrides the
    /// default namespace (the segment after the last `/`, or after `sass:`).
    fn parse_use(&mut self, pos: Pos) -> Result<Stmt, Error> {
        if self.block_depth > 0 {
            return Err(Error::at("This at-rule is not allowed here.", pos));
        }
        if self.seen_non_module_stmt {
            return Err(Error::at(
                "@use rules must be written before any other rules.",
                pos,
            ));
        }
        self.skip_ws_trivia();
        let url = self.parse_module_url(pos)?;
        self.skip_ws_trivia();
        let mut namespace = None;
        let mut star = false;
        if self.try_keyword("as") {
            self.skip_ws_inline();
            if self.sc.eat('*') {
                star = true;
            } else {
                namespace = Some(self.read_namespace_ident()?);
            }
            self.skip_ws_trivia();
        }
        // `with (...)` overrides the module's `!default` variables.
        let mut config = Vec::new();
        if self.try_keyword("with") {
            config = self.parse_config_clause(pos)?;
        }
        self.skip_ws_trivia();
        self.sc.eat(';');
        Ok(Stmt::Use {
            url,
            namespace,
            star,
            config,
            pos,
        })
    }

    /// Parse `@forward "<url>" [as <prefix>-*] [show ...|hide ...] [with (...)];`.
    fn parse_forward(&mut self, pos: Pos) -> Result<Stmt, Error> {
        if self.block_depth > 0 {
            return Err(Error::at("This at-rule is not allowed here.", pos));
        }
        if self.seen_non_module_stmt {
            return Err(Error::at(
                "@forward rules must be written before any other rules.",
                pos,
            ));
        }
        self.skip_ws_trivia();
        let url = self.parse_module_url(pos)?;
        self.skip_ws_trivia();
        let mut prefix = None;
        if self.try_keyword("as") {
            self.skip_ws_inline();
            let name = self.read_namespace_ident()?;
            if !self.sc.eat('*') {
                return Err(Error::at("expected \"*\".", self.sc.position()));
            }
            prefix = Some(name);
            self.skip_ws_trivia();
        }
        let mut show = None;
        let mut hide = None;
        if self.try_keyword("show") {
            show = Some(self.parse_forward_members(pos)?);
            self.skip_ws_trivia();
        } else if self.try_keyword("hide") {
            hide = Some(self.parse_forward_members(pos)?);
            self.skip_ws_trivia();
        }
        let mut config = Vec::new();
        if self.try_keyword("with") {
            config = self.parse_config_clause(pos)?;
        }
        self.skip_ws_trivia();
        self.sc.eat(';');
        Ok(Stmt::Forward {
            url,
            prefix,
            show,
            hide,
            config,
            pos,
        })
    }

    /// Parse a `with ( $name: value [!default], ... )` configuration clause.
    fn parse_config_clause(&mut self, pos: Pos) -> Result<Vec<ConfigEntry>, Error> {
        self.skip_ws_trivia();
        if !self.sc.eat('(') {
            return Err(Error::at("expected \"(\".", self.sc.position()));
        }
        let mut entries = Vec::new();
        loop {
            self.skip_ws_trivia();
            if self.sc.eat(')') {
                break;
            }
            let entry_pos = self.sc.position();
            if self.sc.peek() != Some('$') {
                return Err(Error::at("expected \"$\".", entry_pos));
            }
            self.sc.bump();
            let name = self.read_variable_name()?;
            self.skip_ws_trivia();
            if !self.sc.eat(':') {
                return Err(Error::at("expected \":\".", self.sc.position()));
            }
            self.skip_ws_trivia();
            // A configuration value is a single (space-list) expression: a
            // comma is the entry separator, so a comma-list value needs parens.
            let value = self.space_list()?;
            let mut is_default = false;
            self.skip_ws_inline();
            if self.sc.peek() == Some('!') {
                self.sc.bump();
                let flag = self.read_ident_name()?;
                if flag == "default" {
                    is_default = true;
                } else {
                    return Err(Error::at("Invalid flag name.".to_string(), entry_pos));
                }
            }
            entries.push(ConfigEntry {
                name,
                value,
                is_default,
            });
            self.skip_ws_trivia();
            if self.sc.eat(',') {
                continue;
            }
            self.skip_ws_trivia();
            if self.sc.eat(')') {
                break;
            }
            return Err(Error::at("expected \")\".", self.sc.position()));
        }
        let _ = pos;
        Ok(entries)
    }

    /// Parse a `show`/`hide` member list: comma-separated identifiers and
    /// `$variable` names.
    fn parse_forward_members(&mut self, pos: Pos) -> Result<Vec<ForwardMember>, Error> {
        let mut members = Vec::new();
        loop {
            self.skip_ws_trivia();
            if self.sc.peek() == Some('$') {
                self.sc.bump();
                let name = self.read_variable_name()?;
                members.push(ForwardMember::Var(name));
            } else {
                let name = self.read_ident_name()?;
                members.push(ForwardMember::Name(name));
            }
            self.skip_ws_trivia();
            if self.sc.eat(',') {
                continue;
            }
            break;
        }
        if members.is_empty() {
            return Err(Error::at("Expected variable, mixin, or function name", pos));
        }
        Ok(members)
    }

    /// Read a quoted module URL (no interpolation allowed) for `@use`/`@forward`.
    fn parse_module_url(&mut self, pos: Pos) -> Result<String, Error> {
        match self.sc.peek() {
            Some('"') | Some('\'') => {
                let pieces = self.parse_quoted_string()?;
                let mut url = String::new();
                for p in &pieces {
                    match p {
                        TplPiece::Lit(s) => url.push_str(s),
                        TplPiece::Interp(_) => {
                            return Err(Error::at("dynamic module URLs are not supported", pos));
                        }
                    }
                }
                Ok(url)
            }
            _ => Err(Error::at("expected a string.", pos)),
        }
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
            let url = self.parse_import_url_func()?;
            self.skip_ws_trivia();
            let modifiers = self.parse_import_modifiers()?;
            return Ok(ImportArg::Css { url, modifiers });
        }
        // Quoted-string form.
        match self.sc.peek() {
            Some('"') | Some('\'') => {
                let url_pos = self.sc.position();
                let mark = self.sc.mark();
                let pieces = self.parse_quoted_string()?;
                let url_len = self.sc.byte_len_from(mark);
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
                    Ok(ImportArg::Sass {
                        path,
                        pos: url_pos,
                        length: url_len,
                    })
                } else {
                    Ok(ImportArg::Css {
                        url: vec![TplPiece::Lit(raw_url)],
                        modifiers,
                    })
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

    /// Capture a `url(...)` argument (parens may nest). The `url(` wrapper and
    /// the URL text are literal, but `#{…}` interpolation — at the top level or
    /// inside a quoted string — is expanded (dart-sass resolves
    /// `@import url("#{$p}://…")`). A URL with no interpolation yields a single
    /// literal piece, byte-identical to the verbatim source.
    fn parse_import_url_func(&mut self) -> Result<Vec<TplPiece>, Error> {
        let mut pieces: Vec<TplPiece> = Vec::new();
        let mut lit = String::new();
        for _ in 0..4 {
            if let Some(c) = self.sc.bump() {
                lit.push(c); // `url(`
            }
        }
        let mut depth = 1i32;
        while let Some(c) = self.sc.peek() {
            if c == '#' && self.sc.peek_at(1) == Some('{') {
                if self.plain_css {
                    return Err(Error::at(
                        "Interpolation isn't allowed in plain CSS.",
                        self.sc.position(),
                    ));
                }
                if !lit.is_empty() {
                    pieces.push(TplPiece::Lit(std::mem::take(&mut lit)));
                }
                pieces.push(TplPiece::Interp(self.read_interp()?));
                continue;
            }
            match c {
                '"' | '\'' => {
                    let q = c;
                    lit.push(c);
                    self.sc.bump();
                    while let Some(ch) = self.sc.peek() {
                        if ch == '\\' {
                            lit.push(ch);
                            self.sc.bump();
                            if let Some(n) = self.sc.bump() {
                                lit.push(n);
                            }
                            continue;
                        }
                        // A raw newline terminates the string with dart's
                        // `Expected ".` (issue_1096 CRLF url strings).
                        if ch == '\n' || ch == '\r' {
                            return Err(Error::at(format!("Expected {q}."), self.sc.position()));
                        }
                        if ch == '#' && self.sc.peek_at(1) == Some('{') {
                            if self.plain_css {
                                return Err(Error::at(
                                    "Interpolation isn't allowed in plain CSS.",
                                    self.sc.position(),
                                ));
                            }
                            if !lit.is_empty() {
                                pieces.push(TplPiece::Lit(std::mem::take(&mut lit)));
                            }
                            pieces.push(TplPiece::Interp(self.read_interp()?));
                            continue;
                        }
                        lit.push(ch);
                        self.sc.bump();
                        if ch == q {
                            break;
                        }
                    }
                }
                '(' => {
                    depth += 1;
                    lit.push(c);
                    self.sc.bump();
                }
                ')' => {
                    depth -= 1;
                    lit.push(c);
                    self.sc.bump();
                    if depth == 0 {
                        break;
                    }
                }
                _ => {
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
        Ok(pieces)
    }

    /// Parse the optional modifiers that follow an `@import` URL, mirroring
    /// dart-sass `tryImportModifiers`: a run of bare identifiers and unknown
    /// functions (kept near-verbatim), at most interleaved `supports(<query>)`
    /// clauses (parsed structurally so they re-serialize canonically), then —
    /// terminally — a media query list, entered either at a `(`-feature or at
    /// a comma following a bare identifier (a media type). After the media
    /// list the modifiers END: a following `supports(...)`/identifier is a
    /// syntax error surfaced by `parse_import`'s `;` check.
    fn parse_import_modifiers(&mut self) -> Result<Vec<ImportModifier>, Error> {
        let mut mods: Vec<ImportModifier> = Vec::new();
        // The current run of identifiers/unknown-functions, space-joined.
        let mut raw: Vec<TplPiece> = Vec::new();
        let flush = |raw: &mut Vec<TplPiece>, mods: &mut Vec<ImportModifier>| {
            if !raw.is_empty() {
                mods.push(ImportModifier::Raw(std::mem::take(raw)));
            }
        };
        let space = |raw: &mut Vec<TplPiece>| {
            if !raw.is_empty() {
                if let Some(TplPiece::Lit(s)) = raw.last_mut() {
                    s.push(' ');
                } else {
                    raw.push(TplPiece::Lit(" ".to_string()));
                }
            }
        };
        loop {
            self.skip_ws_trivia();
            if self.looking_at_interpolated_identifier() {
                let identifier = self.parse_interpolated_identifier()?;
                let name = tpl_plain(&identifier).map(|s| s.to_ascii_lowercase());
                if name.as_deref() != Some("and") && self.sc.peek() == Some('(') {
                    if name.as_deref() == Some("supports") {
                        self.sc.bump(); // '('
                        flush(&mut raw, &mut mods);
                        let (condition, declaration) = self.parse_import_supports_query()?;
                        if !self.sc.eat(')') {
                            return Err(Error::at("expected \")\"", self.sc.position()));
                        }
                        mods.push(ImportModifier::Supports {
                            condition,
                            declaration,
                        });
                    } else {
                        // Unknown function: `name(<declaration-value>)` verbatim.
                        self.sc.bump(); // '('
                        space(&mut raw);
                        raw.extend(identifier);
                        push_lit(&mut raw, '(');
                        let args = self.parse_supports_decl_value(true, true, true)?;
                        raw.extend(args);
                        if !self.sc.eat(')') {
                            return Err(Error::at("expected \")\"", self.sc.position()));
                        }
                        push_lit(&mut raw, ')');
                    }
                    continue;
                }
                // A bare identifier (or `and`).
                space(&mut raw);
                raw.extend(identifier);
                self.skip_ws_trivia();
                if self.sc.peek() == Some(',') {
                    // The identifier was a media type; the comma continues its
                    // media query LIST (only media queries may follow).
                    self.sc.bump();
                    flush(&mut raw, &mut mods);
                    let list = self.parse_media_query_list()?;
                    mods.push(ImportModifier::Media {
                        list,
                        comma_before: true,
                    });
                    return Ok(mods);
                }
                continue;
            } else if self.sc.peek() == Some('(') {
                // A media feature begins the terminal media query list.
                flush(&mut raw, &mut mods);
                let list = self.parse_media_query_list()?;
                mods.push(ImportModifier::Media {
                    list,
                    comma_before: false,
                });
                return Ok(mods);
            } else {
                flush(&mut raw, &mut mods);
                return Ok(mods);
            }
        }
    }

    /// dart-sass `_importSupportsQuery`: the content of an `@import ...
    /// supports(...)` modifier. Returns the condition and whether it was a bare
    /// declaration (`supports(a: b)`), whose serialization carries its own
    /// parens.
    fn parse_import_supports_query(&mut self) -> Result<(SupportsCondition, bool), Error> {
        self.skip_ws_trivia();
        if self.scan_keyword_ci("not") {
            self.skip_ws_trivia();
            let inner = self.parse_supports_condition_in_parens()?;
            return Ok((SupportsCondition::Negation(Box::new(inner)), false));
        }
        if self.sc.peek() == Some('(') {
            let condition = self.parse_supports_condition()?;
            let declaration = matches!(condition, SupportsCondition::Declaration { .. });
            return Ok((condition, declaration));
        }
        // `name(<args>)` function form (e.g. `supports(calc(1))`).
        if self.looking_at_interpolated_identifier() {
            let mark = self.sc.mark();
            let identifier = self.parse_interpolated_identifier()?;
            if self.sc.eat('(') {
                let arguments = self.parse_supports_decl_value(true, true, true)?;
                if !self.sc.eat(')') {
                    return Err(Error::at("expected \")\"", self.sc.position()));
                }
                return Ok((
                    SupportsCondition::Function {
                        name: identifier,
                        arguments,
                    },
                    false,
                ));
            }
            self.sc.reset(mark);
        }
        // Bare declaration: `<name>: <value>`.
        let name = self.parse_supports_decl_name()?;
        self.skip_ws_trivia();
        if !self.sc.eat(':') {
            return Err(Error::at("expected \":\".", self.sc.position()));
        }
        let custom = expr_is_custom_property(&name);
        let value = if custom {
            let raw = self.parse_supports_decl_value(false, false, true)?;
            SupportsValue::Raw(raw)
        } else {
            self.skip_ws_trivia();
            let v = self.parse_value()?;
            self.skip_ws_trivia();
            SupportsValue::Expr(v)
        };
        Ok((
            SupportsCondition::Declaration {
                name,
                value: Box::new(value),
                custom,
            },
            true,
        ))
    }

    /// Parse `@warn`/`@debug`/`@error <expr>;`.
    fn parse_message(&mut self, kind: MessageKind, pos: Pos, start_mark: Mark) -> Result<Stmt, Error> {
        self.skip_ws_inline();
        let value = self.parse_value()?;
        // Byte length of the `@error <expr>` span (from the `@` through the end
        // of the value, before the trailing `;`), for the diagnostic caret.
        let length = self.sc.byte_len_from(start_mark);
        self.skip_ws_inline();
        self.sc.eat(';');
        Ok(match kind {
            MessageKind::Warn => Stmt::Warn { value, pos },
            MessageKind::Debug => Stmt::Debug { value, pos },
            MessageKind::Error => Stmt::Error { value, pos, length },
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

    // ---- @supports --------------------------------------------------

    /// Parse `@supports <condition> { body }`. The condition is parsed into a
    /// structured [`SupportsCondition`] (dart-sass grammar) so it serializes
    /// canonically and malformed conditions are rejected; the body bubbles like
    /// any at-rule.
    fn parse_supports(&mut self) -> Result<Stmt, Error> {
        self.skip_ws_inline();
        let condition = self.parse_supports_condition()?;
        let body = self.parse_braced_body()?;
        Ok(Stmt::Supports { condition, body })
    }

    /// dart-sass `_supportsCondition`: an optional leading `not`, otherwise a
    /// condition-in-parens followed by a uniform `and`/`or` chain.
    fn parse_supports_condition(&mut self) -> Result<SupportsCondition, Error> {
        if self.scan_keyword_ci("not") {
            self.skip_ws_inline();
            let inner = self.parse_supports_condition_in_parens()?;
            return Ok(SupportsCondition::Negation(Box::new(inner)));
        }
        let mut condition = self.parse_supports_condition_in_parens()?;
        self.skip_ws_inline();
        let mut operator: Option<Conjunction> = None;
        while self.looking_at_plain_identifier() {
            match operator {
                Some(Conjunction::And) => self.expect_keyword_ci("and")?,
                Some(Conjunction::Or) => self.expect_keyword_ci("or")?,
                None => {
                    if self.scan_keyword_ci("or") {
                        operator = Some(Conjunction::Or);
                    } else {
                        self.expect_keyword_ci("and")?;
                        operator = Some(Conjunction::And);
                    }
                }
            }
            self.skip_ws_inline();
            let right = self.parse_supports_condition_in_parens()?;
            condition = SupportsCondition::Operation {
                left: Box::new(condition),
                right: Box::new(right),
                op: operator.unwrap_or(Conjunction::And),
            };
            self.skip_ws_inline();
        }
        Ok(condition)
    }

    /// dart-sass `_supportsConditionInParens`: a parenthesised condition, a
    /// function call, or a lone interpolation.
    fn parse_supports_condition_in_parens(&mut self) -> Result<SupportsCondition, Error> {
        if self.looking_at_interpolated_identifier() {
            let pos = self.sc.position();
            let identifier = self.parse_interpolated_identifier()?;
            if tpl_plain(&identifier).map(|s| s.eq_ignore_ascii_case("not")) == Some(true) {
                return Err(Error::at("\"not\" is not a valid identifier here.", pos));
            }
            if self.sc.eat('(') {
                let arguments = self.parse_supports_decl_value(true, true, true)?;
                if !self.sc.eat(')') {
                    return Err(Error::at("expected \")\"", self.sc.position()));
                }
                return Ok(SupportsCondition::Function {
                    name: identifier,
                    arguments,
                });
            } else if let Some(expr) = tpl_single_interp(identifier) {
                return Ok(SupportsCondition::Interpolation(expr));
            } else {
                return Err(Error::at("Expected @supports condition.", pos));
            }
        }

        let start = self.sc.position();
        if !self.sc.eat('(') {
            return Err(Error::at("expected \"(\"", start));
        }
        self.skip_ws_inline();
        if self.scan_keyword_ci("not") {
            self.skip_ws_inline();
            let condition = self.parse_supports_condition_in_parens()?;
            if !self.sc.eat(')') {
                return Err(Error::at("expected \")\"", self.sc.position()));
            }
            return Ok(SupportsCondition::Negation(Box::new(condition)));
        } else if self.sc.peek() == Some('(') {
            let condition = self.parse_supports_condition()?;
            if !self.sc.eat(')') {
                return Err(Error::at("expected \")\"", self.sc.position()));
            }
            return Ok(condition);
        }

        // Backtracking branch: try `<expression> ":" <value>` (a declaration),
        // and on failure re-parse as an interpolated identifier followed either
        // by an `and`/`or` operation or by an arbitrary "anything" value.
        let name_mark = self.sc.mark();
        let parsed = match self.parse_supports_decl_name() {
            Ok(name) => {
                self.skip_ws_inline();
                if self.sc.eat(':') {
                    Ok(Some(name))
                } else {
                    Ok(None)
                }
            }
            Err(e) => Err(e),
        };
        match parsed {
            Ok(Some(name)) => {
                let custom = expr_is_custom_property(&name);
                let value = if custom {
                    // A custom-property value is captured verbatim.
                    let raw = self.parse_supports_decl_value(false, false, true)?;
                    SupportsValue::Raw(raw)
                } else {
                    self.skip_ws_inline();
                    let v = self.parse_value()?;
                    // Consume any trailing whitespace/comments before `)`
                    // (dart-sass `_expression` leaves the cursor at `)`).
                    self.skip_ws_inline();
                    SupportsValue::Expr(v)
                };
                if !self.sc.eat(')') {
                    return Err(Error::at("expected \")\"", self.sc.position()));
                }
                Ok(SupportsCondition::Declaration {
                    name,
                    value: Box::new(value),
                    custom,
                })
            }
            _ => {
                self.sc.reset(name_mark);
                let identifier = self.parse_interpolated_identifier()?;
                match self.try_supports_operation(identifier)? {
                    Ok(op) => {
                        if !self.sc.eat(')') {
                            return Err(Error::at("expected \")\"", self.sc.position()));
                        }
                        Ok(op)
                    }
                    Err(mut contents) => {
                        // Otherwise parse an `<anything>` value (forbidding a
                        // top-level colon: a colon there means this was meant to
                        // be a declaration, so we report the missing-`:` error).
                        let rest = self.parse_supports_decl_value(true, true, false)?;
                        contents.extend(rest);
                        if self.sc.peek() == Some(':') {
                            return Err(Error::at("expected \":\".", self.sc.position()));
                        }
                        if !self.sc.eat(')') {
                            return Err(Error::at("expected \")\"", self.sc.position()));
                        }
                        Ok(SupportsCondition::Anything(contents))
                    }
                }
            }
        }
    }

    /// Parse the name expression of a `@supports` declaration: a space-/comma-
    /// separated SassScript expression that stops at a top-level `:` (dart-sass
    /// `_expression`). Unlike the general value grammar, a trailing `:` cleanly
    /// terminates the expression rather than erroring, so `(a /**/: b)` and
    /// `(a : b)` parse as declarations.
    fn parse_supports_decl_name(&mut self) -> Result<Expr, Error> {
        let mut commas: Vec<Expr> = Vec::new();
        loop {
            let mut spaces: Vec<Expr> = vec![self.or_expr()?];
            loop {
                let mark = self.sc.mark();
                let had_ws = self.skip_ws_inline();
                // Stop the space-list before a top-level `:`, a `,`, or any
                // value terminator; require whitespace to continue otherwise.
                if !had_ws || self.sc.peek() == Some(':') || self.at_value_terminator() {
                    self.sc.reset(mark);
                    break;
                }
                spaces.push(self.or_expr()?);
            }
            let element = if spaces.len() == 1 {
                spaces.pop().unwrap_or(Expr::Null)
            } else {
                Expr::List {
                    items: spaces,
                    sep: ListSep::Space,
                    bracketed: false,
                }
            };
            commas.push(element);
            let mark = self.sc.mark();
            self.skip_ws_inline();
            if self.sc.peek() == Some(',') {
                self.sc.bump();
                self.skip_ws_inline();
                if self.sc.peek() == Some(':') || self.at_value_terminator() {
                    self.sc.reset(mark);
                    break;
                }
                continue;
            }
            self.sc.reset(mark);
            break;
        }
        Ok(if commas.len() == 1 {
            commas.pop().unwrap_or(Expr::Null)
        } else {
            Expr::List {
                items: commas,
                sep: ListSep::Comma,
                bracketed: false,
            }
        })
    }

    /// dart-sass `_trySupportsOperation`: if a single-interpolation identifier
    /// is followed by `and`/`or`, parse the operation chain (`Ok`); otherwise
    /// restore the scanner position and return the identifier for reuse (`Err`).
    fn try_supports_operation(
        &mut self,
        mut identifier: Vec<TplPiece>,
    ) -> Result<Result<SupportsCondition, Vec<TplPiece>>, Error> {
        // Only a single-interpolation identifier can form an operation. Pull
        // the lone interpolation expression out; if it isn't one, give back the
        // identifier untouched.
        let expr = if matches!(identifier.as_slice(), [TplPiece::Interp(_)]) {
            match identifier.pop() {
                Some(TplPiece::Interp(e)) => e,
                other => {
                    // Unreachable given the guard above, but stay panic-free.
                    if let Some(p) = other {
                        identifier.push(p);
                    }
                    return Ok(Err(identifier));
                }
            }
        } else {
            return Ok(Err(identifier));
        };

        let before_ws = self.sc.mark();
        self.skip_ws_inline();
        let mut operation: Option<SupportsCondition> = None;
        let mut left_expr = Some(expr);
        let mut operator: Option<Conjunction> = None;
        while self.looking_at_plain_identifier() {
            match operator {
                Some(Conjunction::And) => self.expect_keyword_ci("and")?,
                Some(Conjunction::Or) => self.expect_keyword_ci("or")?,
                None => {
                    if self.scan_keyword_ci("and") {
                        operator = Some(Conjunction::And);
                    } else if self.scan_keyword_ci("or") {
                        operator = Some(Conjunction::Or);
                    } else {
                        self.sc.reset(before_ws);
                        // Reconstruct the single-interpolation identifier.
                        let e = left_expr.take().unwrap_or(Expr::Null);
                        return Ok(Err(vec![TplPiece::Interp(e)]));
                    }
                }
            }
            self.skip_ws_inline();
            let right = self.parse_supports_condition_in_parens()?;
            let left = match operation.take() {
                Some(op) => op,
                None => SupportsCondition::Interpolation(left_expr.take().unwrap_or(Expr::Null)),
            };
            operation = Some(SupportsCondition::Operation {
                left: Box::new(left),
                right: Box::new(right),
                op: operator.unwrap_or(Conjunction::And),
            });
            self.skip_ws_inline();
        }
        match operation {
            Some(op) => Ok(Ok(op)),
            None => {
                let e = left_expr.take().unwrap_or(Expr::Null);
                Ok(Err(vec![TplPiece::Interp(e)]))
            }
        }
    }

    /// dart-sass `_interpolatedDeclarationValue`, specialized for `@supports`
    /// (silent comments are dropped, loud comments kept; whitespace collapses;
    /// `#{…}` interpolation captured as template pieces). Stops at an unbalanced
    /// `)`/`}`/`]`, at `;` (unless `allow_semicolon`), at a top-level `:` (unless
    /// `allow_colon`), or at `{`. Errors "Expected token." when `!allow_empty`
    /// and nothing was read.
    fn parse_supports_decl_value(
        &mut self,
        allow_empty: bool,
        allow_semicolon: bool,
        allow_colon: bool,
    ) -> Result<Vec<TplPiece>, Error> {
        let start = self.sc.position();
        let mut pieces: Vec<TplPiece> = Vec::new();
        let mut lit = String::new();
        let mut brackets: Vec<char> = Vec::new();
        // Whether the immediately-preceding source character was a newline
        // (dart-sass `wroteNewline`/`peekChar(-1).isNewline`): used to preserve
        // indentation and collapse consecutive newlines.
        let mut prev_newline = false;
        let mut wrote_anything = false;
        while let Some(c) = self.sc.peek() {
            match c {
                '\\' => {
                    lit.push(c);
                    self.sc.bump();
                    if let Some(n) = self.sc.bump() {
                        lit.push(n);
                    }
                    prev_newline = false;
                    wrote_anything = true;
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
                    prev_newline = false;
                    wrote_anything = true;
                }
                '/' if self.sc.peek_at(1) == Some('*') => {
                    lit.push_str(&self.consume_loud_comment());
                    prev_newline = false;
                    wrote_anything = true;
                }
                '/' if self.sc.peek_at(1) == Some('/') => {
                    self.consume_silent_comment();
                    prev_newline = false;
                }
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
                    prev_newline = false;
                    wrote_anything = true;
                }
                ' ' | '\t'
                    if !prev_newline
                        && matches!(self.sc.peek_at(1), Some(w) if w == ' ' || w == '\t' || w == '\n' || w == '\r' || w == '\u{c}') =>
                {
                    // Collapse runs of whitespace to a single character, unless
                    // following a newline (then it's indentation, preserved).
                    self.sc.bump();
                }
                ' ' | '\t' => {
                    lit.push(c);
                    self.sc.bump();
                    wrote_anything = true;
                }
                '\n' | '\r' | '\u{c}' => {
                    // Collapse multiple newlines into one.
                    if !prev_newline {
                        lit.push('\n');
                        wrote_anything = true;
                    }
                    self.sc.bump();
                    prev_newline = true;
                }
                '(' | '[' | '{' => {
                    // Open brackets are always pushed (dart-sass
                    // `allowOpenBrace` defaults true): a `@supports` value reader
                    // always lives inside `(...)`, so the at-rule body `{` is
                    // never reached here — it follows the unbalanced `)`.
                    self.sc.bump();
                    lit.push(c);
                    brackets.push(match c {
                        '(' => ')',
                        '[' => ']',
                        _ => '}',
                    });
                    prev_newline = false;
                    wrote_anything = true;
                }
                ')' | ']' | '}' => {
                    let Some(expected) = brackets.last().copied() else {
                        break;
                    };
                    if c != expected {
                        return Err(Error::at(format!("expected \"{expected}\"."), self.sc.position()));
                    }
                    brackets.pop();
                    self.sc.bump();
                    lit.push(c);
                    prev_newline = false;
                    wrote_anything = true;
                }
                ';' => {
                    if !allow_semicolon && brackets.is_empty() {
                        break;
                    }
                    lit.push(c);
                    self.sc.bump();
                    prev_newline = false;
                    wrote_anything = true;
                }
                ':' => {
                    if !allow_colon && brackets.is_empty() {
                        break;
                    }
                    lit.push(c);
                    self.sc.bump();
                    prev_newline = false;
                    wrote_anything = true;
                }
                _ => {
                    lit.push(c);
                    self.sc.bump();
                    prev_newline = false;
                    wrote_anything = true;
                }
            }
        }
        if let Some(expected) = brackets.last() {
            return Err(Error::at(format!("expected \"{expected}\""), self.sc.position()));
        }
        if !lit.is_empty() {
            pieces.push(TplPiece::Lit(lit));
        }
        if !allow_empty && !wrote_anything {
            return Err(Error::at("Expected token.", start));
        }
        Ok(pieces)
    }

    /// dart-sass `_lookingAtInterpolatedIdentifier`: whether the cursor begins
    /// an identifier (possibly with `#{…}` interpolation).
    fn looking_at_interpolated_identifier(&self) -> bool {
        match self.sc.peek() {
            None => false,
            Some('\\') => true,
            Some('#') => self.sc.peek_at(1) == Some('{'),
            Some('-') => match self.sc.peek_at(1) {
                None => false,
                Some('#') => self.sc.peek_at(2) == Some('{'),
                Some('-') | Some('\\') => true,
                Some(c) => is_name_start_codepoint(c),
            },
            Some(c) => is_name_start_codepoint(c),
        }
    }

    /// Whether the cursor begins a plain (non-interpolated) identifier — used to
    /// detect a trailing `and`/`or` keyword in a supports condition.
    fn looking_at_plain_identifier(&self) -> bool {
        match self.sc.peek() {
            Some('\\') => true,
            Some('-') => {
                matches!(self.sc.peek_at(1), Some(c) if is_name_start_codepoint(c) || c == '-' || c == '\\')
            }
            Some(c) => is_name_start_codepoint(c),
            None => false,
        }
    }

    /// Parse a loud comment's body — the text after `/*`, consuming the closing
    /// `*/` — into template pieces so `#{…}` interpolation resolves at eval
    /// time. Literal text (including newlines) is preserved verbatim. An
    /// unterminated comment ends at EOF, matching dart-sass.
    fn parse_loud_comment_body(&mut self) -> Result<Vec<TplPiece>, Error> {
        let mut pieces = Vec::new();
        let mut lit = String::new();
        loop {
            match self.sc.peek() {
                None => break,
                Some('*') if self.sc.peek_at(1) == Some('/') => {
                    self.sc.bump();
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
                        return Err(Error::at("expected \"}\".", self.sc.position()));
                    }
                    pieces.push(TplPiece::Interp(e));
                }
                // CSS treats CR, FF, and CRLF as newlines inside a comment;
                // dart-sass normalizes them all to LF in the comment contents.
                Some(c @ ('\r' | '\u{c}')) => {
                    self.sc.bump();
                    if c == '\r' && self.sc.peek() == Some('\n') {
                        self.sc.bump();
                    }
                    lit.push('\n');
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

    /// dart-sass `interpolatedIdentifier`: an identifier with optional `#{…}`
    /// interpolation, returned as template pieces. Errors if no identifier.
    fn parse_interpolated_identifier(&mut self) -> Result<Vec<TplPiece>, Error> {
        let mut pieces: Vec<TplPiece> = Vec::new();
        let mut lit = String::new();
        // A leading `--` (custom-property-style) is always a valid start.
        if self.sc.peek() == Some('-') {
            lit.push('-');
            self.sc.bump();
            if self.sc.peek() == Some('-') {
                lit.push('-');
                self.sc.bump();
                self.interpolated_identifier_body(&mut pieces, &mut lit)?;
                if !lit.is_empty() {
                    pieces.push(TplPiece::Lit(lit));
                }
                return Ok(pieces);
            }
        }
        match self.sc.peek() {
            None => return Err(Error::at("Expected identifier.", self.sc.position())),
            Some('\\') => {
                if let Some(ch) = self.consume_escape()? {
                    lit.push(ch);
                }
            }
            Some('#') if self.sc.peek_at(1) == Some('{') => {
                self.sc.bump();
                self.sc.bump();
                let e = self.parse_value()?;
                self.skip_ws_inline();
                if !self.sc.eat('}') {
                    return Err(Error::at("expected \"}\"", self.sc.position()));
                }
                if !lit.is_empty() {
                    pieces.push(TplPiece::Lit(std::mem::take(&mut lit)));
                }
                pieces.push(TplPiece::Interp(e));
            }
            Some(c) if is_name_start_codepoint(c) => {
                lit.push(c);
                self.sc.bump();
            }
            _ => return Err(Error::at("Expected identifier.", self.sc.position())),
        }
        self.interpolated_identifier_body(&mut pieces, &mut lit)?;
        if !lit.is_empty() {
            pieces.push(TplPiece::Lit(lit));
        }
        Ok(pieces)
    }

    /// Consume the body of an interpolated identifier (name chars, escapes,
    /// `#{…}`) into the given piece/literal accumulators.
    fn interpolated_identifier_body(
        &mut self,
        pieces: &mut Vec<TplPiece>,
        lit: &mut String,
    ) -> Result<(), Error> {
        loop {
            match self.sc.peek() {
                Some(c) if c == '_' || c == '-' || c.is_ascii_alphanumeric() || (c as u32) >= 0x80 => {
                    lit.push(c);
                    self.sc.bump();
                }
                Some('\\') => {
                    if let Some(ch) = self.consume_escape()? {
                        lit.push(ch);
                    }
                }
                Some('#') if self.sc.peek_at(1) == Some('{') => {
                    self.reject_plain_css_interp()?;
                    self.sc.bump();
                    self.sc.bump();
                    let e = self.parse_value()?;
                    self.skip_ws_inline();
                    if !self.sc.eat('}') {
                        return Err(Error::at("expected \"}\"", self.sc.position()));
                    }
                    if !lit.is_empty() {
                        pieces.push(TplPiece::Lit(std::mem::take(lit)));
                    }
                    pieces.push(TplPiece::Interp(e));
                }
                _ => break,
            }
        }
        Ok(())
    }

    /// Scan a bare keyword (case-insensitive) when it stands as a complete
    /// identifier; on a match consume it and return true, else leave the
    /// position unchanged.
    fn scan_keyword_ci(&mut self, kw: &str) -> bool {
        let mark = self.sc.mark();
        let cs = self.sc.rest();
        let mut i = 0;
        while i < cs.len() && is_ident_char(cs[i]) {
            i += 1;
        }
        let word: String = cs[..i].iter().collect();
        if word.eq_ignore_ascii_case(kw) {
            for _ in 0..i {
                self.sc.bump();
            }
            true
        } else {
            self.sc.reset(mark);
            false
        }
    }

    /// Expect a bare keyword (case-insensitive); error in dart-sass's spelling
    /// (`Expected "and".`) when it doesn't match.
    fn expect_keyword_ci(&mut self, kw: &str) -> Result<(), Error> {
        if self.scan_keyword_ci(kw) {
            Ok(())
        } else {
            Err(Error::at(format!("Expected \"{kw}\"."), self.sc.position()))
        }
    }

    /// Parse `@at-root [query] { body }`. The optional query is the
    /// parenthesised `(with: …)` / `(without: …)` form; an inline selector
    /// (`@at-root .x { … }`) is desugared into a single rule inside the body.
    fn parse_at_root(&mut self) -> Result<Stmt, Error> {
        self.skip_ws_inline();
        let query = if self.sc.peek() == Some('(') {
            Some(self.parse_at_root_query()?)
        } else if self.sc.peek() == Some('{') {
            None
        } else {
            // The shorthand prelude is a SELECTOR; an at-rule there is
            // dart's "expected selector." (issue_238764 `@at-root @bar`).
            if self.sc.peek() == Some('@') {
                return Err(Error::at("expected selector.", self.sc.position()));
            }
            let selector_pos = self.sc.position();
            let selector = self.parse_template(&['{'])?;
            let (body, lines) = self.parse_braced_body_lines()?;
            return Ok(Stmt::AtRoot {
                query: None,
                body: vec![Stmt::Rule(Rule {
                    selector,
                    body,
                    selector_pos,
                    selector_interp_spans: Vec::new(),
                    brace_line: lines.start,
                    end_line: lines.end,
                })],
            });
        };
        // Whitespace / comments / newlines may sit between a `(…)` query (or
        // the bare `@at-root`) and the body block.
        self.skip_ws_trivia();
        let body = self.parse_braced_body()?;
        Ok(Stmt::AtRoot { query, body })
    }

    /// Parse and validate the `@at-root (with: …)` / `(without: …)` query
    /// (the scanner is on the opening `(`), returning the reconstructed query
    /// template for eval. dart-sass requires `( "with"|"without" : <expr> )`:
    /// a bad keyword is `Expected "with" or "without".`, a missing colon
    /// `expected ":".`, an empty value `Expected expression.`, and a stray
    /// token (e.g. a comma in the value, or junk after `)`) errors in turn
    /// (`expected ")".` / `expected "{".`) — none are silently accepted.
    fn parse_at_root_query(&mut self) -> Result<Vec<TplPiece>, Error> {
        self.sc.bump(); // '('
        self.skip_ws_inline();
        let kw_pos = self.sc.position();
        let kw = self.read_ident_name().unwrap_or_default();
        if !kw.eq_ignore_ascii_case("with") && !kw.eq_ignore_ascii_case("without") {
            return Err(Error::at("Expected \"with\" or \"without\".", kw_pos));
        }
        self.skip_ws_inline();
        if !self.sc.eat(':') {
            return Err(Error::at("expected \":\".", self.sc.position()));
        }
        self.skip_ws_inline();
        let value_pos = self.sc.position();
        // The value is a space-separated expression; a top-level comma ends it
        // and then dart expects `)` (`(with: rule, media)` → `expected ")".`).
        let value = self.parse_template(&[')', ','])?;
        if value
            .iter()
            .all(|p| matches!(p, TplPiece::Lit(s) if s.trim().is_empty()))
        {
            return Err(Error::at("Expected expression.", value_pos));
        }
        if !self.sc.eat(')') {
            return Err(Error::at("expected \")\".", self.sc.position()));
        }
        let mut pieces = vec![TplPiece::Lit(format!("({kw}: "))];
        pieces.extend(trim_prelude(value));
        pieces.push(TplPiece::Lit(")".to_string()));
        Ok(pieces)
    }

    /// Parse `@keyframes <name> { from {…} 50% {…} … }`. The body is parsed as
    /// ordinary statements; each frame block classifies as a rule (its keyframe
    /// selector is terminated by `{`). Parent resolution is suppressed at eval
    /// time so the frame selectors emit verbatim.
    fn parse_keyframes(&mut self, name: String) -> Result<Stmt, Error> {
        self.skip_ws_inline();
        let prelude = trim_prelude(self.parse_template(&['{'])?);
        let (body, lines) = self.parse_braced_body_lines()?;
        Ok(Stmt::Keyframes {
            name,
            prelude,
            body,
            lines,
        })
    }

    /// Parse a generic/unknown at-rule: `@name <prelude up to { ; or }>` then
    /// either a `{ … }` body or a terminating `;` (or an immediate `}` closing
    /// the enclosing block, as in `@page … {@g}`). Covers `@font-face`,
    /// `@page`, `@charset`, vendor `@foo`, and unknown directives.
    /// Parse the rest of an at-rule whose name contains interpolation:
    /// finish the name template, then a generic prelude and optional body.
    fn parse_interp_at_rule(&mut self, mut name: Vec<TplPiece>) -> Result<Stmt, Error> {
        loop {
            if self.sc.peek() == Some('#') && self.sc.peek_at(1) == Some('{') {
                self.sc.bump();
                self.sc.bump();
                self.skip_ws_inline();
                let e = self.parse_value()?;
                self.skip_ws_inline();
                if !self.sc.eat('}') {
                    return Err(Error::at("expected \"}\"", self.sc.position()));
                }
                name.push(TplPiece::Interp(e));
                continue;
            }
            match self.sc.peek() {
                Some(c) if is_ident_char(c) => {
                    let lit = self.read_ident_name()?;
                    name.push(TplPiece::Lit(lit));
                }
                _ => break,
            }
        }
        self.skip_ws_inline();
        let prelude = trim_prelude(self.parse_template_mode(&['{', ';', '}'], CommentMode::UnknownPrelude)?);
        self.skip_ws_inline();
        let body = if self.sc.peek() == Some('{') {
            Some(self.parse_braced_body()?)
        } else {
            self.sc.eat(';');
            None
        };
        Ok(Stmt::InterpAtRule { name, prelude, body })
    }

    fn parse_generic_at_rule(&mut self, name: String) -> Result<Stmt, Error> {
        self.skip_ws_inline();
        // dart-sass parses `@-moz-document` (only the exact lowercase spelling)
        // with a structured grammar that strips trivia comments between tokens;
        // every other at-rule keeps loud comments verbatim and treats silent
        // comments as whitespace. (`@supports` has its own dedicated parser.)
        let comment_mode = if name == "-moz-document" {
            CommentMode::StripTopLevel
        } else {
            CommentMode::UnknownPrelude
        };
        let prelude = self.parse_template_mode(&['{', ';', '}'], comment_mode)?;
        let prelude = trim_prelude(prelude);
        self.skip_ws_inline();
        let (body, lines) = if self.sc.peek() == Some('{') {
            let (body, lines) = self.parse_braced_body_lines()?;
            (Some(body), lines)
        } else {
            self.sc.eat(';');
            // The `;` form: the statement starts and ends on the `;` line.
            let line = self.sc.position().line as u32;
            (
                None,
                SrcLines {
                    file: 0,
                    start: line,
                    end: line,
                    col: 0,
                },
            )
        };
        Ok(Stmt::AtRule {
            name,
            prelude,
            body,
            lines,
        })
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
        let (body, lines) = self.parse_braced_body_lines()?;
        Ok(Stmt::Media { query, body, lines })
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
            // `@media only screen [and …]` — ident1 is the modifier (kept as
            // a template verbatim: it may contain interpolation, and dart
            // preserves its original case).
            let modifier = Some(ident1);
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
        Ok(MediaInParens::Feature(Box::new(feature)))
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
        // dart `_expressionUntilComparison`: a full SassScript expression
        // stopping at `<`, `>`, or a single `=` — `(screen and (color))`
        // evaluates as ONE boolean expression (issue_485: `and` returns its
        // second operand, so the feature renders `(color)`), while range
        // syntax `(width > 0)` still leaves the comparison to the media
        // grammar (the additive operand never consumes those).
        let mut lhs = self.media_and_chain()?;
        loop {
            let mark = self.sc.mark();
            self.skip_ws_inline();
            if !self.plain_css && self.try_keyword("or") {
                self.skip_ws_inline();
                let pos = self.sc.position();
                let rhs = self.media_and_chain()?;
                lhs = Expr::Binary {
                    op: BinOp::Or,
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

    /// The `and` level of [`Self::media_expression`], over additive operands.
    fn media_and_chain(&mut self) -> Result<Expr, Error> {
        let mut lhs = self.additive()?;
        loop {
            let mark = self.sc.mark();
            self.skip_ws_inline();
            if !self.plain_css && self.try_keyword("and") {
                self.skip_ws_inline();
                let pos = self.sc.position();
                let rhs = self.additive()?;
                lhs = Expr::Binary {
                    op: BinOp::And,
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
                // A media type/modifier may carry identifier escapes
                // (`@media screen\9`); decode and re-serialize canonically
                // (`screen\9 ` keeps the escape's terminating space).
                Some('\\') => {
                    let at_start = lit.is_empty() && pieces.is_empty();
                    let c = self.read_escape_char()?;
                    push_ident_escape(&mut lit, c, at_start);
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
        let name_pos = self.sc.position();
        let name = self.read_ident_name()?;
        // A user `@function` may not reuse a name the parser treats as a
        // SassScript operator or a special plain-CSS function (dart-sass).
        if is_function && is_reserved_function_name(&name) {
            return Err(Error::at("Invalid function name.", name_pos));
        }
        let params = self.parse_param_list()?;
        let body = self.parse_braced_body()?;
        // Unknown at-rules aren't allowed in a function body (parse-time in
        // dart-sass: "This at-rule is not allowed here.").
        if is_function {
            reject_at_rules_in(&body)?;
        }
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
            if has_interp {
                self.skip_ws_inline();
                // An interpolated property name follows the ordinary nested-
                // property rules: a value-less `{ … }` block expands each
                // child as `property-child` (`#{re}sult: {b: c}` emits
                // `result-b: c`) — unlike a literal `result`, whose value is
                // captured verbatim (braces and all).
                if self.sc.peek() == Some('{') {
                    self.sc.bump();
                    let mut children: Vec<(Vec<TplPiece>, Expr)> = Vec::new();
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
                        let child = trim_prelude(self.parse_template(&[':', '{', ';', '}'])?);
                        if !self.sc.eat(':') {
                            return Err(Error::at("expected \":\".", self.sc.position()));
                        }
                        self.skip_ws_inline();
                        let expr = self.parse_value()?;
                        self.skip_ws_inline();
                        self.sc.eat(';');
                        children.push((child, expr));
                    }
                    items.push(CssCustomItem {
                        property,
                        value: CssCustomValue::Set(children),
                    });
                    continue;
                }
                let expr = self.parse_value()?;
                self.skip_ws_inline();
                self.sc.eat(';');
                items.push(CssCustomItem {
                    property,
                    value: CssCustomValue::Script(expr),
                });
            } else {
                let raw = self.parse_css_custom_value()?;
                items.push(CssCustomItem {
                    property,
                    value: CssCustomValue::Raw(raw),
                });
            }
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

    /// Capture a custom-property (`--name`) declaration value verbatim, from
    /// just after the colon up to the terminating top-level `;` or `}`. This
    /// mirrors dart-sass `_interpolatedDeclarationValue`: `#{…}` interpolation
    /// resolves *everywhere* (including inside strings), `//` and `/* */` are
    /// literal value characters (not comments), strings and `()`/`[]`/`{}` keep
    /// their delimiters, and whitespace runs collapse to a single space. The
    /// captured pieces are trimmed of surrounding whitespace; the terminating
    /// `;` is left for the caller.
    fn parse_custom_property_value(&mut self) -> Result<Vec<TplPiece>, Error> {
        let mut pieces: Vec<TplPiece> = Vec::new();
        let mut lit = String::new();
        let mut depth = 0i32;
        // dart-sass `_interpolatedDeclarationValue` writes whitespace lazily:
        // a run of spaces/tabs collapses to its *last* character (a tab survives
        // a tab-only run), while a run containing a newline emits one `\n` plus
        // the following indentation verbatim. `wrote_newline` tracks whether the
        // most recently written character was a newline so indentation after a
        // newline is preserved and blank lines are not duplicated. The value is
        // NOT trimmed — leading whitespace after the colon and trailing
        // whitespace are both significant for a custom property.
        let mut wrote_newline = false;
        loop {
            match self.sc.peek() {
                None => break,
                Some(';') if depth == 0 => break,
                Some('}') if depth == 0 => break,
                Some('#') if self.sc.peek_at(1) == Some('{') => {
                    self.reject_plain_css_interp()?;
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
                    wrote_newline = false;
                }
                Some(q @ ('"' | '\'')) => {
                    // Capture the string, resolving interpolation inside it but
                    // keeping the quotes and other characters verbatim.
                    lit.push(q);
                    self.sc.bump();
                    loop {
                        match self.sc.peek() {
                            None => break,
                            Some('\\') => {
                                lit.push('\\');
                                self.sc.bump();
                                if let Some(esc) = self.sc.peek() {
                                    lit.push(esc);
                                    self.sc.bump();
                                }
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
                            Some(c) => {
                                lit.push(c);
                                self.sc.bump();
                                if c == q {
                                    break;
                                }
                            }
                        }
                    }
                    wrote_newline = false;
                }
                Some(c @ ('\n' | '\r' | '\u{c}')) => {
                    self.sc.bump();
                    // Treat `\r\n` as one newline.
                    if c == '\r' && self.sc.peek() == Some('\n') {
                        self.sc.bump();
                    }
                    if !wrote_newline {
                        lit.push('\n');
                        wrote_newline = true;
                    }
                }
                Some(c) if c == ' ' || c == '\t' => {
                    // Write this space/tab only if we just emitted a newline (so
                    // indentation is preserved) or it is the last whitespace of
                    // the run (so an inline run collapses to its final char).
                    let next_is_inline_ws = matches!(self.sc.peek_at(1), Some(' ') | Some('\t'));
                    if wrote_newline || !next_is_inline_ws {
                        lit.push(c);
                    }
                    self.sc.bump();
                }
                Some(c @ ('(' | '[' | '{')) => {
                    depth += 1;
                    lit.push(c);
                    self.sc.bump();
                    wrote_newline = false;
                }
                Some(c @ (')' | ']' | '}')) => {
                    depth -= 1;
                    lit.push(c);
                    self.sc.bump();
                    wrote_newline = false;
                }
                Some(c) => {
                    lit.push(c);
                    self.sc.bump();
                    wrote_newline = false;
                }
            }
        }
        if !lit.is_empty() {
            pieces.push(TplPiece::Lit(lit));
        }
        // A trailing newline-run collapses to a single space (dart-sass emits a
        // trailing newline before the terminator as a space, not a line break).
        if let Some(TplPiece::Lit(s)) = pieces.last_mut() {
            if s.ends_with('\n') {
                let trimmed_len = s.trim_end_matches([' ', '\t', '\n']).len();
                s.truncate(trimmed_len);
                s.push(' ');
            }
        }
        Ok(pieces)
    }

    /// Parse a declared parameter list `($a, $b: default, $rest...)`.
    /// Missing parentheses means no parameters.
    fn parse_param_list(&mut self) -> Result<ParamList, Error> {
        let mut params = Vec::new();
        let mut rest = None;
        // dart-sass forbids two parameters with the same name, treating `-` and
        // `_` as identical (`$a-b` and `$a_b` collide) — track the normalized
        // names seen so far and reject a repeat with "Duplicate parameter.".
        let mut seen: Vec<String> = Vec::new();
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
            let name_pos = self.sc.position();
            if !self.sc.eat('$') {
                return Err(Error::at("expected a parameter", self.sc.position()));
            }
            let name = self.read_variable_name()?;
            let norm = name.replace('_', "-");
            if seen.contains(&norm) {
                return Err(Error::at("Duplicate parameter.", name_pos));
            }
            seen.push(norm);
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
    fn parse_include(&mut self, pos: Pos, start_mark: Mark) -> Result<Stmt, Error> {
        self.skip_ws_inline();
        let include_name_pos = self.sc.position();
        let mut name = self.read_ident_name()?;
        // `@include --a` is reserved for plain CSS mixins (dart-sass), even
        // though `@mixin __a` normalizes to the same name.
        if name.starts_with("--") {
            return Err(Error::at(
                "Sass @mixin names beginning with -- are forbidden for \
                     forward-compatibility with plain CSS mixins.",
                include_name_pos,
            ));
        }
        // `@include ns.mixin(...)` — a namespaced mixin reference.
        let mut module = None;
        if self.sc.peek() == Some('.') {
            self.sc.bump();
            module = Some(name);
            let member_pos = self.sc.position();
            name = self.read_ident_name()?;
            if is_private_member(&name) {
                return Err(Error::at(
                    "Private members can't be accessed from outside their modules.",
                    member_pos,
                ));
            }
        }
        self.skip_ws_inline();
        let args = if self.sc.peek() == Some('(') {
            self.sc.bump();
            self.parse_args_after_paren()?
        } else {
            Vec::new()
        };
        // Byte length of `@include name(args)` (through the closing `)` or the
        // end of the name), excluding the content block / trailing `;`.
        let length = self.sc.byte_len_from(start_mark);
        self.skip_ws_inline();
        // An optional `using (params)` clause names the content block's
        // parameters, bound from the `@content(args)` call.
        let using_mark = self.sc.mark();
        let content_params = if self
            .read_ident_name()
            .ok()
            .is_some_and(|kw| kw.eq_ignore_ascii_case("using"))
        {
            self.skip_ws_inline();
            // `using` must be followed by a parenthesized parameter list.
            if self.sc.peek() != Some('(') {
                return Err(Error::at("expected \"(\".", self.sc.position()));
            }
            Some(Rc::new(self.parse_param_list()?))
        } else {
            self.sc.reset(using_mark);
            None
        };
        self.skip_ws_inline();
        let content = if self.sc.peek() == Some('{') {
            Some(Rc::new(self.parse_braced_body()?))
        } else if content_params.is_some() {
            // A `using (params)` clause requires a content block to bind into.
            return Err(Error::at("expected \"{\".", self.sc.position()));
        } else {
            self.sc.eat(';');
            None
        };
        Ok(Stmt::Include {
            name,
            args,
            content,
            content_params,
            module,
            pos,
            length,
        })
    }

    /// `@for $i from <start> through|to <end> { … }`. Bounds are parsed at
    /// the additive level so the `through`/`to` keywords are not swallowed
    /// into a space list.
    fn parse_for(&mut self) -> Result<Stmt, Error> {
        self.skip_ws_inline();
        if !self.sc.eat('$') {
            return Err(Error::at("expected a variable after @for", self.sc.position()));
        }
        let var = self.read_variable_name()?;
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
            vars.push(self.read_variable_name()?);
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
            self.skip_trivia(&mut discard)?;
            if self.sc.peek() != Some('@') {
                self.sc.reset(mark);
                break;
            }
            self.sc.bump(); // '@'
                            // `@elseif` is a deprecated spelling of `@else if` (dart still
                            // accepts it with an [elseif] deprecation warning).
            let kw = self.read_ident_name().unwrap_or_default();
            if kw == "elseif" {
                self.skip_ws_inline();
                let cond = self.parse_value()?;
                let body = self.parse_braced_body()?;
                branches.push(IfBranch {
                    cond: Some(cond),
                    body,
                });
                continue;
            }
            if kw != "else" {
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
        Ok(self.parse_braced_body_lines()?.0)
    }

    /// Parse a `{ … }` statement block, also reporting the `{`/`}` source
    /// lines (for the serializer's trailing-comment rule; `file` stays 0).
    fn parse_braced_body_lines(&mut self) -> Result<(Vec<Stmt>, SrcLines), Error> {
        self.skip_ws_inline();
        let brace_line = self.sc.position().line as u32;
        if !self.sc.eat('{') {
            return Err(Error::at("expected \"{\".", self.sc.position()));
        }
        self.block_depth += 1;
        let body = self.parse_statements(false);
        self.block_depth -= 1;
        let body = body?;
        if !self.sc.eat('}') {
            return Err(Error::at("expected \"}\"", self.sc.position()));
        }
        let lines = SrcLines {
            file: 0,
            start: brace_line,
            end: self.sc.position().line as u32,
            col: 0,
        };
        Ok((body, lines))
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

    /// In plain-CSS mode, a `#{…}` interpolation is rejected at its `#`.
    fn reject_plain_css_interp(&self) -> Result<(), Error> {
        if self.plain_css {
            return Err(Error::at(
                "Interpolation isn't allowed in plain CSS.",
                self.sc.position(),
            ));
        }
        Ok(())
    }

    /// Consume a `#{ … }` interpolation and return its expression. The caller
    /// must have verified the cursor is at `#` with `{` next.
    fn read_interp(&mut self) -> Result<Expr, Error> {
        self.sc.bump(); // '#'
        self.sc.bump(); // '{'
        let e = self.parse_value()?;
        self.skip_ws_inline();
        if !self.sc.eat('}') {
            return Err(Error::at("expected \"}\"", self.sc.position()));
        }
        Ok(e)
    }

    fn parse_template_mode(&mut self, stops: &[char], comments: CommentMode) -> Result<Vec<TplPiece>, Error> {
        let mut pieces = Vec::new();
        let mut lit = String::new();
        let mut paren = 0i32;
        let mut bracket = 0i32;
        // Whether a glued loud comment already joined a DeclName template
        // (dart appends at most ONE `rawText(loudComment)` to the name).
        let mut glued = false;
        while let Some(c) = self.sc.peek() {
            if paren == 0 && bracket == 0 && stops.contains(&c) {
                break;
            }
            // Comment handling depends on the mode and nesting depth.
            if c == '/' && comments != CommentMode::Keep {
                let top = paren == 0 && bracket == 0;
                let strip_here = match comments {
                    CommentMode::Strip | CommentMode::DeclName => true,
                    CommentMode::StripTopLevel | CommentMode::UnknownPrelude => top,
                    CommentMode::Keep => false,
                };
                if strip_here {
                    match self.sc.peek_at(1) {
                        Some('*') => {
                            // A loud comment is kept verbatim for unknown
                            // preludes (dart-sass preserves it) and when glued
                            // directly to a declaration name (issue_1422);
                            // otherwise it collapses to whitespace.
                            let glue_to_name = comments == CommentMode::DeclName
                                && !glued
                                && lit
                                    .chars()
                                    .last()
                                    .map_or(!pieces.is_empty(), |p| !p.is_whitespace());
                            if comments == CommentMode::UnknownPrelude || glue_to_name {
                                let text = self.consume_loud_comment();
                                lit.push_str(&text);
                                glued = true;
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
            // For unknown at-rule preludes dart collapses a whitespace run
            // WITHOUT a newline to one space and keeps a run WITH one from
            // its first newline (`@asdf a  b` → `a b`; `c \n   d` →
            // `c\n   d`). Comments terminate a run, so `foo //\n  bar`
            // keeps foo's trailing space AND the newline indent.
            if comments == CommentMode::UnknownPrelude && c.is_whitespace() {
                let mut run = String::new();
                while matches!(self.sc.peek(), Some(w) if w.is_whitespace()) {
                    if let Some(w) = self.sc.bump() {
                        run.push(w);
                    }
                }
                match run.find('\n') {
                    Some(nl) => lit.push_str(&run[nl..]),
                    None => lit.push(' '),
                }
                continue;
            }
            match c {
                '#' if self.sc.peek_at(1) == Some('{') => {
                    if self.plain_css {
                        return Err(Error::at(
                            "Interpolation isn't allowed in plain CSS.",
                            self.sc.position(),
                        ));
                    }
                    if !lit.is_empty() {
                        pieces.push(TplPiece::Lit(std::mem::take(&mut lit)));
                    }
                    self.sc.bump();
                    self.sc.bump();
                    // Record the expression span for a rule selector's
                    // dual-span diagnostic; nested template parses inside the
                    // expression suspend collection.
                    let span_start = self.sc.position();
                    let collect = std::mem::replace(&mut self.collect_interp_spans, false);
                    let e = self.parse_value()?;
                    self.skip_ws_inline();
                    self.collect_interp_spans = collect;
                    if collect {
                        let end = self.sc.position();
                        self.interp_spans.push((
                            span_start.line as u32,
                            span_start.col as u32,
                            end.col as u32,
                        ));
                    }
                    if !self.sc.eat('}') {
                        return Err(Error::at("expected \"}\"", self.sc.position()));
                    }
                    pieces.push(TplPiece::Interp(e));
                }
                '"' | '\'' => {
                    // The quote characters are literal prelude text, but `#{…}`
                    // inside the string is still interpolation (dart-sass resolves
                    // `"foo#{x}baz"` in an at-rule prelude to `"foo<x>baz"`).
                    lit.push(c);
                    self.sc.bump();
                    while let Some(ch) = self.sc.peek() {
                        if ch == '\\' {
                            lit.push(ch);
                            self.sc.bump();
                            if let Some(n) = self.sc.bump() {
                                lit.push(n);
                            }
                            continue;
                        }
                        if ch == '#' && self.sc.peek_at(1) == Some('{') {
                            if self.plain_css {
                                return Err(Error::at(
                                    "Interpolation isn't allowed in plain CSS.",
                                    self.sc.position(),
                                ));
                            }
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
                            continue;
                        }
                        lit.push(ch);
                        self.sc.bump();
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

    /// Read a variable name (after `$`), normalizing `_` to `-` like dart's
    /// `variableName()` — `$a_b` and `$a-b` are the SAME variable. Function /
    /// mixin and CSS identifiers keep their spelling (normalization for those
    /// happens at definition/lookup, so plain-CSS output preserves `_`).
    fn read_variable_name(&mut self) -> Result<String, Error> {
        let name = self.read_ident_name()?;
        if name.contains('_') {
            Ok(name.replace('_', "-"))
        } else {
            Ok(name)
        }
    }

    fn read_ident_name(&mut self) -> Result<String, Error> {
        let mut s = String::new();
        loop {
            match self.sc.peek() {
                Some(c) if is_ident_char(c) => {
                    self.sc.bump();
                    s.push(c);
                }
                // dart decodes identifier escapes at the lexer level —
                // `@w\61rn` IS `@warn` and `f\6Fo-bar` defines `foo-bar` —
                // so the stored name is the decoded spelling.
                Some('\\') => s.push(self.read_escape_char()?),
                _ => break,
            }
        }
        if s.is_empty() {
            return Err(Error::at("expected an identifier", self.sc.position()));
        }
        Ok(s)
    }

    /// Read a module namespace / forward prefix. dart-sass requires a real
    /// identifier here, so a digit-leading name (`@use "x" as 0`) is
    /// "Expected identifier." rather than a silently-accepted namespace.
    fn read_namespace_ident(&mut self) -> Result<String, Error> {
        if matches!(self.sc.peek(), Some(c) if c.is_ascii_digit()) {
            return Err(Error::at("Expected identifier.", self.sc.position()));
        }
        self.read_ident_name()
    }

    /// Decode a `\` escape inside an identifier (dart `escapeCharacter`):
    /// 1-6 hex digits (plus one optional trailing whitespace) become the
    /// code point — zero, surrogates, and out-of-range collapse to U+FFFD —
    /// and any other character is taken literally.
    fn read_escape_char(&mut self) -> Result<char, Error> {
        self.sc.bump(); // the backslash
        let first = match self.sc.peek() {
            None | Some('\n') | Some('\r') => {
                return Err(Error::at("Expected escape sequence.", self.sc.position()))
            }
            Some(c) => c,
        };
        if first.is_ascii_hexdigit() {
            let mut value: u32 = 0;
            for _ in 0..6 {
                match self.sc.peek() {
                    Some(c) if c.is_ascii_hexdigit() => {
                        value = (value << 4) + c.to_digit(16).unwrap();
                        self.sc.bump();
                    }
                    _ => break,
                }
            }
            // One optional whitespace terminator (dart `scanCharIf`).
            if matches!(self.sc.peek(), Some(' ' | '\t' | '\n' | '\r' | '\u{c}')) {
                self.sc.bump();
            }
            // NUL is KEPT (it re-serializes as `\0 `, dart's consume_escape);
            // surrogates and out-of-range code points become the replacement
            // character.
            if (0xD800..=0xDFFF).contains(&value) || value > 0x10FFFF {
                return Ok('\u{FFFD}');
            }
            Ok(char::from_u32(value).unwrap_or('\u{FFFD}'))
        } else {
            self.sc.bump();
            Ok(first)
        }
    }
}
