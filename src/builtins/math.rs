//! Math built-in functions (global `floor`, `ceil`, `round`, `abs`).
//!
//! These operate on a single number and preserve its unit. `min`, `max`,
//! `clamp`, and `calc` never reach here — the parser preserves them
//! verbatim as CSS — and `percentage` belongs to the color family.
//!
//! Shared argument helpers live in the parent module:
//! `super::{arg, require, num, as_color, channel, clamp01}`. Return
//! `Some(Ok(..))`/`Some(Err(..))` for a name this family owns, or `None`
//! to let the next family try.

use crate::error::Error;
use crate::scanner::Pos;
use crate::value::{Number, Value};

pub(super) fn try_call(
    name: &str,
    pos_args: &[Value],
    named: &[(String, Value)],
    pos: Pos,
) -> Option<Result<Value, Error>> {
    let op: fn(f64) -> f64 = match name {
        "abs" => f64::abs,
        "ceil" => f64::ceil,
        "floor" => f64::floor,
        // dart-sass rounds half away from zero, matching `f64::round`.
        "round" => f64::round,
        _ => return None,
    };
    Some(unary(name, pos_args, named, pos, op))
}

/// Apply a unit-preserving unary numeric operation, requiring a number.
fn unary(
    fname: &str,
    pos_args: &[Value],
    named: &[(String, Value)],
    pos: Pos,
    op: fn(f64) -> f64,
) -> Result<Value, Error> {
    let v = super::require(&["number"], pos_args, named, 0, fname, pos)?;
    match v {
        Value::Number(n) => Ok(Value::Number(Number {
            value: op(n.value),
            unit: n.unit.clone(),
        })),
        other => Err(Error::at(
            format!("{} is not a number.", other.to_css(false)),
            pos,
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scanner::Pos;

    fn pos() -> Pos {
        Pos { line: 1, col: 1 }
    }

    fn n(value: f64, unit: &str) -> Value {
        Value::Number(Number {
            value,
            unit: unit.to_string(),
        })
    }

    fn call(name: &str, args: &[Value]) -> Value {
        try_call(name, args, &[], pos())
            .expect("name owned by math family")
            .expect("no error")
    }

    #[test]
    fn abs_keeps_unit_and_magnitude() {
        assert_eq!(call("abs", &[n(-3.0, "px")]).to_css(false), "3px");
        assert_eq!(call("abs", &[n(3.0, "")]).to_css(false), "3");
    }

    #[test]
    fn ceil_and_floor_keep_unit() {
        assert_eq!(call("ceil", &[n(4.2, "px")]).to_css(false), "5px");
        assert_eq!(call("floor", &[n(4.8, "%")]).to_css(false), "4%");
    }

    #[test]
    fn round_is_half_away_from_zero() {
        assert_eq!(call("round", &[n(2.5, "")]).to_css(false), "3");
        assert_eq!(call("round", &[n(-2.5, "deg")]).to_css(false), "-3deg");
        assert_eq!(call("round", &[n(0.5, "")]).to_css(false), "1");
    }

    #[test]
    fn rejects_non_numbers_and_unknown_names() {
        let err = try_call("abs", &[Value::Bool(true)], &[], pos());
        assert!(err.is_some());
        assert!(err.expect("some").is_err());
        assert!(try_call("sqrt", &[n(4.0, "")], &[], pos()).is_none());
    }
}
