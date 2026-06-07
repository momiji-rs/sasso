//! The built-in function library.
//!
//! Each family lives in its own module and exposes `try_call`, returning
//! `Some(result)` for the names it owns and `None` otherwise. The
//! dispatcher tries the families in turn and falls back to preserving an
//! unknown function verbatim as a plain CSS call (`translate(...)`,
//! `var(...)`), matching dart-sass.
//!
//! The modules are disjoint, so new families can be implemented in
//! parallel: an implementer fills one `try_call` without touching another.

mod color;
mod color_ext;
mod list;
mod map;
mod math;
mod meta;
mod selector;
mod string;

use crate::error::Error;
use crate::scanner::Pos;
use crate::value::{Color, Number, SassStr, Value};

/// Dispatch a function call by name across the builtin families.
pub(crate) fn call(
    name: &str,
    pos_args: &[Value],
    named: &[(String, Value)],
    pos: Pos,
) -> Result<Value, Error> {
    if let Some(r) = color::try_call(name, pos_args, named, pos) {
        return r;
    }
    if let Some(r) = color_ext::try_call(name, pos_args, named, pos) {
        return r;
    }
    if let Some(r) = math::try_call(name, pos_args, named, pos) {
        return r;
    }
    if let Some(r) = string::try_call(name, pos_args, named, pos) {
        return r;
    }
    // Map runs before list so `length`/`nth` on a map are handled here; the
    // map family declines those names for non-map arguments, falling through.
    if let Some(r) = map::try_call(name, pos_args, named, pos) {
        return r;
    }
    if let Some(r) = list::try_call(name, pos_args, named, pos) {
        return r;
    }
    if let Some(r) = meta::try_call(name, pos_args, named, pos) {
        return r;
    }
    if let Some(r) = selector::try_call(name, pos_args, named, pos) {
        return r;
    }
    Ok(plain_css_function(name, pos_args, named))
}

/// Whether `name` is a real Sass builtin function (as opposed to an unknown
/// plain CSS function that is preserved verbatim). Used by the evaluator to
/// decide whether a slash-division argument should collapse to its number:
/// Sass functions collapse it, plain CSS functions keep the `a/b` spelling.
///
/// Each family's `try_call` returns `None` only for names it does not own,
/// and the families are pure, so probing with empty arguments is a
/// side-effect-free ownership test.
pub(crate) fn is_builtin(name: &str) -> bool {
    let pos: &[Value] = &[];
    let named: &[(String, Value)] = &[];
    let p = Pos { line: 1, col: 1 };
    color::try_call(name, pos, named, p).is_some()
        || color_ext::try_call(name, pos, named, p).is_some()
        || math::try_call(name, pos, named, p).is_some()
        || string::try_call(name, pos, named, p).is_some()
        || map::try_call(name, pos, named, p).is_some()
        || list::try_call(name, pos, named, p).is_some()
        || meta::try_call(name, pos, named, p).is_some()
        || selector::try_call(name, pos, named, p).is_some()
}

// ---- shared argument helpers, available to every family module --------

/// The argument at index `i`: positional first, then by name (`params[i]`).
pub(super) fn arg<'v>(
    params: &[&str],
    pos_args: &'v [Value],
    named: &'v [(String, Value)],
    i: usize,
) -> Option<&'v Value> {
    if let Some(v) = pos_args.get(i) {
        return Some(v);
    }
    let pname = params.get(i)?;
    named.iter().find(|(n, _)| n == pname).map(|(_, v)| v)
}

/// Like [`arg`] but errors with a "missing argument" message when absent.
pub(super) fn require<'v>(
    params: &[&str],
    pos_args: &'v [Value],
    named: &'v [(String, Value)],
    i: usize,
    fname: &str,
    pos: Pos,
) -> Result<&'v Value, Error> {
    arg(params, pos_args, named, i).ok_or_else(|| {
        let pname = params.get(i).copied().unwrap_or("");
        Error::at(format!("Missing argument ${pname} for {fname}()."), pos)
    })
}

/// Extract an `f64` from a number value (ignoring its unit).
pub(super) fn num(v: &Value, pos: Pos) -> Result<f64, Error> {
    match v {
        Value::Number(n) => Ok(n.value),
        other => Err(Error::at(
            format!("{} is not a number.", other.to_css(false)),
            pos,
        )),
    }
}

/// Extract a color value.
pub(super) fn as_color(v: &Value, pos: Pos) -> Result<Color, Error> {
    match v {
        Value::Color(c) => Ok(c.clone()),
        other => Err(Error::at(format!("{} is not a color.", other.to_css(false)), pos)),
    }
}

