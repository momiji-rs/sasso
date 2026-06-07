//! Core color builtins: `rgb`/`rgba`/`hsl`/`hsla`/`mix`, the legacy
//! `lighten`/`darken`, `percentage`, and the channel getters
//! `red`/`green`/`blue`/`alpha`.

use super::color_ext::{computed, named_repr};
use super::{arg, as_color, channel, num, require};
use crate::error::Error;
use crate::scanner::Pos;
use crate::value::{fmt_num, Color, ListSep, Number, Value};

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

/// Whether a value is the `none` missing-channel keyword (an unquoted `none`).
fn is_none_keyword(v: &Value) -> bool {
    matches!(v, Value::Str(s) if !s.quoted && s.text.eq_ignore_ascii_case("none"))
}

/// Re-serialize a special-value color call: `name(arg1, arg2, …)`, each
/// argument via `to_css(false)`, comma-joined (the form dart-sass normalizes
/// every legacy `rgb()`/`hsl()` special-value call to).
fn special_call(name: &str, args: &[&Value]) -> Value {
    let parts: Vec<String> = args.iter().map(|v| v.to_css(false)).collect();
    Value::Str(crate::value::SassStr {
        text: format!("{name}({})", parts.join(", ")),
        quoted: false,
    })
}

/// Preserve a color call verbatim from a single channels value (space- or
/// slash-joined), matching dart-sass's "wrong channel count" passthrough
/// (`rgb(var(--foo) 2)` → `rgb(var(--foo) 2)`).
fn verbatim_call(name: &str, channels: &Value) -> Value {
    Value::Str(crate::value::SassStr {
        text: format!("{name}({})", channels.to_css(false)),
        quoted: false,
    })
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
        "mix" => fn_mix(pos_args, named, pos),
        "lighten" => fn_adjust_lightness(name, pos_args, named, pos, 1.0),
        "darken" => fn_adjust_lightness(name, pos_args, named, pos, -1.0),
        "percentage" => fn_percentage(pos_args, named, pos),
        "red" | "green" | "blue" => fn_channel(name, pos_args, named, pos),
        "alpha" => fn_alpha(pos_args, named, pos),
        _ => return None,
    })
}

fn fn_rgb(pos_args: &[Value], named: &[(String, Value)], pos: Pos) -> Result<Value, Error> {
    let params = ["red", "green", "blue", "alpha"];
    let n = pos_args.len() + named.len();
    if n > 4 {
        return Err(Error::at(
            format!("Only 4 arguments allowed, but {n} were passed."),
            pos,
        ));
    }
    // rgb($color, $alpha) two-argument form: a concrete color and an alpha.
    // The result is a computed color (serialized by name/hex/rgba), not the
    // literal rgb() spelling. When either argument is a special value the
    // call is preserved verbatim instead.
    if pos_args.len() == 2 {
        if let Value::Color(c) = &pos_args[0] {
            if is_special(&pos_args[1]) {
                // rgb(blue, calc(0.4)) → rgb(0, 0, 255, calc(0.4)).
                let r = Value::Number(int_num(c.r));
                let g = Value::Number(int_num(c.g));
                let b = Value::Number(int_num(c.b));
                return Ok(special_call("rgb", &[&r, &g, &b, &pos_args[1]]));
            }
            let a = alpha_value(&pos_args[1], pos)?;
            return Ok(Value::Color(computed(c.r, c.g, c.b, a)));
        }
    }
    // Otherwise gather the channel list and an optional alpha.
    let channels = Channels::collect("rgb", &params, pos_args, named, pos)?;
    if let Some(verbatim) = channels.special_passthrough("rgb") {
        return Ok(verbatim);
    }
    let Channels { comps, alpha, .. } = channels;
    let r = rgb_channel(&comps[0], pos)?;
    let g = rgb_channel(&comps[1], pos)?;
    let b = rgb_channel(&comps[2], pos)?;
    let a = match &alpha {
        Some(v) => alpha_value(v, pos)?,
        None => 1.0,
    };
    let mut c = Color::rgb(r, g, b, a);
    // rgb()/rgba() literals keep their function representation, matching
    // dart-sass (the channels form never collapses to a hex spelling).
    c.repr = Some(rgb_repr(r, g, b, a));
    Ok(Value::Color(c))
}

/// A whole number as a unitless [`Number`] (for re-serializing a color's
/// channels in a special-value passthrough call).
fn int_num(v: f64) -> Number {
    Number {
        value: v.round(),
        unit: String::new(),
    }
}

