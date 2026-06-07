//! Extended color built-ins (`adjust-hue`, `saturate`, `desaturate`,
//! `complement`, `invert`, `grayscale`, `opacify`/`transparentize`, the
//! `hue`/`saturation`/`lightness` getters, …).
//!
//! Results are *computed* colors: they are built with [`Color::rgb`] or
//! [`Color::from_hsl`] and left with `repr = None`, so they serialize via
//! the normal `rgb()`/`rgba()`/hex rule, matching dart-sass.

use super::color::{
    missing_channel_err, missing_legacy_channel, modify_in_space, modify_in_space_opt, space_arg, ModifyOp,
};
use super::{arg, as_color, clamp01, num, require};
use crate::error::Error;
use crate::scanner::Pos;
use crate::value::{Color, ColorSpace, Number, SassStr, Value};

/// The modern change/adjust/scale path: used when `$space` is given, or when
/// the color is non-legacy (its own space is the default). Returns `None` for a
/// legacy color with no `$space`, so the caller runs the legacy path.
fn modify_with_space(
    c: &Color,
    named: &[(String, Value)],
    op: ModifyOp,
    pos: Pos,
) -> Option<Result<Value, Error>> {
    let space_arg_v = named.iter().find(|(n, _)| n == "space").map(|(_, v)| v);
    let is_legacy = c.modern.as_ref().map(|m| m.space.is_legacy()).unwrap_or(true);
    if space_arg_v.is_none() && is_legacy {
        return None;
    }
    let space = match space_arg_v {
        Some(v) => match space_arg(v, pos) {
            Ok(s) => s,
            Err(e) => return Some(Err(e)),
        },
        None => c.modern.as_ref().map(|m| m.space).unwrap_or(ColorSpace::Rgb),
    };
    let chans: Vec<(String, &Value)> = named
        .iter()
        .filter(|(n, _)| n != "space")
        .map(|(n, v)| (n.clone(), v))
        .collect();
    // An explicit `$space` enables the powerless-channel missing check.
    Some(modify_in_space_opt(
        c,
        space,
        &chans,
        op,
        space_arg_v.is_some(),
        pos,
    ))
}

/// Build a *computed* color, tagging it with a CSS named-color spelling when
/// its rounded RGB exactly matches one of the 148 CSS color names and it is
/// fully opaque. dart-sass serializes such colors by name (e.g.
/// `lighten(red, 100%)` → `white`, `adjust-hue(red, 180)` → `aqua`); colors
/// that don't match a name fall back to the usual hex/rgb rule via `repr =
/// None`.
pub(super) fn computed(r: f64, g: f64, b: f64, a: f64) -> Color {
    let mut c = Color::rgb(r, g, b, a);
    c.repr = named_repr(r, g, b, a);
    c
}