/// Extract an RGB channel value (`0..=255`), converting a percentage.
pub(super) fn channel(v: &Value, pos: Pos) -> Result<f64, Error> {
    match v {
        Value::Number(n) => {
            if n.unit == "%" {
                Ok((n.value / 100.0 * 255.0).clamp(0.0, 255.0))
            } else {
                Ok(n.value.clamp(0.0, 255.0))
            }
        }
        other => Err(Error::at(
            format!("{} is not a number.", other.to_css(false)),
            pos,
        )),
    }
}

/// Clamp a value to `[0, 1]` (e.g. alpha).
pub(super) fn clamp01(v: f64) -> f64 {
    v.clamp(0.0, 1.0)
}

/// Preserve an unknown function call verbatim as an unquoted CSS string.
fn plain_css_function(name: &str, pos_args: &[Value], named: &[(String, Value)]) -> Value {
    let mut parts: Vec<String> = pos_args.iter().map(|v| v.to_css(false)).collect();
    for (n, v) in named {
        parts.push(format!("${n}: {}", v.to_css(false)));
    }
    Value::Str(SassStr {
        text: format!("{name}({})", parts.join(", ")),
        quoted: false,
    })
}

// ---- built-in module system (`@use "sass:<mod>"`) ----------------------

/// Whether `module` names a built-in `sass:*` module this build supports.
pub(crate) fn is_module(module: &str) -> bool {
    matches!(
        module,
        "math" | "color" | "list" | "map" | "string" | "selector" | "meta"
    )
}

/// Translate a `(module, member)` pair to the global builtin name that
/// implements it, or `None` when the member is not a function this build can
/// dispatch to a global implementation. The global implementations are reused
/// verbatim — module members are just renamed views of them.
fn module_member_to_global(module: &str, member: &str) -> Option<&'static str> {
    // Names returned as `Some` must be real global builtins (so the dispatcher
    // finds them). Members handled specially (e.g. `math.div`) or unsupported
    // (color-space math, first-class functions) are deliberately absent.
    match module {
        "math" => match member {
            "abs" => Some("abs"),
            "ceil" => Some("ceil"),
            "floor" => Some("floor"),
            "round" => Some("round"),
            "max" => Some("max"),
            "min" => Some("min"),
            "clamp" => Some("clamp"),
            "sqrt" => Some("sqrt"),
            "pow" => Some("pow"),
            "exp" => Some("exp"),
            "log" => Some("log"),
            "sin" => Some("sin"),
            "cos" => Some("cos"),
            "tan" => Some("tan"),
            "asin" => Some("asin"),
            "acos" => Some("acos"),
            "atan" => Some("atan"),
            "atan2" => Some("atan2"),
            "hypot" => Some("hypot"),
            "sign" => Some("sign"),
            "percentage" => Some("percentage"),
            "unit" => Some("unit"),
            "is-unitless" => Some("unitless"),
            "compatible" => Some("comparable"),
            "random" => Some("random"),
            // `div` is implemented directly in `call_module`; the module
            // variables are resolved by `module_var`.
            _ => None,
        },
        "map" => match member {
            "get" => Some("map-get"),
            "merge" => Some("map-merge"),
            "remove" => Some("map-remove"),
            "keys" => Some("map-keys"),
            "values" => Some("map-values"),
            "has-key" => Some("map-has-key"),
            // `set`/`deep-merge`/`deep-remove` are module-only (no global alias
            // in dart-sass); they are dispatched directly in `call_module`.
            _ => None,
        },
        "string" => match member {
            "length" => Some("str-length"),
            "insert" => Some("str-insert"),
            "index" => Some("str-index"),
            "slice" => Some("str-slice"),
            "quote" => Some("quote"),
            "unquote" => Some("unquote"),
            "to-upper-case" => Some("to-upper-case"),
            "to-lower-case" => Some("to-lower-case"),
            // `unique-id`, `split` have no global equivalent here.
            _ => None,
        },
        "list" => match member {
            "length" => Some("length"),
            "nth" => Some("nth"),
            "set-nth" => Some("set-nth"),
            "append" => Some("append"),
            "join" => Some("join"),
            "zip" => Some("zip"),
            "index" => Some("index"),
            "separator" => Some("list-separator"),
            "is-bracketed" => Some("is-bracketed"),
            _ => None,
        },
        "selector" => match member {
            "nest" => Some("selector-nest"),
            "append" => Some("selector-append"),
            "extend" => Some("selector-extend"),
            "replace" => Some("selector-replace"),
            "unify" => Some("selector-unify"),
            "parse" => Some("selector-parse"),
            "is-superselector" => Some("is-superselector"),
            "simple-selectors" => Some("simple-selectors"),
            _ => None,
        },
        "color" => match member {
            // Legacy members map to the global color functions.
            "adjust" => Some("adjust-color"),
            "scale" => Some("scale-color"),
            "change" => Some("change-color"),
            "red" => Some("red"),
            "green" => Some("green"),
            "blue" => Some("blue"),
            "hue" => Some("hue"),
            "saturation" => Some("saturation"),
            "lightness" => Some("lightness"),
            "whiteness" => Some("whiteness"),
            "blackness" => Some("blackness"),
            "alpha" => Some("alpha"),
            "opacity" => Some("opacity"),
            "grayscale" => Some("grayscale"),
            "complement" => Some("complement"),
            "invert" => Some("invert"),
            "mix" => Some("mix"),
            "ie-hex-str" => Some("ie-hex-str"),
            // Modern CSS Color 4 color-space members. These dispatch to the
            // color-space-aware implementations in the color builtin family
            // under disambiguated global names.
            "space" => Some("color-space"),
            "channel" => Some("color-channel"),
            "to-space" => Some("color-to-space"),
            "is-legacy" => Some("color-is-legacy"),
            "is-missing" => Some("color-is-missing"),
            "is-in-gamut" => Some("color-is-in-gamut"),
            "is-powerless" => Some("color-is-powerless"),
            "to-gamut" => Some("color-to-gamut"),
            "same" => Some("color-same"),
            _ => None,
        },
        "meta" => match member {
            "type-of" => Some("type-of"),
            "inspect" => Some("inspect"),
            "feature-exists" => Some("feature-exists"),
            // `keywords`, `call`, `get-function`, the `*-exists`/`get-mixin`/
            // `apply`/`module-*` members need evaluator context or first-class
            // functions not available in this value-only dispatch — left
            // unsupported (Undefined function).
            _ => None,
        },
        _ => None,
    }
}

