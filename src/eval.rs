//! The evaluator: walks the AST, resolving variables, nesting (`&` and
//! the parent×child selector product), interpolation and arithmetic, and
//! flattens the result into a list of output rules.
//!
//! Like dart-sass (and unlike grass), a rule's own declarations are
//! gathered into a single block emitted *before* its nested rules bubble
//! out after it.

use std::cell::RefCell;
use std::rc::Rc;

// The compiler's internal maps are all keyed on short identifiers taken from
// the stylesheet being compiled, so they use the fast FxHash hasher rather than
// std's DoS-resistant-but-slow SipHash. Aliased to `HashMap` so the many type
// declarations below read normally; only the construction sites differ
// (`HashMap::default()` rather than `::new()`, since the hasher is non-default).
use crate::fxhash::FxHashMap as HashMap;

use crate::ast::{
    BinOp, CallArg, Callable, Conjunction, CssCustomItem, CssCustomValue, CustomDecl, Declaration, Expr,
    IfClause, IfCond, ImportArg, MediaFeature, MediaInParens, MediaQuery, MediaQueryList, ParamList,
    PropertySet, Rule, Stmt, Stylesheet, SupportsCondition, SupportsValue, TplPiece, UnOp, VarDecl,
};
use crate::error::Error;
use crate::scanner::Pos;
use crate::value::{CalcNode, CalcOp, List, ListSep, Map, Number, SassFunction, SassMixin, SassStr, Value};
use crate::{Importer, OutputStyle, Syntax};

/// Parse imported/`@use`d source with the front-end matching its file syntax.
fn parse_with_syntax(src: &str, syntax: Syntax) -> Result<crate::ast::Stylesheet, Error> {
    match syntax {
        Syntax::Scss => crate::parser::parse(src),
        Syntax::Css => crate::parser::parse_plain_css(src),
        Syntax::Sass => crate::sass_parser::parse(src),
    }
}

/// A call's evaluated arguments, split into positional values and named
/// `(name, value)` keyword pairs (after splat expansion).
type EvaledArgs = (Vec<Value>, Vec<(String, Value)>);

/// A flattened output node.
pub(crate) enum OutNode {
    Rule {
        selectors: Vec<String>,
        /// Per-complex "line break before" flags from the source selector list
        /// (`a,\nb` keeps the newline). Empty means none (all comma-joined with
        /// a space); otherwise parallel to `selectors`.
        linebreaks: Vec<bool>,
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
        /// A custom property (`--x`) whose value is emitted verbatim after the
        /// colon (no inserted space); its leading whitespace is part of `value`.
        custom: bool,
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
        /// A custom property (`--x`) whose value is emitted verbatim after the
        /// colon (no inserted space); its leading whitespace is part of `value`.
        custom: bool,
    },
    Comment(String),
    /// A childless at-rule (`@e f;`) that appears directly inside a style rule:
    /// dart-sass keeps it in the parent block (interleaved with declarations),
    /// unlike a block at-rule which bubbles out to the document root.
    ChildlessAtRule {
        name: String,
        prelude: String,
    },
    /// A style rule nested directly inside another, kept verbatim instead of
    /// flattened. Only produced in plain-CSS mode (a loaded `.css` file).
    NestedRule {
        selectors: Vec<String>,
        items: Vec<OutItem>,
    },
}

/// Which kind of module member an existence predicate (`function-exists`,
/// `mixin-exists`, `global-variable-exists` with a `$module`) queries.
#[derive(Clone, Copy)]
enum MemberKind {
    Function,
    Mixin,
    Variable,
}

/// Where evaluated statements deposit their output. At the top level each
/// statement forms its own blank-separated group; inside a style rule,
/// declarations join the rule's block and nested rules bubble out after it.
/// This is the seam that lets one block executor serve the top level, rule
/// bodies, and every nested-block construct (conditionals, loops, mixins).
enum Sink<'a> {
    Top(&'a mut Vec<OutNode>),
    Rule {
        /// The enclosing rule's resolved selector list, used to build a block
        /// node when the accumulated `items` must be flushed (i.e. when a nested
        /// rule or at-rule interrupts the parent's own declarations).
        selectors: &'a [String],
        /// Per-complex source line-break flags (parallel to `selectors`).
        linebreaks: &'a [bool],
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

    fn is_rule(&self) -> bool {
        matches!(self, Sink::Rule { .. })
    }

    /// Deposit a childless at-rule (`@e f;`). Inside a style rule it joins the
    /// parent's block (interleaved with declarations, in source order); at the
    /// top level or inside an at-rule body it is a bubbled-out `OutNode`.
    fn push_childless_at_rule(&mut self, name: String, prelude: String) {
        match self {
            Sink::Rule { items, .. } => items.push(OutItem::ChildlessAtRule { name, prelude }),
            _ => self.push_at_rule(OutNode::AtRule {
                name,
                prelude,
                body: Vec::new(),
                has_block: false,
            }),
        }
    }

    fn push_comment(&mut self, text: String) {
        // dart-sass strips a `/*# sourceMappingURL=… */` / `/*# sourceURL=… */`
        // loud comment (it generates its own); the `# ` space is required, so
        // `/*#sourceMappingURL…*/`, `/*! … */`, and other names are kept.
        if text.starts_with("# sourceMappingURL=") || text.starts_with("# sourceURL=") {
            return;
        }
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
                    custom,
                } => body.push(OutNode::AtDecl {
                    prop,
                    value,
                    important,
                    custom,
                }),
                OutItem::Comment(text) => body.push(OutNode::Comment(text)),
                OutItem::ChildlessAtRule { name, prelude } => body.push(OutNode::AtRule {
                    name,
                    prelude,
                    body: Vec::new(),
                    has_block: false,
                }),
                // A plain-CSS nested rule reaching an at-root sink becomes a
                // top-level rule carrying its items.
                OutItem::NestedRule { selectors, items } => body.push(OutNode::Rule {
                    selectors,
                    linebreaks: Vec::new(),
                    items,
                }),
            },
            Sink::Top(_) => {}
        }
    }

    /// Flush the parent rule's accumulated declarations/loud-comments into a
    /// block node, in source order, before a nested rule or at-rule is emitted.
    /// dart-sass splits the parent block around each bubbled child so that a
    /// declaration (or loud comment) following a nested rule appears AFTER that
    /// rule in the output. No-op for non-`Rule` sinks (which never accumulate a
    /// block) and when there are no pending items.
    fn flush_rule_block(&mut self) {
        if let Sink::Rule {
            selectors,
            linebreaks,
            items,
            nested,
        } = self
        {
            if !items.is_empty() {
                // A rule whose every complex selector was a dropped bogus
                // combinator has no selectors left, so it emits no block.
                if selectors.is_empty() {
                    items.clear();
                } else {
                    nested.push(OutNode::Rule {
                        selectors: selectors.to_vec(),
                        linebreaks: linebreaks.to_vec(),
                        items: std::mem::take(*items),
                    });
                }
            }
        }
    }

    /// Emit a produced style rule's fully interleaved output (its own block
    /// fragments plus the rules that bubbled out of it, in source order).
    fn emit_style_rule(&mut self, output: Vec<OutNode>) {
        match self {
            Sink::Top(out) => {
                let out = &mut **out;
                push_group(out, output);
            }
            Sink::Rule { .. } => {
                // Split the enclosing rule's own block around this nested rule.
                self.flush_rule_block();
                if let Sink::Rule { nested, .. } = self {
                    nested.extend(output);
                }
            }
            Sink::AtRoot(body) => body.extend(output),
        }
    }

    /// Deposit a produced at-rule (or `@at-root` output). At the top level it
    /// forms its own group; inside a style rule it joins the rules that bubble
    /// out to the document root (splitting the parent's own block around it);
    /// inside another at-rule's body it nests.
    fn push_at_rule(&mut self, node: OutNode) {
        match self {
            Sink::Top(out) => {
                let out = &mut **out;
                push_group(out, vec![node]);
            }
            Sink::Rule { .. } => {
                self.flush_rule_block();
                if let Sink::Rule { nested, .. } = self {
                    nested.push(node);
                }
            }
            Sink::AtRoot(body) => body.push(node),
        }
    }
}

/// Options visible to the evaluator (subset of the public `Options`).
pub(crate) struct EvalOptions<'a> {
    pub style: OutputStyle,
    pub importer: Option<&'a dyn Importer>,
    /// The entrypoint's source text, for rendering byte-exact diagnostic
    /// snippets. Empty when the embedder does not supply it (diagnostics then
    /// fall back to the legacy one-liner).
    pub source: &'a str,
    /// The entrypoint's file path/URL as it should appear in diagnostics
    /// (e.g. `input.scss`).
    pub url: &'a str,
    /// The glyph set for snippet/gutter decoration (ASCII under `--no-unicode`).
    pub glyphs: crate::diag::GlyphSet,
}

pub(crate) struct Evaluator<'a> {
    scopes: Vec<HashMap<String, Value>>,
    /// Whether each scope in `scopes` is "semi-global" (dart-sass): a control
    /// flow scope (`@for`/`@each`/`@while`/`@if`) that lets a fresh assignment
    /// reach the global scope, but only when every enclosing scope up to the
    /// root is itself semi-global. Rule/mixin/function scopes are not.
    scope_semi_global: Vec<bool>,
    options: EvalOptions<'a>,
    /// Import paths currently being loaded, deepest last. Re-entering one is a
    /// load cycle (dart-sass "This file is already being loaded."); a path that
    /// has finished loading may be imported again (`@import` re-evaluates).
    loading: Vec<String>,
    functions: HashMap<String, Rc<Callable>>,
    mixins: HashMap<String, Rc<Callable>>,
    /// Stack of `@content` blocks, one per active `@include`.
    content_stack: Vec<Option<ContentBlock>>,
    /// Whether we are *directly* executing a mixin body (dart-sass `_inMixin`).
    /// `true` is pushed while a mixin body runs; running a `@content` block or a
    /// function body pushes `false` (those execute in the caller's context), so
    /// `meta.content-exists()` errors there. Empty at the document root.
    in_mixin: Vec<bool>,
    /// The resolved query list of the enclosing `@media` context (empty at the
    /// document root). Used to merge nested `@media` queries.
    media_queries: Vec<ResolvedQuery>,
    /// The resolved selector list of the enclosing style rule, used to resolve
    /// `&` in value position. `None` at the document root (where `&` is `null`).
    /// Each element is one resolved complex selector (space-joined).
    current_selector: Option<Vec<String>>,
    /// Collected `@extend` directives, applied in a post-eval extension pass.
    extends: Vec<PendingExtend>,
    /// The current nested-property-set name prefix (e.g. `font` then `font-x`).
    /// Empty at the document root and inside ordinary rules; a child declaration
    /// emitted while this is non-empty is namespaced as `<prefix>-<name>`.
    decl_prefix: Option<String>,
    /// Whether we are evaluating the value of a `@supports` declaration. When
    /// set, `calc()` interiors are NOT simplified (dart-sass keeps
    /// `calc(1 + 2)` literal in `@supports (a: calc(1 + 2))`), matching
    /// dart-sass `_inSupportsDeclaration`.
    in_supports_declaration: bool,
    /// Built-in modules made available via `@use "sass:<mod>"`, keyed by the
    /// in-scope namespace (default = the part after `sass:`, or the `as ns`
    /// override). The value is the canonical built-in module name (e.g.
    /// `math`).
    used_modules: HashMap<String, String>,
    /// Built-in modules brought into scope unprefixed via `@use "sass:<mod>"
    /// as *`. Their members resolve as bare calls/variables.
    star_modules: Vec<String>,
    /// User stylesheet modules brought into scope via `@use "<file>" [as ns]`,
    /// keyed by the in-scope namespace.
    used_user_modules: HashMap<String, Rc<Module>>,
    /// User modules brought into scope unprefixed via `@use "<file>" as *`.
    star_user_modules: Vec<Rc<Module>>,
    /// All user modules loaded so far, keyed by the importer's canonical key so
    /// each file is evaluated once and shared between every `@use`/`@forward`.
    /// Shared so a module's own forwarded sub-modules see the same cache.
    module_cache: Rc<RefCell<HashMap<String, Rc<Module>>>>,
    /// Members re-exported from the module currently being evaluated, collected
    /// from its `@forward` rules. Empty when not evaluating a module.
    forwarded: Forwarded,
    /// Configuration (`@use/@forward ... with`) supplied for the module
    /// currently being evaluated, consumed by its `!default` declarations.
    /// Maps variable name -> (value, is_default_override).
    pending_config: HashMap<String, (Value, bool)>,
    /// Config keys actually consumed by a `!default` declaration in the module
    /// currently being evaluated (used to reject unused configuration).
    consumed_config: Vec<String>,
    /// The name of the member whose body is currently executing, as it appears
    /// in a diagnostic stack frame: `root stylesheet` at the entrypoint, or
    /// `<name>()` inside a user mixin/function. dart-sass `_member`.
    member: String,
    /// The diagnostic call stack: one entry per active user callable/import,
    /// recording the call site and the *caller's* member name. dart-sass
    /// `_stack`. Used to render byte-exact stack traces under errors/warnings.
    call_stack: Vec<DiagFrame>,
    /// The file path/URL the statements currently being evaluated came from
    /// (the entrypoint URL, or an imported/used partial's path).
    current_url: String,
    /// The source text of [`Self::current_url`], for rendering snippets that
    /// point into the currently-executing file.
    current_source: Rc<str>,
    /// Sources of every file seen so far, keyed by URL, so a stack trace can
    /// render a snippet that points into a file other than the current one.
    file_sources: Rc<RefCell<HashMap<String, Rc<str>>>>,
    /// Per-id count of deprecation warnings already *printed* (capped at 5 each,
    /// dart-sass). Keyed by the deprecation `[id]`.
    deprecations_shown: HashMap<&'static str, u32>,
    /// Total deprecation warnings *omitted* by the per-id cap, summed across
    /// ids; rendered into the aggregate footer at the end of the compile.
    deprecations_omitted: u32,
    /// Per-location dedup: a `(id, url, line, col)` already warned about is not
    /// warned about again (dart-sass collapses identical repeated warnings).
    deprecations_seen: std::collections::HashSet<(&'static str, String, usize, usize)>,
}

/// One frame of the diagnostic call stack: the call site (file + 1-based
/// position) and the name of the member that contained that call.
#[derive(Clone)]
struct DiagFrame {
    url: String,
    pos: Pos,
    /// The member name to print for this frame (`root stylesheet` or `name()`).
    member: String,
    /// Byte length of the call-site span, to size the snippet caret when this
    /// frame is the primary (innermost) one — used by `@error`.
    length: usize,
}

/// An evaluated user module: its public members plus the bindings it itself
/// `@use`d (so a `ns.member` lookup can recurse through forwards).
struct Module {
    /// Top-level variables (the module's global scope). Shared and mutable so an
    /// outside `ns.$var: value` assignment updates the module and its own
    /// functions/mixins observe the new value on their next call.
    vars: RefCell<HashMap<String, Value>>,
    functions: HashMap<String, Rc<Callable>>,
    mixins: HashMap<String, Rc<Callable>>,
    /// Namespaced user modules this module `@use`d (for transitive `ns.fn()`
    /// calls evaluated inside this module's own functions/mixins).
    used_user_modules: HashMap<String, Rc<Module>>,
    star_user_modules: Vec<Rc<Module>>,
    /// Built-in modules this module `@use`d, by namespace, and unprefixed.
    used_builtin_modules: HashMap<String, String>,
    star_builtin_modules: Vec<String>,
    /// Built-in `sass:*` modules re-exported via `@forward`. A `ns.member` that
    /// misses every captured member is retried against these.
    forwarded_builtins: Vec<ForwardedBuiltin>,
    /// The path/URL of this module's file, for diagnostic snippets pointing
    /// into the module (empty when diagnostics are disabled / unknown).
    diag_url: String,
}

impl Module {
    /// Look up a public variable. Names are dash/underscore-insensitive, so an
    /// exact miss falls back to comparing the canonical (dashed) form against
    /// every key. Private members (leading `-`/`_`) are the caller's
    /// responsibility to exclude.
    fn var(&self, name: &str) -> Option<Value> {
        let vars = self.vars.borrow();
        if let Some(v) = vars.get(name) {
            return Some(v.clone());
        }
        let norm = normalize_var_name(name);
        vars.iter()
            .find(|(k, _)| normalize_var_name(k) == norm)
            .map(|(_, v)| v.clone())
    }
    fn function(&self, name: &str) -> Option<Rc<Callable>> {
        if let Some(f) = self.functions.get(name) {
            return Some(Rc::clone(f));
        }
        let norm = normalize_var_name(name);
        self.functions
            .iter()
            .find(|(k, _)| normalize_var_name(k) == norm)
            .map(|(_, f)| Rc::clone(f))
    }
    fn mixin(&self, name: &str) -> Option<Rc<Callable>> {
        if let Some(m) = self.mixins.get(name) {
            return Some(Rc::clone(m));
        }
        let norm = normalize_var_name(name);
        self.mixins
            .iter()
            .find(|(k, _)| normalize_var_name(k) == norm)
            .map(|(_, m)| Rc::clone(m))
    }
}

/// A `@content` block plus, when the enclosing `@include` targets a mixin from
/// another module, a snapshot of the call-site environment in which the content
/// must run (dart-sass: a content block closes over its definition site).
struct ContentBlock {
    stmts: Rc<Vec<Stmt>>,
    /// The `using (params)` clause's parameters; `@content(args)` binds its
    /// arguments to these before the block runs.
    params: Option<Rc<ParamList>>,
    /// `Some` only for a cross-module include: the environment to install while
    /// the block runs, so the content resolves against the call site, not the
    /// mixin's module.
    caller_env: Option<Box<SavedModuleEnv>>,
}

/// The caller-side environment saved while a cross-module member call runs in
/// the callee module's environment.
#[derive(Clone)]
struct SavedModuleEnv {
    scopes: Vec<HashMap<String, Value>>,
    scope_semi_global: Vec<bool>,
    functions: HashMap<String, Rc<Callable>>,
    mixins: HashMap<String, Rc<Callable>>,
    used_modules: HashMap<String, String>,
    star_modules: Vec<String>,
    used_user_modules: HashMap<String, Rc<Module>>,
    star_user_modules: Vec<Rc<Module>>,
    /// When set (a cross-module call), the module whose global scope was
    /// installed: `leave_module` writes the (possibly mutated) global scope back
    /// so a `!global` assignment inside the module persists.
    write_back: Option<Rc<Module>>,
}

/// Members a module re-exports via `@forward`, accumulated while it evaluates.
#[derive(Default)]
struct Forwarded {
    vars: HashMap<String, Value>,
    functions: HashMap<String, Rc<Callable>>,
    mixins: HashMap<String, Rc<Callable>>,
    /// Built-in `sass:*` modules re-exported via `@forward "sass:x"`.
    builtins: Vec<ForwardedBuiltin>,
    /// For each re-exported member name, the source module it came from (by
    /// pointer). Re-forwarding the SAME module is idempotent (no conflict);
    /// forwarding a same-named member from a DIFFERENT module is a conflict.
    var_src: HashMap<String, *const Module>,
    fn_src: HashMap<String, *const Module>,
    mixin_src: HashMap<String, *const Module>,
}

/// A built-in module re-exported via `@forward "sass:x" [as p-*] [show|hide ...]`.
#[derive(Clone)]
struct ForwardedBuiltin {
    module: String,
    prefix: Option<String>,
    /// `show` allow-list of member names; `None` when no `show` clause.
    show: Option<std::collections::HashSet<String>>,
    /// `hide` deny-list of member names.
    hide: Option<std::collections::HashSet<String>>,
}

impl ForwardedBuiltin {
    /// Whether a re-exported built-in member (given by its bare, un-prefixed
    /// name) is visible through this forward.
    fn visible(&self, bare: &str) -> bool {
        if let Some(show) = &self.show {
            return show.contains(bare);
        }
        if let Some(hide) = &self.hide {
            return !hide.contains(bare);
        }
        true
    }
}

/// A pending `@extend`, captured during eval and applied after flattening.
struct PendingExtend {
    /// The resolved target simple selector (e.g. `.foo`, `%bar`).
    target: crate::selector::Simple,
    /// The resolved target selector string, for error messages.
    target_str: String,
    /// The enclosing rule's resolved selector list (the extenders).
    extenders: Vec<String>,
    optional: bool,
    /// Whether this `@extend` was registered inside a `@media` context.
    in_media: bool,
    pos: Pos,
}

impl<'a> Evaluator<'a> {
    pub(crate) fn new(options: EvalOptions<'a>) -> Self {
        let url = options.url.to_string();
        let source: Rc<str> = Rc::from(options.source);
        let file_sources: HashMap<String, Rc<str>> =
            [(url.clone(), Rc::clone(&source))].into_iter().collect();
        Evaluator {
            member: "root stylesheet".to_string(),
            call_stack: Vec::new(),
            current_url: url,
            current_source: source,
            file_sources: Rc::new(RefCell::new(file_sources)),
            deprecations_shown: HashMap::default(),
            deprecations_omitted: 0,
            deprecations_seen: std::collections::HashSet::new(),
            scopes: vec![HashMap::default()],
            // The global scope is treated as semi-global so a top-level control
            // flow scope (its child) becomes semi-global too.
            scope_semi_global: vec![true],
            options,
            loading: Vec::new(),
            functions: HashMap::default(),
            mixins: HashMap::default(),
            content_stack: Vec::new(),
            in_mixin: Vec::new(),
            media_queries: Vec::new(),
            current_selector: None,
            extends: Vec::new(),
            decl_prefix: None,
            in_supports_declaration: false,
            used_modules: HashMap::default(),
            star_modules: Vec::new(),
            used_user_modules: HashMap::default(),
            star_user_modules: Vec::new(),
            module_cache: Rc::new(RefCell::new(HashMap::default())),
            forwarded: Forwarded::default(),
            pending_config: HashMap::default(),
            consumed_config: Vec::new(),
        }
    }

    pub(crate) fn eval_sheet(&mut self, sheet: &Stylesheet, out: &mut Vec<OutNode>) -> Result<(), Error> {
        {
            let mut sink = Sink::Top(out);
            let r = self.exec(&sheet.stmts, &[], &mut sink);
            // At the outermost boundary, finalize any error into a rendered
            // diagnostic block (header + snippet + frames) if we have a span.
            if let Err(e) = r {
                let e = self.finalize_error(e);
                self.emit_deprecation_footer();
                return Err(e);
            }
        }
        self.apply_extends(out)?;
        hoist_css_imports(out);
        self.emit_deprecation_footer();
        Ok(())
    }

    // ---- diagnostic rendering -------------------------------------------

    /// Whether the entrypoint supplied source text (so snippets can render).
    fn diag_enabled(&self) -> bool {
        !self.options.source.is_empty()
    }

    /// Build the diagnostic stack-frame list (innermost first) for a primary
    /// span located at `pos` in `self.current_url`, executed by `self.member`.
    /// This is `[(current file, pos, member)]` followed by the recorded call
    /// stack, outermost last — exactly dart-sass's `_stackTrace`.
    fn frames_for(&self, pos: Pos) -> Vec<DiagFrame> {
        let mut frames = Vec::with_capacity(self.call_stack.len() + 1);
        frames.push(DiagFrame {
            url: self.current_url.clone(),
            pos,
            member: self.member.clone(),
            length: 0,
        });
        frames.extend(self.call_stack.iter().rev().cloned());
        frames
    }

    /// Render the stack-frame list into the column-aligned block dart-sass
    /// appends under a snippet/warning. `indent` is 2 (errors) or 4
    /// (warnings/deprecations).
    fn render_frame_block(frames: &[DiagFrame], indent: usize) -> String {
        // Column-align: pad each `<url> <line>:<col>` field to the longest.
        let fields: Vec<String> = frames
            .iter()
            .map(|f| format!("{} {}:{}", f.url, f.pos.line, f.pos.col))
            .collect();
        let width = fields.iter().map(String::len).max().unwrap_or(0);
        let pad: String = " ".repeat(indent);
        let mut out = String::new();
        for (i, (field, frame)) in fields.iter().zip(frames).enumerate() {
            if i > 0 {
                out.push('\n');
            }
            out.push_str(&pad);
            out.push_str(field);
            for _ in 0..width.saturating_sub(field.len()) {
                out.push(' ');
            }
            out.push_str("  ");
            out.push_str(&frame.member);
        }
        out
    }

    /// Look up the source text for `url`, defaulting to the current file's.
    fn source_for(&self, url: &str) -> Rc<str> {
        if url == self.current_url {
            return Rc::clone(&self.current_source);
        }
        self.file_sources
            .borrow()
            .get(url)
            .map(Rc::clone)
            .unwrap_or_else(|| Rc::clone(&self.current_source))
    }

    /// Convert an `Error` into a fully-rendered diagnostic block (header +
    /// snippet + 2-space frame trace), if diagnostics are enabled and the error
    /// carries a position. Idempotent: an already-rendered error is returned
    /// unchanged.
    fn finalize_error(&self, mut e: Error) -> Error {
        if e.rendered.is_some() || !self.diag_enabled() || !e.has_position() {
            return e;
        }
        let frames = self.frames_for(Pos {
            line: e.line,
            col: e.col,
        });
        e.rendered = Some(self.render_error_with_frames(&e, &frames));
        e
    }

    /// Render `Error: <msg>` + the snippet pointing at the innermost frame +
    /// the 2-space-indented frame trace.
    fn render_error_with_frames(&self, e: &Error, frames: &[DiagFrame]) -> String {
        let primary = &frames[0];
        let source = self.source_for(&primary.url);
        // Prefer the primary frame's own span length (set for `@error`'s call
        // site); otherwise the error's recorded length.
        let length = if primary.length > 0 {
            primary.length
        } else {
            e.length
        };
        let span = crate::diag::Span {
            line: primary.pos.line,
            col: primary.pos.col,
            length,
        };
        let mut out = format!("Error: {}\n", e.message);
        out.push_str(&crate::diag::render_snippet(
            &source,
            span,
            &[],
            self.options.glyphs,
        ));
        out.push('\n');
        out.push_str(&Self::render_frame_block(frames, 2));
        out
    }

    /// Emit a deprecation warning at `pos` (caret length `len`): the header
    /// block + a snippet pointing at the deprecated construct + a 4-space stack
    /// trace + a trailing blank line. Honours dart-sass's per-location dedup and
    /// per-id cap of 5 (further occurrences are counted into the aggregate
    /// footer rendered by [`Self::emit_deprecation_footer`]). No-op when
    /// diagnostics are disabled.
    fn emit_deprecation(&mut self, dep: &crate::deprecation::Deprecation, pos: Pos, len: usize) {
        if !self.diag_enabled() {
            return;
        }
        // Per-location dedup: an identical (id, file, line, col) warning fires
        // only once.
        let key = (dep.id, self.current_url.clone(), pos.line, pos.col);
        if !self.deprecations_seen.insert(key) {
            return;
        }
        let count = self.deprecations_shown.entry(dep.id).or_insert(0);
        if *count >= 5 {
            self.deprecations_omitted += 1;
            return;
        }
        *count += 1;

        let frames = self.frames_for(pos);
        let span = crate::diag::Span {
            line: pos.line,
            col: pos.col,
            length: len,
        };
        let source = self.source_for(&self.current_url.clone());
        let mut block = dep.render_header();
        block.push_str(&crate::diag::render_snippet(
            &source,
            span,
            &[],
            self.options.glyphs,
        ));
        block.push('\n');
        block.push_str(&Self::render_frame_block(&frames, 4));
        eprintln!("{block}\n");
    }

    /// Emit the aggregate "N repetitive deprecation warnings omitted" footer at
    /// the end of the compile, if the per-id cap dropped any warnings.
    fn emit_deprecation_footer(&self) {
        if self.deprecations_omitted == 0 {
            return;
        }
        eprintln!(
            "WARNING: {} repetitive deprecation warnings omitted.\nRun in verbose mode to see all warnings.\n",
            self.deprecations_omitted
        );
    }

