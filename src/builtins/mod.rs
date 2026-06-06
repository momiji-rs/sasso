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
mod math;
mod meta;
mod string;

use crate::error::Error;
use crate::scanner::Pos;
use crate::value::{Color, SassStr, Value};

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
    if let Some(r) = list::try_call(name, pos_args, named, pos) {
        return r;
    }
    if let Some(r) = meta::try_call(name, pos_args, named, pos) {
        return r;
    }
    Ok(plain_css_function(name, pos_args, named))
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
