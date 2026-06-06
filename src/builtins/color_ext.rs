//! Extended color built-ins (`adjust-hue`, `saturate`, `desaturate`,
//! `complement`, `invert`, `grayscale`, `opacify`/`transparentize`, the
//! `hue`/`saturation`/`lightness` getters, …).
//!
//! Results are *computed* colors: they are built with [`Color::rgb`] or
//! [`Color::from_hsl`] and left with `repr = None`, so they serialize via
//! the normal `rgb()`/`rgba()`/hex rule, matching dart-sass.

use super::{arg, as_color, clamp01, num, require};
use crate::error::Error;
use crate::scanner::Pos;
use crate::value::{Color, Number, Value};

pub(super) fn try_call(
    name: &str,
    pos_args: &[Value],
    named: &[(String, Value)],
    pos: Pos,
) -> Option<Result<Value, Error>> {
    Some(match name {
        "adjust-hue" => fn_adjust_hue(pos_args, named, pos),
        "complement" => fn_complement(pos_args, named, pos),
        "invert" => fn_invert(pos_args, named, pos),
        "grayscale" => fn_grayscale(pos_args, named, pos),
        "saturate" => fn_saturate(pos_args, named, pos, 1.0),
        "desaturate" => fn_saturate(pos_args, named, pos, -1.0),
        "opacify" | "fade-in" => fn_fade(name, pos_args, named, pos, 1.0),
        "transparentize" | "fade-out" => fn_fade(name, pos_args, named, pos, -1.0),
        "hue" | "saturation" | "lightness" => fn_hsl_getter(name, pos_args, named, pos),
        _ => return None,
    })
}

/// `adjust-hue($color, $degrees)` — rotate the hue by `$degrees`.
fn fn_adjust_hue(pos_args: &[Value], named: &[(String, Value)], pos: Pos) -> Result<Value, Error> {
    let params = ["color", "degrees"];
    let c = as_color(require(&params, pos_args, named, 0, "adjust-hue", pos)?, pos)?;
    let degrees = num(require(&params, pos_args, named, 1, "adjust-hue", pos)?, pos)?;
    Ok(Value::Color(rotate_hue(&c, degrees)))
}

/// `complement($color)` — rotate the hue by 180 degrees.
fn fn_complement(pos_args: &[Value], named: &[(String, Value)], pos: Pos) -> Result<Value, Error> {
    let params = ["color"];
    let c = as_color(require(&params, pos_args, named, 0, "complement", pos)?, pos)?;
    Ok(Value::Color(rotate_hue(&c, 180.0)))
}

fn rotate_hue(c: &Color, degrees: f64) -> Color {
    let (h, s, l) = c.to_hsl();
    Color::from_hsl(h + degrees, s, l, c.a)
}

/// `invert($color, $weight: 100%)` — invert the RGB channels, then mix the
/// inverted color toward the original by `(100% - weight)`.
fn fn_invert(pos_args: &[Value], named: &[(String, Value)], pos: Pos) -> Result<Value, Error> {
    let params = ["color", "weight"];
    let c = as_color(require(&params, pos_args, named, 0, "invert", pos)?, pos)?;
    let weight = match arg(&params, pos_args, named, 1) {
        Some(v) => num(v, pos)?,
        None => 100.0,
    };
    let w = (weight / 100.0).clamp(0.0, 1.0);
    let inverted = Color::rgb(255.0 - c.r, 255.0 - c.g, 255.0 - c.b, c.a);
    if (w - 1.0).abs() < f64::EPSILON {
        return Ok(Value::Color(inverted));
    }
    // Weighted mix of the inverted color toward the original: the inverted
    // color carries weight `w`, the original `1 - w`. Both share the same
    // alpha, so this reduces to a straight per-channel lerp.
    let r = inverted.r * w + c.r * (1.0 - w);
    let g = inverted.g * w + c.g * (1.0 - w);
    let b = inverted.b * w + c.b * (1.0 - w);
    Ok(Value::Color(Color::rgb(r, g, b, c.a)))
}

/// `grayscale($color)` — set the HSL saturation to 0.
fn fn_grayscale(pos_args: &[Value], named: &[(String, Value)], pos: Pos) -> Result<Value, Error> {
    let params = ["color"];
    let c = as_color(require(&params, pos_args, named, 0, "grayscale", pos)?, pos)?;
    let (h, _s, l) = c.to_hsl();
    Ok(Value::Color(Color::from_hsl(h, 0.0, l, c.a)))
}

/// `saturate`/`desaturate` — adjust HSL saturation by `$amount` percent.
fn fn_saturate(pos_args: &[Value], named: &[(String, Value)], pos: Pos, sign: f64) -> Result<Value, Error> {
    let params = ["color", "amount"];
    let c = as_color(require(&params, pos_args, named, 0, "saturate", pos)?, pos)?;
    let amount = num(require(&params, pos_args, named, 1, "saturate", pos)?, pos)?;
    let (h, s, l) = c.to_hsl();
    let new_s = (s + sign * amount / 100.0).clamp(0.0, 1.0);
    Ok(Value::Color(Color::from_hsl(h, new_s, l, c.a)))
}