    /// Enter a user callable (mixin/function) for diagnostics: record a stack
    /// frame at `call_pos` (caret length `call_len`) attributed to the *current*
    /// member, then make `new_member` the current member. Returns the previous
    /// member name, to be restored by [`Self::leave_call`].
    fn enter_call(&mut self, call_pos: Pos, call_len: usize, new_member: &str) -> String {
        self.call_stack.push(DiagFrame {
            url: self.current_url.clone(),
            pos: call_pos,
            member: self.member.clone(),
            length: call_len,
        });
        std::mem::replace(&mut self.member, new_member.to_string())
    }

    /// Leave a user callable: pop its diagnostic frame and restore `member`.
    fn leave_call(&mut self, saved_member: String) {
        self.call_stack.pop();
        self.member = saved_member;
    }

    /// Execute a `@warn`: emit `WARNING: <message>` + the 4-space-indented stack
    /// trace + a trailing blank line to stderr. The message is the string value
    /// unquoted; exit code is unaffected.
    fn emit_warn(&mut self, value: &Expr, pos: Pos) -> Result<(), Error> {
        let v = self.eval_expr(value)?;
        let msg = v.to_message();
        if self.diag_enabled() {
            let frames = self.frames_for(pos);
            eprintln!("WARNING: {}\n{}\n", msg, Self::render_frame_block(&frames, 4));
        } else {
            eprintln!("WARNING: {msg}");
        }
        Ok(())
    }

    /// Execute a `@debug`: emit `<path>:<line> DEBUG: <value>` to stderr (the
    /// value serialized as in CSS, a string unquoted). No snippet, no frames.
    fn emit_debug(&mut self, value: &Expr, pos: Pos) -> Result<(), Error> {
        let v = self.eval_expr(value)?;
        let msg = v.to_message();
        if self.diag_enabled() {
            eprintln!("{}:{} DEBUG: {}", self.current_url, pos.line, msg);
        } else {
            eprintln!("DEBUG: {msg}");
        }
        Ok(())
    }

    /// Build the error for an `@error`: its message is the serialized argument
    /// (a string keeps its quotes). The snippet points at the innermost active
    /// call site (so an `@error` inside a mixin highlights the `@include`), or
    /// at the `@error` statement itself when there is no enclosing call. dart's
    /// "unspanned exception attaches at the boundary" rule.
    fn build_error(&mut self, value: &Expr, pos: Pos, length: usize) -> Error {
        let msg = match self.eval_expr(value) {
            Ok(v) => v.to_error_message(),
            Err(e) => return e,
        };
        if !self.diag_enabled() {
            return Error::unpositioned(msg);
        }
        // The @error throws unspanned; the nearest enclosing call boundary
        // attaches its span. With an active call stack, the snippet points at
        // the innermost call site and the trace is the callers only (the
        // @error's own frame is dropped). At the root, the @error span is used.
        let frames: Vec<DiagFrame> = if self.call_stack.is_empty() {
            vec![DiagFrame {
                url: self.current_url.clone(),
                pos,
                member: self.member.clone(),
                length,
            }]
        } else {
            self.call_stack.iter().rev().cloned().collect()
        };
        let mut e = Error::at(msg, frames[0].pos);
        e.length = frames[0].length;
        e.rendered = Some(self.render_error_with_frames(&e, &frames));
        e
    }

    /// Register an `@extend` directive: validate the (interpolation-resolved)
    /// target, then record one [`PendingExtend`] per comma-separated target.
    /// `parents` is the enclosing style-rule selector list; `@extend` outside a
    /// style rule (top level or directly inside `@at-root`/an at-rule) is an
    /// error.
    fn register_extend(
        &mut self,
        selector: &[TplPiece],
        optional: bool,
        pos: Pos,
        parents: &[String],
    ) -> Result<(), Error> {
        if parents.is_empty() {
            return Err(Error::at("@extend may only be used within style rules.", pos));
        }
        let extenders = self.current_selector.clone().unwrap_or_else(|| parents.to_vec());
        let target = self.eval_template(selector)?;
        if target.trim().is_empty() {
            return Err(Error::at("expected selector.", pos));
        }
        let in_media = !self.media_queries.is_empty();
        for t in split_commas(&target) {
            let t = t.trim();
            if t.is_empty() {
                continue;
            }
            match crate::selector::classify_target(t) {
                crate::selector::TargetClass::Simple(simple) => {
                    self.extends.push(PendingExtend {
                        target: simple,
                        target_str: t.to_string(),
                        extenders: extenders.clone(),
                        optional,
                        in_media,
                        pos,
                    });
                }
                crate::selector::TargetClass::Complex => {
                    return Err(Error::at("complex selectors may not be extended.", pos));
                }
                crate::selector::TargetClass::Compound => {
                    return Err(Error::at(
                        "compound selectors may no longer be extended.\n\
                         Consider `@extend a, :hover` instead.\n\
                         See https://sass-lang.com/d/extend-compound for details.",
                        pos,
                    ));
                }
                crate::selector::TargetClass::Invalid => {
                    return Err(Error::at("expected selector.", pos));
                }
            }
        }
        Ok(())
    }