/// The CSS named-color spelling for an exact, fully-opaque RGB triple, using
/// dart-sass's canonical name for each color (e.g. `aqua` not `cyan`, `gray`
/// not `grey`). Returns `None` for translucent colors, non-integer channels,
/// or colors with no name.
pub(super) fn named_repr(r: f64, g: f64, b: f64, a: f64) -> Option<String> {
    if (a - 1.0).abs() >= f64::EPSILON {
        return None;
    }
    let int = |v: f64| {
        if (v - v.round()).abs() < 1e-9 && (0.0..=255.0).contains(&v) {
            Some(v.round() as u16)
        } else {
            None
        }
    };
    let (r, g, b) = (int(r)?, int(g)?, int(b)?);
    let name = match (r, g, b) {
        (0, 0, 0) => "black",
        (0, 0, 128) => "navy",
        (0, 0, 139) => "darkblue",
        (0, 0, 205) => "mediumblue",
        (0, 0, 255) => "blue",
        (0, 100, 0) => "darkgreen",
        (0, 128, 0) => "green",
        (0, 128, 128) => "teal",
        (0, 139, 139) => "darkcyan",
        (0, 191, 255) => "deepskyblue",
        (0, 206, 209) => "darkturquoise",
        (0, 250, 154) => "mediumspringgreen",
        (0, 255, 0) => "lime",
        (0, 255, 127) => "springgreen",
        (0, 255, 255) => "aqua",
        (25, 25, 112) => "midnightblue",
        (30, 144, 255) => "dodgerblue",
        (32, 178, 170) => "lightseagreen",
        (34, 139, 34) => "forestgreen",
        (46, 139, 87) => "seagreen",
        (47, 79, 79) => "darkslategray",
        (50, 205, 50) => "limegreen",
        (60, 179, 113) => "mediumseagreen",
        (64, 224, 208) => "turquoise",
        (65, 105, 225) => "royalblue",
        (70, 130, 180) => "steelblue",
        (72, 61, 139) => "darkslateblue",
        (72, 209, 204) => "mediumturquoise",
        (75, 0, 130) => "indigo",
        (85, 107, 47) => "darkolivegreen",
        (95, 158, 160) => "cadetblue",
        (100, 149, 237) => "cornflowerblue",
        (102, 51, 153) => "rebeccapurple",
        (102, 205, 170) => "mediumaquamarine",
        (105, 105, 105) => "dimgray",
        (106, 90, 205) => "slateblue",
        (107, 142, 35) => "olivedrab",
        (112, 128, 144) => "slategray",
        (119, 136, 153) => "lightslategray",
        (123, 104, 238) => "mediumslateblue",
        (124, 252, 0) => "lawngreen",
        (127, 255, 0) => "chartreuse",
        (127, 255, 212) => "aquamarine",
        (128, 0, 0) => "maroon",
        (128, 0, 128) => "purple",
        (128, 128, 0) => "olive",
        (128, 128, 128) => "gray",
        (135, 206, 235) => "skyblue",
        (135, 206, 250) => "lightskyblue",
        (138, 43, 226) => "blueviolet",
        (139, 0, 0) => "darkred",
        (139, 0, 139) => "darkmagenta",
        (139, 69, 19) => "saddlebrown",
        (143, 188, 143) => "darkseagreen",
        (144, 238, 144) => "lightgreen",
        (147, 112, 219) => "mediumpurple",
        (148, 0, 211) => "darkviolet",
        (152, 251, 152) => "palegreen",
        (153, 50, 204) => "darkorchid",
        (154, 205, 50) => "yellowgreen",
        (160, 82, 45) => "sienna",
        (165, 42, 42) => "brown",
        (169, 169, 169) => "darkgray",
        (173, 216, 230) => "lightblue",
        (173, 255, 47) => "greenyellow",
        (175, 238, 238) => "paleturquoise",
        (176, 196, 222) => "lightsteelblue",
        (176, 224, 230) => "powderblue",
        (178, 34, 34) => "firebrick",
        (184, 134, 11) => "darkgoldenrod",
        (186, 85, 211) => "mediumorchid",
        (188, 143, 143) => "rosybrown",
        (189, 183, 107) => "darkkhaki",
        (192, 192, 192) => "silver",
        (199, 21, 133) => "mediumvioletred",
        (205, 92, 92) => "indianred",
        (205, 133, 63) => "peru",
        (210, 105, 30) => "chocolate",
        (210, 180, 140) => "tan",
        (211, 211, 211) => "lightgray",
        (216, 191, 216) => "thistle",
        (218, 112, 214) => "orchid",
        (218, 165, 32) => "goldenrod",
        (219, 112, 147) => "palevioletred",
        (220, 20, 60) => "crimson",
        (220, 220, 220) => "gainsboro",
        (221, 160, 221) => "plum",
        (222, 184, 135) => "burlywood",
        (224, 255, 255) => "lightcyan",
        (230, 230, 250) => "lavender",
        (233, 150, 122) => "darksalmon",
        (238, 130, 238) => "violet",
        (238, 232, 170) => "palegoldenrod",
        (240, 128, 128) => "lightcoral",
        (240, 230, 140) => "khaki",
        (240, 248, 255) => "aliceblue",
        (240, 255, 240) => "honeydew",
        (240, 255, 255) => "azure",
        (244, 164, 96) => "sandybrown",
        (245, 222, 179) => "wheat",
        (245, 245, 220) => "beige",
        (245, 245, 245) => "whitesmoke",
        (245, 255, 250) => "mintcream",
        (248, 248, 255) => "ghostwhite",
        (250, 128, 114) => "salmon",
        (250, 235, 215) => "antiquewhite",
        (250, 240, 230) => "linen",
        (250, 250, 210) => "lightgoldenrodyellow",
        (253, 245, 230) => "oldlace",
        (255, 0, 0) => "red",
        (255, 0, 255) => "fuchsia",
        (255, 20, 147) => "deeppink",
        (255, 69, 0) => "orangered",
        (255, 99, 71) => "tomato",
        (255, 105, 180) => "hotpink",
        (255, 127, 80) => "coral",
        (255, 140, 0) => "darkorange",
        (255, 160, 122) => "lightsalmon",
        (255, 165, 0) => "orange",
        (255, 182, 193) => "lightpink",
        (255, 192, 203) => "pink",
        (255, 215, 0) => "gold",
        (255, 218, 185) => "peachpuff",
        (255, 222, 173) => "navajowhite",
        (255, 228, 181) => "moccasin",
        (255, 228, 196) => "bisque",
        (255, 228, 225) => "mistyrose",
        (255, 235, 205) => "blanchedalmond",
        (255, 239, 213) => "papayawhip",
        (255, 240, 245) => "lavenderblush",
        (255, 245, 238) => "seashell",
        (255, 248, 220) => "cornsilk",
        (255, 250, 205) => "lemonchiffon",
        (255, 250, 240) => "floralwhite",
        (255, 250, 250) => "snow",
        (255, 255, 0) => "yellow",
        (255, 255, 224) => "lightyellow",
        (255, 255, 240) => "ivory",
        (255, 255, 255) => "white",
        _ => return None,
    };
    Some(name.to_string())
}

