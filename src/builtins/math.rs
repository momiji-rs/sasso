//! Math built-in functions.
//!
//! Unary unit-preserving ops (`abs`, `ceil`, `floor`, `round`), the
//! variadic bounds functions (`min`, `max`, `clamp`), powers/roots/logs
//! (`pow`, `sqrt`, `exp`, `log`), trigonometry (`sin`, `cos`, `tan`,
//! `asin`, `acos`, `atan`, `atan2`), `hypot`, `sign`, and the stepped
//! remainders (`rem`, `mod`). Each is unit-checked and byte-matched to
//! dart-sass.
//!
//! `min`/`max`/`clamp` reduce to a single number only when every argument
//! is a compatible-unit number; otherwise they fall back to a preserved CSS
//! `min()`/`max()`/`clamp()` call (so `min(1px, 2vw)` round-trips).
//!
//! Shared argument helpers live in the parent module. Return
//! `Some(Ok(..))`/`Some(Err(..))` for a name this family owns, or `None`
//! to let the next family try.

use crate::error::Error;
use crate::scanner::Pos;
use crate::value::{convert_factor, Number, SassStr, Value};

pub(super) fn try_call(
    name: &str,
    pos_args: &[Value],
    named: &[(String, Value)],
    pos: Pos,
) -> Option<Result<Value, Error>> {
    // Simple unit-preserving unary ops.
    if let Some(op) = unary_op(name) {
        return Some(unary(name, pos_args, named, pos, op));
    }
    match name {
        "min" => Some(min_max(name, pos_args, named, pos, true)),
        "max" => Some(min_max(name, pos_args, named, pos, false)),
        "clamp" => Some(clamp(pos_args, named, pos)),
        "sign" => Some(sign(pos_args, named, pos)),
        "pow" => Some(pow(pos_args, named, pos)),
        "sqrt" => Some(unitless_unary("sqrt", "number", pos_args, named, pos, f64::sqrt)),
        "exp" => Some(unitless_unary("exp", "number", pos_args, named, pos, f64::exp)),
        "log" => Some(log(pos_args, named, pos)),
        "hypot" => Some(hypot(pos_args, named, pos)),
        "sin" => Some(trig("sin", pos_args, named, pos, f64::sin)),
        "cos" => Some(trig("cos", pos_args, named, pos, f64::cos)),
        "tan" => Some(trig("tan", pos_args, named, pos, f64::tan)),
        "asin" => Some(inverse_trig("asin", pos_args, named, pos, f64::asin)),
        "acos" => Some(inverse_trig("acos", pos_args, named, pos, f64::acos)),
        "atan" => Some(inverse_trig("atan", pos_args, named, pos, f64::atan)),
        "atan2" => Some(atan2(pos_args, named, pos)),
        "rem" => Some(remainder("rem", pos_args, named, pos, true)),
        "mod" => Some(remainder("mod", pos_args, named, pos, false)),
        _ => None,
    }
}

fn unary_op(name: &str) -> Option<fn(f64) -> f64> {
    Some(match name {
        "abs" => f64::abs,
        "ceil" => f64::ceil,
        "floor" => f64::floor,
        // dart-sass rounds half away from zero, matching `f64::round`.
        "round" => f64::round,
        _ => return None,
    })
}

/// Apply a unit-preserving unary numeric operation, requiring a number.
fn unary(
    fname: &str,
    pos_args: &[Value],
    named: &[(String, Value)],
    pos: Pos,
    op: fn(f64) -> f64,
) -> Result<Value, Error> {
    let n = require_num(&["number"], pos_args, named, 0, fname, pos)?;
    Ok(num_value(Number {
        value: op(n.value),
        unit: n.unit.clone(),
    }))
}

/// `sign(x)`: -1, 0, or 1, preserving the operand's unit. `sign(0)` is `0`
/// (not `0px`); dart-sass keeps the unit on non-zero results.
fn sign(pos_args: &[Value], named: &[(String, Value)], pos: Pos) -> Result<Value, Error> {
    let n = require_num(&["number"], pos_args, named, 0, "sign", pos)?;
    let s = if n.value.is_nan() {
        f64::NAN
    } else if n.value > 0.0 {
        1.0
    } else if n.value < 0.0 {
        -1.0
    } else {
        0.0
    };
    let unit = if s == 0.0 { String::new() } else { n.unit.clone() };
    Ok(num_value(Number { value: s, unit }))
}

