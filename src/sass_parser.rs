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
    /// Whether the current block is the body of a plain-CSS custom
    /// `@function --x()`/`@mixin --x()` (where a literal `result` declaration
    /// may not have an indented child block).
    in_css_callable: bool,
}

/// Whether `c` may appear in an identifier (mirrors the SCSS parser).
fn is_ident_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '-' || c == '_'
}

impl Transpiler {
    fn new(src: &str) -> Self {
        // Normalise line endings the way dart-sass does (it treats CR, CRLF and
        // form-feed as newlines for line-splitting purposes).
        let normalized = src.replace("\r\n", "\n").replace(['\r', '\u{c}'], "\n");
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
            in_css_callable: false,
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

        // A custom-property declaration (`--name: …`) takes its value verbatim
        // (a `//` is *not* a comment inside it), so handle it from the raw line
        // before the logical-line assembly strips silent comments.
        if trimmed.starts_with("--") {
            if let Some(()) = self.try_parse_custom_property_stmt(indent, line_no)? {
                return Ok(());
            }
        }

        // --- assemble the logical line (bracket / trailing-comma / `\`
        //     continuations) ---------------------------------------------
        let (mut logical, child_indent) = self.assemble_logical_line(indent)?;

        // A leading `\` escapes the statement into a *style rule* and is
        // consumed (the legacy `\:hover` form, distinguishing a pseudo-class
        // selector from the old `:prop val` declaration syntax; SCSS keeps
        // the backslash, the indented syntax drops it).
        if logical.starts_with('\\') {
            logical.remove(0);
            self.out.push_str(&logical);
            if !self.parse_child_into_braces(indent)? {
                self.out.push_str(" {}\n");
            }
            return Ok(());
        }

        // Rewrite the indented-syntax mixin shorthands to their `@mixin`/
        // `@include` equivalents *before* prelude continuation. `=name` defines
        // a mixin, and a bare `=` continues onto the next line like the
        // directive it stands for; `+name` (no space before the name) includes
        // one, but a bare `+` is the next-sibling combinator *selector* (like
        // `+ a` with a space), since `+` has a selector meaning `=` lacks.
        if let Some(rest) = logical.strip_prefix('=') {
            logical = format!("@mixin {}", rest.trim_start());
        } else if matches!(logical.strip_prefix('+'), Some(r) if r.starts_with(|c: char| is_ident_char(c) || c == '#' || c == '\\'))
        {
            let rest = logical[1..].trim_start();
            logical = format!("@include {rest}");
        }

        // A directive whose prelude is grammatically incomplete at the end of
        // its line continues onto the next (deeper-indented) line(s) — the
        // newline acts as whitespace inside the prelude. The remaining
        // deeper-indented lines after the prelude completes are its body.
        self.extend_directive_prelude(&mut logical, indent)?;

        // The indented syntax allows *unquoted* `@import` URLs (`@import foo,
        // sub/bar`, `@import other.css`); quote each bare URL token for the
        // SCSS grammar (which requires quoted strings or `url(…)`).
        if directive_name(&logical).as_deref() == Some("import") {
            logical = quote_import_urls(&logical);
        }

        // The statement keyword decides whether a `;` or a `{ … }` block is
        // appropriate, and handles custom props.
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
        let mut content = self.lines[start].content.trim_start().to_string();
        // An open `#{` interpolation spans lines as expression whitespace, not
        // comment-line structure: join continuation lines verbatim until it
        // closes (`/* #{a` + `+ b} */` is one comment containing `#{a\n+ b}`).
        let mut first_line_end = start + 1;
        while interp_open_anywhere(&content) && first_line_end < self.lines.len() {
            content.push('\n');
            content.push_str(&self.lines[first_line_end].indent_str);
            content.push_str(&self.lines[first_line_end].content);
            first_line_end += 1;
        }
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
            self.idx = first_line_end;
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
        // Unterminated on this line: gather the deeper-indented block as
        // comment body. dart-sass reindents a multi-line loud comment so the
        // first content line follows `/*` and each subsequent line keeps its
        // *source column*, with ` *` written across columns 0-1 (so content
        // never starts before column 3). Blank lines inside the block are
        // preserved as a bare ` *`; trailing blanks after the block are not.
        // Each entry is `(source_column, text)`; a blank line has no entry
        // text — modelled as None.
        let mut content_lines: Vec<Option<(usize, String)>> = Vec::new();
        // Line 0 content after the `/*` marker (drop it if only whitespace).
        let first = content.trim_start_matches("/*");
        if !first.trim().is_empty() {
            content_lines.push(Some((3, first.trim_start().to_string())));
        }
        self.idx = first_line_end;
        let mut closed = false;
        let mut pending_blanks = 0usize;
        while self.idx < self.lines.len() {
            let l = &self.lines[self.idx];
            if l.content.trim().is_empty() {
                pending_blanks += 1;
                self.idx += 1;
                continue;
            }
            if l.indent <= indent {
                break;
            }
            // Flush blanks only between block lines (a blank run *inside* the
            // comment renders as ` *` lines).
            for _ in 0..pending_blanks {
                content_lines.push(None);
            }
            pending_blanks = 0;
            let body = l.content.trim_end().to_string();
            content_lines.push(Some((l.indent, body.clone())));
            self.idx += 1;
            if body.contains("*/") {
                closed = true;
                break;
            }
        }
        if content_lines.is_empty() {
            content_lines.push(Some((3, String::new())));
        }
        if !closed {
            // dart-sass auto-closes an unterminated loud comment at the end of
            // its block, appending ` */` to the last line.
            if let Some(Some((_, last))) = content_lines.last_mut() {
                if last.is_empty() {
                    *last = "*/".to_string();
                } else {
                    last.push_str(" */");
                }
            }
        }
        for (i, line) in content_lines.iter().enumerate() {
            match line {
                Some((col, text)) => {
                    // `/*`/` *` occupy columns 0-1; pad so the text keeps its
                    // source column (minimum column 3).
                    let pad = col.max(&3) - 2;
                    if i == 0 {
                        self.out.push_str("/*");
                    } else {
                        self.out.push_str(" *");
                    }
                    for _ in 0..pad {
                        self.out.push(' ');
                    }
                    self.out.push_str(text);
                }
                None => self.out.push_str(" *"),
            }
            self.out.push('\n');
        }
        Ok(())
    }