pub(super) fn try_call(
    name: &str,
    pos_args: &[Value],
    named: &[(String, Value)],
    pos: Pos,
) -> Option<Result<Value, Error>> {
    Some(match name {
        "adjust-hue" => fn_adjust_hue(pos_args, named, pos),
        "complement" => fn_complement(pos_args, named, pos),
        "invert" => return fn_invert(pos_args, named, pos),
        "grayscale" => return fn_grayscale(pos_args, named, pos),
        "saturate" => return fn_saturate(name, pos_args, named, pos, 1.0),
        "desaturate" => fn_saturate_two(name, pos_args, named, pos, -1.0),
        "opacify" | "fade-in" => fn_fade(name, pos_args, named, pos, 1.0),
        "transparentize" | "fade-out" => fn_fade(name, pos_args, named, pos, -1.0),
        "hue" | "saturation" | "lightness" | "whiteness" | "blackness" => {
            fn_hsl_getter(name, pos_args, named, pos)
        }
        "opacity" => return fn_opacity(pos_args, named, pos),
        "ie-hex-str" => fn_ie_hex_str(pos_args, named, pos),
        "scale-color" => fn_scale_color(pos_args, named, pos),
        "adjust-color" => fn_adjust_color(pos_args, named, pos),
        "change-color" => fn_change_color(pos_args, named, pos),
        _ => return None,
    })
}

/// Error when more than `max` positional/named arguments were supplied to a
/// fixed-arity builtin, matching dart-sass's message.
fn check_max_args(pos_args: &[Value], named: &[(String, Value)], max: usize, pos: Pos) -> Result<(), Error> {
    let n = pos_args.len() + named.len();
    if n > max {
        let plural = if max == 1 { "argument" } else { "arguments" };
        Err(Error::at(
            format!("Only {max} {plural} allowed, but {n} were passed."),
            pos,
        ))
    } else {
        Ok(())
    }
}

/// True when a value is a plain-CSS "special" expression (a number, a
/// calculation, or an unquoted string holding a CSS function like `var(...)`).
/// The legacy filter-overloaded color functions (`invert`, `grayscale`,
/// `saturate`, `opacity`) pass such an argument straight through as a CSS
/// function call rather than treating it as a color.
fn is_css_special(v: &Value) -> bool {
    match v {
        Value::Number(_) | Value::Calc(_) | Value::Slash(_, _) => true,
        Value::Str(s) => !s.quoted && s.text.contains('('),
        _ => false,
    }
}

/// Preserve a one-argument filter overload verbatim (`invert(10%)`,
/// `grayscale(var(--c))`).
fn plain_filter(name: &str, arg: &Value) -> Value {
    Value::Str(SassStr {
        text: format!("{name}({})", arg.to_css(false)),
        quoted: false,
    })
}

/// `adjust-hue($color, $degrees)` — rotate the hue by `$degrees`, converting
/// any angle unit (`rad`/`grad`/`turn`) to degrees first.
fn fn_adjust_hue(pos_args: &[Value], named: &[(String, Value)], pos: Pos) -> Result<Value, Error> {
    let params = ["color", "degrees"];
    check_max_args(pos_args, named, 2, pos)?;
    let c = as_color(require(&params, pos_args, named, 0, "adjust-hue", pos)?, pos)?;
    // The legacy `adjust-hue()` getter only supports legacy colors.
    if c.modern.as_ref().is_some_and(|m| !m.space.is_legacy()) {
        return Err(Error::at(
            "adjust-hue() is only supported for legacy colors. Please use color.adjust() \
             instead with an explicit $space argument."
                .to_string(),
            pos,
        ));
    }
    let degrees = angle_degrees(require(&params, pos_args, named, 1, "adjust-hue", pos)?, pos)?;
    Ok(Value::Color(rotate_hue(&c, degrees)))
}

/// Extract a hue angle in degrees from a number, converting the common CSS
/// angle units. Unknown units (and the deprecated unitless/`in` cases) are
/// treated as degrees, matching dart-sass's lenient legacy behavior.
fn angle_degrees(v: &Value, pos: Pos) -> Result<f64, Error> {
    match v {
        Value::Number(n) => Ok(match n.unit.as_str() {
            "rad" => n.value.to_degrees(),
            "grad" => n.value * 360.0 / 400.0,
            "turn" => n.value * 360.0,
            _ => n.value,
        }),
        other => Err(Error::at(
            format!("{} is not a number.", other.to_css(false)),
            pos,
        )),
    }
}

