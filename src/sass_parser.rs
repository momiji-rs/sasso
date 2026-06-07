//! The indented (`.sass`) syntax front-end.
//!
//! The indented syntax describes the *same* language as SCSS — the same
//! statements, the same SassScript value grammar, the same AST — but block
//! structure comes from indentation and statement boundaries from newlines,
//! rather than from `{ … }` and `;`. dart-sass parses `.sass` into the very
//! same tree as `.scss`, so the evaluator and emitter are shared verbatim.
//!
//! This module is a *front-end only*: it reads the indentation-structured
//! source, recovers the block tree (handling multiline continuations, the
//! `=`/`+` mixin shorthands, `//`/`/* */` comments and custom-property
//! values), and reconstructs an equivalent brace/semicolon SCSS source which
//! it hands to the SCSS parser ([`crate::parser::parse`]). The whole
//! SassScript value/prelude/selector grammar is therefore reused unchanged.

use crate::ast::Stylesheet;
use crate::error::Error;
use crate::scanner::Pos;

/// Parse indented (`.sass`) source into the shared [`Stylesheet`] AST.
pub(crate) fn parse(src: &str) -> Result<Stylesheet, Error> {
    let scss = Transpiler::new(src).run()?;
    crate::parser::parse(&scss)
}

/// One physical source line, split into its indentation and content.
struct Raw {
    /// 1-based line number (for error positions).
    line: usize,
    /// Indentation width in columns (tabs and spaces, with tab == 1 column —
    /// dart-sass measures indentation in characters, and forbids mixing).
    indent: usize,
    /// The raw indentation characters (to detect tab/space mixing).
    indent_str: String,
    /// The line content with the leading indentation removed (trailing
    /// whitespace kept; it matters for continuation detection only after trim).
    content: String,
}

struct Transpiler {
    lines: Vec<Raw>,
    /// Cursor into `lines`.
    idx: usize,
    /// The assembled SCSS output.
    out: String,
}

/// Whether `c` may appear in an identifier (mirrors the SCSS parser).
fn is_ident_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '-' || c == '_'
}

impl Transpiler {
    fn new(src: &str) -> Self {
        // Normalise line endings the way dart-sass does (it treats CR, CRLF and
        // form-feed as newlines for line-splitting purposes).
        let normalized = src.replace("\r\n", "\n").replace('\r', "\n");
        let mut lines = Vec::new();
        for (i, physical) in normalized.split('\n').enumerate() {
            let mut indent = 0usize;
            let mut indent_str = String::new();
            let mut rest = physical;
            for (b, ch) in physical.char_indices() {
                if ch == ' ' || ch == '\t' {
                    indent += 1;
                    indent_str.push(ch);
                } else {
                    rest = &physical[b..];
                    break;
                }
                rest = &physical[b + ch.len_utf8()..];
            }
            lines.push(Raw {
                line: i + 1,
                indent,
                indent_str,
                content: rest.to_string(),
            });
        }
        Transpiler {
            lines,
            idx: 0,
            out: String::new(),
        }
    }

    /// Whether the line at `i` is blank (only whitespace).
    fn is_blank(&self, i: usize) -> bool {
        self.lines
            .get(i)
            .map(|l| l.content.trim().is_empty())
            .unwrap_or(true)
    }

    fn run(mut self) -> Result<String, Error> {
        // The base indentation is whatever the first non-blank line uses.
        self.parse_block(0, 0)?;
        Ok(self.out)
    }

    /// Find the next non-blank line index at or after `from`.
    fn next_nonblank(&self, from: usize) -> Option<usize> {
        let mut i = from;
        while i < self.lines.len() {
            if !self.is_blank(i) {
                return Some(i);
            }
            i += 1;
        }
        None
    }

    /// Parse a block whose statements are indented at exactly `block_indent`
    /// (statements at a *greater* indent belong to a child block). `parent_indent`
    /// is the indentation of the line that opened this block (or 0 at the root),
    /// used only to validate that a deeper indent is consistent. Emits SCSS into
    /// `self.out`.
    fn parse_block(&mut self, block_indent: usize, _parent_indent: usize) -> Result<(), Error> {
        loop {
            // Skip blank lines.
            let Some(i) = self.next_nonblank(self.idx) else {
                self.idx = self.lines.len();
                break;
            };
            self.idx = i;
            let indent = self.lines[i].indent;
            if indent < block_indent {
                // Dedent: this line belongs to an outer block.
                break;
            }
            if indent > block_indent {
                // A deeper indent with no statement to attach to is an error.
                return Err(Error::at(
                    "This line was indented unexpectedly.".to_string(),
                    Pos {
                        line: self.lines[i].line,
                        col: indent + 1,
                    },
                ));
            }
            self.parse_statement(block_indent)?;
        }
        Ok(())
    }