    /// Post-eval extension pass: rewrite every emitted style-rule selector list
    /// according to the collected `@extend` directives, drop placeholder-only
    /// rules, and error on an unmatched non-`!optional` extend.
    fn apply_extends(&mut self, out: &mut Vec<OutNode>) -> Result<(), Error> {
        let mut extensions: Vec<crate::selector::Extension> = Vec::new();
        for pe in &self.extends {
            let mut extenders = Vec::new();
            for ext in &pe.extenders {
                if let Some(c) = crate::selector::parse_complex_one(ext) {
                    // A bogus extender with a trailing combinator (`d +`) can't
                    // extend anything — dart-sass drops it (with a deprecation).
                    if c.trailing.is_empty() {
                        extenders.push(c);
                    }
                }
            }
            extensions.push(crate::selector::Extension {
                target: Some(pe.target.clone()),
                extenders,
                optional: pe.optional,
                matched: std::cell::Cell::new(false),
            });
        }

        // An `@extend` registered inside `@media` may not extend a selector
        // outside any media context (dart-sass "You may not @extend selectors
        // across media queries."). Detect when an in-media extend's target
        // matches a root-level (non-media) rule.
        for pe in &self.extends {
            if pe.in_media && root_rule_contains_target(out, &pe.target) {
                return Err(Error::at(
                    "You may not @extend selectors across media queries.",
                    pe.pos,
                ));
            }
        }

        rewrite_nodes(out, &extensions);

        // Report the first unmatched non-optional extend.
        for (pe, ext) in self.extends.iter().zip(extensions.iter()) {
            if !ext.optional && !ext.matched.get() {
                return Err(Error::at(
                    format!(
                        "The target selector was not found.\nUse \"@extend {} !optional\" to avoid this error.",
                        pe.target_str
                    ),
                    pe.pos,
                ));
            }
        }
        Ok(())
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

    /// Push a new scope. `semi_global` requests semi-global behavior (control
    /// flow), which only takes effect when the current innermost scope is
    /// already semi-global (dart-sass `Environment.scope`).
    fn push_scope(&mut self, semi_global: bool) {
        let effective = semi_global && self.scope_semi_global.last().copied().unwrap_or(false);
        self.scopes.push(HashMap::default());
        self.scope_semi_global.push(effective);
    }

    /// Push a pre-populated, non-semi-global scope (a mixin/function argument
    /// frame).
    fn push_scope_frame(&mut self, frame: HashMap<String, Value>) {
        self.scopes.push(frame);
        self.scope_semi_global.push(false);
    }

    fn pop_scope(&mut self) {
        self.scopes.pop();
        self.scope_semi_global.pop();
    }

    /// Assign a non-global variable (dart-sass `Environment.setVariable`). The
    /// value updates the variable at the innermost scope where it already
    /// exists; if it exists only in the global scope and the current scope is
    /// not semi-global, a new local is created instead so a nested rule cannot
    /// silently rewrite a global.
    fn assign(&mut self, name: &str, val: Value) {
        if self.scopes.len() == 1 {
            if let Some(g) = self.scopes.first_mut() {
                g.insert(name.to_string(), val);
            }
            return;
        }
        // Innermost scope index holding the variable (None if undeclared).
        let mut index = None;
        for (i, scope) in self.scopes.iter().enumerate().rev() {
            if scope.contains_key(name) {
                index = Some(i);
                break;
            }
        }
        let in_semi_global = self.scope_semi_global.last().copied().unwrap_or(false);
        let target = match index {
            Some(0) if !in_semi_global => self.scopes.len() - 1,
            Some(i) => i,
            None => self.scopes.len() - 1,
        };
        if let Some(scope) = self.scopes.get_mut(target) {
            scope.insert(name.to_string(), val);
        }
    }

    fn apply_var(&mut self, v: &VarDecl) -> Result<(), Error> {
        // A namespaced assignment `ns.$name: value` updates the variable in the
        // `@use`d module bound to `ns`.
        if let Some(ns) = &v.namespace {
            return self.assign_module_var(ns, v);
        }
        // A top-level `!default` declaration whose name is exposed by more than
        // one `@use … as *` module can't resolve which global it shadows.
        if v.is_default
            && self.scopes.len() == 1
            && self.lookup(&v.name).is_none()
            && !is_private_member(&v.name)
            && self
                .star_user_modules
                .iter()
                .filter(|m| m.var(&v.name).is_some())
                .count()
                > 1
        {
            return Err(Error::unpositioned(
                "This variable is available from multiple global modules.",
            ));
        }
        // A top-level `!default` variable in a module being evaluated with
        // configuration: the supplied value overrides the default (unless the
        // override itself is `!default` and the variable already has a value).
        // Configuration is keyed by the canonical (dashed) variable name.
        if v.is_default && self.scopes.len() == 1 {
            let key = normalize_var_name(&v.name);
            if let Some((cfg_val, cfg_is_default)) = self.pending_config.get(&key).cloned() {
                self.consumed_config.push(key);
                let already_set = matches!(self.lookup(&v.name), Some(x) if !matches!(x, Value::Null));
                // A `null` configuration value leaves the `!default` in place;
                // a `@forward ... with ($x !default)` only applies if the module
                // hasn't already defined the variable.
                if !(matches!(cfg_val, Value::Null) || cfg_is_default && already_set) {
                    if let Some(g) = self.scopes.first_mut() {
                        g.insert(v.name.clone(), cfg_val);
                    }
                    return Ok(());
                }
            }
        }
        let val = self.eval_expr(&v.value)?;
        if v.is_default {
            if let Some(existing) = self.lookup(&v.name) {
                if !matches!(existing, Value::Null) {
                    return Ok(());
                }
            }
        }
        // A top-level assignment to a name not in scope but exposed by exactly
        // one `@use … as *` module updates that module's variable (so the
        // module's own functions/mixins observe the change).
        if self.scopes.len() == 1 && !is_private_member(&v.name) {
            if let Some(g) = self.scopes.first() {
                if !g.contains_key(&v.name) {
                    let targets: Vec<Rc<Module>> = self
                        .star_user_modules
                        .iter()
                        .filter(|m| m.var(&v.name).is_some())
                        .cloned()
                        .collect();
                    if targets.len() == 1 {
                        if v.is_default {
                            if let Some(existing) = targets[0].var(&v.name) {
                                if !matches!(existing, Value::Null) {
                                    return Ok(());
                                }
                            }
                        }
                        targets[0].vars.borrow_mut().insert(v.name.clone(), val);
                        return Ok(());
                    }
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

    /// Assign to a `@use`d module's variable (`ns.$name: value`). The variable
    /// must already exist in the module and be public; `!default` only assigns
    /// when the existing value is null; built-in modules are immutable.
    fn assign_module_var(&mut self, ns: &str, v: &VarDecl) -> Result<(), Error> {
        if is_private_member(&v.name) {
            return Err(Error::unpositioned(
                "Private members can't be accessed from outside their modules.",
            ));
        }
        let module = match self.used_user_modules.get(ns).cloned() {
            Some(m) => m,
            None => {
                if self.used_modules.contains_key(ns) {
                    return Err(Error::unpositioned("Cannot modify built-in variable."));
                }
                return Err(Error::unpositioned(format!(
                    "There is no module with the namespace \"{ns}\"."
                )));
            }
        };
        let exists = module.var(&v.name).is_some();
        if !exists {
            return Err(Error::unpositioned("Undefined variable."));
        }
        if v.is_default {
            if let Some(existing) = module.var(&v.name) {
                if !matches!(existing, Value::Null) {
                    return Ok(());
                }
            }
        }
        let val = self.eval_expr(&v.value)?.without_slash();
        module.vars.borrow_mut().insert(v.name.clone(), val);
        Ok(())
    }

    // ---- loop helpers ------------------------------------------------

    /// Set a variable in the innermost scope. A loop pushes its own scope, so a
    /// loop variable bound here lives in the loop's scope and is re-bound each
    /// iteration (dart-sass `setLocalVariable`).
    fn set_local(&mut self, name: &str, val: Value) {
        if let Some(sc) = self.scopes.last_mut() {
            sc.insert(name.to_string(), val);
        }
    }

    /// Evaluate a `@for` bound to a [`Number`], preserving its unit (the loop
    /// variable inherits the `from` bound's unit).
    fn eval_for_number(&mut self, e: &Expr) -> Result<Number, Error> {
        match self.eval_expr(e)? {
            Value::Number(n) => Ok(n),
            other => Err(Error::unpositioned(format!(
                "{} is not a number.",
                other.type_name()
            ))),
        }
    }

    /// Resolve a `@for`'s bounds: the integer start, the integer end (the TO
    /// bound converted to the FROM bound's unit), and the loop variable's unit
    /// (taken from FROM). Errors on incompatible units or a non-integer bound,
    /// matching dart-sass.
    fn for_bounds(&mut self, from: &Expr, to: &Expr) -> Result<(i64, i64, String), Error> {
        let start = self.eval_for_number(from)?;
        let end = self.eval_for_number(to)?;
        // The loop variable takes FROM's unit; TO is converted to match. A
        // unitless side defers (no conversion); two incompatible real units err.
        let end_value = if start.unit.is_empty() || end.unit.is_empty() {
            end.value
        } else {
            match crate::value::convert_factor(&end.unit, &start.unit) {
                Some(f) => end.value * f,
                None => {
                    return Err(Error::unpositioned(format!(
                        "Expected {} to have unit {}.",
                        Value::Number(end.clone()).to_css(false),
                        start.unit,
                    )))
                }
            }
        };
        // Both bounds must be integers (dart-sass: "<n> is not an int.").
        let to_int = |v: f64, n: Number| -> Result<i64, Error> {
            if (v - v.round()).abs() < 1e-11 {
                Ok(v.round() as i64)
            } else {
                Err(Error::unpositioned(format!(
                    "{} is not an int.",
                    Value::Number(n).to_css(false)
                )))
            }
        };
        let start_i = to_int(start.value, start.clone())?;
        let end_i = to_int(
            end_value,
            Number {
                value: end_value,
                unit: start.unit.clone(),
            },
        )?;
        Ok((start_i, end_i, start.unit))
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
                        keywords: None,
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
                    Value::List(l) => {
                        splat_pos.extend(l.items);
                        // An argument-list splat (`$args...`) also forwards its
                        // captured keyword arguments as named arguments.
                        if let Some(kw) = l.keywords {
                            for (k, val) in kw {
                                if let Value::Str(s) = k {
                                    push_named(&mut keyword, s.text, val)?;
                                }
                            }
                        }
                    }
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
        let evaled = self.eval_call_args(args)?;
        self.bind_evaled(params, evaled, name)
    }

    /// Bind already-evaluated `(positional, keyword)` arguments into a call
    /// frame. Used by `meta.call`, which has only evaluated values to pass on.
    fn bind_evaled(
        &mut self,
        params: &ParamList,
        evaled: EvaledArgs,
        name: &str,
    ) -> Result<HashMap<String, Value>, Error> {
        let (positional, keyword_vec) = evaled;
        let mut keyword: HashMap<String, Value> = HashMap::default();
        // Track the order and source spelling of keyword names so an
        // "unknown parameter" error can list them as the caller wrote them.
        let mut keyword_order: Vec<(String, String)> = Vec::new();
        for (n, v) in keyword_vec {
            let norm = normalize_arg_name(&n);
            if !keyword.contains_key(&norm) {
                keyword_order.push((norm.clone(), n));
            }
            keyword.insert(norm, v);
        }
        let mut frame = HashMap::default();
        let mut pos_iter = positional.into_iter();
        for param in &params.params {
            let val = if let Some(v) = pos_iter.next() {
                v
            } else if let Some(v) = keyword.remove(&normalize_arg_name(&param.name)) {
                v
            } else if let Some(def) = &param.default {
                self.eval_expr(def)?
            } else {
                return Err(Error::unpositioned(format!("Missing argument ${}.", param.name)));
            };
            frame.insert(param.name.clone(), val);
        }
        if let Some(rest) = &params.rest {
            let remaining: Vec<Value> = pos_iter.collect();
            // Any keyword args left after binding the declared params become the
            // arglist's keywords, in caller order and keyed by their
            // hyphen-normalized name (what `meta.keywords` reports).
            let kw: Vec<(Value, Value)> = keyword_order
                .iter()
                .filter_map(|(norm, _)| {
                    keyword.remove(norm).map(|v| {
                        (
                            Value::Str(SassStr {
                                text: norm.clone(),
                                quoted: false,
                            }),
                            v,
                        )
                    })
                })
                .collect();
            frame.insert(
                rest.clone(),
                Value::List(List {
                    items: remaining,
                    sep: ListSep::Comma,
                    bracketed: false,
                    keywords: Some(kw),
                }),
            );
        } else if pos_iter.next().is_some() {
            return Err(Error::unpositioned(format!(
                "{name} was passed too many arguments."
            )));
        }
        // Reject keyword arguments that name no declared parameter. A `...`
        // rest parameter would absorb them into an arglist (whose keywords
        // are not yet modelled), so only validate when there is no rest.
        if params.rest.is_none() && !keyword.is_empty() {
            let leftover: Vec<&str> = keyword_order
                .iter()
                .filter(|(norm, _)| keyword.contains_key(norm))
                .map(|(_, orig)| orig.as_str())
                .collect();
            if let Some((last, init)) = leftover.split_last() {
                let msg = if init.is_empty() {
                    format!("No parameter named ${last}.")
                } else {
                    let head = init
                        .iter()
                        .map(|n| format!("${n}"))
                        .collect::<Vec<_>>()
                        .join(", ");
                    format!("No parameters named {head} or ${last}.")
                };
                return Err(Error::unpositioned(msg));
            }
        }
        Ok(frame)
    }

    /// Call a user-defined `@function`, returning its `@return` value. `call`,
    /// when present, is the (name-start position, byte length) of the call
    /// expression, recorded as a diagnostic stack frame around the body.
    fn call_function(
        &mut self,
        func: &Rc<Callable>,
        args: &[CallArg],
        call: Option<(Pos, usize)>,
    ) -> Result<Value, Error> {
        let frame = self.bind_args(&func.params, args, &func.name)?;
        let saved = call.map(|(pos, len)| self.enter_call(pos, len, &format!("{}()", func.name)));
        self.push_scope_frame(frame);
        // A function body is not a mixin body: `meta.content-exists()` called
        // from a function (even one invoked by a mixin) is an error.
        self.in_mixin.push(false);
        let result = self.run_fn_body(&func.body);
        self.in_mixin.pop();
        self.pop_scope();
        if let Some(saved) = saved {
            self.leave_call(saved);
        }
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
                            self.push_scope(true);
                            let result = self.run_fn_body(&branch.body);
                            self.pop_scope();
                            if let Some(v) = result? {
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
                    let (start_i, end_i, unit) = self.for_bounds(from, to)?;
                    self.push_scope(true);
                    let mut result = Ok(None);
                    for i in for_indices(start_i, end_i, *inclusive) {
                        self.set_local(
                            var,
                            Value::Number(Number {
                                value: i as f64,
                                unit: unit.clone(),
                            }),
                        );
                        result = self.run_fn_body(body);
                        if matches!(result, Ok(None)) {
                            continue;
                        }
                        break;
                    }
                    self.pop_scope();
                    if let Some(v) = result? {
                        return Ok(Some(v));
                    }
                }
                Stmt::Each { vars, list, body } => {
                    let items = self.eval_each_items(list)?;
                    self.push_scope(true);
                    let mut result = Ok(None);
                    for item in items {
                        self.bind_each(vars, item);
                        result = self.run_fn_body(body);
                        if matches!(result, Ok(None)) {
                            continue;
                        }
                        break;
                    }
                    self.pop_scope();
                    if let Some(v) = result? {
                        return Ok(Some(v));
                    }
                }
                Stmt::While { cond, body } => {
                    self.push_scope(true);
                    let mut result: Result<Option<Value>, Error> = Ok(None);
                    let mut guard = 0u32;
                    loop {
                        match self.eval_expr(cond) {
                            Ok(v) if v.is_truthy() => {}
                            Ok(_) => break,
                            Err(e) => {
                                result = Err(e);
                                break;
                            }
                        }
                        result = self.run_fn_body(body);
                        if !matches!(result, Ok(None)) {
                            break;
                        }
                        guard += 1;
                        if guard >= 100_000 {
                            result = Err(Error::unpositioned("@while exceeded 100000 iterations"));
                            break;
                        }
                    }
                    self.pop_scope();
                    if let Some(v) = result? {
                        return Ok(Some(v));
                    }
                }
                Stmt::Warn { value, pos } => self.emit_warn(value, *pos)?,
                Stmt::Debug { value, pos } => self.emit_debug(value, *pos)?,
                Stmt::Error { value, pos, length } => {
                    return Err(self.build_error(value, *pos, *length));
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
    #[allow(clippy::too_many_arguments)]
    fn exec_include(
        &mut self,
        name: &str,
        args: &[CallArg],
        content: Option<Rc<Vec<Stmt>>>,
        content_params: Option<Rc<ParamList>>,
        module: Option<&str>,
        pos: Pos,
        parents: &[String],
        sink: &mut Sink<'_>,
    ) -> Result<(), Error> {
        // NOTE: the diagnostic call frame for this `@include` is pushed by the
        // caller (the `Stmt::Include` arm) so it wraps every resolution path.
        // The built-in `@include meta.apply(...)` / `meta.load-css(...)` are
        // bound to the `sass:meta` namespace, so resolve them before the generic
        // module path.
        if let Some(ns) = module {
            if self.used_modules.get(ns).map(String::as_str) == Some("meta") {
                if name == "apply" {
                    return self.exec_apply(args, content, content_params, parents, sink);
                }
                if name == "load-css" {
                    return self.exec_load_css(args, content, pos, parents, sink);
                }
            }
        }
        // A namespaced `@include ns.mixin`: resolve a user module bound to the
        // namespace, then a built-in (which exposes no mixins in this build).
        if let Some(ns) = module {
            if let Some(target) = self.used_user_modules.get(ns).cloned() {
                if is_private_member(name) {
                    return Err(Error::unpositioned(
                        "Private members can't be accessed from outside their modules.",
                    ));
                }
                let mixin = target
                    .mixin(name)
                    .ok_or_else(|| Error::unpositioned("Undefined mixin."))?;
                return self.run_module_mixin(&target, &mixin, args, content, content_params, parents, sink);
            }
            if !self.used_modules.contains_key(ns) {
                return Err(Error::unpositioned(format!(
                    "There is no module with the namespace \"{ns}\"."
                )));
            }
            return Err(Error::unpositioned("Undefined mixin."));
        }
        // A bare `@include` may resolve a user module mixin exposed unprefixed
        // via `@use … as *`.
        if !self.mixins.contains_key(name) && !self.star_user_modules.is_empty() && !is_private_member(name) {
            let hits: Vec<(Rc<Module>, Rc<Callable>)> = self
                .star_user_modules
                .iter()
                .filter_map(|m| m.mixin(name).map(|mx| (Rc::clone(m), mx)))
                .collect();
            if hits.len() > 1 {
                return Err(Error::unpositioned(
                    "This mixin is available from multiple global modules.",
                ));
            }
            if let Some((m, mx)) = hits.into_iter().next() {
                return self.run_module_mixin(&m, &mx, args, content, content_params, parents, sink);
            }
        }
        let mixin = self
            .mixins
            .get(name)
            .cloned()
            .ok_or_else(|| Error::unpositioned(format!("Undefined mixin {name}.")))?;
        // dart-sass: passing a content block to a mixin that never uses
        // `@content` is an error, even when the block is empty.
        if content.is_some() && !body_uses_content(&mixin.body) {
            return Err(Error::unpositioned("Mixin doesn't accept a content block."));
        }
        let frame = self.bind_args(&mixin.params, args, &mixin.name)?;
        self.push_scope_frame(frame);
        self.content_stack.push(content.map(|stmts| ContentBlock {
            stmts,
            params: content_params.clone(),
            caller_env: None,
        }));
        self.in_mixin.push(true);
        let result = self.exec(&mixin.body, parents, sink);
        self.in_mixin.pop();
        self.content_stack.pop();
        self.pop_scope();
        result
    }

    /// Execute an `@include ns.mixin` where `ns` is a user module: run the mixin
    /// body in the module's own environment, while its `@content` block (if any)
    /// runs back in the call site's environment.
    #[allow(clippy::too_many_arguments)]
    fn run_module_mixin(
        &mut self,
        module: &Rc<Module>,
        mixin: &Rc<Callable>,
        args: &[CallArg],
        content: Option<Rc<Vec<Stmt>>>,
        content_params: Option<Rc<ParamList>>,
        parents: &[String],
        sink: &mut Sink<'_>,
    ) -> Result<(), Error> {
        if content.is_some() && !body_uses_content(&mixin.body) {
            return Err(Error::unpositioned("Mixin doesn't accept a content block."));
        }
        // Bind the arguments at the call site (so they resolve in the caller's
        // scope), then enter the module's environment for the body. Snapshot the
        // call-site env so a `@content` block runs there, not in the module.
        let frame = self.bind_args(&mixin.params, args, &mixin.name)?;
        let content_block = content.map(|stmts| {
            let snapshot = self.snapshot_env();
            ContentBlock {
                stmts,
                params: content_params.clone(),
                caller_env: Some(Box::new(snapshot)),
            }
        });
        let saved = self.enter_module(module);
        let saved_file = self.enter_module_file(module);
        self.push_scope_frame(frame);
        self.content_stack.push(content_block);
        let result = self.exec(&mixin.body, parents, sink);
        self.content_stack.pop();
        self.pop_scope();
        self.leave_module_file(saved_file);
        self.leave_module(saved);
        result
    }

    /// `@include meta.apply($mixin, $args...)`: invoke a first-class mixin
    /// reference. The first argument is the mixin reference; the rest are the
    /// arguments passed on to that mixin (which may also accept a `@content`
    /// block).
    fn exec_apply(
        &mut self,
        args: &[CallArg],
        content: Option<Rc<Vec<Stmt>>>,
        content_params: Option<Rc<ParamList>>,
        parents: &[String],
        sink: &mut Sink<'_>,
    ) -> Result<(), Error> {
        // Evaluate apply's own arguments (expanding any `...` splat). The first
        // positional (or named `$mixin`) is the mixin reference; the remainder
        // are forwarded to the mixin.
        let (mut pos_args, mut named) = self.eval_call_args(args)?;
        for v in &mut pos_args {
            *v = std::mem::replace(v, Value::Null).without_slash();
        }
        for (_, v) in &mut named {
            *v = std::mem::replace(v, Value::Null).without_slash();
        }
        let (mixin_val, rest_pos): (Value, Vec<Value>) = if !pos_args.is_empty() {
            let mut iter = pos_args.into_iter();
            let first = iter.next().unwrap_or(Value::Null);
            (first, iter.collect())
        } else if let Some(idx) = named.iter().position(|(n, _)| n == "mixin") {
            (named.remove(idx).1, Vec::new())
        } else {
            return Err(Error::unpositioned("Missing argument $mixin."));
        };
        let rest_named: Vec<(String, Value)> = named.into_iter().filter(|(n, _)| n != "mixin").collect();
        let mixin = match mixin_val {
            Value::Mixin(m) => m,
            other => {
                return Err(Error::unpositioned(format!(
                    "$mixin: {} is not a mixin reference.",
                    other.to_css(false)
                )))
            }
        };
        self.invoke_mixin_ref(
            &mixin,
            rest_pos,
            rest_named,
            content,
            content_params,
            parents,
            sink,
        )
    }

    /// `@include meta.load-css($url, $with: (...))`: load the module at `$url`
    /// and emit its CSS into the current sink, optionally configuring it with
    /// `$with`. Unlike `@use`, it binds no namespace and exposes no members; it
    /// reuses the shared `load_module` machinery (cache, cycle guard, CSS emit).
    fn exec_load_css(
        &mut self,
        args: &[CallArg],
        content: Option<Rc<Vec<Stmt>>>,
        pos: Pos,
        _parents: &[String],
        sink: &mut Sink<'_>,
    ) -> Result<(), Error> {
        if content.is_some() {
            return Err(Error::at(
                "Mixin doesn't accept a content block.".to_string(),
                pos,
            ));
        }
        let (pos_args, named) = self.eval_call_args(args)?;
        let mut iter = pos_args.into_iter();
        let mut url_val = iter.next();
        let mut with_val = iter.next();
        if iter.next().is_some() {
            return Err(Error::at(
                "Only 2 arguments allowed, but 3 were passed.".to_string(),
                pos,
            ));
        }
        for (n, v) in named {
            match n.as_str() {
                "url" => url_val = Some(v),
                "with" => with_val = Some(v),
                other => return Err(Error::at(format!("No argument named ${other}."), pos)),
            }
        }
        let url = match url_val {
            Some(Value::Str(s)) => s.text,
            Some(other) => {
                return Err(Error::at(
                    format!("$url: {} is not a string.", other.to_css(false)),
                    pos,
                ))
            }
            None => return Err(Error::at("Missing argument $url.".to_string(), pos)),
        };
        // Build the configuration from the `$with` map (string keys → variables).
        let mut config: HashMap<String, (Value, bool)> = HashMap::default();
        match with_val.take() {
            None => {}
            // An empty literal `()` parses as an empty list, not a map.
            Some(Value::List(l)) if l.items.is_empty() => {}
            Some(Value::Map(m)) => {
                for (k, v) in m.entries {
                    let key = match k {
                        Value::Str(s) => normalize_var_name(&s.text),
                        other => {
                            return Err(Error::at(
                                format!("$with key: {} is not a string.", other.to_css(false)),
                                pos,
                            ))
                        }
                    };
                    // Dash/underscore-insensitive: `a-b` and `a_b` collide.
                    if config.contains_key(&key) {
                        return Err(Error::at(
                            format!("The variable ${key} was configured twice."),
                            pos,
                        ));
                    }
                    config.insert(key, (v.without_slash(), false));
                }
            }
            Some(other) => {
                return Err(Error::at(
                    format!("$with: {} is not a map.", other.to_css(false)),
                    pos,
                ))
            }
        }
        let conf_keys: Vec<String> = config.keys().cloned().collect();
        // Evaluate the module into a fresh TOP-LEVEL buffer so its body runs in
        // its own top-level context — a module top-level declaration errors no
        // matter where load-css is invoked (dart-sass) — then splice the emitted
        // nodes into the caller's position.
        let mut buf: Vec<OutNode> = Vec::new();
        let consumed = {
            let mut module_sink = Sink::Top(&mut buf);
            let (_module, consumed) = self.load_module(&url, config, pos, &mut module_sink)?;
            consumed
        };
        if conf_keys.iter().any(|k| !consumed.contains(k)) {
            return Err(Error::at(
                "This variable was not declared with !default in the @used module.".to_string(),
                pos,
            ));
        }
        for node in buf {
            sink.push_at_rule(node);
        }
        Ok(())
    }

    /// Invoke a resolved mixin reference with already-evaluated arguments and an
    /// optional `@content` block, emitting into `sink`.
    #[allow(clippy::too_many_arguments)]
    fn invoke_mixin_ref(
        &mut self,
        mixin: &SassMixin,
        pos_args: Vec<Value>,
        named: Vec<(String, Value)>,
        content: Option<Rc<Vec<Stmt>>>,
        content_params: Option<Rc<ParamList>>,
        parents: &[String],
        sink: &mut Sink<'_>,
    ) -> Result<(), Error> {
        // A captured user `@mixin`: recover the type-erased `Callable`.
        let callable = match &mixin.user {
            Some(any) => match Rc::clone(any).downcast::<Callable>() {
                Ok(c) => c,
                Err(_) => return Err(Error::unpositioned("Undefined mixin.")),
            },
            // A built-in mixin reference (`meta.load-css`/`meta.apply`). Only the
            // content-block validation is observable in the supported cases.
            None => {
                if content.is_some() {
                    return Err(Error::unpositioned("Mixin doesn't accept a content block."));
                }
                return Err(Error::unpositioned("Undefined mixin."));
            }
        };
        if content.is_some() && !body_uses_content(&callable.body) {
            return Err(Error::unpositioned("Mixin doesn't accept a content block."));
        }
        let frame = self.bind_evaled(&callable.params, (pos_args, named), &callable.name)?;
        // A mixin captured from another module runs in that module's environment;
        // its `@content` block runs back at the call site.
        if let Some(module_any) = &mixin.module {
            if let Ok(module) = Rc::clone(module_any).downcast::<Module>() {
                let content_block = content.map(|stmts| {
                    let snapshot = self.snapshot_env();
                    ContentBlock {
                        stmts,
                        params: content_params.clone(),
                        caller_env: Some(Box::new(snapshot)),
                    }
                });
                let saved = self.enter_module(&module);
                self.push_scope_frame(frame);
                self.content_stack.push(content_block);
                self.in_mixin.push(true);
                let result = self.exec(&callable.body, parents, sink);
                self.in_mixin.pop();
                self.content_stack.pop();
                self.pop_scope();
                self.leave_module(saved);
                return result;
            }
        }
        self.push_scope_frame(frame);
        self.content_stack.push(content.map(|stmts| ContentBlock {
            stmts,
            params: content_params.clone(),
            caller_env: None,
        }));
        self.in_mixin.push(true);
        let result = self.exec(&callable.body, parents, sink);
        self.in_mixin.pop();
        self.content_stack.pop();
        self.pop_scope();
        result
    }

    /// Run the innermost active `@content` block. For a cross-module include the
    /// block carries a snapshot of the call-site environment, which is installed
    /// for the duration so the content resolves there rather than in the mixin's
    /// module.
    fn exec_content(
        &mut self,
        args: &[CallArg],
        parents: &[String],
        sink: &mut Sink<'_>,
    ) -> Result<(), Error> {
        let (stmts, params, caller_env) = match self.content_stack.last() {
            Some(Some(block)) => (
                Rc::clone(&block.stmts),
                block.params.clone(),
                block.caller_env.as_ref().map(|e| (**e).clone()),
            ),
            _ => return Ok(()),
        };
        // `@content(args)` evaluates its arguments at the call site (the mixin
        // body), then binds them to the content block's `using (params)`, which
        // become visible inside the block.
        let frame = match &params {
            Some(p) => Some(self.bind_args(p, args, "@content")?),
            None => {
                // A content block with no `using (params)` accepts no
                // arguments; passing any is an error (dart-sass).
                if !args.is_empty() {
                    let n = args.len();
                    let verb = if n == 1 { "was" } else { "were" };
                    return Err(Error::unpositioned(format!(
                        "Only 0 arguments allowed, but {n} {verb} passed."
                    )));
                }
                None
            }
        };
        let restore = caller_env.map(|env| self.install_env(env));
        let pushed = frame.is_some();
        if let Some(frame) = frame {
            self.push_scope_frame(frame);
        }
        let result = self.exec(&stmts, parents, sink);
        if pushed {
            self.pop_scope();
        }
        if let Some(restore) = restore {
            self.leave_module(restore);
        }
        result
    }

    /// Install a saved environment snapshot, returning the displaced one to
    /// restore afterwards.
    fn install_env(&mut self, env: SavedModuleEnv) -> SavedModuleEnv {
        SavedModuleEnv {
            scopes: std::mem::replace(&mut self.scopes, env.scopes),
            scope_semi_global: std::mem::replace(&mut self.scope_semi_global, env.scope_semi_global),
            functions: std::mem::replace(&mut self.functions, env.functions),
            mixins: std::mem::replace(&mut self.mixins, env.mixins),
            used_modules: std::mem::replace(&mut self.used_modules, env.used_modules),
            star_modules: std::mem::replace(&mut self.star_modules, env.star_modules),
            used_user_modules: std::mem::replace(&mut self.used_user_modules, env.used_user_modules),
            star_user_modules: std::mem::replace(&mut self.star_user_modules, env.star_user_modules),
            write_back: None,
        }
    }

    /// Clone the current per-module environment (for capturing a content block's
    /// call-site closure).
    fn snapshot_env(&self) -> SavedModuleEnv {
        SavedModuleEnv {
            scopes: self.scopes.clone(),
            scope_semi_global: self.scope_semi_global.clone(),
            functions: self.functions.clone(),
            mixins: self.mixins.clone(),
            used_modules: self.used_modules.clone(),
            star_modules: self.star_modules.clone(),
            used_user_modules: self.used_user_modules.clone(),
            star_user_modules: self.star_user_modules.clone(),
            write_back: None,
        }
    }

    /// Process a `@use "<url>" [as ns|as *] [with (...)];` for a built-in
    /// `sass:*` module or a user stylesheet.
    fn exec_use(
        &mut self,
        url: &str,
        namespace: Option<&str>,
        star: bool,
        config: &[crate::ast::ConfigEntry],
        pos: Pos,
        sink: &mut Sink<'_>,
    ) -> Result<(), Error> {
        // Built-in `sass:<mod>` modules.
        if let Some(m) = url.strip_prefix("sass:") {
            if !crate::builtins::is_module(m) {
                return Err(Error::at("Can't find stylesheet to import.".to_string(), pos));
            }
            if !config.is_empty() {
                return Err(Error::at(
                    "Built-in modules can't be configured.".to_string(),
                    pos,
                ));
            }
            let module = m.to_string();
            if star {
                if !self.star_modules.contains(&module) {
                    self.star_modules.push(module);
                }
                return Ok(());
            }
            let ns = namespace.unwrap_or(&module).to_string();
            self.check_namespace_free(&ns, pos)?;
            self.used_modules.insert(ns, module);
            return Ok(());
        }

        // A user stylesheet module.
        let conf = self.eval_config(config)?;
        let conf_keys: Vec<String> = conf.keys().cloned().collect();
        let (module, consumed) = self.load_module(url, conf, pos, sink)?;
        // Any configured variable the module did not consume via a `!default`
        // declaration is an error.
        if conf_keys.iter().any(|k| !consumed.contains(k)) {
            return Err(Error::at(
                "This variable was not declared with !default in the @used module.".to_string(),
                pos,
            ));
        }
        if star {
            // A member the new global module exposes that the current sheet
            // already defines at the top level is a conflict.
            if let Some(g) = self.scopes.first() {
                for name in module.vars.borrow().keys() {
                    if !is_private_member(name) && g.contains_key(name) {
                        return Err(Error::at(
                            format!(
                                "This module and the new module both define a variable named \"${name}\"."
                            ),
                            pos,
                        ));
                    }
                }
            }
            // `@use`ing the same module twice as `*` is idempotent (no
            // ambiguity), so de-duplicate by module identity.
            let ptr = Rc::as_ptr(&module);
            if !self.star_user_modules.iter().any(|m| Rc::as_ptr(m) == ptr) {
                self.star_user_modules.push(module);
            }
            return Ok(());
        }
        let ns = match namespace {
            Some(n) => n.to_string(),
            None => default_namespace(url, pos)?,
        };
        self.check_namespace_free(&ns, pos)?;
        self.used_user_modules.insert(ns, module);
        Ok(())
    }

    /// Reject a namespace already bound by another `@use` in the same sheet.
    fn check_namespace_free(&self, ns: &str, pos: Pos) -> Result<(), Error> {
        if self.used_modules.contains_key(ns) || self.used_user_modules.contains_key(ns) {
            return Err(Error::at(
                format!("There's already a module with namespace \"{ns}\"."),
                pos,
            ));
        }
        Ok(())
    }

    /// Evaluate a `with (...)` configuration clause into a name -> (value,
    /// is_default) map.
    fn eval_config(
        &mut self,
        config: &[crate::ast::ConfigEntry],
    ) -> Result<HashMap<String, (Value, bool)>, Error> {
        let mut map = HashMap::default();
        for entry in config {
            let v = self.eval_expr(&entry.value)?.without_slash();
            // Variable names are dash/underscore-insensitive: store the
            // canonical (dashed) form so `$a_b` and `$a-b` configure the same
            // variable. A duplicate key is an error.
            let key = normalize_var_name(&entry.name);
            if map.contains_key(&key) {
                return Err(Error::unpositioned(format!(
                    "The variable ${} was configured twice.",
                    entry.name
                )));
            }
            map.insert(key, (v, entry.is_default));
        }
        Ok(map)
    }

    /// Load (and cache) a user module: resolve its URL, evaluate it once into an
    /// isolated environment with `config` applied to its `!default` variables,
    /// emit its CSS into `sink`, and return the shared module instance plus the
    /// list of config keys the module consumed (for `@forward ... with`
    /// pass-through).
    fn load_module(
        &mut self,
        url: &str,
        config: HashMap<String, (Value, bool)>,
        pos: Pos,
        sink: &mut Sink<'_>,
    ) -> Result<(Rc<Module>, Vec<String>), Error> {
        let importer = self.options.importer;
        // The caller's importer runs OUTSIDE the arena scope: anything it
        // allocates (e.g. a cache of paths it owns) must survive past this
        // compile's arena reset, so route its allocations to the system
        // allocator. The returned `String`s are then deep-copied into the arena
        // below by the parse/eval pipeline.
        let saved = crate::arena::pause();
        let resolved = importer.and_then(|imp| imp.resolve_module_with_syntax(url));
        crate::arena::resume(saved);
        let (key, src, syntax) = match resolved {
            Some(triple) => triple,
            None => {
                return Err(Error::at("Can't find stylesheet to import.".to_string(), pos));
            }
        };
        // A module evaluated once and cached is shared; its CSS is NOT
        // re-emitted. Re-loading it with configuration is an error — unless the
        // configuration targets no variable the module actually defines (a
        // module with no configurable variables may be loaded with or without
        // config). The keys it *does* define count as consumed for the caller.
        if let Some(existing) = self.module_cache.borrow().get(&key).cloned() {
            let consumed: Vec<String> = config
                .keys()
                .filter(|k| existing.var(k).is_some())
                .cloned()
                .collect();
            if !consumed.is_empty() {
                return Err(Error::at(
                    "This module was already loaded, so it can't be configured using \"with\".".to_string(),
                    pos,
                ));
            }
            // The cached module consumed nothing (it defines none of the
            // configured variables); the caller's own/forwarded handling decides
            // whether the leftover configuration is an error.
            return Ok((existing, Vec::new()));
        }
        // Guard against a load cycle.
        if self.loading.iter().any(|p| p == &key) {
            return Err(Error::at(
                "Module loop: this module is already being loaded.".to_string(),
                pos,
            ));
        }
        let sheet = parse_with_syntax(&src, syntax)?;
        // Register the module's source under a diagnostic display URL so a
        // snippet/frame that points into this file renders against its text.
        let diag_url = self.module_diag_url(url, &key);
        if self.diag_enabled() {
            self.file_sources
                .borrow_mut()
                .insert(diag_url.clone(), Rc::from(src.as_str()));
        }
        let is_css = matches!(syntax, Syntax::Css);
        let (module, consumed) = self.eval_module(&key, &diag_url, &sheet, config, pos, sink, is_css)?;
        let module = Rc::new(module);
        self.module_cache.borrow_mut().insert(key, Rc::clone(&module));
        Ok((module, consumed))
    }

    /// Emit a plain-CSS (`.css`) module's statements, preserving nesting (no
    /// Sass flattening), keeping `&` parent references literal, and resolving
    /// only `#{…}` interpolation. The parser has already rejected Sass-only
    /// constructs, so the remaining statements are plain CSS.
    fn exec_css(&mut self, stmts: &[Stmt], sink: &mut Sink<'_>) -> Result<(), Error> {
        for stmt in stmts {
            match stmt {
                Stmt::Rule(r) => {
                    let selectors = self.css_selectors(&r.selector, true)?;
                    let items = self.css_body(&r.body)?;
                    sink.push_at_rule(OutNode::Rule {
                        selectors,
                        linebreaks: Vec::new(),
                        items,
                    });
                }
                Stmt::Comment(c) => {
                    let text = self.eval_template(c)?;
                    sink.push_at_rule(OutNode::Comment(text));
                }
                _ => {}
            }
        }
        Ok(())
    }

    /// Resolve a plain-CSS selector to its comma-separated parts, keeping `&`
    /// and combinators verbatim (no parent resolution), and rejecting the
    /// Sass-only selector forms that plain CSS forbids.
    fn css_selectors(&mut self, sel: &[crate::ast::TplPiece], top_level: bool) -> Result<Vec<String>, Error> {
        let s = self.eval_template(sel)?;
        let parts: Vec<String> = split_commas(&s)
            .into_iter()
            .map(|p| p.trim().to_string())
            .filter(|p| !p.is_empty())
            .collect();
        for p in &parts {
            validate_plain_css_selector(p, top_level)?;
        }
        Ok(parts)
    }

    /// Build a plain-CSS rule body: declarations and nested style rules, with
    /// nesting preserved (`OutItem::NestedRule`).
    fn css_body(&mut self, stmts: &[Stmt]) -> Result<Vec<OutItem>, Error> {
        let mut items = Vec::new();
        for stmt in stmts {
            match stmt {
                Stmt::Decl(d) => {
                    let prop = self.eval_template(&d.property)?.trim().to_string();
                    let value = self.eval_expr(&d.value)?.to_css(false);
                    items.push(OutItem::Decl {
                        prop,
                        value,
                        important: d.important,
                        custom: false,
                    });
                }
                Stmt::CustomDecl(d) => {
                    let prop = self.eval_template(&d.property)?.trim().to_string();
                    let value = self.eval_template(&d.value)?;
                    items.push(OutItem::Decl {
                        prop,
                        value,
                        important: false,
                        custom: true,
                    });
                }
                Stmt::Rule(r) => {
                    let selectors = self.css_selectors(&r.selector, false)?;
                    let inner = self.css_body(&r.body)?;
                    items.push(OutItem::NestedRule {
                        selectors,
                        items: inner,
                    });
                }
                Stmt::Comment(c) => {
                    let text = self.eval_template(c)?;
                    items.push(OutItem::Comment(text));
                }
                _ => {}
            }
        }
        Ok(items)
    }

    /// Evaluate a parsed module sheet in an isolated environment. The module's
    /// top-level CSS is emitted into `sink`; its members are captured into a
    /// [`Module`]. `config` overrides its `!default` variables.
    #[allow(clippy::too_many_arguments)]
    fn eval_module(
        &mut self,
        key: &str,
        diag_url: &str,
        sheet: &Stylesheet,
        config: HashMap<String, (Value, bool)>,
        pos: Pos,
        sink: &mut Sink<'_>,
        css: bool,
    ) -> Result<(Module, Vec<String>), Error> {
        // Save and reset the per-module environment, then restore on the way out.
        // The module's body runs against its own source file for diagnostics.
        let module_source = self.source_for(diag_url);
        let saved_url = std::mem::replace(&mut self.current_url, diag_url.to_string());
        let saved_source = std::mem::replace(&mut self.current_source, module_source);
        let saved_scopes = std::mem::replace(&mut self.scopes, vec![HashMap::default()]);
        let saved_semi = std::mem::replace(&mut self.scope_semi_global, vec![true]);
        let saved_funcs = std::mem::take(&mut self.functions);
        let saved_mixins = std::mem::take(&mut self.mixins);
        let saved_used = std::mem::take(&mut self.used_modules);
        let saved_star = std::mem::take(&mut self.star_modules);
        let saved_used_user = std::mem::take(&mut self.used_user_modules);
        let saved_star_user = std::mem::take(&mut self.star_user_modules);
        let saved_fwd = std::mem::take(&mut self.forwarded);
        let saved_config = std::mem::replace(&mut self.pending_config, config);
        let saved_consumed = std::mem::take(&mut self.consumed_config);
        let saved_selector = self.current_selector.take();
        self.loading.push(key.to_string());

        // A plain-CSS module preserves its nesting (no Sass flattening, `&` kept
        // literal); a Sass module runs the normal evaluator.
        let result = if css {
            self.exec_css(&sheet.stmts, sink)
        } else {
            self.exec(&sheet.stmts, &[], sink)
        };

        self.loading.pop();
        // Capture this module's evaluated members before restoring the caller's
        // environment.
        let mut vars = std::mem::take(&mut self.scopes)
            .into_iter()
            .next()
            .unwrap_or_default();
        let mut functions = std::mem::take(&mut self.functions);
        let mut mixins = std::mem::take(&mut self.mixins);
        let used_user_modules = std::mem::take(&mut self.used_user_modules);
        let star_user_modules = std::mem::take(&mut self.star_user_modules);
        let used_builtin_modules = std::mem::take(&mut self.used_modules);
        let star_builtin_modules = std::mem::take(&mut self.star_modules);
        let forwarded = std::mem::take(&mut self.forwarded);
        // Config keys this module actually consumed (via a `!default` declaration
        // or by passing them through a `@forward ... with`).
        let consumed = std::mem::take(&mut self.consumed_config);

        // Restore the caller's environment.
        self.scopes = saved_scopes;
        self.scope_semi_global = saved_semi;
        self.functions = saved_funcs;
        self.mixins = saved_mixins;
        self.used_modules = saved_used;
        self.star_modules = saved_star;
        self.used_user_modules = saved_used_user;
        self.star_user_modules = saved_star_user;
        self.forwarded = saved_fwd;
        self.pending_config = saved_config;
        self.consumed_config = saved_consumed;
        self.current_selector = saved_selector;
        self.current_url = saved_url;
        self.current_source = saved_source;

        result?;
        let _ = pos;

        // Merge `@forward`ed members (lower precedence than the module's own).
        for (k, v) in forwarded.vars {
            vars.entry(k).or_insert(v);
        }
        for (k, v) in forwarded.functions {
            functions.entry(k).or_insert(v);
        }
        for (k, v) in forwarded.mixins {
            mixins.entry(k).or_insert(v);
        }

        Ok((
            Module {
                vars: RefCell::new(vars),
                functions,
                mixins,
                used_user_modules,
                star_user_modules,
                used_builtin_modules,
                star_builtin_modules,
                forwarded_builtins: forwarded.builtins,
                diag_url: diag_url.to_string(),
            },
            consumed,
        ))
    }

    /// Process a `@forward "<url>" [as p-*] [show ..|hide ..] [with (..)];`:
    /// load the target module (emitting its CSS), then re-export its public
    /// members from the module currently being evaluated, applying prefix and
    /// show/hide filters.
    #[allow(clippy::too_many_arguments)]
    fn exec_forward(
        &mut self,
        url: &str,
        prefix: Option<&str>,
        show: &Option<Vec<crate::ast::ForwardMember>>,
        hide: &Option<Vec<crate::ast::ForwardMember>>,
        config: &[crate::ast::ConfigEntry],
        pos: Pos,
        sink: &mut Sink<'_>,
    ) -> Result<(), Error> {
        // `@forward "sass:<mod>"` re-exports a built-in module. Built-ins can't
        // be configured.
        if let Some(m) = url.strip_prefix("sass:") {
            if !crate::builtins::is_module(m) {
                return Err(Error::at("Can't find stylesheet to import.".to_string(), pos));
            }
            if !config.is_empty() {
                return Err(Error::at(
                    "Built-in modules can't be configured.".to_string(),
                    pos,
                ));
            }
            self.forwarded.builtins.push(ForwardedBuiltin {
                module: m.to_string(),
                prefix: prefix.map(str::to_string),
                show: member_set(show, false),
                hide: member_set(hide, false),
            });
            return Ok(());
        }

        // Build the configuration passed to the forwarded module. The forward's
        // own `with (...)` entries combine with the configuration of the module
        // currently being evaluated (`pending_config`): a non-`!default` forward
        // entry hard-overrides; a `!default` forward entry yields to a matching
        // downstream override; downstream entries for variables the forward
        // re-exports (visible and matching its `as` prefix) flow through.
        let forward_conf = self.eval_config(config)?;
        let downstream = self.pending_config.clone();
        // Only downstream config for variables this forward actually re-exports
        // flows through. A `show`/`hide` filter or an `as p-*` prefix that hides
        // a variable also makes it unconfigurable through this forward. The map
        // value tracks (upstream-name, downstream-name) so consumption maps back.
        let var_visible = forward_var_visibility(show, hide);
        let pfx_opt = prefix;
        let mut passthrough: HashMap<String, (Value, bool)> = HashMap::default();
        // upstream config key -> downstream key it came from.
        let mut passthrough_origin: HashMap<String, String> = HashMap::default();
        for (dk, dv) in &downstream {
            // Map a downstream (prefixed) name back to the upstream member name.
            let upstream_name = match pfx_opt {
                Some(p) => match dk.strip_prefix(p) {
                    Some(rest) => rest.to_string(),
                    None => continue,
                },
                None => dk.clone(),
            };
            if is_private_member(&upstream_name) || !var_visible(&upstream_name) {
                continue;
            }
            passthrough.insert(upstream_name.clone(), dv.clone());
            passthrough_origin.insert(upstream_name, dk.clone());
        }
        let mut combined: HashMap<String, (Value, bool)> = passthrough.clone();
        // Keys whose downstream entry a `!default` forward override consumed.
        let mut forward_claimed: Vec<String> = Vec::new();
        // The forward's own (non-passthrough) keys, which the forwarded module
        // must consume (else configuring a non-`!default` variable -> error).
        let mut forward_own: Vec<String> = Vec::new();
        // Keys (upstream-side) a non-`!default` forward entry hard-overrode.
        let mut forward_shadowed: Vec<String> = Vec::new();
        for (name, (val, is_default)) in &forward_conf {
            if *is_default {
                // A downstream override wins over a `!default` forward entry —
                // but a `null` downstream value counts as "not configured", so
                // the forward default still applies.
                let downstream_overrides = passthrough
                    .get(name)
                    .is_some_and(|(v, _)| !matches!(v, Value::Null));
                if downstream_overrides {
                    forward_claimed.push(name.clone());
                } else {
                    combined.insert(name.clone(), (val.clone(), false));
                    forward_own.push(name.clone());
                }
            } else {
                if passthrough.contains_key(name) {
                    forward_shadowed.push(name.clone());
                }
                combined.insert(name.clone(), (val.clone(), false));
                forward_own.push(name.clone());
            }
        }

        let (module, consumed) = self.load_module(url, combined, pos, sink)?;

        // A non-passthrough forward entry the module never consumed configured a
        // variable that isn't `!default` in the forwarded module.
        if forward_own.iter().any(|k| !consumed.contains(k)) {
            return Err(Error::at(
                "This variable was not declared with !default in the @used module.".to_string(),
                pos,
            ));
        }
        // Mark the downstream config keys this forward consumed (passthrough +
        // `!default`-claimed) as consumed in the enclosing module, so they are
        // not reported as unused. A key a non-`!default` forward entry shadowed
        // stays unconsumed (the downstream override is then an error). The
        // consumed keys are upstream-side; map them back to downstream names.
        for up in consumed.iter().chain(forward_claimed.iter()) {
            if forward_shadowed.contains(up) {
                continue;
            }
            if let Some(dk) = passthrough_origin.get(up) {
                if !self.consumed_config.contains(dk) {
                    self.consumed_config.push(dk.clone());
                }
            }
        }

        let show_vars = member_set(show, true);
        let show_names = member_set(show, false);
        let hide_vars = member_set(hide, true);
        let hide_names = member_set(hide, false);
        let has_show = show.is_some();

        // `show`/`hide` names are dash/underscore-insensitive, so compare the
        // canonical (dashed) form.
        let visible_var = |name: &str| -> bool {
            if is_private_member(name) {
                return false;
            }
            let n = normalize_var_name(name);
            if has_show {
                show_vars.as_ref().map(|s| s.contains(&n)).unwrap_or(false)
            } else {
                !hide_vars.as_ref().map(|s| s.contains(&n)).unwrap_or(false)
            }
        };
        let visible_name = |name: &str| -> bool {
            if is_private_member(name) {
                return false;
            }
            let n = normalize_var_name(name);
            if has_show {
                show_names.as_ref().map(|s| s.contains(&n)).unwrap_or(false)
            } else {
                !hide_names.as_ref().map(|s| s.contains(&n)).unwrap_or(false)
            }
        };

        // Two `@forward`s that bring the same member name from DIFFERENT modules
        // conflict — an error reported immediately, even when the member is
        // never used. Re-forwarding the SAME module is idempotent.
        // With a prefix, `show`/`hide` names match the PREFIXED member name.
        // Private members (by their ORIGINAL name) are never re-exported.
        let src: *const Module = Rc::as_ptr(&module);
        let pfx = prefix.unwrap_or("");
        let module_vars: Vec<(String, Value)> = module
            .vars
            .borrow()
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        for (name, val) in &module_vars {
            let key = format!("{pfx}{name}");
            if !is_private_member(name) && visible_var(&key) {
                if let Some(prev) = self.forwarded.var_src.get(&key) {
                    if *prev != src {
                        return Err(Error::at(
                            format!("Two forwarded modules both define a variable named ${key}."),
                            pos,
                        ));
                    }
                }
                self.forwarded.vars.insert(key.clone(), val.clone());
                self.forwarded.var_src.insert(key, src);
            }
        }
        for (name, f) in &module.functions {
            let key = format!("{pfx}{name}");
            if !is_private_member(name) && visible_name(&key) {
                if let Some(prev) = self.forwarded.fn_src.get(&key) {
                    if *prev != src {
                        return Err(Error::at(
                            format!("Two forwarded modules both define a function named {key}."),
                            pos,
                        ));
                    }
                }
                self.forwarded.functions.insert(key.clone(), Rc::clone(f));
                self.forwarded.fn_src.insert(key, src);
            }
        }
        for (name, m) in &module.mixins {
            let key = format!("{pfx}{name}");
            if !is_private_member(name) && visible_name(&key) {
                if let Some(prev) = self.forwarded.mixin_src.get(&key) {
                    if *prev != src {
                        return Err(Error::at(
                            format!("Two forwarded modules both define a mixin named {key}."),
                            pos,
                        ));
                    }
                }
                self.forwarded.mixins.insert(key.clone(), Rc::clone(m));
                self.forwarded.mixin_src.insert(key, src);
            }
        }
        Ok(())
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
                Stmt::Comment(c) => {
                    let text = self.eval_template(c)?;
                    sink.push_comment(text);
                }
                Stmt::Decl(d) => {
                    if sink.is_top() {
                        return Err(Error::at("top-level declarations aren't allowed", d.pos));
                    }
                    if let Some(oi) = self.eval_decl(d)? {
                        sink.push_item(oi);
                    }
                }
                Stmt::PropertySet(ps) => {
                    if sink.is_top() {
                        return Err(Error::at("top-level declarations aren't allowed", ps.pos));
                    }
                    self.eval_property_set(ps, parents, sink)?;
                }
                Stmt::CustomDecl(d) => {
                    if sink.is_top() {
                        return Err(Error::at("top-level declarations aren't allowed", d.pos));
                    }
                    // A literal `--` name may never be nested inside a property
                    // set (dart-sass parse-time error).
                    if self.decl_prefix.is_some() {
                        return Err(Error::at(
                            "Declarations whose names begin with \"--\" may not be nested.",
                            d.pos,
                        ));
                    }
                    if let Some(oi) = self.eval_custom_decl(d)? {
                        sink.push_item(oi);
                    }
                }
                Stmt::Rule(r) => self.eval_style_rule(r, parents, sink)?,
                Stmt::If(branches) => {
                    // Evaluate conditions top to bottom; run the first match's
                    // body in a fresh semi-global scope so an assignment to an
                    // existing outer variable updates it (and can reach the
                    // global scope when every enclosing scope is semi-global),
                    // while a freshly declared variable stays local to the
                    // branch (dart-sass `visitIf`).
                    for branch in branches {
                        let take = match &branch.cond {
                            None => true,
                            Some(c) => self.eval_expr(c)?.is_truthy(),
                        };
                        if take {
                            self.push_scope(true);
                            let result = self.exec(&branch.body, parents, sink);
                            self.pop_scope();
                            result?;
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
                    let (start_i, end_i, unit) = self.for_bounds(from, to)?;
                    // The loop body runs in its own semi-global scope: the loop
                    // variable and any fresh assignments live there and vanish
                    // when the loop ends (dart-sass `visitForRule`).
                    self.push_scope(true);
                    let mut result = Ok(());
                    for i in for_indices(start_i, end_i, *inclusive) {
                        self.set_local(
                            var,
                            Value::Number(Number {
                                value: i as f64,
                                unit: unit.clone(),
                            }),
                        );
                        result = self.exec(body, parents, sink);
                        if result.is_err() {
                            break;
                        }
                    }
                    self.pop_scope();
                    result?;
                }
                Stmt::Each { vars, list, body } => {
                    let items = self.eval_each_items(list)?;
                    self.push_scope(true);
                    let mut result = Ok(());
                    for item in items {
                        self.bind_each(vars, item);
                        result = self.exec(body, parents, sink);
                        if result.is_err() {
                            break;
                        }
                    }
                    self.pop_scope();
                    result?;
                }
                Stmt::While { cond, body } => {
                    self.push_scope(true);
                    let mut result = Ok(());
                    let mut guard = 0u32;
                    loop {
                        match self.eval_expr(cond) {
                            Ok(v) if v.is_truthy() => {}
                            Ok(_) => break,
                            Err(e) => {
                                result = Err(e);
                                break;
                            }
                        }
                        result = self.exec(body, parents, sink);
                        if result.is_err() {
                            break;
                        }
                        guard += 1;
                        if guard >= 100_000 {
                            result = Err(Error::unpositioned("@while exceeded 100000 iterations"));
                            break;
                        }
                    }
                    self.pop_scope();
                    result?;
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
                Stmt::Include {
                    name,
                    args,
                    content,
                    content_params,
                    module,
                    pos,
                    length,
                } => {
                    // Push a diagnostic call frame so an error/warning raised in
                    // the mixin body unwinds through this `@include` call site.
                    let saved = self.enter_call(*pos, *length, &mixin_frame_name(name, module));
                    let r = self.exec_include(
                        name,
                        args,
                        content.clone(),
                        content_params.clone(),
                        module.as_deref(),
                        *pos,
                        parents,
                        sink,
                    );
                    self.leave_call(saved);
                    r?;
                }
                Stmt::Use {
                    url,
                    namespace,
                    star,
                    config,
                    pos,
                } => self.exec_use(url, namespace.as_deref(), *star, config, *pos, sink)?,
                Stmt::Forward {
                    url,
                    prefix,
                    show,
                    hide,
                    config,
                    pos,
                } => self.exec_forward(url, prefix.as_deref(), show, hide, config, *pos, sink)?,
                Stmt::Content(content_args) => {
                    // The content block runs in the caller's context, so it is no
                    // longer "directly in a mixin" (dart-sass): a
                    // `meta.content-exists()` inside it is an error.
                    self.in_mixin.push(false);
                    let result = self.exec_content(content_args, parents, sink);
                    self.in_mixin.pop();
                    result?;
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
                Stmt::Supports { condition, body } => {
                    self.eval_supports(condition, body, parents, sink)?;
                }
                Stmt::AtRoot { query, body } => {
                    self.eval_at_root(query.as_deref(), body, sink)?;
                }
                Stmt::Keyframes { name, prelude, body } => {
                    self.eval_keyframes(name, prelude, body, sink)?;
                }
                Stmt::Extend {
                    selector,
                    optional,
                    pos,
                } => self.register_extend(selector, *optional, *pos, parents)?,
                Stmt::Warn { value, pos } => self.emit_warn(value, *pos)?,
                Stmt::Debug { value, pos } => self.emit_debug(value, *pos)?,
                Stmt::Error { value, pos, length } => {
                    return Err(self.build_error(value, *pos, *length));
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
        // A selector that resolves to nothing (e.g. `#{&}` at the document root,
        // where `&` is null) is rejected by dart-sass with "expected selector".
        if sel_str.trim().is_empty() {
            return Err(Error::unpositioned("expected selector."));
        }
        validate_selector(&sel_str, !parents.is_empty())?;
        let current = resolve_selectors(&sel_str, parents);
        // Drop "bogus combinator" complex selectors from the emitted block;
        // dart-sass omits them from the generated CSS. A top-level TRAILING
        // combinator (`a >`) is bogus as a leaf (its own declaration block is
        // dropped) but valid for NESTING, so the full `current` list — including
        // such selectors — is still used for `&` resolution and as the `parents`
        // for nested rules (`a >` + `b` -> `a > b`). A nested rule that inherits
        // a genuinely bogus combinator (double, or leading/trailing in a pseudo)
        // is dropped in turn.
        // Per-complex source line-breaks (`a,\nb`). `current` is `parents ×
        // parts` (or just `parts` at the root), so complex `i` came from part
        // `i % parts.len()`; carry that part's "newline before" flag, filtered
        // in step with the dropped bogus selectors.
        let part_lbs = comma_linebreaks(&sel_str, !parents.is_empty());
        let n = part_lbs.len().max(1);
        let mut emit_selectors: Vec<String> = Vec::new();
        let mut emit_linebreaks: Vec<bool> = Vec::new();
        for (i, s) in current.iter().enumerate() {
            if complex_selector_block_is_bogus(s) {
                continue;
            }
            emit_selectors.push(s.clone());
            emit_linebreaks.push(part_lbs.get(i % n).copied().unwrap_or(false));
        }
        self.push_scope(false);
        let prev_selector = self.current_selector.replace(current.clone());
        let mut items: Vec<OutItem> = Vec::new();
        let mut nested: Vec<OutNode> = Vec::new();
        let result = {
            let mut child = Sink::Rule {
                selectors: &emit_selectors,
                linebreaks: &emit_linebreaks,
                items: &mut items,
                nested: &mut nested,
            };
            let r = self.exec(&rule.body, &current, &mut child);
            // Flush any declarations/loud comments that follow the last nested
            // rule, so they emit (in their own block) after the bubbled rules.
            if r.is_ok() {
                child.flush_rule_block();
            }
            r
        };
        self.current_selector = prev_selector;
        self.pop_scope();
        result?;
        sink.emit_style_rule(nested);
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
        let Some(stmts) = body else {
            // dart-sass strips a top-level (or bubbled-out) `@charset` entirely,
            // but keeps one that appears inside a style rule's block.
            if name == "charset" && !sink.is_rule() {
                return Ok(());
            }
            sink.push_childless_at_rule(name.to_string(), prelude);
            return Ok(());
        };
        // `@font-face` (exactly, case-sensitively, unprefixed) holds plain
        // declarations: dart-sass does NOT carry the enclosing style-rule
        // selector into its body — `a { @font-face { d: e } }` emits a bare
        // `@font-face { d: e }`. Every other at-rule (including `@page`,
        // `@-moz-font-face`, and unknown directives) wraps its body in the
        // enclosing selector.
        let body_parents: &[String] = if name == "font-face" { &[] } else { parents };
        let out_body = self.eval_at_body(stmts, body_parents)?;
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
        self.push_scope(false);
        let mut body: Vec<OutNode> = Vec::new();
        let result = if parents.is_empty() {
            let mut child = Sink::AtRoot(&mut body);
            self.exec(stmts, &[], &mut child)
        } else {
            let mut items: Vec<OutItem> = Vec::new();
            let mut nested: Vec<OutNode> = Vec::new();
            let res = {
                let mut child = Sink::Rule {
                    selectors: parents,
                    linebreaks: &[],
                    items: &mut items,
                    nested: &mut nested,
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

    /// Evaluate `@supports <condition> { body }`: serialize the structured
    /// condition canonically, run the body (bubbling like any at-rule), and emit
    /// the node — skipping emission entirely when the body produces nothing
    /// (dart-sass drops an empty/invisible `@supports`).
    fn eval_supports(
        &mut self,
        condition: &SupportsCondition,
        body: &[Stmt],
        parents: &[String],
        sink: &mut Sink<'_>,
    ) -> Result<(), Error> {
        let prelude = self.serialize_supports_condition(condition)?;
        let out_body = self.eval_at_body(body, parents)?;
        if out_body.is_empty() {
            return Ok(());
        }
        sink.push_at_rule(OutNode::AtRule {
            name: "supports".to_string(),
            prelude,
            body: out_body,
            has_block: true,
        });
        Ok(())
    }

    /// Serialize a `@supports` condition to its canonical CSS string
    /// (dart-sass `_visitSupportsCondition`).
    fn serialize_supports_condition(&mut self, condition: &SupportsCondition) -> Result<String, Error> {
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
            SupportsCondition::Interpolation(expr) => Ok(self.eval_expr(expr)?.to_interp()),
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
        self.push_scope(false);
        let mut out: Vec<OutNode> = Vec::new();
        let res = {
            let mut child = Sink::AtRoot(&mut out);
            self.exec(body, &[], &mut child)
        };
        self.pop_scope();
        res?;
        for node in out {
            sink.push_at_rule(node);
        }
        Ok(())
    }

    fn eval_decl(&mut self, d: &Declaration) -> Result<Option<OutItem>, Error> {
        let name = self.eval_template(&d.property)?.trim().to_string();
        let prop = match &self.decl_prefix {
            Some(prefix) => format!("{prefix}-{name}"),
            None => name,
        };
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
        // A first-class function reference is likewise not a valid CSS value.
        if let Value::Function(f) = &value {
            return Err(Error::at(
                format!("{} isn't a valid CSS value.", f.inspect()),
                d.pos,
            ));
        }
        // A first-class mixin reference is likewise not a valid CSS value.
        if let Value::Mixin(m) = &value {
            return Err(Error::at(
                format!("{} isn't a valid CSS value.", m.inspect()),
                d.pos,
            ));
        }
        // An empty unbracketed list (`()`, or e.g. `list.join((), ())`) cannot
        // serialize as a CSS value; a bracketed `[]` is fine, as is any list
        // with at least one element.
        if let Value::List(l) = &value {
            if l.items.is_empty() && !l.bracketed {
                return Err(Error::at("() isn't a valid CSS value.", d.pos));
            }
        }
        let vstr = value.to_css(self.compressed());
        // A value that serializes to nothing (an empty unquoted string, an
        // all-`null` list) drops the whole declaration, like a `null` value.
        if vstr.is_empty() {
            return Ok(None);
        }
        Ok(Some(OutItem::Decl {
            prop,
            value: vstr,
            important: d.important,
            custom: false,
        }))
    }

    /// Evaluate a custom-property declaration: the name and verbatim value are
    /// templates whose `#{…}` interpolation resolves; the value is otherwise
    /// emitted exactly as written (no SassScript evaluation). An empty value
    /// (`--x: ;`) still emits.
    fn eval_custom_decl(&mut self, d: &CustomDecl) -> Result<Option<OutItem>, Error> {
        let prop = self.eval_template(&d.property)?.trim().to_string();
        let value = self.eval_template(&d.value)?;
        Ok(Some(OutItem::Decl {
            prop,
            value,
            important: false,
            custom: true,
        }))
    }

    /// Evaluate a nested property set: resolve its (already prefixed) name,
    /// emit the optional leading value as a declaration, then run the body with
    /// that name installed as the child prefix so each child declaration is
    /// namespaced `<name>-<child>` and emitted in source order.
    fn eval_property_set(
        &mut self,
        ps: &PropertySet,
        parents: &[String],
        sink: &mut Sink<'_>,
    ) -> Result<(), Error> {
        // A nested property set whose own literal name begins with `--` is
        // rejected just like a plain `--` declaration would be when nested.
        if self.decl_prefix.is_some() && literal_name_is_custom_property(&ps.property) {
            return Err(Error::at(
                "Declarations whose names begin with \"--\" may not be nested.",
                ps.pos,
            ));
        }
        let name = self.eval_template(&ps.property)?.trim().to_string();
        let full = match &self.decl_prefix {
            Some(prefix) => format!("{prefix}-{name}"),
            None => name,
        };
        // The leading value (`b: c { … }`) emits `<full>: c;` before children.
        if let Some(value_expr) = &ps.value {
            let value = self.eval_expr(value_expr)?;
            if !matches!(value, Value::Null) {
                if let Some(m) = find_map(&value) {
                    return Err(Error::at(
                        format!("{} isn't a valid CSS value.", m.to_css(false)),
                        ps.pos,
                    ));
                }
                let vstr = value.to_css(self.compressed());
                sink.push_item(OutItem::Decl {
                    prop: full.clone(),
                    value: vstr,
                    important: ps.important,
                    custom: false,
                });
            }
        }
        let saved = self.decl_prefix.replace(full);
        let result = self.exec(&ps.body, parents, sink);
        self.decl_prefix = saved;
        result
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
                ImportArg::Sass { path, pos, length } => {
                    if is_css_import(path) {
                        sink.push_at_rule(OutNode::Raw(format!("@import \"{path}\";")));
                        continue;
                    }
                    // Every Sass `@import` of a non-CSS file fires the `[import]`
                    // deprecation, pointing at the quoted URL token.
                    self.emit_deprecation(&crate::deprecation::Deprecation::import(), *pos, *length);
                    // Run the caller's importer outside the arena scope so any
                    // state it caches (paths, sources) outlives this compile's
                    // arena reset; see the matching note in `load_module`.
                    let saved = crate::arena::pause();
                    let resolved = importer.and_then(|imp| imp.resolve_with_syntax(path));
                    crate::arena::resume(saved);
                    match resolved {
                        Some((src, syntax)) => {
                            if self.loading.iter().any(|p| p == path) {
                                return Err(Error::unpositioned("This file is already being loaded."));
                            }
                            let sheet = parse_with_syntax(&src, syntax)?;
                            self.loading.push(path.clone());
                            // `@import` inlines the file's variables/functions/
                            // mixins into the current scope, but its module
                            // bindings (`@use`/`@forward`) stay local to the
                            // imported file and must not leak to the importer.
                            let saved_used = std::mem::take(&mut self.used_modules);
                            let saved_star = std::mem::take(&mut self.star_modules);
                            let saved_used_user = std::mem::take(&mut self.used_user_modules);
                            let saved_star_user = std::mem::take(&mut self.star_user_modules);
                            // The imported file's own `@forward`s expose members
                            // as if defined in the importer; collect them
                            // separately, then merge into the current scope.
                            let saved_fwd = std::mem::take(&mut self.forwarded);
                            let result = self.exec(&sheet.stmts, parents, sink);
                            let imported_fwd = std::mem::replace(&mut self.forwarded, saved_fwd);
                            self.used_modules = saved_used;
                            self.star_modules = saved_star;
                            self.used_user_modules = saved_used_user;
                            self.star_user_modules = saved_star_user;
                            self.loading.pop();
                            result?;
                            // A `@forward`ed member from the imported file becomes
                            // an ordinary member of the importing scope. This
                            // build's functions/mixins are global, so only a
                            // top-level `@import` exposes them (a nested import's
                            // members stay scoped to the enclosing rule).
                            if self.scopes.len() == 1 {
                                for (k, f) in imported_fwd.functions {
                                    self.functions.insert(k, f);
                                }
                                for (k, m) in imported_fwd.mixins {
                                    self.mixins.insert(k, m);
                                }
                                if let Some(g) = self.scopes.first_mut() {
                                    for (k, val) in imported_fwd.vars {
                                        g.entry(k).or_insert(val);
                                    }
                                }
                            }
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

    /// The value of `&` in value position: the current resolved selector list
    /// as a comma-separated Sass list where each item is one complex selector
    /// (a space-separated list of compound-selector strings). At the document
    /// root (no enclosing style rule) this is `null`. This matches dart-sass,
    /// where `&` is always a comma list even for a single selector.
    fn parent_selector_value(&self) -> Value {
        let Some(selectors) = &self.current_selector else {
            return Value::Null;
        };
        if selectors.is_empty() {
            return Value::Null;
        }
        let items: Vec<Value> = selectors
            .iter()
            .map(|complex| {
                let mut compounds: Vec<Value> = complex
                    .split_whitespace()
                    .map(|c| {
                        Value::Str(SassStr {
                            text: c.to_string(),
                            quoted: false,
                        })
                    })
                    .collect();
                match compounds.len() {
                    1 => compounds.remove(0),
                    _ => Value::List(List {
                        items: compounds,
                        sep: ListSep::Space,
                        bracketed: false,
                        keywords: None,
                    }),
                }
            })
            .collect();
        Value::List(List {
            items,
            sep: ListSep::Comma,
            bracketed: false,
            keywords: None,
        })
    }

    fn eval_expr(&mut self, expr: &Expr) -> Result<Value, Error> {
        // Finalize any positioned error into a rendered diagnostic block here,
        // where `current_url`/`current_source`/`call_stack` still describe the
        // file and call context the error was raised in (cross-file safe).
        match self.eval_expr_inner(expr) {
            Ok(v) => Ok(v),
            Err(e) => Err(self.finalize_error(e)),
        }
    }

    fn eval_expr_inner(&mut self, expr: &Expr) -> Result<Value, Error> {
        match expr {
            Expr::Number(v, unit) => Ok(Value::Number(Number {
                value: *v,
                unit: unit.clone(),
            })),
            Expr::Color(c) => Ok(Value::Color(c.clone())),
            Expr::Bool(b) => Ok(Value::Bool(*b)),
            Expr::Null => Ok(Value::Null),
            Expr::Parent => Ok(self.parent_selector_value()),
            // Reading a variable drops a bare slash-division's spelling
            // (dart-sass `withoutSlash`): `$x: 1/2; a {b: $x}` is `0.5`.
            // Slashes nested inside a stored list are preserved.
            Expr::Var { name, pos } => match self.lookup(name) {
                Some(v) => Ok(v.clone().without_slash()),
                None => {
                    // A user module variable exposed unprefixed via `@use … as *`.
                    let star_hits: Vec<Value> = if is_private_member(name) {
                        Vec::new()
                    } else {
                        self.star_user_modules
                            .iter()
                            .filter_map(|m| m.var(name))
                            .collect()
                    };
                    if star_hits.len() > 1 {
                        return Err(Error::at(
                            "This variable is available from multiple global modules.",
                            *pos,
                        ));
                    }
                    if let Some(v) = star_hits.into_iter().next() {
                        return Ok(v.without_slash());
                    }
                    // A built-in module variable exposed unprefixed via
                    // `@use "sass:…" as *` (e.g. `$pi` from `sass:math`).
                    for m in &self.star_modules {
                        if let Ok(v) = crate::builtins::module_var(m, name, *pos) {
                            return Ok(v);
                        }
                    }
                    // The caret covers `$name` (the `$` plus the identifier).
                    Err(Error::at("Undefined variable.", *pos).with_length(1 + name.len()))
                }
            },
            Expr::NsVar { module, name } => self.eval_module_var(module, name, Pos { line: 1, col: 1 }),
            // A string expression (quoted/unquoted/lone-interpolation) resolves
            // its interpolation in a context where the `@supports`-declaration
            // no-simplify flag is OFF, so `(a: #{calc(1 + 2)})` -> `(a: 3)`
            // (the interpolated calc simplifies), matching dart-sass
            // `visitStringExpression`.
            Expr::QuotedString(pieces) => {
                let saved = std::mem::replace(&mut self.in_supports_declaration, false);
                let text = self.eval_template(pieces);
                self.in_supports_declaration = saved;
                Ok(Value::Str(SassStr {
                    text: text?,
                    quoted: true,
                }))
            }
            Expr::Ident(pieces) => {
                let saved = std::mem::replace(&mut self.in_supports_declaration, false);
                let text = self.eval_template(pieces);
                self.in_supports_declaration = saved;
                Ok(Value::Str(SassStr {
                    text: text?,
                    quoted: false,
                }))
            }
            Expr::Interp(inner) => {
                let saved = std::mem::replace(&mut self.in_supports_declaration, false);
                let v = self.eval_expr(inner);
                self.in_supports_declaration = saved;
                Ok(Value::Str(SassStr {
                    text: v?.to_interp(),
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
                    keywords: None,
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
                    // Unary minus negates a number; on any other operand
                    // dart-sass produces an unquoted `-<value>` string
                    // (`- red` -> `-red`, `- "q"` -> `-"q"`). A calculation that
                    // could not reduce to a number has no negation operator, so
                    // dart-sass rejects it ("Undefined operation \"-calc(…)\".").
                    UnOp::Neg => match v {
                        Value::Number(n) => Ok(Value::Number(Number {
                            value: -n.value,
                            unit: n.unit,
                        })),
                        Value::Calc(_) => Err(Error::unpositioned(format!(
                            "Undefined operation \"-{}\".",
                            v.to_css(false)
                        ))),
                        other => Ok(Value::Str(SassStr {
                            text: format!("-{}", other.to_css(false)),
                            quoted: false,
                        })),
                    },
                    // Unary plus is numeric identity; on any other operand it
                    // prepends `+` as an unquoted string (`+foo` -> `+foo`). A
                    // residual calculation has no unary-plus operator and is
                    // rejected the same way.
                    UnOp::Plus => match v {
                        Value::Number(_) => Ok(v),
                        Value::Calc(_) => Err(Error::unpositioned(format!(
                            "Undefined operation \"+{}\".",
                            v.to_css(false)
                        ))),
                        other => Ok(Value::Str(SassStr {
                            text: format!("+{}", other.to_css(false)),
                            quoted: false,
                        })),
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
            Expr::Div {
                lhs, rhs, slash, pos, ..
            } => {
                let l = self.eval_expr(lhs)?;
                let r = self.eval_expr(rhs)?;
                eval_div(l, r, *slash, *pos)
            }
            Expr::Calc { inner, .. } => {
                let node = self.eval_calc(inner)?;
                // Inside a `@supports` declaration the calculation is kept
                // unsimplified: the `calc()` wrapper is always preserved (even
                // around a single number), matching dart-sass `simplify: false`.
                if self.in_supports_declaration {
                    return Ok(Value::Calc(node));
                }
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
            Expr::Func {
                name,
                args,
                pos,
                length,
                module,
            } => {
                // A namespaced call `ns.fn(...)` resolves only against the used
                // built-in module bound to `ns`.
                if let Some(ns) = module {
                    return self.eval_module_call(ns, name, args, *pos, *length);
                }
                // Inside a `@supports` declaration, a CSS math function
                // (`min`/`max`/`clamp`/…) is kept unsimplified: its arguments
                // are resolved through the (non-folding) calc machinery and the
                // call is serialized verbatim, matching dart-sass
                // `simplify: false`. A user-defined function of the same name
                // still wins, so this only applies to builtins.
                if self.in_supports_declaration
                    && is_supports_calc_function(name)
                    && !self.functions.contains_key(name)
                {
                    return self.eval_supports_calc_func(name, args, *pos);
                }
                // if() is lazy: only the selected branch is evaluated.
                if name == "if" {
                    return self.eval_if_function(args, *pos);
                }
                // User-defined @function takes precedence over builtins.
                if let Some(func) = self.functions.get(name).cloned() {
                    return self.call_function(&func, args, Some((*pos, *length)));
                }
                // A user module function exposed unprefixed via `@use … as *`.
                if !self.star_user_modules.is_empty() && !is_private_member(name) {
                    let hits: Vec<(Rc<Module>, Rc<Callable>)> = self
                        .star_user_modules
                        .iter()
                        .filter_map(|m| m.function(name).map(|f| (Rc::clone(m), f)))
                        .collect();
                    if hits.len() > 1 {
                        return Err(Error::at(
                            "This function is available from multiple global modules.".to_string(),
                            *pos,
                        ));
                    }
                    if let Some((m, f)) = hits.into_iter().next() {
                        return self.call_user_module_function(&m, &f, args, Some((*pos, *length)));
                    }
                }
                // A bare `calc()` reaches here as a plain call (the parser only
                // treats `calc(<arg>)` as a calculation), so a user
                // `@function calc()` could have handled it above. With no user
                // override it is the CSS `calc()`, which requires an argument.
                if name.eq_ignore_ascii_case("calc") && args.is_empty() {
                    return Err(Error::at("Missing argument.", *pos));
                }
                // The pure CSS-calculation functions are parsed as
                // calculations, which cannot take a `...` rest argument.
                if is_calc_function(name) && args.iter().any(|a| a.splat) {
                    return Err(Error::at("Rest arguments can't be used with calculations.", *pos));
                }
                // The single-/double-argument math calculations (`sin`, `cos`,
                // `sqrt`, `pow`, `log`, `hypot`, …) parse their arguments as
                // calculation expressions, not ordinary SassScript. That means a
                // disallowed operator (`%`, comparison) inside an argument is
                // rejected ("This operation can't be used in a calculation."),
                // and an argument that does not reduce to a single number — it
                // still references a `var()`/interpolation/unknown ident — keeps
                // the whole call as a preserved calculation
                // (`sin(2px + var(--c))`) rather than erroring. When every
                // argument reduces to a plain number the normal builtin path
                // computes the result (and applies its unit checks), so this only
                // changes the two calc-specific behaviours above.
                if is_pure_calc_math_function(name)
                    && !self.functions.contains_key(name)
                    && !args.iter().any(|a| a.splat || a.name.is_some())
                {
                    if let Some(v) = self.try_eval_calc_math_call(name, args, *pos)? {
                        return Ok(v);
                    }
                }
                // `calc-size()` is a two-argument calculation: a sizing keyword
                // (or `var()`/calculation) plus a calculation, always preserved.
                if name.eq_ignore_ascii_case("calc-size")
                    && !self.functions.contains_key(name)
                    && !args.iter().any(|a| a.splat || a.name.is_some())
                {
                    return self.eval_calc_size(args, *pos);
                }
                // A three-argument `clamp()` evaluates its bounds and value as
                // calculations, so a `var()`/operation argument (`1% + 1px`)
                // keeps the call preserved instead of erroring as Sass
                // arithmetic. Other arities (a preserved single argument, or an
                // arity error) fall through to the builtin.
                if name.eq_ignore_ascii_case("clamp")
                    && !self.functions.contains_key(name)
                    && args.len() == 3
                    && !args.iter().any(|a| a.splat || a.name.is_some())
                {
                    return self.try_eval_clamp(args, *pos);
                }
                // `abs()` is a legacy global function that also exists as the CSS
                // `abs()` calculation. When its single positional argument
                // references a `var()`/interpolation it is parsed as a
                // calculation and preserved with its numeric subtree folded
                // (`abs(1px + 2px - var(--c))` -> `abs(3px - var(--c))`).
                // Without such a substitution the argument resolves to a plain
                // number, so the deprecated `math.abs` global handles it as
                // before (`abs(1 + 1px)` -> `2px`, `abs(-3) -> 3`).
                if name.eq_ignore_ascii_case("abs")
                    && !self.functions.contains_key(name)
                    && args.len() == 1
                    && args[0].name.is_none()
                    && !args[0].splat
                    && expr_contains_calc_substitution(&args[0].value)
                {
                    let node = self.eval_calc(&args[0].value)?;
                    return Ok(Value::Str(SassStr {
                        text: format!("abs({})", node.to_calc_css(self.compressed())),
                        quoted: false,
                    }));
                }
                // Evaluate args, expanding any `...` splat into positional /
                // keyword arguments.
                let (mut pos_args, mut named) = self.eval_call_args(args)?;
                // The global (deprecated) aliases of the `sass:meta` existence
                // predicates resolve against the evaluator state, not the
                // value-only builtin layer. A user-defined function of the same
                // name still wins (checked above).
                if matches!(
                    name.as_str(),
                    "variable-exists"
                        | "global-variable-exists"
                        | "mixin-exists"
                        | "function-exists"
                        | "content-exists"
                        | "get-function"
                        | "call"
                ) {
                    for v in &mut pos_args {
                        *v = std::mem::replace(v, Value::Null).without_slash();
                    }
                    for (_, v) in &mut named {
                        *v = std::mem::replace(v, Value::Null).without_slash();
                    }
                    if let Some(r) = self.try_meta_eval_call(name, &pos_args, &named, *pos) {
                        return r;
                    }
                }
                // The proprietary Microsoft `alpha()` filter overload: when the
                // global `alpha()` is called with one or more unquoted-string
                // arguments that each contain a `=` (an IE `alpha(opacity=80)`
                // hack, produced by the single-`=` operator), dart-sass passes
                // the call through verbatim as a CSS function instead of
                // treating the argument as a color.
                if name == "alpha"
                    && named.is_empty()
                    && !pos_args.is_empty()
                    && pos_args
                        .iter()
                        .all(|v| matches!(v, Value::Str(s) if !s.quoted && s.text.contains('=')))
                {
                    let inner = pos_args
                        .iter()
                        .map(|v| v.to_css(false))
                        .collect::<Vec<_>>()
                        .join(", ");
                    return Ok(Value::Str(SassStr {
                        text: format!("alpha({inner})"),
                        quoted: false,
                    }));
                }
                // A member exposed unprefixed via `@use "sass:<mod>" as *`: when
                // the bare name is not already a global builtin, route it to the
                // first star module that owns it (e.g. `div` -> `math.div`,
                // `set` -> `map.set`). Global builtins keep their own behaviour.
                if !crate::builtins::is_builtin(name) {
                    for m in self.star_modules.clone() {
                        if crate::builtins::module_has_member(&m, name) {
                            for v in &mut pos_args {
                                *v = std::mem::replace(v, Value::Null).without_slash();
                            }
                            for (n, v) in &mut named {
                                *v = std::mem::replace(v, Value::Null).without_slash();
                                let _ = n;
                            }
                            return crate::builtins::call_module(&m, name, &pos_args, &named, *pos);
                        }
                    }
                }
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

    /// Dispatch a namespaced call `ns.member(args)`. Resolves a user module
    /// first, then a built-in module bound to `ns`.
    fn eval_module_call(
        &mut self,
        ns: &str,
        member: &str,
        args: &[CallArg],
        pos: Pos,
        length: usize,
    ) -> Result<Value, Error> {
        // A user module bound to this namespace.
        if let Some(module) = self.used_user_modules.get(ns).cloned() {
            if is_private_member(member) {
                return Err(Error::at(
                    "Private members can't be accessed from outside their modules.".to_string(),
                    pos,
                ));
            }
            if let Some(func) = module.function(member) {
                return self.call_user_module_function(&module, &func, args, Some((pos, length)));
            }
            // Fall back to a built-in re-exported by this module via @forward.
            if let Some(v) = self.try_forwarded_builtin_call(&module, member, args, pos)? {
                return Ok(v);
            }
            return Err(Error::at("Undefined function.".to_string(), pos));
        }
        // A built-in module bound to this namespace.
        let module = match self.used_modules.get(ns) {
            Some(m) => m.clone(),
            None => {
                return Err(Error::at(
                    format!("There is no module with the namespace \"{ns}\"."),
                    pos,
                ));
            }
        };
        let (mut pos_args, mut named) = self.eval_call_args(args)?;
        for v in &mut pos_args {
            *v = std::mem::replace(v, Value::Null).without_slash();
        }
        for (_, v) in &mut named {
            *v = std::mem::replace(v, Value::Null).without_slash();
        }
        // The `sass:meta` introspection predicates need the evaluator's scopes /
        // definitions, which the value-only `call_module` cannot see.
        if module == "meta" {
            if let Some(r) = self.try_meta_eval_call(member, &pos_args, &named, pos) {
                return r;
            }
        }
        crate::builtins::call_module(&module, member, &pos_args, &named, pos)
    }

    /// Handle a `sass:meta` member that depends on the evaluator's state
    /// (variable/function/mixin/content existence). Returns `None` for any
    /// member this layer does not own, so the caller falls back to the
    /// value-only `call_module`. The arguments are already evaluated.
    fn try_meta_eval_call(
        &mut self,
        member: &str,
        pos_args: &[Value],
        named: &[(String, Value)],
        pos: Pos,
    ) -> Option<Result<Value, Error>> {
        match member {
            "variable-exists" => Some(self.meta_variable_exists(pos_args, named, pos, false)),
            "global-variable-exists" => Some(self.meta_variable_exists(pos_args, named, pos, true)),
            "mixin-exists" => Some(self.meta_mixin_exists(pos_args, named, pos)),
            "function-exists" => Some(self.meta_function_exists(pos_args, named, pos)),
            "content-exists" => Some(self.meta_content_exists(pos_args, pos)),
            "get-function" => Some(self.meta_get_function(pos_args, named, pos)),
            "get-mixin" => Some(self.meta_get_mixin(pos_args, named, pos)),
            "call" => Some(self.meta_call(pos_args, named, pos)),
            "module-variables" => Some(self.meta_module_members(pos_args, named, pos, MemberKind::Variable)),
            "module-functions" => Some(self.meta_module_members(pos_args, named, pos, MemberKind::Function)),
            "module-mixins" => Some(self.meta_module_members(pos_args, named, pos, MemberKind::Mixin)),
            "accepts-content" => Some(self.meta_accepts_content(pos_args, named, pos)),
            "keywords" => Some(Self::meta_keywords(pos_args, named, pos)),
            _ => None,
        }
    }

    /// `meta.keywords($args)`: the keyword arguments captured by a `$args...`
    /// rest parameter, as a map from each name (hyphen-normalized, unquoted) to
    /// its value. The argument must be an argument list, not an ordinary value.
    fn meta_keywords(pos_args: &[Value], named: &[(String, Value)], pos: Pos) -> Result<Value, Error> {
        let v = pos_args
            .first()
            .or_else(|| named.iter().find(|(n, _)| n == "args").map(|(_, v)| v))
            .ok_or_else(|| Error::at("Missing argument $args.".to_string(), pos))?;
        match v {
            Value::List(l) if l.keywords.is_some() => Ok(Value::Map(Map {
                entries: l.keywords.clone().unwrap_or_default(),
            })),
            other => Err(Error::at(
                format!("$args: {} is not an argument list.", other.to_css(false)),
                pos,
            )),
        }
    }

    /// `meta.accepts-content($mixin)`: whether the mixin reference's body uses a
    /// `@content` block. The only built-in mixin that does is `meta.apply`.
    fn meta_accepts_content(
        &self,
        pos_args: &[Value],
        named: &[(String, Value)],
        pos: Pos,
    ) -> Result<Value, Error> {
        let v = pos_args
            .first()
            .or_else(|| named.iter().find(|(n, _)| n == "mixin").map(|(_, v)| v))
            .ok_or_else(|| Error::at("Missing argument $mixin.".to_string(), pos))?;
        let mixin = match v {
            Value::Mixin(m) => m,
            other => {
                return Err(Error::at(
                    format!("$mixin: {} is not a mixin reference.", other.to_css(false)),
                    pos,
                ))
            }
        };
        let accepts = match &mixin.user {
            Some(any) => Rc::clone(any)
                .downcast::<Callable>()
                .map(|c| body_uses_content(&c.body))
                .unwrap_or(false),
            None => mixin.name == "apply",
        };
        Ok(Value::Bool(accepts))
    }

    /// `meta.get-function($name, $css: false, $module: null)`: capture a
    /// reference to the named function. A `$module` argument needs the user
    /// module loader (unsupported here) and is reported as an error. A user
    /// `@function` is captured by identity; otherwise a built-in (or, with
    /// `$css: true`, a plain-CSS) reference is returned.
    fn meta_get_function(
        &self,
        pos_args: &[Value],
        named: &[(String, Value)],
        pos: Pos,
    ) -> Result<Value, Error> {
        let params = ["name", "css", "module"];
        if pos_args.len() > params.len() {
            return Err(Error::at(
                format!(
                    "Only {} arguments allowed, but {} were passed.",
                    params.len(),
                    pos_args.len()
                ),
                pos,
            ));
        }
        let arg = |i: usize| -> Option<&Value> {
            pos_args
                .get(i)
                .or_else(|| named.iter().find(|(n, _)| n == params[i]).map(|(_, v)| v))
        };
        let name = match arg(0) {
            Some(Value::Str(s)) => s.text.clone(),
            Some(other) => {
                return Err(Error::at(
                    format!("$name: {} is not a string.", other.to_css(false)),
                    pos,
                ))
            }
            None => return Err(Error::at("Missing argument $name.", pos)),
        };
        let css = matches!(arg(1), Some(v) if v.is_truthy());
        // A `$module` namespace resolves the function from that `@use`d module.
        if let Some(module_v) = arg(2) {
            match module_v {
                Value::Null => {}
                Value::Str(s) => return self.get_function_from_module(&name, &s.text, pos),
                other => {
                    return Err(Error::at(
                        format!("$module: {} is not a string.", other.to_css(false)),
                        pos,
                    ))
                }
            }
        }
        if css {
            return Ok(Value::Function(SassFunction {
                name,
                css: true,
                user: None,
            }));
        }
        // A user `@function` of that name (dash/underscore-insensitive) wins.
        let key = normalize_arg_name(&name);
        if let Some((_, f)) = self.functions.iter().find(|(k, _)| normalize_arg_name(k) == key) {
            return Ok(Value::Function(SassFunction {
                name,
                css: false,
                user: Some(Rc::clone(f) as Rc<dyn std::any::Any>),
            }));
        }
        // A function exposed unprefixed via `@use … as *` (or forwarded into one).
        if !is_private_member(&name) {
            for m in &self.star_user_modules {
                if let Some(f) = m.function(&name) {
                    return Ok(Value::Function(SassFunction {
                        name,
                        css: false,
                        user: Some(Rc::clone(&f) as Rc<dyn std::any::Any>),
                    }));
                }
            }
        }
        if crate::builtins::is_builtin(&name) {
            return Ok(Value::Function(SassFunction {
                name,
                css: false,
                user: None,
            }));
        }
        Err(Error::at(format!("Function not found: {name}"), pos))
    }

    /// `meta.get-mixin($name, $module: null)`: capture a reference to the named
    /// mixin. A user `@mixin` is captured by identity (so a later redefinition
    /// yields a distinct reference); the built-in `sass:meta` mixins
    /// (`load-css`, `apply`) are captured by name. A `$module` argument resolves
    /// the mixin from that `@use`d module's namespace.
    fn meta_get_mixin(
        &self,
        pos_args: &[Value],
        named: &[(String, Value)],
        pos: Pos,
    ) -> Result<Value, Error> {
        let params = ["name", "module"];
        if pos_args.len() > params.len() {
            return Err(Error::at(
                format!(
                    "Only {} arguments allowed, but {} were passed.",
                    params.len(),
                    pos_args.len()
                ),
                pos,
            ));
        }
        let arg = |i: usize| -> Option<&Value> {
            pos_args
                .get(i)
                .or_else(|| named.iter().find(|(n, _)| n == params[i]).map(|(_, v)| v))
        };
        let name = match arg(0) {
            Some(Value::Str(s)) => s.text.clone(),
            Some(other) => {
                return Err(Error::at(
                    format!("$name: {} is not a string.", other.to_css(false)),
                    pos,
                ))
            }
            None => return Err(Error::at("Missing argument $name.", pos)),
        };
        // A `$module` argument resolves the mixin from another module's scope.
        if let Some(module_val) = arg(1) {
            if !matches!(module_val, Value::Null) {
                let module_name = match module_val {
                    Value::Str(s) => s.text.clone(),
                    other => {
                        return Err(Error::at(
                            format!("$module: {} is not a string.", other.to_css(false)),
                            pos,
                        ))
                    }
                };
                return self.get_mixin_from_module(&name, &module_name, pos);
            }
        }
        // A user `@mixin` of that name (dash/underscore-insensitive) wins.
        let key = normalize_arg_name(&name);
        if let Some((_, m)) = self.mixins.iter().find(|(k, _)| normalize_arg_name(k) == key) {
            return Ok(Value::Mixin(SassMixin {
                name,
                user: Some(Rc::clone(m) as Rc<dyn std::any::Any>),
                module: None,
            }));
        }
        // A mixin exposed unprefixed via `@use … as *`. Its body runs in the
        // owning module's environment, so capture that module too.
        if !self.star_user_modules.is_empty() && !is_private_member(&name) {
            let hits: Vec<&Rc<Module>> = self
                .star_user_modules
                .iter()
                .filter(|m| m.mixin(&name).is_some())
                .collect();
            if hits.len() > 1 {
                return Err(Error::at(
                    "This mixin is available from multiple global modules.",
                    pos,
                ));
            }
            if let Some(module) = hits.into_iter().next() {
                let m = module
                    .mixin(&name)
                    .ok_or_else(|| Error::at(format!("Mixin not found: {name}"), pos))?;
                return Ok(Value::Mixin(SassMixin {
                    name,
                    user: Some(Rc::clone(&m) as Rc<dyn std::any::Any>),
                    module: Some(Rc::clone(module) as Rc<dyn std::any::Any>),
                }));
            }
        }
        Err(Error::at(format!("Mixin not found: {name}"), pos))
    }

    /// Resolve a `$module`-qualified mixin reference for `meta.get-mixin`. The
    /// namespace must name a currently-`@use`d module; a built-in module's
    /// mixins (`meta.load-css`, `meta.apply`) resolve by name.
    /// `meta.get-function($name, $module: ns)`: capture a function reference from
    /// the module bound to `ns` — a user `@function` by identity, or a built-in
    /// member by name.
    fn get_function_from_module(&self, name: &str, module_name: &str, pos: Pos) -> Result<Value, Error> {
        if let Some(module) = self.used_user_modules.get(module_name) {
            if is_private_member(name) {
                return Err(Error::at(
                    "Private members can't be accessed from outside their modules.".to_string(),
                    pos,
                ));
            }
            if let Some(f) = module.function(name) {
                return Ok(Value::Function(SassFunction {
                    name: name.to_string(),
                    css: false,
                    user: Some(Rc::clone(&f) as Rc<dyn std::any::Any>),
                }));
            }
            return Err(Error::at(format!("Function not found: {name}"), pos));
        }
        if let Some(builtin) = self.used_modules.get(module_name) {
            if crate::builtins::module_has_member(builtin, name) {
                return Ok(Value::Function(SassFunction {
                    name: name.to_string(),
                    css: false,
                    user: None,
                }));
            }
            return Err(Error::at(format!("Function not found: {name}"), pos));
        }
        Err(Error::at(
            format!("There is no module with the namespace \"{module_name}\"."),
            pos,
        ))
    }

    fn get_mixin_from_module(&self, name: &str, module_name: &str, pos: Pos) -> Result<Value, Error> {
        if let Some(module) = self.used_user_modules.get(module_name) {
            if is_private_member(name) {
                return Err(Error::at(
                    "Private members can't be accessed from outside their modules.".to_string(),
                    pos,
                ));
            }
            if let Some(m) = module.mixin(name) {
                return Ok(Value::Mixin(SassMixin {
                    name: name.to_string(),
                    user: Some(Rc::clone(&m) as Rc<dyn std::any::Any>),
                    module: Some(Rc::clone(module) as Rc<dyn std::any::Any>),
                }));
            }
            return Err(Error::at(format!("Mixin not found: {name}"), pos));
        }
        if self.used_modules.contains_key(module_name) {
            if is_builtin_mixin(module_name, name) {
                return Ok(Value::Mixin(SassMixin {
                    name: name.to_string(),
                    user: None,
                    module: None,
                }));
            }
            return Err(Error::at(format!("Mixin not found: {name}"), pos));
        }
        Err(Error::at(
            format!("There is no module with the namespace \"{module_name}\"."),
            pos,
        ))
    }

    /// `meta.call($function, $args...)`: invoke a function reference (or, when
    /// `$function` is a string, the named function). The trailing arguments were
    /// already splat-expanded by `eval_call_args`.
    fn meta_call(&mut self, pos_args: &[Value], named: &[(String, Value)], pos: Pos) -> Result<Value, Error> {
        // `$function` is the first positional argument, or the named `$function`.
        let (func_val, rest_pos): (Value, Vec<Value>) = if let Some(first) = pos_args.first() {
            (first.clone(), pos_args[1..].to_vec())
        } else if let Some((_, v)) = named.iter().find(|(n, _)| n == "function") {
            (v.clone(), Vec::new())
        } else {
            return Err(Error::at("Missing argument $function.", pos));
        };
        // The remaining named args (excluding `$function`) are call keywords.
        let rest_named: Vec<(String, Value)> =
            named.iter().filter(|(n, _)| n != "function").cloned().collect();

        match func_val {
            // A first-class function reference.
            Value::Function(f) => self.invoke_function_ref(&f, rest_pos, rest_named, pos),
            // The deprecated string form: look up by name.
            Value::Str(s) => {
                let f = SassFunction {
                    name: s.text.clone(),
                    css: false,
                    user: self
                        .functions
                        .iter()
                        .find(|(k, _)| normalize_arg_name(k) == normalize_arg_name(&s.text))
                        .map(|(_, c)| Rc::clone(c) as Rc<dyn std::any::Any>),
                };
                self.invoke_function_ref(&f, rest_pos, rest_named, pos)
            }
            other => Err(Error::at(
                format!("$function: {} is not a function reference.", other.to_css(false)),
                pos,
            )),
        }
    }

    /// Invoke a resolved function reference with already-evaluated arguments.
    fn invoke_function_ref(
        &mut self,
        f: &SassFunction,
        pos_args: Vec<Value>,
        named: Vec<(String, Value)>,
        pos: Pos,
    ) -> Result<Value, Error> {
        // A captured user `@function`: bind the evaluated args and run its body.
        // The payload is a type-erased `Rc<Callable>`; recover it (cloning the
        // `Rc` so the borrow on `f` is released before running the body).
        if let Some(any) = &f.user {
            if let Ok(callable) = Rc::clone(any).downcast::<Callable>() {
                let frame = self.bind_evaled(&callable.params, (pos_args, named), &callable.name)?;
                self.push_scope_frame(frame);
                self.in_mixin.push(false);
                let result = self.run_fn_body(&callable.body);
                self.in_mixin.pop();
                self.pop_scope();
                return match result? {
                    Some(v) => Ok(v.without_slash()),
                    None => Err(Error::unpositioned(format!(
                        "Function {}() did not @return a value.",
                        callable.name
                    ))),
                };
            }
        }
        // A plain-CSS reference is preserved verbatim as a CSS function call.
        if f.css {
            let mut parts: Vec<String> = pos_args.iter().map(|v| v.to_css(false)).collect();
            for (n, v) in &named {
                parts.push(format!("${n}: {}", v.to_css(false)));
            }
            return Ok(Value::Str(SassStr {
                text: format!("{}({})", f.name, parts.join(", ")),
                quoted: false,
            }));
        }
        // A built-in reference: dispatch through the builtin library.
        crate::builtins::call(&f.name, &pos_args, &named, pos)
    }

    /// Read the single string `$name` argument of an existence predicate,
    /// enforcing arity (1 positional, or `$name`) and the string type.
    /// Parse the `$name` (and optional `$module` namespace, when `allow_module`)
    /// arguments of an existence predicate. A `null` `$module` is treated as
    /// absent. Returns `(name, module)`.
    fn exists_name_module_args(
        &self,
        pos_args: &[Value],
        named: &[(String, Value)],
        fname: &str,
        pos: Pos,
        allow_module: bool,
    ) -> Result<(String, Option<String>), Error> {
        let max = if allow_module { 2 } else { 1 };
        if pos_args.len() > max {
            return Err(Error::at(
                format!(
                    "Only {max} argument{} allowed, but {} were passed.",
                    if max == 1 { "" } else { "s" },
                    pos_args.len()
                ),
                pos,
            ));
        }
        let name_v = pos_args
            .first()
            .or_else(|| named.iter().find(|(n, _)| n == "name").map(|(_, v)| v))
            .ok_or_else(|| Error::at(format!("Missing argument $name for {fname}()."), pos))?;
        let name = match name_v {
            Value::Str(s) => s.text.clone(),
            other => {
                return Err(Error::at(
                    format!("$name: {} is not a string.", other.to_css(false)),
                    pos,
                ))
            }
        };
        let module = if allow_module {
            let m = pos_args
                .get(1)
                .or_else(|| named.iter().find(|(n, _)| n == "module").map(|(_, v)| v));
            match m {
                None | Some(Value::Null) => None,
                Some(Value::Str(s)) => Some(s.text.clone()),
                Some(other) => {
                    return Err(Error::at(
                        format!("$module: {} is not a string.", other.to_css(false)),
                        pos,
                    ))
                }
            }
        } else {
            None
        };
        Ok((name, module))
    }

    /// Whether the module bound to namespace `ns` defines a member `name` of the
    /// given kind (function/mixin/variable). An unknown namespace is an error.
    fn module_member_exists(&self, ns: &str, name: &str, kind: MemberKind, pos: Pos) -> Result<bool, Error> {
        if let Some(m) = self.used_user_modules.get(ns) {
            return Ok(match kind {
                MemberKind::Function => m.function(name).is_some(),
                MemberKind::Mixin => m.mixin(name).is_some(),
                MemberKind::Variable => m.var(name).is_some(),
            });
        }
        if let Some(builtin) = self.used_modules.get(ns).cloned() {
            return Ok(match kind {
                MemberKind::Function => crate::builtins::module_has_member(&builtin, name),
                MemberKind::Mixin => builtin == "meta" && matches!(name, "load-css" | "apply"),
                MemberKind::Variable => crate::builtins::module_var(&builtin, name, pos).is_ok(),
            });
        }
        Err(Error::at(
            format!("There is no module with the namespace \"{ns}\"."),
            pos,
        ))
    }

    /// `meta.module-variables/-functions/-mixins($module)`: a map from each
    /// (non-private) member name of the `@use`d module bound to `$module` to its
    /// value (variables) or a first-class reference (functions/mixins). Members
    /// are ordered by name (dart-sass uses source order; every spec module
    /// defines them alphabetically, so this matches byte-for-byte).
    fn meta_module_members(
        &self,
        pos_args: &[Value],
        named: &[(String, Value)],
        pos: Pos,
        kind: MemberKind,
    ) -> Result<Value, Error> {
        let fname = match kind {
            MemberKind::Function => "module-functions",
            MemberKind::Mixin => "module-mixins",
            MemberKind::Variable => "module-variables",
        };
        if pos_args.len() > 1 {
            return Err(Error::at(
                format!("Only 1 argument allowed, but {} were passed.", pos_args.len()),
                pos,
            ));
        }
        let v = pos_args
            .first()
            .or_else(|| named.iter().find(|(n, _)| n == "module").map(|(_, v)| v))
            .ok_or_else(|| Error::at(format!("Missing argument $module for {fname}()."), pos))?;
        let ns = match v {
            Value::Str(s) => s.text.clone(),
            other => {
                return Err(Error::at(
                    format!("$module: {} is not a string.", other.to_css(false)),
                    pos,
                ))
            }
        };
        let Some(module) = self.used_user_modules.get(&ns).cloned() else {
            // Built-in module introspection isn't modeled here; an unknown
            // namespace (built-in or absent) is reported uniformly.
            return Err(Error::at(
                format!("There is no module with the namespace \"{ns}\"."),
                pos,
            ));
        };
        let mut names: Vec<String> = match kind {
            MemberKind::Variable => module.vars.borrow().keys().cloned().collect(),
            MemberKind::Function => module.functions.keys().cloned().collect(),
            MemberKind::Mixin => module.mixins.keys().cloned().collect(),
        };
        names.retain(|n| !is_private_member(n));
        names.sort();
        let entries: Vec<(Value, Value)> = names
            .into_iter()
            .map(|name| {
                // Member names are canonicalized to the dashed form for the map
                // key (dart-sass: `$e_f` is keyed `"e-f"`); the value keeps the
                // variable's own value verbatim.
                let key = Value::Str(SassStr {
                    text: name.replace('_', "-"),
                    quoted: true,
                });
                let val = match kind {
                    MemberKind::Variable => module.var(&name).unwrap_or(Value::Null),
                    MemberKind::Function => Value::Function(SassFunction {
                        name: name.clone(),
                        css: false,
                        user: module
                            .function(&name)
                            .map(|f| Rc::clone(&f) as Rc<dyn std::any::Any>),
                    }),
                    MemberKind::Mixin => Value::Mixin(SassMixin {
                        name: name.clone(),
                        user: module
                            .mixin(&name)
                            .map(|m| Rc::clone(&m) as Rc<dyn std::any::Any>),
                        module: Some(Rc::clone(&module) as Rc<dyn std::any::Any>),
                    }),
                };
                (key, val)
            })
            .collect();
        Ok(Value::Map(Map { entries }))
    }

    /// `meta.variable-exists($name)` / `meta.global-variable-exists($name)`:
    /// whether a variable of that name is in scope (globally only when
    /// `global`). Names are matched dash/underscore-insensitively.
    fn meta_variable_exists(
        &self,
        pos_args: &[Value],
        named: &[(String, Value)],
        pos: Pos,
        global: bool,
    ) -> Result<Value, Error> {
        let fname = if global {
            "global-variable-exists"
        } else {
            "variable-exists"
        };
        // Only `global-variable-exists` takes the optional `$module` namespace.
        let (name, module) = self.exists_name_module_args(pos_args, named, fname, pos, global)?;
        if let Some(ns) = module {
            return Ok(Value::Bool(self.module_member_exists(
                &ns,
                &name,
                MemberKind::Variable,
                pos,
            )?));
        }
        let key = normalize_arg_name(&name);
        let scopes: &[HashMap<String, Value>] = if global { &self.scopes[..1] } else { &self.scopes };
        let found = scopes
            .iter()
            .any(|s| s.keys().any(|k| normalize_arg_name(k) == key));
        if found {
            return Ok(Value::Bool(true));
        }
        // A variable exposed unprefixed via `@use … as *` (or forwarded into
        // one). Exposure from more than one star module is ambiguous.
        let count = self.star_member_count(&name, MemberKind::Variable);
        if count > 1 {
            return Err(Error::at(
                "This variable is available from multiple global modules.",
                pos,
            ));
        }
        Ok(Value::Bool(count >= 1))
    }

    /// `meta.mixin-exists($name)`: whether a mixin of that name is defined.
    fn meta_mixin_exists(
        &self,
        pos_args: &[Value],
        named: &[(String, Value)],
        pos: Pos,
    ) -> Result<Value, Error> {
        let (name, module) = self.exists_name_module_args(pos_args, named, "mixin-exists", pos, true)?;
        if let Some(ns) = module {
            return Ok(Value::Bool(self.module_member_exists(
                &ns,
                &name,
                MemberKind::Mixin,
                pos,
            )?));
        }
        let key = normalize_arg_name(&name);
        let local = self.mixins.keys().any(|k| normalize_arg_name(k) == key);
        if local {
            return Ok(Value::Bool(true));
        }
        let count = self.star_member_count(&name, MemberKind::Mixin);
        if count > 1 {
            return Err(Error::at(
                "This mixin is available from multiple global modules.",
                pos,
            ));
        }
        Ok(Value::Bool(count >= 1))
    }

    /// `meta.function-exists($name)`: whether a user `@function` or a built-in
    /// of that name exists.
    fn meta_function_exists(
        &self,
        pos_args: &[Value],
        named: &[(String, Value)],
        pos: Pos,
    ) -> Result<Value, Error> {
        let (name, module) = self.exists_name_module_args(pos_args, named, "function-exists", pos, true)?;
        if let Some(ns) = module {
            return Ok(Value::Bool(self.module_member_exists(
                &ns,
                &name,
                MemberKind::Function,
                pos,
            )?));
        }
        let key = normalize_arg_name(&name);
        let user = self.functions.keys().any(|k| normalize_arg_name(k) == key);
        if user {
            return Ok(Value::Bool(true));
        }
        // A function exposed unprefixed via `@use … as *` (or forwarded into a
        // module that is itself `@use`d as `*`). Exposure from more than one
        // star module is ambiguous.
        let count = self.star_member_count(&name, MemberKind::Function);
        if count > 1 {
            return Err(Error::at(
                "This function is available from multiple global modules.",
                pos,
            ));
        }
        Ok(Value::Bool(count >= 1 || crate::builtins::is_builtin(&name)))
    }

    /// Count how many `@use … as *` modules expose `name` as the given member
    /// kind; more than one means an unqualified reference is ambiguous.
    fn star_member_count(&self, name: &str, kind: MemberKind) -> usize {
        if is_private_member(name) {
            return 0;
        }
        self.star_user_modules
            .iter()
            .filter(|m| match kind {
                MemberKind::Variable => m.var(name).is_some(),
                MemberKind::Mixin => m.mixin(name).is_some(),
                MemberKind::Function => m.function(name).is_some(),
            })
            .count()
    }

    /// `meta.content-exists()`: whether the enclosing mixin was passed a
    /// `@content` block. It is an error to call this outside a mixin body.
    fn meta_content_exists(&self, pos_args: &[Value], pos: Pos) -> Result<Value, Error> {
        if !pos_args.is_empty() {
            return Err(Error::at(
                format!("Only 0 arguments allowed, but {} were passed.", pos_args.len()),
                pos,
            ));
        }
        if self.in_mixin.last().copied() != Some(true) {
            return Err(Error::at(
                "content-exists() may only be called within a mixin.",
                pos,
            ));
        }
        let has = matches!(self.content_stack.last(), Some(Some(_)));
        Ok(Value::Bool(has))
    }

    /// Try `member` against a built-in module re-exported by `module` via
    /// `@forward "sass:x"` (honouring an `as p-*` prefix).
    fn try_forwarded_builtin_call(
        &mut self,
        module: &Rc<Module>,
        member: &str,
        args: &[CallArg],
        pos: Pos,
    ) -> Result<Option<Value>, Error> {
        for fb in &module.forwarded_builtins {
            let bare = match &fb.prefix {
                Some(p) => match member.strip_prefix(p.as_str()) {
                    Some(rest) => rest,
                    None => continue,
                },
                None => member,
            };
            if fb.visible(bare) && crate::builtins::module_has_member(&fb.module, bare) {
                let (mut pos_args, mut named) = self.eval_call_args(args)?;
                for v in &mut pos_args {
                    *v = std::mem::replace(v, Value::Null).without_slash();
                }
                for (_, v) in &mut named {
                    *v = std::mem::replace(v, Value::Null).without_slash();
                }
                return Ok(Some(crate::builtins::call_module(
                    &fb.module, bare, &pos_args, &named, pos,
                )?));
            }
        }
        Ok(None)
    }

    /// Call a user module's function in the module's own environment: bind the
    /// arguments in the caller's context, then swap in the module's globals/
    /// functions/mixins/used-modules so the body resolves against the module.
    fn call_user_module_function(
        &mut self,
        module: &Rc<Module>,
        func: &Rc<Callable>,
        args: &[CallArg],
        call: Option<(Pos, usize)>,
    ) -> Result<Value, Error> {
        let frame = self.bind_args(&func.params, args, &func.name)?;
        let saved_member = call.map(|(pos, len)| self.enter_call(pos, len, &format!("{}()", func.name)));
        let saved = self.enter_module(module);
        let saved_file = self.enter_module_file(module);
        self.push_scope_frame(frame);
        let result = self.run_fn_body(&func.body);
        self.pop_scope();
        self.leave_module_file(saved_file);
        self.leave_module(saved);
        if let Some(saved_member) = saved_member {
            self.leave_call(saved_member);
        }
        match result? {
            Some(v) => Ok(v.without_slash()),
            None => Err(Error::unpositioned(format!(
                "Function {}() did not @return a value.",
                func.name
            ))),
        }
    }

    /// Swap in `module`'s source file for diagnostics during a cross-module
    /// member invocation. Returns the previous `(url, source)` to restore.
    fn enter_module_file(&mut self, module: &Rc<Module>) -> Option<(String, Rc<str>)> {
        if module.diag_url.is_empty() {
            return None;
        }
        let source = self.source_for(&module.diag_url);
        Some((
            std::mem::replace(&mut self.current_url, module.diag_url.clone()),
            std::mem::replace(&mut self.current_source, source),
        ))
    }

    /// Restore the file swapped out by [`Self::enter_module_file`].
    fn leave_module_file(&mut self, saved: Option<(String, Rc<str>)>) {
        if let Some((url, source)) = saved {
            self.current_url = url;
            self.current_source = source;
        }
    }

    /// The diagnostic display URL for a `@use`/`@import`ed module: the basename
    /// of the resolved key (dart-sass shows e.g. `_libchain.scss`), falling back
    /// to the `@use` url spelling when the key has no useful tail.
    fn module_diag_url(&self, url: &str, key: &str) -> String {
        let base = key.rsplit(['/', '\\']).next().unwrap_or(key);
        if base.is_empty() {
            url.to_string()
        } else {
            base.to_string()
        }
    }

    /// Install `module`'s environment for a cross-module member invocation,
    /// returning the previous environment to restore with [`leave_module`].
    fn enter_module(&mut self, module: &Rc<Module>) -> SavedModuleEnv {
        let module_scope = module.vars.borrow().clone();
        SavedModuleEnv {
            scopes: std::mem::replace(&mut self.scopes, vec![module_scope]),
            scope_semi_global: std::mem::replace(&mut self.scope_semi_global, vec![true]),
            functions: std::mem::replace(&mut self.functions, module.functions.clone()),
            mixins: std::mem::replace(&mut self.mixins, module.mixins.clone()),
            used_modules: std::mem::replace(&mut self.used_modules, module.used_builtin_modules.clone()),
            star_modules: std::mem::replace(&mut self.star_modules, module.star_builtin_modules.clone()),
            used_user_modules: std::mem::replace(
                &mut self.used_user_modules,
                module.used_user_modules.clone(),
            ),
            star_user_modules: std::mem::replace(
                &mut self.star_user_modules,
                module.star_user_modules.clone(),
            ),
            write_back: Some(Rc::clone(module)),
        }
    }

    /// Restore the environment captured by [`enter_module`]. If the saved env
    /// recorded a module, its (possibly mutated) global scope is written back so
    /// a `!global` assignment inside the module persists.
    fn leave_module(&mut self, saved: SavedModuleEnv) {
        if let Some(module) = &saved.write_back {
            if let Some(scope0) = self.scopes.first() {
                *module.vars.borrow_mut() = scope0.clone();
            }
        }
        self.scopes = saved.scopes;
        self.scope_semi_global = saved.scope_semi_global;
        self.functions = saved.functions;
        self.mixins = saved.mixins;
        self.used_modules = saved.used_modules;
        self.star_modules = saved.star_modules;
        self.used_user_modules = saved.used_user_modules;
        self.star_user_modules = saved.star_user_modules;
    }

    /// Resolve a namespaced module variable `ns.$name`. Resolves a user module
    /// first, then a built-in module bound to `ns`.
    fn eval_module_var(&self, ns: &str, name: &str, pos: Pos) -> Result<Value, Error> {
        if let Some(module) = self.used_user_modules.get(ns) {
            if is_private_member(name) {
                return Err(Error::at(
                    "Private members can't be accessed from outside their modules.".to_string(),
                    pos,
                ));
            }
            return match module.var(name) {
                Some(v) => Ok(v.without_slash()),
                None => Err(Error::at("Undefined variable.".to_string(), pos)),
            };
        }
        match self.used_modules.get(ns) {
            Some(module) => crate::builtins::module_var(module, name, pos),
            None => Err(Error::at(
                format!("There is no module with the namespace \"{ns}\"."),
                pos,
            )),
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

    /// Serialize a CSS math function (`min`/`max`/`clamp`/…) verbatim inside a
    /// `@supports` declaration: each argument is resolved through the
    /// (non-folding) calc machinery and joined with `, `. Used only when
    /// `in_supports_declaration` is set.
    fn eval_supports_calc_func(&mut self, name: &str, args: &[CallArg], pos: Pos) -> Result<Value, Error> {
        if args.iter().any(|a| a.splat) {
            return Err(Error::at("Rest arguments can't be used with calculations.", pos));
        }
        let mut parts = Vec::with_capacity(args.len());
        for a in args {
            let inner = self.eval_calc(&a.value)?.to_calc_css(self.compressed());
            // A named argument (`min($a: 1)`) is not valid in a calculation, but
            // we preserve any name verbatim to mirror the surface syntax.
            match &a.name {
                Some(n) => parts.push(format!("${n}: {inner}")),
                None => parts.push(inner),
            }
        }
        Ok(Value::Str(SassStr {
            text: format!("{name}({})", parts.join(", ")),
            quoted: false,
        }))
    }

    /// Try to evaluate a single-/double-argument math calculation (`sin`,
    /// `sqrt`, `pow`, `log`, `hypot`, …) as a calculation. Each argument is
    /// evaluated through the calc machinery, which rejects disallowed operators
    /// (`%`, comparisons) the way dart-sass does.
    ///
    /// - When *every* argument folds to a single number, returns `Ok(None)`:
    ///   the caller falls through to the ordinary builtin, which computes the
    ///   result and applies its unit checks (so `sqrt(2)`, `sin(1deg)`,
    ///   `sin(1px)`-the-error all behave exactly as before).
    /// - When an argument still carries an opaque operand — a `var()`,
    ///   interpolation, or unknown identifier — the whole call is preserved as a
    ///   calculation string (`sin(2px + var(--c))`).
    /// - When an argument reduces to a numeric operation we cannot collapse to a
    ///   single number (compound/inverse units), returns `Ok(None)` so the
    ///   builtin re-evaluates and reports its own error, rather than silently
    ///   preserving a value dart-sass would reject.
    fn try_eval_calc_math_call(
        &mut self,
        name: &str,
        args: &[CallArg],
        _pos: Pos,
    ) -> Result<Option<Value>, Error> {
        let mut nodes = Vec::with_capacity(args.len());
        for a in args {
            nodes.push(self.eval_calc(&a.value)?);
        }
        // Every argument is a plain number: let the builtin compute.
        if nodes.iter().all(|n| matches!(n, CalcNode::Number(_))) {
            return Ok(None);
        }
        // No opaque operand anywhere: the calculation is purely numeric but did
        // not collapse (compound/inverse units). Defer to the builtin so it can
        // raise the dart-sass error instead of us preserving an invalid value.
        if !nodes.iter().any(calc_node_has_opaque) {
            return Ok(None);
        }
        let lname = name.to_ascii_lowercase();
        let parts: Vec<String> = nodes.iter().map(|n| n.to_calc_css(self.compressed())).collect();
        Ok(Some(Value::Str(SassStr {
            text: format!("{lname}({})", parts.join(", ")),
            quoted: false,
        })))
    }

    /// Evaluate a three-argument `clamp(min, value, max)` calculation. Each
    /// argument is evaluated through the calc machinery (rejecting `%` and other
    /// non-calculation operators). When every argument folds to a single number,
    /// the builtin clamps/unit-checks them as before; when an argument keeps a
    /// `var()`/calculation operand the call is preserved
    /// (`clamp(1% + 1px, 2px, 3px)`). A resolved operand with complex units is
    /// rejected like dart-sass.
    fn try_eval_clamp(&mut self, args: &[CallArg], pos: Pos) -> Result<Value, Error> {
        let mut nodes = Vec::with_capacity(args.len());
        for a in args {
            let node = self.eval_calc(&a.value)?;
            if let Some(complex) = calc_complex_unit_operand(&node) {
                return Err(Error::at(
                    format!(
                        "Number calc({}) isn't compatible with CSS calculations.",
                        complex.to_calc_css(false)
                    ),
                    pos,
                ));
            }
            nodes.push(node);
        }
        // Every argument is a plain number: let the builtin clamp them (and run
        // its incompatible-unit checks).
        if nodes.iter().all(|n| matches!(n, CalcNode::Number(_))) {
            let values: Vec<Value> = nodes
                .into_iter()
                .map(|n| match n {
                    CalcNode::Number(num) => Value::Number(num),
                    // Unreachable: guarded by the `all` check above.
                    other => Value::Calc(other),
                })
                .collect();
            return crate::builtins::call("clamp", &values, &[], pos);
        }
        let parts: Vec<String> = nodes.iter().map(|n| n.to_calc_css(self.compressed())).collect();
        Ok(Value::Str(SassStr {
            text: format!("clamp({})", parts.join(", ")),
            quoted: false,
        }))
    }

    /// Evaluate a `calc-size(target, value)` calculation. The target (`auto`,
    /// `none`, `size`, a `var()`, or a nested calculation) and the optional
    /// value are each evaluated through the calc machinery and the call is kept
    /// preserved (`calc-size()` never reduces to a number). Exactly one or two
    /// arguments are accepted.
    fn eval_calc_size(&mut self, args: &[CallArg], pos: Pos) -> Result<Value, Error> {
        if args.is_empty() {
            return Err(Error::at("Missing argument.", pos));
        }
        if args.len() > 2 {
            return Err(Error::at(
                format!("Only 2 arguments allowed, but {} were passed.", args.len()),
                pos,
            ));
        }
        let mut parts = Vec::with_capacity(args.len());
        for a in args {
            parts.push(self.eval_calc(&a.value)?.to_calc_css(self.compressed()));
        }
        Ok(Value::Str(SassStr {
            text: format!("calc-size({})", parts.join(", ")),
            quoted: false,
        }))
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
                    // Modulo, comparisons, and `and`/`or` are not arithmetic;
                    // dart-sass rejects them inside a calculation rather than
                    // evaluating them (`calc(1px % 2px)`, `calc(1 > 2)`).
                    _ => return Err(Error::at("This operation can't be used in a calculation.", *pos)),
                };
                let l = self.eval_calc(lhs)?;
                let r = self.eval_calc(rhs)?;
                if self.in_supports_declaration {
                    return Ok(CalcNode::Op {
                        op: calc_op,
                        left: Box::new(l),
                        right: Box::new(r),
                    });
                }
                fold_calc(calc_op, l, r, *pos)
            }
            Expr::Div { lhs, rhs, pos, .. } => {
                let l = self.eval_calc(lhs)?;
                let r = self.eval_calc(rhs)?;
                if self.in_supports_declaration {
                    return Ok(CalcNode::Op {
                        op: CalcOp::Div,
                        left: Box::new(l),
                        right: Box::new(r),
                    });
                }
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
            // A nested calc() flattens into the surrounding calculation —
            // except inside a `@supports` declaration, where dart-sass keeps the
            // calculation unsimplified, so the inner `calc(...)` stays wrapped.
            // Otherwise an unresolved single-string operand (an interpolation or
            // a `var()` substitution that is not a clean operand) is
            // parenthesized: dart-sass writes `calc(calc(#{"c*"}))` as
            // `calc((c*))` and `calc(1 + calc(var(--c)))` as `calc(1 +
            // (var(--c)))`. A clean identifier, number, operation, or complete
            // sub-calculation flattens without extra parens.
            Expr::Calc { inner, .. } => {
                if self.in_supports_declaration {
                    let s = self.eval_calc(inner)?.to_calc_css(self.compressed());
                    return Ok(CalcNode::Str(format!("calc({s})")));
                }
                let node = self.eval_calc(inner)?;
                match node {
                    CalcNode::Str(s) if nested_calc_needs_parens(&s) => Ok(CalcNode::Str(format!("({s})"))),
                    other => Ok(other),
                }
            }
            // A space-separated list written directly in the calc interior is
            // an "unparsed" run: it is only valid when it contains a `var()`/
            // `env()` substitution or an interpolation, which dart-sass splices
            // verbatim (`calc(var(--c) 1)`, `calc(#{"1 +"} 2)` -> `calc(1 +
            // 2)`). A space-list of ordinary operands (`calc(1 2)`,
            // `calc(c 1 2)`, `calc($c $d)`) has no operator between adjacent
            // terms, which dart-sass rejects with "Missing math operator.".
            Expr::List {
                items,
                sep: ListSep::Space,
                bracketed: false,
            } => {
                if !items.iter().any(expr_has_substitution) {
                    return Err(Error::unpositioned("Missing math operator."));
                }
                let mut parts = Vec::with_capacity(items.len());
                for it in items {
                    parts.push(self.eval_calc(it)?.to_calc_css(false));
                }
                Ok(CalcNode::Str(parts.join(" ")))
            }
            // Any leaf (number, var(), interpolation, ident) evaluates to a
            // value and becomes a calc operand.
            other => {
                let v = self.eval_expr(other)?;
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
                // Only a number, a nested calculation, or an unquoted string
                // (a `var()`, interpolation, ident, or other special CSS value)
                // is a legal calculation operand. A null, bool, color, list,
                // map, or quoted string evaluated into the calc — typically via
                // a `$variable` or function call — is rejected the way
                // dart-sass does ("Value … can't be used in a calculation.").
                match &v {
                    Value::Number(_) | Value::Calc(_) | Value::Slash(_, _) => {}
                    Value::Str(s) if !s.quoted => {}
                    other => {
                        return Err(Error::unpositioned(format!(
                            "Value {} can't be used in a calculation.",
                            calc_value_repr(other)
                        )));
                    }
                }
                Ok(value_to_calc_node(v))
            }
        }
    }
}

/// Whether a calc space-list item is a substitution that makes the unparsed
/// run legal: a `#{…}` interpolation, or a `var()`/`env()` reference (which
/// the parser lowers to an [`Expr::Ident`] whose text begins with `var(`/
/// `env(`, possibly after a vendor prefix). A plain ident, number, nested
/// calc, or variable is NOT a substitution.
/// Whether a property-name template begins, *literally*, with `--` (a custom
/// property). A name whose first piece is `#{…}` interpolation is not literal,
/// so `#{--b}` is allowed to namespace inside a property set while a written
/// `--b` is not.
fn literal_name_is_custom_property(property: &[TplPiece]) -> bool {
    match property.first() {
        Some(TplPiece::Lit(s)) => s.trim_start().starts_with("--"),
        _ => false,
    }
}

fn expr_has_substitution(e: &Expr) -> bool {
    match e {
        Expr::Interp(_) => true,
        // `var()`/`env()` are parsed as plain function calls; inside a calc
        // space-list they are legal substitutions (`calc(var(--c) 1)`).
        Expr::Func { name, .. } => name.eq_ignore_ascii_case("var") || name.eq_ignore_ascii_case("env"),
        Expr::Ident(pieces) => pieces.iter().any(|p| match p {
            TplPiece::Interp(_) => true,
            TplPiece::Lit(s) => {
                let lower = s.trim_start().to_ascii_lowercase();
                lower.starts_with("var(") || lower.starts_with("env(")
            }
        }),
        _ => false,
    }
}

/// Whether an expression tree contains a calculation substitution — a
/// `var()`/`env()` reference or an interpolation — anywhere within it
/// (recursing through operations, parentheses, nested calculations, and
/// lists). Used to decide whether a legacy global math function such as `abs()`
/// is being used as a CSS calculation (so its argument is preserved) rather
/// than as the deprecated Sass global.
fn expr_contains_calc_substitution(e: &Expr) -> bool {
    if expr_has_substitution(e) {
        return true;
    }
    match e {
        Expr::Binary { lhs, rhs, .. } => {
            expr_contains_calc_substitution(lhs) || expr_contains_calc_substitution(rhs)
        }
        Expr::Div { lhs, rhs, .. } => {
            expr_contains_calc_substitution(lhs) || expr_contains_calc_substitution(rhs)
        }
        Expr::Unary { operand, .. } => expr_contains_calc_substitution(operand),
        Expr::Paren(inner) => expr_contains_calc_substitution(inner),
        Expr::Calc { inner } => expr_contains_calc_substitution(inner),
        Expr::List { items, .. } => items.iter().any(expr_contains_calc_substitution),
        _ => false,
    }
}

/// The inspect-style spelling of a value rejected from a calculation, for
/// dart-sass's "Value … can't be used in a calculation." error. `null`
/// spells out as `null`, a list is parenthesized (`(1 2 3)`, `(1, 2)`); every
/// other type matches its plain CSS form (`true`, `blue`, `"foo"`, `(b: c)`).
fn calc_value_repr(v: &Value) -> String {
    match v {
        Value::Null => "null".to_string(),
        Value::List(_) => format!("({})", v.to_css(false)),
        other => other.to_css(false),
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
/// Whether a single-string operand produced by a *nested* `calc()` must be
/// wrapped in parentheses when spliced into the surrounding calculation.
///
/// dart-sass keeps a nested calc's unresolved interpolation/`var()` operand
/// grouped (`calc(calc(#{"c*"}))` -> `calc((c*))`, `calc(1 + calc(var(--c)))`
/// -> `calc(1 + (var(--c)))`), but a clean single token flattens bare
/// (`calc(calc(c))` -> `calc(c)`, `calc(calc(c-d))` -> `calc(c-d)`). The
/// operand needs grouping when it is not already a complete sub-calculation
/// (`min(…)`, `clamp(…)`, …) and either contains a character that would be
/// ambiguous unparenthesized — whitespace, `*`, `/`, or `\` — or is a `var()`
/// substitution (which dart-sass always treats as an opaque group).
fn nested_calc_needs_parens(s: &str) -> bool {
    if is_complete_calculation(s) {
        return false;
    }
    let trimmed = s.trim_start();
    let is_var = trimmed.len() >= 4 && trimmed[..4].eq_ignore_ascii_case("var(");
    is_var
        || s.chars()
            .any(|c| c.is_whitespace() || matches!(c, '*' | '/' | '\\'))
}

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
    // An addition/subtraction operand that is a purely-numeric multiplication of
    // two unit operands (a compound unit like `1px * 1px`) or a division by a
    // unit operand (an inverse unit like `1 / 1px`) is a number with complex
    // units, which CSS calculations cannot mix into a sum. dart-sass rejects it
    // ("Number calc(1px * 1px) isn't compatible with CSS calculations."). A
    // standalone `calc(1px * 1px)` is fine — only the `+`/`-` context checks.
    if matches!(op, CalcOp::Add | CalcOp::Sub) {
        for operand in [&left, &right] {
            if let Some(node) = calc_complex_unit_operand(operand) {
                return Err(Error::at(
                    format!(
                        "Number calc({}) isn't compatible with CSS calculations.",
                        node.to_calc_css(false)
                    ),
                    pos,
                ));
            }
        }
    }
    Ok(CalcNode::Op {
        op,
        left: Box::new(left),
        right: Box::new(right),
    })
}

/// If `node` is a purely-numeric calc operation that produces a number with
/// complex units — a multiplication of two unit-bearing numeric operands
/// (`1px * 1px`), or a division whose denominator carries a unit (`1 / 1px`) —
/// return the offending node. An operation involving a `var()`/interpolation
/// (opaque) operand is not a resolved number and is left preserved, so it never
/// triggers this check.
fn calc_complex_unit_operand(node: &CalcNode) -> Option<&CalcNode> {
    match node {
        CalcNode::Op {
            op: CalcOp::Mul,
            left,
            right,
        } if calc_node_carries_unit(left) && calc_node_carries_unit(right) => Some(node),
        CalcNode::Op {
            op: CalcOp::Div,
            right,
            ..
        } if calc_node_carries_unit(right) => Some(node),
        _ => None,
    }
}

/// Whether a resolved (no opaque operand) calc node carries a real unit: a
/// unit-bearing number, or a `*`/`/` chain of such numbers. A node containing a
/// `var()`/interpolation is opaque (unknown unit) and reported as not carrying
/// a unit, so it does not count toward a compound/inverse unit.
fn calc_node_carries_unit(node: &CalcNode) -> bool {
    match node {
        CalcNode::Number(n) => !n.unit.is_empty(),
        CalcNode::Str(_) => false,
        CalcNode::Op {
            op: CalcOp::Mul | CalcOp::Div,
            left,
            right,
        } => calc_node_carries_unit(left) || calc_node_carries_unit(right),
        CalcNode::Op { .. } => false,
        // A nested calculation function has no single determinable unit here.
        CalcNode::Func { .. } => false,
    }
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
pub(crate) fn eval_div(l: Value, r: Value, slash: bool, pos: Pos) -> Result<Value, Error> {
    if let Some(e) = callable_value_error(&l, &r, pos) {
        return Err(e);
    }
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
    match (l.clone().without_slash(), r.clone().without_slash()) {
        (Value::Number(a), Value::Number(b)) => divide_numbers(&a, &b, pos),
        // dart-sass: `SassColor.dividedBy` throws "Undefined operation"; a
        // color on the *left* of `/` is the one error case here.
        (lv @ Value::Color(_), rv) => Err(undefined_op(&lv, "/", &rv, pos)),
        // Every other left/right pair (a calculation, `var()`, unquoted
        // string, list, `true`/`null`, or a number divided by a non-number)
        // forms a slash-separated unquoted string `left/right`, mirroring
        // dart-sass's default `Value.dividedBy`. This is what lets a `/` next
        // to a `calc()`/`var()` special value survive (and what carries the
        // alpha slash through `rgb(1 2 var(--x) / 0.4)`). A slash-division
        // operand keeps its chained spelling (`1/2/foo()`, not `0.5/foo()`).
        _ => Ok(Value::Str(SassStr {
            text: format!("{}/{}", slash_repr(&l), slash_repr(&r)),
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
        BinOp::Sub => binary_sub(l, r, pos),
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
        // The single-`=` Microsoft-filter operator joins both evaluated sides
        // with `=` (no surrounding whitespace) into an unquoted string,
        // matching dart-sass (`alpha(opacity=80)` -> `alpha(opacity=80)`).
        BinOp::SingleEq => Ok(Value::Str(SassStr {
            text: format!("{}={}", l.to_css(false), r.to_css(false)),
            quoted: false,
        })),
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
    // dart-sass removed color arithmetic: `color + color`/`color + number`
    // (either order) is "Undefined operation", not string concatenation.
    if color_arith_undefined(&l, &r) {
        return Err(undefined_op(&l, "+", &r, pos));
    }
    if let Some(e) = callable_value_error(&l, &r, pos) {
        return Err(e);
    }
    // A calculation can only be `+`-concatenated with a string; against any
    // other operand (number, color, bool, list, another calculation) dart-sass
    // raises "Undefined operation" rather than string-concatenating.
    let calc_with_nonstring = (matches!(&l, Value::Calc(_)) && !matches!(&r, Value::Str(_)))
        || (matches!(&r, Value::Calc(_)) && !matches!(&l, Value::Str(_)));
    if calc_with_nonstring {
        return Err(undefined_op(&l, "+", &r, pos));
    }
    // A map cannot be serialized for string concatenation, so `map + x`
    // errors like dart-sass with "(…) isn't a valid CSS value.".
    if let Some(m) = find_map(&l).or_else(|| find_map(&r)) {
        return Err(Error::at(
            format!("{} isn't a valid CSS value.", m.to_css(false)),
            pos,
        ));
    }
    // String concatenation. When the left operand is a string the result keeps
    // the left string's quotedness; for any other left operand dart-sass's
    // default `Value.plus` quotes the result iff the right operand is a quoted
    // string (`1 + "x"` -> `"1x"`, `red + "x"` -> `"redx"`).
    let quoted = match &l {
        Value::Str(s) => s.quoted,
        _ => matches!(&r, Value::Str(s) if s.quoted),
    };
    let text = format!("{}{}", concat_str(&l), concat_str(&r));
    Ok(Value::Str(SassStr { text, quoted }))
}

/// The `-` (minus) operator. Two numbers subtract numerically (coercing to a
/// common unit); for any other operand pair dart-sass falls back to its
/// default `Value.minus`, an *unquoted* string join `<left>-<right>` where each
/// side keeps its own serialization (so quoted strings keep their quotes:
/// `"q" - 1` -> `"q"-1`). A `calc()` value has no `minus` overload and errors,
/// and a map cannot serialize as a CSS value.
fn binary_sub(l: Value, r: Value, pos: Pos) -> Result<Value, Error> {
    if let (Value::Number(a), Value::Number(b)) = (&l, &r) {
        let (av, bv, unit) = coerce_pair(a, b, pos)?;
        return Ok(Value::Number(Number { value: av - bv, unit }));
    }
    // Removed color arithmetic: `color - color`/`color - number` (either
    // order) is "Undefined operation", not a string join.
    if color_arith_undefined(&l, &r) {
        return Err(undefined_op(&l, "-", &r, pos));
    }
    if let Some(e) = callable_value_error(&l, &r, pos) {
        return Err(e);
    }
    if matches!(&l, Value::Calc(_)) || matches!(&r, Value::Calc(_)) {
        return Err(undefined_op(&l, "-", &r, pos));
    }
    if let Some(m) = find_map(&l).or_else(|| find_map(&r)) {
        return Err(Error::at(
            format!("{} isn't a valid CSS value.", m.to_css(false)),
            pos,
        ));
    }
    let text = format!("{}-{}", l.to_css(false), r.to_css(false));
    Ok(Value::Str(SassStr { text, quoted: false }))
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

/// The declaration scope a statement is nested in, for validating that
/// `@function`/`@mixin` declarations appear only where dart-sass allows them.
#[derive(Clone, Copy, PartialEq)]
enum DeclScope {
    /// Top level, a style rule, or a plain at-rule (`@media`): declarations OK.
    Allowed,
    /// Inside `@if`/`@each`/`@for`/`@while` (propagates through style rules).
    Control,
    /// Inside a `@function` body.
    Function,
    /// Inside a `@mixin` body.
    Mixin,
}

/// Reject `@function`/`@mixin` declarations nested where dart-sass forbids
/// them: inside control directives, function bodies, or mixin bodies. Runs once
/// after parsing, so an unexecuted `@while (false) { @function … }` still
/// errors (it is a compile-time, not run-time, restriction).
pub(crate) fn validate_declarations(sheet: &Stylesheet) -> Result<(), Error> {
    validate_decl_scope(&sheet.stmts, DeclScope::Allowed)
}

fn validate_decl_scope(stmts: &[Stmt], scope: DeclScope) -> Result<(), Error> {
    for stmt in stmts {
        match stmt {
            Stmt::FunctionDef(c) => {
                if let Some(msg) = decl_error(scope, "function") {
                    return Err(Error::unpositioned(msg));
                }
                validate_decl_scope(&c.body, DeclScope::Function)?;
            }
            Stmt::MixinDef(c) => {
                if let Some(msg) = decl_error(scope, "mixin") {
                    return Err(Error::unpositioned(msg));
                }
                validate_decl_scope(&c.body, DeclScope::Mixin)?;
            }
            // Control directives establish (or keep) the control/function/mixin
            // scope; a `@function`/`@mixin` body's scope sticks through them.
            Stmt::If(branches) => {
                let inner = enter_control(scope);
                for b in branches {
                    validate_decl_scope(&b.body, inner)?;
                }
            }
            Stmt::For { body, .. } | Stmt::Each { body, .. } | Stmt::While { body, .. } => {
                validate_decl_scope(body, enter_control(scope))?;
            }
            // Style rules and plain at-rules preserve the current scope.
            Stmt::Rule(r) => validate_decl_scope(&r.body, scope)?,
            Stmt::AtRule { body: Some(body), .. }
            | Stmt::Media { body, .. }
            | Stmt::Supports { body, .. }
            | Stmt::AtRoot { body, .. }
            | Stmt::Keyframes { body, .. } => validate_decl_scope(body, scope)?,
            Stmt::Include {
                content: Some(content),
                ..
            } => validate_decl_scope(content, scope)?,
            // A Sass `@import` (one that inlines a partial) is forbidden inside
            // a control directive or a function/mixin body; a plain-CSS
            // `@import` is always allowed (passed through verbatim).
            Stmt::Import(args)
                if scope != DeclScope::Allowed
                    && args.iter().any(|a| matches!(a, ImportArg::Sass { .. })) =>
            {
                return Err(Error::unpositioned(
                    "This at-rule is not allowed here.".to_string(),
                ));
            }
            _ => {}
        }
    }
    Ok(())
}

/// Entering a control directive: a `@function`/`@mixin` body keeps its own
/// scope (declarations inside still get the function/mixin message); otherwise
/// control flow establishes the `Control` scope.
fn enter_control(scope: DeclScope) -> DeclScope {
    match scope {
        DeclScope::Function | DeclScope::Mixin => scope,
        _ => DeclScope::Control,
    }
}

fn decl_error(scope: DeclScope, kind: &str) -> Option<String> {
    match scope {
        DeclScope::Allowed => None,
        DeclScope::Control => Some(format!(
            "{} may not be declared in control directives.",
            if kind == "function" { "Functions" } else { "Mixins" }
        )),
        DeclScope::Function => Some("This at-rule is not allowed here.".to_string()),
        DeclScope::Mixin => Some(format!("Mixins may not contain {kind} declarations.")),
    }
}

/// Whether a `+`/`-` operation is the removed color arithmetic that dart-sass
/// rejects with "Undefined operation": a color combined with another color or a
/// number. A color with a string (or other type) still string-concatenates via
/// the default `Value.plus`/`Value.minus`.
fn color_arith_undefined(l: &Value, r: &Value) -> bool {
    let numeric = |v: &Value| matches!(v, Value::Color(_) | Value::Number(_));
    (matches!(l, Value::Color(_)) && numeric(r)) || (matches!(r, Value::Color(_)) && numeric(l))
}

/// A first-class function or mixin reference is not a valid CSS value, so it
/// cannot appear in arithmetic or a slash: dart-sass errors "<inspect> isn't a
/// valid CSS value." for the first such operand (left before right).
fn callable_value_error(l: &Value, r: &Value, pos: Pos) -> Option<Error> {
    for v in [l, r] {
        let inspect = match v {
            Value::Function(f) => Some(f.inspect()),
            Value::Mixin(m) => Some(m.inspect()),
            _ => None,
        };
        if let Some(s) = inspect {
            return Some(Error::at(format!("{s} isn't a valid CSS value."), pos));
        }
    }
    None
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
/// Move every top-level plain-CSS `@import` (a `Raw` node) to the front of the
/// document, preserving their relative order — dart-sass requires CSS `@import`
/// rules to precede all style rules. Imports nested in `@media`/rules are not
/// `Raw` top-level nodes and are unaffected. A no-op when there is at most one
/// import or no rules precede any import.
fn hoist_css_imports(out: &mut Vec<OutNode>) {
    let is_import = |n: &OutNode| matches!(n, OutNode::Raw(s) if s.starts_with("@import"));
    // Hoisting only kicks in when a CSS `@import` follows a *style-producing*
    // node (a rule/at-rule/declaration). Imports interleaved only with comments
    // and blanks keep their source order (dart-sass preserves comment context).
    let produces_css = |n: &OutNode| !matches!(n, OutNode::Blank | OutNode::Comment(_)) && !is_import(n);
    let mut seen_css = false;
    let mut needs_hoist = false;
    for n in out.iter() {
        if is_import(n) {
            if seen_css {
                needs_hoist = true;
                break;
            }
        } else if produces_css(n) {
            seen_css = true;
        }
    }
    if !needs_hoist {
        return;
    }
    let original = std::mem::take(out);
    let mut imports = Vec::new();
    let mut rest = Vec::new();
    for node in original {
        match node {
            n if is_import(&n) => imports.push(n),
            OutNode::Blank => {}
            other => rest.push(other),
        }
    }
    // Imports first (tight, no blank between them), then a blank, then the rest.
    out.extend(imports);
    for node in rest {
        push_group(out, vec![node]);
    }
}

fn push_group(out: &mut Vec<OutNode>, mut group: Vec<OutNode>) {
    if group.is_empty() {
        return;
    }
    // dart-sass never prefixes a blank line after an at-rule, a passed-through
    // CSS `@import` (a `Raw` at-rule), or a loud comment: the next group packs
    // tight against them. A blank line is only inserted after a style rule (or
    // top-level declaration) that already emitted CSS.
    let prev_packs_tight = matches!(
        out.last(),
        Some(OutNode::AtRule { .. } | OutNode::Raw(_) | OutNode::Comment(_))
    );
    if !out.is_empty() && !prev_packs_tight {
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

/// Whether `name` is a fixed-arity math calculation (`sin`, `cos`, `sqrt`,
/// `pow`, `log`, `hypot`, …) that dart-sass parses as a calculation rather than
/// an ordinary SassScript function. Matched case-insensitively. The legacy
/// global functions `abs`/`round`/`min`/`max`/`ceil`/`floor` are deliberately
/// excluded: they fall back to the Sass math builtin (with a deprecation
/// warning) instead of rejecting non-calculation operands, and `clamp`/`min`/
/// `max` keep their dedicated builtin preservation.
fn is_pure_calc_math_function(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    matches!(
        lower.as_str(),
        "sin"
            | "cos"
            | "tan"
            | "asin"
            | "acos"
            | "atan"
            | "atan2"
            | "exp"
            | "log"
            | "pow"
            | "hypot"
            | "sqrt"
            | "sign"
            | "mod"
            | "rem"
    )
}

/// Whether a calc node carries an opaque operand — a `var()`, interpolation, or
/// unknown identifier preserved verbatim — anywhere in its tree. Such a node
/// cannot reduce to a single number, so a math calculation containing it stays
/// preserved (`sin(2px + var(--c))`).
fn calc_node_has_opaque(node: &CalcNode) -> bool {
    match node {
        CalcNode::Number(_) => false,
        CalcNode::Str(_) => true,
        CalcNode::Op { left, right, .. } => calc_node_has_opaque(left) || calc_node_has_opaque(right),
        // A nested calculation function is already preserved, so a calc holding
        // it cannot reduce to a single number.
        CalcNode::Func { .. } => true,
    }
}

/// Serialize an unquoted string as dart-sass `_visitUnquotedString` does: each
/// newline becomes a single space, and any whitespace immediately following a
/// newline is dropped. Used for `@supports` custom-property values.
fn unquoted_string_css(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut after_newline = false;
    for ch in s.chars() {
        match ch {
            '\n' => {
                out.push(' ');
                after_newline = true;
            }
            ' ' => {
                if !after_newline {
                    out.push(' ');
                }
            }
            other => {
                after_newline = false;
                out.push(other);
            }
        }
    }
    out
}

/// Whether `name` is a CSS math function that dart-sass parses as a calculation
/// (and so keeps unsimplified inside a `@supports` declaration). Matched
/// case-insensitively, mirroring dart-sass's calculation-function set.
fn is_supports_calc_function(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    matches!(
        lower.as_str(),
        "min"
            | "max"
            | "clamp"
            | "round"
            | "mod"
            | "rem"
            | "abs"
            | "sign"
            | "sin"
            | "cos"
            | "tan"
            | "asin"
            | "acos"
            | "atan"
            | "atan2"
            | "exp"
            | "sqrt"
            | "pow"
            | "log"
            | "hypot"
    )
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

/// The diagnostic stack-frame name for an `@include`: dart-sass prints the bare
/// mixin name with empty parens (`name()`), without the `ns.` namespace.
fn mixin_frame_name(name: &str, _module: &Option<String>) -> String {
    format!("{name}()")
}

/// Whether a mixin body contains a reachable `@content`. dart-sass scans the
/// whole body tree — control flow, at-rules, nested style rules, and nested
/// `@include` content blocks all count (nested mixin/function definitions are
/// disallowed by the grammar, so there is no separate scope to exclude).
fn body_uses_content(body: &[Stmt]) -> bool {
    body.iter().any(stmt_uses_content)
}

fn stmt_uses_content(stmt: &Stmt) -> bool {
    match stmt {
        Stmt::Content(_) => true,
        Stmt::Rule(r) => body_uses_content(&r.body),
        Stmt::If(branches) => branches.iter().any(|b| body_uses_content(&b.body)),
        Stmt::For { body, .. }
        | Stmt::Each { body, .. }
        | Stmt::While { body, .. }
        | Stmt::Media { body, .. }
        | Stmt::AtRoot { body, .. }
        | Stmt::Keyframes { body, .. } => body_uses_content(body),
        Stmt::AtRule { body: Some(body), .. } => body_uses_content(body),
        Stmt::Include {
            content: Some(content),
            ..
        } => body_uses_content(content),
        _ => false,
    }
}

/// Validate a (post-interpolation) selector string against the subset of
/// dart-sass's parser rules this build can safely enforce:
///   * `&` may appear only at the beginning of a compound selector (so `b&`,
///     `[x]&`, `.y&` are all errors). A `&` followed directly by an
///     identifier-name character (`a`, `-`, `_`, digit, `\`) is a "suffix":
///     at the document root (no parent) that is an error, but inside a style
///     rule it concatenates onto the parent.
///   * A `%` placeholder must be followed directly by an identifier name-start
///     character; a bare `%` (or `%` before `.`, a digit, whitespace, …) is
///     "Expected identifier.". A `%` right after a digit is a percentage
///     keyframe selector (`10%`), not a placeholder.
///   * An `[…]` attribute selector's modifier must be a single ASCII letter
///     immediately before the closing `]`.
///
/// Quoted strings (with `\` escapes) and the contents of nested `[…]`/`(…)`
/// groups are skipped so combinators/`&`/`%` inside them are not misread.
/// Reject the selector forms plain CSS forbids in one comma-part: a placeholder
/// (`%x`), a parent reference with a suffix (`&x`), a top-level leading
/// combinator (`> a`), and a trailing combinator (`a >`). The text is the
/// already-resolved selector (no interpolation left).
fn validate_plain_css_selector(part: &str, top_level: bool) -> Result<(), Error> {
    let trimmed = part.trim();
    let chars: Vec<char> = trimmed.chars().collect();
    // A leading combinator is allowed when *nested* (it joins onto the parent),
    // but not at the top level.
    if top_level && matches!(chars.first(), Some('>' | '+' | '~')) {
        return Err(Error::unpositioned(
            "Top-level leading combinators aren't allowed in plain CSS.",
        ));
    }
    // A trailing combinator never has a selector to bind to.
    if matches!(chars.last(), Some('>' | '+' | '~')) {
        return Err(Error::unpositioned("expected selector."));
    }
    let mut i = 0;
    // True at the start of each compound (start, or after a combinator/space).
    let mut at_compound_start = true;
    let mut depth = 0i32; // inside `[...]`/`(...)`
    while i < chars.len() {
        let c = chars[i];
        match c {
            '\\' => {
                i += 2;
                at_compound_start = false;
                continue;
            }
            '[' | '(' => depth += 1,
            ']' | ')' => depth -= 1,
            _ if depth > 0 => {}
            ' ' | '\t' | '\n' | '\r' | '>' | '+' | '~' => at_compound_start = true,
            '%' if at_compound_start => {
                return Err(Error::unpositioned(
                    "Placeholder selectors aren't allowed in plain CSS.",
                ));
            }
            '&' => {
                let next = chars.get(i + 1).copied();
                if matches!(next, Some(n) if n.is_ascii_alphanumeric() || n == '-' || n == '_' || n == '\\') {
                    return Err(Error::unpositioned(
                        "Parent selectors can't have suffixes in plain CSS.",
                    ));
                }
                at_compound_start = false;
            }
            _ => at_compound_start = false,
        }
        i += 1;
    }
    Ok(())
}

fn validate_selector(sel: &str, has_parent: bool) -> Result<(), Error> {
    for part in split_commas(sel) {
        let chars: Vec<char> = part.chars().collect();
        let mut i = 0;
        // True at the start of each compound selector (start of the part and
        // immediately after any combinator or whitespace).
        let mut at_compound_start = true;
        let mut depth = 0i32; // inside `[...]` or `(...)`
        while i < chars.len() {
            let c = chars[i];
            match c {
                '\\' => {
                    // An escape consumes the following character verbatim.
                    i += 2;
                    at_compound_start = false;
                    continue;
                }
                '"' | '\'' => {
                    i = skip_string(&chars, i);
                    at_compound_start = false;
                    continue;
                }
                '[' if depth == 0 => {
                    let end = matching_bracket(&chars, i);
                    validate_attribute(&chars[i + 1..end])?;
                    i = end + 1;
                    at_compound_start = false;
                    continue;
                }
                '[' | '(' => {
                    depth += 1;
                    at_compound_start = false;
                }
                ']' | ')' => {
                    depth -= 1;
                    at_compound_start = false;
                }
                _ if depth > 0 => {}
                ' ' | '\t' | '\n' | '\r' => at_compound_start = true,
                '>' | '+' | '~' => at_compound_start = true,
                '&' => {
                    if !at_compound_start {
                        return Err(Error::unpositioned(
                            "\"&\" may only used at the beginning of a compound selector.",
                        ));
                    }
                    let next = chars.get(i + 1).copied();
                    let is_suffix = matches!(next, Some(n) if n.is_ascii_alphanumeric() || n == '-' || n == '_' || n == '\\');
                    if is_suffix && !has_parent {
                        return Err(Error::unpositioned(
                            "A top-level selector may not contain a parent selector with a suffix.",
                        ));
                    }
                    at_compound_start = false;
                }
                '%' => {
                    let prev_is_digit = i > 0 && chars[i - 1].is_ascii_digit();
                    if !prev_is_digit {
                        let next = chars.get(i + 1).copied();
                        let starts_ident = matches!(next, Some(n) if n.is_ascii_alphabetic() || n == '-' || n == '_' || n == '\\');
                        if !starts_ident {
                            return Err(Error::unpositioned("Expected identifier."));
                        }
                    }
                    at_compound_start = false;
                }
                // A `#`/`.` (id/class) must be followed by an identifier name
                // start; `#2b` / `.3c` are "Expected identifier." (a `-`, `_`,
                // letter, escape, or non-ASCII char is fine). A `.` right after a
                // digit is a decimal point (a keyframe stop like `50.5%`), not a
                // class, so it is skipped. A bare digit *type* selector (`1a`) is
                // also left alone: `50%` keyframe stops reach this same validator.
                '#' | '.' if !(c == '.' && i > 0 && chars[i - 1].is_ascii_digit()) => {
                    let next = chars.get(i + 1).copied();
                    let starts_ident = matches!(next, Some(n) if n.is_ascii_alphabetic() || n == '-' || n == '_' || n == '\\' || (n as u32) >= 0x80);
                    if !starts_ident {
                        return Err(Error::unpositioned("Expected identifier."));
                    }
                    at_compound_start = false;
                }
                // A reference combinator (`/foo/`) used to be valid CSS but is
                // no longer supported; dart-sass now rejects any top-level `/`
                // in a selector with "expected selector.". (A `/` inside an
                // attribute value or a pseudo argument is at depth > 0 and is
                // handled above.)
                '/' => {
                    return Err(Error::unpositioned("expected selector."));
                }
                _ => at_compound_start = false,
            }
            i += 1;
        }
    }
    Ok(())
}

/// Index just past a quoted string starting at `start` (a `"` or `'`),
/// honouring `\` escapes. Returns `chars.len()` for an unterminated string.
fn skip_string(chars: &[char], start: usize) -> usize {
    let quote = chars[start];
    let mut i = start + 1;
    while i < chars.len() {
        match chars[i] {
            '\\' => i += 2,
            c if c == quote => return i + 1,
            _ => i += 1,
        }
    }
    chars.len()
}

/// Index of the `]` matching the `[` at `open`, skipping quoted strings and
/// escapes. Returns `chars.len()` when unmatched.
fn matching_bracket(chars: &[char], open: usize) -> usize {
    let mut i = open + 1;
    while i < chars.len() {
        match chars[i] {
            '\\' => i += 2,
            '"' | '\'' => i = skip_string(chars, i),
            ']' => return i,
            _ => i += 1,
        }
    }
    chars.len()
}

/// Validate the inner content of an `[…]` attribute selector. dart-sass allows
/// at most a single trailing ASCII-letter modifier, directly before the close
/// bracket: `[a]`, `[a=b]`, `[a=b ]`, `[a="b"i]`, and `[a=b i]` are valid, but
/// `[a b]` (no operator), `[a=b cd]` (too long), `[a=b 1]`/`[a=b _]`/`[a=b ï]`
/// (non-letter), and `[a=b i ]` (trailing space after the modifier) are not.
fn validate_attribute(inner: &[char]) -> Result<(), Error> {
    let err = || Error::unpositioned("expected \"]\".");
    let mut i = 0;
    let skip_ws = |i: &mut usize| {
        while *i < inner.len() && inner[*i].is_whitespace() {
            *i += 1;
        }
    };
    // Namespace + attribute name (identifiers, escapes, and a `|` namespace
    // separator); interpolation has already been resolved to literal text.
    skip_ws(&mut i);
    while i < inner.len() {
        let c = inner[i];
        if c == '\\' {
            i += 2;
        } else if is_name_char(c) || c == '|' || c == '*' {
            i += 1;
        } else {
            break;
        }
    }
    skip_ws(&mut i);
    if i >= inner.len() {
        return Ok(()); // bare `[name]`
    }
    // An operator must follow the name; anything else (e.g. a second
    // identifier in `[a b]`) is invalid.
    let op_ok = match inner[i] {
        '=' => true,
        '~' | '|' | '^' | '$' | '*' => inner.get(i + 1) == Some(&'='),
        _ => false,
    };
    if !op_ok {
        return Err(err());
    }
    i += if inner[i] == '=' { 1 } else { 2 };
    skip_ws(&mut i);
    // The value: a quoted string or an unquoted identifier (with escapes).
    match inner.get(i) {
        Some('"') | Some('\'') => i = skip_string(inner, i),
        Some(_) => {
            while i < inner.len() {
                let c = inner[i];
                if c == '\\' {
                    i += 2;
                } else if c.is_whitespace() {
                    break;
                } else {
                    i += 1;
                }
            }
        }
        None => return Err(err()),
    }
    skip_ws(&mut i);
    if i >= inner.len() {
        return Ok(()); // value, no modifier
    }
    // A modifier: exactly one ASCII letter, immediately before the close.
    if inner[i].is_ascii_alphabetic() && i + 1 == inner.len() {
        return Ok(());
    }
    Err(err())
}

fn is_name_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '-' || c == '_' || !c.is_ascii()
}

fn is_name_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_' || !c.is_ascii()
}

/// Whether `s` is a plain CSS identifier that needs no escaping — so a quoted
/// attribute value `"<s>"` can be emitted unquoted as `<s>` by simply dropping
/// the quotes. Matches dart-sass's `_isIdentifier` for the escape-free case:
/// a leading `-` must be followed by a name-start char (`--x`, `-1`, `-` alone
/// are not identifiers), the first significant char is a name-start, and the
/// rest are name chars. Strings containing escapes or non-name characters are
/// conservatively treated as non-identifiers (kept quoted) so nothing
/// regresses.
fn is_plain_css_identifier(s: &str) -> bool {
    let chars: Vec<char> = s.chars().collect();
    if chars.is_empty() {
        return false;
    }
    let mut i = 0;
    if chars[0] == '-' {
        i = 1;
    }
    match chars.get(i) {
        Some(&c) if is_name_start(c) => i += 1,
        _ => return false,
    }
    chars[i..].iter().all(|&c| is_name_char(c))
}

/// Canonicalize the interior of an `[…]` attribute selector for emit, matching
/// dart-sass's `[name op value modifier]` form: whitespace around the operator
/// and at the edges is removed, and a trailing single-letter modifier is
/// preceded by exactly one space. The value text is preserved verbatim (no
/// unquoting). On any parse uncertainty the original (trimmed) text is kept so
/// no currently-passing selector regresses.
fn normalize_attribute_text(inner: &str) -> String {
    let chars: Vec<char> = inner.chars().collect();
    let fallback = || inner.trim().to_string();
    let mut i = 0;
    let skip_ws = |i: &mut usize| {
        while *i < chars.len() && chars[*i].is_whitespace() {
            *i += 1;
        }
    };
    skip_ws(&mut i);
    // Namespace + name.
    let name_start = i;
    while i < chars.len() {
        let c = chars[i];
        if c == '\\' {
            i += 2;
        } else if is_name_char(c) || c == '|' || c == '*' {
            i += 1;
        } else {
            break;
        }
    }
    if i == name_start {
        return fallback();
    }
    let name: String = chars[name_start..i.min(chars.len())].iter().collect();
    skip_ws(&mut i);
    if i >= chars.len() {
        return name; // `[name]`
    }
    // Operator.
    let op: String = match chars[i] {
        '=' => {
            i += 1;
            "=".to_string()
        }
        c @ ('~' | '|' | '^' | '$' | '*') if chars.get(i + 1) == Some(&'=') => {
            i += 2;
            format!("{c}=")
        }
        _ => return fallback(),
    };
    skip_ws(&mut i);
    // Value (quoted string or unquoted run), preserved verbatim.
    let value_start = i;
    match chars.get(i) {
        Some('"') | Some('\'') => i = skip_string(&chars, i),
        Some(_) => {
            while i < chars.len() {
                let c = chars[i];
                if c == '\\' {
                    i += 2;
                } else if c.is_whitespace() {
                    break;
                } else {
                    i += 1;
                }
            }
        }
        None => return fallback(),
    }
    let raw_value: String = chars[value_start..i.min(chars.len())].iter().collect();
    // dart-sass emits a quoted value unquoted when its content is a plain CSS
    // identifier (`[a="b"]` -> `[a=b]`). Only the escape-free, plain case is
    // unquoted here; anything needing re-escaping is kept verbatim.
    let value = unquote_plain_attribute_value(&raw_value);
    skip_ws(&mut i);
    if i >= chars.len() {
        return format!("{name}{op}{value}");
    }
    // A single-letter modifier (case-insensitive) before the close.
    if chars[i].is_ascii_alphabetic() && i + 1 == chars.len() {
        return format!("{name}{op}{value} {}", chars[i]);
    }
    fallback()
}

/// Drop the quotes from an attribute value when its content is a plain CSS
/// identifier; otherwise return it unchanged.
fn unquote_plain_attribute_value(raw: &str) -> String {
    let bytes: Vec<char> = raw.chars().collect();
    if bytes.len() >= 2 {
        let q = bytes[0];
        if (q == '"' || q == '\'') && bytes[bytes.len() - 1] == q {
            let content: String = bytes[1..bytes.len() - 1].iter().collect();
            if is_plain_css_identifier(&content) {
                return content;
            }
        }
    }
    raw.to_string()
}

/// Whether any TOP-LEVEL style rule (not nested inside an at-rule such as
/// `@media`) contains the extend `target` simple selector. Used to detect an
/// `@extend` that crosses a media-query boundary.
fn root_rule_contains_target(nodes: &[OutNode], target: &crate::selector::Simple) -> bool {
    nodes.iter().any(|node| match node {
        OutNode::Rule { selectors, .. } => selectors.iter().any(|s| {
            crate::selector::parse_list(s)
                .map(|cs| crate::selector::list_contains_simple(&cs, target))
                .unwrap_or(false)
        }),
        _ => false,
    })
}

/// Walk the flattened output tree, rewriting each style-rule selector list per
/// the collected extensions and dropping rules whose every complex selector
/// still contains a placeholder. Recurses into at-rule bodies (e.g. `@media`),
/// but NOT into `@keyframes` (whose "selectors" are keyframe stops like `50%`).
fn rewrite_nodes(nodes: &mut Vec<OutNode>, extensions: &[crate::selector::Extension]) {
    let mut i = 0;
    while i < nodes.len() {
        let drop = match &mut nodes[i] {
            OutNode::Rule {
                selectors,
                linebreaks,
                ..
            } => {
                let new_sel = extend_selector_list(selectors, extensions);
                match new_sel {
                    Some(s) => {
                        // Source line-break flags are positional; if @extend
                        // changed the complex count they no longer line up, so
                        // fall back to plain `, ` joining.
                        if s.len() != selectors.len() {
                            linebreaks.clear();
                        }
                        *selectors = s;
                        false
                    }
                    // None means the rule is entirely placeholders → drop.
                    None => true,
                }
            }
            OutNode::AtRule {
                name,
                body,
                has_block,
                ..
            } => {
                if !is_keyframes_name(name) {
                    rewrite_nodes(body, extensions);
                }
                // A conditional group rule (`@media`/`@supports`) whose body is
                // emptied by placeholder removal produces no CSS, so drop it
                // (dart-sass omits empty `@media`/`@supports`).
                *has_block && body.is_empty() && (name == "media" || name == "supports")
            }
            _ => false,
        };
        if drop {
            nodes.remove(i);
            // Removing a rule can leave a dangling Blank separator; drop a
            // leading Blank so adjacent groups don't collapse to a blank line.
            if i < nodes.len() && matches!(nodes[i], OutNode::Blank) {
                nodes.remove(i);
            } else if i > 0 && matches!(nodes[i - 1], OutNode::Blank) {
                nodes.remove(i - 1);
                continue;
            }
        } else {
            i += 1;
        }
    }
}

/// True for `@keyframes` and its vendor-prefixed spellings, whose block
/// selectors are keyframe stops, not real selectors.
fn is_keyframes_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower == "keyframes" || lower.ends_with("-keyframes")
}

/// Derive the default namespace for `@use "<url>"`: the final path component,
/// with any leading `_` removed and everything from the first `.` (i.e. all
/// extensions) discarded. dart-sass rejects a result that is not a valid Sass
/// identifier.
fn default_namespace(url: &str, pos: Pos) -> Result<String, Error> {
    let last = url.rsplit('/').next().unwrap_or(url);
    let last = last.strip_prefix('_').unwrap_or(last);
    // Strip every extension: the namespace is the basename up to its first dot.
    let stem = match last.split_once('.') {
        Some((before, _)) => before,
        None => last,
    };
    if !is_valid_namespace(stem) {
        return Err(Error::at(
            format!("The default namespace \"{stem}\" is not a valid Sass identifier."),
            pos,
        ));
    }
    Ok(stem.to_string())
}

/// Build a predicate deciding whether an upstream `$variable` (by bare name) is
/// re-exported through a `@forward`'s `show`/`hide` clause.
fn forward_var_visibility(
    show: &Option<Vec<crate::ast::ForwardMember>>,
    hide: &Option<Vec<crate::ast::ForwardMember>>,
) -> impl Fn(&str) -> bool {
    let show = member_set(show, true);
    let hide = member_set(hide, true);
    move |name: &str| -> bool {
        let n = normalize_var_name(name);
        if let Some(s) = &show {
            return s.contains(&n);
        }
        if let Some(h) = &hide {
            return !h.contains(&n);
        }
        true
    }
}

/// Canonicalize a Sass variable name: `-` and `_` are interchangeable, so the
/// canonical form replaces every `_` with `-` (dart-sass dash-insensitivity).
fn normalize_var_name(name: &str) -> String {
    name.replace('_', "-")
}

/// Whether a member name is private (dart-sass: a leading `-` or `_`), so it is
/// not accessible across module boundaries.
fn is_private_member(name: &str) -> bool {
    name.starts_with('-') || name.starts_with('_')
}

/// Whether `module` exposes `name` as a built-in mixin. dart-sass's `sass:meta`
/// module defines the `load-css` and `apply` mixins; no other built-in module
/// exposes a mixin. Matched dash/underscore-insensitively.
fn is_builtin_mixin(module: &str, name: &str) -> bool {
    if module != "meta" {
        return false;
    }
    matches!(normalize_arg_name(name).as_str(), "load-css" | "apply")
}

/// Collect the names from a `@forward` `show`/`hide` member list, selecting
/// either the `$variable` entries (`vars == true`) or the function/mixin names.
/// Names are stored in canonical (dashed) form for dash-insensitive matching.
fn member_set(
    members: &Option<Vec<crate::ast::ForwardMember>>,
    vars: bool,
) -> Option<std::collections::HashSet<String>> {
    members.as_ref().map(|list| {
        list.iter()
            .filter_map(|m| match (m, vars) {
                (crate::ast::ForwardMember::Var(n), true) => Some(normalize_var_name(n)),
                (crate::ast::ForwardMember::Name(n), false) => Some(normalize_var_name(n)),
                _ => None,
            })
            .collect()
    })
}

/// Whether `s` is a valid Sass identifier usable as a module namespace.
fn is_valid_namespace(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c == '_' || c == '-' || c.is_ascii_alphabetic() || !c.is_ascii() => {}
        _ => return false,
    }
    chars.all(|c| c == '_' || c == '-' || c.is_ascii_alphanumeric() || !c.is_ascii())
}

/// Compute the extended selector list for a rule. Returns `None` when, after
/// extension, every complex selector still contains a placeholder (the rule
/// emits no CSS). Returns `Some(unchanged)` when the selector needs no change.
fn extend_selector_list(
    selectors: &[String],
    extensions: &[crate::selector::Extension],
) -> Option<Vec<String>> {
    let has_placeholder = selectors.iter().any(|s| s.contains('%'));
    // Fast path: no extensions and no placeholder → the selector is untouched.
    // Crucially this leaves selectors we don't model (keyframe stops are handled
    // separately, but also unusual selectors) byte-for-byte intact.
    if extensions.is_empty() && !has_placeholder {
        return Some(selectors.to_vec());
    }
    let joined = selectors.join(", ");
    let Some(parsed) = crate::selector::parse_list(&joined) else {
        // Unparseable selector: never lose it; leave untouched.
        return Some(selectors.to_vec());
    };
    let result = crate::selector::extend_selectors(&parsed, extensions);
    if result.all_placeholders {
        return None;
    }
    Some(result.selectors)
}

/// For each non-empty top-level comma part of a selector list, whether the
/// emitted complex selector should begin on its own line — parallel to the
/// parts `resolve_selectors` keeps.
///
/// dart-sass carries a per-complex `lineBreak` flag set when a newline precedes
/// the part in source (`a,\nb`). During parent resolution that flag survives for
/// an *implicit*-parent part (`parent.lineBreak || child.lineBreak`), but a part
/// that *references* the parent with `&` takes the parent complex's flag instead
/// and drops its own. We don't track parent line-breaks, so for a `&`-part in a
/// nested rule we conservatively report `false` (correct whenever the governing
/// parent is the first/unbroken one, and never emits a break dart-sass wouldn't).
fn comma_linebreaks(sel: &str, nested: bool) -> Vec<bool> {
    split_commas(sel)
        .iter()
        .enumerate()
        .filter(|(_, seg)| !seg.trim().is_empty())
        .map(|(i, seg)| {
            let newline_before = i > 0 && seg.chars().take_while(|c| c.is_whitespace()).any(|c| c == '\n');
            newline_before && !(nested && part_has_parent_ref(seg))
        })
        .collect()
}

/// Whether a selector comma-part contains a top-level parent reference `&`
/// (not inside `[…]` or a quoted string). Interpolation has already been
/// resolved into `sel` by the time this runs, so a `&` here is a real parent
/// reference.
fn part_has_parent_ref(part: &str) -> bool {
    let mut bracket = 0i32;
    let mut quote: Option<char> = None;
    let mut chars = part.chars();
    while let Some(c) = chars.next() {
        match quote {
            Some(q) => {
                if c == '\\' {
                    chars.next();
                } else if c == q {
                    quote = None;
                }
            }
            None => match c {
                '"' | '\'' => quote = Some(c),
                '[' => bracket += 1,
                ']' => bracket = (bracket - 1).max(0),
                '&' if bracket == 0 => return true,
                _ => {}
            },
        }
    }
    false
}

fn resolve_selectors(sel: &str, parents: &[String]) -> Vec<String> {
    let parts: Vec<String> = split_commas(sel)
        .into_iter()
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty())
        .collect();
    let mut result = Vec::new();
    if parents.is_empty() {
        // At the document root (no enclosing style rule) a parent selector `&`
        // has no parent to substitute, so dart-sass keeps it literal: `& {a: b}`
        // emits `& {…}` and `&.foo {…}` emits `&.foo {…}`. (A `&`-with-suffix
        // such as `&foo` is rejected earlier by `validate_selector`.)
        for part in &parts {
            result.push(normalize_selector(part));
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

/// Split `s` on top-level commas (paren/bracket depth 0), returning borrowed
/// slices of `s` — no per-part allocation. Commas inside `(...)`/`[...]` stay
/// within their part. Each part is a contiguous substring of `s`, so callers
/// that need an owned `String` call `.to_string()` themselves.
fn split_commas(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut paren = 0i32;
    let mut bracket = 0i32;
    let mut start = 0usize;
    for (idx, c) in s.char_indices() {
        match c {
            '(' => paren += 1,
            ')' => paren -= 1,
            '[' => bracket += 1,
            ']' => bracket -= 1,
            ',' if paren == 0 && bracket == 0 => {
                out.push(&s[start..idx]);
                start = idx + 1; // ',' is ASCII (1 byte)
            }
            _ => {}
        }
    }
    out.push(&s[start..]);
    out
}

/// Collapse whitespace and put single spaces around `>`/`+`/`~`
/// combinators (at bracket depth 0), matching dart-sass's selector
/// serialization. Also separates adjacent compounds: a bare type/element
/// selector appearing mid-compound (`[a]b`, `:not(.x)b`) is joined to the
/// preceding simple with a descendant combinator (`[a] b`), matching
/// dart-sass's `[adjacent-compounds]` normalization.
fn normalize_selector(s: &str) -> String {
    // Collapse runs of whitespace to single spaces (and trim) without the
    // intermediate Vec<&str> that `split_whitespace().collect().join()` builds.
    let mut collapsed = String::with_capacity(s.len());
    for word in s.split_whitespace() {
        if !collapsed.is_empty() {
            collapsed.push(' ');
        }
        collapsed.push_str(word);
    }
    let chars: Vec<char> = collapsed.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    // True when the current top-level compound already holds a simple selector.
    let mut mid_compound = false;
    while i < chars.len() {
        let c = chars[i];
        match c {
            '[' => {
                let end = matching_bracket(&chars, i);
                let inner: String = chars[i + 1..end.min(chars.len())].iter().collect();
                out.push('[');
                out.push_str(&normalize_attribute_text(&inner));
                if end < chars.len() {
                    out.push(']');
                }
                i = end + 1;
                mid_compound = true;
                continue;
            }
            '.' | '#' | '%' => {
                // A class/id/placeholder sigil plus its name (one simple).
                out.push(c);
                i += 1;
                copy_name(&chars, &mut i, &mut out);
                mid_compound = true;
                continue;
            }
            ':' => {
                // A pseudo-class/element (with any `(...)` argument), copied
                // verbatim. Its interior is not subject to compound separation.
                copy_pseudo(&chars, &mut i, &mut out);
                mid_compound = true;
                continue;
            }
            '*' if chars.get(i + 1) != Some(&'|') || chars.get(i + 2) == Some(&'=') => {
                // A bare universal `*` (not a `*|...` namespace prefix). It does
                // not start a new adjacent compound on its own.
                out.push('*');
                i += 1;
                mid_compound = true;
                continue;
            }
            '>' | '~' | '+' => {
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
                mid_compound = false;
                continue;
            }
            ' ' | '\t' | '\n' | '\r' => {
                out.push(c);
                i += 1;
                mid_compound = false;
                continue;
            }
            _ if type_selector_starts_at(&chars, i) => {
                // A type/namespaced-type selector. Mid-compound, it is a
                // separate adjacent compound: join with a descendant space.
                if mid_compound && !out.ends_with(' ') {
                    out.push(' ');
                }
                copy_type_selector(&chars, &mut i, &mut out);
                mid_compound = true;
                continue;
            }
            _ => {
                // Any other character (e.g. a digit or `%` in a keyframe stop
                // like `1e2%`, which is not a real selector). It does NOT make a
                // following identifier an adjacent compound, so clear the flag.
                out.push(c);
                i += 1;
                mid_compound = false;
            }
        }
    }
    out.trim().to_string()
}

/// One token of a complex selector split at the top level (paren/bracket depth
/// 0): either a combinator or a compound selector (a borrowed slice of the
/// input, trimmed).
enum SelToken<'a> {
    Combinator,
    Compound(&'a str),
}

/// Tokenize a complex selector into combinators and compounds at the top level,
/// honouring `[...]`, `(...)`, strings, and escapes so combinators inside a
/// pseudo argument or attribute aren't split out here. Compounds borrow `s`
/// (no per-token allocation): each is a contiguous, trimmed substring.
fn tokenize_complex(s: &str) -> Vec<SelToken<'_>> {
    let mut tokens = Vec::new();
    let mut paren = 0i32;
    let mut bracket = 0i32;
    let mut start = 0usize; // byte start of the compound being accumulated
    let mut it = s.char_indices();
    while let Some((idx, c)) = it.next() {
        match c {
            '\\' => {
                // An escape consumes the following character verbatim.
                it.next();
            }
            '"' | '\'' => {
                // Skip a quoted string (honouring `\` escapes) so combinators
                // inside it aren't split out. Mirrors `skip_string`.
                while let Some((_, c2)) = it.next() {
                    match c2 {
                        '\\' => {
                            it.next();
                        }
                        q if q == c => break,
                        _ => {}
                    }
                }
            }
            '(' => paren += 1,
            ')' => paren -= 1,
            '[' => bracket += 1,
            ']' => bracket -= 1,
            '>' | '+' | '~' if paren == 0 && bracket == 0 => {
                let t = s[start..idx].trim();
                if !t.is_empty() {
                    tokens.push(SelToken::Compound(t));
                }
                tokens.push(SelToken::Combinator);
                start = idx + 1; // combinator char is ASCII (1 byte)
            }
            _ => {}
        }
    }
    let t = s[start..].trim();
    if !t.is_empty() {
        tokens.push(SelToken::Compound(t));
    }
    tokens
}

/// Whether a resolved complex selector is a "bogus combinator" that dart-sass
/// omits from the generated CSS: two combinators in a row anywhere, or — inside
/// a pseudo argument (`in_pseudo`) — a trailing combinator, or a leading
/// combinator unless `allow_leading` (true only for `:has`, a relative selector
/// list). A single leading/trailing combinator at the top level is NOT bogus
/// here (it is kept, or handled separately by the nesting rules). The check
/// recurses into selector pseudo arguments (`:is()`, `:not()`, …).
fn complex_selector_is_bogus(s: &str, in_pseudo: bool, allow_leading: bool) -> bool {
    let tokens = tokenize_complex(s);
    if tokens.is_empty() {
        return false;
    }
    // Two adjacent combinators (no compound between) is always invalid.
    let mut prev_combinator = false;
    for t in &tokens {
        match t {
            SelToken::Combinator => {
                if prev_combinator {
                    return true;
                }
                prev_combinator = true;
            }
            SelToken::Compound(_) => prev_combinator = false,
        }
    }
    if in_pseudo {
        if !allow_leading && matches!(tokens.first(), Some(SelToken::Combinator)) {
            return true;
        }
        if matches!(tokens.last(), Some(SelToken::Combinator)) {
            return true;
        }
    }
    // Recurse into selector pseudo arguments of each compound.
    for t in &tokens {
        if let SelToken::Compound(comp) = t {
            if compound_has_bogus_pseudo(comp) {
                return true;
            }
        }
    }
    false
}

/// Whether a resolved complex selector should be dropped from its own emitted
/// declaration block. This is [`complex_selector_is_bogus`] (double combinators,
/// pseudo leading/trailing combinators) PLUS a top-level trailing combinator
/// (`a >`): a trailing combinator is valid only for nesting, so the leaf block
/// it would head is omitted while the selector still serves as a parent.
fn complex_selector_block_is_bogus(s: &str) -> bool {
    if complex_selector_is_bogus(s, false, false) {
        return true;
    }
    let tokens = tokenize_complex(s);
    matches!(tokens.last(), Some(SelToken::Combinator))
}

/// Whether `name` (a pseudo-class/element name, case-insensitive, without the
/// leading colon(s)) is one whose argument dart-sass parses as a selector list,
/// and so is subject to bogus-combinator checking. Mirrors dart-sass's
/// `_selectorPseudoClasses`/`_selectorPseudoElements`. Notably this EXCLUDES
/// `:global`/`:local` (CSS-modules pseudos kept verbatim).
fn is_selector_pseudo(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "not" | "is" | "matches" | "where" | "current" | "any" | "has" | "host" | "host-context" | "slotted"
    )
}

/// Whether any selector-pseudo argument (`:is(...)`, `:has(...)`, `:not(...)`,
/// `:where(...)`, …) inside a compound contains a bogus-combinator complex
/// selector. Only pseudos in [`is_selector_pseudo`] are scanned (others, like
/// `:nth-child(2n)` or `:global(> a)`, keep their argument verbatim). `:has` is
/// a relative selector list, so a leading combinator there is allowed.
fn compound_has_bogus_pseudo(compound: &str) -> bool {
    let chars: Vec<char> = compound.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '\\' {
            i += 2;
            continue;
        }
        if c == '[' {
            // Skip an attribute selector verbatim.
            i = matching_bracket(&chars, i) + 1;
            continue;
        }
        if c == ':' {
            // A pseudo with a `(...)` argument: extract and scan the argument as
            // a selector list (each comma part is one complex selector).
            let mut j = i + 1;
            if j < chars.len() && chars[j] == ':' {
                j += 1;
            }
            let name_start = j;
            while j < chars.len() && (is_name_char(chars[j]) || chars[j] == '\\') {
                if chars[j] == '\\' {
                    j += 1;
                }
                j += 1;
            }
            let name: String = chars[name_start..j.min(chars.len())].iter().collect();
            if j < chars.len() && chars[j] == '(' {
                let open = j;
                let mut depth = 0i32;
                let mut k = open;
                while k < chars.len() {
                    match chars[k] {
                        '\\' => {
                            k += 2;
                            continue;
                        }
                        '"' | '\'' => {
                            k = skip_string(&chars, k);
                            continue;
                        }
                        '(' => depth += 1,
                        ')' => {
                            depth -= 1;
                            if depth == 0 {
                                break;
                            }
                        }
                        _ => {}
                    }
                    k += 1;
                }
                if is_selector_pseudo(&name) {
                    let allow_leading = name.eq_ignore_ascii_case("has");
                    let arg: String = chars[open + 1..k.min(chars.len())].iter().collect();
                    for part in split_commas(&arg) {
                        let part = part.trim();
                        if !part.is_empty() && complex_selector_is_bogus(part, true, allow_leading) {
                            return true;
                        }
                    }
                }
                i = k + 1;
                continue;
            }
            i = j;
            continue;
        }
        i += 1;
    }
    false
}

/// Copy a CSS name (the part after a `.`/`#`/`%` sigil or a type name) starting
/// at `*i`, honouring `\` escapes, advancing `*i` past it. The captured name is
/// canonicalized to dart-sass's escape form (a numeric escape of a printable
/// character becomes the escaped character, an inline digit drops its escape,
/// etc.).
fn copy_name(chars: &[char], i: &mut usize, out: &mut String) {
    let start = *i;
    let mut has_escape = false;
    while *i < chars.len() {
        let c = chars[*i];
        if c == '\\' {
            has_escape = true;
            *i += 1;
            if *i < chars.len() {
                let esc = chars[*i];
                *i += 1;
                // A hex escape continues for up to six hex digits plus one
                // optional trailing whitespace; consume the rest of it so it
                // decodes as a single code point.
                if esc.is_ascii_hexdigit() {
                    let mut digits = 1;
                    while digits < 6 && *i < chars.len() && chars[*i].is_ascii_hexdigit() {
                        *i += 1;
                        digits += 1;
                    }
                    if *i < chars.len() && chars[*i].is_whitespace() {
                        *i += 1;
                    }
                }
            }
        } else if is_name_char(c) {
            *i += 1;
        } else {
            break;
        }
    }
    // Fast path: a plain name (no escapes) round-trips through
    // `canonicalize_ident` unchanged, so copy the slice straight out with no
    // intermediate String allocation.
    if has_escape {
        let raw: String = chars[start..*i].iter().collect();
        out.push_str(&canonicalize_ident(&raw));
    } else {
        out.extend(chars[start..*i].iter());
    }
}

/// Decode a CSS identifier's `\` escapes to their literal characters, then
/// re-serialize it in dart-sass's canonical escape form (its `_writeIdentifier`).
/// A plain ASCII identifier with no escapes round-trips unchanged.
fn canonicalize_ident(raw: &str) -> String {
    if !raw.contains('\\') {
        return raw.to_string();
    }
    // ---- decode ----
    let chars: Vec<char> = raw.chars().collect();
    let mut decoded: Vec<char> = Vec::with_capacity(chars.len());
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '\\' {
            i += 1;
            if i >= chars.len() {
                // A trailing lone backslash decodes to U+FFFD per CSS.
                decoded.push('\u{FFFD}');
                break;
            }
            if chars[i].is_ascii_hexdigit() {
                let mut hex = String::new();
                let mut digits = 0;
                while digits < 6 && i < chars.len() && chars[i].is_ascii_hexdigit() {
                    hex.push(chars[i]);
                    i += 1;
                    digits += 1;
                }
                // One optional trailing whitespace terminates the escape.
                if i < chars.len() && chars[i].is_whitespace() {
                    i += 1;
                }
                let cp = u32::from_str_radix(&hex, 16).unwrap_or(0);
                // U+0000 and out-of-range/surrogate code points map to U+FFFD.
                let ch = if cp == 0 {
                    '\u{FFFD}'
                } else {
                    char::from_u32(cp).unwrap_or('\u{FFFD}')
                };
                decoded.push(ch);
            } else {
                decoded.push(chars[i]);
                i += 1;
            }
        } else {
            decoded.push(chars[i]);
            i += 1;
        }
    }
    // ---- re-serialize (dart-sass `_writeIdentifier`) ----
    let mut out = String::new();
    let first_is_hyphen = decoded.first() == Some(&'-');
    for (idx, &c) in decoded.iter().enumerate() {
        let cu = c as u32;
        let needs_hex = cu < 0x20
            || cu == 0x7F
            || (idx == 0 && c.is_ascii_digit())
            || (idx == 1 && c.is_ascii_digit() && first_is_hyphen);
        if needs_hex {
            out.push('\\');
            out.push_str(&format!("{cu:x}"));
            // dart-sass always terminates a numeric escape with a single space
            // so it can never be misread as continuing into the next character.
            out.push(' ');
        } else if c == '_' || c == '-' || c.is_ascii_alphanumeric() || cu >= 0x80 {
            out.push(c);
        } else {
            out.push('\\');
            out.push(c);
        }
    }
    out
}

/// Copy a pseudo-class/element selector (`:name` / `::name` plus any balanced
/// `(...)` argument) verbatim, advancing `*i` past it.
fn copy_pseudo(chars: &[char], i: &mut usize, out: &mut String) {
    out.push(chars[*i]); // first ':'
    *i += 1;
    if *i < chars.len() && chars[*i] == ':' {
        out.push(':');
        *i += 1;
    }
    copy_name(chars, i, out);
    if *i < chars.len() && chars[*i] == '(' {
        let mut depth = 0i32;
        while *i < chars.len() {
            let c = chars[*i];
            match c {
                '\\' => {
                    out.push(c);
                    *i += 1;
                    if *i < chars.len() {
                        out.push(chars[*i]);
                        *i += 1;
                    }
                    continue;
                }
                '(' => depth += 1,
                ')' => depth -= 1,
                _ => {}
            }
            out.push(c);
            *i += 1;
            if depth == 0 {
                break;
            }
        }
    }
}

/// Copy a type/element selector, including an optional `ns|`/`*|`/`|` namespace
/// prefix and the type name (or `*`), advancing `*i` past it.
fn copy_type_selector(chars: &[char], i: &mut usize, out: &mut String) {
    // Optional namespace prefix.
    if chars[*i] == '*' {
        out.push('*');
        *i += 1;
    } else if chars[*i] == '|' {
        // bare `|type` — no namespace, handled by falling through.
    } else {
        copy_name(chars, i, out);
    }
    if *i < chars.len() && chars[*i] == '|' && chars.get(*i + 1) != Some(&'=') {
        out.push('|');
        *i += 1;
        if *i < chars.len() && chars[*i] == '*' {
            out.push('*');
            *i += 1;
        } else {
            copy_name(chars, i, out);
        }
    }
}

/// Whether a bare type/namespaced-type selector (an identifier, optionally
/// preceded by a `*|`/`ns|`/`|` namespace) begins at `chars[i]`. Used to detect
/// an adjacent compound (`[a]b` → `[a] b`). A `*` universal, the `.#%:` sigils,
/// an attribute `[`, and a combinator are NOT type-selector starts. `*|type`
/// and `|type` count as type starts.
fn type_selector_starts_at(chars: &[char], i: usize) -> bool {
    let c = chars[i];
    // `*|...` is a namespaced type/universal (but not `*|=`, an operator).
    if c == '*' {
        return chars.get(i + 1) == Some(&'|') && chars.get(i + 2) != Some(&'=');
    }
    // `|type` (empty namespace) is a type selector (but not `|=`).
    if c == '|' {
        return chars.get(i + 1) != Some(&'=');
    }
    if is_name_start(c) {
        return true;
    }
    // A leading `-` of an identifier (`-foo`, `--foo`) or an escape `\` begins
    // an identifier-led type selector.
    if c == '-' {
        return matches!(chars.get(i + 1), Some(&n) if is_name_start(n) || n == '-' || n == '\\');
    }
    c == '\\'
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