/// `pow(base, exp)`: both operands must be unitless; result is unitless.
fn pow(pos_args: &[Value], named: &[(String, Value)], pos: Pos) -> Result<Value, Error> {
    let base = require_num(&["base", "exponent"], pos_args, named, 0, "pow", pos)?;
    let exp = require_num(&["base", "exponent"], pos_args, named, 1, "pow", pos)?;
    no_unit(&base, pos)?;
    no_unit(&exp, pos)?;
    Ok(unitless(base.value.powf(exp.value)))
}

/// A unitless unary op (`sqrt`, `exp`): the argument must have no units and
/// the result is unitless.
fn unitless_unary(
    fname: &str,
    param: &str,
    pos_args: &[Value],
    named: &[(String, Value)],
    pos: Pos,
    op: fn(f64) -> f64,
) -> Result<Value, Error> {
    let n = require_num(&[param], pos_args, named, 0, fname, pos)?;
    no_unit(&n, pos)?;
    Ok(unitless(op(n.value)))
}

/// `log(x)` (natural) or `log(x, base)`. Both operands must be unitless.
fn log(pos_args: &[Value], named: &[(String, Value)], pos: Pos) -> Result<Value, Error> {
    let params = &["number", "base"];
    let x = require_num(params, pos_args, named, 0, "log", pos)?;
    no_unit(&x, pos)?;
    match super::arg(params, pos_args, named, 1) {
        Some(b) => {
            let b = as_num(b, pos)?;
            no_unit(&b, pos)?;
            Ok(unitless(x.value.ln() / b.value.ln()))
        }
        None => Ok(unitless(x.value.ln())),
    }
}

/// `hypot(a, b, ...)`: sqrt of the sum of squares, with unit coercion onto
/// the first argument's unit (`hypot(3px, 4cm)` converts cm to px).
fn hypot(pos_args: &[Value], named: &[(String, Value)], pos: Pos) -> Result<Value, Error> {
    let nums = collect_nums("hypot", pos_args, named, pos)?;
    let first = match nums.first() {
        Some(n) => n.clone(),
        None => return Err(Error::at("At least one argument must be passed.", pos)),
    };
    let mut sum = 0.0;
    for n in &nums {
        let v = coerce_to(n, &first, pos)?;
        sum += v * v;
    }
    Ok(num_value(Number {
        value: sum.sqrt(),
        unit: first.unit.clone(),
    }))
}

/// `sin`/`cos`/`tan`: accept an angle (`deg`/`grad`/`rad`/`turn`) or a
/// unitless number treated as radians; return a unitless number.
fn trig(
    fname: &str,
    pos_args: &[Value],
    named: &[(String, Value)],
    pos: Pos,
    op: fn(f64) -> f64,
) -> Result<Value, Error> {
    let n = require_num(&["number"], pos_args, named, 0, fname, pos)?;
    let radians = angle_to_radians(&n, pos)?;
    Ok(unitless(op(radians)))
}

/// `asin`/`acos`/`atan`: the argument must be unitless; the result is in
/// degrees.
fn inverse_trig(
    fname: &str,
    pos_args: &[Value],
    named: &[(String, Value)],
    pos: Pos,
    op: fn(f64) -> f64,
) -> Result<Value, Error> {
    let n = require_num(&["number"], pos_args, named, 0, fname, pos)?;
    no_unit(&n, pos)?;
    Ok(degrees(op(n.value).to_degrees()))
}

/// `atan2(y, x)`: the two-argument arctangent, returned in degrees. The
/// operands must share a unit dimension (they are coerced together).
fn atan2(pos_args: &[Value], named: &[(String, Value)], pos: Pos) -> Result<Value, Error> {
    let params = &["y", "x"];
    let y = require_num(params, pos_args, named, 0, "atan2", pos)?;
    let x = require_num(params, pos_args, named, 1, "atan2", pos)?;
    let xv = coerce_to(&x, &y, pos)?;
    Ok(degrees(y.value.atan2(xv).to_degrees()))
}

