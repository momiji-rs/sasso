//! The Sass value model and its CSS serialization.
//!
//! Numbers carry a unit, colors keep full `f64` channel precision (so
//! computed colors serialize exactly like current dart-sass, e.g.
//! `rgb(63.75, 127.5, 191.25)`), and colors remember their authored
//! spelling so untransformed literals round-trip unchanged.

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
                            let pos = Number {
                                value: -n.value,
                                unit: n.unit.clone(),
                            };
                            return format!("{l} {flipped} {}", pos.to_css(compressed));
                        }
                    }
                }
                format!("{l}{sep}{r}")
            }
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
            CalcNode::Number(n) if !n.value.is_finite() && !n.unit.is_empty() => Some(CalcOp::Mul),
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

/// Render a number inside a `calc()` interior. Finite numbers use their
/// ordinary CSS form; non-finite numbers use dart-sass's canonical lowercase
/// constants — `infinity` / `-infinity` / `NaN` when unitless, and
/// `infinity * 1px` / `NaN * 1px` (the operand spelled out) when they carry a
/// unit.
fn calc_number_css(n: &Number, compressed: bool) -> String {
    if n.value.is_finite() {
        return n.to_css(compressed);
    }
    let constant = if n.value.is_nan() {
        "NaN"
    } else if n.value > 0.0 {
        "infinity"
    } else {
        "-infinity"
    };
    if n.unit.is_empty() {
        constant.to_string()
    } else {
        let star = if compressed { "*" } else { " * " };
        format!("{constant}{star}1{}", n.unit)
    }
}

/// A number and its unit (`unit` is empty for unitless numbers).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Number {
    pub value: f64,
    pub unit: String,
}

/// A string value; `quoted` controls whether it serializes with quotes.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct SassStr {
    pub text: String,
    pub quoted: bool,
}

/// Serialize a string as a quoted CSS string, matching dart-sass's
/// `_visitQuotedString`. The text holds decoded code points; this re-escapes
/// only what it must: the chosen quote character, backslashes, and control
/// characters (everything `0x00..=0x1F` except tab, plus `0x7F`). Double quotes
/// are preferred; a string that contains `"` but no `'` is wrapped in `'`.
pub(crate) fn serialize_quoted(text: &str) -> String {
    let has_double = text.contains('"');
    let has_single = text.contains('\'');
    // Use single quotes only when the text has a `"` and no `'`.
    let quote = if has_double && !has_single { '\'' } else { '"' };
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len() + 2);
    out.push(quote);
    for (i, &c) in chars.iter().enumerate() {
        let cp = c as u32;
        if c == quote || c == '\\' {
            out.push('\\');
            out.push(c);
        } else if (cp <= 0x1F && c != '\t') || cp == 0x7F {
            // Control character: `\<hex>` with a trailing space only when the
            // next character would otherwise extend the escape (a hex digit,
            // space, or tab).
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

/// A list value.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct List {
    pub items: Vec<Value>,
    pub sep: ListSep,
    /// Whether the list was written with square brackets (`[a b]`); such
    /// lists serialize wrapped in `[`...`]` and report `true` from
    /// `list.is-bracketed`.
    pub bracketed: bool,
}

/// A map value: an ordered list of key/value entries. Insertion order is
/// preserved (dart-sass maps are ordered); duplicate keys are resolved by the
/// constructor so at most one entry exists per Sass-equal key.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Map {
    pub entries: Vec<(Value, Value)>,
}

impl Map {
    /// Look up a value by key (Sass `==`), or `None` when absent.
    pub(crate) fn get(&self, key: &Value) -> Option<&Value> {
        self.entries.iter().find(|(k, _)| k.sass_eq(key)).map(|(_, v)| v)
    }