/// `complement($color, $space)` — rotate the hue by 180 degrees in `$space`
/// (default `hsl` for legacy colors; required for non-legacy colors).
fn fn_complement(pos_args: &[Value], named: &[(String, Value)], pos: Pos) -> Result<Value, Error> {
    let params = ["color", "space"];
    check_max_args(pos_args, named, 2, pos)?;
    let c = as_color(require(&params, pos_args, named, 0, "complement", pos)?, pos)?;
    let space_v = arg(&params, pos_args, named, 1);
    let is_legacy = c.modern.as_ref().map(|m| m.space.is_legacy()).unwrap_or(true);
    let space = match space_v {
        Some(v) => space_arg(v, pos)?,
        None if is_legacy => ColorSpace::Hsl,
        None => {
            return Err(Error::at(
                format!(
                    "$space: To use color.complement() with non-legacy color {}, you must provide a $space.",
                    c.to_css(false)
                ),
                pos,
            ))
        }
    };
    // The space must have a hue channel.
    if !matches!(
        space,
        ColorSpace::Hsl | ColorSpace::Hwb | ColorSpace::Lch | ColorSpace::Oklch
    ) {
        return Err(Error::at(
            format!("$space: Color space {} doesn't have a hue channel.", space.name()),
            pos,
        ));
    }
    // complement = adjust the hue by +180deg in the space. An explicit `$space`
    // enables the powerless-channel missing check.
    let deg = Value::Number(Number {
        value: 180.0,
        unit: "deg".to_string(),
    });
    modify_in_space_opt(
        &c,
        space,
        &[("hue".to_string(), &deg)],
        ModifyOp::Adjust,
        space_v.is_some(),
        pos,
    )
}

fn rotate_hue(c: &Color, degrees: f64) -> Color {
    let (h, s, l) = c.to_hsl();
    from_hsl(h + degrees, s, l, c.a)
}

/// Like [`Color::from_hsl`] but tags the result with a CSS named-color
/// spelling when it matches one (so e.g. `adjust-hue(red, 180)` → `aqua`).
fn from_hsl(h: f64, s: f64, l: f64, a: f64) -> Color {
    let c = Color::from_hsl(h, s, l, a);
    let mut c = c;
    c.repr = named_repr(c.r, c.g, c.b, c.a);
    c
}

/// `invert($color, $weight: 100%)` — invert the RGB channels, then mix the
/// inverted color toward the original by `(100% - weight)`.
///
/// When the single argument is a plain-CSS special value (a number, `var()`,
/// …) this is the CSS `invert()` filter and is preserved verbatim; passing a
/// weight alongside that form is an error.
fn fn_invert(pos_args: &[Value], named: &[(String, Value)], pos: Pos) -> Option<Result<Value, Error>> {
    let params = ["color", "weight"];
    if let Err(e) = check_max_args(pos_args, named, 3, pos) {
        return Some(Err(e));
    }
    let color = match require(&params, pos_args, named, 0, "invert", pos) {
        Ok(v) => v,
        Err(e) => return Some(Err(e)),
    };
    if is_css_special(color) {
        if pos_args.len() + named.len() > 1 {
            return Some(Err(Error::at(
                "Only one argument may be passed to the plain-CSS invert() function.".to_string(),
                pos,
            )));
        }
        return Some(Ok(plain_filter("invert", color)));
    }
    let params = ["color", "weight", "space"];
    Some((|| {
        let c = as_color(color, pos)?;
        let weight = match arg(&params, pos_args, named, 1) {
            Some(v) => num(v, pos)?,
            None => 100.0,
        };
        let w = (weight / 100.0).clamp(0.0, 1.0);
        let space_v = arg(&params, pos_args, named, 2);
        let is_legacy = c.modern.as_ref().map(|m| m.space.is_legacy()).unwrap_or(true);
        // The modern form (`$space` given, or a non-legacy color) inverts each
        // channel in that space; a non-legacy color without $space errors.
        if space_v.is_some() || !is_legacy {
            let space = match space_v {
                Some(v) => space_arg(v, pos)?,
                None => {
                    return Err(Error::at(
                        format!(
                        "$color: To use color.invert() with non-legacy color {}, you must provide a $space.",
                        c.to_css(false)
                    ),
                        pos,
                    ))
                }
            };
            // An explicit `$space` enables the powerless-channel missing check.
            return Ok(Value::Color(super::color::invert_in_space(
                &c,
                space,
                w,
                space_v.is_some(),
                pos,
            )?));
        }
        // Legacy color, no `$space`: invert in rgb (which still rejects a missing
        // rgb channel, but has no powerless channels).
        Ok(Value::Color(super::color::invert_in_space(
            &c,
            ColorSpace::Rgb,
            w,
            false,
            pos,
        )?))
    })())
}

