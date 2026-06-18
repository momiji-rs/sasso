//! The Sass value model and its CSS serialization.
//!
//! Numbers carry a unit, colors keep full `f64` channel precision (so
//! computed colors serialize exactly like current dart-sass, e.g.
//! `rgb(63.75, 127.5, 191.25)`), and colors remember their authored
//! spelling so untransformed literals round-trip unchanged.

use std::rc::Rc;

/// A fully-evaluated Sass value.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Value {
    /// A number with an optional unit.
    Number(Number),
    /// An sRGB color with an alpha channel.
    Color(Color),
    /// A quoted or unquoted string.
    Str(SassStr),
    /// A space- or comma-separated list.
    List(List),
    /// A map of key/value pairs, preserving insertion order. Keys are
    /// compared by Sass `==`. An empty map serializes (and inspects) as `()`.
    Map(Map),
    /// A boolean.
    Bool(bool),
    /// The `null` value.
    Null,
    /// A number produced by the deprecated `a / b` slash division of two
    /// numeric literals. It behaves as `number` numerically (so arithmetic
    /// and functions use `number`), but serializes as the original
    /// `left/right` slash text. The slash is dropped (collapsing to
    /// `number`) when the value crosses a variable, function/mixin, or
    /// arithmetic boundary — matching dart-sass.
    Slash(Number, String),
    /// A `calc()` calculation that could not be reduced to a single number
    /// (e.g. it contains `var()`, an interpolation, or incompatible units).
    /// Stored as its simplified operand tree for canonical serialization.
    Calc(CalcNode),
    /// A first-class function reference (`meta.get-function(...)`), invoked via
    /// `meta.call`. Not a valid CSS value; `inspect` renders it as
    /// `get-function("name")`.
    Function(SassFunction),
    /// A first-class mixin reference (`meta.get-mixin(...)`), invoked via
    /// `@include meta.apply(...)`. Not a valid CSS value; `inspect` renders it
    /// as `get-mixin("name")`.
    // Boxed: a first-class mixin carries an optional captured file context, so
    // the inline struct is larger than Value's 64-byte budget. Mixins are never
    // emitted as CSS, so the indirection is off every hot path.
    Mixin(Box<SassMixin>),
}

/// `Value` is moved/cloned constantly (every scope slot, every `Vec<Value>`
/// element, every `$var` read), so its size is a hot-path constant. `Color` was
/// the sole 128-byte variant (its 72-byte `ModernColor` is now boxed); keep
/// `Value` at one cache line. Bump this only with a deliberate measurement.
const _: () = assert!(std::mem::size_of::<Value>() <= 64);

/// A first-class function reference. Built-in references compare equal by name
/// (and CSS flag); user references compare by identity of the captured
/// definition (so a redefined `@function` yields a distinct reference). The
/// captured user definition is held as a type-erased `Rc` (the concrete
/// `Callable` lives in `ast`, which cannot be referenced from this module
/// without a dependency cycle); the evaluator downcasts it when invoking.
#[derive(Clone)]
pub(crate) struct SassFunction {
    /// The function's name (for `inspect` and error messages).
    pub name: String,
    /// Whether this is a plain-CSS reference (`$css: true`), which is preserved
    /// verbatim instead of dispatched to a Sass builtin.
    pub css: bool,
    /// The captured user `@function` definition, or `None` for a built-in /
    /// plain-CSS reference. Type-erased to break the `ast` ↔ `value` cycle.
    pub user: Option<std::rc::Rc<dyn std::any::Any>>,
}

impl std::fmt::Debug for SassFunction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SassFunction")
            .field("name", &self.name)
            .field("css", &self.css)
            .field("user", &self.user.is_some())
            .finish()
    }
}

impl PartialEq for SassFunction {
    fn eq(&self, other: &Self) -> bool {
        match (&self.user, &other.user) {
            // User references: identity of the captured definition.
            (Some(a), Some(b)) => std::rc::Rc::ptr_eq(a, b),
            // Built-in / plain-CSS references: same name and CSS flag.
            (None, None) => self.name == other.name && self.css == other.css,
            _ => false,
        }
    }
}

impl SassFunction {
    /// The `inspect()` / debug form, `get-function("name")` (dart-sass).
    pub(crate) fn inspect(&self) -> String {
        format!("get-function(\"{}\")", self.name)
    }
}

/// A first-class mixin reference, analogous to [`SassFunction`]. Built-in
/// references (e.g. `meta.load-css`) compare equal by name; user references
/// compare by identity of the captured `@mixin` definition (so a redefined
/// `@mixin` yields a distinct reference). The captured user definition is held
/// as a type-erased `Rc` (the concrete `Callable` lives in `ast`, which cannot
/// be referenced from this module without a dependency cycle); the evaluator
/// downcasts it when invoking.
#[derive(Clone)]
pub(crate) struct SassMixin {
    /// The mixin's name (for `inspect` and error messages).
    pub name: String,
    /// The captured user `@mixin` definition, or `None` for a built-in
    /// reference. Type-erased to break the `ast` ↔ `value` cycle.
    pub user: Option<std::rc::Rc<dyn std::any::Any>>,
    /// The module the captured mixin came from, when it was resolved from
    /// another `@use`d module (so its body runs in that module's environment).
    /// `None` for a same-module reference. Type-erased to break the cycle.
    pub module: Option<std::rc::Rc<dyn std::any::Any>>,
    /// The file context the mixin was captured in, used only for a same-module
    /// (`module: None`) first-class capture: when such a reference is applied
    /// via `meta.apply` from another file, a relative `meta.load-css` in its
    /// body must still resolve against the file that defined the mixin, not the
    /// caller. (The cross-module case carries its `module` and resolves via the
    /// module's own file context instead.) Held as plain strings to keep
    /// `value` free of `eval`/`ast` types.
    pub origin: Option<MixinOrigin>,
}

/// The defining-file context captured with a same-module first-class mixin —
/// the fields [`crate::eval::Evaluator::enter_module_file`] would otherwise pull
/// from a `Module`, so a `meta.apply`'d body resolves relative loads correctly.
#[derive(Clone)]
pub(crate) struct MixinOrigin {
    /// The diagnostic display URL of the defining file (e.g. `_mod.scss`).
    pub diag_url: String,
    /// The defining file's directory (the legacy `@import` resolution base);
    /// empty when there is none.
    pub file_dir: String,
    /// The defining file's canonical URL (the importer resolution base).
    pub canonical: String,
}

impl std::fmt::Debug for SassMixin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SassMixin")
            .field("name", &self.name)
            .field("user", &self.user.is_some())
            .finish()
    }
}

impl PartialEq for SassMixin {
    fn eq(&self, other: &Self) -> bool {
        match (&self.user, &other.user) {
            // User references: identity of the captured definition.
            (Some(a), Some(b)) => std::rc::Rc::ptr_eq(a, b),
            // Built-in references: same name.
            (None, None) => self.name == other.name,
            _ => false,
        }
    }
}

impl SassMixin {
    /// The `inspect()` / debug form, `get-mixin("name")` (dart-sass).
    pub(crate) fn inspect(&self) -> String {
        format!("get-mixin(\"{}\")", self.name)
    }
}

/// A node in a simplified `calc()` tree. Numeric subtrees are folded during
/// evaluation; everything else (variables, interpolations, percentages with
/// incompatible neighbours) is preserved for canonical serialization.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum CalcNode {
    /// A resolved number operand.
    Number(Number),
    /// An opaque operand: `var(--x)`, an interpolation result, a nested
    /// unknown function — anything kept verbatim.
    Str(String),
    /// A binary operation `left <op> right`.
    Op {
        op: CalcOp,
        left: Box<CalcNode>,
        right: Box<CalcNode>,
    },
    /// A calculation function call that could not be reduced to a number
    /// (`min`/`max`/`clamp`/`hypot`/…). The `Value::Calc` holding this serializes
    /// as `name(arg, …)` (no `calc()` wrapper).
    Func { name: String, args: Vec<CalcNode> },
}

/// A `calc()` binary operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CalcOp {
    Add,
    Sub,
    Mul,
    Div,
}

impl CalcOp {
    fn symbol(self) -> &'static str {
        match self {
            CalcOp::Add => "+",
            CalcOp::Sub => "-",
            CalcOp::Mul => "*",
            CalcOp::Div => "/",
        }
    }

    /// Precedence: `*`/`/` bind tighter than `+`/`-`.
    fn precedence(self) -> u8 {
        match self {
            CalcOp::Add | CalcOp::Sub => 1,
            CalcOp::Mul | CalcOp::Div => 2,
        }
    }
}

impl CalcNode {
    /// Serialize this node's interior (without the enclosing `calc(`...`)`),
    /// adding parentheses only where operator precedence/associativity
    /// requires them, matching dart-sass's canonical form.
    pub(crate) fn to_calc_css(&self, compressed: bool) -> String {
        match self {
            CalcNode::Number(n) => calc_number_css(n, compressed),
            CalcNode::Str(s) => s.clone(),
            CalcNode::Op { op, left, right } => {
                let l = self.fmt_operand(left, *op, false, compressed);
                let r = self.fmt_operand(right, *op, true, compressed);
                let sep = match (op, compressed) {
                    (CalcOp::Mul | CalcOp::Div, true) => op.symbol().to_string(),
                    _ => format!(" {} ", op.symbol()),
                };
                // A `+ -n` / `- -n` numeric right operand flips the operator
                // (only for a finite negative; non-finite values keep their
                // canonical `infinity`/`NaN` spelling).
                if matches!(op, CalcOp::Add | CalcOp::Sub) && !compressed {
                    if let CalcNode::Number(n) = right.as_ref() {
                        if n.value.is_finite() && n.value.is_sign_negative() && n.value != 0.0 {
                            let flipped = if *op == CalcOp::Add { "-" } else { "+" };
                            let pos = n.copy_units(-n.value);
                            return format!("{l} {flipped} {}", pos.to_css(compressed));
                        }
                    }
                }
                format!("{l}{sep}{r}")
            }
            CalcNode::Func { name, args } => {
                let parts: Vec<String> = args.iter().map(|a| a.to_calc_css(compressed)).collect();
                let sep = if compressed { "," } else { ", " };
                format!("{name}({})", parts.join(sep))
            }
        }
    }

    /// Serialize as the top-level `Value::Calc` holding this node would: a
    /// calculation-function node renders bare as `name(args)`, while any other
    /// node is wrapped in `calc(...)`.
    pub(crate) fn to_calc_value_css(&self, compressed: bool) -> String {
        match self {
            CalcNode::Func { .. } => self.to_calc_css(compressed),
            _ => format!("calc({})", self.to_calc_css(compressed)),
        }
    }

    /// Format `operand` as a child of a parent `op`, wrapping in parens when
    /// the child binds more loosely (or equally on the right of `-`/`/`).
    fn fmt_operand(&self, operand: &CalcNode, parent: CalcOp, is_right: bool, compressed: bool) -> String {
        // A unit-carrying non-finite number renders as a `*` operation
        // (`infinity * 1px`), so it parenthesizes like a `Mul`-precedence
        // child rather than a bare leaf number.
        let child_op = match operand {
            CalcNode::Op { op, .. } => Some(*op),
            CalcNode::Number(n) if !n.value.is_finite() && !n.is_unitless() => Some(CalcOp::Mul),
            _ => None,
        };
        if let Some(child_op) = child_op {
            let needs_paren = child_op.precedence() < parent.precedence()
                || (child_op.precedence() == parent.precedence()
                    && is_right
                    && matches!(parent, CalcOp::Sub | CalcOp::Div));
            if needs_paren {
                return format!("({})", operand.to_calc_css(compressed));
            }
        }
        operand.to_calc_css(compressed)
    }
}

/// Render a number inside a `calc()` interior. Finite single-unit numbers use
/// their ordinary CSS form. Non-finite numbers use dart-sass's canonical
/// lowercase constants — `infinity` / `-infinity` / `NaN`, with units spelled
/// out as operands (`infinity * 1px`). Multi-unit numbers spell every unit as
/// an operand: a finite value attaches to the first numerator
/// (`1000px * 1rad / 1Hz`), a numerator-less one stays bare (`1 / 1px`).
fn calc_number_css(n: &Number, compressed: bool) -> String {
    if n.value.is_finite() && !n.has_complex_units() {
        return n.to_css(compressed);
    }
    let star = if compressed { "*" } else { " * " };
    let slash = if compressed { "/" } else { " / " };
    let mut out;
    let mut numer = n.numer_units().iter();
    if n.value.is_finite() {
        // A finite value rides on the first numerator unit when there is one.
        out = match numer.next() {
            Some(u) => format!("{}{u}", fmt_num(n.value, compressed)),
            None => fmt_num(n.value, compressed),
        };
    } else {
        out = if n.value.is_nan() {
            "NaN".to_string()
        } else if n.value > 0.0 {
            "infinity".to_string()
        } else {
            "-infinity".to_string()
        };
    }
    for u in numer {
        out.push_str(star);
        out.push('1');
        out.push_str(u);
    }
    for u in n.denom_units() {
        out.push_str(slash);
        out.push('1');
        out.push_str(u);
    }
    out
}

/// A number with its units (dart-sass `SassNumber`: a list of numerator units
/// and a list of denominator units). The representation keeps the two
/// overwhelmingly common cases — unitless and a single numerator unit —
/// allocation-identical to a plain `String` field; multi-unit numbers
/// (`px*px`, `px/s`) box their unit lists.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Number {
    pub value: f64,
    units: Units,
}

#[derive(Debug, Clone, PartialEq)]
enum Units {
    None,
    Single(String),
    Complex(Box<ComplexUnits>),
}

/// The unit lists of a multi-unit number. Invariant (kept by
/// [`Number::with_units`]): not representable as `None`/`Single` — there is a
/// denominator unit, or more than one numerator unit.
#[derive(Debug, Clone, PartialEq)]
struct ComplexUnits {
    numer: Vec<String>,
    denom: Vec<String>,
}

impl Number {
    pub(crate) fn unitless(value: f64) -> Number {
        Number {
            value,
            units: Units::None,
        }
    }

    /// A number with zero or one numerator unit (`""` means unitless).
    pub(crate) fn with_unit(value: f64, unit: impl Into<String>) -> Number {
        let unit = unit.into();
        Number {
            value,
            units: if unit.is_empty() {
                Units::None
            } else {
                Units::Single(unit)
            },
        }
    }

    /// A number with full numerator/denominator unit lists, normalized to the
    /// compact representation when possible. No unit cancellation happens
    /// here — callers cancel before constructing (dart-sass keeps whatever
    /// lists arithmetic produces).
    pub(crate) fn with_units(value: f64, mut numer: Vec<String>, denom: Vec<String>) -> Number {
        let units = if denom.is_empty() && numer.len() <= 1 {
            match numer.pop() {
                None => Units::None,
                Some(u) => Units::Single(u),
            }
        } else {
            Units::Complex(Box::new(ComplexUnits { numer, denom }))
        };
        Number { value, units }
    }

    /// A new number carrying this number's units (the common "derive a result
    /// from an operand" pattern; preserves multi-unit lists).
    pub(crate) fn copy_units(&self, value: f64) -> Number {
        Number {
            value,
            units: self.units.clone(),
        }
    }

