//! Core color builtins: `rgb`/`rgba`/`hsl`/`hsla`/`mix`, the legacy
//! `lighten`/`darken`, `percentage`, and the channel getters
//! `red`/`green`/`blue`/`alpha`.

use super::color_ext::{computed, named_repr};
use super::{arg, as_color, channel, num, require};
use crate::error::Error;
use crate::scanner::Pos;
use crate::value::{fmt_num, CalcNode, Color, ListSep, Number, Value};

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
    // call is preserved verbatim instead. The arguments may be passed by name
    // (`rgb($color: red, $alpha: 0.5)`).
    if n == 2 {
        let color = pos_args
            .first()
            .or_else(|| named.iter().find(|(k, _)| k == "color").map(|(_, v)| v));
        if let Some(Value::Color(c)) = color {
            let alpha = pos_args
                .get(1)
                .or_else(|| named.iter().find(|(k, _)| k == "alpha").map(|(_, v)| v));
            if let Some(alpha) = alpha {
                if is_special_legacy(alpha) {
                    // rgb(blue, calc(0.4)) → rgb(0, 0, 255, calc(0.4)).
                    let r = Value::Number(int_num(c.r));
                    let g = Value::Number(int_num(c.g));
                    let b = Value::Number(int_num(c.b));
                    return Ok(special_call("rgb", &[&r, &g, &b, alpha]));
                }
                let a = alpha_value(alpha, pos)?;
                return Ok(Value::Color(computed(c.r, c.g, c.b, a)));
            }
        }
    }
    // Otherwise gather the channel list and an optional alpha.
    let channels = Channels::collect("rgb", &params, pos_args, named, pos)?;
    if let Some(verbatim) = channels.relative_passthrough("rgb") {
        return Ok(verbatim);
    }
    if let Some(c) = legacy_none_color(&channels, ColorSpace::Rgb, pos)? {
        return Ok(c);
    }
    if let Some(verbatim) = channels.special_passthrough("rgb") {
        return Ok(verbatim);
    }
    channels.validate_numeric(&["red", "green", "blue"], pos)?;
    channels.validate_count("rgb", pos)?;
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
    if let Value::Calc(node) = v {
        if let Some(c) = degenerate_const(node) {
            return Ok(clamp_alpha(c));
        }
    }
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
    if let Value::Calc(node) = v {
        if let Some(c) = degenerate_const(node) {
            return Ok(clamp_finite(c, 0.0, 255.0));
        }
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
    /// Whether `alpha` was peeled from the trailing item of `single` (a
    /// `… / alpha` slash). A verbatim passthrough then re-serializes `single`
    /// (which still holds the glued alpha) rather than reconstructing a comma
    /// call from the components plus a separate alpha.
    alpha_split: bool,
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
                alpha_split: false,
            });
        }
        // One argument: a channels value. dart-sass also accepts this single
        // value under the name `$channels` (`hsl($channels: 0 100% 50%)`); a
        // second positional/`$alpha` argument is an explicit alpha for a
        // special-value channels list.
        let channels = match arg(params, pos_args, named, 0) {
            Some(v) => v.clone(),
            None => named
                .iter()
                .find(|(n, _)| n == "channels")
                .map(|(_, v)| v.clone())
                .ok_or_else(|| Error::at(format!("Missing argument $channels for {fname}()."), pos))?,
        };
        // A channels list must be unbracketed and space/slash-separated. A
        // bracketed and/or comma list is rejected with dart-sass's message.
        if let Value::List(l) = &channels {
            let comma = l.sep == ListSep::Comma;
            if l.bracketed || comma {
                let kind = if l.bracketed && comma {
                    "an unbracketed, space- or slash-separated list"
                } else if l.bracketed {
                    "an unbracketed list"
                } else {
                    "a space- or slash-separated list"
                };
                // A bracketed list serializes with its own `[...]`; a bare
                // (unbracketed) comma list is shown parenthesized, matching
                // dart-sass (`(1, 2, 3)`).
                let shown = if l.bracketed {
                    channels.to_css(false)
                } else {
                    list_paren_css(&channels)
                };
                return Err(Error::at(format!("$channels: Expected {kind}, was {shown}"), pos));
            }
        }
        let extra_alpha = arg(params, pos_args, named, 1).cloned();
        let SplitChannels {
            comps,
            mut alpha,
            mut alpha_split,
        } = split_channels(&channels);
        if extra_alpha.is_some() {
            alpha = extra_alpha;
            // An explicit `$alpha` is not part of the `single` spelling, so a
            // verbatim passthrough must reconstruct rather than re-serialize.
            alpha_split = false;
        }
        Ok(Channels {
            comps,
            alpha,
            single: Some(channels),
            alpha_split,
        })
    }

    /// Validate that every channel of a single-argument channels list is a
    /// number, matching dart-sass's per-channel check. A non-number channel
    /// (a plain string such as a non-`from` relative keyword, e.g.
    /// `rgb(c #aaa r g b)`) reports `Expected <name> channel to be a number,
    /// was X` before the channel-count check. Special/`none` channels are
    /// handled earlier by [`Channels::special_passthrough`], so callers run
    /// this only after it returns `None`; the positional forms (`single ==
    /// None`) keep their own per-argument errors.
    fn validate_numeric(&self, names: &[&str], pos: Pos) -> Result<(), Error> {
        if self.single.is_none() {
            return Ok(());
        }
        for (i, comp) in self.comps.iter().enumerate() {
            // A degenerate `calc()` is a valid (NaN/infinity) channel value, so
            // it is left for the count/compute path rather than reported here.
            let numeric = matches!(comp, Value::Number(_) | Value::Slash(..)) || is_degenerate_calc(comp);
            if !numeric {
                return Err(Error::at(
                    format!(
                        "$channels: Expected {} to be a number, was {}.",
                        legacy_channel_name(names, i),
                        comp.to_css(false)
                    ),
                    pos,
                ));
            }
        }
        Ok(())
    }

    /// Validate that a single-argument channels list holds exactly three
    /// components for a legacy color space. dart-sass only enforces this when
    /// all channels are plain (a special/`none` channel preserves the call), so
    /// callers must run this *after* [`Channels::special_passthrough`] returns
    /// `None`. The three/four-positional forms (`single == None`) skip the
    /// check — their arity is validated by the argument count.
    fn validate_count(&self, space: &str, pos: Pos) -> Result<(), Error> {
        if let Some(single) = &self.single {
            if self.comps.len() != 3 {
                return Err(Error::at(
                    format!(
                        "$channels: The {space} color space has 3 channels but {} has {}.",
                        list_paren_css(single),
                        self.comps.len()
                    ),
                    pos,
                ));
            }
        }
        Ok(())
    }

    /// If this is a relative-color call (`rgb(from … )`), preserve it verbatim.
    /// dart-sass keeps the whole `from`-based form rather than computing it.
    fn relative_passthrough(&self, name: &str) -> Option<Value> {
        let is_relative = self
            .comps
            .first()
            .is_some_and(|v| matches!(v, Value::Str(s) if !s.quoted && s.text.eq_ignore_ascii_case("from")));
        if !is_relative {
            return None;
        }
        Some(self.verbatim_passthrough(name))
    }

    /// If these channels contain a special value (`var()`, `calc()`, …) or a
    /// `none` keyword, return the re-serialized passthrough call dart-sass
    /// would emit; otherwise `None` (the channels are all plain numbers and a
    /// real color should be computed, or a count error should be raised).
    fn special_passthrough(&self, name: &str) -> Option<Value> {
        let comps_special = self.comps.iter().any(is_special_legacy);
        let alpha_special = self.alpha.as_ref().is_some_and(is_special_legacy);
        let comps_none = self.comps.iter().any(is_none_keyword);
        let alpha_none = self.alpha.as_ref().is_some_and(is_none_keyword);
        let has_special = comps_special || alpha_special;
        let has_none = comps_none || alpha_none;
        if !has_special && !has_none {
            return None;
        }
        // A special function present forces the legacy comma form when the
        // channel count is exactly three (a `none` is simply one of the three
        // comma items). With a different count the *original* spelling is kept
        // verbatim (so the `/` alpha separator stays glued).
        if has_special {
            if self.comps.len() == 3 {
                let mut args: Vec<&Value> = self.comps.iter().collect();
                if let Some(a) = &self.alpha {
                    args.push(a);
                }
                return Some(special_call(name, &args));
            }
            return Some(self.verbatim_passthrough(name));
        }
        // No special function, only a `none`: the space/slash form is kept when
        // there are exactly three channels (hsl gives a bare hue a `deg`). A
        // wrong channel count falls through to the count error.
        if self.comps.len() != 3 {
            return None;
        }
        let is_hsl = name.eq_ignore_ascii_case("hsl") || name.eq_ignore_ascii_case("hsla");
        Some(self.none_verbatim(name, is_hsl))
    }

    /// Re-serialize a special-value passthrough whose channel count is not the
    /// canonical three. Prefer the *original* single channels value (which
    /// keeps the glued `/` alpha spelling) when the alpha was peeled from it or
    /// no alpha was supplied; otherwise reconstruct a comma call.
    fn verbatim_passthrough(&self, name: &str) -> Value {
        if let Some(single) = &self.single {
            if self.alpha.is_none() || self.alpha_split {
                return verbatim_call(name, single);
            }
        }
        let mut args: Vec<&Value> = self.comps.iter().collect();
        if let Some(a) = &self.alpha {
            args.push(a);
        }
        special_call(name, &args)
    }

    /// Serialize a legacy color call preserved because of a `none` channel, in
    /// the space-separated (slash-alpha) form. For hsl/hsla a bare-number hue
    /// gains an explicit `deg` (`hsl(180 none 50%)` → `hsl(180deg none 50%)`).
    fn none_verbatim(&self, name: &str, is_hsl: bool) -> Value {
        let hue = match &self.comps[0] {
            Value::Number(n) if is_hsl && n.unit.is_empty() => {
                format!("{}deg", fmt_num(n.value, false))
            }
            other => other.to_css(false),
        };
        let body = format!(
            "{} {} {}",
            hue,
            self.comps[1].to_css(false),
            self.comps[2].to_css(false)
        );
        let text = match &self.alpha {
            Some(a) => format!("{name}({body} / {})", a.to_css(false)),
            None => format!("{name}({body})"),
        };
        Value::Str(crate::value::SassStr { text, quoted: false })
    }
}

/// The components and optional alpha peeled off a one-argument channels value.
struct SplitChannels {
    /// The channel components (the alpha removed if one was found).
    comps: Vec<Value>,
    /// The alpha value, if a trailing `… / alpha` was peeled off.
    alpha: Option<Value>,
    /// Whether `alpha` was peeled from the trailing item of the original
    /// channels value (rather than being absent). This drives the verbatim
    /// passthrough, which prefers to re-serialize the *original* channels list
    /// when the channel count is wrong.
    alpha_split: bool,
}

/// Split a one-argument channels value into its components and optional alpha.
/// A space list contributes its items; a trailing slash-division on the last
/// item (`1 2 3 / 0.5`, parsed as `[1, 2, 3/0.5]`) peels off the alpha. When
/// the trailing slash crosses a special value (`var()`, `calc()`, `none`, …)
/// the division does not fold to a [`Value::Slash`] but to an unquoted string
/// like `var(--x)/0.4` or `3/none`; that trailing `X/Y` string is split at its
/// top-level slash into the last channel and the alpha (each becoming a plain
/// [`Number`] or an unquoted string).
fn split_channels(channels: &Value) -> SplitChannels {
    let no_split = |comps: Vec<Value>| SplitChannels {
        comps,
        alpha: None,
        alpha_split: false,
    };
    let Value::List(l) = channels else {
        return no_split(vec![channels.clone()]);
    };
    if l.sep != ListSep::Space {
        return no_split(l.items.clone());
    }
    let mut items: Vec<Value> = l.items.clone();
    // A trailing `n / a` slash-division shows up as a `Slash` whose textual
    // spelling contains `/`; recover the channel and alpha (each may carry a
    // unit, e.g. `50%/0.4`).
    if let Some(Value::Slash(_, repr)) = items.last() {
        if let Some((lhs, rhs)) = repr.split_once('/') {
            if let (Some(last), Some(alpha)) = (parse_number_token(lhs), parse_number_token(rhs)) {
                items.pop();
                items.push(Value::Number(last));
                return SplitChannels {
                    comps: items,
                    alpha: Some(Value::Number(alpha)),
                    alpha_split: true,
                };
            }
        }
    }
    // A trailing unquoted `X/Y` string: the slash crossed a special value (or a
    // `none`), so it evaluated to a string rather than a numeric `Slash`. Split
    // it at the top-level slash into the last channel and the alpha.
    if let Some(Value::Str(s)) = items.last() {
        if !s.quoted {
            if let Some(idx) = top_level_slash(&s.text) {
                let lhs = s.text[..idx].trim();
                let rhs = s.text[idx + 1..].trim();
                if !lhs.is_empty() && !rhs.is_empty() {
                    let last = channel_token(lhs);
                    let alpha = channel_token(rhs);
                    items.pop();
                    items.push(last);
                    return SplitChannels {
                        comps: items,
                        alpha: Some(alpha),
                        alpha_split: true,
                    };
                }
            }
        }
    }
    no_split(items)
}

/// Find the byte index of the (single) top-level `/` in an unquoted channel
/// string — the slash that separates the last channel from the alpha. Slashes
/// inside parentheses (e.g. `calc(a/b)`) are skipped. Returns the last such
/// slash, or `None` if there is none.
fn top_level_slash(s: &str) -> Option<usize> {
    let mut depth: i32 = 0;
    let mut found = None;
    for (i, c) in s.char_indices() {
        match c {
            '(' | '[' => depth += 1,
            ')' | ']' => depth -= 1,
            '/' if depth == 0 => found = Some(i),
            _ => {}
        }
    }
    found
}