/// `grayscale($color)` — set the HSL saturation to 0. With a plain-CSS
/// special argument it is the CSS `grayscale()` filter, preserved verbatim.
fn fn_grayscale(pos_args: &[Value], named: &[(String, Value)], pos: Pos) -> Option<Result<Value, Error>> {
    let params = ["color"];
    if let Err(e) = check_max_args(pos_args, named, 1, pos) {
        return Some(Err(e));
    }
    let color = match require(&params, pos_args, named, 0, "grayscale", pos) {
        Ok(v) => v,
        Err(e) => return Some(Err(e)),
    };
    if is_css_special(color) {
        return Some(Ok(plain_filter("grayscale", color)));
    }
    Some((|| {
        let c = as_color(color, pos)?;
        // A non-legacy color is desaturated by setting its oklch chroma to 0
        // and converting back to its own space; legacy colors set HSL
        // saturation to 0.
        let is_legacy = c.modern.as_ref().map(|m| m.space.is_legacy()).unwrap_or(true);
        if !is_legacy {
            return Ok(Value::Color(super::color::grayscale_modern(&c)));
        }
        let (h, _s, l) = c.to_hsl();
        Ok(Value::Color(from_hsl(h, 0.0, l, c.a)))
    })())
}

/// `saturate($amount)` (CSS filter overload) and `saturate($color, $amount)`.
/// The one-argument form with a special CSS value is preserved verbatim.
fn fn_saturate(
    name: &str,
    pos_args: &[Value],
    named: &[(String, Value)],
    pos: Pos,
    sign: f64,
) -> Option<Result<Value, Error>> {
    // One argument that is a plain-CSS special value → CSS `saturate()` filter.
    if pos_args.len() + named.len() == 1 {
        let arg0 = pos_args
            .first()
            .or_else(|| named.iter().find(|(n, _)| n == "amount").map(|(_, v)| v));
        if let Some(v) = arg0 {
            if is_css_special(v) {
                return Some(Ok(plain_filter("saturate", v)));
            }
        }
    }
    Some(fn_saturate_two(name, pos_args, named, pos, sign))
}

/// `saturate`/`desaturate` — adjust HSL saturation by `$amount` percent
/// (validated to be within 0 and 100).
fn fn_saturate_two(
    name: &str,
    pos_args: &[Value],
    named: &[(String, Value)],
    pos: Pos,
    sign: f64,
) -> Result<Value, Error> {
    let params = ["color", "amount"];
    check_max_args(pos_args, named, 2, pos)?;
    let c = as_color(require(&params, pos_args, named, 0, name, pos)?, pos)?;
    let amount = require(&params, pos_args, named, 1, name, pos)?;
    let amount = bounded(amount, 0.0, 100.0, pos)?;
    let (h, s, l) = c.to_hsl();
    let new_s = (s + sign * amount / 100.0).clamp(0.0, 1.0);
    Ok(Value::Color(from_hsl(h, new_s, l, c.a)))
}

/// Read a number argument and require its value to be within `[lo, hi]`,
/// raising dart-sass's "Expected … to be within …" error otherwise. The
/// number's unit is preserved in the message (matching dart-sass), but the
/// bound is applied to the raw value.
fn bounded(v: &Value, lo: f64, hi: f64, pos: Pos) -> Result<f64, Error> {
    match v {
        Value::Number(n) => {
            if n.value < lo || n.value > hi {
                Err(Error::at(
                    format!(
                        "$amount: Expected {} to be within {} and {}.",
                        n.to_css(false),
                        fmt_bound(lo),
                        fmt_bound(hi),
                    ),
                    pos,
                ))
            } else {
                Ok(n.value)
            }
        }
        other => Err(Error::at(
            format!("$amount: {} is not a number.", other.to_css(false)),
            pos,
        )),
    }
}

fn fmt_bound(v: f64) -> String {
    if v.fract() == 0.0 {
        format!("{}", v as i64)
    } else {
        format!("{v}")
    }
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
    check_max_args(pos_args, named, 2, pos)?;
    let c = as_color(require(&params, pos_args, named, 0, name, pos)?, pos)?;
    let amount = require(&params, pos_args, named, 1, name, pos)?;
    let amount = bounded(amount, 0.0, 1.0, pos)?;
    let a = clamp01(c.a + sign * amount);
    Ok(Value::Color(computed(c.r, c.g, c.b, a)))
}