    pub(crate) fn is_unitless(&self) -> bool {
        matches!(self.units, Units::None)
    }

    /// The single unit for the common path: `""` when unitless, the unit of a
    /// single-numerator number, or the FIRST numerator unit of a multi-unit
    /// number (callers that can meet multi-unit numbers must check
    /// [`Number::has_complex_units`] first).
    pub(crate) fn unit(&self) -> &str {
        match &self.units {
            Units::None => "",
            Units::Single(u) => u,
            Units::Complex(c) => c.numer.first().map(String::as_str).unwrap_or(""),
        }
    }

    pub(crate) fn has_complex_units(&self) -> bool {
        matches!(self.units, Units::Complex(_))
    }

    pub(crate) fn numer_units(&self) -> &[String] {
        match &self.units {
            Units::None => &[],
            Units::Single(u) => std::slice::from_ref(u),
            Units::Complex(c) => &c.numer,
        }
    }

    pub(crate) fn denom_units(&self) -> &[String] {
        match &self.units {
            Units::None | Units::Single(_) => &[],
            Units::Complex(c) => &c.denom,
        }
    }
}

/// A string value; `quoted` controls whether it serializes with quotes.
///
/// The payload is held as `Rc<str>` so cloning a `Value::Str` (e.g. on every
/// read-only `$var` lookup) is an O(1) refcount bump rather than a heap copy of
/// the text. Strings are immutable Sass values, so the sharing is invisible:
/// any "mutating" string builtin builds a fresh `Rc<str>`.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct SassStr {
    pub text: Rc<str>,
    pub quoted: bool,
}

/// A private-use code point per dart-sass `isPrivateUseCharacter`: the BMP
/// private-use area plus the whole of planes 15 and 16 (so `\e000`, `\f0000`,
/// `\10fffd` escape, while a non-character like `\fdd0` or `\f900` stays raw).
pub(crate) fn is_private_use(cp: u32) -> bool {
    (0xE000..=0xF8FF).contains(&cp) || (0xF0000..=0x10FFFF).contains(&cp)
}

/// Serialize a string as a quoted CSS string, matching dart-sass's
/// `_visitQuotedString`. The text holds decoded code points; this re-escapes
/// only what it must: the chosen quote character, backslashes, control
/// characters (everything `0x00..=0x1F` except tab, plus `0x7F`), and
/// private-use characters. Double quotes are preferred; a string that contains
/// `"` but no `'` is wrapped in `'`.
pub(crate) fn serialize_quoted(text: &str) -> String {
    let has_double = text.contains('"');
    let has_single = text.contains('\'');
    // Use single quotes only when the text has a `"` and no `'`.
    let quote = if has_double && !has_single { '\'' } else { '"' };
    // Fast path: when no character needs escaping, the body is `text` verbatim
    // between the quotes — skip the `Vec<char>` and the per-char loop. The
    // predicate mirrors the loop's two escape branches exactly.
    let needs_escape = text.chars().any(|c| {
        let cp = c as u32;
        c == quote || c == '\\' || (cp <= 0x1F && c != '\t') || cp == 0x7F || is_private_use(cp)
    });
    if !needs_escape {
        let mut out = String::with_capacity(text.len() + 2);
        out.push(quote);
        out.push_str(text);
        out.push(quote);
        return out;
    }
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len() + 2);
    out.push(quote);
    for (i, &c) in chars.iter().enumerate() {
        let cp = c as u32;
        if c == quote || c == '\\' {
            out.push('\\');
            out.push(c);
        } else if (cp <= 0x1F && c != '\t') || cp == 0x7F || is_private_use(cp) {
            // A control character, DEL, or a private-use character: `\<hex>`
            // with a trailing space only when the next character would otherwise
            // extend the escape (a hex digit, space, or tab).
            out.push('\\');
            out.push_str(&format!("{cp:x}"));
            let needs_space = chars
                .get(i + 1)
                .is_some_and(|n| n.is_ascii_hexdigit() || *n == ' ' || *n == '\t');
            if needs_space {
                out.push(' ');
            }
        } else {
            out.push(c);
        }
    }
    out.push(quote);
    out
}

/// Serialize an unquoted string for CSS / interpolation output (dart-sass
/// `_visitUnquotedString`): a newline becomes a space, and a space directly
/// after a newline is dropped so `"\a "`-style line breaks collapse to a single
/// space; a private-use character is escaped as `\<hex>` (control characters,
/// unlike in a quoted string, are written verbatim here); every other code
/// point is written verbatim.
pub(crate) fn serialize_unquoted(text: &str) -> String {
    // Fast path: nothing to rewrite unless a newline or private-use char is
    // present. Scan without allocating; only collect into a `Vec<char>` (needed
    // for the escape look-ahead) on the rare slow path.
    if !text.chars().any(|c| c == '\n' || is_private_use(c as u32)) {
        return text.to_string();
    }
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut after_newline = false;
    for (i, &c) in chars.iter().enumerate() {
        let cp = c as u32;
        if is_private_use(cp) {
            out.push('\\');
            out.push_str(&format!("{cp:x}"));
            let needs_space = chars
                .get(i + 1)
                .is_some_and(|n| n.is_ascii_hexdigit() || *n == ' ' || *n == '\t');
            if needs_space {
                out.push(' ');
            }
            after_newline = false;
            continue;
        }
        match c {
            '\n' => {
                out.push(' ');
                after_newline = true;
            }
            ' ' => {
                if !after_newline {
                    out.push(' ');
                }
            }
            _ => {
                out.push(c);
                after_newline = false;
            }
        }
    }
    out
}

/// A list value.
///
/// `items` is held as `Rc<[Value]>` so cloning a `Value::List` (e.g. on every
/// read-only `$var` lookup, or passing a list through a function) is an O(1)
/// refcount bump rather than a deep element copy. Lists are immutable Sass
/// values; every "mutating" list builtin (`append`, `join`, `set-nth`, …)
/// builds a fresh `Vec<Value>` and re-wraps it in a new `Rc`, so the sharing is
/// invisible.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct List {
    pub items: Rc<[Value]>,
    pub sep: ListSep,
    /// Whether the list was written with square brackets (`[a b]`); such
    /// lists serialize wrapped in `[`...`]` and report `true` from
    /// `list.is-bracketed`.
    pub bracketed: bool,
    /// `Some` when this list is an *argument list* (`$args...`): it carries the
    /// caller's leftover keyword arguments (name -> value), which `meta.keywords`
    /// reads and a `$args...` splat forwards as named arguments. `None` for an
    /// ordinary list. The positional arguments are the list's `items`.
    pub keywords: Option<Vec<(Value, Value)>>,
}

impl List {
    /// An ordinary (non-argument) list.
    pub(crate) fn new(items: impl Into<Rc<[Value]>>, sep: ListSep, bracketed: bool) -> Self {
        List {
            items: items.into(),
            sep,
            bracketed,
            keywords: None,
        }
    }
}

/// A map value: an ordered list of key/value entries. Insertion order is
/// preserved (dart-sass maps are ordered); duplicate keys are resolved by the
/// constructor so at most one entry exists per Sass-equal key.
///
/// `entries` is held as `Rc<Vec<…>>` so cloning a `Value::Map` (e.g. on every
/// read-only `$var` lookup) is an O(1) refcount bump rather than a deep copy of
/// every entry. Maps are immutable Sass values; `Map::insert` and the mutating
/// map builtins (`map.merge`, `map.set`, `map.remove`, …) use copy-on-write
/// (`Rc::make_mut` on a uniquely-owned `Rc`, or build a fresh `Vec`), so the
/// sharing never lets one binding observe another's mutation.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Map {
    pub entries: Rc<Vec<(Value, Value)>>,
}

impl Map {
    /// Construct a map from an owned entry vector.
    pub(crate) fn new(entries: Vec<(Value, Value)>) -> Self {
        Map {
            entries: Rc::new(entries),
        }
    }

    /// Look up a value by key (Sass `==`), or `None` when absent.
    pub(crate) fn get(&self, key: &Value) -> Option<&Value> {
        self.entries.iter().find(|(k, _)| k.sass_eq(key)).map(|(_, v)| v)
    }

    /// Insert or overwrite an entry, preserving the position of an existing
    /// key (matching dart-sass map ordering). Copy-on-write: if the entry
    /// vector is shared, `Rc::make_mut` clones it first so no other `Map`
    /// observes the change.
    pub(crate) fn insert(&mut self, key: Value, value: Value) {
        let entries = Rc::make_mut(&mut self.entries);
        if let Some(slot) = entries.iter_mut().find(|(k, _)| k.sass_eq(&key)) {
            slot.1 = value;
        } else {
            entries.push((key, value));
        }
    }

    /// Serialize the map for CSS / error messages: `(k1: v1, k2: v2)`, with
    /// the empty map rendered as `()`.
    pub(crate) fn to_css(&self, compressed: bool) -> String {
        self.to_map_css(compressed)
    }

    /// Serialize the map as dart-sass does: `(k1: v1, k2: v2)`, with the empty
    /// map rendered as `()`. Keys and values use their inspect form so nested
    /// quoted strings keep their quotes.
    fn to_map_css(&self, compressed: bool) -> String {
        if self.entries.is_empty() {
            return "()".to_string();
        }
        let sep = if compressed { "," } else { ", " };
        let pair_sep = if compressed { ":" } else { ": " };
        let inner = self
            .entries
            .iter()
            .map(|(k, v)| format!("{}{pair_sep}{}", map_key_css(k), map_val_css(v)))
            .collect::<Vec<_>>()
            .join(sep);
        format!("({inner})")
    }
}

/// Serialize a map key (dart-sass uses the inspect form for keys/values).
fn map_key_css(v: &Value) -> String {
    match v {
        Value::Str(s) if s.quoted => serialize_quoted(&s.text),
        Value::Map(m) => m.to_map_css(false),
        other => other.to_css(false),
    }
}

/// Serialize a map value entry. A bare comma list inside a map is wrapped in
/// parentheses to disambiguate from the entry separators.
fn map_val_css(v: &Value) -> String {
    match v {
        Value::Str(s) if s.quoted => serialize_quoted(&s.text),
        Value::Map(m) => m.to_map_css(false),
        Value::List(l) if l.sep == ListSep::Comma && !l.bracketed && l.items.len() >= 2 => {
            format!("({})", l.to_css(false))
        }
        other => other.to_css(false),
    }
}

/// List separator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ListSep {
    /// Space-separated (`1px 2px`).
    Space,
    /// Comma-separated (`1px, 2px`).
    Comma,
    /// Slash-separated (`1px / 2px`), produced by `list.slash()`.
    Slash,
    /// Not yet decided: an empty or single-element list that has no explicit
    /// separator. It serializes like a space list but defers to the other
    /// operand's separator in `list.join`/`list.append`, and `list.separator`
    /// reports it as `space`. Only ever held by a list with fewer than two
    /// elements (two elements force a real separator).
    Undecided,
}

/// An sRGB color. Channels are `0..=255` and may be fractional; alpha is
/// `0..=1`.
///
/// A color may also carry a *modern* color-space representation in
/// [`Color::modern`]. When present, the color belongs to a non-legacy CSS
/// Color 4 space (or a legacy space stored in canonical units, e.g. `hsl`),
/// and serialization/channel access use that representation. The legacy
/// `r`/`g`/`b`/`a` fields still hold an sRGB-byte approximation so the large
/// body of legacy color builtins keep working unchanged.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Color {
    pub r: f64,
    pub g: f64,
    pub b: f64,
    pub a: f64,
    /// The authored spelling (`"red"`, `"#336699"`). Used verbatim when
    /// the color is emitted unchanged; `None` for computed colors.
    pub repr: Option<String>,
    /// Modern CSS Color 4 representation. `None` for plain legacy sRGB
    /// colors (the common case); `Some` once the color is space-aware. BOXED so
    /// the 72-byte `ModernColor` doesn't inflate every `Color` (and thus every
    /// `Value`, since `Color` is the largest variant): `Color` shrinks 128 -> 64
    /// bytes, halving every scope slot / `Vec<Value>` element / lookup clone.
    pub modern: Option<Box<ModernColor>>,
}

/// A CSS Color 4 color space.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ColorSpace {
    Rgb,
    Hsl,
    Hwb,
    Srgb,
    SrgbLinear,
    DisplayP3,
    DisplayP3Linear,
    A98Rgb,
    ProphotoRgb,
    Rec2020,
    XyzD65,
    XyzD50,
    Lab,
    Lch,
    Oklab,
    Oklch,
}

impl ColorSpace {
    pub(crate) fn name(self) -> &'static str {
        match self {
            ColorSpace::Rgb => "rgb",
            ColorSpace::Hsl => "hsl",
            ColorSpace::Hwb => "hwb",
            ColorSpace::Srgb => "srgb",
            ColorSpace::SrgbLinear => "srgb-linear",
            ColorSpace::DisplayP3 => "display-p3",
            ColorSpace::DisplayP3Linear => "display-p3-linear",
            ColorSpace::A98Rgb => "a98-rgb",
            ColorSpace::ProphotoRgb => "prophoto-rgb",
            ColorSpace::Rec2020 => "rec2020",
            ColorSpace::XyzD65 => "xyz",
            ColorSpace::XyzD50 => "xyz-d50",
            ColorSpace::Lab => "lab",
            ColorSpace::Lch => "lch",
            ColorSpace::Oklab => "oklab",
            ColorSpace::Oklch => "oklch",
        }
    }

    pub(crate) fn from_name(s: &str) -> Option<ColorSpace> {
        // Color-space names are ASCII case-insensitive (`sRGB`, `Display-P3`,
        // `XYZ` all resolve); callers keep the original spelling for errors and
        // serialization always uses the canonical lower-case `name()`.
        Some(match s.to_ascii_lowercase().as_str() {
            "rgb" => ColorSpace::Rgb,
            "hsl" => ColorSpace::Hsl,
            "hwb" => ColorSpace::Hwb,
            "srgb" => ColorSpace::Srgb,
            "srgb-linear" => ColorSpace::SrgbLinear,
            "display-p3" => ColorSpace::DisplayP3,
            "display-p3-linear" => ColorSpace::DisplayP3Linear,
            "a98-rgb" => ColorSpace::A98Rgb,
            "prophoto-rgb" => ColorSpace::ProphotoRgb,
            "rec2020" => ColorSpace::Rec2020,
            "xyz" | "xyz-d65" => ColorSpace::XyzD65,
            "xyz-d50" => ColorSpace::XyzD50,
            "lab" => ColorSpace::Lab,
            "lch" => ColorSpace::Lch,
            "oklab" => ColorSpace::Oklab,
            "oklch" => ColorSpace::Oklch,
            _ => return None,
        })
    }

    /// Whether this is a legacy space (`rgb`/`hsl`/`hwb`).
    pub(crate) fn is_legacy(self) -> bool {
        matches!(self, ColorSpace::Rgb | ColorSpace::Hsl | ColorSpace::Hwb)
    }

    /// The three channel names for this space (in storage order).
    pub(crate) fn channel_names(self) -> [&'static str; 3] {
        match self {
            ColorSpace::Rgb
            | ColorSpace::Srgb
            | ColorSpace::SrgbLinear
            | ColorSpace::DisplayP3
            | ColorSpace::DisplayP3Linear
            | ColorSpace::A98Rgb
            | ColorSpace::ProphotoRgb
            | ColorSpace::Rec2020 => ["red", "green", "blue"],
            ColorSpace::Hsl => ["hue", "saturation", "lightness"],
            ColorSpace::Hwb => ["hue", "whiteness", "blackness"],
            ColorSpace::XyzD65 | ColorSpace::XyzD50 => ["x", "y", "z"],
            ColorSpace::Lab | ColorSpace::Oklab => ["lightness", "a", "b"],
            ColorSpace::Lch | ColorSpace::Oklch => ["lightness", "chroma", "hue"],
        }
    }
}