    /// Assemble a logical line from `self.idx`, consuming bracket / trailing
    /// comma / backslash continuations. Returns the joined content and the
    /// indentation a child block (if any) must exceed (always the statement's
    /// own `indent`). Leaves `self.idx` past the last consumed continuation
    /// line. Errors on tab/space indentation mixing within the continuation.
    fn assemble_logical_line(&mut self, indent: usize) -> Result<(String, usize), Error> {
        let start = self.idx;
        let mut logical = strip_silent_comment(self.lines[start].content.trim_start());
        self.idx = start + 1;
        // A trailing `,` continues the line only in a selector context — a bare
        // declaration value (`b: c,`) does *not* wrap onto the next line in the
        // indented syntax. A directive prelude handles its own continuation
        // later, so a trailing comma there is also not consumed here.
        let comma_continues =
            !logical.starts_with('@') && !logical.starts_with('$') && find_decl_colon(&logical).is_none();
        // A *declaration value* mid-expression continues on a trailing binary
        // operator (`b: 3 %` + `2` is the modulo `3 % 2`).
        let op_continues =
            !logical.starts_with('@') && !logical.starts_with('$') && find_decl_colon(&logical).is_some();
        loop {
            if continuation_pending(&logical, comma_continues)
                || (op_continues && ends_with_value_operator(&logical))
            {
                // Need a following line to continue. Continuations join the
                // *immediate next physical line* (dart-sass does not skip blanks
                // here).
                if self.idx >= self.lines.len() {
                    break;
                }
                let next_content = self.lines[self.idx].content.clone();
                let next_indent_str = self.lines[self.idx].indent_str.clone();
                let st = scan_state(&logical);
                // Inside an open interpolation or loud comment, the next line's
                // text is captured verbatim (a `//` there is not a comment).
                let verbatim = st.in_interp || st.in_loud_comment || st.in_string;
                if next_content.trim().is_empty() {
                    if st.bracket_depth > 0 || verbatim {
                        // A blank line inside brackets/interp/comment joins as a
                        // newline.
                        logical.push('\n');
                        self.idx += 1;
                        continue;
                    }
                    break;
                }
                let joined = if verbatim {
                    next_content.clone()
                } else {
                    strip_silent_comment(&next_content)
                };
                // Backslash continuation: drop the trailing backslash and join
                // with a single space (the line "wraps"). Inside an open
                // quoted string the `\`+newline is a CSS line continuation:
                // it vanishes entirely and the next line's indentation
                // characters stay part of the string.
                if logical.ends_with('\\') {
                    logical.pop();
                    if st.in_string {
                        logical.push_str(&next_indent_str);
                        logical.push_str(&joined);
                    } else {
                        logical.push(' ');
                        logical.push_str(joined.trim_start());
                    }
                } else {
                    // Bracket / trailing-comma / interp / comment continuation:
                    // preserve the newline and the line's original indentation so
                    // the SCSS parser sees the same whitespace as dart-sass.
                    logical.push('\n');
                    logical.push_str(&next_indent_str);
                    logical.push_str(&joined);
                }
                self.idx += 1;
                continue;
            }
            break;
        }
        Ok((logical, indent))
    }