/// `hue` (deg), `saturation`/`lightness` (%), and the HWB-derived
/// `whiteness`/`blackness` (%) getters.
fn fn_hsl_getter(
    name: &str,
    pos_args: &[Value],
    named: &[(String, Value)],
    pos: Pos,
) -> Result<Value, Error> {
    let params = ["color"];
    check_max_args(pos_args, named, 1, pos)?;
    let c = as_color(require(&params, pos_args, named, 0, name, pos)?, pos)?;
    // These legacy getters only support legacy colors.
    let is_legacy = c.modern.as_ref().map(|m| m.space.is_legacy()).unwrap_or(true);
    if !is_legacy {
        let space = if matches!(name, "whiteness" | "blackness") {
            "hwb"
        } else {
            "hsl"
        };
        return Err(Error::at(
            format!(
                "color.{name}() is only supported for legacy colors. Please use color.channel() \
                 instead with an explicit $space argument.\n\n\
                 color.channel($color, \"{name}\", $space: {space})"
            ),
            pos,
        ));
    }
    // Prefer the stored hsl/hwb channels (exact) over re-deriving from rgb.
    if let Some(m) = &c.modern {
        let idx = match (m.space, name) {
            (crate::value::ColorSpace::Hsl, "hue") | (crate::value::ColorSpace::Hwb, "hue") => {
                Some((0, "deg"))
            }
            (crate::value::ColorSpace::Hsl, "saturation") => Some((1, "%")),
            (crate::value::ColorSpace::Hsl, "lightness") => Some((2, "%")),
            (crate::value::ColorSpace::Hwb, "whiteness") => Some((1, "%")),
            (crate::value::ColorSpace::Hwb, "blackness") => Some((2, "%")),
            _ => None,
        };
        if let Some((i, unit)) = idx {
            return Ok(Value::Number(Number {
                value: m.channels[i].unwrap_or(0.0),
                unit: unit.to_string(),
            }));
        }
    }
    let (h, s, l) = c.to_hsl();
    let (value, unit) = match name {
        "hue" => (h, "deg"),
        "saturation" => (s * 100.0, "%"),
        "lightness" => (l * 100.0, "%"),
        "whiteness" => (c.r.min(c.g).min(c.b) / 255.0 * 100.0, "%"),
        // blackness
        _ => ((1.0 - c.r.max(c.g).max(c.b) / 255.0) * 100.0, "%"),
    };
    Ok(Value::Number(Number {
        value,
        unit: unit.to_string(),
    }))
}

/// `opacity($color)` returns the alpha; `opacity($number)` (CSS filter
/// overload) is preserved verbatim.
fn fn_opacity(pos_args: &[Value], named: &[(String, Value)], pos: Pos) -> Option<Result<Value, Error>> {
    let params = ["color"];
    if let Err(e) = check_max_args(pos_args, named, 1, pos) {
        return Some(Err(e));
    }
    let color = match require(&params, pos_args, named, 0, "opacity", pos) {
        Ok(v) => v,
        Err(e) => return Some(Err(e)),
    };
    if is_css_special(color) {
        return Some(Ok(plain_filter("opacity", color)));
    }
    Some((|| {
        let c = as_color(color, pos)?;
        Ok(Value::Number(Number {
            value: c.a,
            unit: String::new(),
        }))
    })())
}

/// `ie-hex-str($color)` — the `#AARRGGBB` Internet Explorer hex string.
fn fn_ie_hex_str(pos_args: &[Value], named: &[(String, Value)], pos: Pos) -> Result<Value, Error> {
    let params = ["color"];
    check_max_args(pos_args, named, 1, pos)?;
    let c = as_color(require(&params, pos_args, named, 0, "ie-hex-str", pos)?, pos)?;
    let byte = |v: f64| v.round().clamp(0.0, 255.0) as u8;
    let text = format!(
        "#{:02X}{:02X}{:02X}{:02X}",
        byte(c.a * 255.0),
        byte(c.r),
        byte(c.g),
        byte(c.b)
    );
    Ok(Value::Str(SassStr { text, quoted: false }))
}

// ---- scale-color / adjust-color / change-color ------------------------

/// Which legacy color space a set of keyword channels operates in.
#[derive(Clone, Copy, PartialEq)]
enum Space {
    Rgb,
    Hsl,
    Hwb,
}

impl Space {
    fn name(self) -> &'static str {
        match self {
            Space::Rgb => "rgb",
            Space::Hsl => "hsl",
            Space::Hwb => "hwb",
        }
    }
}

/// A resolved channel argument: its keyword name and the supplied value.
type ChannelArg<'v> = (&'v str, &'v Value);

/// A channel keyword (`red`, `hue`, `alpha`, …) and the space(s) it belongs
/// to. `hue` is shared between HSL and HWB; `alpha` is universal.
fn channel_space(name: &str) -> Option<(Option<Space>, bool)> {
    // (specific space, is_hue_shared)
    Some(match name {
        "red" | "green" | "blue" => (Some(Space::Rgb), false),
        "saturation" | "lightness" => (Some(Space::Hsl), false),
        "whiteness" | "blackness" => (Some(Space::Hwb), false),
        "hue" => (None, true),
        "alpha" => return Some((None, false)),
        _ => return None,
    })
}

