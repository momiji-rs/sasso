//! The replacement code dart-sass prints in a `[color-functions]` deprecation.
//!
//! Every legacy `sass:color` member Color 4 replaced carries a suggestion
//! computed from the call's OWN arguments: a channel getter names the channel
//! and its space, while a legacy adjuster offers `color.scale` (a relative
//! move, whose percentage depends on where the colour already is) and
//! `color.adjust` (the absolute one it always was). Every shape here was
//! measured against dart-sass 1.103.1.

use crate::value::{Number, Value};

/// The suggestions for a deprecated call, or `None` when the member carries no
/// `[color-functions]` deprecation at all (`alpha`, `mix`, `grayscale`,
/// `complement` and `invert` do not).
///
/// Called only once the function itself has SUCCEEDED, which is where dart
/// raises it too — a call whose arguments are invalid warns about nothing — so
/// the arguments are known good and anything unreadable here simply yields
/// `None`.
pub(crate) fn suggestions(name: &str, pos_args: &[Value], named: &[(String, Value)]) -> Option<Vec<String>> {
    if let Some(space) = channel_space(name) {
        return Some(vec![format!(
            "color.channel($color, \"{name}\", $space: {space})"
        )]);
    }
    let (channel, sign, param) = adjuster(name)?;
    let color = arg(pos_args, named, 0, "color")?;
    let amount = arg(pos_args, named, 1, param)?;
    let Value::Color(color) = color else { return None };
    let Value::Number(amount) = amount else {
        return None;
    };

    // `$hue` is an angle, converted to degrees and never scaled: a hue has no
    // bound to move a fraction of the way towards.
    if channel == "hue" {
        let deg = match amount.unit() {
            "rad" => amount.value.to_degrees(),
            "grad" => amount.value * 360.0 / 400.0,
            "turn" => amount.value * 360.0,
            _ => amount.value,
        };
        return Some(vec![adjust_line(channel, deg, "deg")]);
    }

    // `$lightness`/`$saturation` are read and written as percentages; `$alpha`
    // is the unitless 0-1 value it is passed as.
    let (current, limit, unit) = match channel {
        "alpha" => (color.a, 1.0, ""),
        "saturation" => (hsl_channel(color, 1), 100.0, "%"),
        _ => (hsl_channel(color, 2), 100.0, "%"),
    };
    let signed = sign * amount.value;
    let mut out = Vec::with_capacity(2);
    // Moving by nothing scales by nothing, and dart omits the line entirely.
    if signed != 0.0 {
        // `color.scale` moves a fraction of the way to the channel's bound, so
        // the equivalent fraction is the move over the distance left in that
        // direction — clamped, since the distance can be zero (lightening
        // white is "all the way" however much you ask for).
        let room = if signed > 0.0 { limit - current } else { current };
        let pct = (signed / room * 100.0).clamp(-100.0, 100.0);
        out.push(format!("color.scale($color, ${channel}: {})", number(pct, "%")));
    }
    out.push(adjust_line(channel, signed, unit));
    Some(out)
}

/// The colour's saturation (`i == 1`) or lightness (`i == 2`), as a
/// percentage.
///
/// An `hsl()` colour KEEPS its channels, and dart's suggestion is derived from
/// those, not from a round trip through rgb. The difference is normally
/// invisible, but the scale percentage divides by the room left in the channel
/// — so against a colour at 99.9999% saturation it is the difference between
/// dart's `9.9999999997%` and a re-derived `9.9999999982%`. The same reading
/// the `saturation`/`lightness` getters already prefer.
fn hsl_channel(color: &crate::value::Color, i: usize) -> f64 {
    if let Some(m) = &color.modern {
        if m.space == crate::value::ColorSpace::Hsl {
            return m.channels[i].unwrap_or(0.0);
        }
    }
    let hsl = color.to_hsl();
    let channel = if i == 1 { hsl.1 } else { hsl.2 };
    channel * 100.0
}

/// `color.adjust($color, $<channel>: <amount>)`.
fn adjust_line(channel: &str, amount: f64, unit: &str) -> String {
    format!("color.adjust($color, ${channel}: {})", number(amount, unit))
}

/// A number in the spelling Sass would write it, which is what dart embeds in
/// the suggestion.
fn number(value: f64, unit: &str) -> String {
    Value::Number(Number::with_unit(value, unit.to_string())).to_css(false)
}

/// The space a deprecated channel getter reads its channel from. dart names it
/// explicitly in the suggestion because `color.channel` defaults to the
/// colour's own space, which a legacy colour does not pin down.
fn channel_space(name: &str) -> Option<&'static str> {
    Some(match name {
        "red" | "green" | "blue" => "rgb",
        "hue" | "saturation" | "lightness" => "hsl",
        "whiteness" | "blackness" => "hwb",
        _ => return None,
    })
}

/// The channel a legacy adjuster moves, the direction it moves it, and the
/// name of the parameter carrying the amount.
///
/// The pairs differ only by sign (`darken` is `lighten` negated), and dart's
/// suggestion spells the signed amount. The parameter name is not uniform:
/// `adjust-hue` binds `$degrees` where the rest bind `$amount`, and reading
/// the wrong one loses the warning entirely for a call that names it.
fn adjuster(name: &str) -> Option<(&'static str, f64, &'static str)> {
    Some(match name {
        "lighten" => ("lightness", 1.0, "amount"),
        "darken" => ("lightness", -1.0, "amount"),
        "saturate" => ("saturation", 1.0, "amount"),
        "desaturate" => ("saturation", -1.0, "amount"),
        "opacify" | "fade-in" => ("alpha", 1.0, "amount"),
        "transparentize" | "fade-out" => ("alpha", -1.0, "amount"),
        "adjust-hue" => ("hue", 1.0, "degrees"),
        _ => return None,
    })
}

/// The `i`th argument, positionally or by name — the same binding the function
/// itself used.
fn arg<'a>(pos_args: &'a [Value], named: &'a [(String, Value)], i: usize, name: &str) -> Option<&'a Value> {
    pos_args
        .get(i)
        .or_else(|| named.iter().find(|(n, _)| n == name).map(|(_, v)| v))
}