    /// Insert or overwrite an entry, preserving the position of an existing
    /// key (matching dart-sass map ordering).
    pub(crate) fn insert(&mut self, key: Value, value: Value) {
        if let Some(slot) = self.entries.iter_mut().find(|(k, _)| k.sass_eq(&key)) {
            slot.1 = value;
        } else {
            self.entries.push((key, value));
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
}

/// An sRGB color. Channels are `0..=255` and may be fractional; alpha is
/// `0..=1`.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Color {
    pub r: f64,
    pub g: f64,
    pub b: f64,
    pub a: f64,
    /// The authored spelling (`"red"`, `"#336699"`). Used verbatim when
    /// the color is emitted unchanged; `None` for computed colors.
    pub repr: Option<String>,
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
                    s.text.clone()
                }
            }
            Value::List(l) => l.to_css(compressed),
            Value::Map(m) => m.to_map_css(compressed),
            Value::Bool(b) => b.to_string(),
            Value::Null => String::new(),
            Value::Slash(_, repr) => repr.clone(),
            Value::Calc(node) => format!("calc({})", node.to_calc_css(compressed)),
        }
    }

    /// Serialize as it would appear inside `#{...}` interpolation, where
    /// strings lose their quotes.
    pub(crate) fn to_interp(&self) -> String {
        match self {
            Value::Str(s) => s.text.clone(),
            Value::Null => String::new(),
            Value::List(l) => l.to_interp(),
            Value::Map(m) => m.to_map_css(false),
            Value::Slash(_, repr) => repr.clone(),
            Value::Calc(node) => format!("calc({})", node.to_calc_css(false)),
            other => other.to_css(false),
        }
    }

    pub(crate) fn type_name(&self) -> &'static str {
        match self {
            Value::Number(_) => "number",
            Value::Color(_) => "color",
            Value::Str(_) => "string",
            Value::List(_) => "list",
            Value::Map(_) => "map",
            Value::Bool(_) => "bool",
            Value::Null => "null",
            Value::Slash(_, _) => "number",
            Value::Calc(_) => "calculation",
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
        // A slash-division value compares as the plain number it wraps.
        let unslash = |v: &Value| match v {
            Value::Slash(n, _) => Value::Number(n.clone()),
            other => other.clone(),
        };
        if matches!(self, Value::Slash(_, _)) || matches!(other, Value::Slash(_, _)) {
            return unslash(self).sass_eq(&unslash(other));
        }
        match (self, other) {
            (Value::Number(a), Value::Number(b)) => numbers_eq(a, b),
            (Value::Calc(a), Value::Calc(b)) => a == b,
            (Value::Str(a), Value::Str(b)) => a.text == b.text,
            (Value::Color(a), Value::Color(b)) => {
                fuzzy_eq(a.r, b.r) && fuzzy_eq(a.g, b.g) && fuzzy_eq(a.b, b.b) && fuzzy_eq(a.a, b.a)
            }
            (Value::Bool(a), Value::Bool(b)) => a == b,
            (Value::Null, Value::Null) => true,
            (Value::List(a), Value::List(b)) => {
                a.sep == b.sep
                    && a.bracketed == b.bracketed
                    && a.items.len() == b.items.len()
                    && a.items.iter().zip(&b.items).all(|(x, y)| x.sass_eq(y))
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

/// Sass `==` for two numbers. Numbers with the *exact* same unit compare by
/// value; numbers with different but convertible units compare after
/// converting `b` into `a`'s unit (so `1in == 96px`, `100grad == 90deg`).
/// Units are case-sensitive in `==` (dart-sass: `1PX != 1px`), so conversion
/// only applies to canonical lowercase units. Unitless vs unit-bearing, or
/// incompatible units, are never equal. Value comparisons are fuzzy
/// ([`fuzzy_eq`]).
fn numbers_eq(a: &Number, b: &Number) -> bool {
    if a.unit == b.unit {
        return fuzzy_eq(a.value, b.value);
    }
    // A unitless number is only equal to another unitless number; a differing
    // unit (handled above) is the only remaining same-emptiness case.
    if a.unit.is_empty() || b.unit.is_empty() {
        return false;
    }
    // Conversion is keyed on canonical lowercase units, matching dart-sass's
    // unit table; an authored uppercase unit (`IN`) is treated as unknown and
    // never converts.
    if a.unit.bytes().any(|c| c.is_ascii_uppercase()) || b.unit.bytes().any(|c| c.is_ascii_uppercase()) {
        return false;
    }
    match convert_factor(&b.unit, &a.unit) {
        Some(factor) => fuzzy_eq(a.value, b.value * factor),
        None => false,
    }
}

impl Number {
    pub(crate) fn to_css(&self, compressed: bool) -> String {
        // A bare non-finite number serializes as a `calc()` constant, matching
        // dart-sass: a unitless `infinity`/`-infinity`/`NaN` prints as
        // `calc(infinity)` etc., and a unit-bearing one as `calc(infinity * 1px)`.
        if !self.value.is_finite() {
            return format!("calc({})", calc_number_css(self, compressed));
        }
        format!("{}{}", fmt_num(self.value, compressed), self.unit)
    }
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
    Resolution,
}

/// The dimension group a unit belongs to, or `None` for `%`, an unknown
/// unit, or a unitless number. Unit names compare case-insensitively, as in
/// dart-sass (`PX` and `px` are the same unit).
pub(crate) fn unit_dimension(unit: &str) -> Option<Dim> {
    match unit.to_ascii_lowercase().as_str() {
        "px" | "in" | "cm" | "mm" | "q" | "pt" | "pc" => Some(Dim::Length),
        "deg" | "grad" | "rad" | "turn" => Some(Dim::Angle),
        "s" | "ms" => Some(Dim::Time),
        "dpi" | "dpcm" | "dppx" => Some(Dim::Resolution),
        _ => None,
    }
}

/// The canonical factor for a unit: a value of `1<unit>` equals
/// `canonical_factor(unit)` canonical units of its group. `None` for any
/// unit not in a convertible dimension group.
///
/// Canonical bases (verified against dart-sass):
/// length → px, angle → deg, time → s, resolution → dpi.
fn canonical_factor(unit: &str) -> Option<f64> {
    use std::f64::consts::PI;
    Some(match unit.to_ascii_lowercase().as_str() {
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

/// Whether two units can be combined in arithmetic. Equal units (case
/// insensitively, as dart-sass does) are always compatible; otherwise both
/// must be non-empty and share a dimension group. An empty unit (unitless)
/// is handled by the caller, not here — this is the strict "two real units"
/// test, mirroring dart-sass's `isComparableTo`.
pub(crate) fn units_compatible(a: &str, b: &str) -> bool {
    if a.eq_ignore_ascii_case(b) {
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
/// Unlike [`Dim`] (the *convertible* groups), this includes frequency, whose
/// `hz`/`khz` dart-sass recognises as absolute units but does not convert
/// between, and the relative length units, which are known lengths but not
/// convertible. Returns `None` for `%`, `fr`, and genuinely unknown units,
/// which `calc()` preserves verbatim rather than rejecting.
fn known_calc_class(unit: &str) -> Option<u8> {
    if let Some(d) = unit_dimension(unit) {
        return Some(match d {
            Dim::Length => 0,
            Dim::Angle => 1,
            Dim::Time => 2,
            Dim::Resolution => 3,
        });
    }
    if is_relative_length(unit) {
        return Some(0); // length class (relative, not convertible)
    }
    match unit.to_ascii_lowercase().as_str() {
        "hz" | "khz" => Some(4), // frequency: known, but not inter-convertible
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

impl List {
    fn to_css(&self, compressed: bool) -> String {
        let sep = match (self.sep, compressed) {
            (ListSep::Space, _) => " ",
            (ListSep::Comma, true) => ",",
            (ListSep::Comma, false) => ", ",
        };
        let inner = self
            .items
            .iter()
            .filter(|v| !matches!(v, Value::Null))
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
            ListSep::Space => " ",
            ListSep::Comma => ", ",
        };
        let inner = self
            .items
            .iter()
            .filter(|v| !matches!(v, Value::Null))
            .map(|v| v.to_interp())
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
        // Opaque hex round-trips as a canonical lowercase 6-digit hex,
        // matching dart-sass; alpha hex falls back to computed rgba().
        let repr = if (a - 1.0).abs() < f64::EPSILON {
            Some(format!(
                "#{:02x}{:02x}{:02x}",
                r.round() as u8,
                g.round() as u8,
                b.round() as u8
            ))
        } else {
            None
        };
        Some(Color { r, g, b, a, repr })
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
        if !compressed {
            if let Some(repr) = &self.repr {
                return repr.clone();
            }
        }
        let opaque = (self.a - 1.0).abs() < f64::EPSILON;
        if opaque && self.channels_are_int() {
            let hex = format!(
                "#{:02x}{:02x}{:02x}",
                self.r.round().clamp(0.0, 255.0) as u8,
                self.g.round().clamp(0.0, 255.0) as u8,
                self.b.round().clamp(0.0, 255.0) as u8
            );
            if compressed {
                return shorten_hex(&hex);
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

fn shorten_hex(hex: &str) -> String {
    // `#aabbcc` -> `#abc` when each channel's nibbles match.
    let b = hex.as_bytes();
    if b.len() == 7 && b[1] == b[2] && b[3] == b[4] && b[5] == b[6] {
        format!("#{}{}{}", b[1] as char, b[3] as char, b[5] as char)
    } else {
        hex.to_string()
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
    // Integers (including huge literals well beyond `2^53`) print via the
    // shortest round-tripping form, which never overflows into exponential
    // notation and matches dart-sass. Multiplying by `1e10` to round would
    // corrupt large magnitudes, so it is reserved for fractional values.
    let mut s = if n.fract() == 0.0 {
        format!("{n}")
    } else {
        // Round to 10 decimal places via fixed formatting (correct at every
        // magnitude), then re-emit the shortest decimal that round-trips.
        let rounded: f64 = format!("{n:.10}").parse().unwrap_or(n);
        format!("{rounded}")
    };
    if s == "-0" {
        s = "0".to_string();
    }
    if compressed {
        if let Some(rest) = s.strip_prefix("0.") {
            s = format!(".{rest}");
        } else if let Some(rest) = s.strip_prefix("-0.") {
            s = format!("-.{rest}");
        }
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
        // Beyond 2^53 the shortest round-tripping form matches dart-sass.
        assert_eq!(fmt_num(1234567890123456789.0, false), "1234567890123456800");
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
    }

    #[test]
    fn fmt_num_compressed_drops_leading_zero() {
        assert_eq!(fmt_num(0.5, true), ".5");
        assert_eq!(fmt_num(-0.25, true), "-.25");
        assert_eq!(fmt_num(2.0, true), "2");
    }

    fn num(value: f64, unit: &str) -> CalcNode {
        CalcNode::Number(Number {
            value,
            unit: unit.to_string(),
        })
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
        let c = Color::from_hex("336699").expect("valid hex");
        assert_eq!(c.to_css(false), "#336699");
        // 3-digit expands to canonical 6-digit lowercase.
        let short = Color::from_hex("369").expect("valid hex");
        assert_eq!(short.to_css(false), "#336699");
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
        assert_eq!(unit_dimension("PT"), Some(Dim::Length));
        assert_eq!(unit_dimension("deg"), Some(Dim::Angle));
        assert_eq!(unit_dimension("ms"), Some(Dim::Time));
        assert_eq!(unit_dimension("dppx"), Some(Dim::Resolution));
        // `%`, unknown viewport units, frequency, and unitless have no group.
        assert_eq!(unit_dimension("%"), None);
        assert_eq!(unit_dimension("vw"), None);
        assert_eq!(unit_dimension("khz"), None);
        assert_eq!(unit_dimension(""), None);
    }

    #[test]
    fn units_compatible_within_groups_only() {
        assert!(units_compatible("in", "cm"));
        assert!(units_compatible("px", "PX"));
        assert!(units_compatible("deg", "turn"));
        assert!(units_compatible("s", "ms"));
        assert!(units_compatible("dpi", "dppx"));
        // equal units are always compatible, even `%` and unknown units.
        assert!(units_compatible("%", "%"));
        // cross-group and unknown units are incompatible.
        assert!(!units_compatible("px", "s"));
        assert!(!units_compatible("px", "vw"));
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
        Value::Number(Number {
            value,
            unit: unit.to_string(),
        })
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