/// A color-space-aware color. Channels are stored in CSS Color 4 canonical
/// units (e.g. lab lightness `0..=100`, oklab lightness `0..=1`, hues in
/// degrees, rgb channels `0..=1`). A `None` channel is a missing channel.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ModernColor {
    pub space: ColorSpace,
    pub channels: [Option<f64>; 3],
    pub alpha: Option<f64>,
}

impl Value {
    /// Serialize as it would appear in a CSS declaration value.
    pub(crate) fn to_css(&self, compressed: bool) -> String {
        match self {
            Value::Number(n) => n.to_css(compressed),
            Value::Color(c) => c.to_css(compressed),
            Value::Str(s) => {
                if s.quoted {
                    serialize_quoted(&s.text)
                } else {
                    serialize_unquoted(&s.text)
                }
            }
            Value::List(l) => l.to_css(compressed),
            Value::Map(m) => m.to_map_css(compressed),
            Value::Bool(b) => b.to_string(),
            Value::Null => String::new(),
            Value::Slash(_, repr) => repr.clone(),
            Value::Calc(node) => node.to_calc_value_css(compressed),
            // Not a valid CSS value; rendered for the "isn't a valid CSS value"
            // error and for `inspect`.
            Value::Function(func) => func.inspect(),
            Value::Mixin(m) => m.inspect(),
        }
    }

    /// Serialize as it would appear inside `#{...}` interpolation, where
    /// strings lose their quotes.
    pub(crate) fn to_interp(&self) -> String {
        match self {
            // A directly interpolated string contributes its RAW text (dart
            // `_performInterpolation` takes `result.text` verbatim); only a
            // string nested in a composite value goes through the unquoted
            // serializer (see `List::to_interp`).
            Value::Str(s) => s.text.to_string(),
            Value::Null => String::new(),
            Value::List(l) => l.to_interp(),
            Value::Map(m) => m.to_map_css(false),
            Value::Slash(_, repr) => repr.clone(),
            Value::Calc(node) => node.to_calc_value_css(false),
            other => other.to_css(false),
        }
    }

    /// The message text for `@warn` / `@debug`: a top-level string is shown
    /// **unquoted**, every other value (including strings nested in a list) is
    /// shown with the dart-sass `inspect()` serialization (`"a" "b"`, `a b c`,
    /// `(k: v)`). Matches dart-sass's `value.toString()`.
    pub(crate) fn to_message(&self) -> String {
        match self {
            Value::Str(s) => s.text.to_string(),
            other => crate::builtins::inspect_value(other),
        }
    }

    /// The message text for `@error`: the dart-sass `inspect()` serialization
    /// of the argument, additionally wrapping an unbracketed multi-element list
    /// in parentheses (`(a b c)`, `(a, b)`). A string keeps its quotes.
    pub(crate) fn to_error_message(&self) -> String {
        crate::builtins::inspect_element(self, ListSep::Space)
    }

    pub(crate) fn type_name(&self) -> &'static str {
        match self {
            Value::Number(_) => "number",
            Value::Color(_) => "color",
            Value::Str(_) => "string",
            // An argument list (`$args...`) reports as `arglist`, not `list`.
            Value::List(l) if l.keywords.is_some() => "arglist",
            Value::List(_) => "list",
            Value::Map(_) => "map",
            Value::Bool(_) => "bool",
            Value::Null => "null",
            Value::Slash(_, _) => "number",
            Value::Calc(_) => "calculation",
            Value::Function(_) => "function",
            Value::Mixin(_) => "mixin",
        }
    }

    /// Collapse a top-level slash-division value to its plain `number`,
    /// matching dart-sass's `withoutSlash`. Numbers nested inside lists keep
    /// their slash spelling, so lists are returned unchanged.
    pub(crate) fn without_slash(self) -> Value {
        match self {
            Value::Slash(n, _) => Value::Number(n),
            other => other,
        }
    }

    /// Sass truthiness: everything except `false` and `null` is truthy.
    pub(crate) fn is_truthy(&self) -> bool {
        !matches!(self, Value::Bool(false) | Value::Null)
    }

    /// Sass `==` equality. Numbers compare by value and unit; strings by
    /// text (quotedness is ignored); colors by channel; lists structurally.
    pub(crate) fn sass_eq(&self, other: &Value) -> bool {
        // A slash-division value compares as the plain number it wraps. Unwrap
        // only the slash operand(s) — the non-slash side is compared by
        // reference (no clone of a potentially large list/map).
        if matches!(self, Value::Slash(_, _)) || matches!(other, Value::Slash(_, _)) {
            let unwrap_num;
            let lhs: &Value = match self {
                Value::Slash(n, _) => {
                    unwrap_num = Value::Number(n.clone());
                    &unwrap_num
                }
                other => other,
            };
            let unwrap_num2;
            let rhs: &Value = match other {
                Value::Slash(n, _) => {
                    unwrap_num2 = Value::Number(n.clone());
                    &unwrap_num2
                }
                other => other,
            };
            return lhs.sass_eq(rhs);
        }
        match (self, other) {
            (Value::Number(a), Value::Number(b)) => numbers_eq(a, b),
            (Value::Calc(a), Value::Calc(b)) => a == b,
            (Value::Str(a), Value::Str(b)) => a.text == b.text,
            (Value::Color(a), Value::Color(b)) => color_sass_eq(a, b),
            (Value::Bool(a), Value::Bool(b)) => a == b,
            (Value::Null, Value::Null) => true,
            (Value::List(a), Value::List(b)) => {
                a.sep == b.sep
                    && a.bracketed == b.bracketed
                    && a.items.len() == b.items.len()
                    && a.items.iter().zip(b.items.iter()).all(|(x, y)| x.sass_eq(y))
            }
            // Maps compare by content regardless of entry order.
            (Value::Map(a), Value::Map(b)) => {
                a.entries.len() == b.entries.len()
                    && a.entries
                        .iter()
                        .all(|(k, v)| b.get(k).is_some_and(|bv| bv.sass_eq(v)))
            }
            // An empty map and an empty list are equal in dart-sass.
            (Value::Map(m), Value::List(l)) | (Value::List(l), Value::Map(m)) => {
                m.entries.is_empty() && l.items.is_empty()
            }
            // Function references: built-ins by name, user functions by the
            // identity of their captured definition (see `SassFunction`).
            (Value::Function(a), Value::Function(b)) => a == b,
            // Mixin references: built-ins by name, user mixins by the identity
            // of their captured definition (see `SassMixin`).
            (Value::Mixin(a), Value::Mixin(b)) => a == b,
            _ => false,
        }
    }
}

/// dart-sass's numeric tolerance for `==` comparisons. Two finite numbers are
/// equal when their difference is below this epsilon (so `1in == 96px` and a
/// color channel like `127.99999999999861` equals `128`), mirroring
/// dart-sass's `fuzzyEquals`.
const FUZZY_EPSILON: f64 = 1e-11;

/// dart-sass `fuzzyEquals`: exact equality (so `Infinity == Infinity`) or a
/// finite difference within [`FUZZY_EPSILON`]. `NaN` is never equal to
/// anything (matching dart-sass and IEEE semantics).
fn fuzzy_eq(a: f64, b: f64) -> bool {
    a == b || (a - b).abs() < FUZZY_EPSILON
}

/// Sass `==` for two colors (dart-sass). Legacy colors with no missing channel
/// compare by their sRGB bytes, so they're space-agnostic
/// (`rgb(255 0 0) == hsl(0 100% 50%)`). Otherwise — a non-legacy space on either
/// side, or any missing (`none`) channel — they must share a color space and
/// have channel-by-channel equal (and equally-missing) channels and alpha.
fn color_sass_eq(a: &Color, b: &Color) -> bool {
    let space = |c: &Color| c.modern.as_ref().map(|m| m.space).unwrap_or(ColorSpace::Rgb);
    let chan_eq = |x: Option<f64>, y: Option<f64>| match (x, y) {
        (Some(p), Some(q)) => fuzzy_eq(p, q),
        (None, None) => true,
        _ => false,
    };
    // An untagged color is a plain legacy rgb with no missing channel.
    let channels = |c: &Color| -> [Option<f64>; 3] {
        match &c.modern {
            Some(m) => m.channels,
            None => [Some(c.r), Some(c.g), Some(c.b)],
        }
    };
    let alpha = |c: &Color| c.modern.as_ref().map_or(Some(c.a), |m| m.alpha);
    let (sa, sb) = (space(a), space(b));
    if sa != sb {
        // Different spaces are equal only when BOTH are legacy, compared via
        // their sRGB bytes (with `none` resolved); a non-legacy space never
        // equals a different space.
        if sa.is_legacy() && sb.is_legacy() {
            return fuzzy_eq(a.r, b.r) && fuzzy_eq(a.g, b.g) && fuzzy_eq(a.b, b.b) && fuzzy_eq(a.a, b.a);
        }
        return false;
    }
    // Same space: compare channel-by-channel, where a missing (`none`) channel
    // equals only another missing channel.
    let (ca, cb) = (channels(a), channels(b));
    (0..3).all(|i| chan_eq(ca[i], cb[i])) && chan_eq(alpha(a), alpha(b))
}

/// Sass `==` for two numbers. Numbers with the *exact* same unit compare by
/// value; numbers with different but convertible units compare after
/// converting `b` into `a`'s unit (so `1in == 96px`, `100grad == 90deg`).
/// Units are case-sensitive in `==` (dart-sass: `1PX != 1px`), so conversion
/// only applies to canonical lowercase units. Unitless vs unit-bearing, or
/// incompatible units, are never equal. Value comparisons are fuzzy
/// ([`fuzzy_eq`]).
fn numbers_eq(a: &Number, b: &Number) -> bool {
    if a.unit() == b.unit() {
        return fuzzy_eq(a.value, b.value);
    }
    // A unitless number is only equal to another unitless number; a differing
    // unit (handled above) is the only remaining same-emptiness case.
    if a.is_unitless() || b.is_unitless() {
        return false;
    }
    // Conversion is keyed on canonical lowercase units, matching dart-sass's
    // unit table; an authored uppercase unit (`IN`) is treated as unknown and
    // never converts.
    if a.unit().bytes().any(|c| c.is_ascii_uppercase()) || b.unit().bytes().any(|c| c.is_ascii_uppercase()) {
        return false;
    }
    match convert_factor(b.unit(), a.unit()) {
        Some(factor) => fuzzy_eq(a.value, b.value * factor),
        None => false,
    }
}

/// Append a unit with dart's `_writeIdentifier` escaping: a control
/// character (a decoded `1\9` IE hack) re-serializes as a hex escape with
/// its terminating space (`1\9 `); ordinary units append verbatim.
fn push_unit_escaped(out: &mut String, unit: &str) {
    if unit.chars().all(|c| (c as u32) >= 0x20 && (c as u32) != 0x7F) {
        out.push_str(unit);
        return;
    }
    for c in unit.chars() {
        let cu = c as u32;
        if cu < 0x20 || cu == 0x7F {
            out.push('\\');
            out.push_str(&format!("{cu:x}"));
            out.push(' ');
        } else {
            out.push(c);
        }
    }
}

impl Number {
    pub(crate) fn to_css(&self, compressed: bool) -> String {
        // A non-finite number serializes as a `calc()` constant, matching
        // dart-sass: a unitless `infinity`/`-infinity`/`NaN` prints as
        // `calc(infinity)` etc., and a unit-bearing one as `calc(infinity * 1px)`.
        // A multi-unit number likewise has no plain CSS form and serializes as
        // a calc with its units spelled out (`calc(1px * 1em)`, `calc(1 / 1px)`)
        // — dart-sass uses this form everywhere, including `meta.inspect`,
        // interpolation, and error messages.
        if !self.value.is_finite() || self.has_complex_units() {
            return format!("calc({})", calc_number_css(self, compressed));
        }
        // Append the unit onto fmt_num's own String — a `format!` here would
        // re-run the formatting machinery and allocate a second time on the
        // hottest serialization path in the compiler.
        let mut s = fmt_num(self.value, compressed);
        push_unit_escaped(&mut s, self.unit());
        s
    }

    /// dart-sass `unitString`: the human-readable unit list — `px`, `px*em`,
    /// `px/em`, `px*em/(rad*s)`, `px^-1`, `(px*em)^-1`, or `""` for unitless.
    /// Used by `math.unit()`.
    pub(crate) fn unit_string(&self) -> String {
        let numer = self.numer_units();
        let denom = self.denom_units();
        match (numer.is_empty(), denom.is_empty()) {
            (_, true) => numer.join("*"),
            (true, false) => {
                if denom.len() == 1 {
                    format!("{}^-1", denom[0])
                } else {
                    format!("({})^-1", denom.join("*"))
                }
            }
            (false, false) => {
                if denom.len() == 1 {
                    format!("{}/{}", numer.join("*"), denom[0])
                } else {
                    format!("{}/({})", numer.join("*"), denom.join("*"))
                }
            }
        }
    }

    /// Multiply by `other` with dart-sass `multiplyUnits` cancellation: each
    /// numerator cancels the first convertible denominator on the other side
    /// (scaling the value by the conversion), leftovers concatenate.
    pub(crate) fn mul(&self, other: &Number) -> Number {
        // Fast path: a unitless operand has nothing to cancel or concatenate,
        // so the result carries the other side's units verbatim — no unit-list
        // materialization (the general path builds four Vec<String>s).
        if matches!(other.units, Units::None) {
            return self.copy_units(self.value * other.value);
        }
        if matches!(self.units, Units::None) {
            return other.copy_units(self.value * other.value);
        }
        multiply_units(
            self.value * other.value,
            self.numer_units().to_vec(),
            self.denom_units().to_vec(),
            other.numer_units().to_vec(),
            other.denom_units().to_vec(),
        )
    }

    /// Divide by `other` (multiplication with `other`'s units inverted).
    pub(crate) fn div(&self, other: &Number) -> Number {
        // Fast paths mirroring `mul`: a unitless divisor leaves the units
        // untouched, and same-named single units cancel exactly (`px/px`).
        if matches!(other.units, Units::None) {
            return self.copy_units(self.value / other.value);
        }
        if let (Units::Single(a), Units::Single(b)) = (&self.units, &other.units) {
            if a == b {
                return Number::unitless(self.value / other.value);
            }
        }
        multiply_units(
            self.value / other.value,
            self.numer_units().to_vec(),
            self.denom_units().to_vec(),
            other.denom_units().to_vec(),
            other.numer_units().to_vec(),
        )
    }
}