    /// Parse one statement starting at `self.idx` (indented at `indent`),
    /// consuming any continuation lines and its child block.
    fn parse_statement(&mut self, indent: usize) -> Result<(), Error> {
        let start = self.idx;
        let line_no = self.lines[start].line;

        // --- comments -----------------------------------------------------
        let trimmed = self.lines[start].content.trim_start().to_string();
        if trimmed.starts_with("//") {
            // Silent comment: drop it and any deeper-indented continuation
            // block (which is also silent).
            self.idx += 1;
            self.consume_child_block(indent);
            return Ok(());
        }
        if trimmed.starts_with("/*") {
            return self.parse_loud_comment(indent);
        }

        // --- assemble the logical line (bracket / trailing-comma / `\`
        //     continuations) ---------------------------------------------
        let (logical, child_indent) = self.assemble_logical_line(indent)?;

        // The statement keyword decides whether a `;` or a `{ … }` block is
        // appropriate, and handles the `=`/`+` shorthands and custom props.
        self.emit_statement(&logical, child_indent, indent, line_no)
    }

    /// Consume (discard) a child block deeper than `indent` — used for silent
    /// comments, whose nested lines are also part of the comment.
    fn consume_child_block(&mut self, indent: usize) {
        while let Some(i) = self.next_nonblank(self.idx) {
            if self.lines[i].indent <= indent {
                break;
            }
            self.idx = i + 1;
        }
    }

    /// A loud `/* … */` comment statement. It may close on the same line, or
    /// span a deeper-indented block (whose lines become comment text). dart-sass
    /// collapses such a block to ` <text>` joined by spaces.
    fn parse_loud_comment(&mut self, indent: usize) -> Result<(), Error> {
        let start = self.idx;
        let line_no = self.lines[start].line;
        let content = self.lines[start].content.trim_start().to_string();
        // Does the comment terminate on the same line?
        if let Some(end) = content.find("*/") {
            // Anything after the close (besides whitespace / another comment) is
            // an error in `.sass`.
            let after = content[end + 2..].trim_start();
            if !after.is_empty() && !after.starts_with("//") && !after.starts_with("/*") {
                return Err(Error::at(
                    "expected expression.".to_string(),
                    Pos {
                        line: line_no,
                        col: indent + end + 3,
                    },
                ));
            }
            self.out.push_str(&content[..end + 2]);
            self.out.push('\n');
            self.idx = start + 1;
            // A deeper-indented block after a closed comment is an error.
            if let Some(i) = self.next_nonblank(self.idx) {
                if self.lines[i].indent > indent {
                    return Err(Error::at(
                        "This line was indented unexpectedly.".to_string(),
                        Pos {
                            line: self.lines[i].line,
                            col: self.lines[i].indent + 1,
                        },
                    ));
                }
            }
            return Ok(());
        }
        // Unterminated on this line: gather the deeper-indented block as comment
        // body, normalising to a single collapsed comment.
        let mut text = content.trim_end().to_string();
        self.idx = start + 1;
        let mut closed = false;
        while let Some(i) = self.next_nonblank(self.idx) {
            if self.lines[i].indent <= indent {
                break;
            }
            let body = self.lines[i].content.trim();
            self.idx = i + 1;
            if let Some(end) = body.find("*/") {
                let piece = body[..end].trim_end();
                if !piece.is_empty() {
                    text.push(' ');
                    text.push_str(piece);
                }
                text.push_str(" */");
                closed = true;
                // strip duplicate trailing if comment already ended in */
                break;
            }
            if !body.is_empty() {
                text.push(' ');
                text.push_str(body);
            }
        }
        if !closed {
            // dart-sass auto-closes an unterminated loud comment at the end of
            // its block.
            text.push_str(" */");
        }
        // Collapse the leading "/*" + following text. `text` already starts
        // with "/*"; ensure exactly one space after it.
        let inner = text.trim_start_matches("/*").trim();
        self.out.push_str("/* ");
        self.out.push_str(inner);
        self.out.push('\n');
        Ok(())
    }