    /// While the directive prelude in `logical` is grammatically incomplete,
    /// pull in the next deeper-indented line(s) as prelude continuation. A
    /// newline acts as whitespace inside a directive prelude, so a directive may
    /// span several indented lines before its body block (which is whatever
    /// deeper-indented lines remain afterwards).
    fn extend_directive_prelude(&mut self, logical: &mut String, _indent: usize) -> Result<(), Error> {
        if !prelude_can_span(logical) {
            return Ok(());
        }
        while prelude_incomplete(logical) {
            // An incomplete prelude consumes the next non-blank line regardless
            // of its indentation: a newline acts as whitespace inside a prelude,
            // so the expression/clause grammar keeps reading until satisfied.
            let Some(i) = self.next_nonblank(self.idx) else {
                break;
            };
            let piece = strip_silent_comment(self.lines[i].content.trim_start());
            self.idx = i + 1;
            if piece.is_empty() {
                continue;
            }
            if !logical.is_empty() && !logical.ends_with(char::is_whitespace) {
                logical.push(' ');
            }
            logical.push_str(&piece);
            // Pull in any bracket continuations of this new line too (a
            // trailing comma only continues a `@use`/`@forward` member list).
            let comma_continues = matches!(directive_name(logical).as_deref(), Some("use" | "forward"));
            while continuation_pending(logical, comma_continues) {
                let Some(j) = self.next_nonblank(self.idx) else {
                    break;
                };
                let cont = strip_silent_comment(self.lines[j].content.trim_start());
                self.idx = j + 1;
                logical.push(' ');
                logical.push_str(&cont);
            }
        }
        Ok(())
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
        let mut logical = logical.trim();

        // A single trailing `;` ends a statement and is tolerated; a `;` with
        // further (non-comment) content after it means two statements share a
        // line, which the indented syntax forbids. An explicit trailing `;`
        // also means "no block" — even an otherwise block-owning directive
        // (`@a b;`) is then a leaf statement.
        let mut explicit_semicolon = false;
        if let Some(semi) = find_top_level_semicolon(logical) {
            // A trailing loud comment after the `;` is tolerated and dropped
            // (`b: c; /* f */`); anything else is two statements on a line.
            let after = trim_trailing_loud_comments(logical[semi + 1..].trim());
            if !after.is_empty() {
                let col = logical[..semi].chars().count();
                return Err(Error::at(
                    "multiple statements on one line are not supported in the indented syntax.".to_string(),
                    Pos {
                        line: line_no,
                        col: indent + col + 1,
                    },
                ));
            }
            // Drop the harmless trailing `;` (the transform re-adds the right
            // terminator).
            logical = logical[..semi].trim_end();
            explicit_semicolon = true;
        }

        // A leaf directive (e.g. `@import`, `@return`, `@extend`, `@charset`)
        // may not have anything indented beneath it.
        if let Some(name) = forbids_indented_child(logical) {
            if let Some(i) = self.next_nonblank(self.idx) {
                if self.lines[i].indent > indent {
                    return Err(Error::at(
                        format!("Nothing may be indented beneath a @{name} rule."),
                        Pos {
                            line: self.lines[i].line,
                            col: self.lines[i].indent + 1,
                        },
                    ));
                }
            }
        }

        // Whether this logical line wants a brace block (a rule / directive) or
        // a `;` terminator (a declaration / leaf directive) is decided after we
        // know whether a child block follows. We emit the prelude, then either a
        // block or — when no child block follows — the empty form appropriate to
        // the statement kind (an empty `{}` for block constructs, `;` otherwise).
        // A declaration WITH a value whose child block contains a line that
        // is not itself a `prop: value` declaration is dart-sass's
        // "expected \":\"." error (nested properties only) — e.g.
        // `b: c,` followed by a deeper-indented bare `d`.
        let is_decl_with_value = !logical.starts_with('@')
            && !logical.starts_with('$')
            && !logical.starts_with("--")
            && find_decl_colon(logical).is_some_and(|c| {
                let after = &logical[c + 1..];
                after.starts_with(char::is_whitespace) && !after.trim().is_empty()
            });
        if is_decl_with_value {
            if let Some(i) = self.next_nonblank(self.idx) {
                if self.lines[i].indent > indent {
                    let child_indent = self.lines[i].indent;
                    let mut j = i;
                    while j < self.lines.len() {
                        let l = &self.lines[j];
                        if l.content.trim().is_empty() {
                            j += 1;
                            continue;
                        }
                        if l.indent < child_indent {
                            break;
                        }
                        if l.indent == child_indent {
                            let t = l.content.trim_start();
                            if !t.starts_with("//") && !t.starts_with("/*") && find_decl_colon(t).is_none() {
                                return Err(Error::at(
                                    "expected \":\".".to_string(),
                                    Pos {
                                        line: l.line,
                                        col: l.content.trim_end().chars().count() + 1,
                                    },
                                ));
                            }
                        }
                        j += 1;
                    }
                }
            }
        }
        // Inside a plain-CSS custom callable body, a literal `result`
        // declaration may not have an indented child block (an interpolated
        // `#{result}:` follows the ordinary nested-property rules instead).
        if self.in_css_callable && !logical.contains("#{") {
            if let Some(colon) = find_decl_colon(logical) {
                if logical[..colon].trim().eq_ignore_ascii_case("result") {
                    if let Some(i) = self.next_nonblank(self.idx) {
                        if self.lines[i].indent > indent {
                            return Err(Error::at(
                                "Nothing may be indented beneath a @function result.".to_string(),
                                Pos {
                                    line: self.lines[i].line,
                                    col: self.lines[i].indent + 1,
                                },
                            ));
                        }
                    }
                }
            }
        }
        self.out.push_str(logical);
        // A `@function --x()`/`@mixin --x()` body is a plain-CSS custom
        // callable; flag it for the `result` child check above.
        let css_callable = matches!(directive_name(logical).as_deref(), Some("function" | "mixin"))
            && logical
                .trim_start()
                .trim_start_matches('@')
                .trim_start_matches(|c: char| is_ident_char(c))
                .trim_start()
                .starts_with("--");
        let saved_callable = self.in_css_callable;
        if css_callable {
            self.in_css_callable = true;
        }
        let had_block = self.parse_child_into_braces(indent)?;
        self.in_css_callable = saved_callable;
        if !had_block {
            let form = if explicit_semicolon {
                EmptyForm::Semicolon
            } else {
                empty_form(logical)
            };
            match form {
                EmptyForm::Braces => self.out.push_str(" {}\n"),
                EmptyForm::Semicolon => self.out.push_str(";\n"),
            }
        }
        Ok(())
    }