/// The conversion factor used when a numerator unit cancels a denominator
/// unit: identical names (exact strings, like dart) cancel exactly (covering
/// unknown units like `foo/foo`), and units in the same dimension convert.
fn cancel_factor(numerator: &str, denominator: &str) -> Option<f64> {
    if numerator == denominator {
        return Some(1.0);
    }
    convert_factor(numerator, denominator)
}

/// The factor converting a value with `from` unit lists into `to` unit lists,
/// or `None` when they are incompatible. Each `from` numerator must match a
/// distinct convertible `to` numerator (any order), and likewise for the
/// denominators (a denominator's factor divides). Mirrors dart-sass's
/// `coerce`/`convertValue` for multi-unit numbers.
pub(crate) fn unit_lists_factor(from: (&[String], &[String]), to: (&[String], &[String])) -> Option<f64> {
    fn match_lists(from: &[String], to: &[String]) -> Option<f64> {
        if from.len() != to.len() {
            return None;
        }
        let mut factor = 1.0;
        let mut remaining: Vec<&String> = to.iter().collect();
        for f in from {
            let i = remaining.iter().position(|t| cancel_factor(f, t).is_some())?;
            if let Some(fac) = cancel_factor(f, remaining[i]) {
                factor *= fac;
            }
            remaining.remove(i);
        }
        Some(factor)
    }
    let numer = match_lists(from.0, to.0)?;
    let denom = match_lists(from.1, to.1)?;
    Some(numer / denom)
}

/// dart-sass `multiplyUnits`: combine two numbers' unit lists, cancelling
/// each numerator against the first convertible denominator of the OTHER
/// operand (the value picks up the conversion factor — `1s` cancelling a
/// `/ms` denominator scales by 1000), and concatenating what remains.
fn multiply_units(
    mut value: f64,
    numer1: Vec<String>,
    denom1: Vec<String>,
    numer2: Vec<String>,
    denom2: Vec<String>,
) -> Number {
    let mut numer = Vec::new();
    let mut denom2 = denom2;
    for n in numer1 {
        match denom2.iter().position(|d| cancel_factor(&n, d).is_some()) {
            Some(i) => {
                if let Some(factor) = cancel_factor(&n, &denom2[i]) {
                    value *= factor;
                }
                denom2.remove(i);
            }
            None => numer.push(n),
        }
    }
    let mut denom1 = denom1;
    for n in numer2 {
        match denom1.iter().position(|d| cancel_factor(&n, d).is_some()) {
            Some(i) => {
                if let Some(factor) = cancel_factor(&n, &denom1[i]) {
                    value *= factor;
                }
                denom1.remove(i);
            }
            None => numer.push(n),
        }
    }
    denom1.extend(denom2);
    Number::with_units(value, numer, denom1)
}

/// A CSS dimension group whose units can be converted into one another.
///
/// Units in the same group are mutually convertible via [`convert_factor`];
/// units in different groups (or unknown units, `%`, etc.) are incompatible.
///
/// Note: dart-sass does NOT treat the frequency units `hz`/`khz` as
/// convertible in arithmetic (`1khz + 500hz` is an error), so frequency is
/// deliberately omitted here to match its behaviour byte-for-byte.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Dim {
    Length,
    Angle,
    Time,
    Frequency,
    Resolution,
}

/// The CONVERTIBLE dimension group a unit belongs to, or `None` for `%`, an
/// unknown unit, or a unitless number. dart-sass's conversion table matches
/// unit names case-SENSITIVELY (`1in + 1q` converts but `1in + 1Q` errors,
/// `PX` never converts to `px`), even though calc-class *classification* is
/// case-insensitive (see `known_calc_class`).
pub(crate) fn unit_dimension(unit: &str) -> Option<Dim> {
    match unit {
        "px" | "in" | "cm" | "mm" | "q" | "pt" | "pc" => Some(Dim::Length),
        "deg" | "grad" | "rad" | "turn" => Some(Dim::Angle),
        "s" | "ms" => Some(Dim::Time),
        "Hz" | "kHz" => Some(Dim::Frequency),
        "dpi" | "dpcm" | "dppx" => Some(Dim::Resolution),
        _ => None,
    }
}

/// The canonical factor for a unit: a value of `1<unit>` equals
/// `canonical_factor(unit)` canonical units of its group. `None` for any
/// unit not in a convertible dimension group.
///
/// Canonical bases (verified against dart-sass):
/// length → px, angle → deg, time → s, frequency → Hz, resolution → dpi.
/// Lookups are case-sensitive like dart's conversion table.
fn canonical_factor(unit: &str) -> Option<f64> {
    use std::f64::consts::PI;
    Some(match unit {
        // length (canonical: px)
        "px" => 1.0,
        "in" => 96.0,
        "cm" => 96.0 / 2.54,
        "mm" => 96.0 / 25.4,
        "q" => 96.0 / 101.6,
        "pt" => 96.0 / 72.0,
        "pc" => 16.0,
        // angle (canonical: deg)
        "deg" => 1.0,
        "grad" => 9.0 / 10.0,
        "rad" => 180.0 / PI,
        "turn" => 360.0,
        // time (canonical: s)
        "s" => 1.0,
        "ms" => 1.0 / 1000.0,
        // frequency (canonical: Hz)
        "Hz" => 1.0,
        "kHz" => 1000.0,
        // resolution (canonical: dpi)
        "dpi" => 1.0,
        "dpcm" => 2.54,
        "dppx" => 96.0,
        _ => return None,
    })
}

/// The multiplier to convert a value in `from` units to `to` units
/// (`value_in_to = value_in_from * convert_factor(from, to)`). Returns
/// `None` when the two units are not in the same convertible group.
pub(crate) fn convert_factor(from: &str, to: &str) -> Option<f64> {
    if !units_compatible(from, to) {
        return None;
    }
    let f = canonical_factor(from)?;
    let t = canonical_factor(to)?;
    Some(f / t)
}

/// Whether two units can be combined in arithmetic. Identical units (exact
/// strings — dart treats `PX` and `px` as DIFFERENT units) are always
/// compatible; otherwise both must share a convertible dimension group. An
/// empty unit (unitless) is handled by the caller, not here — this is the
/// strict "two real units" test, mirroring dart-sass's `isComparableTo`.
pub(crate) fn units_compatible(a: &str, b: &str) -> bool {
    if a == b {
        return true;
    }
    match (unit_dimension(a), unit_dimension(b)) {
        (Some(da), Some(db)) => da == db,
        _ => false,
    }
}

/// A relative CSS length unit (`em`, `vw`, `ch`, …). dart-sass knows these are
/// *lengths* (so `calc(1ch + 1deg)` is a cross-dimension error) even though
/// they are not convertible to absolute lengths (so `calc(1px + 1vw)` is
/// preserved). The list mirrors dart-sass's font/viewport/container relative
/// length units; `%` and `fr` are NOT lengths (they preserve against
/// everything), and unknown units stay unknown.
fn is_relative_length(unit: &str) -> bool {
    matches!(
        unit.to_ascii_lowercase().as_str(),
        // font-relative
        "em" | "rem"
            | "ex"
            | "rex"
            | "cap"
            | "rcap"
            | "ch"
            | "rch"
            | "ic"
            | "ric"
            | "lh"
            | "rlh"
            // viewport-relative
            | "vw"
            | "svw"
            | "lvw"
            | "dvw"
            | "vh"
            | "svh"
            | "lvh"
            | "dvh"
            | "vi"
            | "svi"
            | "lvi"
            | "dvi"
            | "vb"
            | "svb"
            | "lvb"
            | "dvb"
            | "vmin"
            | "svmin"
            | "lvmin"
            | "dvmin"
            | "vmax"
            | "svmax"
            | "lvmax"
            | "dvmax"
            // container-relative
            | "cqw"
            | "cqh"
            | "cqi"
            | "cqb"
            | "cqmin"
            | "cqmax"
    )
}

/// The broad class a unit belongs to for `calc()` compatibility decisions.
/// Unlike [`Dim`] (the *convertible*, case-sensitive groups), classification
/// is case-INSENSITIVE (`calc(1PX + 1s)` and `calc(1Q + 1s)` error
/// cross-class even though `PX`/`Q` never convert) and includes the relative
/// length units. Returns `None` for `%`, `fr`, and genuinely unknown units,
/// which `calc()` preserves verbatim rather than rejecting.
fn known_calc_class(unit: &str) -> Option<u8> {
    if is_relative_length(unit) {
        return Some(0); // length class (relative, not convertible)
    }
    match unit.to_ascii_lowercase().as_str() {
        "px" | "in" | "cm" | "mm" | "q" | "pt" | "pc" => Some(0),
        "deg" | "grad" | "rad" | "turn" => Some(1),
        "s" | "ms" => Some(2),
        "dpi" | "dpcm" | "dppx" => Some(3),
        "hz" | "khz" => Some(4),
        _ => None,
    }
}

/// Whether two distinct real units make a `calc()` `+`/`-` an error rather
/// than a preserved expression. dart-sass errors only when BOTH units are
/// known absolute units of DIFFERENT classes (`calc(1px + 1s)`,
/// `calc(1px + 1hz)`); when either unit is relative/unknown
/// (`calc(1px + 1vw)`, `calc(100% - 10px)`) or they share a class but are
/// not convertible (`calc(1khz + 1hz)`), the expression is kept verbatim.
pub(crate) fn calc_units_incompatible(a: &str, b: &str) -> bool {
    match (known_calc_class(a), known_calc_class(b)) {
        (Some(ca), Some(cb)) => ca != cb,
        _ => false,
    }
}

/// dart `Value.isBlank`: `null`, an EMPTY unquoted string, or a list whose
/// every element is blank. Blank elements vanish from list serialization
/// (`foo #{""}` emits `foo`, separator included).
fn value_is_blank(v: &Value) -> bool {
    match v {
        Value::Null => true,
        Value::Str(s) => !s.quoted && s.text.is_empty(),
        // A BRACKETED list is never blank (`[[]]` and `foo []` survive).
        Value::List(l) => !l.bracketed && l.items.iter().all(value_is_blank),
        _ => false,
    }
}

impl List {
    fn to_css(&self, compressed: bool) -> String {
        let sep = match (self.sep, compressed) {
            // An undecided list has at most one element, so its separator
            // string is never actually used; serialize it like a space list.
            (ListSep::Space | ListSep::Undecided, _) => " ",
            (ListSep::Comma, true) => ",",
            (ListSep::Comma, false) => ", ",
            (ListSep::Slash, true) => "/",
            (ListSep::Slash, false) => " / ",
        };
        let inner = self
            .items
            .iter()
            .filter(|v| !value_is_blank(v))
            .map(|v| v.to_css(compressed))
            .collect::<Vec<_>>()
            .join(sep);
        if self.bracketed {
            format!("[{inner}]")
        } else {
            inner
        }
    }

    fn to_interp(&self) -> String {
        let sep = match self.sep {
            ListSep::Space | ListSep::Undecided => " ",
            ListSep::Comma => ", ",
            ListSep::Slash => " / ",
        };
        let inner = self
            .items
            .iter()
            .filter(|v| !value_is_blank(v))
            // A string INSIDE a composite value serializes through dart's
            // quote-less serializer (`_visitUnquotedString`): its newlines
            // collapse to single spaces (issue_1786 `"#{a $str-with-lf}"`),
            // unlike a directly interpolated string's raw text.
            .map(|v| match v {
                Value::Str(s) => serialize_unquoted(&s.text),
                other => other.to_interp(),
            })
            .collect::<Vec<_>>()
            .join(sep);
        if self.bracketed {
            format!("[{inner}]")
        } else {
            inner
        }
    }
}

impl Color {
    pub(crate) fn rgb(r: f64, g: f64, b: f64, a: f64) -> Self {
        Color {
            r,
            g,
            b,
            a,
            repr: None,
            modern: None,
        }
    }

    /// Parse the hex digits following a `#` (3/4/6/8 long). Returns `None`
    /// on an invalid length or digit.
    pub(crate) fn from_hex(digits: &str) -> Option<Color> {
        let parse = |s: &str| u8::from_str_radix(s, 16).ok().map(|v| v as f64);
        let (r, g, b, a) = match digits.len() {
            3 => {
                let d: Vec<char> = digits.chars().collect();
                (
                    parse(&format!("{0}{0}", d[0]))?,
                    parse(&format!("{0}{0}", d[1]))?,
                    parse(&format!("{0}{0}", d[2]))?,
                    255.0,
                )
            }
            4 => {
                let d: Vec<char> = digits.chars().collect();
                (
                    parse(&format!("{0}{0}", d[0]))?,
                    parse(&format!("{0}{0}", d[1]))?,
                    parse(&format!("{0}{0}", d[2]))?,
                    parse(&format!("{0}{0}", d[3]))?,
                )
            }
            6 => (
                parse(&digits[0..2])?,
                parse(&digits[2..4])?,
                parse(&digits[4..6])?,
                255.0,
            ),
            8 => (
                parse(&digits[0..2])?,
                parse(&digits[2..4])?,
                parse(&digits[4..6])?,
                parse(&digits[6..8])?,
            ),
            _ => return None,
        };
        let a = a / 255.0;
        let opaque = (a - 1.0).abs() < f64::EPSILON;
        // dart-sass preserves the *authored* spelling of an opaque 3- or
        // 6-digit hex literal verbatim — `#fff` stays `#fff`, `#FFAA00` keeps
        // its uppercase. The alpha-channel forms (4/8 digits) are always
        // canonicalized: opaque ones round-trip as lowercase 6-digit hex
        // (`#abcf` -> `#aabbcc`), while partial-alpha ones fall back to a
        // computed `rgba()`.
        let repr = if opaque {
            match digits.len() {
                3 | 6 => Some(format!("#{digits}")),
                _ => Some(format!(
                    "#{:02x}{:02x}{:02x}",
                    r.round() as u8,
                    g.round() as u8,
                    b.round() as u8
                )),
            }
        } else {
            None
        };
        Some(Color {
            r,
            g,
            b,
            a,
            repr,
            modern: None,
        })
    }

    /// Convert to HSL: hue in degrees `[0,360)`, saturation/lightness in
    /// `[0,1]`.
    pub(crate) fn to_hsl(&self) -> (f64, f64, f64) {
        let r = self.r / 255.0;
        let g = self.g / 255.0;
        let b = self.b / 255.0;
        let max = r.max(g).max(b);
        let min = r.min(g).min(b);
        let l = (max + min) / 2.0;
        let d = max - min;
        let s = if d == 0.0 {
            0.0
        } else {
            d / (1.0 - (2.0 * l - 1.0).abs())
        };
        let h = if d == 0.0 {
            0.0
        } else if max == r {
            60.0 * (((g - b) / d).rem_euclid(6.0))
        } else if max == g {
            60.0 * ((b - r) / d + 2.0)
        } else {
            60.0 * ((r - g) / d + 4.0)
        };
        (h.rem_euclid(360.0), s, l)
    }

