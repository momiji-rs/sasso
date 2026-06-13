//! Core color builtins: `rgb`/`rgba`/`hsl`/`hsla`/`mix`, the legacy
//! `lighten`/`darken`, `percentage`, and the channel getters
//! `red`/`green`/`blue`/`alpha`.
//!
//! Split into focused submodules around the `try_call` dispatch (below), the
//! legacy color constructors ([`legacy`]), the modern `sass:color` module
//! members ([`modern`]), and the shared color-space math ([`math`]). This is a
//! pure organizational split: every `color::…` path external callers use is
//! preserved via the re-exports at the bottom of this file.

mod legacy;
mod math;
mod modern;

// Shared imports. These are private to `color`, but Rust makes them visible to
// the child submodules, which pull them in with `use super::*;`.
use super::color_ext::{computed, named_repr};
use super::{arg, as_color, channel, max_positional, num, require, require_legacy_color};
use crate::error::Error;
use crate::scanner::Pos;
use crate::value::{fmt_num, CalcNode, Color, ColorSpace, List, ListSep, ModernColor, Number, Value};

// Bring every sibling submodule item into this module's scope so the glue
// `try_call` below (and each submodule's `use super::*;`) can name them.
use legacy::*;
use math::*;
use modern::*;

// Re-exports preserving the external `color::…` API surface (the paths
// `crate::builtins::color::…` and `super::color::…` that `builtins::mod` and
// `builtins::color_ext` already use).
pub(crate) use legacy::call_module_member;
pub(crate) use math::{convert_modern, legacy_to_modern, make_modern_in, space_arg};
pub(crate) use modern::{
    grayscale_modern, invert_in_space, modify_in_space, modify_in_space_full, modify_in_space_opt, ModifyOp,
};

/// Whether a value is a "special" channel argument that cannot be evaluated
/// to a plain number — a `var()`/`env()`/`attr()` (an unquoted string holding
/// a CSS function) or a `calc()` (a [`Value::Calc`]). dart-sass does not error
/// on these; it preserves the whole color call verbatim, re-serialized from
/// the evaluated arguments.
fn is_special(v: &Value) -> bool {
    match v {
        Value::Calc(_) => true,
        Value::Str(s) => !s.quoted && s.text.contains('('),
        _ => false,
    }
}

/// Whether a value is specifically a `var(...)` reference (an unquoted string
/// whose first CSS function is `var`, case-insensitively). dart's legacy
/// two-argument `rgb($color, $alpha)` / `hsl` overloads short-circuit to a
/// verbatim passthrough only for `var()` — not `env()`, `calc()`, or any other
/// special — so the color-type check is suppressed exactly when a `var()` is
/// present.
fn is_var(v: &Value) -> bool {
    matches!(v, Value::Str(s) if !s.quoted && {
        let t = s.text.trim_start();
        t.len() >= 4 && t[..4].eq_ignore_ascii_case("var(")
    })
}

/// Like [`is_special`] but for the legacy `rgb`/`hsl` channels, where a
/// `calc()` that folds to a degenerate constant (`infinity`, `-infinity`,
/// `NaN`) is *not* special — dart-sass folds it to that floating point value
/// and computes/clamps the real channel rather than preserving the call.
fn is_special_legacy(v: &Value) -> bool {
    match v {
        Value::Calc(node) => degenerate_const(node).is_none(),
        other => is_special(other),
    }
}

/// The floating-point value of a degenerate `calc()` constant
/// (`calc(infinity)`, `calc(-infinity)`, `calc(NaN)`), or `None` for any other
/// calculation. dart-sass folds these constants to the corresponding `f64`.
fn degenerate_const(node: &CalcNode) -> Option<f64> {
    if let CalcNode::Str(s) = node {
        return match s.trim().to_ascii_lowercase().as_str() {
            "infinity" => Some(f64::INFINITY),
            "-infinity" => Some(f64::NEG_INFINITY),
            "nan" => Some(f64::NAN),
            _ => None,
        };
    }
    None
}

/// Whether a value is the `none` missing-channel keyword (an unquoted `none`).
pub(super) fn is_none_keyword(v: &Value) -> bool {
    matches!(v, Value::Str(s) if !s.quoted && s.text.eq_ignore_ascii_case("none"))
}