/// Collect the channel keyword arguments and resolve the operating space,
/// matching dart-sass's legacy detection: the first space-specific channel
/// fixes the space (hue-only defaults to HSL); any later channel not valid in
/// that space is an error.
fn resolve_channels<'v>(
    _fname: &str,
    named: &'v [(String, Value)],
    pos: Pos,
) -> Result<(Space, Vec<ChannelArg<'v>>), Error> {
    // The `$color` argument may be passed by name; it is not a channel.
    let chans: Vec<ChannelArg<'v>> = named
        .iter()
        .filter(|(n, _)| n != "color")
        .map(|(n, v)| (n.as_str(), v))
        .collect();
    // Determine the space from the first space-specific channel. If only the
    // shared `hue` channel is given, default to HSL; with no recognized
    // channel at all, dart-sass falls back to RGB.
    let mut space: Option<Space> = None;
    let mut has_hue = false;
    for (n, _) in &chans {
        match channel_space(n) {
            Some((Some(s), _)) => {
                space = Some(s);
                break;
            }
            Some((None, true)) => has_hue = true,
            _ => {}
        }
    }
    let space = space.unwrap_or(if has_hue { Space::Hsl } else { Space::Rgb });
    // Validate every channel belongs to the resolved space (alpha is allowed
    // everywhere; unknown channels error against the resolved space too).
    for (n, _) in &chans {
        if *n == "alpha" {
            continue;
        }
        let ok = match channel_space(n) {
            Some((Some(s), _)) => s == space,
            Some((None, true)) => space != Space::Rgb, // hue
            _ => false,
        };
        if !ok {
            return Err(Error::at(
                format!(
                    "${n}: Color space {} doesn't have a channel with this name.",
                    space.name()
                ),
                pos,
            ));
        }
    }
    Ok((space, chans))
}

/// Decompose a color into the channel tuple for `space` (each in dart-sass's
/// natural unit: rgb 0–255, hsl/hwb percentages 0–100, hue degrees).
fn decompose(c: &Color, space: Space) -> [f64; 3] {
    match space {
        Space::Rgb => [c.r, c.g, c.b],
        Space::Hsl => {
            let (h, s, l) = c.to_hsl();
            [h, s * 100.0, l * 100.0]
        }
        Space::Hwb => {
            let (h, _s, _l) = c.to_hsl();
            let w = c.r.min(c.g).min(c.b) / 255.0 * 100.0;
            let b = (1.0 - c.r.max(c.g).max(c.b) / 255.0) * 100.0;
            [h, w, b]
        }
    }
}

/// Rebuild a color from a channel tuple in `space` plus alpha.
fn recompose(space: Space, ch: [f64; 3], a: f64) -> Color {
    match space {
        Space::Rgb => computed(ch[0], ch[1], ch[2], a),
        Space::Hsl => from_hsl(
            ch[0],
            (ch[1] / 100.0).clamp(0.0, 1.0),
            (ch[2] / 100.0).clamp(0.0, 1.0),
            a,
        ),
        Space::Hwb => from_hwb(ch[0], ch[1], ch[2], a),
    }
}

/// HWB → sRGB, per the CSS Color 4 algorithm. Whiteness/blackness are
/// percentages (0–100); they are normalized when their sum exceeds 100.
fn from_hwb(h: f64, w_pct: f64, b_pct: f64, a: f64) -> Color {
    let mut w = (w_pct / 100.0).clamp(0.0, 1.0);
    let mut b = (b_pct / 100.0).clamp(0.0, 1.0);
    if w + b > 1.0 {
        let sum = w + b;
        w /= sum;
        b /= sum;
    }
    // Start from the pure hue, then scale by (1 - w - b) and add w.
    let base = Color::from_hsl(h, 1.0, 0.5, a);
    let mix = |v: f64| ((v / 255.0) * (1.0 - w - b) + w) * 255.0;
    computed(mix(base.r), mix(base.g), mix(base.b), a)
}

/// The index of a channel within its space's tuple, and whether it is the
/// hue channel.
fn channel_index(space: Space, name: &str) -> Option<usize> {
    Some(match (space, name) {
        (Space::Rgb, "red") => 0,
        (Space::Rgb, "green") => 1,
        (Space::Rgb, "blue") => 2,
        (Space::Hsl, "hue") => 0,
        (Space::Hsl, "saturation") => 1,
        (Space::Hsl, "lightness") => 2,
        (Space::Hwb, "hue") => 0,
        (Space::Hwb, "whiteness") => 1,
        (Space::Hwb, "blackness") => 2,
        _ => return None,
    })
}

/// `adjust-color($color, channels…)` — add each amount to the matching
/// channel (rgb values 0–255, hsl/hwb percentages, hue degrees, alpha 0–1),
/// then clamp.
fn fn_adjust_color(pos_args: &[Value], named: &[(String, Value)], pos: Pos) -> Result<Value, Error> {
    let params = ["color"];
    let c = as_color(require(&params, pos_args, named, 0, "adjust-color", pos)?, pos)?;
    if pos_args.len() > 1 {
        return Err(Error::at(
            "Only one positional argument is allowed. All other arguments must be passed by name."
                .to_string(),
            pos,
        ));
    }
    if let Some(r) = modify_with_space(&c, named, super::color::ModifyOp::Adjust, pos) {
        return r;
    }
    // Legacy color with no $space: detect the legacy space from the channel
    // keywords, then run the modern adjust path (which clamps only rgb/[0,255]
    // and the perceptual lightness/chroma, leaving hsl/hwb percentages free).
    let (space, chans) = resolve_channels("adjust-color", named, pos)?;
    let cspace = match space {
        Space::Rgb => ColorSpace::Rgb,
        Space::Hsl => ColorSpace::Hsl,
        Space::Hwb => ColorSpace::Hwb,
    };
    let chan_args: Vec<(String, &Value)> = chans.iter().map(|(n, v)| (n.to_string(), *v)).collect();
    modify_in_space(&c, cspace, &chan_args, super::color::ModifyOp::Adjust, pos)
}