/// `rem(a, b)` (truncated, sign of dividend) or `mod(a, b)` (floored, sign
/// of divisor). The divisor is coerced into the dividend's unit.
fn remainder(
    fname: &str,
    pos_args: &[Value],
    named: &[(String, Value)],
    pos: Pos,
    truncated: bool,
) -> Result<Value, Error> {
    let params = &["dividend", "modulus"];
    let a = require_num(params, pos_args, named, 0, fname, pos)?;
    let b = require_num(params, pos_args, named, 1, fname, pos)?;
    let bv = coerce_to(&b, &a, pos)?;
    let value = if bv == 0.0 {
        f64::NAN
    } else if truncated {
        a.value % bv
    } else {
        a.value - bv * (a.value / bv).floor()
    };
    Ok(num_value(Number {
        value,
        unit: a.unit.clone(),
    }))
}

/// `min`/`max`: reduce all-numeric, compatible-unit arguments to the
/// smallest/largest (keeping the winning argument's own unit). If any
/// argument is a non-number or any pair of units is incompatible, fall back
/// to a preserved CSS `min()`/`max()` call over the evaluated arguments.
fn min_max(
    fname: &str,
    pos_args: &[Value],
    named: &[(String, Value)],
    pos: Pos,
    is_min: bool,
) -> Result<Value, Error> {
    let args: Vec<Value> = all_args(pos_args, named)
        .into_iter()
        .map(normalize_const)
        .collect();
    if args.is_empty() {
        return Err(Error::at("Missing argument.", pos));
    }
    match reduce_min_max(&args, is_min) {
        Some(n) => Ok(num_value(n)),
        None => Ok(preserved_call(fname, &args)),
    }
}

/// `clamp(min, value, max)`: when all three are compatible-unit numbers,
/// returns `max(min, min(value, max))`; otherwise preserves the CSS call.
fn clamp(pos_args: &[Value], named: &[(String, Value)], pos: Pos) -> Result<Value, Error> {
    let args: Vec<Value> = all_args(pos_args, named)
        .into_iter()
        .map(normalize_const)
        .collect();
    if args.len() != 3 {
        return Err(Error::at(
            format!("3 arguments required, but {} were passed.", args.len()),
            pos,
        ));
    }
    let (lo, val, hi) = match (&args[0], &args[1], &args[2]) {
        (Value::Number(a), Value::Number(b), Value::Number(c)) => (a, b, c),
        _ => return Ok(preserved_call("clamp", &args)),
    };
    // Coerce `value` and `hi` into `lo`'s unit; bail to preserved form if
    // any unit pair is incompatible.
    let val_v = match try_coerce(val, lo) {
        Some(v) => v,
        None => return Ok(preserved_call("clamp", &args)),
    };
    let hi_v = match try_coerce(hi, lo) {
        Some(v) => v,
        None => return Ok(preserved_call("clamp", &args)),
    };
    let clamped = lo.value.max(val_v.min(hi_v));
    Ok(num_value(Number {
        value: clamped,
        unit: lo.unit.clone(),
    }))
}

// ---- shared helpers --------------------------------------------------

/// All positional arguments followed by all named-argument values, in order.
fn all_args(pos_args: &[Value], named: &[(String, Value)]) -> Vec<Value> {
    let mut v: Vec<Value> = pos_args.to_vec();
    v.extend(named.iter().map(|(_, val)| val.clone()));
    v
}

/// Replace a bare calc-constant string (`infinity`/`-infinity`/`NaN`/`pi`/`e`)
/// with its numeric value; pass any other value through unchanged. Used by
/// `min`/`max`/`clamp`, which accept the constants while still preserving a
/// genuine non-number argument as a CSS call.
fn normalize_const(v: Value) -> Value {
    if let Value::Str(s) = &v {
        if !s.quoted {
            if let Some(n) = const_number(&s.text) {
                return Value::Number(n);
            }
        }
    }
    v
}