    /// Parse a custom-property declaration (`--name: value`) from the raw lines
    /// starting at `self.idx`. The value is captured verbatim — a `//` is not a
    /// comment inside it, and a deeper-indented child block continues the value
    /// — and emitted as an SCSS custom declaration. Returns `Some(())` when the
    /// line really is a custom property; otherwise leaves `self.idx` untouched
    /// and returns `None` so normal statement handling proceeds.
    fn try_parse_custom_property_stmt(
        &mut self,
        indent: usize,
        _line_no: usize,
    ) -> Result<Option<()>, Error> {
        let start = self.idx;
        let raw = self.lines[start].content.trim_start().to_string();
        // Confirm a top-level colon follows the `--name` token.
        let Some(colon) = find_decl_colon(&raw) else {
            return Ok(None);
        };
        let name = raw[..colon].trim_end();
        if !name.starts_with("--") || name.len() < 2 || name.contains(char::is_whitespace) {
            return Ok(None);
        }
        self.idx = start + 1;
        let mut value = raw[colon + 1..].trim_start().to_string();
        // Only an *open* bracket (`(`/`[`/`{`) or `#{` interpolation continues
        // the value onto following lines (verbatim, preserving each line's
        // source indentation); otherwise nothing may be indented beneath a
        // custom property (dart-sass error).
        while custom_value_open(&value) && self.idx < self.lines.len() {
            let l = &self.lines[self.idx];
            value.push('\n');
            value.push_str(&l.indent_str);
            value.push_str(&l.content);
            self.idx += 1;
        }
        if let Some(i) = self.next_nonblank(self.idx) {
            if self.lines[i].indent > indent {
                return Err(Error::at(
                    "Nothing may be indented beneath a custom property.".to_string(),
                    Pos {
                        line: self.lines[i].line,
                        col: self.lines[i].indent + 1,
                    },
                ));
            }
        }
        self.out.push_str(name);
        self.out.push_str(": ");
        self.out.push_str(value.trim_end());
        self.out.push_str(";\n");
        Ok(Some(()))
    }
}

/// The lowercased directive keyword of a logical line (`@for` -> `"for"`), or
/// `None` if the line is not an at-rule.
fn directive_name(logical: &str) -> Option<String> {
    let t = logical.trim_start();
    let rest = t.strip_prefix('@')?;
    let name: String = rest.chars().take_while(|c| is_ident_char(*c)).collect();
    if name.is_empty() {
        None
    } else {
        Some(name.to_ascii_lowercase())
    }
}

/// Whether a statement's prelude may span multiple lines (the prelude is an
/// expression / structured clause that the indented parser reads with the real
/// grammar, treating newlines as whitespace). True for the expression-bearing
/// directives and for `$variable` declarations.
fn prelude_can_span(logical: &str) -> bool {
    if logical.trim_start().starts_with('$') {
        return true;
    }
    matches!(
        directive_name(logical).as_deref(),
        Some(
            "for"
                | "each"
                | "if"
                | "else"
                | "while"
                | "function"
                | "mixin"
                | "include"
                | "return"
                | "warn"
                | "debug"
                | "error"
                | "extend"
                | "use"
                | "forward"
                | "at-root"
                | "content",
        )
    )
}