    /// Build a color from HSL (hue degrees, sat/light `[0,1]`) + alpha.
    pub(crate) fn from_hsl(h: f64, s: f64, l: f64, a: f64) -> Color {
        let h = h.rem_euclid(360.0);
        let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
        let x = c * (1.0 - (((h / 60.0) % 2.0) - 1.0).abs());
        let m = l - c / 2.0;
        let (r1, g1, b1) = if h < 60.0 {
            (c, x, 0.0)
        } else if h < 120.0 {
            (x, c, 0.0)
        } else if h < 180.0 {
            (0.0, c, x)
        } else if h < 240.0 {
            (0.0, x, c)
        } else if h < 300.0 {
            (x, 0.0, c)
        } else {
            (c, 0.0, x)
        };
        Color::rgb((r1 + m) * 255.0, (g1 + m) * 255.0, (b1 + m) * 255.0, a)
    }

    fn channels_are_int(&self) -> bool {
        let int = |v: f64| (v - v.round()).abs() < 1e-9;
        int(self.r) && int(self.g) && int(self.b)
    }

    pub(crate) fn to_css(&self, compressed: bool) -> String {
        if let Some(m) = &self.modern {
            return m.to_css(compressed);
        }
        if !compressed {
            if let Some(repr) = &self.repr {
                return repr.clone();
            }
        }
        let opaque = (self.a - 1.0).abs() < f64::EPSILON;
        if opaque && self.channels_are_int() {
            let r = self.r.round().clamp(0.0, 255.0) as u8;
            let g = self.g.round().clamp(0.0, 255.0) as u8;
            let b = self.b.round().clamp(0.0, 255.0) as u8;
            let hex = format!("#{r:02x}{g:02x}{b:02x}");
            if compressed {
                // dart-sass compressed: emit whichever is shorter, the shortest
                // hex or the color's canonical CSS name (name wins ties — e.g.
                // `aqua` == `#0ff`). `compressed_color_name` is the value-keyed
                // reverse table of exactly the names that are no longer than the
                // hex; absent => hex is shorter.
                let short = shorten_hex(&hex);
                if let Some(name) = compressed_color_name(r, g, b) {
                    if name.len() <= short.len() {
                        return name.to_string();
                    }
                }
                return short;
            }
            return hex;
        }
        let (r, g, b) = (
            fmt_num(self.r, compressed),
            fmt_num(self.g, compressed),
            fmt_num(self.b, compressed),
        );
        if opaque {
            if compressed {
                format!("rgb({r},{g},{b})")
            } else {
                format!("rgb({r}, {g}, {b})")
            }
        } else {
            let a = fmt_num(self.a, compressed);
            if compressed {
                format!("rgba({r},{g},{b},{a})")
            } else {
                format!("rgba({r}, {g}, {b}, {a})")
            }
        }
    }
}

/// The CSS named-color spelling for an exact, fully-opaque integer RGB
/// triple (dart-sass canonical names). `None` if the triple has no name.
pub(crate) fn rgb_name(r: f64, g: f64, b: f64) -> Option<&'static str> {
    let int = |v: f64| {
        let r = v.round();
        // Range-check the ROUNDED value: a converted channel may sit a few
        // ulps past 255 and still round to a named color (dart fuzzyRound).
        if (v - r).abs() < 1e-9 && (0.0..=255.0).contains(&r) {
            Some(r as u16)
        } else {
            None
        }
    };
    let (r, g, b) = (int(r)?, int(g)?, int(b)?);
    match (r, g, b) {
        (0, 0, 0) => Some("black"),
        (0, 0, 128) => Some("navy"),
        (0, 0, 139) => Some("darkblue"),
        (0, 0, 205) => Some("mediumblue"),
        (0, 0, 255) => Some("blue"),
        (0, 100, 0) => Some("darkgreen"),
        (0, 128, 0) => Some("green"),
        (0, 128, 128) => Some("teal"),
        (0, 139, 139) => Some("darkcyan"),
        (0, 191, 255) => Some("deepskyblue"),
        (0, 206, 209) => Some("darkturquoise"),
        (0, 250, 154) => Some("mediumspringgreen"),
        (0, 255, 0) => Some("lime"),
        (0, 255, 127) => Some("springgreen"),
        (0, 255, 255) => Some("aqua"),
        (25, 25, 112) => Some("midnightblue"),
        (30, 144, 255) => Some("dodgerblue"),
        (32, 178, 170) => Some("lightseagreen"),
        (34, 139, 34) => Some("forestgreen"),
        (46, 139, 87) => Some("seagreen"),
        (47, 79, 79) => Some("darkslategray"),
        (50, 205, 50) => Some("limegreen"),
        (60, 179, 113) => Some("mediumseagreen"),
        (64, 224, 208) => Some("turquoise"),
        (65, 105, 225) => Some("royalblue"),
        (70, 130, 180) => Some("steelblue"),
        (72, 61, 139) => Some("darkslateblue"),
        (72, 209, 204) => Some("mediumturquoise"),
        (75, 0, 130) => Some("indigo"),
        (85, 107, 47) => Some("darkolivegreen"),
        (95, 158, 160) => Some("cadetblue"),
        (100, 149, 237) => Some("cornflowerblue"),
        (102, 51, 153) => Some("rebeccapurple"),
        (102, 205, 170) => Some("mediumaquamarine"),
        (105, 105, 105) => Some("dimgray"),
        (106, 90, 205) => Some("slateblue"),
        (107, 142, 35) => Some("olivedrab"),
        (112, 128, 144) => Some("slategray"),
        (119, 136, 153) => Some("lightslategray"),
        (123, 104, 238) => Some("mediumslateblue"),
        (124, 252, 0) => Some("lawngreen"),
        (127, 255, 0) => Some("chartreuse"),
        (127, 255, 212) => Some("aquamarine"),
        (128, 0, 0) => Some("maroon"),
        (128, 0, 128) => Some("purple"),
        (128, 128, 0) => Some("olive"),
        (128, 128, 128) => Some("gray"),
        (135, 206, 235) => Some("skyblue"),
        (135, 206, 250) => Some("lightskyblue"),
        (138, 43, 226) => Some("blueviolet"),
        (139, 0, 0) => Some("darkred"),
        (139, 0, 139) => Some("darkmagenta"),
        (139, 69, 19) => Some("saddlebrown"),
        (143, 188, 143) => Some("darkseagreen"),
        (144, 238, 144) => Some("lightgreen"),
        (147, 112, 219) => Some("mediumpurple"),
        (148, 0, 211) => Some("darkviolet"),
        (152, 251, 152) => Some("palegreen"),
        (153, 50, 204) => Some("darkorchid"),
        (154, 205, 50) => Some("yellowgreen"),
        (160, 82, 45) => Some("sienna"),
        (165, 42, 42) => Some("brown"),
        (169, 169, 169) => Some("darkgray"),
        (173, 216, 230) => Some("lightblue"),
        (173, 255, 47) => Some("greenyellow"),
        (175, 238, 238) => Some("paleturquoise"),
        (176, 196, 222) => Some("lightsteelblue"),
        (176, 224, 230) => Some("powderblue"),
        (178, 34, 34) => Some("firebrick"),
        (184, 134, 11) => Some("darkgoldenrod"),
        (186, 85, 211) => Some("mediumorchid"),
        (188, 143, 143) => Some("rosybrown"),
        (189, 183, 107) => Some("darkkhaki"),
        (192, 192, 192) => Some("silver"),
        (199, 21, 133) => Some("mediumvioletred"),
        (205, 92, 92) => Some("indianred"),
        (205, 133, 63) => Some("peru"),
        (210, 105, 30) => Some("chocolate"),
        (210, 180, 140) => Some("tan"),
        (211, 211, 211) => Some("lightgray"),
        (216, 191, 216) => Some("thistle"),
        (218, 112, 214) => Some("orchid"),
        (218, 165, 32) => Some("goldenrod"),
        (219, 112, 147) => Some("palevioletred"),
        (220, 20, 60) => Some("crimson"),
        (220, 220, 220) => Some("gainsboro"),
        (221, 160, 221) => Some("plum"),
        (222, 184, 135) => Some("burlywood"),
        (224, 255, 255) => Some("lightcyan"),
        (230, 230, 250) => Some("lavender"),
        (233, 150, 122) => Some("darksalmon"),
        (238, 130, 238) => Some("violet"),
        (238, 232, 170) => Some("palegoldenrod"),
        (240, 128, 128) => Some("lightcoral"),
        (240, 230, 140) => Some("khaki"),
        (240, 248, 255) => Some("aliceblue"),
        (240, 255, 240) => Some("honeydew"),
        (240, 255, 255) => Some("azure"),
        (244, 164, 96) => Some("sandybrown"),
        (245, 222, 179) => Some("wheat"),
        (245, 245, 220) => Some("beige"),
        (245, 245, 245) => Some("whitesmoke"),
        (245, 255, 250) => Some("mintcream"),
        (248, 248, 255) => Some("ghostwhite"),
        (250, 128, 114) => Some("salmon"),
        (250, 235, 215) => Some("antiquewhite"),
        (250, 240, 230) => Some("linen"),
        (250, 250, 210) => Some("lightgoldenrodyellow"),
        (253, 245, 230) => Some("oldlace"),
        (255, 0, 0) => Some("red"),
        (255, 0, 255) => Some("fuchsia"),
        (255, 20, 147) => Some("deeppink"),
        (255, 69, 0) => Some("orangered"),
        (255, 99, 71) => Some("tomato"),
        (255, 105, 180) => Some("hotpink"),
        (255, 127, 80) => Some("coral"),
        (255, 140, 0) => Some("darkorange"),
        (255, 160, 122) => Some("lightsalmon"),
        (255, 165, 0) => Some("orange"),
        (255, 182, 193) => Some("lightpink"),
        (255, 192, 203) => Some("pink"),
        (255, 215, 0) => Some("gold"),
        (255, 218, 185) => Some("peachpuff"),
        (255, 222, 173) => Some("navajowhite"),
        (255, 228, 181) => Some("moccasin"),
        (255, 228, 196) => Some("bisque"),
        (255, 228, 225) => Some("mistyrose"),
        (255, 235, 205) => Some("blanchedalmond"),
        (255, 239, 213) => Some("papayawhip"),
        (255, 240, 245) => Some("lavenderblush"),
        (255, 245, 238) => Some("seashell"),
        (255, 248, 220) => Some("cornsilk"),
        (255, 250, 205) => Some("lemonchiffon"),
        (255, 250, 240) => Some("floralwhite"),
        (255, 250, 250) => Some("snow"),
        (255, 255, 0) => Some("yellow"),
        (255, 255, 224) => Some("lightyellow"),
        (255, 255, 240) => Some("ivory"),
        (255, 255, 255) => Some("white"),
        _ => None,
    }
}

fn shorten_hex(hex: &str) -> String {
    // `#aabbcc` -> `#abc` when each channel's nibbles match.
    let b = hex.as_bytes();
    if b.len() == 7 && b[1] == b[2] && b[3] == b[4] && b[5] == b[6] {
        format!("#{}{}{}", b[1] as char, b[3] as char, b[5] as char)
    } else {
        hex.to_string()
    }
}

/// The canonical CSS name for an opaque RGB value, but ONLY for the colors whose
/// name is no longer than their shortest hex — i.e. the exact set dart-sass
/// prefers in compressed output (`red` over `#f00`, `aqua` over `#0ff`). Colors
/// whose hex is shorter (e.g. `magenta` -> `#f0f`) are deliberately absent.
/// Derived from the dart-sass oracle, so duplicate names resolve to dart's
/// canonical pick (cyan -> aqua, grey -> gray).
fn compressed_color_name(r: u8, g: u8, b: u8) -> Option<&'static str> {
    Some(match (r, g, b) {
        (0, 255, 255) => "aqua",
        (240, 255, 255) => "azure",
        (245, 245, 220) => "beige",
        (255, 228, 196) => "bisque",
        (0, 0, 255) => "blue",
        (165, 42, 42) => "brown",
        (255, 127, 80) => "coral",
        (220, 20, 60) => "crimson",
        (139, 0, 0) => "darkred",
        (105, 105, 105) => "dimgray",
        (255, 215, 0) => "gold",
        (128, 128, 128) => "gray",
        (0, 128, 0) => "green",
        (255, 105, 180) => "hotpink",
        (75, 0, 130) => "indigo",
        (255, 255, 240) => "ivory",
        (240, 230, 140) => "khaki",
        (0, 255, 0) => "lime",
        (250, 240, 230) => "linen",
        (128, 0, 0) => "maroon",
        (0, 0, 128) => "navy",
        (253, 245, 230) => "oldlace",
        (128, 128, 0) => "olive",
        (255, 165, 0) => "orange",
        (218, 112, 214) => "orchid",
        (205, 133, 63) => "peru",
        (255, 192, 203) => "pink",
        (221, 160, 221) => "plum",
        (128, 0, 128) => "purple",
        (255, 0, 0) => "red",
        (250, 128, 114) => "salmon",
        (160, 82, 45) => "sienna",
        (192, 192, 192) => "silver",
        (135, 206, 235) => "skyblue",
        (255, 250, 250) => "snow",
        (210, 180, 140) => "tan",
        (0, 128, 128) => "teal",
        (216, 191, 216) => "thistle",
        (255, 99, 71) => "tomato",
        (238, 130, 238) => "violet",
        (245, 222, 179) => "wheat",
        _ => return None,
    })
}

impl ModernColor {
    /// Whether any of the three channels (or alpha) is a missing channel.
    fn has_missing(&self) -> bool {
        self.channels.iter().any(|c| c.is_none()) || self.alpha.is_none()
    }

    fn is_opaque(&self) -> bool {
        matches!(self.alpha, Some(a) if (a - 1.0).abs() < f64::EPSILON)
    }

    /// Serialize a single channel, rendering a missing channel as `none`. A
    /// non-finite value (from a degenerate `calc()`, e.g. a lab a/b channel)
    /// serializes as a `calc()` constant like dart-sass (`calc(infinity)`).
    fn chan(&self, i: usize, compressed: bool) -> String {
        match self.channels[i] {
            None => "none".to_string(),
            Some(v) if !v.is_finite() => Number::unitless(v).to_css(compressed),
            Some(v) => fmt_num(v, compressed),
        }
    }

    /// Serialize a channel as a percentage of `denom` (e.g. lab lightness over
    /// 100, oklab lightness over 1). Missing → `none`; non-finite → a `%`-unit
    /// `calc()` constant (`calc(infinity * 1%)`).
    fn chan_pct(&self, i: usize, denom: f64, compressed: bool) -> String {
        match self.channels[i] {
            None => "none".to_string(),
            Some(v) if !v.is_finite() => Number::with_unit(v, "%".to_string()).to_css(compressed),
            Some(v) => format!("{}%", fmt_num(v / denom * 100.0, compressed)),
        }
    }

    /// Serialize a hue channel with the `deg` suffix. Missing → `none`;
    /// non-finite → a `deg`-unit `calc()` constant (`calc(NaN * 1deg)`).
    fn chan_hue(&self, i: usize, compressed: bool) -> String {
        match self.channels[i] {
            None => "none".to_string(),
            Some(v) if !v.is_finite() => Number::with_unit(v, "deg".to_string()).to_css(compressed),
            Some(v) => format!("{}deg", fmt_num(v, compressed)),
        }
    }

    /// The trailing `/ alpha` (modern form) or `, alpha` (handled by caller).
    fn alpha_str(&self, compressed: bool) -> String {
        match self.alpha {
            None => "none".to_string(),
            Some(a) => fmt_num(a, compressed),
        }
    }