/// Reduce a list of values to the min/max number, or `None` if any argument
/// is not a number or any unit pair is incompatible. The result keeps the
/// winning argument's own unit, matching dart-sass.
fn reduce_min_max(args: &[Value], is_min: bool) -> Option<Number> {
    let mut best: Option<Number> = None;
    for v in args {
        let n = match v {
            Value::Number(n) => n.clone(),
            _ => return None,
        };
        best = Some(match best {
            None => n,
            Some(cur) => {
                // Compare `n` in `cur`'s unit; pick whichever wins, keeping
                // the winner's authored unit.
                let nv = try_coerce(&n, &cur)?;
                let pick_n = if is_min { nv < cur.value } else { nv > cur.value };
                if pick_n {
                    n
                } else {
                    cur
                }
            }
        });
    }
    best
}

/// Convert `n` into `target`'s unit, returning the converted scalar, or
/// `None` if their units are incompatible. A unitless operand keeps its
/// value and adopts the comparison silently.
fn try_coerce(n: &Number, target: &Number) -> Option<f64> {
    if n.unit.eq_ignore_ascii_case(&target.unit) || n.unit.is_empty() || target.unit.is_empty() {
        return Some(n.value);
    }
    convert_factor(&n.unit, &target.unit).map(|f| n.value * f)
}

/// Like [`try_coerce`] but errors (dart-sass "<a> and <b> are
/// incompatible.") when the units cannot be combined. Used by `hypot`,
/// `atan2`, `rem`, and `mod`, which — like `calc()` — reject a unitless/real
/// mix as well as cross-dimension units.
fn coerce_to(n: &Number, target: &Number, pos: Pos) -> Result<f64, Error> {
    if n.unit.eq_ignore_ascii_case(&target.unit) {
        return Ok(n.value);
    }
    if n.unit.is_empty() != target.unit.is_empty() {
        return Err(incompatible(n, target, pos));
    }
    if n.unit.is_empty() && target.unit.is_empty() {
        return Ok(n.value);
    }
    match convert_factor(&n.unit, &target.unit) {
        Some(f) => Ok(n.value * f),
        None => Err(incompatible(n, target, pos)),
    }
}

/// Convert an angle number to radians, accepting `deg`/`grad`/`rad`/`turn`
/// or a unitless value (already radians). Errors on any other unit.
fn angle_to_radians(n: &Number, pos: Pos) -> Result<f64, Error> {
    use std::f64::consts::PI;
    let deg = match n.unit.to_ascii_lowercase().as_str() {
        // unitless and radians are already in radians.
        "" | "rad" => return Ok(n.value),
        "deg" => n.value,
        "grad" => n.value * 9.0 / 10.0,
        "turn" => n.value * 360.0,
        _ => return Err(Error::at(format!("{} is not an angle.", n.to_css(false)), pos)),
    };
    Ok(deg * PI / 180.0)
}

/// Collect every argument as a number, erroring on the first non-number.
fn collect_nums(
    fname: &str,
    pos_args: &[Value],
    named: &[(String, Value)],
    pos: Pos,
) -> Result<Vec<Number>, Error> {
    let args = all_args(pos_args, named);
    if args.is_empty() {
        return Err(Error::at(format!("Missing argument for {fname}()."), pos));
    }
    let mut out = Vec::with_capacity(args.len());
    for v in &args {
        out.push(as_num(v, pos)?);
    }
    Ok(out)
}

/// Interpret an unquoted-string calc constant (`infinity`, `-infinity`,
/// `nan`, `pi`, `e`, case-insensitive) as the matching unitless number. These
/// are the same constants `calc()` recognizes; dart-sass's math functions
/// accept them as numeric inputs too.
fn const_number(text: &str) -> Option<Number> {
    let value = match text.to_ascii_lowercase().as_str() {
        "infinity" => f64::INFINITY,
        "-infinity" => f64::NEG_INFINITY,
        "nan" => f64::NAN,
        "pi" => std::f64::consts::PI,
        "e" => std::f64::consts::E,
        _ => return None,
    };
    Some(Number {
        value,
        unit: String::new(),
    })
}