/// If `logical` is a leaf directive that forbids anything indented beneath it,
/// return its keyword (without the `@`). dart-sass: `@import`, `@charset`,
/// `@return`, `@extend`, `@error`, `@warn`, `@debug` and `@content` take no
/// child block. (`@use`/`@forward` permit a continued prelude, handled
/// separately, but no body either.)
fn forbids_indented_child(logical: &str) -> Option<String> {
    match directive_name(logical).as_deref() {
        Some(
            name @ ("import" | "charset" | "return" | "extend" | "error" | "warn" | "debug" | "content"
            | "use" | "forward"),
        ) => Some(name.to_string()),
        _ => None,
    }
}

/// Whether `c` is an operator/structural character that, when it ends a prelude
/// line, demands a following operand (so the prelude continues).
fn ends_with_pending_operator(t: &str) -> bool {
    let t = t.trim_end();
    // A trailing comma does NOT continue a directive prelude or variable
    // declaration in the indented syntax (`@each $a in b,` iterates the
    // single-element list `(b,)`; `$a: 1,` + indented line is "Nothing may
    // be indented beneath a variable declaration."). Only selector lists
    // continue on a comma, handled in `assemble_logical_line`.
    if t.ends_with('\\') {
        return true;
    }
    // Trailing binary/relational/arithmetic operators.
    for op in ["+", "-", "*", "/", "%", "<", ">", "=", ":"] {
        if t.ends_with(op) {
            return true;
        }
    }
    // Trailing keyword that requires more (case-insensitive whole word).
    let last_word: String = t
        .chars()
        .rev()
        .take_while(|c| is_ident_char(*c))
        .collect::<String>()
        .chars()
        .rev()
        .collect();
    matches!(
        last_word.to_ascii_lowercase().as_str(),
        "from"
            | "through"
            | "to"
            | "in"
            | "and"
            | "or"
            | "not"
            | "using"
            | "as"
            | "with"
            | "show"
            | "hide"
            | "if"
    )
}