    pub(crate) fn to_css(&self, compressed: bool) -> String {
        use ColorSpace::*;
        let sp = if compressed { "" } else { " " };
        // Legacy spaces with no missing channel use the comma legacy form.
        if self.space.is_legacy() && !self.has_missing() {
            return self.legacy_css(compressed);
        }
        // A lab/lch/oklab/oklch lightness outside [0, 100%] cannot round-trip
        // through the space's own function syntax (the CSS parser clamps it),
        // so dart-sass serializes the exact value via a `color-mix()` whose
        // first color is the unclamped xyz-d65 conversion. Only when no
        // channel is missing (a missing channel uses a different fallback,
        // relative color syntax, not exercised by the spec suite and left
        // as-is here). NaN lightness also fails the range check.
        if matches!(self.space, Lab | Lch | Oklab | Oklch) {
            if let (Some(l), Some(_), Some(_)) = (self.channels[0], self.channels[1], self.channels[2]) {
                let max = if matches!(self.space, Lab | Lch) {
                    100.0
                } else {
                    1.0
                };
                // dart fuzzy compare (epsilon 1e-11): in range iff above 0 and
                // below max, or fuzzy-EQUAL to a bound. NaN fails everything.
                let fuzzy_eq = |a: f64, b: f64| (a - b).abs() < 1e-11;
                let in_range = (l > 0.0 || fuzzy_eq(l, 0.0)) && (l < max || fuzzy_eq(l, max));
                if !in_range {
                    return self.out_of_range_css(compressed);
                }
            }
        }
        match self.space {
            Rgb | Hsl | Hwb => {
                // Legacy space with a missing channel: modern space-separated
                // form keeping the space's own name. rgb channels are plain;
                // hsl/hwb use hue+deg and percentages for the other two.
                let body = match self.space {
                    Hsl => format!(
                        "{} {} {}",
                        self.chan_hue(0, compressed),
                        self.pct_or_none(1, compressed),
                        self.pct_or_none(2, compressed),
                    ),
                    Hwb => format!(
                        "{} {} {}",
                        self.chan_hue(0, compressed),
                        self.pct_or_none(1, compressed),
                        self.pct_or_none(2, compressed),
                    ),
                    _ => format!(
                        "{} {} {}",
                        self.chan(0, compressed),
                        self.chan(1, compressed),
                        self.chan(2, compressed),
                    ),
                };
                self.wrap_modern(self.space.name(), &body, compressed)
            }
            Lab => {
                let body = format!(
                    "{} {} {}",
                    self.chan_pct(0, 100.0, compressed),
                    self.chan(1, compressed),
                    self.chan(2, compressed),
                );
                self.wrap_modern("lab", &body, compressed)
            }
            Lch => {
                let body = format!(
                    "{} {} {}",
                    self.chan_pct(0, 100.0, compressed),
                    self.chan(1, compressed),
                    self.chan_hue(2, compressed),
                );
                self.wrap_modern("lch", &body, compressed)
            }
            Oklab => {
                let body = format!(
                    "{} {} {}",
                    self.chan_pct(0, 1.0, compressed),
                    self.chan(1, compressed),
                    self.chan(2, compressed),
                );
                self.wrap_modern("oklab", &body, compressed)
            }
            Oklch => {
                let body = format!(
                    "{} {} {}",
                    self.chan_pct(0, 1.0, compressed),
                    self.chan(1, compressed),
                    self.chan_hue(2, compressed),
                );
                self.wrap_modern("oklch", &body, compressed)
            }
            // Predefined color() spaces.
            _ => {
                let body = format!(
                    "{}{sp}{}{sp}{}",
                    self.chan(0, compressed),
                    self.chan(1, compressed),
                    self.chan(2, compressed),
                );
                let space = self.space.name();
                if self.is_opaque() {
                    format!("color({space}{sp}{body})")
                } else {
                    format!("color({space}{sp}{body}{sp}/{sp}{})", self.alpha_str(compressed))
                }
            }
        }
    }

    /// The `meta.inspect`/`@debug` serialization (dart `_writeLegacyColor`
    /// with `inspect: true`): an hwb color with no missing channels prints
    /// its own `hwb(H W% B%[ / A])` form (hue without `deg`), and an
    /// out-of-gamut legacy rgb skips the hsl reroute, falling to the plain
    /// `rgb(...)` comma form (named/hex only apply in-gamut). Everything
    /// else serializes exactly like CSS output.
    pub(crate) fn inspect_css(&self) -> String {
        if self.space.is_legacy() && !self.has_missing() {
            let a = self.alpha.unwrap_or(0.0);
            let opaque = (a - 1.0).abs() < 1e-11;
            match self.space {
                ColorSpace::Hwb => {
                    let h = fmt_num(self.channels[0].unwrap_or(0.0), false);
                    let w = fmt_num(self.channels[1].unwrap_or(0.0), false);
                    let b = fmt_num(self.channels[2].unwrap_or(0.0), false);
                    return if opaque {
                        format!("hwb({h} {w}% {b}%)")
                    } else {
                        format!("hwb({h} {w}% {b}% / {})", fmt_num(a, false))
                    };
                }
                ColorSpace::Rgb => {
                    let in_gamut =
                        |i: usize| (-1e-9..=255.0 + 1e-9).contains(&self.channels[i].unwrap_or(0.0));
                    if !(in_gamut(0) && in_gamut(1) && in_gamut(2)) {
                        let r = fmt_num(self.channels[0].unwrap_or(0.0), false);
                        let g = fmt_num(self.channels[1].unwrap_or(0.0), false);
                        let b = fmt_num(self.channels[2].unwrap_or(0.0), false);
                        return if opaque {
                            format!("rgb({r}, {g}, {b})")
                        } else {
                            format!("rgba({r}, {g}, {b}, {})", fmt_num(a, false))
                        };
                    }
                }
                _ => {}
            }
        }
        self.to_css(false)
    }

    /// Serialize an out-of-range lab/lch/oklab/oklch color as
    /// `color-mix(in <space>, color(xyz <x> <y> <z>[ / <alpha>]) 100%, black)`
    /// (dart-sass's fallback for a lightness the own-space syntax would clamp).
    /// Compressed form per dart: no space after commas or before `100%`, and
    /// the irrelevant base color is the shorter `red`:
    /// `color-mix(in lab,color(xyz x y z[/a])100%,red)`. A `none` alpha
    /// converts to `0` in this form.
    fn out_of_range_css(&self, compressed: bool) -> String {
        let xyz = crate::builtins::convert_modern(self, ColorSpace::XyzD65);
        let x = xyz.chan(0, compressed);
        let y = xyz.chan(1, compressed);
        let z = xyz.chan(2, compressed);
        let alpha = self.alpha.unwrap_or(0.0);
        let opaque = self.alpha.is_some() && (alpha - 1.0).abs() < f64::EPSILON;
        let inner = if opaque {
            format!("color(xyz {x} {y} {z})")
        } else if compressed {
            format!("color(xyz {x} {y} {z}/{})", fmt_num(alpha, true))
        } else {
            format!("color(xyz {x} {y} {z} / {})", fmt_num(alpha, false))
        };
        let space = self.space.name();
        if compressed {
            format!("color-mix(in {space},{inner}100%,red)")
        } else {
            format!("color-mix(in {space}, {inner} 100%, black)")
        }
    }

    /// A non-hue percentage channel for the legacy modern form (saturation,
    /// lightness, whiteness, blackness).
    fn pct_or_none(&self, i: usize, compressed: bool) -> String {
        match self.channels[i] {
            None => "none".to_string(),
            Some(v) => format!("{}%", fmt_num(v, compressed)),
        }
    }

    /// Wrap a function-form modern color (`lab(...)`, `hsl(none ...)`) adding a
    /// `/ alpha` when not opaque.
    fn wrap_modern(&self, name: &str, body: &str, compressed: bool) -> String {
        let sp = if compressed { "" } else { " " };
        if self.is_opaque() && self.alpha.is_some() {
            format!("{name}({body})")
        } else {
            format!("{name}({body}{sp}/{sp}{})", self.alpha_str(compressed))
        }
    }

    /// Serialize a legacy color (rgb/hsl/hwb) with no missing channels in the
    /// classic comma form. rgb may collapse to hex; hwb routes through hsl.
    fn legacy_css(&self, compressed: bool) -> String {
        let a = self.alpha.unwrap_or(1.0);
        let opaque = (a - 1.0).abs() < f64::EPSILON;
        match self.space {
            ColorSpace::Rgb => {
                let r = self.channels[0].unwrap_or(0.0);
                let g = self.channels[1].unwrap_or(0.0);
                let b = self.channels[2].unwrap_or(0.0);
                // A legacy rgb color whose channels exceed the [0,255] gamut is
                // serialized via hsl(), matching dart-sass.
                let in_gamut = |v: f64| (-1e-9..=255.0 + 1e-9).contains(&v);
                if !(in_gamut(r) && in_gamut(g) && in_gamut(b)) {
                    let hsl = crate::builtins::srgb_to_hsl([r / 255.0, g / 255.0, b / 255.0]);
                    let hh = fmt_num(hsl[0], compressed);
                    let ss = fmt_num(hsl[1], compressed);
                    let ll = fmt_num(hsl[2], compressed);
                    let comma = if compressed { "," } else { ", " };
                    return if opaque {
                        format!("hsl({hh}{comma}{ss}%{comma}{ll}%)")
                    } else {
                        let aa = fmt_num(a, compressed);
                        format!("hsla({hh}{comma}{ss}%{comma}{ll}%{comma}{aa})")
                    };
                }
                Color {
                    r,
                    g,
                    b,
                    a,
                    repr: None,
                    modern: None,
                }
                .to_css(compressed)
            }
            ColorSpace::Hsl => {
                let (h, s, l) = self.legacy_hsl_triple();
                self.hsl_comma_css(h, s, l, a, opaque, compressed)
            }
            ColorSpace::Hwb => {
                // hwb serializes through its sRGB representation: an
                // integer, in-gamut rgb becomes hex/named; otherwise the
                // classic hsl comma form is used.
                let (rgb, h, s, l) = self.hwb_rgb_and_hsl();
                let int = |v: f64| (v - v.round()).abs() < 1e-9;
                let in_gamut = |v: f64| (-1e-9..=255.0 + 1e-9).contains(&v);
                let rgb_ok = rgb.iter().all(|&v| int(v) && in_gamut(v));
                // Only a fully-OPAQUE integer hwb collapses to a named color or
                // hex; a non-opaque hwb always uses the hsl comma form (dart-sass
                // never emits rgba() for an hwb color).
                if rgb_ok && opaque {
                    if !compressed {
                        if let Some(name) = rgb_name(rgb[0], rgb[1], rgb[2]) {
                            return name.to_string();
                        }
                    }
                    return Color {
                        r: rgb[0],
                        g: rgb[1],
                        b: rgb[2],
                        a,
                        repr: None,
                        modern: None,
                    }
                    .to_css(compressed);
                }
                self.hsl_comma_css(h, s, l, a, opaque, compressed)
            }
            _ => String::new(),
        }
    }

    /// Serialize the classic `hsl(h, s%, l%)` / `hsla(…)` comma form.
    fn hsl_comma_css(&self, h: f64, s: f64, l: f64, a: f64, opaque: bool, compressed: bool) -> String {
        // dart-sass normalizes a negative HSL saturation — which arises when an
        // out-of-range hwb color converts to hsl — by flipping the hue 180° and
        // taking the saturation's magnitude. A non-negative saturation (every
        // normal color) is untouched.
        let (h, s) = if s < 0.0 {
            ((h + 180.0).rem_euclid(360.0), -s)
        } else {
            (h, s)
        };
        // A non-finite channel (an hwb color with a degenerate calc() channel
        // propagates NaN through the hsl conversion) serializes in its calc
        // form: `calc(NaN)` for the hue, `calc(NaN * 1%)` for percentages.
        let hh = if h.is_finite() {
            fmt_num(h, compressed)
        } else {
            Number::unitless(h).to_css(compressed)
        };
        let pct = |v: f64| {
            if v.is_finite() {
                format!("{}%", fmt_num(v, compressed))
            } else {
                Number::with_unit(v, "%").to_css(compressed)
            }
        };
        let ss = pct(s);
        let ll = pct(l);
        let comma = if compressed { "," } else { ", " };
        if opaque {
            format!("hsl({hh}{comma}{ss}{comma}{ll})")
        } else {
            let aa = fmt_num(a, compressed);
            format!("hsla({hh}{comma}{ss}{comma}{ll}{comma}{aa})")
        }
    }

    /// The sRGB-byte triple and the (hue, sat%, light%) for an hwb color,
    /// via the engine's exact dart conversions. A fuzzy-zero saturation makes
    /// the hue powerless; the legacy fill renders it as 0.
    fn hwb_rgb_and_hsl(&self) -> ([f64; 3], f64, f64, f64) {
        let hwb = [
            self.channels[0].unwrap_or(0.0),
            self.channels[1].unwrap_or(0.0),
            self.channels[2].unwrap_or(0.0),
        ];
        let rgb01 = crate::builtins::hwb_to_srgb(hwb);
        let hsl = crate::builtins::srgb_to_hsl(rgb01);
        let hue = if hsl[1].abs() < 1e-11 { 0.0 } else { hsl[0] };
        (
            [rgb01[0] * 255.0, rgb01[1] * 255.0, rgb01[2] * 255.0],
            hue,
            hsl[1],
            hsl[2],
        )
    }

    /// The (hue, saturation%, lightness%) triple for legacy hsl/hwb
    /// serialization. hwb is first converted to hsl via sRGB.
    fn legacy_hsl_triple(&self) -> (f64, f64, f64) {
        match self.space {
            ColorSpace::Hsl => (
                self.channels[0].unwrap_or(0.0),
                self.channels[1].unwrap_or(0.0),
                self.channels[2].unwrap_or(0.0),
            ),
            ColorSpace::Hwb => {
                // Share the hwb->hsl conversion (and its achromatic-hue
                // canonicalization) with hwb_rgb_and_hsl.
                let (_, h, s, l) = self.hwb_rgb_and_hsl();
                (h, s, l)
            }
            _ => (0.0, 0.0, 0.0),
        }
    }
}