/// Convert a value to a `Number`, also accepting the bare calc constants
/// (`infinity`/`-infinity`/`NaN`/`pi`/`e`); erroring on any other non-number.
fn as_num(v: &Value, pos: Pos) -> Result<Number, Error> {
    match v {
        Value::Number(n) => Ok(n.clone()),
        Value::Str(s) if !s.quoted => match const_number(&s.text) {
            Some(n) => Ok(n),
            None => Err(Error::at(format!("{} is not a number.", s.text), pos)),
        },
        other => Err(Error::at(
            format!("{} is not a number.", other.to_css(false)),
            pos,
        )),
    }
}

/// Fetch the argument at `i` as a `Number`, erroring on a missing or
/// non-number argument.
fn require_num(
    params: &[&str],
    pos_args: &[Value],
    named: &[(String, Value)],
    i: usize,
    fname: &str,
    pos: Pos,
) -> Result<Number, Error> {
    let v = super::require(params, pos_args, named, i, fname, pos)?;
    as_num(v, pos)
}

/// Wrap a result number as a value: a finite number stays a plain `Number`,
/// while a non-finite result (infinity/NaN) becomes a `calc()` so it
/// serializes as `calc(infinity)` / `calc(NaN)` / `calc(-infinity)`, matching
/// dart-sass.
fn num_value(n: Number) -> Value {
    if n.value.is_finite() {
        Value::Number(n)
    } else {
        Value::Calc(crate::value::CalcNode::Number(n))
    }
}

/// Ensure a number is unitless, erroring with dart-sass's wording.
fn no_unit(n: &Number, pos: Pos) -> Result<(), Error> {
    if n.unit.is_empty() {
        Ok(())
    } else {
        Err(Error::at(
            format!("Expected {} to have no units.", n.to_css(false)),
            pos,
        ))
    }
}

fn unitless(value: f64) -> Value {
    num_value(Number {
        value,
        unit: String::new(),
    })
}

fn degrees(value: f64) -> Value {
    num_value(Number {
        value,
        unit: "deg".to_string(),
    })
}

fn incompatible(a: &Number, b: &Number, pos: Pos) -> Error {
    Error::at(
        format!("{} and {} are incompatible.", a.to_css(false), b.to_css(false)),
        pos,
    )
}