/// Re-serialize a special-value color call: `name(arg1, arg2, …)`, each
/// argument via `to_css(false)`, comma-joined (the form dart-sass normalizes
/// every legacy `rgb()`/`hsl()` special-value call to).
fn special_call(name: &str, args: &[&Value]) -> Value {
    let parts: Vec<String> = args.iter().map(|v| v.to_css(false)).collect();
    Value::Str(crate::value::SassStr {
        text: format!("{name}({})", parts.join(", ")).into(),
        quoted: false,
    })
}

/// Preserve a color call verbatim from a single channels value (space- or
/// slash-joined), matching dart-sass's "wrong channel count" passthrough
/// (`rgb(var(--foo) 2)` → `rgb(var(--foo) 2)`).
fn verbatim_call(name: &str, channels: &Value) -> Value {
    Value::Str(crate::value::SassStr {
        text: format!("{name}({})", channels.to_css(false)).into(),
        quoted: false,
    })
}

/// The name of the channel at index `i` for a legacy color-function error
/// message: a named channel for the first three (`red channel`,
/// `hue channel`, …), or `channel <N>` (1-based) for any overflow position,
/// matching dart-sass.
fn legacy_channel_name(names: &[&str], i: usize) -> String {
    match names.get(i) {
        Some(name) => format!("{name} channel"),
        None => format!("channel {}", i + 1),
    }
}

pub(super) fn try_call(
    name: &str,
    pos_args: &[Value],
    named: &[(String, Value)],
    pos: Pos,
) -> Option<Result<Value, Error>> {
    Some(match name {
        "rgb" | "rgba" => fn_rgb(pos_args, named, pos),
        "hsl" | "hsla" => fn_hsl(pos_args, named, pos),
        "hwb" => fn_hwb(pos_args, named, pos),
        "lab" | "lch" | "oklab" | "oklch" => fn_lab_family(name, pos_args, named, pos),
        "color" => fn_color(pos_args, named, pos),
        "mix" => fn_mix(pos_args, named, pos),
        "lighten" => fn_adjust_lightness(name, pos_args, named, pos, 1.0),
        "darken" => fn_adjust_lightness(name, pos_args, named, pos, -1.0),
        "percentage" => fn_percentage(pos_args, named, pos),
        "red" | "green" | "blue" => fn_channel(name, pos_args, named, pos),
        "alpha" => fn_alpha(pos_args, named, pos),
        _ => return try_call_modern(name, pos_args, named, pos),
    })
}

/// Read an alpha argument: a `%` is divided by 100, a unitless number is used
/// directly, and the result is clamped to `[0, 1]`. NaN clamps to 0. Any
/// other unit is an error (`Expected … to have unit "%" or no units.`).
fn alpha_value(v: &Value, pos: Pos) -> Result<f64, Error> {
    if let Some(c) = degenerate_value(v) {
        return Ok(clamp_alpha(c));
    }
    match v {
        Value::Number(num) => {
            let raw = if num.unit() == "%" {
                num.value / 100.0
            } else if num.is_unitless() {
                num.value
            } else {
                return Err(Error::at(
                    format!(
                        "$alpha: Expected {} to have unit \"%\" or no units.",
                        num.to_css(false)
                    ),
                    pos,
                ));
            };
            Ok(clamp_alpha(raw))
        }
        Value::Slash(num, _) => Ok(clamp_alpha(num.value)),
        other => Err(Error::at(
            format!("$alpha: {} is not a number.", other.to_css(false)),
            pos,
        )),
    }
}

/// Clamp an alpha value to `[0, 1]`, mapping NaN to 0 (matching dart-sass).
fn clamp_alpha(v: f64) -> f64 {
    if v.is_nan() {
        0.0
    } else {
        v.clamp(0.0, 1.0)
    }
}

/// Whether a value is a `calc()` that folds to a degenerate constant
/// (`infinity`, `-infinity`, `NaN`).
fn is_degenerate_calc(v: &Value) -> bool {
    degenerate_value(v).is_some()
}

/// The non-finite value of a degenerate channel: a non-finite number (the
/// usual form, since a fully-folded `calc()` unwraps to a number), or a
/// residual `calc()` constant.
fn degenerate_value(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) if !n.value.is_finite() => Some(n.value),
        Value::Calc(node) => match node {
            CalcNode::Number(n) if !n.value.is_finite() => Some(n.value),
            _ => degenerate_const(node),
        },
        _ => None,
    }
}

/// Serialize a list value wrapped in parentheses, as dart-sass does in its
/// channel-list error messages (`(1%, 2, 3)`, `(1% 2)`).
fn list_paren_css(v: &Value) -> String {
    format!("({})", v.to_css(false))
}