    /// Assemble a logical line from `self.idx`, consuming bracket / trailing
    /// comma / backslash continuations. Returns the joined content and the
    /// indentation a child block (if any) must exceed (always the statement's
    /// own `indent`). Leaves `self.idx` past the last consumed continuation
    /// line. Errors on tab/space indentation mixing within the continuation.
    fn assemble_logical_line(&mut self, indent: usize) -> Result<(String, usize), Error> {
        let start = self.idx;
        let mut logical = self.lines[start].content.trim_start().to_string();
        self.idx = start + 1;
        loop {
            if continuation_pending(&logical) {
                // Need a following line to continue. Use the next *physical*
                // line (continuations join physically, even across blanks is
                // disallowed; dart-sass joins the immediate next line).
                if self.idx >= self.lines.len() {
                    break;
                }
                let next = &self.lines[self.idx];
                if next.content.trim().is_empty() && self.bracket_depth(&logical) > 0 {
                    // blank line inside brackets — join as a space.
                    logical.push(' ');
                    self.idx += 1;
                    continue;
                }
                if next.content.trim().is_empty() {
                    break;
                }
                // Backslash continuation: drop the trailing backslash.
                if logical.ends_with('\\') {
                    logical.pop();
                }
                logical.push(' ');
                logical.push_str(next.content.trim_start());
                self.idx += 1;
                continue;
            }
            break;
        }
        Ok((logical, indent))
    }

    /// Net bracket depth (`(`+`[` minus `)`+`]`) of `s`, ignoring strings,
    /// comments and interpolation.
    fn bracket_depth(&self, s: &str) -> i32 {
        bracket_depth(s)
    }

    /// Parse the child block of the statement that begins at `self.idx`'s
    /// previous position; statements indented strictly deeper than `indent`
    /// form the block. Returns whether a block was found.
    fn parse_child_into_braces(&mut self, indent: usize) -> Result<bool, Error> {
        let Some(i) = self.next_nonblank(self.idx) else {
            return Ok(false);
        };
        let child_indent = self.lines[i].indent;
        if child_indent <= indent {
            return Ok(false);
        }
        self.out.push_str(" {\n");
        self.idx = i;
        self.parse_block(child_indent, indent)?;
        self.out.push_str("}\n");
        Ok(true)
    }

    /// Emit one statement (already assembled into `logical`), attaching its
    /// child block if present.
    fn emit_statement(
        &mut self,
        logical: &str,
        _child_indent: usize,
        indent: usize,
        line_no: usize,
    ) -> Result<(), Error> {
        let logical = logical.trim();

        // `=name(args)` defines a mixin; `+name(args)` includes one.
        if let Some(rest) = logical.strip_prefix('=') {
            let body = rest.trim_start();
            self.out.push_str("@mixin ");
            self.out.push_str(body);
            if !self.parse_child_into_braces(indent)? {
                self.out.push_str(" {}\n");
            }
            return Ok(());
        }
        if let Some(rest) = logical.strip_prefix('+') {
            let body = rest.trim_start();
            self.out.push_str("@include ");
            self.out.push_str(body);
            if !self.parse_child_into_braces(indent)? {
                self.out.push_str(";\n");
            }
            return Ok(());
        }

        // A custom-property declaration: `--name: value`. The value is captured
        // verbatim (including any deeper-indented block lines) and emitted with
        // a trailing `;` so the SCSS parser sees a custom declaration.
        if let Some(decl) = self.try_custom_property(logical, indent, line_no)? {
            self.out.push_str(&decl);
            return Ok(());
        }

        // Reject `.sass`-illegal punctuation up front to surface dart-sass-style
        // errors (a literal `{` / `;` is not allowed in indented syntax).
        if let Some(col) = illegal_punctuation(logical) {
            return Err(Error::at(
                "expected newline.".to_string(),
                Pos {
                    line: line_no,
                    col: indent + col + 1,
                },
            ));
        }

        // Whether this logical line wants a brace block (a rule / directive) or
        // a `;` terminator (a declaration / leaf directive) is decided after we
        // know whether a child block follows. We emit the prelude, then either a
        // block or — when no child block follows — the empty form appropriate to
        // the statement kind (an empty `{}` for block constructs, `;` otherwise).
        self.out.push_str(logical);
        if !self.parse_child_into_braces(indent)? {
            match empty_form(logical) {
                EmptyForm::Braces => self.out.push_str(" {}\n"),
                EmptyForm::Semicolon => self.out.push_str(";\n"),
            }
        }
        Ok(())
    }