/// Convert one side of a split `X/Y` channel string into a value: a numeric
/// token (`0.4`, `50%`) becomes a [`Number`]; a degenerate calculation
/// (`calc(NaN)`, `calc(infinity)`, `calc(-infinity)`) is recovered as a
/// [`Value::Calc`] so it folds/serializes like the original; anything else
/// (`var(--x)`, `none`, other `calc(…)`) becomes an unquoted string.
fn channel_token(s: &str) -> Value {
    if let Some(n) = parse_number_token(s) {
        // Reject a token that has leftover non-unit text (e.g. `1px2` would not
        // round-trip); `parse_number_token` only consumes the numeric prefix.
        if fmt_token_matches(&n, s) {
            return Value::Number(n);
        }
    }
    if let Some(inner) = degenerate_calc_str(s) {
        return Value::Calc(CalcNode::Str(inner));
    }
    Value::Str(crate::value::SassStr {
        text: s.to_string(),
        quoted: false,
    })
}

/// The inner constant of a `calc(<const>)` string when `<const>` is a
/// degenerate constant (`NaN`, `infinity`, `-infinity`), or `None` otherwise.
/// Used to recover a [`Value::Calc`] from a split channel/alpha string.
fn degenerate_calc_str(s: &str) -> Option<String> {
    let s = s.trim();
    if !s.to_ascii_lowercase().starts_with("calc(") || !s.ends_with(')') {
        return None;
    }
    let inner = s[5..s.len() - 1].trim();
    match inner.to_ascii_lowercase().as_str() {
        "nan" | "infinity" | "-infinity" => Some(inner.to_string()),
        _ => None,
    }
}