/// Format a number the way dart-sass does: round to 10 decimal places,
/// trim trailing zeros, and (when compressed) drop a leading `0`.
pub(crate) fn fmt_num(n: f64, compressed: bool) -> String {
    if n.is_nan() {
        return "NaN".to_string();
    }
    if n.is_infinite() {
        return if n > 0.0 {
            "Infinity".to_string()
        } else {
            "-Infinity".to_string()
        };
    }
    // Integers print the way the dart VM does: `fuzzyAsInt` converts the
    // double to a NATIVE int64 (saturating, exactly like Rust's `as i64`)
    // and prints its exact decimal expansion — `593644542057412224`, not the
    // shortest-roundtrip `…200`. A magnitude past int64 fails the round-trip
    // check (saturation changes the value) and falls back to the shortest
    // form, which never overflows into exponential notation.
    let mut s = if n.fract() == 0.0 {
        let i = n as i64;
        if i as f64 == n {
            fmt_i64(i)
        } else {
            // Integer-valued but past i64: Ryū prints the shortest form in
            // plain positional notation, exactly like Display.
            let mut s = String::with_capacity(24);
            crate::ryu::format64(n, &mut s);
            s
        }
    } else {
        // dart `_writeNumber`: serialize the SHORTEST round-trip decimal
        // (`double.toString()` + exponent removal), then round it AT THE
        // STRING LEVEL to 10 decimal places, looking only at the 11th digit
        // (half-up). This follows the shortest spelling, not the true value:
        // `2154.15598416745` (true value …44978) still rounds UP to
        // `…1675` because its shortest form ends in a literal `5`.
        round_decimal_string(ecma_shortest(n))
    };
    if s == "-0" {
        s = "0".to_string();
    }
    if compressed {
        // Drop the leading `0` of `0.5`/`-0.5` -> `.5`/`-.5` in place (the `0`
        // is ASCII, so the byte index is the char index) rather than building a
        // fresh String with `format!`.
        if s.starts_with("0.") {
            s.remove(0);
        } else if s.starts_with("-0.") {
            s.remove(1);
        }
    }
    s
}

/// `i64` → decimal `String` without the `core::fmt` machinery. Numbers are
/// the hottest serialization path (every dimension in every declaration), and
/// `format!("{i}")` spends most of its time in `Formatter` dispatch, not in
/// digit generation. Output is byte-identical to `i64`'s `Display`.
fn fmt_i64(v: i64) -> String {
    // Longest spelling: "-9223372036854775808" = 20 bytes.
    let mut buf = [0u8; 20];
    let neg = v < 0;
    let mut u = v.unsigned_abs();
    let mut at = buf.len();
    loop {
        at -= 1;
        buf[at] = b'0' + (u % 10) as u8;
        u /= 10;
        if u == 0 {
            break;
        }
    }
    if neg {
        at -= 1;
        buf[at] = b'-';
    }
    // The buffer holds only ASCII digits and '-'.
    std::str::from_utf8(&buf[at..]).expect("ascii").to_string()
}

/// dart `double.toString()` (ECMA-262 Number::toString): the shortest
/// decimal that round-trips, breaking a tie between two equidistant
/// spellings by choosing the EVEN final digit. Rust's `{}` is also shortest
/// but ties differently (`657390374199289.25` prints `…289.3`, dart prints
/// `…289.2`), so re-round to the same significant-digit count through
/// `{:e}`'s half-to-even rounding and expand the exponent form.
fn ecma_shortest(n: f64) -> String {
    // The zero-dep Ryū port: byte-identical to `format!("{n}")` (proven by
    // its differential fuzz) without the core::fmt machinery, writing into
    // one pre-sized allocation.
    let mut rust = String::with_capacity(24);
    crate::ryu::format64(n, &mut rust);
    // Count significant digits (skipping sign, dot, and leading zeros).
    let sig = rust
        .chars()
        .filter(|c| c.is_ascii_digit())
        .skip_while(|&c| c == '0')
        .count();
    if sig == 0 {
        return rust;
    }
    if !rust.contains('e') && n.abs() >= f64::MIN_POSITIVE {
        // A tie between two equidistant shortest spellings requires the
        // spacing of sig-digit decimals (relative, at least 10^-sig) to fit
        // inside the value's one-ulp round-trip window (relative at most
        // 2^-52 for normal doubles), which forces sig >= 16. At 15 or fewer
        // significant digits the shortest round-trip spelling is unique, so
        // Rust's `{}` already IS ECMA toString — skip the re-round and its
        // two parses. Subnormals (wider relative ulps) and exponent-form
        // spellings take the slow path.
        if sig <= 15 {
            return rust;
        }
        // A tie can only flip the FINAL digit of the spelling: the two
        // candidates are same-length decimals one apart in the last place,
        // and a carry would end the higher one in `0` — a trailing zero
        // means a shorter spelling round-trips, contradicting shortest-form.
        // The caller rounds at the 10th fractional digit reading only the
        // 11th, so a final digit deeper than the 11th fractional place never
        // reaches the output and the tie-break is moot.
        if let Some(dot) = rust.find('.') {
            if rust.len() - dot - 1 > 11 {
                return rust;
            }
        }
    }
    let sci = format!("{:.*e}", sig - 1, n);
    if sci.parse::<f64>() != Ok(n) {
        return rust;
    }
    let expanded = expand_exponent(&sci);
    if expanded.parse::<f64>() == Ok(n) {
        expanded
    } else {
        rust
    }
}

/// Expand `D.DDDDeK` scientific notation into a plain decimal string (dart
/// `_removeExponent`).
fn expand_exponent(sci: &str) -> String {
    let Some(epos) = sci.find('e') else {
        return sci.to_string();
    };
    let exp: i32 = sci[epos + 1..].parse().unwrap_or(0);
    let mantissa = &sci[..epos];
    let neg = mantissa.starts_with('-');
    let digits: String = mantissa.chars().filter(|c| c.is_ascii_digit()).collect();
    let sign = if neg { "-" } else { "" };
    // One digit sits before the dot in `{:e}` form, so the decimal point in
    // the digit string belongs after position `exp + 1`.
    let point = exp + 1;
    if point <= 0 {
        let zeros = "0".repeat((-point) as usize);
        format!("{sign}0.{zeros}{digits}")
    } else if (point as usize) >= digits.len() {
        let zeros = "0".repeat(point as usize - digits.len());
        format!("{sign}{digits}{zeros}")
    } else {
        format!(
            "{sign}{}.{}",
            &digits[..point as usize],
            &digits[point as usize..]
        )
    }
}

/// dart `_writeRounded`: round a plain decimal string to 10 fractional
/// digits by inspecting ONLY the 11th digit (>= '5' carries up), then trim
/// trailing fractional zeros. Ten or fewer fractional digits pass verbatim.
fn round_decimal_string(text: String) -> String {
    let Some(dot) = text.find('.') else {
        return text;
    };
    if text.len() - dot - 1 <= 10 {
        return trim_fraction(text);
    }
    let mut bytes: Vec<u8> = text.into_bytes();
    let round_up = bytes[dot + 11] >= b'5';
    bytes.truncate(dot + 11);
    if round_up {
        let mut i = bytes.len() - 1;
        loop {
            match bytes[i] {
                b'.' => i -= 1,
                b'9' => {
                    bytes[i] = b'0';
                    if i == 0 {
                        bytes.insert(0, b'1');
                        break;
                    }
                    i -= 1;
                }
                b'-' => {
                    bytes.insert(i + 1, b'1');
                    break;
                }
                _ => {
                    bytes[i] += 1;
                    break;
                }
            }
        }
    }
    trim_fraction(String::from_utf8(bytes).expect("ascii decimal"))
}

/// Trim trailing fractional zeros (and a then-trailing dot); `-0` and
/// all-zero results collapse to `0` at the caller.
fn trim_fraction(mut s: String) -> String {
    if s.contains('.') {
        while s.ends_with('0') {
            s.pop();
        }
        if s.ends_with('.') {
            s.pop();
        }
    }
    if s == "-0" || s == "0" {
        return "0".to_string();
    }
    s
}