/// Read an alpha argument: a `%` is divided by 100, a unitless number is used
/// directly, and the result is clamped to `[0, 1]`. NaN clamps to 0. Any
/// other unit is an error (`Expected … to have unit "%" or no units.`).
fn alpha_value(v: &Value, pos: Pos) -> Result<f64, Error> {
    match v {
        Value::Number(num) => {
            let raw = if num.unit == "%" {
                num.value / 100.0
            } else if num.unit.is_empty() {
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

/// Read an rgb channel value (`0..=255`): a `%` is taken as a fraction of
/// 255. NaN maps to 0, `±Infinity` clamp to the bounds. Delegates to the
/// shared [`channel`] helper for the finite case, then normalizes NaN.
fn rgb_channel(v: &Value, pos: Pos) -> Result<f64, Error> {
    if let Value::Slash(num, _) = v {
        return Ok(clamp_finite(num.value, 0.0, 255.0));
    }
    if let Value::Number(num) = v {
        if num.value.is_nan() {
            return Ok(0.0);
        }
    }
    channel(v, pos)
}

/// Clamp to `[lo, hi]`, mapping NaN to `lo` (matching dart-sass's channel
/// clamping, where `calc(NaN)` becomes the lower bound).
fn clamp_finite(v: f64, lo: f64, hi: f64) -> f64 {
    if v.is_nan() {
        lo
    } else {
        v.clamp(lo, hi)
    }
}

/// The parsed channel arguments of a legacy color function, normalized from
/// either the three-positional form (`rgb(1, 2, 3)`) or the one-argument
/// channels form (`rgb(1 2 3)`, `rgb(1 2 3 / 0.5)`).
struct Channels {
    /// The (up to three) channel component values.
    comps: Vec<Value>,
    /// The alpha value, if one was supplied.
    alpha: Option<Value>,
    /// The original single channels value when this came from the one-argument
    /// form, used to re-serialize a verbatim passthrough.
    single: Option<Value>,
}

impl Channels {
    /// Gather the channel components and optional alpha. The three- and
    /// four-positional forms map directly; a single positional/named argument
    /// is treated as a channels list, splitting a trailing slash-division
    /// (`1 2 3 / 0.5`) into components plus alpha.
    fn collect(
        fname: &str,
        params: &[&str],
        pos_args: &[Value],
        named: &[(String, Value)],
        pos: Pos,
    ) -> Result<Channels, Error> {
        let count = pos_args.len() + named.len();
        if count >= 3 {
            let c0 = require(params, pos_args, named, 0, fname, pos)?.clone();
            let c1 = require(params, pos_args, named, 1, fname, pos)?.clone();
            let c2 = require(params, pos_args, named, 2, fname, pos)?.clone();
            let alpha = arg(params, pos_args, named, 3).cloned();
            return Ok(Channels {
                comps: vec![c0, c1, c2],
                alpha,
                single: None,
            });
        }
        // One argument: a channels value. A second argument (when present) is
        // an explicit alpha for a special-value channels list.
        let channels = require(params, pos_args, named, 0, fname, pos)?.clone();
        let extra_alpha = arg(params, pos_args, named, 1).cloned();
        let (comps, mut alpha) = split_channels(&channels);
        if extra_alpha.is_some() {
            alpha = extra_alpha;
        }
        Ok(Channels {
            comps,
            alpha,
            single: Some(channels),
        })
    }

    /// If these channels contain a special value (`var()`, `calc()`, …) or a
    /// `none` keyword, return the re-serialized passthrough call dart-sass
    /// would emit; otherwise `None` (the channels are all plain numbers and a
    /// real color should be computed).
    fn special_passthrough(&self, name: &str) -> Option<Value> {
        let comps_special = self.comps.iter().any(is_special);
        let comps_none = self.comps.iter().any(is_none_keyword);
        let alpha_special = self.alpha.as_ref().is_some_and(is_special);
        let alpha_none = self.alpha.as_ref().is_some_and(is_none_keyword);
        let has_special = comps_special || alpha_special;
        let has_none = comps_none || alpha_none;
        if !has_special && !has_none {
            return None;
        }
        // Exactly three components with a special function (and no bare `none`)
        // normalize to a comma-joined call. A different component count, or a
        // `none` keyword, is preserved verbatim in its original spelling.
        if self.comps.len() == 3 && has_special && !has_none {
            let mut args: Vec<&Value> = self.comps.iter().collect();
            if let Some(a) = &self.alpha {
                args.push(a);
            }
            return Some(special_call(name, &args));
        }
        // Verbatim: prefer the single channels value (preserves the space/slash
        // spelling); fall back to reconstructing from the positional args.
        if let Some(single) = &self.single {
            if self.alpha.is_none() {
                return Some(verbatim_call(name, single));
            }
        }
        let mut args: Vec<&Value> = self.comps.iter().collect();
        if let Some(a) = &self.alpha {
            args.push(a);
        }
        Some(special_call(name, &args))
    }
}

/// Split a one-argument channels value into its components and optional alpha.
/// A space list contributes its items; a trailing slash-division on the last
/// item (`1 2 3 / 0.5`, parsed as `[1, 2, 3/0.5]`) peels off the alpha.
fn split_channels(channels: &Value) -> (Vec<Value>, Option<Value>) {
    match channels {
        Value::List(l) if l.sep == ListSep::Space => {
            let mut items: Vec<Value> = l.items.clone();
            // A trailing `n / a` slash-division shows up as a `Slash` whose
            // textual spelling contains `/`; recover the channel and alpha
            // (each may carry a unit, e.g. `50%/0.4`).
            if let Some(Value::Slash(_, repr)) = items.last() {
                if let Some((lhs, rhs)) = repr.split_once('/') {
                    if let (Some(last), Some(alpha)) = (parse_number_token(lhs), parse_number_token(rhs)) {
                        items.pop();
                        items.push(Value::Number(last));
                        return (items, Some(Value::Number(alpha)));
                    }
                }
            }
            (items, None)
        }
        other => (vec![other.clone()], None),
    }
}

/// Parse a CSS number token that may carry a unit (`"3"`, `"0.5"`, `"50%"`)
/// into a [`Number`]. Returns `None` for anything not of that shape.
fn parse_number_token(s: &str) -> Option<Number> {
    let s = s.trim();
    let split = s
        .char_indices()
        .find(|(_, c)| !(c.is_ascii_digit() || matches!(c, '.' | '-' | '+' | 'e' | 'E')))
        .map(|(i, _)| i)
        .unwrap_or(s.len());
    let (num_part, unit) = s.split_at(split);
    let value = num_part.parse::<f64>().ok()?;
    Some(Number {
        value,
        unit: unit.to_string(),
    })
}

pub(super) fn rgb_repr(r: f64, g: f64, b: f64, a: f64) -> String {
    if (a - 1.0).abs() < f64::EPSILON {
        format!(
            "rgb({}, {}, {})",
            fmt_num(r, false),
            fmt_num(g, false),
            fmt_num(b, false)
        )
    } else {
        format!(
            "rgba({}, {}, {}, {})",
            fmt_num(r, false),
            fmt_num(g, false),
            fmt_num(b, false),
            fmt_num(a, false)
        )
    }
}

fn fn_hsl(pos_args: &[Value], named: &[(String, Value)], pos: Pos) -> Result<Value, Error> {
    let params = ["hue", "saturation", "lightness", "alpha"];
    let n = pos_args.len() + named.len();
    if n > 4 {
        return Err(Error::at(
            format!("Only 4 arguments allowed, but {n} were passed."),
            pos,
        ));
    }
    let channels = Channels::collect("hsl", &params, pos_args, named, pos)?;
    if let Some(verbatim) = channels.special_passthrough("hsl") {
        return Ok(verbatim);
    }
    let Channels { comps, alpha, .. } = channels;
    let h = hsl_hue(&comps[0], pos)?;
    // The repr preserves the supplied saturation/lightness percentages, except
    // saturation is floored at 0 (matching dart-sass: `hsl(0, 500%, 50%)` keeps
    // `500%`, `hsl(0, -100%, 50%)` becomes `0%`, lightness is left untouched).
    let s_raw = num(&comps[1], pos)?;
    let l_raw = num(&comps[2], pos)?;
    let s_pct = if s_raw.is_nan() { 0.0 } else { s_raw.max(0.0) };
    let l_pct = if l_raw.is_nan() { 0.0 } else { l_raw };
    let a = match &alpha {
        Some(v) => alpha_value(v, pos)?,
        None => 1.0,
    };
    let mut c = Color::from_hsl(
        h,
        (s_pct / 100.0).clamp(0.0, 1.0),
        (l_pct / 100.0).clamp(0.0, 1.0),
        a,
    );
    // hsl()/hsla() literals keep their function representation, matching
    // dart-sass (e.g. `hsl(120, 50%, 40%)` does not collapse to hex). The hue
    // is normalized to degrees in `[0, 360)`.
    let h_norm = h.rem_euclid(360.0);
    c.repr = Some(if (a - 1.0).abs() < f64::EPSILON {
        format!(
            "hsl({}, {}%, {}%)",
            fmt_num(h_norm, false),
            fmt_num(s_pct, false),
            fmt_num(l_pct, false)
        )
    } else {
        format!(
            "hsla({}, {}%, {}%, {})",
            fmt_num(h_norm, false),
            fmt_num(s_pct, false),
            fmt_num(l_pct, false),
            fmt_num(a, false)
        )
    });
    Ok(Value::Color(c))
}

/// Read an hsl hue value in degrees, converting `rad`/`grad`/`turn` units
/// (matching dart-sass's lenient legacy angle handling). Other/empty units
/// are taken as degrees.
fn hsl_hue(v: &Value, pos: Pos) -> Result<f64, Error> {
    match v {
        Value::Number(num) => Ok(match num.unit.as_str() {
            "rad" => num.value.to_degrees(),
            "grad" => num.value * 360.0 / 400.0,
            "turn" => num.value * 360.0,
            _ => num.value,
        }),
        Value::Slash(num, _) => Ok(num.value),
        other => Err(Error::at(
            format!("{} is not a number.", other.to_css(false)),
            pos,
        )),
    }
}

fn fn_mix(pos_args: &[Value], named: &[(String, Value)], pos: Pos) -> Result<Value, Error> {
    let params = ["color1", "color2", "weight", "method"];
    let n = pos_args.len() + named.len();
    if n > 4 {
        return Err(Error::at(
            format!("Only 4 arguments allowed, but {n} were passed."),
            pos,
        ));
    }
    let c1 = as_color(require(&params, pos_args, named, 0, "mix", pos)?, pos)?;
    let c2 = as_color(require(&params, pos_args, named, 1, "mix", pos)?, pos)?;
    let weight = match arg(&params, pos_args, named, 2) {
        Some(Value::Number(w)) => {
            if w.value < 0.0 || w.value > 100.0 {
                return Err(Error::at(
                    format!("$weight: Expected {} to be within 0% and 100%.", w.to_css(false)),
                    pos,
                ));
            }
            w.value
        }
        Some(other) => {
            return Err(Error::at(
                format!("$weight: {} is not a number.", other.to_css(false)),
                pos,
            ))
        }
        None => 50.0,
    };
    let p = weight / 100.0;
    let w = p * 2.0 - 1.0;
    let a = c1.a - c2.a;
    let w1 = ((if (w * a) == -1.0 {
        w
    } else {
        (w + a) / (1.0 + w * a)
    }) + 1.0)
        / 2.0;
    let w2 = 1.0 - w1;
    let r = c1.r * w1 + c2.r * w2;
    let g = c1.g * w1 + c2.g * w2;
    let b = c1.b * w1 + c2.b * w2;
    let alpha = c1.a * p + c2.a * (1.0 - p);
    Ok(Value::Color(computed(r, g, b, alpha)))
}

fn fn_adjust_lightness(
    name: &str,
    pos_args: &[Value],
    named: &[(String, Value)],
    pos: Pos,
    sign: f64,
) -> Result<Value, Error> {
    let params = ["color", "amount"];
    let n = pos_args.len() + named.len();
    if n > 2 {
        return Err(Error::at(
            format!("Only 2 arguments allowed, but {n} were passed."),
            pos,
        ));
    }
    let c = as_color(require(&params, pos_args, named, 0, name, pos)?, pos)?;
    let amount = match require(&params, pos_args, named, 1, name, pos)? {
        Value::Number(num) => {
            if num.value < 0.0 || num.value > 100.0 {
                return Err(Error::at(
                    format!("$amount: Expected {} to be within 0 and 100.", num.to_css(false)),
                    pos,
                ));
            }
            num.value
        }
        other => {
            return Err(Error::at(
                format!("$amount: {} is not a number.", other.to_css(false)),
                pos,
            ))
        }
    };
    let (h, s, l) = c.to_hsl();
    let new_l = (l + sign * amount / 100.0).clamp(0.0, 1.0);
    let mut out = Color::from_hsl(h, s, new_l, c.a);
    out.repr = named_repr(out.r, out.g, out.b, out.a);
    Ok(Value::Color(out))
}

fn fn_percentage(pos_args: &[Value], named: &[(String, Value)], pos: Pos) -> Result<Value, Error> {
    let params = ["number"];
    let n = num(require(&params, pos_args, named, 0, "percentage", pos)?, pos)?;
    Ok(Value::Number(Number {
        value: n * 100.0,
        unit: "%".to_string(),
    }))
}

fn fn_channel(name: &str, pos_args: &[Value], named: &[(String, Value)], pos: Pos) -> Result<Value, Error> {
    let params = ["color"];
    let c = as_color(require(&params, pos_args, named, 0, name, pos)?, pos)?;
    let v = match name {
        "red" => c.r,
        "green" => c.g,
        "blue" => c.b,
        _ => 0.0,
    };
    Ok(Value::Number(Number {
        value: v.round(),
        unit: String::new(),
    }))
}

fn fn_alpha(pos_args: &[Value], named: &[(String, Value)], pos: Pos) -> Result<Value, Error> {
    let params = ["color"];
    let n = pos_args.len() + named.len();
    if n > 1 {
        return Err(Error::at(
            format!("Only 1 argument allowed, but {n} were passed."),
            pos,
        ));
    }
    let c = as_color(require(&params, pos_args, named, 0, "alpha", pos)?, pos)?;
    Ok(Value::Number(Number {
        value: c.a,
        unit: String::new(),
    }))
}