/// Build the preserved CSS form `name(arg, arg, ...)` over evaluated
/// arguments, matching dart-sass's serialization (comma + space separated).
fn preserved_call(name: &str, args: &[Value]) -> Value {
    let parts: Vec<String> = args.iter().map(|v| v.to_css(false)).collect();
    Value::Str(SassStr {
        text: format!("{name}({})", parts.join(", ")),
        quoted: false,
    })
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

    fn err(name: &str, args: &[Value]) -> bool {
        try_call(name, args, &[], pos()).expect("name owned").is_err()
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
    fn min_max_pick_winning_argument_unit() {
        assert_eq!(call("min", &[n(1.0, "px"), n(2.0, "px")]).to_css(false), "1px");
        assert_eq!(
            call("max", &[n(1.0, ""), n(2.0, ""), n(3.0, "")]).to_css(false),
            "3"
        );
        // min(1in, 2cm): 1in == 2.54cm > 2cm, so 2cm wins (keeps cm).
        assert_eq!(call("min", &[n(1.0, "in"), n(2.0, "cm")]).to_css(false), "2cm");
        // single arg returns itself even with an unknown unit.
        assert_eq!(call("min", &[n(1.0, "vw")]).to_css(false), "1vw");
    }

    #[test]
    fn min_max_preserve_css_form_on_incompatible() {
        assert_eq!(
            call("min", &[n(1.0, "px"), n(2.0, "vw")]).to_css(false),
            "min(1px, 2vw)"
        );
        // a non-number argument also forces the preserved form.
        let var = Value::Str(SassStr {
            text: "var(--x)".into(),
            quoted: false,
        });
        assert_eq!(
            call("min", &[n(1.0, "px"), var]).to_css(false),
            "min(1px, var(--x))"
        );
    }

    #[test]
    fn clamp_orders_min_value_max() {
        assert_eq!(
            call("clamp", &[n(1.0, "px"), n(5.0, "px"), n(3.0, "px")]).to_css(false),
            "3px"
        );
        assert_eq!(
            call("clamp", &[n(1.0, "px"), n(0.0, "px"), n(3.0, "px")]).to_css(false),
            "1px"
        );
        // unresolvable middle arg preserves the CSS call.
        let var = Value::Str(SassStr {
            text: "var(--x)".into(),
            quoted: false,
        });
        assert_eq!(
            call("clamp", &[n(1.0, "px"), var, n(3.0, "px")]).to_css(false),
            "clamp(1px, var(--x), 3px)"
        );
    }

    #[test]
    fn pow_sqrt_exp_log_are_unitless() {
        assert_eq!(call("pow", &[n(2.0, ""), n(3.0, "")]).to_css(false), "8");
        assert_eq!(call("sqrt", &[n(4.0, "")]).to_css(false), "2");
        assert_eq!(call("exp", &[n(0.0, "")]).to_css(false), "1");
        assert_eq!(call("log", &[n(8.0, ""), n(2.0, "")]).to_css(false), "3");
        assert!(err("sqrt", &[n(4.0, "px")]));
        assert!(err("pow", &[n(2.0, "px"), n(2.0, "")]));
    }

    #[test]
    fn trig_accepts_angles_and_returns_unitless() {
        assert_eq!(call("sin", &[n(30.0, "deg")]).to_css(false), "0.5");
        assert_eq!(call("cos", &[n(0.0, "")]).to_css(false), "1");
        assert_eq!(call("tan", &[n(45.0, "deg")]).to_css(false), "1");
        assert_eq!(call("sin", &[n(100.0, "grad")]).to_css(false), "1");
        assert_eq!(call("sin", &[n(0.25, "turn")]).to_css(false), "1");
        assert!(err("sin", &[n(1.0, "px")]));
    }

    #[test]
    fn inverse_trig_returns_degrees() {
        assert_eq!(call("asin", &[n(0.5, "")]).to_css(false), "30deg");
        assert_eq!(call("acos", &[n(1.0, "")]).to_css(false), "0deg");
        assert_eq!(call("atan", &[n(1.0, "")]).to_css(false), "45deg");
        assert_eq!(call("atan2", &[n(1.0, ""), n(1.0, "")]).to_css(false), "45deg");
    }

    #[test]
    fn sign_keeps_unit_except_zero() {
        assert_eq!(call("sign", &[n(-5.0, "px")]).to_css(false), "-1px");
        assert_eq!(call("sign", &[n(0.0, "px")]).to_css(false), "0");
        assert_eq!(call("sign", &[n(3.0, "")]).to_css(false), "1");
    }

    #[test]
    fn hypot_converts_onto_first_unit() {
        assert_eq!(call("hypot", &[n(3.0, ""), n(4.0, "")]).to_css(false), "5");
        assert_eq!(call("hypot", &[n(3.0, "px"), n(4.0, "px")]).to_css(false), "5px");
        assert!(err("hypot", &[n(3.0, "px"), n(4.0, "")]));
    }

    #[test]
    fn rem_and_mod_signs() {
        assert_eq!(call("rem", &[n(10.0, ""), n(3.0, "")]).to_css(false), "1");
        assert_eq!(call("rem", &[n(-10.0, ""), n(3.0, "")]).to_css(false), "-1");
        assert_eq!(call("mod", &[n(-10.0, ""), n(3.0, "")]).to_css(false), "2");
        // 10px rem 3pt -> 3pt == 4px, 10 % 4 == 2 -> 2px.
        assert_eq!(call("rem", &[n(10.0, "px"), n(3.0, "pt")]).to_css(false), "2px");
        assert!(err("rem", &[n(10.0, "px"), n(3.0, "")]));
    }

    #[test]
    fn rejects_non_numbers_and_unknown_names() {
        let e = try_call("abs", &[Value::Bool(true)], &[], pos());
        assert!(e.is_some());
        assert!(e.expect("some").is_err());
        assert!(try_call("definitely-not-math", &[n(4.0, "")], &[], pos()).is_none());
    }
}