    /// If `logical` is a custom-property declaration (`--name:` …), emit it as a
    /// verbatim SCSS custom declaration (consuming any child block as part of
    /// the value), returning the SCSS text. Otherwise `None`.
    fn try_custom_property(
        &mut self,
        logical: &str,
        indent: usize,
        _line_no: usize,
    ) -> Result<Option<String>, Error> {
        if !logical.starts_with("--") {
            return Ok(None);
        }
        // Confirm a top-level colon follows the `--name` token.
        let Some(colon) = find_decl_colon(logical) else {
            return Ok(None);
        };
        let name = logical[..colon].trim_end();
        if !name.starts_with("--") || name.len() < 2 {
            return Ok(None);
        }
        let mut value = logical[colon + 1..].trim_start().to_string();
        // A child block continues the custom-property value verbatim.
        if let Some(i) = self.next_nonblank(self.idx) {
            let child_indent = self.lines[i].indent;
            if child_indent > indent {
                self.idx = i;
                while let Some(j) = self.next_nonblank(self.idx) {
                    if self.lines[j].indent <= indent {
                        break;
                    }
                    if !value.is_empty() {
                        value.push('\n');
                    }
                    value.push_str(&self.lines[j].content);
                    self.idx = j + 1;
                }
            }
        }
        let mut s = String::new();
        s.push_str(name);
        s.push_str(": ");
        s.push_str(value.trim());
        s.push_str(";\n");
        Ok(Some(s))
    }
}

/// Which empty form a child-less statement takes in the reconstructed SCSS.
enum EmptyForm {
    /// A block construct with no body: append ` {}` (style rules, `@function`,
    /// `@mixin`, `@if`/`@else`, `@each`/`@for`/`@while`, `@media`, `@supports`,
    /// `@at-root`, `@keyframes`, generic block at-rules).
    Braces,
    /// A leaf statement: append `;` (declarations, variables, `@return`,
    /// `@include` without content, `@import`/`@use`/`@forward`/`@extend`,
    /// `@content`, `@warn`/`@debug`/`@error`, `@charset`, …).
    Semicolon,
}

/// Classify the empty (child-less) form of a statement from its logical line.
///
/// dart-sass: directives that own a block always parse a block (even an empty
/// one) in `.sass`; leaf directives and declarations end at the newline. A line
/// without a directive keyword is a style rule (`{}`) unless it is a declaration
/// (`prop: value`), in which case it is a leaf (`;`).
fn empty_form(logical: &str) -> EmptyForm {
    let t = logical.trim_start();
    if let Some(rest) = t.strip_prefix('@') {
        // The directive keyword (lowercased, up to the first non-ident char).
        let name: String = rest
            .chars()
            .take_while(|c| is_ident_char(*c))
            .collect::<String>()
            .to_ascii_lowercase();
        return match name.as_str() {
            // Block-owning directives.
            "function" | "mixin" | "if" | "else" | "each" | "for" | "while" | "media" | "supports"
            | "at-root" | "keyframes" | "-webkit-keyframes" | "-moz-keyframes" | "-o-keyframes"
            | "-ms-keyframes" => EmptyForm::Braces,
            // Leaf directives. `@include` without a child content block ends at
            // the newline (a child block, if present, is its content).
            "include" | "return" | "import" | "use" | "forward" | "extend" | "content" | "warn" | "debug"
            | "error" | "charset" => EmptyForm::Semicolon,
            // Unknown / generic at-rules (`@font-face`, `@page`, vendor, …) own a
            // block in dart-sass's `.sass` parser unless written as a statement.
            // We default them to a block so an empty `@foo` round-trips.
            _ => EmptyForm::Braces,
        };
    }
    // A `$variable: …` declaration is a leaf.
    if t.starts_with('$') {
        return EmptyForm::Semicolon;
    }
    // Otherwise: a declaration (`prop: value`) is a leaf; a bare selector is a
    // style rule. A top-level `:` with a non-empty value -> declaration.
    if let Some(colon) = find_decl_colon(t) {
        let value = t[colon + 1..].trim();
        // `prop:` with empty value is a nested property set (a block); `prop: v`
        // is a declaration (leaf). `a:hover` (no whitespace, has a value) reads
        // as a selector in SCSS but here we have no child block, so it is a
        // child-less rule -> braces. Distinguish by whitespace after the colon.
        let after = &t[colon + 1..];
        let ws_after = after.starts_with(char::is_whitespace) || after.is_empty();
        if value.is_empty() {
            // `prop:` alone with no block is an empty declaration value.
            return EmptyForm::Semicolon;
        }
        if ws_after {
            return EmptyForm::Semicolon;
        }
        // `a:hover` style with no block -> style rule.
        return EmptyForm::Braces;
    }
    EmptyForm::Braces
}

