//! Core color builtins: `rgb`/`rgba`/`hsl`/`hsla`/`mix`, the legacy
//! `lighten`/`darken`, `percentage`, and the channel getters
//! `red`/`green`/`blue`/`alpha`.

use super::{arg, as_color, channel, clamp01, num, require};
use crate::error::Error;
use crate::scanner::Pos;
use crate::value::{fmt_num, Color, Number, Value};

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
        "lighten" => fn_adjust_lightness(pos_args, named, pos, 1.0),
        "darken" => fn_adjust_lightness(pos_args, named, pos, -1.0),
        "percentage" => fn_percentage(pos_args, named, pos),
        "red" | "green" | "blue" => fn_channel(name, pos_args, named, pos),
        "alpha" => fn_alpha(pos_args, named, pos),
        _ => return None,
    })
}

fn fn_rgb(pos_args: &[Value], named: &[(String, Value)], pos: Pos) -> Result<Value, Error> {
    let params = ["red", "green", "blue", "alpha"];
    // rgba($color, $alpha) two-argument form.
    if pos_args.len() == 2 {
        if let Value::Color(c) = &pos_args[0] {
            let a = clamp01(num(&pos_args[1], pos)?);
            let mut nc = c.clone();
            nc.a = a;
            nc.repr = Some(rgb_repr(nc.r, nc.g, nc.b, nc.a));
            return Ok(Value::Color(nc));
        }
    }
    let r = channel(require(&params, pos_args, named, 0, "rgb", pos)?, pos)?;
    let g = channel(require(&params, pos_args, named, 1, "rgb", pos)?, pos)?;
    let b = channel(require(&params, pos_args, named, 2, "rgb", pos)?, pos)?;
    let a = match arg(&params, pos_args, named, 3) {
        Some(v) => clamp01(num(v, pos)?),
        None => 1.0,
    };
    let mut c = Color::rgb(r, g, b, a);
    // rgb()/rgba() literals keep their function representation when emitted
    // unchanged, matching dart-sass.
    c.repr = Some(rgb_repr(r, g, b, a));
    Ok(Value::Color(c))
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
    let h = num(require(&params, pos_args, named, 0, "hsl", pos)?, pos)?;
    let s_pct = num(require(&params, pos_args, named, 1, "hsl", pos)?, pos)?;
    let l_pct = num(require(&params, pos_args, named, 2, "hsl", pos)?, pos)?;
    let a = match arg(&params, pos_args, named, 3) {
        Some(v) => clamp01(num(v, pos)?),
        None => 1.0,
    };
    let mut c = Color::from_hsl(
        h,
        (s_pct / 100.0).clamp(0.0, 1.0),
        (l_pct / 100.0).clamp(0.0, 1.0),
        a,
    );
    // hsl()/hsla() literals keep their function representation, matching
    // dart-sass (e.g. `hsl(120, 50%, 40%)` does not collapse to hex).
    c.repr = Some(if (a - 1.0).abs() < f64::EPSILON {
        format!(
            "hsl({}, {}%, {}%)",
            fmt_num(h, false),
            fmt_num(s_pct, false),
            fmt_num(l_pct, false)
        )
    } else {
        format!(
            "hsla({}, {}%, {}%, {})",
            fmt_num(h, false),
            fmt_num(s_pct, false),
            fmt_num(l_pct, false),
            fmt_num(a, false)
        )
    });
    Ok(Value::Color(c))
}

fn fn_mix(pos_args: &[Value], named: &[(String, Value)], pos: Pos) -> Result<Value, Error> {
    let params = ["color1", "color2", "weight"];
    let c1 = as_color(require(&params, pos_args, named, 0, "mix", pos)?, pos)?;
    let c2 = as_color(require(&params, pos_args, named, 1, "mix", pos)?, pos)?;
    let weight = match arg(&params, pos_args, named, 2) {
        Some(v) => num(v, pos)?,
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
    Ok(Value::Color(Color::rgb(r, g, b, alpha)))
}

fn fn_adjust_lightness(
    pos_args: &[Value],
    named: &[(String, Value)],
    pos: Pos,
    sign: f64,
) -> Result<Value, Error> {
    let params = ["color", "amount"];
    let c = as_color(require(&params, pos_args, named, 0, "lightness", pos)?, pos)?;
    let amount = num(require(&params, pos_args, named, 1, "lightness", pos)?, pos)?;
    let (h, s, l) = c.to_hsl();
    let new_l = (l + sign * amount / 100.0).clamp(0.0, 1.0);
    Ok(Value::Color(Color::from_hsl(h, s, new_l, c.a)))
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
    let c = as_color(require(&params, pos_args, named, 0, "alpha", pos)?, pos)?;
    Ok(Value::Number(Number {
        value: c.a,
        unit: String::new(),
    }))
}