/// Look up a CSS named color. Covers the complete set of 148 CSS Color 4
/// named colors plus `transparent`; unknown identifiers stay unquoted strings.
pub(crate) fn named_color(name: &str) -> Option<Color> {
    let (r, g, b, a) = match name.to_ascii_lowercase().as_str() {
        "transparent" => (0, 0, 0, 0.0),
        "aliceblue" => (240, 248, 255, 1.0),
        "antiquewhite" => (250, 235, 215, 1.0),
        "aqua" | "cyan" => (0, 255, 255, 1.0),
        "aquamarine" => (127, 255, 212, 1.0),
        "azure" => (240, 255, 255, 1.0),
        "beige" => (245, 245, 220, 1.0),
        "bisque" => (255, 228, 196, 1.0),
        "black" => (0, 0, 0, 1.0),
        "blanchedalmond" => (255, 235, 205, 1.0),
        "blue" => (0, 0, 255, 1.0),
        "blueviolet" => (138, 43, 226, 1.0),
        "brown" => (165, 42, 42, 1.0),
        "burlywood" => (222, 184, 135, 1.0),
        "cadetblue" => (95, 158, 160, 1.0),
        "chartreuse" => (127, 255, 0, 1.0),
        "chocolate" => (210, 105, 30, 1.0),
        "coral" => (255, 127, 80, 1.0),
        "cornflowerblue" => (100, 149, 237, 1.0),
        "cornsilk" => (255, 248, 220, 1.0),
        "crimson" => (220, 20, 60, 1.0),
        "darkblue" => (0, 0, 139, 1.0),
        "darkcyan" => (0, 139, 139, 1.0),
        "darkgoldenrod" => (184, 134, 11, 1.0),
        "darkgray" | "darkgrey" => (169, 169, 169, 1.0),
        "darkgreen" => (0, 100, 0, 1.0),
        "darkkhaki" => (189, 183, 107, 1.0),
        "darkmagenta" => (139, 0, 139, 1.0),
        "darkolivegreen" => (85, 107, 47, 1.0),
        "darkorange" => (255, 140, 0, 1.0),
        "darkorchid" => (153, 50, 204, 1.0),
        "darkred" => (139, 0, 0, 1.0),
        "darksalmon" => (233, 150, 122, 1.0),
        "darkseagreen" => (143, 188, 143, 1.0),
        "darkslateblue" => (72, 61, 139, 1.0),
        "darkslategray" | "darkslategrey" => (47, 79, 79, 1.0),
        "darkturquoise" => (0, 206, 209, 1.0),
        "darkviolet" => (148, 0, 211, 1.0),
        "deeppink" => (255, 20, 147, 1.0),
        "deepskyblue" => (0, 191, 255, 1.0),
        "dimgray" | "dimgrey" => (105, 105, 105, 1.0),
        "dodgerblue" => (30, 144, 255, 1.0),
        "firebrick" => (178, 34, 34, 1.0),
        "floralwhite" => (255, 250, 240, 1.0),
        "forestgreen" => (34, 139, 34, 1.0),
        "fuchsia" | "magenta" => (255, 0, 255, 1.0),
        "gainsboro" => (220, 220, 220, 1.0),
        "ghostwhite" => (248, 248, 255, 1.0),
        "gold" => (255, 215, 0, 1.0),
        "goldenrod" => (218, 165, 32, 1.0),
        "gray" | "grey" => (128, 128, 128, 1.0),
        "green" => (0, 128, 0, 1.0),
        "greenyellow" => (173, 255, 47, 1.0),
        "honeydew" => (240, 255, 240, 1.0),
        "hotpink" => (255, 105, 180, 1.0),
        "indianred" => (205, 92, 92, 1.0),
        "indigo" => (75, 0, 130, 1.0),
        "ivory" => (255, 255, 240, 1.0),
        "khaki" => (240, 230, 140, 1.0),
        "lavender" => (230, 230, 250, 1.0),
        "lavenderblush" => (255, 240, 245, 1.0),
        "lawngreen" => (124, 252, 0, 1.0),
        "lemonchiffon" => (255, 250, 205, 1.0),
        "lightblue" => (173, 216, 230, 1.0),
        "lightcoral" => (240, 128, 128, 1.0),
        "lightcyan" => (224, 255, 255, 1.0),
        "lightgoldenrodyellow" => (250, 250, 210, 1.0),
        "lightgray" | "lightgrey" => (211, 211, 211, 1.0),
        "lightgreen" => (144, 238, 144, 1.0),
        "lightpink" => (255, 182, 193, 1.0),
        "lightsalmon" => (255, 160, 122, 1.0),
        "lightseagreen" => (32, 178, 170, 1.0),
        "lightskyblue" => (135, 206, 250, 1.0),
        "lightslategray" | "lightslategrey" => (119, 136, 153, 1.0),
        "lightsteelblue" => (176, 196, 222, 1.0),
        "lightyellow" => (255, 255, 224, 1.0),
        "lime" => (0, 255, 0, 1.0),
        "limegreen" => (50, 205, 50, 1.0),
        "linen" => (250, 240, 230, 1.0),
        "maroon" => (128, 0, 0, 1.0),
        "mediumaquamarine" => (102, 205, 170, 1.0),
        "mediumblue" => (0, 0, 205, 1.0),
        "mediumorchid" => (186, 85, 211, 1.0),
        "mediumpurple" => (147, 112, 219, 1.0),
        "mediumseagreen" => (60, 179, 113, 1.0),
        "mediumslateblue" => (123, 104, 238, 1.0),
        "mediumspringgreen" => (0, 250, 154, 1.0),
        "mediumturquoise" => (72, 209, 204, 1.0),
        "mediumvioletred" => (199, 21, 133, 1.0),
        "midnightblue" => (25, 25, 112, 1.0),
        "mintcream" => (245, 255, 250, 1.0),
        "mistyrose" => (255, 228, 225, 1.0),
        "moccasin" => (255, 228, 181, 1.0),
        "navajowhite" => (255, 222, 173, 1.0),
        "navy" => (0, 0, 128, 1.0),
        "oldlace" => (253, 245, 230, 1.0),
        "olive" => (128, 128, 0, 1.0),
        "olivedrab" => (107, 142, 35, 1.0),
        "orange" => (255, 165, 0, 1.0),
        "orangered" => (255, 69, 0, 1.0),
        "orchid" => (218, 112, 214, 1.0),
        "palegoldenrod" => (238, 232, 170, 1.0),
        "palegreen" => (152, 251, 152, 1.0),
        "paleturquoise" => (175, 238, 238, 1.0),
        "palevioletred" => (219, 112, 147, 1.0),
        "papayawhip" => (255, 239, 213, 1.0),
        "peachpuff" => (255, 218, 185, 1.0),
        "peru" => (205, 133, 63, 1.0),
        "pink" => (255, 192, 203, 1.0),
        "plum" => (221, 160, 221, 1.0),
        "powderblue" => (176, 224, 230, 1.0),
        "purple" => (128, 0, 128, 1.0),
        "rebeccapurple" => (102, 51, 153, 1.0),
        "red" => (255, 0, 0, 1.0),
        "rosybrown" => (188, 143, 143, 1.0),
        "royalblue" => (65, 105, 225, 1.0),
        "saddlebrown" => (139, 69, 19, 1.0),
        "salmon" => (250, 128, 114, 1.0),
        "sandybrown" => (244, 164, 96, 1.0),
        "seagreen" => (46, 139, 87, 1.0),
        "seashell" => (255, 245, 238, 1.0),
        "sienna" => (160, 82, 45, 1.0),
        "silver" => (192, 192, 192, 1.0),
        "skyblue" => (135, 206, 235, 1.0),
        "slateblue" => (106, 90, 205, 1.0),
        "slategray" | "slategrey" => (112, 128, 144, 1.0),
        "snow" => (255, 250, 250, 1.0),
        "springgreen" => (0, 255, 127, 1.0),
        "steelblue" => (70, 130, 180, 1.0),
        "tan" => (210, 180, 140, 1.0),
        "teal" => (0, 128, 128, 1.0),
        "thistle" => (216, 191, 216, 1.0),
        "tomato" => (255, 99, 71, 1.0),
        "turquoise" => (64, 224, 208, 1.0),
        "violet" => (238, 130, 238, 1.0),
        "wheat" => (245, 222, 179, 1.0),
        "white" => (255, 255, 255, 1.0),
        "whitesmoke" => (245, 245, 245, 1.0),
        "yellow" => (255, 255, 0, 1.0),
        "yellowgreen" => (154, 205, 50, 1.0),
        _ => return None,
    };
    Some(Color {
        r: r as f64,
        g: g as f64,
        b: b as f64,
        a,
        repr: Some(name.to_string()),
        modern: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fmt_num_trims_and_rounds() {
        assert_eq!(fmt_num(153.0, false), "153");
        assert_eq!(fmt_num(178.5, false), "178.5");
        assert_eq!(fmt_num(0.5, false), "0.5");
        assert_eq!(fmt_num(-0.0, false), "0");
        assert_eq!(fmt_num(16.0, false), "16");
    }

    #[test]
    fn fmt_num_large_values_stay_plain_decimals() {
        // Exact integers print in full, never exponential.
        assert_eq!(fmt_num(123456789012345.0, false), "123456789012345");
        assert_eq!(fmt_num(-123456789012345.0, false), "-123456789012345");
        assert_eq!(fmt_num(1e20, false), "100000000000000000000");
        // Within int64, the exact decimal expansion (dart VM int printing).
        assert_eq!(fmt_num(1234567890123456789.0, false), "1234567890123456768");
        assert_eq!(fmt_num(593644542057412224.0, false), "593644542057412224");
        // 2^63 saturates to int64 max and still round-trips (dart VM).
        assert_eq!(fmt_num(9223372036854775808.0, false), "9223372036854775807");
        // Beyond int64 the shortest round-tripping form matches dart-sass.
        assert_eq!(fmt_num(92233720368547758070.0, false), "92233720368547760000");
        assert_eq!(
            fmt_num(99999999999999999999999999999.0, false),
            "100000000000000000000000000000"
        );
    }

    #[test]
    fn fmt_num_rounds_fractions_to_ten_places() {
        assert_eq!(fmt_num(0.1 + 0.2, false), "0.3");
        assert_eq!(fmt_num(1.0 / 3.0, false), "0.3333333333");
        assert_eq!(fmt_num(2.0 / 3.0, false), "0.6666666667");
        assert_eq!(fmt_num(123456.78901234567, false), "123456.7890123457");
        // Below the tenth decimal place rounds away entirely.
        assert_eq!(fmt_num(1e-11, false), "0");
        // Halfway digits round away from zero (dart-sass), not to even.
        assert_eq!(fmt_num(0.00000000015, false), "0.0000000002");
        assert_eq!(fmt_num(0.00000000035, false), "0.0000000004");
        assert_eq!(fmt_num(0.99999999995, false), "1");
        assert_eq!(fmt_num(0.30000000005, false), "0.3000000001");
    }

    #[test]
    fn fmt_num_compressed_drops_leading_zero() {
        assert_eq!(fmt_num(0.5, true), ".5");
        assert_eq!(fmt_num(-0.25, true), "-.25");
        assert_eq!(fmt_num(2.0, true), "2");
    }

    fn num(value: f64, unit: &str) -> CalcNode {
        CalcNode::Number(Number::with_unit(value, unit.to_string()))
    }

    #[test]
    fn calc_serialization_drops_redundant_parens() {
        // `1px + 2% * var(--c)`: `*` binds tighter, so no parens are needed.
        let node = CalcNode::Op {
            op: CalcOp::Add,
            left: Box::new(num(1.0, "px")),
            right: Box::new(CalcNode::Op {
                op: CalcOp::Mul,
                left: Box::new(num(2.0, "%")),
                right: Box::new(CalcNode::Str("var(--c)".into())),
            }),
        };
        assert_eq!(Value::Calc(node).to_css(false), "calc(1px + 2% * var(--c))");
    }

    #[test]
    fn calc_serialization_keeps_required_parens_and_flips_sign() {
        // `1px - (2% + var(--c))`: subtracting a sum needs parens.
        let node = CalcNode::Op {
            op: CalcOp::Sub,
            left: Box::new(num(1.0, "px")),
            right: Box::new(CalcNode::Op {
                op: CalcOp::Add,
                left: Box::new(num(2.0, "%")),
                right: Box::new(CalcNode::Str("var(--c)".into())),
            }),
        };
        assert_eq!(Value::Calc(node).to_css(false), "calc(1px - (2% + var(--c)))");
        // `1% + -1px` serializes as `1% - 1px`.
        let flip = CalcNode::Op {
            op: CalcOp::Add,
            left: Box::new(num(1.0, "%")),
            right: Box::new(num(-1.0, "px")),
        };
        assert_eq!(Value::Calc(flip).to_css(false), "calc(1% - 1px)");
    }

    #[test]
    fn hex_parsing_and_serialization() {
        // dart-sass preserves the authored spelling of opaque 3-/6-digit hex.
        let c = Color::from_hex("336699").expect("valid hex");
        assert_eq!(c.to_css(false), "#336699");
        let short = Color::from_hex("369").expect("valid hex");
        assert_eq!(short.to_css(false), "#369");
        // Uppercase and mixed case round-trip verbatim.
        let upper = Color::from_hex("FFAA00").expect("valid hex");
        assert_eq!(upper.to_css(false), "#FFAA00");
        let upper_short = Color::from_hex("ABC").expect("valid hex");
        assert_eq!(upper_short.to_css(false), "#ABC");
        // Compressed mode ignores the authored form and shortens.
        assert_eq!(upper.to_css(true), "#fa0");
        // 4-/8-digit alpha forms are canonicalized, not preserved. An opaque
        // one collapses to lowercase 6-digit hex; partial alpha -> rgba().
        let opaque4 = Color::from_hex("369f").expect("valid hex");
        assert_eq!(opaque4.to_css(false), "#336699");
        let partial = Color::from_hex("33669980").expect("valid hex");
        assert!(partial.to_css(false).starts_with("rgba("));
    }

    #[test]
    fn compressed_uses_shortest_of_name_or_hex() {
        // dart-sass compressed emits whichever is shorter, the canonical name or
        // the shortest hex (name wins ties).
        let name = |hex: &str| Color::from_hex(hex).expect("valid hex").to_css(true);

        // Name strictly shorter than hex -> name.
        assert_eq!(name("ff0000"), "red"); // red(3) < #f00(4)
        assert_eq!(name("ffa500"), "orange"); // orange(6) < #ffa500(7)
                                              // Tie (#rgb-expandable, 4 vs 4) -> name wins.
        assert_eq!(name("00ffff"), "aqua");
        assert_eq!(name("0000ff"), "blue");
        assert_eq!(name("00ff00"), "lime");
        // Hex shorter than name -> hex.
        assert_eq!(name("ff00ff"), "#f0f"); // magenta(7) > #f0f(4)
        assert_eq!(name("ffffff"), "#fff"); // white(5) > #fff(4)
                                            // No name -> hex.
        assert_eq!(name("3366cc"), "#36c");

        // Duplicate names resolve to dart's canonical pick (value, not spelling).
        assert_eq!(named_color("cyan").unwrap().to_css(true), "aqua");
        assert_eq!(named_color("grey").unwrap().to_css(true), "gray");
        // A computed (non-literal) color still names by value.
        assert_eq!(Color::rgb(0.0, 255.0, 255.0, 1.0).to_css(true), "aqua");

        // Expanded is unaffected: authored spelling is preserved.
        assert_eq!(Color::from_hex("ff0000").unwrap().to_css(false), "#ff0000");
        assert_eq!(named_color("red").unwrap().to_css(false), "red");
    }

    #[test]
    fn hsl_roundtrip_is_exact_for_integer_rgb() {
        // #336699 == hsl(210, 50%, 40%)
        let c = Color::from_hex("336699").expect("valid hex");
        let (h, s, l) = c.to_hsl();
        assert!((h - 210.0).abs() < 1e-9);
        assert!((s - 0.5).abs() < 1e-9);
        assert!((l - 0.4).abs() < 1e-9);
        // lighten by 10% -> exactly dart-sass's fractional rgb.
        let lit = Color::from_hsl(h, s, l + 0.1, 1.0);
        assert_eq!(lit.to_css(false), "rgb(63.75, 127.5, 191.25)");
    }

    #[test]
    fn computed_fractional_channels_serialize_as_rgb() {
        let c = Color::rgb(153.0, 178.5, 204.0, 1.0);
        assert_eq!(c.to_css(false), "rgb(153, 178.5, 204)");
    }

    #[test]
    fn alpha_color_serializes_as_rgba() {
        let c = Color::rgb(0.0, 0.0, 0.0, 0.5);
        assert_eq!(c.to_css(false), "rgba(0, 0, 0, 0.5)");
    }

    #[test]
    fn named_colors_resolve_and_preserve_spelling() {
        let red = named_color("red").expect("named");
        assert_eq!(red.to_css(false), "red");
        assert!(named_color("definitely-not-a-color").is_none());
        // Extended CSS names resolve to their exact sRGB channels.
        let plum = named_color("plum").expect("named");
        assert_eq!((plum.r, plum.g, plum.b, plum.a), (221.0, 160.0, 221.0, 1.0));
        let rebecca = named_color("rebeccapurple").expect("named");
        assert_eq!((rebecca.r, rebecca.g, rebecca.b), (102.0, 51.0, 153.0));
        // Grey/gray aliases share channel values (the repr keeps the spelling).
        let grey = named_color("darkgrey").expect("named");
        let gray = named_color("darkgray").expect("named");
        assert_eq!((grey.r, grey.g, grey.b), (gray.r, gray.g, gray.b));
    }

    #[test]
    fn unit_dimensions_group_known_units() {
        assert_eq!(unit_dimension("px"), Some(Dim::Length));
        assert_eq!(unit_dimension("deg"), Some(Dim::Angle));
        assert_eq!(unit_dimension("ms"), Some(Dim::Time));
        assert_eq!(unit_dimension("kHz"), Some(Dim::Frequency));
        assert_eq!(unit_dimension("dppx"), Some(Dim::Resolution));
        // The conversion table is case-sensitive (dart: `1in + 1Q` errors,
        // `1in + 1q` converts), so `PT`/`khz` have no group; nor do `%`,
        // viewport units, or unitless.
        assert_eq!(unit_dimension("PT"), None);
        assert_eq!(unit_dimension("khz"), None);
        assert_eq!(unit_dimension("%"), None);
        assert_eq!(unit_dimension("vw"), None);
        assert_eq!(unit_dimension(""), None);
    }

    #[test]
    fn units_compatible_within_groups_only() {
        assert!(units_compatible("in", "cm"));
        assert!(units_compatible("deg", "turn"));
        assert!(units_compatible("s", "ms"));
        assert!(units_compatible("Hz", "kHz"));
        assert!(units_compatible("dpi", "dppx"));
        // equal units are always compatible, even `%` and unknown units.
        assert!(units_compatible("%", "%"));
        assert!(units_compatible("PX", "PX"));
        // cross-group, unknown, and case-mismatched units are incompatible
        // (dart: `1px + 1PX` errors).
        assert!(!units_compatible("px", "s"));
        assert!(!units_compatible("px", "vw"));
        assert!(!units_compatible("px", "PX"));
        assert!(!units_compatible("khz", "hz"));
    }

    #[test]
    fn convert_factor_matches_dart_sass() {
        // length: 1in == 96px, 1cm == 96/2.54 px.
        assert_eq!(convert_factor("in", "px"), Some(96.0));
        assert_eq!(convert_factor("px", "px"), Some(1.0));
        let cm_to_in = convert_factor("cm", "in").expect("compatible");
        assert!((cm_to_in - (1.0 / 2.54)).abs() < 1e-12);
        // 1in + 1cm in inches: 1 + 1*(1/2.54) == 1.3937007874...
        assert!((1.0 + cm_to_in - 1.393700787401575).abs() < 1e-12);
        // time: 1ms == 0.001s.
        assert_eq!(convert_factor("ms", "s"), Some(0.001));
        // angle: 1turn == 360deg, 100grad == 90deg.
        assert_eq!(convert_factor("turn", "deg"), Some(360.0));
        assert_eq!(convert_factor("grad", "deg"), Some(0.9));
        // resolution: 1dppx == 96dpi.
        assert_eq!(convert_factor("dppx", "dpi"), Some(96.0));
        // incompatible -> None.
        assert_eq!(convert_factor("px", "s"), None);
        assert_eq!(convert_factor("px", "vw"), None);
    }

    fn numval(value: f64, unit: &str) -> Value {
        Value::Number(Number::with_unit(value, unit.to_string()))
    }

    #[test]
    fn sass_eq_numbers_are_unit_aware_and_fuzzy() {
        // Same unit, equal value.
        assert!(numval(2.0, "px").sass_eq(&numval(2.0, "px")));
        // Convertible units compare after conversion.
        assert!(numval(1.0, "in").sass_eq(&numval(96.0, "px")));
        assert!(numval(96.0, "px").sass_eq(&numval(1.0, "in")));
        assert!(numval(1.0, "cm").sass_eq(&numval(10.0, "mm")));
        assert!(numval(100.0, "grad").sass_eq(&numval(90.0, "deg")));
        assert!(numval(1.0, "s").sass_eq(&numval(1000.0, "ms")));
        // Fuzzy: tiny differences within epsilon are equal.
        assert!(numval(1.000_000_000_000_1, "px").sass_eq(&numval(1.0, "px")));
        // Just outside epsilon is not equal.
        assert!(!numval(1.000_000_001, "px").sass_eq(&numval(1.0, "px")));
        // Unitless vs unit-bearing, incompatible units, `%`, and uppercase
        // units (case-sensitive) are never equal.
        assert!(!numval(1.0, "").sass_eq(&numval(1.0, "px")));
        assert!(!numval(1.0, "px").sass_eq(&numval(1.0, "em")));
        assert!(!numval(50.0, "%").sass_eq(&numval(50.0, "")));
        assert!(!numval(1.0, "PX").sass_eq(&numval(1.0, "px")));
        assert!(!numval(1.0, "IN").sass_eq(&numval(96.0, "px")));
    }

    #[test]
    fn sass_eq_colors_compare_channels_fuzzily() {
        // `purple` (128,0,128) equals an HSL color that resolves to channels
        // a tiny epsilon away (127.999999999998607...).
        let purple = Value::Color(Color::rgb(128.0, 0.0, 128.0, 1.0));
        let computed = Value::Color(Color::rgb(127.999_999_999_998_6, 0.0, 127.999_999_999_998_6, 1.0));
        assert!(purple.sass_eq(&computed));
        // Genuinely different channels are not equal.
        assert!(!purple.sass_eq(&Value::Color(Color::rgb(255.0, 0.0, 0.0, 1.0))));
        // A fractional channel is not equal to its rounded neighbour.
        let frac = Value::Color(Color::rgb(0.4, 0.0, 0.0, 1.0));
        assert!(!frac.sass_eq(&Value::Color(Color::rgb(0.0, 0.0, 0.0, 1.0))));
        assert!(frac.sass_eq(&Value::Color(Color::rgb(0.4, 0.0, 0.0, 1.0))));
        // Differing alpha is not equal.
        assert!(!purple.sass_eq(&Value::Color(Color::rgb(128.0, 0.0, 128.0, 0.5))));
    }
}