/// Whether `module` exposes `member` as a callable function (used by the
/// evaluator to resolve unprefixed `@use … as *` members).
pub(crate) fn module_has_member(module: &str, member: &str) -> bool {
    if module == "math" && member == "div" {
        return true;
    }
    if module == "map" && matches!(member, "set" | "deep-merge" | "deep-remove") {
        return true;
    }
    module_member_to_global(module, member).is_some()
}

/// Call a module member `module.member(args)`, dispatching to the reused global
/// implementation. An unknown member is "Undefined function." (dart-sass).
pub(crate) fn call_module(
    module: &str,
    member: &str,
    pos_args: &[Value],
    named: &[(String, Value)],
    pos: Pos,
) -> Result<Value, Error> {
    // `math.div(a, b)` is true (always-divide) division, unit-aware.
    if module == "math" && member == "div" {
        return math::module_div(pos_args, named, pos);
    }
    // `sass:map` members without a global alias (`set`, `deep-merge`,
    // `deep-remove`).
    if module == "map" {
        if let Some(r) = map::call_module_member(member, pos_args, named, pos) {
            return r;
        }
    }
    match module_member_to_global(module, member) {
        Some(global) => call(global, pos_args, named, pos),
        None => Err(Error::at("Undefined function.".to_string(), pos)),
    }
}

/// Resolve a built-in module variable (`math.$pi`, etc.). dart-sass exposes
/// these only on `sass:math`; an unknown member is "Undefined variable.".
pub(crate) fn module_var(module: &str, name: &str, pos: Pos) -> Result<Value, Error> {
    let number = |value: f64| {
        Ok(Value::Number(Number {
            value,
            unit: String::new(),
        }))
    };
    if module == "math" {
        return match name {
            "pi" => number(std::f64::consts::PI),
            "e" => number(std::f64::consts::E),
            "epsilon" => number(f64::EPSILON),
            "max-safe-integer" => number(9_007_199_254_740_991.0),
            "min-safe-integer" => number(-9_007_199_254_740_991.0),
            "max-number" => number(f64::MAX),
            "min-number" => number(f64::MIN_POSITIVE),
            _ => Err(Error::at("Undefined variable.".to_string(), pos)),
        };
    }
    Err(Error::at("Undefined variable.".to_string(), pos))
}