/// Whether `n` re-serializes to exactly `s` (so the whole token was numeric).
fn fmt_token_matches(n: &Number, s: &str) -> bool {
    format!("{}{}", fmt_num(n.value, false), n.unit) == s
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
    if let Some(verbatim) = channels.relative_passthrough("hsl") {
        return Ok(verbatim);
    }
    // A `none` channel (with no real special function) builds a modern legacy
    // hsl color rather than a verbatim string.
    if let Some(c) = legacy_none_color(&channels, ColorSpace::Hsl, pos)? {
        return Ok(c);
    }
    if let Some(verbatim) = channels.special_passthrough("hsl") {
        return Ok(verbatim);
    }
    // A degenerate `calc()` channel (`calc(infinity)`, `calc(-infinity)`,
    // `calc(NaN)`) keeps the whole call as a special hsl() spelling, with each
    // channel coerced per dart-sass's modern parsing (see `hsl_degenerate`).
    if channels.comps.len() == 3 && channels.comps.iter().any(is_degenerate_calc) {
        return hsl_degenerate(&channels, pos);
    }
    channels.validate_numeric(&["hue", "saturation", "lightness"], pos)?;
    channels.validate_count("hsl", pos)?;
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
    // is normalized to degrees in `[0, 360)`. The modern Hsl tag carries the
    // space so `color.space`/`color.channel` work; serialization uses the
    // classic comma form via `ModernColor::legacy_css`.
    let h_norm = h.rem_euclid(360.0);
    c.modern = Some(ModernColor {
        space: ColorSpace::Hsl,
        channels: [Some(h_norm), Some(s_pct), Some(l_pct)],
        alpha: Some(a),
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

/// Whether a value is a `calc()` that folds to a degenerate constant
/// (`infinity`, `-infinity`, `NaN`).
fn is_degenerate_calc(v: &Value) -> bool {
    matches!(v, Value::Calc(node) if degenerate_const(node).is_some())
}

/// Serialize an `hsl()`/`hsla()` call that carries a degenerate `calc()`
/// channel. dart-sass keeps the legacy comma spelling and coerces each
/// channel: the hue is reduced modulo 360 (so any non-finite becomes
/// `calc(NaN)`); saturation/lightness gain an implicit `%` (`calc(X * 1%)`),
/// with saturation additionally clamped at 0 (so `-infinity`/`NaN` → `0%`).
fn hsl_degenerate(channels: &Channels, pos: Pos) -> Result<Value, Error> {
    let hue = hsl_degenerate_hue(&channels.comps[0], pos)?;
    let sat = hsl_degenerate_pct(&channels.comps[1], true, pos)?;
    let light = hsl_degenerate_pct(&channels.comps[2], false, pos)?;
    let name = match &channels.alpha {
        Some(a) => {
            let av = alpha_value(a, pos)?;
            return Ok(Value::Str(crate::value::SassStr {
                text: format!("hsla({hue}, {sat}, {light}, {})", fmt_num(av, false)),
                quoted: false,
            }));
        }
        None => "hsl",
    };
    Ok(Value::Str(crate::value::SassStr {
        text: format!("{name}({hue}, {sat}, {light})"),
        quoted: false,
    }))
}

/// Serialize the hue channel of a degenerate hsl() call: a degenerate `calc()`
/// reduces modulo 360 to `NaN` (emitted as `calc(NaN)`); any plain value keeps
/// its normalized degree spelling.
fn hsl_degenerate_hue(v: &Value, pos: Pos) -> Result<String, Error> {
    if is_degenerate_calc(v) {
        // infinity/-infinity/NaN, all reduced mod 360 → NaN.
        return Ok("calc(NaN)".to_string());
    }
    let h = hsl_hue(v, pos)?;
    Ok(fmt_num(h.rem_euclid(360.0), false))
}

/// Serialize a saturation/lightness channel of a degenerate hsl() call. A
/// degenerate `calc()` is treated as a `%` value: saturation clamps a
/// non-positive/`NaN` result to `0%`, otherwise both emit `calc(X * 1%)`. A
/// plain number keeps its literal `%` spelling (saturation floored at 0).
fn hsl_degenerate_pct(v: &Value, is_saturation: bool, pos: Pos) -> Result<String, Error> {
    if let Value::Calc(node) = v {
        if let Some(c) = degenerate_const(node) {
            if is_saturation && (c.is_nan() || c <= 0.0) {
                return Ok("0%".to_string());
            }
            let token = if c.is_nan() {
                "NaN"
            } else if c.is_sign_negative() {
                "-infinity"
            } else {
                "infinity"
            };
            return Ok(format!("calc({token} * 1%)"));
        }
    }
    let raw = num(v, pos)?;
    let pct = if is_saturation {
        if raw.is_nan() {
            0.0
        } else {
            raw.max(0.0)
        }
    } else if raw.is_nan() {
        0.0
    } else {
        raw
    };
    Ok(format!("{}%", fmt_num(pct, false)))
}

/// The global `hwb()` function. It takes a single channels argument
/// (`hwb(h w b)`, `hwb(h w b / a)`). With all plain numeric channels it
/// converts HWB → sRGB → HSL and emits the `hsl()`/`hsla()` spelling that
/// dart-sass uses for legacy hwb colors. With a special value (`var()`,
/// `calc()`) or a `none` missing-channel keyword it preserves the call
/// verbatim, space-joined, with a bare numeric hue suffixed `deg`.
fn fn_hwb(pos_args: &[Value], named: &[(String, Value)], pos: Pos) -> Result<Value, Error> {
    let params = ["channels"];
    let n = pos_args.len() + named.len();
    if n != 1 {
        return Err(Error::at(
            format!("Only 1 argument allowed, but {n} were passed."),
            pos,
        ));
    }
    let channels = require(&params, pos_args, named, 0, "hwb", pos)?.clone();
    let SplitChannels { comps, alpha, .. } = split_channels(&channels);
    // A relative-color call (`hwb(from … )`) or a special function
    // (`var()`/`calc()`/…) anywhere preserves the *original* spelling verbatim
    // (a bare numeric hue keeps its bare form, the `/` alpha separator stays
    // glued), regardless of channel count.
    let is_relative = comps
        .first()
        .is_some_and(|v| matches!(v, Value::Str(s) if !s.quoted && s.text.eq_ignore_ascii_case("from")));
    let comps_func = comps.iter().any(is_special);
    let alpha_func = alpha.as_ref().is_some_and(is_special);
    if is_relative || comps_func || alpha_func {
        return Ok(verbatim_call("hwb", &channels));
    }
    // A non-number channel (a non-`from` keyword such as `c`, or a quoted
    // string) is reported before the channel-count check, matching dart-sass.
    for (i, comp) in comps.iter().enumerate() {
        let numeric = matches!(comp, Value::Number(_) | Value::Slash(..))
            || is_none_keyword(comp)
            || is_degenerate_calc(comp);
        if !numeric {
            return Err(Error::at(
                format!(
                    "$channels: Expected {} to be a number, was {}.",
                    legacy_channel_name(&["hue", "whiteness", "blackness"], i),
                    comp.to_css(false)
                ),
                pos,
            ));
        }
    }
    // Without a special function, the channel count must be exactly three.
    if comps.len() != 3 {
        return Err(Error::at(
            format!(
                "$channels: The hwb color space has 3 channels but {} has {}.",
                list_paren_css(&channels),
                comps.len()
            ),
            pos,
        ));
    }
    // A `none` missing-channel keyword (with otherwise plain numbers) builds a
    // modern legacy hwb color.
    let comps_none = comps.iter().any(is_none_keyword);
    let alpha_none = alpha.as_ref().is_some_and(is_none_keyword);
    if comps_none || alpha_none {
        let h = if is_none_keyword(&comps[0]) {
            None
        } else {
            modern_hue(&comps[0])
        };
        let w = modern_channel(&comps[1], 100.0);
        let bl = modern_channel(&comps[2], 100.0);
        let mc = ModernColor {
            space: ColorSpace::Hwb,
            channels: [h, w, bl],
            alpha: modern_alpha(alpha.as_ref()),
        };
        return Ok(Value::Color(make_modern(mc)));
    }
    let h = hsl_hue(&comps[0], pos)?;
    let w_pct = num(&comps[1], pos)?;
    let b_pct = num(&comps[2], pos)?;
    let a = match &alpha {
        Some(v) => alpha_value(v, pos)?,
        None => 1.0,
    };
    let mut out = hwb_to_color(h, w_pct, b_pct, a);
    // Carry the modern Hwb tag (so `color.space`/`color.channel` work);
    // serialization uses the classic hsl comma form via `legacy_css`.
    let h_norm = h.rem_euclid(360.0);
    out.modern = Some(ModernColor {
        space: ColorSpace::Hwb,
        channels: [Some(h_norm), Some(w_pct), Some(b_pct)],
        alpha: Some(a),
    });
    Ok(Value::Color(out))
}

/// Convert HWB (hue degrees, whiteness/blackness percentages) to an sRGB
/// color. Whiteness and blackness are normalized when their sum exceeds 100.
fn hwb_to_color(h: f64, w_pct: f64, b_pct: f64, a: f64) -> Color {
    let mut w = w_pct / 100.0;
    let mut b = b_pct / 100.0;
    if w + b > 1.0 {
        let sum = w + b;
        w /= sum;
        b /= sum;
    }
    let base = Color::from_hsl(h, 1.0, 0.5, a);
    let mix = |v: f64| ((v / 255.0) * (1.0 - w - b) + w) * 255.0;
    Color::rgb(mix(base.r), mix(base.g), mix(base.b), a)
}

/// The three channel names of a CIE/OK color space, for error messages.
fn lab_channel_names(name: &str) -> [&'static str; 3] {
    match name {
        "lch" | "oklch" => ["lightness", "chroma", "hue"],
        // lab / oklab
        _ => ["lightness", "a", "b"],
    }
}

/// The modern CIE/OK color functions `lab()`, `lch()`, `oklab()`, `oklch()`.
///
/// Full color-space math is out of scope: a fully numeric, well-formed call is
/// preserved verbatim (it is never reduced to another space here), and a call
/// containing a special value (`var()`/`calc()`), a `none` channel, or the
/// `from` relative-color keyword is likewise preserved verbatim. Malformed
/// calls raise the same validation errors as dart-sass.
fn fn_lab_family(
    name: &str,
    pos_args: &[Value],
    named: &[(String, Value)],
    pos: Pos,
) -> Result<Value, Error> {
    let params = ["channels"];
    let n = pos_args.len() + named.len();
    if n == 0 {
        return Err(Error::at("Missing argument $channels.".to_string(), pos));
    }
    if n > 1 {
        return Err(Error::at(
            format!("Only 1 argument allowed, but {n} were passed."),
            pos,
        ));
    }
    let channels = require(&params, pos_args, named, 0, name, pos)?.clone();
    // A comma-separated or bracketed list is not a valid channels list.
    if let Value::List(l) = &channels {
        let comma = l.sep == ListSep::Comma;
        if l.bracketed || comma {
            let kind = if l.bracketed && comma {
                "an unbracketed, space- or slash-separated list"
            } else if l.bracketed {
                "an unbracketed list"
            } else {
                "a space- or slash-separated list"
            };
            let shown = if l.bracketed {
                channels.to_css(false)
            } else {
                list_paren_css(&channels)
            };
            return Err(Error::at(format!("$channels: Expected {kind}, was {shown}"), pos));
        }
        if l.items.is_empty() {
            return Err(Error::at(
                "$channels: Color component list may not be empty.".to_string(),
                pos,
            ));
        }
    }
    let SplitChannels { comps, alpha, .. } = split_channels(&channels);
    // A relative-color call (`lab(from … )`) or a special function
    // (`var()`/non-degenerate `calc()`) is preserved verbatim. A `none`
    // channel is computed (it produces a missing channel).
    let is_relative = comps
        .first()
        .is_some_and(|v| matches!(v, Value::Str(s) if !s.quoted && s.text.eq_ignore_ascii_case("from")));
    let special = |v: &Value| is_special(v) && !is_degenerate_calc(v);
    let has_special = comps.iter().any(special) || alpha.as_ref().is_some_and(special);
    if is_relative || has_special {
        return Ok(verbatim_call(name, &channels));
    }
    // All-plain channels: validate count, types, and units like dart-sass.
    let names = lab_channel_names(name);
    if comps.len() != 3 {
        return Err(Error::at(
            format!(
                "$channels: The {} color space has 3 channels but {} has {}.",
                name,
                list_paren_css(&channels),
                comps.len()
            ),
            pos,
        ));
    }
    let is_hue = |i: usize| matches!(name, "lch" | "oklch") && i == 2;
    for (i, comp) in comps.iter().enumerate() {
        if is_none_keyword(comp) || is_degenerate_calc(comp) {
            continue;
        }
        match comp {
            Value::Number(num) => {
                if is_hue(i) {
                    let ok =
                        num.unit.is_empty() || matches!(num.unit.as_str(), "deg" | "grad" | "rad" | "turn");
                    if !ok {
                        return Err(Error::at(
                            format!(
                                "$hue: Expected {} to have an angle unit (deg, grad, rad, turn).",
                                num.to_css(false)
                            ),
                            pos,
                        ));
                    }
                } else if !num.unit.is_empty() && num.unit != "%" {
                    return Err(Error::at(
                        format!(
                            "${}: Expected {} to have unit \"%\" or no units.",
                            names[i],
                            num.to_css(false)
                        ),
                        pos,
                    ));
                }
            }
            Value::Slash(..) => {}
            other => {
                return Err(Error::at(
                    format!(
                        "$channels: Expected {} channel to be a number, was {}.",
                        names[i],
                        other.to_css(false)
                    ),
                    pos,
                ));
            }
        }
    }
    if let Some(a) = &alpha {
        // Validate the alpha's unit (errors on e.g. `0.4px`).
        if !is_none_keyword(a) {
            alpha_value(a, pos)?;
        }
    }
    // Compute the modern color. Lightness is clamped (lab/lch 0..100, oklab/oklch
    // 0..1); chroma is floored at 0; a/b and the hue are unclamped.
    let (space, l_max, l_base) = match name {
        "lab" => (ColorSpace::Lab, 100.0, 100.0),
        "lch" => (ColorSpace::Lch, 100.0, 100.0),
        "oklab" => (ColorSpace::Oklab, 1.0, 1.0),
        _ => (ColorSpace::Oklch, 1.0, 1.0),
    };
    // A degenerate `calc()` in a non-lightness channel is preserved verbatim
    // (dart-sass keeps the `calc(...)` text); only the clamped lightness and
    // alpha are folded.
    if comps[1..].iter().any(is_degenerate_calc) {
        return Ok(lab_degenerate(name, &comps, alpha.as_ref(), l_base, l_max, pos));
    }
    let is_polar = matches!(name, "lch" | "oklch");
    // Percentage references per CSS Color 4: lab a/b 100% = 125, oklab a/b
    // 100% = 0.4, lch chroma 100% = 150, oklch chroma 100% = 0.4.
    let (ab_base, chroma_base) = match name {
        "lab" => (125.0, 0.0),
        "lch" => (0.0, 150.0),
        "oklab" => (0.4, 0.0),
        _ => (0.0, 0.4), // oklch
    };
    let l = modern_channel(&comps[0], l_base).map(|v| v.clamp(0.0, l_max));
    let c1;
    let c2;
    if is_polar {
        // [lightness, chroma, hue]
        c1 = modern_channel(&comps[1], chroma_base).map(|v| v.max(0.0));
        c2 = modern_hue(&comps[2]);
    } else {
        // [lightness, a, b]
        c1 = modern_channel(&comps[1], ab_base);
        c2 = modern_channel(&comps[2], ab_base);
    }
    let mc = ModernColor {
        space,
        channels: [l, c1, c2],
        alpha: modern_alpha(alpha.as_ref()),
    };
    Ok(Value::Color(make_modern(mc)))
}

/// Serialize a list value wrapped in parentheses, as dart-sass does in its
/// channel-list error messages (`(1%, 2, 3)`, `(1% 2)`).
fn list_paren_css(v: &Value) -> String {
    format!("({})", v.to_css(false))
}

/// The known predefined color spaces accepted by `color()`. All have three
/// channels, so the channel-count message is uniform.
fn is_known_color_space(name: &str) -> bool {
    matches!(
        name,
        "srgb"
            | "srgb-linear"
            | "display-p3"
            | "display-p3-linear"
            | "a98-rgb"
            | "prophoto-rgb"
            | "rec2020"
            | "xyz"
            | "xyz-d50"
            | "xyz-d65"
    )
}

/// The `color()` function for predefined color spaces
/// (`color(srgb 0.1 0.2 0.3)`). Full color-space math is out of scope: a
/// well-formed call (and any special/`none`/`from`-relative call) is preserved
/// verbatim, while malformed calls raise dart-sass's validation errors.
fn fn_color(pos_args: &[Value], named: &[(String, Value)], pos: Pos) -> Result<Value, Error> {
    let params = ["description"];
    let n = pos_args.len() + named.len();
    if n == 0 {
        return Err(Error::at("Missing argument $description.".to_string(), pos));
    }
    if n > 1 {
        return Err(Error::at(
            format!("Only 1 argument allowed, but {n} were passed."),
            pos,
        ));
    }
    let desc = require(&params, pos_args, named, 0, "color", pos)?.clone();
    if let Value::List(l) = &desc {
        let comma = l.sep == ListSep::Comma;
        if l.bracketed || comma {
            let kind = if l.bracketed && comma {
                "an unbracketed, space- or slash-separated list"
            } else if l.bracketed {
                "an unbracketed list"
            } else {
                "a space- or slash-separated list"
            };
            let shown = if l.bracketed {
                desc.to_css(false)
            } else {
                list_paren_css(&desc)
            };
            return Err(Error::at(
                format!("$description: Expected {kind}, was {shown}"),
                pos,
            ));
        }
    }
    let SplitChannels {
        comps: items, alpha, ..
    } = split_channels(&desc);
    // The first item names the color space; the rest are channels.
    let space = items.first().ok_or_else(|| {
        Error::at(
            "$description: Color component list may not be empty.".to_string(),
            pos,
        )
    })?;
    let space_name = match space {
        Value::Str(s) if !s.quoted => s.text.clone(),
        Value::Str(s) => {
            return Err(Error::at(
                format!("$description: Expected \"{}\" to be an unquoted string.", s.text),
                pos,
            ));
        }
        other => {
            return Err(Error::at(
                format!("$description: {} is not a string.", other.to_css(false)),
                pos,
            ));
        }
    };
    let channels = &items[1..];
    // A relative-color call (`color(from … )`) or any special/`none` channel
    // is preserved verbatim. A *degenerate* `calc()` (`calc(NaN)`/`infinity`)
    // is not special here: dart-sass folds it to a finite/NaN channel value and
    // parses the color, so it flows through validation and the modern
    // (space-around-`/`) serialization below.
    let is_relative = space_name.eq_ignore_ascii_case("from");
    let special_chan = |v: &Value| is_special(v) && !is_degenerate_calc(v);
    let has_special = channels.iter().any(special_chan) || alpha.as_ref().is_some_and(special_chan);
    if is_relative || has_special {
        return Ok(verbatim_call("color", &desc));
    }
    if !is_known_color_space(&space_name) {
        return Err(Error::at(
            format!("$description: Unknown color space \"{space_name}\"."),
            pos,
        ));
    }
    // Type-check each supplied channel (with its index-based name) before the
    // count check, matching dart-sass (`color(srgb (0.1 0.2 0.3))` reports a
    // non-number channel rather than a wrong count). A degenerate `calc()` is
    // accepted as a number channel.
    let names = ["red", "green", "blue"];
    for (i, comp) in channels.iter().enumerate() {
        let name = names.get(i).copied().unwrap_or("");
        if is_none_keyword(comp) || is_degenerate_calc(comp) {
            continue;
        }
        match comp {
            Value::Number(num) => {
                if !num.unit.is_empty() && num.unit != "%" {
                    return Err(Error::at(
                        format!(
                            "${name}: Expected {} to have unit \"%\" or no units.",
                            num.to_css(false)
                        ),
                        pos,
                    ));
                }
            }
            Value::Slash(..) => {}
            Value::Calc(_) if is_degenerate_calc(comp) => {}
            other => {
                return Err(Error::at(
                    format!(
                        "$description: Expected {name} channel to be a number, was {}.",
                        other.to_css(false)
                    ),
                    pos,
                ));
            }
        }
    }
    if channels.len() != 3 {
        return Err(Error::at(
            format!(
                "$description: The {} color space has 3 channels but {} has {}.",
                space_name,
                color_desc_css(&desc),
                channels.len()
            ),
            pos,
        ));
    }
    if let Some(a) = &alpha {
        if !is_none_keyword(a) {
            alpha_value(a, pos)?;
        }
    }
    // `display-p3-linear` is accepted but not a real CSS Color 4 space in
    // dart-sass; it is preserved verbatim.
    let space = match predefined_space(&space_name) {
        Some(s) => s,
        None => return Ok(verbatim_call("color", &desc)),
    };
    // A degenerate `calc()` channel (`calc(infinity)`/`calc(-infinity)`/
    // `calc(NaN)`) is preserved verbatim in `color()`'s channels (dart-sass
    // keeps the `calc(...)` text), while a degenerate alpha is folded.
    let degenerate =
        channels.iter().any(is_degenerate_calc) || alpha.as_ref().is_some_and(is_degenerate_calc);
    if degenerate {
        return Ok(modern_color(&space_name, channels, alpha.as_ref(), pos));
    }
    // Compute the color: predefined `color()` spaces store red/green/blue (and
    // xyz x/y/z) channels in 0..1 with no clamping.
    let ch = [
        modern_channel(&channels[0], 1.0),
        modern_channel(&channels[1], 1.0),
        modern_channel(&channels[2], 1.0),
    ];
    let mc = ModernColor {
        space,
        channels: ch,
        alpha: modern_alpha(alpha.as_ref()),
    };
    Ok(Value::Color(make_modern(mc)))
}

/// Serialize a `color()` whose channels contain a degenerate `calc()` constant
/// preserved verbatim: the space name, each channel via `to_css`, and—if the
/// (folded) alpha is not fully opaque—a space-padded `/ alpha`. A degenerate
/// `calc()` alpha folds (`infinity` → 1 = opaque, `-infinity`/`NaN` → 0).
fn modern_color(space: &str, channels: &[Value], alpha: Option<&Value>, pos: Pos) -> Value {
    let a = match alpha {
        Some(v) if is_none_keyword(v) => 1.0,
        Some(v) => alpha_value(v, pos).unwrap_or(1.0),
        None => 1.0,
    };
    let body: Vec<String> = channels.iter().map(|v| v.to_css(false)).collect();
    let body = body.join(" ");
    let text = if (a - 1.0).abs() < f64::EPSILON {
        format!("color({space} {body})")
    } else {
        format!("color({space} {body} / {})", fmt_num(a, false))
    };
    Value::Str(crate::value::SassStr { text, quoted: false })
}

/// Serialize a `color()` description for its channel-count error message:
/// wrapped in parentheses for a multi-item list, bare for a single value
/// (`color(srgb)` → `srgb`).
fn color_desc_css(desc: &Value) -> String {
    match desc {
        Value::List(l) if l.items.len() > 1 => list_paren_css(desc),
        _ => desc.to_css(false),
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
    // A $method (CSS Color 4 interpolation method) triggers real color-space
    // interpolation in the named space; without it, the legacy mix runs (which
    // requires both colors to be legacy).
    if let Some(method) = arg(&params, pos_args, named, 3) {
        let (space, hue_method) = validate_mix_method(method, pos)?;
        return Ok(Value::Color(interpolate_mix(&c1, &c2, weight, space, hue_method)));
    }
    for (i, c) in [&c1, &c2].iter().enumerate() {
        if !color_space_of(c).is_legacy() {
            return Err(Error::at(
                format!(
                    "$color{}: To use color.mix() with non-legacy color {}, you must provide a $method.",
                    i + 1,
                    c.to_css(false)
                ),
                pos,
            ));
        }
    }
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

/// The color-interpolation spaces dart-sass accepts for `mix()`'s `$method`,
/// with whether each is *polar* (carries a hue channel: a hue interpolation
/// method may follow it).
fn mix_method_space(name: &str) -> Option<bool> {
    match name {
        "hsl" | "hwb" | "lch" | "oklch" => Some(true),
        "rgb" | "srgb" | "srgb-linear" | "display-p3" | "a98-rgb" | "prophoto-rgb" | "rec2020" | "xyz"
        | "xyz-d50" | "xyz-d65" | "lab" | "oklab" => Some(false),
        _ => None,
    }
}

/// The hue interpolation method for a polar `mix()` `$method`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum HueMethod {
    Shorter,
    Longer,
    Increasing,
    Decreasing,
}

/// Validate a `mix()` `$method` value (a CSS Color 4 interpolation method:
/// `srgb`, `oklch longer hue`, …). Errors match dart-sass exactly. Returns the
/// resolved interpolation space and hue method.
fn validate_mix_method(method: &Value, pos: Pos) -> Result<(ColorSpace, HueMethod), Error> {
    let err = |msg: String| Err(Error::at(msg, pos));
    // The method is either a bare color-space string or a space-separated
    // list `space [<hue> hue]`.
    let items: Vec<&Value> = match method {
        Value::List(l) if l.sep == ListSep::Space => l.items.iter().collect(),
        single => vec![single],
    };
    let space_val = items[0];
    let space = match space_val {
        Value::Str(s) if !s.quoted => s.text.clone(),
        Value::Str(s) => {
            return err(format!(
                "$method: Expected \"{}\" to be an unquoted string.",
                s.text
            ));
        }
        other => {
            return err(format!("$method: {} is not a string.", other.to_css(false)));
        }
    };
    let space = space.to_ascii_lowercase();
    let polar = match mix_method_space(&space) {
        Some(p) => p,
        None => return err(format!("$method: Unknown color space \"{space}\".")),
    };
    let cspace = ColorSpace::from_name(&space).unwrap_or(ColorSpace::Srgb);
    // A bare color space (no trailing hue method) is always valid.
    if items.len() == 1 {
        return Ok((cspace, HueMethod::Shorter));
    }
    // `space <hue-method> hue`: the second token names a hue interpolation
    // method and the list must end with the literal `hue`.
    let method_token = match items[1] {
        Value::Str(s) if !s.quoted => s.text.clone(),
        // A parenthesized list shows wrapped in parens (`(decreasing hue)`).
        Value::List(_) => return err(format!("$method: {} is not a string.", list_paren_css(items[1]))),
        other => return err(format!("$method: {} is not a string.", other.to_css(false))),
    };
    // The hue-method keyword is validated before the trailing `hue` keyword,
    // matching dart-sass's error order.
    let hue_method = match method_token.to_ascii_lowercase().as_str() {
        "shorter" => HueMethod::Shorter,
        "longer" => HueMethod::Longer,
        "increasing" => HueMethod::Increasing,
        "decreasing" => HueMethod::Decreasing,
        "specified" => return err("$method: Unknown hue interpolation method specified.".to_string()),
        other => return err(format!("$method: Unknown hue interpolation method {other}.")),
    };
    // The list must end with an unquoted `hue` keyword.
    let last = items[items.len() - 1];
    let last_is_hue = matches!(last, Value::Str(s) if !s.quoted && s.text.eq_ignore_ascii_case("hue"));
    if items.len() == 2 {
        // `space <method>` with no trailing `hue`.
        return err(format!(
            "$method: Expected unquoted string \"hue\" after ({}).",
            method.to_css(false)
        ));
    }
    if !last_is_hue {
        return err(format!(
            "$method: Expected unquoted string \"hue\" at the end of ({}), was {}.",
            method.to_css(false),
            last.to_css(false)
        ));
    }
    // A hue method may not be applied to a rectangular (non-polar) space.
    if !polar {
        return err(format!(
            "$method: Hue interpolation method \"HueInterpolationMethod.{method_token} hue\" \
             may not be set for rectangular color space {space}."
        ));
    }
    Ok((cspace, hue_method))
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

// =====================================================================
// CSS Color 4 color-space conversion engine.
//
// Channels are stored in each space's canonical units:
//   * rgb/srgb/srgb-linear/display-p3/a98-rgb/prophoto-rgb/rec2020:
//       red/green/blue in 0..=1 (the legacy `rgb` space additionally keeps a
//       0..=255 mirror in Color::{r,g,b}).
//   * hsl: hue deg, saturation/lightness in percent (0..=100).
//   * hwb: hue deg, whiteness/blackness in percent (0..=100).
//   * xyz / xyz-d50: x/y/z unbounded.
//   * lab: lightness 0..=100, a/b unbounded; lch: lightness 0..=100,
//       chroma >= 0, hue deg.
//   * oklab: lightness 0..=1, a/b unbounded; oklch: lightness 0..=1,
//       chroma >= 0, hue deg.
// A `None` channel is a missing channel; conversions treat it as 0.
// =====================================================================

use crate::value::{ColorSpace, ModernColor};

/// Replace a missing (`None`) channel with `0.0` for arithmetic.
fn z(v: Option<f64>) -> f64 {
    v.unwrap_or(0.0)
}

/// Matrix * vector (3x3 * 3).
fn mat3(m: [[f64; 3]; 3], v: [f64; 3]) -> [f64; 3] {
    [
        m[0][0] * v[0] + m[0][1] * v[1] + m[0][2] * v[2],
        m[1][0] * v[0] + m[1][1] * v[1] + m[1][2] * v[2],
        m[2][0] * v[0] + m[2][1] * v[1] + m[2][2] * v[2],
    ]
}

// ---- sRGB transfer functions ----------------------------------------

fn srgb_to_linear(c: f64) -> f64 {
    let sign = c.signum();
    let abs = c.abs();
    if abs <= 0.04045 {
        c / 12.92
    } else {
        sign * ((abs + 0.055) / 1.055).powf(2.4)
    }
}

fn linear_to_srgb(c: f64) -> f64 {
    let sign = c.signum();
    let abs = c.abs();
    if abs <= 0.0031308 {
        c * 12.92
    } else {
        sign * (1.055 * abs.powf(1.0 / 2.4) - 0.055)
    }
}

// ---- rec2020 transfer functions -------------------------------------

fn rec2020_to_linear(c: f64) -> f64 {
    const ALPHA: f64 = 1.09929682680944;
    const BETA: f64 = 0.018053968510807;
    let sign = c.signum();
    let abs = c.abs();
    if abs < BETA * 4.5 {
        c / 4.5
    } else {
        sign * ((abs + ALPHA - 1.0) / ALPHA).powf(1.0 / 0.45)
    }
}

fn linear_to_rec2020(c: f64) -> f64 {
    const ALPHA: f64 = 1.09929682680944;
    const BETA: f64 = 0.018053968510807;
    let sign = c.signum();
    let abs = c.abs();
    if abs < BETA {
        c * 4.5
    } else {
        sign * (ALPHA * abs.powf(0.45) - (ALPHA - 1.0))
    }
}

// ---- a98-rgb transfer functions -------------------------------------

fn a98_to_linear(c: f64) -> f64 {
    c.signum() * c.abs().powf(563.0 / 256.0)
}

fn linear_to_a98(c: f64) -> f64 {
    c.signum() * c.abs().powf(256.0 / 563.0)
}

// ---- prophoto-rgb transfer functions --------------------------------

fn prophoto_to_linear(c: f64) -> f64 {
    const ET2: f64 = 16.0 / 512.0;
    let sign = c.signum();
    let abs = c.abs();
    if abs <= ET2 {
        c / 16.0
    } else {
        sign * abs.powf(1.8)
    }
}

fn linear_to_prophoto(c: f64) -> f64 {
    const ET: f64 = 1.0 / 512.0;
    let sign = c.signum();
    let abs = c.abs();
    if abs >= ET {
        sign * abs.powf(1.0 / 1.8)
    } else {
        c * 16.0
    }
}

// ---- linear-light RGB <-> XYZ (D65) matrices ------------------------

// All matrices are byte-for-byte the values dart-sass uses in
// `lib/src/value/color/conversions.dart`, so conversions round-trip exactly.
const SRGB_TO_XYZ: [[f64; 3]; 3] = [
    [0.4123907992659595, 0.35758433938387796, 0.1804807884018343],
    [0.21263900587151036, 0.7151686787677559, 0.07219231536073371],
    [0.01933081871559185, 0.11919477979462598, 0.9505321522496606],
];
const XYZ_TO_SRGB: [[f64; 3]; 3] = [
    [3.2409699419045213, -1.5373831775700935, -0.4986107602930033],
    [-0.9692436362808798, 1.8759675015077206, 0.04155505740717561],
    [0.0556300796969936, -0.20397695888897657, 1.0569715142428786],
];

const P3_TO_XYZ: [[f64; 3]; 3] = [
    [0.48657094864821626, 0.26566769316909294, 0.1982172852343625],
    [0.22897456406974884, 0.6917385218365062, 0.079286914093745],
    [0.0, 0.04511338185890257, 1.0439443689009757],
];
const XYZ_TO_P3: [[f64; 3]; 3] = [
    [2.4934969119414245, -0.9313836179191236, -0.40271078445071684],
    [-0.8294889695615749, 1.7626640603183468, 0.02362468584194359],
    [0.03584583024378433, -0.0761723892680417, 0.9568845240076873],
];

const A98_TO_XYZ: [[f64; 3]; 3] = [
    [0.5766690429101308, 0.18555823790654627, 0.18822864623499472],
    [0.29734497525053616, 0.627363566255466, 0.07529145849399789],
    [0.02703136138641237, 0.07068885253582714, 0.9913375368376389],
];
const XYZ_TO_A98: [[f64; 3]; 3] = [
    [2.041587903810746, -0.5650069742788596, -0.3447313507783295],
    [-0.9692436362808798, 1.8759675015077206, 0.04155505740717561],
    [0.01344428063203102, -0.11836239223101823, 1.0151749943912054],
];

const PROPHOTO_TO_XYZ_D50: [[f64; 3]; 3] = [
    [0.7977666449006423, 0.13518129740053308, 0.0313477341283922],
    [0.2880748288194013, 0.711835234241873, 0.00008993693872564],
    [0.0, 0.0, 0.8251046025104602],
];
const XYZ_D50_TO_PROPHOTO: [[f64; 3]; 3] = [
    [1.3457868816471583, -0.25557208737979464, -0.05110186497554526],
    [-0.5446307051249019, 1.5082477428451468, 0.02052744743642139],
    [0.0, 0.0, 1.2119675456389452],
];

const REC2020_TO_XYZ: [[f64; 3]; 3] = [
    [0.6369580483012913, 0.14461690358620838, 0.16888097516417205],
    [0.26270021201126703, 0.677998071518871, 0.05930171646986194],
    [0.0, 0.0280726930490875, 1.0609850577107909],
];
const XYZ_TO_REC2020: [[f64; 3]; 3] = [
    [1.7166511879712676, -0.3556707837763924, -0.2533662813736598],
    [-0.666684351832489, 1.616481236634939, 0.01576854581391113],
    [0.01763985744531091, -0.04277061325780865, 0.942103121235474],
];

// Bradford chromatic adaptation D65<->D50.
const D65_TO_D50: [[f64; 3]; 3] = [
    [1.0479297925449966, 0.02294687060160952, -0.05019226628920519],
    [0.02962780877005567, 0.99043442675388, -0.01707379906341879],
    [-0.00924304064620452, 0.01505519149029816, 0.751874281428137],
];
const D50_TO_D65: [[f64; 3]; 3] = [
    [0.9554734214880752, -0.02309845494876452, 0.06325924320057065],
    [-0.02836970933386358, 1.0099953980813041, 0.0210414411919173],
    [0.01231401486448199, -0.02050764929889898, 1.330365926242124],
];

// oklab matrices.
const XYZ_TO_LMS: [[f64; 3]; 3] = [
    [0.819022437996703, 0.36190626005289034, -0.12887378152098788],
    [0.03298365393238846, 0.9292868615863433, 0.03614466635064235],
    [0.0481771893596242, 0.2642395317527308, 0.6335478284694308],
];
const LMS_TO_OKLAB: [[f64; 3]; 3] = [
    [0.210454268309314, 0.7936177747023054, -0.0040720430116193],
    [1.9779985324311684, -2.42859224204858, 0.450593709617411],
    [0.0259040424655478, 0.7827717124575296, -0.8086757549230774],
];
const LMS_TO_XYZ: [[f64; 3]; 3] = [
    [1.2268798758459243, -0.5578149944602171, 0.2813910456659646],
    [-0.04057574521480084, 1.1122868032803173, -0.07171105806551635],
    [-0.07637293667466007, -0.42149333240224324, 1.5869240198367818],
];
const OKLAB_TO_LMS: [[f64; 3]; 3] = [
    [1.0, 0.3963377773761749, 0.2158037573099136],
    [0.9999999999999998, -0.10556134581565854, -0.06385417282581334],
    [0.9999999999999999, -0.0894841775298118, -1.2914855480194094],
];

// ---- per-space <-> XYZ D65 conversions ------------------------------

/// Convert a color's three (missing-as-0) channels in `space` to XYZ D65.
fn to_xyz_d65(space: ColorSpace, c: [f64; 3]) -> [f64; 3] {
    match space {
        ColorSpace::Rgb => {
            let lin = [
                srgb_to_linear(c[0] / 255.0),
                srgb_to_linear(c[1] / 255.0),
                srgb_to_linear(c[2] / 255.0),
            ];
            mat3(SRGB_TO_XYZ, lin)
        }
        ColorSpace::Srgb => {
            let lin = [srgb_to_linear(c[0]), srgb_to_linear(c[1]), srgb_to_linear(c[2])];
            mat3(SRGB_TO_XYZ, lin)
        }
        ColorSpace::SrgbLinear => mat3(SRGB_TO_XYZ, c),
        ColorSpace::DisplayP3 => {
            let lin = [srgb_to_linear(c[0]), srgb_to_linear(c[1]), srgb_to_linear(c[2])];
            mat3(P3_TO_XYZ, lin)
        }
        ColorSpace::DisplayP3Linear => mat3(P3_TO_XYZ, c),
        ColorSpace::A98Rgb => {
            let lin = [a98_to_linear(c[0]), a98_to_linear(c[1]), a98_to_linear(c[2])];
            mat3(A98_TO_XYZ, lin)
        }
        ColorSpace::ProphotoRgb => {
            let lin = [
                prophoto_to_linear(c[0]),
                prophoto_to_linear(c[1]),
                prophoto_to_linear(c[2]),
            ];
            mat3(D50_TO_D65, mat3(PROPHOTO_TO_XYZ_D50, lin))
        }
        ColorSpace::Rec2020 => {
            let lin = [
                rec2020_to_linear(c[0]),
                rec2020_to_linear(c[1]),
                rec2020_to_linear(c[2]),
            ];
            mat3(REC2020_TO_XYZ, lin)
        }
        ColorSpace::XyzD65 => c,
        ColorSpace::XyzD50 => mat3(D50_TO_D65, c),
        ColorSpace::Hsl => to_xyz_d65(ColorSpace::Rgb, hsl_to_rgb255(c)),
        ColorSpace::Hwb => to_xyz_d65(ColorSpace::Rgb, hwb_to_rgb255(c)),
        ColorSpace::Lab => mat3(D50_TO_D65, lab_to_xyz(c)),
        ColorSpace::Lch => to_xyz_d65(ColorSpace::Lab, lch_to_lab(c)),
        ColorSpace::Oklab => oklab_to_xyz(c),
        ColorSpace::Oklch => oklab_to_xyz(lch_to_lab(c)),
    }
}

/// Convert XYZ D65 to a color's three channels in `space`.
fn from_xyz_d65(space: ColorSpace, xyz: [f64; 3]) -> [f64; 3] {
    match space {
        ColorSpace::Rgb => {
            let lin = mat3(XYZ_TO_SRGB, xyz);
            [
                linear_to_srgb(lin[0]) * 255.0,
                linear_to_srgb(lin[1]) * 255.0,
                linear_to_srgb(lin[2]) * 255.0,
            ]
        }
        ColorSpace::Srgb => {
            let lin = mat3(XYZ_TO_SRGB, xyz);
            [
                linear_to_srgb(lin[0]),
                linear_to_srgb(lin[1]),
                linear_to_srgb(lin[2]),
            ]
        }
        ColorSpace::SrgbLinear => mat3(XYZ_TO_SRGB, xyz),
        ColorSpace::DisplayP3 => {
            let lin = mat3(XYZ_TO_P3, xyz);
            [
                linear_to_srgb(lin[0]),
                linear_to_srgb(lin[1]),
                linear_to_srgb(lin[2]),
            ]
        }
        ColorSpace::DisplayP3Linear => mat3(XYZ_TO_P3, xyz),
        ColorSpace::A98Rgb => {
            let lin = mat3(XYZ_TO_A98, xyz);
            [
                linear_to_a98(lin[0]),
                linear_to_a98(lin[1]),
                linear_to_a98(lin[2]),
            ]
        }
        ColorSpace::ProphotoRgb => {
            let lin = mat3(XYZ_D50_TO_PROPHOTO, mat3(D65_TO_D50, xyz));
            [
                linear_to_prophoto(lin[0]),
                linear_to_prophoto(lin[1]),
                linear_to_prophoto(lin[2]),
            ]
        }
        ColorSpace::Rec2020 => {
            let lin = mat3(XYZ_TO_REC2020, xyz);
            [
                linear_to_rec2020(lin[0]),
                linear_to_rec2020(lin[1]),
                linear_to_rec2020(lin[2]),
            ]
        }
        ColorSpace::XyzD65 => xyz,
        ColorSpace::XyzD50 => mat3(D65_TO_D50, xyz),
        ColorSpace::Hsl => rgb255_to_hsl(from_xyz_d65(ColorSpace::Rgb, xyz)),
        ColorSpace::Hwb => rgb255_to_hwb(from_xyz_d65(ColorSpace::Rgb, xyz)),
        ColorSpace::Lab => xyz_to_lab(mat3(D65_TO_D50, xyz)),
        ColorSpace::Lch => lab_to_lch(xyz_to_lab(mat3(D65_TO_D50, xyz))),
        ColorSpace::Oklab => xyz_to_oklab(xyz),
        ColorSpace::Oklch => lab_to_lch(xyz_to_oklab(xyz)),
    }
}

// ---- lab / lch (D50) ------------------------------------------------

const D50: [f64; 3] = [0.3457 / 0.3585, 1.0, (1.0 - 0.3457 - 0.3585) / 0.3585];

fn lab_to_xyz(lab: [f64; 3]) -> [f64; 3] {
    const KAPPA: f64 = 24389.0 / 27.0;
    const EPSILON: f64 = 216.0 / 24389.0;
    let fy = (lab[0] + 16.0) / 116.0;
    let fx = lab[1] / 500.0 + fy;
    let fz = fy - lab[2] / 200.0;
    let x = if fx.powi(3) > EPSILON {
        fx.powi(3)
    } else {
        (116.0 * fx - 16.0) / KAPPA
    };
    let y = if lab[0] > KAPPA * EPSILON {
        ((lab[0] + 16.0) / 116.0).powi(3)
    } else {
        lab[0] / KAPPA
    };
    let zz = if fz.powi(3) > EPSILON {
        fz.powi(3)
    } else {
        (116.0 * fz - 16.0) / KAPPA
    };
    [x * D50[0], y * D50[1], zz * D50[2]]
}

fn xyz_to_lab(xyz: [f64; 3]) -> [f64; 3] {
    const KAPPA: f64 = 24389.0 / 27.0;
    const EPSILON: f64 = 216.0 / 24389.0;
    let xr = xyz[0] / D50[0];
    let yr = xyz[1] / D50[1];
    let zr = xyz[2] / D50[2];
    let f = |t: f64| {
        if t > EPSILON {
            t.cbrt()
        } else {
            (KAPPA * t + 16.0) / 116.0
        }
    };
    let fx = f(xr);
    let fy = f(yr);
    let fz = f(zr);
    [116.0 * fy - 16.0, 500.0 * (fx - fy), 200.0 * (fy - fz)]
}

/// lch -> lab (lightness, chroma, hue-deg) -> (lightness, a, b).
fn lch_to_lab(lch: [f64; 3]) -> [f64; 3] {
    let h = lch[2].to_radians();
    [lch[0], lch[1] * h.cos(), lch[1] * h.sin()]
}

/// lab -> lch.
fn lab_to_lch(lab: [f64; 3]) -> [f64; 3] {
    let c = (lab[1] * lab[1] + lab[2] * lab[2]).sqrt();
    let mut h = lab[2].atan2(lab[1]).to_degrees();
    if h < 0.0 {
        h += 360.0;
    }
    [lab[0], c, h]
}

// ---- oklab / oklch --------------------------------------------------

fn xyz_to_oklab(xyz: [f64; 3]) -> [f64; 3] {
    let lms = mat3(XYZ_TO_LMS, xyz);
    let lms_ = [lms[0].cbrt(), lms[1].cbrt(), lms[2].cbrt()];
    mat3(LMS_TO_OKLAB, lms_)
}

fn oklab_to_xyz(oklab: [f64; 3]) -> [f64; 3] {
    let lms_ = mat3(OKLAB_TO_LMS, oklab);
    let lms = [lms_[0].powi(3), lms_[1].powi(3), lms_[2].powi(3)];
    mat3(LMS_TO_XYZ, lms)
}

// ---- hsl / hwb (legacy, channels in canonical units) ----------------

/// hsl [hue-deg, sat-%, light-%] -> rgb [0..255, 0..255, 0..255].
fn hsl_to_rgb255(hsl: [f64; 3]) -> [f64; 3] {
    let c = Color::from_hsl(hsl[0], hsl[1] / 100.0, hsl[2] / 100.0, 1.0);
    [c.r, c.g, c.b]
}

/// rgb [0..255] -> hsl [hue-deg, sat-%, light-%].
fn rgb255_to_hsl(rgb: [f64; 3]) -> [f64; 3] {
    let c = Color::rgb(rgb[0], rgb[1], rgb[2], 1.0);
    let (h, s, l) = c.to_hsl();
    [h, s * 100.0, l * 100.0]
}

/// hwb [hue-deg, white-%, black-%] -> rgb [0..255].
fn hwb_to_rgb255(hwb: [f64; 3]) -> [f64; 3] {
    let h = hwb[0];
    let mut w = hwb[1] / 100.0;
    let mut bl = hwb[2] / 100.0;
    if w + bl > 1.0 {
        let sum = w + bl;
        w /= sum;
        bl /= sum;
    }
    let base = Color::from_hsl(h, 1.0, 0.5, 1.0);
    let mix = |v: f64| ((v / 255.0) * (1.0 - w - bl) + w) * 255.0;
    [mix(base.r), mix(base.g), mix(base.b)]
}

/// rgb [0..255] -> hwb [hue-deg, white-%, black-%].
fn rgb255_to_hwb(rgb: [f64; 3]) -> [f64; 3] {
    let (h, _s, _l) = Color::rgb(rgb[0], rgb[1], rgb[2], 1.0).to_hsl();
    let w = rgb[0].min(rgb[1]).min(rgb[2]) / 255.0 * 100.0;
    let bl = (1.0 - rgb[0].max(rgb[1]).max(rgb[2]) / 255.0) * 100.0;
    [h, w, bl]
}

/// The CSS Color 4 "analogous component" category of a channel, used to carry
/// a missing channel through a color-space conversion. `None` for channels that
/// are never analogous to a differently-named channel.
#[derive(PartialEq, Eq, Clone, Copy)]
enum ChannelCategory {
    Red,
    Green,
    Blue,
    Lightness,
    Colorfulness, // saturation / chroma
    Hue,
    LabA,
    LabB,
    Whiteness,
    Blackness,
    X,
    Y,
    Z,
}

fn channel_category(space: ColorSpace, idx: usize) -> Option<ChannelCategory> {
    use ChannelCategory::*;
    use ColorSpace::*;
    Some(match (space, idx) {
        (Rgb, 0)
        | (Srgb, 0)
        | (SrgbLinear, 0)
        | (DisplayP3, 0)
        | (DisplayP3Linear, 0)
        | (A98Rgb, 0)
        | (ProphotoRgb, 0)
        | (Rec2020, 0) => Red,
        (Rgb, 1)
        | (Srgb, 1)
        | (SrgbLinear, 1)
        | (DisplayP3, 1)
        | (DisplayP3Linear, 1)
        | (A98Rgb, 1)
        | (ProphotoRgb, 1)
        | (Rec2020, 1) => Green,
        (Rgb, 2)
        | (Srgb, 2)
        | (SrgbLinear, 2)
        | (DisplayP3, 2)
        | (DisplayP3Linear, 2)
        | (A98Rgb, 2)
        | (ProphotoRgb, 2)
        | (Rec2020, 2) => Blue,
        (Hsl, 0) | (Hwb, 0) | (Lch, 2) | (Oklch, 2) => Hue,
        (Hsl, 1) | (Lch, 1) | (Oklch, 1) => Colorfulness,
        (Hsl, 2) | (Lab, 0) | (Lch, 0) | (Oklab, 0) | (Oklch, 0) => Lightness,
        (Hwb, 1) => Whiteness,
        (Hwb, 2) => Blackness,
        (Lab, 1) | (Oklab, 1) => LabA,
        (Lab, 2) | (Oklab, 2) => LabB,
        (XyzD65, 0) | (XyzD50, 0) => X,
        (XyzD65, 1) | (XyzD50, 1) => Y,
        (XyzD65, 2) | (XyzD50, 2) => Z,
        _ => return None,
    })
}

/// Convert a [`ModernColor`] to a new space, preserving alpha and carrying
/// over missing channels into analogous channels of the target (CSS Color 4).
pub(super) fn convert_modern(mc: &ModernColor, target: ColorSpace) -> ModernColor {
    if mc.space == target {
        return mc.clone();
    }
    let src = [z(mc.channels[0]), z(mc.channels[1]), z(mc.channels[2])];
    // Same-primary conversions avoid an XYZ round-trip (which would introduce
    // tiny floating-point error): rgb (0..255) and srgb (0..1) differ only by a
    // 255 scale, and hsl/hwb share rgb's primaries.
    let out = if let Some(direct) = direct_convert(mc.space, target, src) {
        direct
    } else {
        let xyz = to_xyz_d65(mc.space, src);
        from_xyz_d65(target, xyz)
    };
    // For each output channel, become missing if the analogous source channel
    // was missing.
    let missing_in_src = |cat: ChannelCategory| {
        (0..3).any(|i| mc.channels[i].is_none() && channel_category(mc.space, i) == Some(cat))
    };
    let mk = |i: usize| match channel_category(target, i) {
        Some(cat) if missing_in_src(cat) => None,
        _ => Some(out[i]),
    };
    let mut channels = [mk(0), mk(1), mk(2)];
    // An lch/oklch hue that is powerless in the result (zero chroma) becomes a
    // missing channel, matching dart-sass's conversion behavior. (hsl keeps a
    // numeric hue at zero saturation.)
    if matches!(target, ColorSpace::Lch | ColorSpace::Oklch) && out[1].abs() < 1e-10 {
        channels[2] = None;
    }
    ModernColor {
        space: target,
        channels,
        alpha: mc.alpha,
    }
}

/// Direct (no XYZ round-trip) conversion for same-primary spaces, returning the
/// three target channels, or `None` if the pair needs the general path.
fn direct_convert(from: ColorSpace, to: ColorSpace, c: [f64; 3]) -> Option<[f64; 3]> {
    use ColorSpace::*;
    Some(match (from, to) {
        (Rgb, Srgb) => [c[0] / 255.0, c[1] / 255.0, c[2] / 255.0],
        (Srgb, Rgb) => [c[0] * 255.0, c[1] * 255.0, c[2] * 255.0],
        (Rgb, Hsl) => rgb255_to_hsl(c),
        (Hsl, Rgb) => hsl_to_rgb255(c),
        (Rgb, Hwb) => rgb255_to_hwb(c),
        (Hwb, Rgb) => hwb_to_rgb255(c),
        (Srgb, Hsl) => rgb255_to_hsl([c[0] * 255.0, c[1] * 255.0, c[2] * 255.0]),
        (Hsl, Srgb) => {
            let r = hsl_to_rgb255(c);
            [r[0] / 255.0, r[1] / 255.0, r[2] / 255.0]
        }
        (Srgb, Hwb) => rgb255_to_hwb([c[0] * 255.0, c[1] * 255.0, c[2] * 255.0]),
        (Hwb, Srgb) => {
            let r = hwb_to_rgb255(c);
            [r[0] / 255.0, r[1] / 255.0, r[2] / 255.0]
        }
        (Hsl, Hwb) => rgb255_to_hwb(hsl_to_rgb255(c)),
        (Hwb, Hsl) => rgb255_to_hsl(hwb_to_rgb255(c)),
        _ => return None,
    })
}

/// Build a [`ModernColor`] from a legacy [`Color`] (its current space is
/// `rgb`/`hsl`/`hwb` per `mc.modern`, or plain sRGB → `rgb`).
pub(super) fn legacy_to_modern(c: &Color) -> ModernColor {
    if let Some(m) = &c.modern {
        return m.clone();
    }
    ModernColor {
        space: ColorSpace::Rgb,
        channels: [Some(c.r), Some(c.g), Some(c.b)],
        alpha: Some(c.a),
    }
}

/// Wrap a [`ModernColor`] in a [`Color`], deriving an in-gamut sRGB-byte
/// approximation for the legacy `r`/`g`/`b`/`a` fields (so legacy code paths
/// that read them keep working). The modern representation drives
/// serialization and channel access.
fn make_modern(mc: ModernColor) -> Color {
    let srgb = convert_modern(&mc, ColorSpace::Rgb);
    let mut c = Color::rgb(
        z(srgb.channels[0]).clamp(0.0, 255.0),
        z(srgb.channels[1]).clamp(0.0, 255.0),
        z(srgb.channels[2]).clamp(0.0, 255.0),
        mc.alpha.unwrap_or(1.0),
    );
    c.modern = Some(mc);
    c
}

/// The known predefined `color()` spaces and their [`ColorSpace`].
fn predefined_space(name: &str) -> Option<ColorSpace> {
    match name {
        "srgb" => Some(ColorSpace::Srgb),
        "srgb-linear" => Some(ColorSpace::SrgbLinear),
        "display-p3" => Some(ColorSpace::DisplayP3),
        "display-p3-linear" => Some(ColorSpace::DisplayP3Linear),
        "a98-rgb" => Some(ColorSpace::A98Rgb),
        "prophoto-rgb" => Some(ColorSpace::ProphotoRgb),
        "rec2020" => Some(ColorSpace::Rec2020),
        "xyz" | "xyz-d65" => Some(ColorSpace::XyzD65),
        "xyz-d50" => Some(ColorSpace::XyzD50),
        _ => None,
    }
}

/// Parse a numeric channel value for a modern `color()`/lab-family call into a
/// canonical channel value, or `None` for a `none` channel. `pct_base` scales a
/// `%` value (e.g. 1.0 for rgb channels in 0..1, 100.0 for lab lightness).
/// Degenerate calc constants fold to infinity/NaN. The result is NOT clamped.
pub(super) fn modern_channel(v: &Value, pct_base: f64) -> Option<f64> {
    if is_none_keyword(v) {
        return None;
    }
    if let Value::Calc(node) = v {
        if let Some(c) = degenerate_const(node) {
            return Some(c);
        }
    }
    match v {
        Value::Number(num) => {
            if num.unit == "%" {
                Some(num.value / 100.0 * pct_base)
            } else {
                Some(num.value)
            }
        }
        Value::Slash(num, _) => Some(num.value),
        _ => Some(0.0),
    }
}

/// Parse a hue channel (degrees), converting angle units. `none` → `None`.
pub(super) fn modern_hue(v: &Value) -> Option<f64> {
    if is_none_keyword(v) {
        return None;
    }
    if let Value::Calc(node) = v {
        if let Some(c) = degenerate_const(node) {
            return Some(c);
        }
    }
    match v {
        Value::Number(num) => Some(match num.unit.as_str() {
            "rad" => num.value.to_degrees(),
            "grad" => num.value * 360.0 / 400.0,
            "turn" => num.value * 360.0,
            _ => num.value,
        }),
        Value::Slash(num, _) => Some(num.value),
        _ => Some(0.0),
    }
}

/// Parse a modern alpha channel. `none` → `None`; otherwise clamp to 0..1.
pub(super) fn modern_alpha(v: Option<&Value>) -> Option<f64> {
    match v {
        None => Some(1.0),
        Some(a) if is_none_keyword(a) => None,
        Some(a) => {
            if let Value::Calc(node) = a {
                if let Some(c) = degenerate_const(node) {
                    return Some(if c.is_nan() { 0.0 } else { c.clamp(0.0, 1.0) });
                }
            }
            match a {
                Value::Number(num) => {
                    let val = if num.unit == "%" {
                        num.value / 100.0
                    } else {
                        num.value
                    };
                    Some(val.clamp(0.0, 1.0))
                }
                Value::Slash(num, _) => Some(num.value.clamp(0.0, 1.0)),
                _ => Some(1.0),
            }
        }
    }
}

// =====================================================================
// Modern `sass:color` module members (color.space / color.channel /
// color.to-space / color.is-legacy / color.is-missing / color.is-in-gamut /
// color.is-powerless / color.to-gamut / color.same). These are dispatched
// from the module seam under the global names `color-space`, `color-channel`,
// etc.
// =====================================================================

/// The space of a [`Color`] as a [`ColorSpace`] (`rgb` for plain legacy).
fn color_space_of(c: &Color) -> ColorSpace {
    c.modern.as_ref().map(|m| m.space).unwrap_or(ColorSpace::Rgb)
}

pub(super) fn try_call_modern(
    name: &str,
    pos_args: &[Value],
    named: &[(String, Value)],
    pos: Pos,
) -> Option<Result<Value, Error>> {
    Some(match name {
        "color-space" => fn_space(pos_args, named, pos),
        "color-channel" => fn_color_channel(pos_args, named, pos),
        "color-to-space" => fn_to_space(pos_args, named, pos),
        "color-is-legacy" => fn_is_legacy(pos_args, named, pos),
        "color-is-missing" => fn_is_missing(pos_args, named, pos),
        "color-is-in-gamut" => fn_is_in_gamut(pos_args, named, pos),
        "color-is-powerless" => fn_is_powerless(pos_args, named, pos),
        "color-to-gamut" => fn_to_gamut(pos_args, named, pos),
        "color-same" => fn_same(pos_args, named, pos),
        _ => return None,
    })
}

/// Look up a channel name within `space`, returning its index (and the special
/// `"alpha"`/missing handling left to the caller).
pub(super) fn channel_index_in(space: ColorSpace, channel: &str) -> Option<usize> {
    space.channel_names().iter().position(|n| *n == channel)
}

/// Read a channel-name argument. dart-sass requires it to be a *quoted*
/// string: an unquoted string (`hue`) errors "Expected … to be a quoted
/// string"; a non-string errors "… is not a string".
fn channel_name_arg(v: &Value, pos: Pos) -> Result<String, Error> {
    match v {
        Value::Str(s) if s.quoted => Ok(s.text.clone()),
        Value::Str(s) => Err(Error::at(
            format!("$channel: Expected {} to be a quoted string.", s.text),
            pos,
        )),
        other => Err(Error::at(
            format!("$channel: {} is not a string.", other.to_css(false)),
            pos,
        )),
    }
}

/// Parse a `$space` argument into a [`ColorSpace`]. dart-sass requires an
/// *unquoted* string: a quoted one errors "Expected … to be an unquoted
/// string".
pub(super) fn space_arg(v: &Value, pos: Pos) -> Result<ColorSpace, Error> {
    let name = match v {
        Value::Str(s) if !s.quoted => s.text.clone(),
        Value::Str(s) => {
            return Err(Error::at(
                format!("$space: Expected \"{}\" to be an unquoted string.", s.text),
                pos,
            ))
        }
        other => {
            return Err(Error::at(
                format!("$space: {} is not a string.", other.to_css(false)),
                pos,
            ))
        }
    };
    ColorSpace::from_name(&name)
        .ok_or_else(|| Error::at(format!("$space: Unknown color space \"{name}\"."), pos))
}

fn fn_space(pos_args: &[Value], named: &[(String, Value)], pos: Pos) -> Result<Value, Error> {
    let params = ["color"];
    let c = as_color(require(&params, pos_args, named, 0, "space", pos)?, pos)?;
    Ok(Value::Str(crate::value::SassStr {
        text: color_space_of(&c).name().to_string(),
        quoted: false,
    }))
}

fn fn_is_legacy(pos_args: &[Value], named: &[(String, Value)], pos: Pos) -> Result<Value, Error> {
    let params = ["color"];
    let c = as_color(require(&params, pos_args, named, 0, "is-legacy", pos)?, pos)?;
    Ok(Value::Bool(color_space_of(&c).is_legacy()))
}

fn fn_to_space(pos_args: &[Value], named: &[(String, Value)], pos: Pos) -> Result<Value, Error> {
    let params = ["color", "space"];
    let n = pos_args.len() + named.len();
    if n > 2 {
        return Err(Error::at(
            format!("Only 2 arguments allowed, but {n} were passed."),
            pos,
        ));
    }
    let c = as_color(require(&params, pos_args, named, 0, "to-space", pos)?, pos)?;
    let space = space_arg(require(&params, pos_args, named, 1, "to-space", pos)?, pos)?;
    let mc = legacy_to_modern(&c);
    if mc.space == space {
        return Ok(Value::Color(c));
    }
    let out = convert_to_space(&mc, space);
    Ok(Value::Color(make_modern_in(out, space)))
}

/// Build a [`Color`] for `mc` already in `space`, choosing whether to leave the
/// `modern` tag attached. Plain-legacy rgb (no missing channels) drops the
/// `modern` field so it serializes like a normal sRGB color.
pub(super) fn make_modern_in(mc: ModernColor, _space: ColorSpace) -> Color {
    if mc.space == ColorSpace::Rgb && mc.channels.iter().all(|c| c.is_some()) {
        let r = mc.channels[0].unwrap_or(0.0);
        let g = mc.channels[1].unwrap_or(0.0);
        let b = mc.channels[2].unwrap_or(0.0);
        let a = mc.alpha.unwrap_or(1.0);
        let mut c = Color::rgb(r, g, b, a);
        // Out-of-gamut legacy rgb serializes via hsl; attach modern so the
        // serializer can apply that rule. Otherwise a computed in-gamut rgb
        // uses its CSS named-color spelling when it matches one.
        let in_gamut = |v: f64| (-1e-9..=255.0 + 1e-9).contains(&v);
        if !(in_gamut(r) && in_gamut(g) && in_gamut(b)) {
            c.modern = Some(mc);
        } else {
            c.repr = named_repr(r, g, b, a);
        }
        return c;
    }
    make_modern(mc)
}

/// Convert `mc` to `space`, carrying over the hue of a polar source when the
/// chroma/saturation is zero (powerless), matching dart-sass's missing-channel
/// behavior is not applied here — only the plain numeric conversion.
fn convert_to_space(mc: &ModernColor, space: ColorSpace) -> ModernColor {
    convert_modern(mc, space)
}

fn fn_color_channel(pos_args: &[Value], named: &[(String, Value)], pos: Pos) -> Result<Value, Error> {
    let params = ["color", "channel", "space"];
    let n = pos_args.len() + named.len();
    if n > 3 {
        return Err(Error::at(
            format!("Only 3 arguments allowed, but {n} were passed."),
            pos,
        ));
    }
    let c = as_color(require(&params, pos_args, named, 0, "channel", pos)?, pos)?;
    let chan = channel_name_arg(require(&params, pos_args, named, 1, "channel", pos)?, pos)?;
    let mc = legacy_to_modern(&c);
    // The space to read the channel in: explicit `$space`, else the color's own.
    let space = match arg(&params, pos_args, named, 2) {
        Some(v) => space_arg(v, pos)?,
        None => mc.space,
    };
    if chan == "alpha" {
        let a = mc.alpha.unwrap_or(0.0);
        return Ok(Value::Number(Number {
            value: a,
            unit: String::new(),
        }));
    }
    let target = convert_modern(&mc, space);
    let idx = channel_index_in(space, &chan).ok_or_else(|| {
        Error::at(
            format!("$channel: Color {} has no channel named {chan}.", c.to_css(false)),
            pos,
        )
    })?;
    let raw = target.channels[idx].unwrap_or(0.0);
    Ok(Value::Number(channel_number(space, idx, raw)))
}

/// Build the [`Number`] for a channel value, applying dart-sass's per-channel
/// unit: percentage for lightness/saturation/lightness/whiteness/blackness,
/// `deg` for hue, plain otherwise. Legacy rgb red/green/blue are 0..255.
fn channel_number(space: ColorSpace, idx: usize, raw: f64) -> Number {
    use ColorSpace::*;
    let names = space.channel_names();
    let cname = names[idx];
    let pct = |v: f64| Number {
        value: v,
        unit: "%".to_string(),
    };
    let deg = |v: f64| Number {
        value: v,
        unit: "deg".to_string(),
    };
    let plain = |v: f64| Number {
        value: v,
        unit: String::new(),
    };
    match (space, cname) {
        (Hsl, "saturation") | (Hsl, "lightness") => pct(raw),
        (Hwb, "whiteness") | (Hwb, "blackness") => pct(raw),
        (Lab, "lightness") | (Lch, "lightness") => pct(raw),
        (Oklab, "lightness") | (Oklch, "lightness") => pct(raw * 100.0),
        (_, "hue") => deg(raw),
        _ => plain(raw),
    }
}

fn fn_is_missing(pos_args: &[Value], named: &[(String, Value)], pos: Pos) -> Result<Value, Error> {
    let params = ["color", "channel"];
    let c = as_color(require(&params, pos_args, named, 0, "is-missing", pos)?, pos)?;
    let chan = channel_name_arg(require(&params, pos_args, named, 1, "is-missing", pos)?, pos)?;
    let mc = legacy_to_modern(&c);
    let missing = if chan == "alpha" {
        mc.alpha.is_none()
    } else {
        match channel_index_in(mc.space, &chan) {
            Some(idx) => mc.channels[idx].is_none(),
            None => false,
        }
    };
    Ok(Value::Bool(missing))
}

fn fn_is_in_gamut(pos_args: &[Value], named: &[(String, Value)], pos: Pos) -> Result<Value, Error> {
    let params = ["color", "space"];
    let n = pos_args.len() + named.len();
    if n > 2 {
        return Err(Error::at(
            format!("Only 2 arguments allowed, but {n} were passed."),
            pos,
        ));
    }
    let c = as_color(require(&params, pos_args, named, 0, "is-in-gamut", pos)?, pos)?;
    let mc = legacy_to_modern(&c);
    let space = match arg(&params, pos_args, named, 1) {
        Some(v) => space_arg(v, pos)?,
        None => mc.space,
    };
    Ok(Value::Bool(in_gamut(&mc, space)))
}

/// Whether `mc` is within the gamut of `space`. The bounded RGB-style spaces
/// check their own channels; the legacy hsl/hwb spaces share the sRGB gamut
/// (their rgb representation must fit `[0,255]`); the unbounded perceptual/xyz
/// spaces are always in gamut.
fn in_gamut(mc: &ModernColor, space: ColorSpace) -> bool {
    use ColorSpace::*;
    let bound = |conv: &ModernColor, lo: f64, hi: f64| {
        conv.channels
            .iter()
            .all(|c| matches!(c, Some(v) if *v >= lo - 1e-7 && *v <= hi + 1e-7) || c.is_none())
    };
    match space {
        Rgb => bound(&convert_modern(mc, space), 0.0, 255.0),
        Srgb | SrgbLinear | DisplayP3 | DisplayP3Linear | A98Rgb | ProphotoRgb | Rec2020 => {
            bound(&convert_modern(mc, space), 0.0, 1.0)
        }
        // hsl/hwb share sRGB's gamut.
        Hsl | Hwb => bound(&convert_modern(mc, Rgb), 0.0, 255.0),
        // The unbounded perceptual/xyz spaces are always in gamut.
        _ => true,
    }
}

fn fn_is_powerless(pos_args: &[Value], named: &[(String, Value)], pos: Pos) -> Result<Value, Error> {
    let params = ["color", "channel", "space"];
    let n = pos_args.len() + named.len();
    if n > 3 {
        return Err(Error::at(
            format!("Only 3 arguments allowed, but {n} were passed."),
            pos,
        ));
    }
    let c = as_color(require(&params, pos_args, named, 0, "is-powerless", pos)?, pos)?;
    let chan = channel_name_arg(require(&params, pos_args, named, 1, "is-powerless", pos)?, pos)?;
    let mc = legacy_to_modern(&c);
    let space = match arg(&params, pos_args, named, 2) {
        Some(v) => space_arg(v, pos)?,
        None => mc.space,
    };
    let conv = convert_modern(&mc, space);
    if chan == "alpha" {
        return Ok(Value::Bool(false));
    }
    let idx = channel_index_in(space, &chan).ok_or_else(|| {
        Error::at(
            format!(
                "$channel: Color {} doesn't have a channel named \"{chan}\".",
                c.to_css(false)
            ),
            pos,
        )
    })?;
    Ok(Value::Bool(channel_powerless(space, idx, &conv)))
}

/// dart-sass powerless rules (with fuzzy comparison): hsl hue is powerless at
/// saturation ~0; hwb hue is powerless when whiteness+blackness >= 100;
/// lch/oklch hue is powerless at chroma ~0. (hsl saturation is never powerless.)
fn channel_powerless(space: ColorSpace, idx: usize, conv: &ModernColor) -> bool {
    use ColorSpace::*;
    let ch = |i: usize| conv.channels[i].unwrap_or(0.0);
    let fuzzy_zero = |v: f64| v.abs() < 1e-11;
    match (space, idx) {
        (Hsl, 0) => fuzzy_zero(ch(1)),
        (Hwb, 0) => ch(1) + ch(2) >= 100.0 - 1e-11,
        (Lch, 2) | (Oklch, 2) => fuzzy_zero(ch(1)),
        _ => false,
    }
}

fn fn_same(pos_args: &[Value], named: &[(String, Value)], pos: Pos) -> Result<Value, Error> {
    let params = ["color1", "color2"];
    let c1 = as_color(require(&params, pos_args, named, 0, "same", pos)?, pos)?;
    let c2 = as_color(require(&params, pos_args, named, 1, "same", pos)?, pos)?;
    let m1 = legacy_to_modern(&c1);
    let m2 = legacy_to_modern(&c2);
    // Compare in xyz-d65 (a canonical space) with alpha.
    let x1 = convert_modern(&m1, ColorSpace::XyzD65);
    let x2 = convert_modern(&m2, ColorSpace::XyzD65);
    let close = |a: f64, b: f64| (a - b).abs() < 1e-7;
    let same = close(z(x1.channels[0]), z(x2.channels[0]))
        && close(z(x1.channels[1]), z(x2.channels[1]))
        && close(z(x1.channels[2]), z(x2.channels[2]))
        && close(m1.alpha.unwrap_or(1.0), m2.alpha.unwrap_or(1.0));
    Ok(Value::Bool(same))
}

fn fn_to_gamut(pos_args: &[Value], named: &[(String, Value)], pos: Pos) -> Result<Value, Error> {
    let params = ["color", "space", "method"];
    let c = as_color(require(&params, pos_args, named, 0, "to-gamut", pos)?, pos)?;
    let mc = legacy_to_modern(&c);
    let space = match arg(&params, pos_args, named, 1) {
        Some(v) => space_arg(v, pos)?,
        None => mc.space,
    };
    // `$method` is required (forwards-compatibility with the CSS spec).
    let method = match arg(&params, pos_args, named, 2) {
        Some(v) => v,
        None => {
            return Err(Error::at(
                "$method: color.to-gamut() requires a $method argument for \
                 forwards-compatibility with changes in the CSS spec. Suggestion:\n\n\
                 $method: local-minde"
                    .to_string(),
                pos,
            ))
        }
    };
    let clip = match method {
        Value::Str(s) if !s.quoted && s.text.eq_ignore_ascii_case("clip") => true,
        Value::Str(s) if !s.quoted && s.text.eq_ignore_ascii_case("local-minde") => false,
        Value::Str(s) if s.quoted => {
            return Err(Error::at(
                format!("$method: Expected \"{}\" to be an unquoted string.", s.text),
                pos,
            ))
        }
        other => {
            return Err(Error::at(
                format!(
                    "$method: {} must be either clip or local-minde.",
                    other.to_css(false)
                ),
                pos,
            ))
        }
    };
    if in_gamut(&mc, space) {
        return Ok(Value::Color(c));
    }
    let mapped = if clip {
        clamp_in_space(&mc, space)
    } else {
        gamut_map(&mc, space)
    };
    // Re-express in the original space.
    let back = convert_modern(&mapped, mc.space);
    Ok(Value::Color(make_modern_in(back, mc.space)))
}

/// CSS Color 4 gamut mapping (`local-minde`): reduce oklch chroma via binary
/// search, clipping in the target space, until the clipped color is within a
/// just-noticeable difference (deltaEOK). Ported from the CSS Color 4 spec /
/// dart-sass.
fn gamut_map(mc: &ModernColor, space: ColorSpace) -> ModernColor {
    const JND: f64 = 0.02;
    const EPS: f64 = 0.0001;
    let oklch = convert_modern(mc, ColorSpace::Oklch);
    let l = z(oklch.channels[0]);
    if l >= 1.0 {
        return clamp_in_space(&white_in(space), space);
    }
    if l <= 0.0 {
        return clamp_in_space(&black_in(space), space);
    }
    let h = z(oklch.channels[2]);
    let make = |c: f64| ModernColor {
        space: ColorSpace::Oklch,
        channels: [Some(l), Some(c), Some(h)],
        alpha: oklch.alpha,
    };
    let mut current = oklch.clone();
    let mut clipped = clamp_in_space(&current, space);
    if delta_eok(&clipped, &current) < JND {
        return convert_modern(&clipped, space);
    }
    let mut min = 0.0;
    let mut max = z(oklch.channels[1]);
    let mut min_in_gamut = true;
    while max - min > EPS {
        let chroma = (min + max) / 2.0;
        current = make(chroma);
        if min_in_gamut && in_gamut(&current, space) {
            min = chroma;
            continue;
        }
        clipped = clamp_in_space(&current, space);
        let e = delta_eok(&clipped, &current);
        if e < JND {
            if JND - e < EPS {
                return convert_modern(&clipped, space);
            }
            min_in_gamut = false;
            min = chroma;
        } else {
            max = chroma;
        }
    }
    convert_modern(&clipped, space)
}

fn white_in(space: ColorSpace) -> ModernColor {
    let mc = ModernColor {
        space: ColorSpace::Oklch,
        channels: [Some(1.0), Some(0.0), Some(0.0)],
        alpha: Some(1.0),
    };
    convert_modern(&mc, space)
}

fn black_in(space: ColorSpace) -> ModernColor {
    let mc = ModernColor {
        space: ColorSpace::Oklch,
        channels: [Some(0.0), Some(0.0), Some(0.0)],
        alpha: Some(1.0),
    };
    convert_modern(&mc, space)
}

/// Clamp each channel of a color (already convertible) into `space`'s bounds.
fn clamp_in_space(mc: &ModernColor, space: ColorSpace) -> ModernColor {
    use ColorSpace::*;
    let conv = convert_modern(mc, space);
    let (lo, hi) = match space {
        Rgb => (0.0, 255.0),
        Srgb | SrgbLinear | DisplayP3 | DisplayP3Linear | A98Rgb | ProphotoRgb | Rec2020 => (0.0, 1.0),
        _ => return conv,
    };
    ModernColor {
        space,
        channels: [
            conv.channels[0].map(|v| v.clamp(lo, hi)),
            conv.channels[1].map(|v| v.clamp(lo, hi)),
            conv.channels[2].map(|v| v.clamp(lo, hi)),
        ],
        alpha: conv.alpha,
    }
}

/// The deltaEOK (Euclidean distance in oklab) between two colors.
fn delta_eok(a: &ModernColor, b: &ModernColor) -> f64 {
    let a = convert_modern(a, ColorSpace::Oklab);
    let b = convert_modern(b, ColorSpace::Oklab);
    let dl = z(a.channels[0]) - z(b.channels[0]);
    let da = z(a.channels[1]) - z(b.channels[1]);
    let db = z(a.channels[2]) - z(b.channels[2]);
    (dl * dl + da * da + db * db).sqrt()
}

/// Build a modern legacy color (rgb/hsl) from a [`Channels`] set when it
/// contains a `none` channel (and no real special function), matching
/// dart-sass's modern parsing. Returns `Ok(None)` when there is no `none`
/// channel or a real special function is present (the caller falls through to
/// its existing handling).
fn legacy_none_color(channels: &Channels, space: ColorSpace, _pos: Pos) -> Result<Option<Value>, Error> {
    let comps_special = channels.comps.iter().any(is_special_legacy);
    let alpha_special = channels.alpha.as_ref().is_some_and(is_special_legacy);
    if comps_special || alpha_special {
        return Ok(None);
    }
    let comps_none = channels.comps.iter().any(is_none_keyword);
    let alpha_none = channels.alpha.as_ref().is_some_and(is_none_keyword);
    if !(comps_none || alpha_none) {
        return Ok(None);
    }
    if channels.comps.len() != 3 {
        return Ok(None);
    }
    let comps = &channels.comps;
    let ch = match space {
        ColorSpace::Hsl => [
            if is_none_keyword(&comps[0]) {
                None
            } else {
                modern_hue(&comps[0])
            },
            modern_channel(&comps[1], 100.0),
            modern_channel(&comps[2], 100.0),
        ],
        // rgb: channels in 0..255.
        _ => [
            modern_channel(&comps[0], 255.0),
            modern_channel(&comps[1], 255.0),
            modern_channel(&comps[2], 255.0),
        ],
    };
    let mc = ModernColor {
        space,
        channels: ch,
        alpha: modern_alpha(channels.alpha.as_ref()),
    };
    Ok(Some(Value::Color(make_modern(mc))))
}

/// Serialize a lab-family call (`lab`/`lch`/`oklab`/`oklch`) that has a
/// degenerate `calc()` in a non-lightness channel: lightness folds and is
/// rendered as a percentage, the alpha folds, and the chroma/a/b/hue channels
/// keep their authored spelling (e.g. `calc(infinity)`).
fn lab_degenerate(
    name: &str,
    comps: &[Value],
    alpha: Option<&Value>,
    l_base: f64,
    l_max: f64,
    pos: Pos,
) -> Value {
    let l = modern_channel(&comps[0], l_base)
        .map(|v| v.clamp(0.0, l_max))
        .unwrap_or(0.0);
    let l_pct = fmt_num(l / l_base * 100.0, false);
    // The chroma channel of lch/oklch is floored at 0: a degenerate calc whose
    // floored value differs (`-infinity`/`NaN` → 0) folds to that number;
    // `+infinity` is unchanged so its `calc(...)` text is kept.
    let is_polar = matches!(name, "lch" | "oklch");
    let degenerate_chroma = if is_polar {
        match &comps[1] {
            Value::Calc(node) => degenerate_const(node),
            _ => None,
        }
    } else {
        None
    };
    let c1 = match degenerate_chroma {
        Some(raw) if raw.is_nan() || raw.max(0.0) != raw => fmt_num(raw.max(0.0), false),
        _ => comps[1].to_css(false),
    };
    let c2 = comps[2].to_css(false);
    let a = match alpha {
        Some(v) if is_none_keyword(v) => 1.0,
        Some(v) => alpha_value(v, pos).unwrap_or(1.0),
        None => 1.0,
    };
    let body = format!("{l_pct}% {c1} {c2}");
    let text = if (a - 1.0).abs() < f64::EPSILON {
        format!("{name}({body})")
    } else {
        format!("{name}({body} / {})", fmt_num(a, false))
    };
    Value::Str(crate::value::SassStr { text, quoted: false })
}

/// CSS Color 4 `color.mix($c1, $c2, $weight, $method)` interpolation in
/// `space`. `weight` is the percentage (0..100) of `c1`. Channels are
/// interpolated with premultiplied alpha (except the hue, which uses
/// `hue_method`); a channel missing in one color takes the other's value.
fn interpolate_mix(c1: &Color, c2: &Color, weight: f64, space: ColorSpace, hue_method: HueMethod) -> Color {
    let p = weight / 100.0;
    // Powerless channels are blanked in each color's own space first, so the
    // missing carries through the conversion to the interpolation space.
    let m1 = convert_modern(&blank_powerless(legacy_to_modern(c1)), space);
    let m2 = convert_modern(&blank_powerless(legacy_to_modern(c2)), space);
    let a1 = m1.alpha;
    let a2 = m2.alpha;
    // Result alpha (missing treated as the other's, else 1).
    let ra1 = a1.unwrap_or_else(|| a2.unwrap_or(1.0));
    let ra2 = a2.unwrap_or_else(|| a1.unwrap_or(1.0));
    let result_alpha = ra1 * p + ra2 * (1.0 - p);
    let hue_idx = match space {
        ColorSpace::Hsl | ColorSpace::Hwb => Some(0),
        ColorSpace::Lch | ColorSpace::Oklch => Some(2),
        _ => None,
    };
    let mut out = [None; 3];
    for (i, slot) in out.iter_mut().enumerate() {
        let v1 = m1.channels[i];
        let v2 = m2.channels[i];
        // A channel missing in both stays missing.
        if v1.is_none() && v2.is_none() {
            *slot = None;
            continue;
        }
        // Missing in one: take the other's value (carry).
        let x1 = v1.unwrap_or_else(|| v2.unwrap_or(0.0));
        let x2 = v2.unwrap_or_else(|| v1.unwrap_or(0.0));
        if Some(i) == hue_idx {
            *slot = Some(interpolate_hue(x1, x2, p, hue_method));
        } else {
            // Premultiplied-alpha interpolation. A missing alpha counts as 1.
            let pa1 = a1.unwrap_or(1.0);
            let pa2 = a2.unwrap_or(1.0);
            let premul = x1 * pa1 * p + x2 * pa2 * (1.0 - p);
            *slot = Some(if result_alpha.abs() < 1e-12 {
                x1 * p + x2 * (1.0 - p)
            } else {
                premul / result_alpha
            });
        }
    }
    let alpha = if a1.is_none() && a2.is_none() {
        None
    } else {
        Some(result_alpha)
    };
    let mc = ModernColor {
        space,
        channels: out,
        alpha,
    };
    // The result is expressed in c1's original space (CSS Color 4 / dart-sass):
    // a legacy c1 yields a legacy result, a modern c1 keeps its own space.
    let dest = legacy_to_modern(c1).space;
    let back = convert_modern(&mc, dest);
    make_modern_in(back, dest)
}

/// Interpolate two hue angles (degrees) by `p` (fraction of `h1`) using the
/// CSS Color 4 hue interpolation method.
fn interpolate_hue(h1: f64, h2: f64, p: f64, method: HueMethod) -> f64 {
    let mut a = h1.rem_euclid(360.0);
    let mut b = h2.rem_euclid(360.0);
    match method {
        HueMethod::Shorter => {
            let diff = b - a;
            if diff > 180.0 {
                a += 360.0;
            } else if diff < -180.0 {
                b += 360.0;
            }
        }
        HueMethod::Longer => {
            // The longer arc: take the complement of the shorter direction.
            let diff = b - a;
            if (0.0..180.0).contains(&diff) {
                b += 360.0;
            } else if (-180.0..0.0).contains(&diff) {
                a += 360.0;
            }
        }
        HueMethod::Increasing => {
            if b < a {
                b += 360.0;
            }
        }
        HueMethod::Decreasing => {
            if a < b {
                a += 360.0;
            }
        }
    }
    a * p + b * (1.0 - p)
}

/// Set a powerless channel to missing (`None`) for interpolation: the hue of
/// hsl (at zero saturation), hwb (whiteness+blackness >= 100) and lch/oklch
/// (at zero chroma) carries no information.
pub(super) fn blank_powerless(mut mc: ModernColor) -> ModernColor {
    let ch = |i: usize| mc.channels[i].unwrap_or(0.0);
    match mc.space {
        ColorSpace::Hsl if ch(1) == 0.0 => mc.channels[0] = None,
        ColorSpace::Hwb if ch(1) + ch(2) >= 100.0 => mc.channels[0] = None,
        ColorSpace::Lch | ColorSpace::Oklch if ch(1) == 0.0 => mc.channels[2] = None,
        _ => {}
    }
    mc
}

// ---- modern change / adjust / scale (with $space) -------------------

/// The kind of modify operation for the modern change/adjust/scale path.
#[derive(Clone, Copy)]
pub(super) enum ModifyOp {
    Change,
    Adjust,
    Scale,
}

/// The scaling bounds [min, max] for a channel of `space` (used by
/// `scale-color`). `None` for the hue channel (not scalable).
fn scale_bounds(space: ColorSpace, idx: usize) -> Option<(f64, f64)> {
    use ColorSpace::*;
    let names = space.channel_names();
    if names[idx] == "hue" {
        return None;
    }
    Some(match space {
        Rgb => (0.0, 255.0),
        Srgb | SrgbLinear | DisplayP3 | DisplayP3Linear | A98Rgb | ProphotoRgb | Rec2020 => (0.0, 1.0),
        Hsl | Hwb => (0.0, 100.0),
        Lab => {
            if idx == 0 {
                (0.0, 100.0)
            } else {
                (-125.0, 125.0)
            }
        }
        Lch => {
            if idx == 0 {
                (0.0, 100.0)
            } else {
                (0.0, 150.0)
            }
        }
        Oklab => {
            if idx == 0 {
                (0.0, 1.0)
            } else {
                (-0.4, 0.4)
            }
        }
        Oklch => {
            if idx == 0 {
                (0.0, 1.0)
            } else {
                (0.0, 0.4)
            }
        }
        XyzD65 | XyzD50 => (0.0, 1.0),
    })
}

/// Read a channel value argument for the modern modify path, in the channel's
/// canonical unit. `idx` selects whether it is the polar hue channel.
fn modify_channel_value(space: ColorSpace, idx: usize, v: &Value) -> Option<f64> {
    if space.is_polar(idx) {
        modern_hue(v)
    } else {
        modern_channel(v, channel_pct_base(space, idx))
    }
}

impl ColorSpace {
    /// Whether the channel at `index` is a polar hue channel.
    fn is_polar(self, index: usize) -> bool {
        self.channel_names()[index] == "hue"
    }
}

/// The percentage reference (100% = ?) for a channel of `space`.
fn channel_pct_base(space: ColorSpace, idx: usize) -> f64 {
    use ColorSpace::*;
    let names = space.channel_names();
    match (space, names[idx]) {
        (Rgb, _) => 255.0,
        (Srgb | SrgbLinear | DisplayP3 | DisplayP3Linear | A98Rgb | ProphotoRgb | Rec2020, _) => 1.0,
        (Hsl | Hwb, _) => 100.0,
        (Lab, "lightness") | (Lch, "lightness") => 100.0,
        (Oklab, "lightness") | (Oklch, "lightness") => 1.0,
        (Lab, _) => 125.0,
        (Oklab, _) => 0.4,
        (Lch, "chroma") => 150.0,
        (Oklch, "chroma") => 0.4,
        (XyzD65 | XyzD50, _) => 1.0,
        _ => 1.0,
    }
}

/// The modern `color.change`/`adjust`/`scale` with an explicit `$space`:
/// convert the color to `space`, apply the per-channel operation, convert back
/// to the color's original space.
pub(super) fn modify_in_space(
    c: &Color,
    space: ColorSpace,
    chans: &[(String, &Value)],
    op: ModifyOp,
    pos: Pos,
) -> Result<Value, Error> {
    let orig = legacy_to_modern(c);
    let dest = orig.space;
    let mut work = convert_modern(&orig, space);
    for (name, v) in chans {
        if name == "alpha" {
            work.alpha = Some(apply_alpha(work.alpha.unwrap_or(1.0), v, op, pos)?);
            continue;
        }
        let idx = channel_index_in(space, name).ok_or_else(|| {
            Error::at(
                format!(
                    "${name}: Color space {} doesn't have a channel with this name.",
                    space.name()
                ),
                pos,
            )
        })?;
        // Validate the channel value's unit (skipped for a scale `%` and for a
        // `none` change keyword, handled below).
        if !matches!(op, ModifyOp::Scale) && !is_none_keyword(v) {
            validate_modify_unit(space, idx, name, v, pos)?;
        }
        match op {
            ModifyOp::Change => {
                if is_none_keyword(v) {
                    work.channels[idx] = None;
                } else {
                    work.channels[idx] = modify_channel_value(space, idx, v);
                }
            }
            ModifyOp::Adjust => {
                let amt = modify_channel_value(space, idx, v).unwrap_or(0.0);
                let cur = work.channels[idx].unwrap_or(0.0);
                work.channels[idx] = Some(clamp_adjust_channel(space, idx, cur + amt));
            }
            ModifyOp::Scale => {
                let bounds = scale_bounds(space, idx)
                    .ok_or_else(|| Error::at(format!("${name}: Channel isn't scalable."), pos))?;
                let factor = scale_pct(v, pos)?;
                let cur = work.channels[idx].unwrap_or(0.0);
                work.channels[idx] = Some(scale_to(cur, factor, bounds));
            }
        }
    }
    let back = convert_modern(&work, dest);
    Ok(Value::Color(make_modern_in(back, dest)))
}

/// Apply a change/adjust/scale to the alpha channel.
fn apply_alpha(cur: f64, v: &Value, op: ModifyOp, pos: Pos) -> Result<f64, Error> {
    Ok(match op {
        ModifyOp::Change => modern_alpha(Some(v)).unwrap_or(cur),
        ModifyOp::Adjust => {
            let amt = match v {
                Value::Number(n) => {
                    if n.unit == "%" {
                        n.value / 100.0
                    } else {
                        n.value
                    }
                }
                _ => 0.0,
            };
            (cur + amt).clamp(0.0, 1.0)
        }
        ModifyOp::Scale => {
            let factor = scale_pct(v, pos)?;
            scale_to(cur, factor, (0.0, 1.0))
        }
    })
}

/// Read a `scale-color` percentage factor in `[-1, 1]`.
fn scale_pct(v: &Value, pos: Pos) -> Result<f64, Error> {
    match v {
        Value::Number(n) if n.unit == "%" => {
            if n.value < -100.0 || n.value > 100.0 {
                return Err(Error::at(
                    format!("Expected {} to be within -100% and 100%.", n.to_css(false)),
                    pos,
                ));
            }
            Ok(n.value / 100.0)
        }
        Value::Number(n) => Err(Error::at(
            format!("$amount: Expected {} to have unit \"%\".", n.to_css(false)),
            pos,
        )),
        other => Err(Error::at(
            format!("$amount: {} is not a number.", other.to_css(false)),
            pos,
        )),
    }
}

/// Scale `current` by `factor` (`-1..=1`) toward `bounds.1` (positive) or
/// `bounds.0` (negative).
fn scale_to(current: f64, factor: f64, bounds: (f64, f64)) -> f64 {
    if factor > 0.0 {
        current + (bounds.1 - current) * factor
    } else {
        current + (current - bounds.0) * factor
    }
}

/// Validate a channel value's unit for the modern change/adjust path. A hue
/// requires an angle unit (or none); other channels accept `%` or no unit.
fn validate_modify_unit(space: ColorSpace, idx: usize, name: &str, v: &Value, pos: Pos) -> Result<(), Error> {
    let num = match v {
        Value::Number(n) => n,
        Value::Slash(..) | Value::Calc(_) => return Ok(()),
        other => {
            return Err(Error::at(
                format!("${name}: {} is not a number.", other.to_css(false)),
                pos,
            ))
        }
    };
    if space.is_polar(idx) {
        let ok = num.unit.is_empty() || matches!(num.unit.as_str(), "deg" | "grad" | "rad" | "turn");
        if !ok {
            return Err(Error::at(
                format!(
                    "${name}: Expected {} to have an angle unit (deg, grad, rad, turn).",
                    num.to_css(false)
                ),
                pos,
            ));
        }
    } else if !num.unit.is_empty() && num.unit != "%" {
        return Err(Error::at(
            format!(
                "${name}: Expected {} to have unit \"%\" or no units.",
                num.to_css(false)
            ),
            pos,
        ));
    }
    Ok(())
}

/// Clamp an `adjust-color` result channel: lab/lch/oklab/oklch lightness is
/// clamped to its range and lch/oklch chroma is floored at 0; all other
/// channels are left unbounded (matching dart-sass).
fn clamp_adjust_channel(space: ColorSpace, idx: usize, v: f64) -> f64 {
    use ColorSpace::*;
    let names = space.channel_names();
    match (space, names[idx]) {
        (Lab, "lightness") | (Lch, "lightness") => v.clamp(0.0, 100.0),
        (Oklab, "lightness") | (Oklch, "lightness") => v.clamp(0.0, 1.0),
        (Lch, "chroma") | (Oklch, "chroma") => v.max(0.0),
        _ => v,
    }
}