/// `opacify`/`fade-in` (`sign = +1`) and `transparentize`/`fade-out`
/// (`sign = -1`) — shift the alpha by `$amount`, clamped to `[0, 1]`.
fn fn_fade(
    name: &str,
    pos_args: &[Value],
    named: &[(String, Value)],
    pos: Pos,
    sign: f64,
) -> Result<Value, Error> {
    let params = ["color", "amount"];
    let c = as_color(require(&params, pos_args, named, 0, name, pos)?, pos)?;
    let amount = num(require(&params, pos_args, named, 1, name, pos)?, pos)?;
    let a = clamp01(c.a + sign * amount);
    Ok(Value::Color(Color::rgb(c.r, c.g, c.b, a)))
}

/// `hue` (deg), `saturation` (%), and `lightness` (%) getters.
fn fn_hsl_getter(
    name: &str,
    pos_args: &[Value],
    named: &[(String, Value)],
    pos: Pos,
) -> Result<Value, Error> {
    let params = ["color"];
    let c = as_color(require(&params, pos_args, named, 0, name, pos)?, pos)?;
    let (h, s, l) = c.to_hsl();
    let (value, unit) = match name {
        "hue" => (h, "deg"),
        "saturation" => (s * 100.0, "%"),
        _ => (l * 100.0, "%"),
    };
    Ok(Value::Number(Number {
        value,
        unit: unit.to_string(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value::Color;

    fn pos() -> Pos {
        Pos { line: 1, col: 1 }
    }

    fn col(s: &str) -> Value {
        let digits = s.strip_prefix('#').unwrap_or(s);
        Value::Color(Color::from_hex(digits).expect("valid hex"))
    }

    fn n(v: f64, unit: &str) -> Value {
        Value::Number(Number {
            value: v,
            unit: unit.to_string(),
        })
    }

    fn css(name: &str, args: &[Value]) -> String {
        try_call(name, args, &[], pos())
            .expect("name owned by color_ext family")
            .expect("no error")
            .to_css(false)
    }

    #[test]
    fn adjust_hue_and_complement() {
        assert_eq!(css("adjust-hue", &[col("#6b717f"), n(60.0, "deg")]), "#796b7f");
        assert_eq!(css("adjust-hue", &[col("#6b717f"), n(-60.0, "deg")]), "#6b7f79");
        assert_eq!(css("complement", &[col("#6b717f")]), "#7f796b");
    }

    #[test]
    fn invert_full_and_weighted() {
        assert_eq!(css("invert", &[col("#b37399")]), "#4c8c66");
        assert_eq!(
            css("invert", &[col("#b37399"), n(80.0, "%")]),
            "rgb(96.6, 135, 112.2)"
        );
        assert_eq!(
            css("invert", &[col("#b37399"), n(50.0, "%")]),
            "rgb(127.5, 127.5, 127.5)"
        );
    }

    #[test]
    fn grayscale_saturate_desaturate() {
        assert_eq!(css("grayscale", &[col("#6b717f")]), "#757575");
        assert_eq!(
            css("saturate", &[col("#cc6699"), n(30.0, "%")]),
            "rgb(234.6, 71.4, 153)"
        );
        assert_eq!(css("desaturate", &[col("#6b717f"), n(20.0, "%")]), "#757575");
    }

    #[test]
    fn alpha_shifts() {
        let rgba = Value::Color(Color::rgb(0.0, 51.0, 102.0, 0.7));
        assert_eq!(
            css("opacify", &[rgba.clone(), n(0.2, "")]),
            "rgba(0, 51, 102, 0.9)"
        );
        assert_eq!(
            css("fade-in", &[rgba.clone(), n(0.2, "")]),
            "rgba(0, 51, 102, 0.9)"
        );
        assert_eq!(
            css("transparentize", &[rgba.clone(), n(0.2, "")]),
            "rgba(0, 51, 102, 0.5)"
        );
        // Opacify past full opacity collapses to an opaque hex.
        assert_eq!(css("opacify", &[rgba, n(0.5, "")]), "#003366");
    }

    #[test]
    fn hsl_getters_units() {
        assert_eq!(css("hue", &[col("#6b717f")]), "222deg");
        assert_eq!(css("saturation", &[col("#6b717f")]), "8.547008547%");
        assert_eq!(css("lightness", &[col("#6b717f")]), "45.8823529412%");
    }

    #[test]
    fn unowned_returns_none() {
        assert!(try_call("not-a-color-fn", &[], &[], pos()).is_none());
    }
}