/// Whether a logical line, as assembled so far, needs another physical line to
/// continue: an unbalanced bracket, or a trailing `,` or `\`.
fn continuation_pending(s: &str) -> bool {
    if bracket_depth(s) > 0 {
        return true;
    }
    let t = s.trim_end();
    t.ends_with(',') || t.ends_with('\\')
}

/// Net bracket depth of `s` ignoring strings, `//`/`/* */` comments and `#{}`
/// interpolation.
fn bracket_depth(s: &str) -> i32 {
    let cs: Vec<char> = s.chars().collect();
    let mut depth = 0i32;
    let mut i = 0;
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
            }
            '/' if cs.get(i + 1) == Some(&'/') => break,
            '/' if cs.get(i + 1) == Some(&'*') => {
                i += 2;
                while i + 1 < cs.len() && !(cs[i] == '*' && cs[i + 1] == '/') {
                    i += 1;
                }
                i += 1;
            }
            '#' if cs.get(i + 1) == Some(&'{') => {
                i += 2;
                let mut d = 1;
                while i < cs.len() && d > 0 {
                    match cs[i] {
                        '{' => d += 1,
                        '}' => d -= 1,
                        _ => {}
                    }
                    i += 1;
                }
                continue;
            }
            '(' | '[' => depth += 1,
            ')' | ']' => depth -= 1,
            _ => {}
        }
        i += 1;
    }
    depth
}

/// Find the byte index of the top-level declaration colon in `logical` (the
/// `:` separating a property/custom-property name from its value), skipping
/// strings, brackets, comments and interpolation. Returns `None` if absent.
fn find_decl_colon(logical: &str) -> Option<usize> {
    let cs: Vec<char> = logical.chars().collect();
    let mut byte = 0usize;
    let mut paren = 0i32;
    let mut bracket = 0i32;
    let mut i = 0;
    while i < cs.len() {
        let c = cs[i];
        match c {
            '"' | '\'' => {
                let q = c;
                byte += c.len_utf8();
                i += 1;
                while i < cs.len() && cs[i] != q {
                    if cs[i] == '\\' {
                        byte += cs[i].len_utf8();
                        i += 1;
                    }
                    if i < cs.len() {
                        byte += cs[i].len_utf8();
                        i += 1;
                    }
                }
                if i < cs.len() {
                    byte += cs[i].len_utf8();
                    i += 1;
                }
                continue;
            }
            '#' if cs.get(i + 1) == Some(&'{') => {
                byte += c.len_utf8() + '{'.len_utf8();
                i += 2;
                let mut d = 1;
                while i < cs.len() && d > 0 {
                    match cs[i] {
                        '{' => d += 1,
                        '}' => d -= 1,
                        _ => {}
                    }
                    byte += cs[i].len_utf8();
                    i += 1;
                }
                continue;
            }
            '(' => paren += 1,
            ')' => paren -= 1,
            '[' => bracket += 1,
            ']' => bracket -= 1,
            ':' if paren == 0 && bracket == 0 => return Some(byte),
            _ => {}
        }
        byte += c.len_utf8();
        i += 1;
    }
    None
}

/// If `logical` contains a `.sass`-illegal `{` or `;` at top level (outside
/// strings/brackets/interpolation), return its char column (0-based). The
/// indented syntax never uses braces or semicolons; their appearance is an
/// error. A `;` is permitted only as the harmless `@content;` / trailing form
/// — but dart-sass errors on a bare `;`, so we report it too.
fn illegal_punctuation(logical: &str) -> Option<usize> {
    let cs: Vec<char> = logical.chars().collect();
    let mut paren = 0i32;
    let mut bracket = 0i32;
    let mut i = 0;
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
            }
            '/' if cs.get(i + 1) == Some(&'/') => break,
            '#' if cs.get(i + 1) == Some(&'{') => {
                i += 2;
                let mut d = 1;
                while i < cs.len() && d > 0 {
                    match cs[i] {
                        '{' => d += 1,
                        '}' => d -= 1,
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
            _ => {}
        }
        i += 1;
    }
    let _ = (paren, bracket);
    None
}

#[allow(dead_code)]
fn _ident_guard(c: char) -> bool {
    is_ident_char(c)
}