/// Whether a directive prelude (the whole logical line so far) is grammatically
/// incomplete and therefore continues onto the next indented line.
fn prelude_incomplete(logical: &str) -> bool {
    // An unbalanced bracket always continues.
    if bracket_depth(logical) > 0 {
        return true;
    }
    // A `$variable` declaration continues until it has `$name: <value>` with a
    // non-empty value (`$a:` and `$a` both continue; `$a: b` is complete).
    let t = logical.trim_start();
    if t.starts_with('$') {
        if ends_with_pending_operator(t) {
            return true;
        }
        return match find_decl_colon(t) {
            Some(colon) => t[colon + 1..].trim().is_empty(),
            None => true,
        };
    }
    let Some(name) = directive_name(logical) else {
        return false;
    };
    // The prelude text after the directive keyword.
    let t = logical.trim_start();
    let after_at = &t[1..]; // skip '@'
    let prelude = after_at.strip_prefix(name.as_str()).unwrap_or(after_at).trim();
    if ends_with_pending_operator(prelude) {
        return true;
    }
    match name.as_str() {
        // `@for $i from <a> (through|to) <b>` — incomplete until both the
        // `from`/`through`/`to` keywords and operands are present.
        "for" => {
            let lower = prelude.to_ascii_lowercase();
            // Need the variable, `from`, an operand, `through`/`to`, an operand.
            if !lower.contains(" from ") && !lower.ends_with(" from") {
                // No `from` yet — but `@for $i` alone should continue.
                return !lower.contains("from");
            }
            // Have `from`; need `through`/`to` with an operand after it.
            let has_bound = lower.contains(" through ") || lower.contains(" to ");
            !has_bound
        }
        // `@each $v[, $k] in <list>` — incomplete until ` in ` appears.
        "each" => {
            let lower = prelude.to_ascii_lowercase();
            !(lower.contains(" in ") || lower.ends_with(" in"))
        }
        // `@if`/`@while`/`@else if` need a non-empty condition.
        "if" | "while" => prelude.is_empty(),
        "else" => {
            // `@else` is complete; `@else if` (no condition yet) needs more.
            let lower = prelude.to_ascii_lowercase();
            lower == "if" || lower.ends_with(" if")
        }
        // `@function`/`@mixin` need a name (and balanced parens if any).
        "function" | "mixin" => prelude.is_empty(),
        // `@return`/`@warn`/`@debug`/`@error`/`@extend` need an expression.
        "return" | "warn" | "debug" | "error" | "extend" => prelude.is_empty(),
        // `@include` needs a name.
        "include" => prelude.is_empty(),
        // `@use`/`@forward` member lists (`show a,`) continue on a comma —
        // their preludes treat newlines as whitespace throughout.
        "use" | "forward" if prelude.ends_with(',') => true,
        // `@use`/`@forward`/`@import` need a URL.
        "use" | "forward" | "import" => prelude.is_empty(),
        _ => false,
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
            // the newline (a child block, if present, is its content) — unless
            // it carries a `using (…)` clause, which always takes a content
            // block (empty here), since SCSS `@include a() using ();` is
            // 'expected "{".'.
            "include" if has_top_level_using(t) => EmptyForm::Braces,
            "include" | "return" | "import" | "use" | "forward" | "extend" | "content" | "warn" | "debug"
            | "error" | "charset" => EmptyForm::Semicolon,
            // Unknown / generic at-rules (`@font-face`, `@page`, `@namespace`,
            // vendor `@-foo`, …) are statements when nothing is indented beneath
            // them (`@namespace url(x)` -> `@namespace url(x);`); a child block,
            // when present, is handled before this point.
            _ => EmptyForm::Semicolon,
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
/// continue: an unbalanced bracket, an unterminated `#{…}` interpolation or
/// `/* … */` loud comment, or a trailing `,` or `\`.
fn continuation_pending(s: &str, comma_continues: bool) -> bool {
    let st = scan_state(s);
    if st.bracket_depth > 0 || st.in_interp || st.in_loud_comment {
        return true;
    }
    // Trailing complete loud comments are invisible to continuation
    // detection (`a, /* c */` still continues the selector list).
    let t = trim_trailing_loud_comments(s);
    if t.ends_with('\\') {
        return true;
    }
    // A trailing `!` awaits its `important` keyword on the next line
    // (`b: c!` + `important` joins; dart errors `Expected "important".`
    // when the next line is something else, which the joined SCSS parse
    // reproduces).
    if t.ends_with('!') {
        return true;
    }
    comma_continues && t.ends_with(',')
}

/// Quote the bare URL tokens of an indented-syntax `@import` for the SCSS
/// grammar: in each top-level comma part, an unquoted first token that is not
/// a `url(…)` call and contains no interpolation gets double quotes
/// (`@import foo, sub/bar` -> `@import "foo", "sub/bar"`); any following
/// modifier text is kept verbatim.
fn quote_import_urls(logical: &str) -> String {
    let Some(rest) = logical.trim_start().strip_prefix("@import") else {
        return logical.to_string();
    };
    let mut out = String::from("@import ");
    let mut first = true;
    for part in split_top_level_commas(rest.trim()) {
        if !first {
            out.push_str(", ");
        }
        first = false;
        let part = part.trim();
        let token_end = part.find(char::is_whitespace).unwrap_or(part.len());
        let (token, modifiers) = part.split_at(token_end);
        let bare = !token.is_empty()
            && !token.starts_with('"')
            && !token.starts_with('\'')
            && !token.contains("#{")
            && !token.to_ascii_lowercase().starts_with("url(");
        if bare {
            out.push('"');
            out.push_str(token);
            out.push('"');
        } else {
            out.push_str(token);
        }
        out.push_str(modifiers);
    }
    out
}

/// Split on top-level commas (outside brackets and quoted strings).
fn split_top_level_commas(s: &str) -> Vec<String> {
    let cs: Vec<char> = s.chars().collect();
    let mut parts = Vec::new();
    let mut cur = String::new();
    let mut depth = 0i32;
    let mut i = 0;
    while i < cs.len() {
        let c = cs[i];
        match c {
            '"' | '\'' => {
                cur.push(c);
                i += 1;
                while i < cs.len() {
                    cur.push(cs[i]);
                    if cs[i] == '\\' && i + 1 < cs.len() {
                        i += 1;
                        cur.push(cs[i]);
                    } else if cs[i] == c {
                        break;
                    }
                    i += 1;
                }
            }
            '(' | '[' => {
                depth += 1;
                cur.push(c);
            }
            ')' | ']' => {
                depth -= 1;
                cur.push(c);
            }
            ',' if depth == 0 => {
                parts.push(std::mem::take(&mut cur));
            }
            _ => cur.push(c),
        }
        i += 1;
    }
    parts.push(cur);
    parts
}

/// Whether a declaration value ends mid-expression with a binary operator
/// awaiting its right operand on the next line (`3 %`, `3 +`, `true and`,
/// `3 /`, `c -`). A `-` directly attached to an identifier character is part
/// of the identifier (`c-` does not continue); `=` only continues as part of
/// a comparison (`==`, `<=`, `>=`, `!=`).
fn ends_with_value_operator(s: &str) -> bool {
    let t = trim_trailing_loud_comments(s);
    if t.ends_with("==") || t.ends_with("<=") || t.ends_with(">=") || t.ends_with("!=") {
        return true;
    }
    if let Some(rest) = t.strip_suffix('-') {
        let prev = rest.chars().next_back();
        return !matches!(prev, Some(p) if is_ident_char(p));
    }
    if let Some(rest) = t.strip_suffix('%') {
        // `3%` is a percent unit (complete); only a detached `%` is the
        // modulo operator awaiting its right operand.
        let prev = rest.chars().next_back();
        return matches!(prev, Some(p) if p.is_whitespace());
    }
    if let Some(c) = t.chars().next_back() {
        if matches!(c, '+' | '*' | '/' | '<' | '>') {
            return true;
        }
    }
    // Trailing keyword operator (whole word, case-insensitive).
    let last_word: String = t
        .chars()
        .rev()
        .take_while(|c| is_ident_char(*c))
        .collect::<String>()
        .chars()
        .rev()
        .collect();
    if last_word.len() < t.trim_end().len() {
        // Must be preceded by whitespace/boundary to be the operator word.
        let before = &t[..t.len() - last_word.len()];
        if !before.ends_with(char::is_whitespace) {
            return false;
        }
    }
    matches!(last_word.to_ascii_lowercase().as_str(), "and" | "or" | "not")
}

/// Strip any run of complete trailing `/* … */` comments (and the whitespace
/// before them) from `s`.
fn trim_trailing_loud_comments(s: &str) -> &str {
    let mut t = s.trim_end();
    while t.ends_with("*/") {
        let Some(open) = t.rfind("/*") else {
            break;
        };
        // Only a comment that closes at the very end qualifies (the `*/` we
        // saw must belong to this `/*`).
        if !t[open..].ends_with("*/") || t[open + 2..t.len() - 2].contains("/*") {
            break;
        }
        t = t[..open].trim_end();
    }
    t
}

/// Strip a trailing `//` silent comment from a single line, respecting quoted
/// strings, `#{…}` interpolation and `/* */` loud comments (a `//` inside a
/// loud comment is not a silent comment). Returns the line with the comment (if
/// any) removed and trailing whitespace trimmed.
fn strip_silent_comment(s: &str) -> String {
    let cs: Vec<char> = s.chars().collect();
    let mut i = 0;
    let mut byte = 0usize;
    while i < cs.len() {
        let c = cs[i];
        match c {
            '"' | '\'' => {
                let q = c;
                byte += c.len_utf8();
                i += 1;
                while i < cs.len() && cs[i] != q {
                    if cs[i] == '\\' && i + 1 < cs.len() {
                        byte += cs[i].len_utf8();
                        i += 1;
                    }
                    byte += cs[i].len_utf8();
                    i += 1;
                }
                if i < cs.len() {
                    byte += cs[i].len_utf8();
                    i += 1;
                }
                continue;
            }
            '/' if cs.get(i + 1) == Some(&'*') => {
                // A loud comment: skip to its close (it may not close on this
                // line, in which case the rest is comment body — leave it).
                byte += 2;
                i += 2;
                while i + 1 < cs.len() && !(cs[i] == '*' && cs[i + 1] == '/') {
                    byte += cs[i].len_utf8();
                    i += 1;
                }
                if i + 1 < cs.len() {
                    byte += 2;
                    i += 2;
                }
                continue;
            }
            '/' if cs.get(i + 1) == Some(&'/') => {
                return s[..byte].trim_end().to_string();
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
            _ => {}
        }
        byte += c.len_utf8();
        i += 1;
    }
    s.trim_end().to_string()
}

/// Net bracket depth of `s` ignoring strings, `//`/`/* */` comments and `#{}`
/// interpolation.
fn bracket_depth(s: &str) -> i32 {
    scan_state(s).bracket_depth
}

/// The "openness" of a logical line: how many brackets remain open, whether the
/// line ends inside an unterminated `#{…}` interpolation, and whether it ends
/// inside an unterminated `/* … */` loud comment.
struct ScanState {
    bracket_depth: i32,
    in_interp: bool,
    in_loud_comment: bool,
    /// The line ends inside an unterminated quoted string.
    in_string: bool,
}

/// Scan `s` once, tracking strings, `//`/`/* */` comments and `#{…}`
/// interpolation, to report its closing state.
/// Whether a custom-property value ends with an open `(`/`[`/`{` bracket or
/// `#{` interpolation (so the next line continues it verbatim). Quoted
/// strings are skipped; a custom value's braces count as brackets.
fn custom_value_open(s: &str) -> bool {
    let cs: Vec<char> = s.chars().collect();
    let mut depth = 0i32;
    let mut i = 0;
    while i < cs.len() {
        match cs[i] {
            '"' | '\'' => {
                let q = cs[i];
                i += 1;
                while i < cs.len() && cs[i] != q {
                    if cs[i] == '\\' {
                        i += 1;
                    }
                    i += 1;
                }
            }
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth -= 1,
            _ => {}
        }
        i += 1;
    }
    depth > 0
}

/// Whether `s` ends inside an open `#{` interpolation, scanning *inside*
/// loud-comment text too (unlike [`scan_state`], which skips comment bodies):
/// a `#{` opens interpolation even within `/* … */`.
fn interp_open_anywhere(s: &str) -> bool {
    let cs: Vec<char> = s.chars().collect();
    let mut interp = 0i32;
    let mut i = 0;
    while i < cs.len() {
        match cs[i] {
            // Quoted strings only have meaning inside interpolation here.
            '"' | '\'' if interp > 0 => {
                let q = cs[i];
                i += 1;
                while i < cs.len() && cs[i] != q {
                    if cs[i] == '\\' {
                        i += 1;
                    }
                    i += 1;
                }
            }
            '#' if cs.get(i + 1) == Some(&'{') => {
                interp += 1;
                i += 2;
                continue;
            }
            '{' if interp > 0 => interp += 1,
            '}' if interp > 0 => interp -= 1,
            _ => {}
        }
        i += 1;
    }
    interp > 0
}

fn scan_state(s: &str) -> ScanState {
    let cs: Vec<char> = s.chars().collect();
    let mut depth = 0i32;
    // Stack of `#{` interpolation brace depths still open.
    let mut interp_depth = 0i32;
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
                if i >= cs.len() {
                    // Unterminated quoted string at end of line.
                    return ScanState {
                        bracket_depth: depth,
                        in_interp: interp_depth > 0,
                        in_loud_comment: false,
                        in_string: true,
                    };
                }
            }
            '/' if cs.get(i + 1) == Some(&'/') => break,
            '/' if cs.get(i + 1) == Some(&'*') => {
                i += 2;
                while i + 1 < cs.len() && !(cs[i] == '*' && cs[i + 1] == '/') {
                    i += 1;
                }
                if i + 1 >= cs.len() && !(cs.get(i) == Some(&'*') && cs.get(i + 1) == Some(&'/')) {
                    // Reached end of line without closing the loud comment.
                    return ScanState {
                        bracket_depth: depth,
                        in_interp: interp_depth > 0,
                        in_loud_comment: true,
                        in_string: false,
                    };
                }
                i += 2;
                continue;
            }
            '#' if cs.get(i + 1) == Some(&'{') => {
                i += 2;
                interp_depth += 1;
                continue;
            }
            '{' if interp_depth > 0 => interp_depth += 1,
            '}' if interp_depth > 0 => interp_depth -= 1,
            '(' | '[' => depth += 1,
            ')' | ']' => depth -= 1,
            _ => {}
        }
        i += 1;
    }
    ScanState {
        bracket_depth: depth,
        in_interp: interp_depth > 0,
        in_loud_comment: false,
        in_string: false,
    }
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

/// Byte index of the first top-level `;` in `logical` (outside strings,
/// brackets, `#{…}` interpolation and `/* */` comments), or `None`. A single
/// trailing `;` ends a statement; a `;` with further content is "multiple
/// statements on one line", which the indented syntax forbids.
/// Whether an `@include` logical line carries a top-level `using` keyword
/// (outside brackets and strings) — i.e. a content-block parameter clause.
fn has_top_level_using(logical: &str) -> bool {
    let cs: Vec<char> = logical.chars().collect();
    let mut depth = 0i32;
    let mut i = 0;
    while i < cs.len() {
        match cs[i] {
            '"' | '\'' => {
                let q = cs[i];
                i += 1;
                while i < cs.len() && cs[i] != q {
                    if cs[i] == '\\' {
                        i += 1;
                    }
                    i += 1;
                }
            }
            '(' | '[' => depth += 1,
            ')' | ']' => depth -= 1,
            'u' | 'U' if depth == 0 => {
                let prev_ident = i > 0 && is_ident_char(cs[i - 1]);
                let word: String = cs[i..].iter().take_while(|c| is_ident_char(**c)).collect();
                if !prev_ident && word.eq_ignore_ascii_case("using") {
                    return true;
                }
                i += word.len().max(1);
                continue;
            }
            _ => {}
        }
        i += 1;
    }
    false
}

fn find_top_level_semicolon(logical: &str) -> Option<usize> {
    let cs: Vec<char> = logical.chars().collect();
    let mut paren = 0i32;
    let mut bracket = 0i32;
    let mut byte = 0usize;
    let mut i = 0;
    while i < cs.len() {
        let c = cs[i];
        match c {
            '"' | '\'' => {
                let q = c;
                byte += c.len_utf8();
                i += 1;
                while i < cs.len() && cs[i] != q {
                    if cs[i] == '\\' && i + 1 < cs.len() {
                        byte += cs[i].len_utf8();
                        i += 1;
                    }
                    byte += cs[i].len_utf8();
                    i += 1;
                }
                if i < cs.len() {
                    byte += cs[i].len_utf8();
                    i += 1;
                }
                continue;
            }
            '/' if cs.get(i + 1) == Some(&'/') => break,
            '/' if cs.get(i + 1) == Some(&'*') => {
                byte += 2;
                i += 2;
                while i + 1 < cs.len() && !(cs[i] == '*' && cs[i + 1] == '/') {
                    byte += cs[i].len_utf8();
                    i += 1;
                }
                if i + 1 < cs.len() {
                    byte += 2;
                    i += 2;
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
            ';' if paren == 0 && bracket == 0 => return Some(byte),
            _ => {}
        }
        byte += c.len_utf8();
        i += 1;
    }
    None
}