/// `change-color($color, channels…)` — set each channel to the given value
/// (alpha validated to `[0, 1]`).
fn fn_change_color(pos_args: &[Value], named: &[(String, Value)], pos: Pos) -> Result<Value, Error> {
    let params = ["color"];
    let c = as_color(require(&params, pos_args, named, 0, "change-color", pos)?, pos)?;
    if pos_args.len() > 1 {
        return Err(Error::at(
            "Only one positional argument is allowed. All other arguments must be passed by name."
                .to_string(),
            pos,
        ));
    }
    if let Some(r) = modify_with_space(&c, named, super::color::ModifyOp::Change, pos) {
        return r;
    }
    // Legacy color with no $space: detect the legacy space from the channel
    // keywords, then run the modern (non-clamping, `none`-aware) modify path so
    // out-of-range channels and missing channels match dart-sass.
    let (space, chans) = resolve_channels("change-color", named, pos)?;
    let cspace = match space {
        Space::Rgb => ColorSpace::Rgb,
        Space::Hsl => ColorSpace::Hsl,
        Space::Hwb => ColorSpace::Hwb,
    };
    let chan_args: Vec<(String, &Value)> = chans.iter().map(|(n, v)| (n.to_string(), *v)).collect();
    modify_in_space(&c, cspace, &chan_args, super::color::ModifyOp::Change, pos)
}

/// `scale-color($color, channels…)` — fluidly scale each channel a percentage
/// of the way toward its bound. Each amount must be a `%` within `[-100,
/// 100]`.
fn fn_scale_color(pos_args: &[Value], named: &[(String, Value)], pos: Pos) -> Result<Value, Error> {
    let params = ["color"];
    let c = as_color(require(&params, pos_args, named, 0, "scale-color", pos)?, pos)?;
    if pos_args.len() > 1 {
        return Err(Error::at(
            "Only one positional argument is allowed. All other arguments must be passed by name."
                .to_string(),
            pos,
        ));
    }
    if let Some(r) = modify_with_space(&c, named, super::color::ModifyOp::Scale, pos) {
        return r;
    }
    let (space, chans) = resolve_channels("scale-color", named, pos)?;
    let cspace = match space {
        Space::Rgb => ColorSpace::Rgb,
        Space::Hsl => ColorSpace::Hsl,
        Space::Hwb => ColorSpace::Hwb,
    };
    let mut tuple = decompose(&c, space);
    let mut alpha = c.a;
    for (n, v) in chans {
        if n == "hue" {
            return Err(Error::at(format!("${n}: Channel isn't scalable."), pos));
        }
        // `scale` combines with the channel's current value, so a missing
        // (`none`) channel is unsupported (dart-sass errors).
        if let Some(color) = missing_legacy_channel(&c, cspace, n) {
            return Err(missing_channel_err(n, &color, pos));
        }
        let factor = scale_factor(n, v, pos)?;
        if n == "alpha" {
            alpha = scale_toward(alpha, factor, 1.0);
        } else if let Some(i) = channel_index(space, n) {
            let max = channel_max(space, i);
            tuple[i] = scale_toward(tuple[i], factor, max);
        }
    }
    Ok(Value::Color(recompose(space, tuple, alpha)))
}

/// Scale `current` by `factor` (`-1..=1`) toward `max` (when positive) or `0`
/// (when negative).
fn scale_toward(current: f64, factor: f64, max: f64) -> f64 {
    if factor > 0.0 {
        current + (max - current) * factor
    } else {
        current + current * factor
    }
}

/// Read a scale-color amount: a `%` within `[-100, 100]`, returned as a
/// fraction in `[-1, 1]`.
fn scale_factor(name: &str, v: &Value, pos: Pos) -> Result<f64, Error> {
    match v {
        Value::Number(n) => {
            if n.unit != "%" {
                return Err(Error::at(
                    format!("${name}: Expected {} to have unit \"%\".", n.to_css(false)),
                    pos,
                ));
            }
            if n.value < -100.0 || n.value > 100.0 {
                return Err(Error::at(
                    format!(
                        "${name}: Expected {} to be within -100% and 100%.",
                        n.to_css(false)
                    ),
                    pos,
                ));
            }
            Ok(n.value / 100.0)
        }
        other => Err(Error::at(
            format!("${name}: {} is not a number.", other.to_css(false)),
            pos,
        )),
    }
}

/// The maximum value a channel can reach when scaling.
fn channel_max(space: Space, i: usize) -> f64 {
    match (space, i) {
        (Space::Rgb, _) => 255.0,
        _ => 100.0,
    }
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
