use super::*;

pub(super) fn fn_rgb(pos_args: &[Value], named: &[(String, Value)], pos: Pos) -> Result<Value, Error> {
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
    if let Some((color, alpha)) = legacy_color_alpha(pos_args, named) {
        // A special first argument (`var()`) is not a color but the call is
        // preserved verbatim by the channels passthrough below; everything
        // else binding to `$color` must be a real color, matching dart's
        // legacy `rgb($color, $alpha)` overload.
        if let Value::Color(c) = color {
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
        // The `$color` must be a real color. dart's legacy `rgb($color,
        // $alpha)` overload only short-circuits to a verbatim passthrough when
        // an argument is specifically a `var()` (not `env()`/`calc()`/any other
        // special), so `rgb(1, var(--foo))` is preserved but `rgb(1, env(--x))`
        // and `rgb(env(--x), 0.5)` both error. A `var()` here falls through to
        // the channels passthrough below, which re-serializes the call.
        if !is_var(color) && !is_var(alpha) {
            return Err(Error::at(
                format!("$color: {} is not a color.", color_arg_css(color)),
                pos,
            ));
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
    channels.validate_rgb_units(&["red", "green", "blue"], pos)?;
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
    Number::unitless(v.round())
}

/// Bind a two-argument legacy `rgb`/`rgba` call to the `rgb($color, $alpha)`
/// overload, returning `(color, alpha)` when it cleanly fits and `None`
/// otherwise (so the caller falls through to the `$channels` path, which
/// preserves a special-value first argument and reports the channel-shape
/// errors dart raises for the other overloads).
///
/// dart resolves a two-argument call to this overload only when both
/// parameters bind: positionally (`rgb(c, a)`) or by the exact names
/// `$color`/`$alpha` (`rgb($color: c, $alpha: a)`, in any order, including a
/// positional `$color` plus a named `$alpha`). Any other named argument
/// (`$green`, `$red`, …) or a positional/named collision leaves the overload
/// unbound — dart then reports `Missing argument $channels.` /
/// `No parameter named $X.` from the `$channels` overload, which the existing
/// channel-collection path already produces (an exit-code match).
fn legacy_color_alpha<'a>(
    pos_args: &'a [Value],
    named: &'a [(String, Value)],
) -> Option<(&'a Value, &'a Value)> {
    if pos_args.len() + named.len() != 2 {
        return None;
    }
    let by_name = |key: &str| named.iter().find(|(k, _)| k == key).map(|(_, v)| v);
    // Every named argument must be one of the overload's parameters, and must
    // not duplicate a parameter already filled positionally.
    for (k, _) in named {
        if k != "color" && k != "alpha" {
            return None;
        }
        let filled_positionally =
            (k == "color" && !pos_args.is_empty()) || (k == "alpha" && pos_args.len() >= 2);
        if filled_positionally {
            return None;
        }
    }
    let color = pos_args.first().or_else(|| by_name("color"))?;
    let alpha = pos_args.get(1).or_else(|| by_name("alpha"))?;
    Some((color, alpha))
}

/// Serialize a `$color` argument for the legacy-overload "is not a color"
/// error, matching dart's inspect form: an unbracketed multi-element list is
/// parenthesized (`(1 2 3)`, `(1, 2, 3)`); every other value (a bracketed
/// list, a single-element list, a map, a quoted string, a number) uses its
/// plain inspect spelling.
fn color_arg_css(v: &Value) -> String {
    match v {
        Value::List(l) if !l.bracketed && l.items.len() >= 2 => {
            format!("({})", crate::builtins::inspect_value(v))
        }
        _ => crate::builtins::inspect_value(v),
    }
}

/// Read an rgb channel value (`0..=255`): a `%` is taken as a fraction of
/// 255. NaN maps to 0, `±Infinity` clamp to the bounds. Delegates to the
/// shared [`channel`] helper for the finite case, then normalizes NaN.
fn rgb_channel(v: &Value, pos: Pos) -> Result<f64, Error> {
    if let Value::Slash(num, _) = v {
        return Ok(clamp_finite(num.value, 0.0, 255.0));
    }
    if let Some(c) = degenerate_value(v) {
        if c.is_nan() {
            return Ok(0.0);
        }
        return Ok(clamp_finite(c, 0.0, 255.0));
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
pub(super) struct Channels {
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

    /// Validate the unit of every `rgb`/`rgba` channel, matching dart-sass:
    /// each `$red`/`$green`/`$blue` must be unitless or carry exactly `%`. Any
    /// other unit (`px`, `deg`, a complex `px*px`, or a unit-bearing degenerate
    /// `calc(infinity * 1px)`) raises `$<param>: Expected <value> to have unit
    /// "%" or no units.`.
    ///
    /// Run only after the special/`none`/relative passthroughs (so `var()`,
    /// `none`, `from …` are preserved) and after the numeric and count checks
    /// (dart reports a non-number or wrong-channel-count channel first). For the
    /// positional comma form (`single == None`) this also supplies the
    /// `$<param>:` prefix on the per-channel "is not a number" error, which
    /// dart attaches but the bare [`channel`] helper does not; the check runs
    /// left-to-right so a non-number channel is reported before a later
    /// bad-unit one, matching dart's two-pass (coerce-then-unit) order.
    fn validate_rgb_units(&self, names: &[&str], pos: Pos) -> Result<(), Error> {
        // The positional form skips `validate_numeric`, so confirm every
        // channel is a number first (with dart's `$<param>:` prefix), matching
        // dart's all-numeric pass before any unit is examined.
        if self.single.is_none() {
            for (i, comp) in self.comps.iter().enumerate() {
                let numeric = matches!(comp, Value::Number(_) | Value::Slash(..)) || is_degenerate_calc(comp);
                if !numeric {
                    return Err(Error::at(
                        format!(
                            "${}: {} is not a number.",
                            names[i.min(names.len() - 1)],
                            comp.to_css(false)
                        ),
                        pos,
                    ));
                }
            }
        }
        for (i, comp) in self.comps.iter().enumerate() {
            if let Some(num) = channel_unit_number(comp) {
                let ok = num.is_unitless() || (!num.has_complex_units() && num.unit() == "%");
                if !ok {
                    return Err(Error::at(
                        format!(
                            "${}: Expected {} to have unit \"%\" or no units.",
                            names[i.min(names.len() - 1)],
                            comp.to_css(false)
                        ),
                        pos,
                    ));
                }
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
            Value::Number(n) if is_hsl && n.is_unitless() => {
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
        Value::Str(crate::value::SassStr {
            text: text.into(),
            quoted: false,
        })
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
    if l.sep == ListSep::Slash {
        // The `<channels> / <alpha>` form (the caller rejects any element count
        // other than two): the first element is the channels (a space list),
        // the second is the alpha.
        if l.items.len() == 2 {
            // Only an unbracketed space list expands into channels; a bracketed
            // first element stays a single value so the caller rejects it
            // ("Expected an unbracketed list"), matching dart-sass.
            let comps = match &l.items[0] {
                Value::List(inner) if inner.sep == ListSep::Space && !inner.bracketed => inner.items.to_vec(),
                other => vec![other.clone()],
            };
            return SplitChannels {
                comps,
                alpha: Some(l.items[1].clone()),
                alpha_split: true,
            };
        }
        return no_split(l.items.to_vec());
    }
    if l.sep != ListSep::Space {
        return no_split(l.items.to_vec());
    }
    let mut items: Vec<Value> = l.items.to_vec();
    // A trailing `n / a` slash-division shows up as a `Slash` whose textual
    // spelling contains `/`; recover the channel and alpha (each may carry a
    // unit, e.g. `50%/0.4`).
    if let Some(Value::Slash(_, repr)) = items.last() {
        if let Some((lhs, rhs)) = repr.split_once('/') {
            let token = |s: &str| parse_number_token(s).or_else(|| parse_degenerate_token(s));
            if let (Some(last), Some(alpha)) = (token(lhs), token(rhs)) {
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
        text: s.to_string().into(),
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
    format!("{}{}", fmt_num(n.value, false), n.unit()) == s
}

/// Parse a CSS number token that may carry a unit (`"3"`, `"0.5"`, `"50%"`)
/// into a [`Number`]. Returns `None` for anything not of that shape.
/// Parse the textual spelling of a degenerate calc back into its number:
/// `calc(NaN)`, `calc(infinity)`, `calc(-infinity)`, and the unit-bearing
/// `calc(<const> * 1<unit>)` forms a slash repr may carry.
fn parse_degenerate_token(s: &str) -> Option<Number> {
    let t = s.trim();
    let inner = t.strip_prefix("calc(")?.strip_suffix(')')?.trim();
    let (const_part, unit) = match inner.split_once('*') {
        Some((c, u)) => (c.trim(), u.trim().strip_prefix('1')?.to_string()),
        None => (inner, String::new()),
    };
    let value = match const_part.to_ascii_lowercase().as_str() {
        "nan" => f64::NAN,
        "infinity" => f64::INFINITY,
        "-infinity" => f64::NEG_INFINITY,
        _ => return None,
    };
    Some(Number::with_unit(value, unit))
}

fn parse_number_token(s: &str) -> Option<Number> {
    let s = s.trim();
    let split = s
        .char_indices()
        .find(|(_, c)| !(c.is_ascii_digit() || matches!(c, '.' | '-' | '+' | 'e' | 'E')))
        .map(|(i, _)| i)
        .unwrap_or(s.len());
    let (num_part, unit) = s.split_at(split);
    let value = num_part.parse::<f64>().ok()?;
    Some(Number::with_unit(value, unit.to_string()))
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

pub(super) fn fn_hsl(pos_args: &[Value], named: &[(String, Value)], pos: Pos) -> Result<Value, Error> {
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
    // hsl/hsla has no two-argument `$color, $alpha` overload (unlike rgb). A
    // plain two-positional call (`hsl(1, 0.5)`, `hsl(#123, 0.5)`) therefore
    // binds to the `hsl($hue, $saturation, $lightness, $alpha)` signature with
    // `$lightness` unfilled, which dart reports as `Missing argument
    // $lightness.`. A special first argument is preserved verbatim above, so it
    // does not reach here; the exotic named-argument fallbacks (which dart maps
    // to the `$channels` overload's `No parameter named …` / `Missing argument
    // $channels.`) keep their existing channel-shape errors (an exit-code
    // match).
    if pos_args.len() == 2 && named.is_empty() {
        return Err(Error::at("Missing argument $lightness.".to_string(), pos));
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
    c.modern = Some(Box::new(ModernColor {
        space: ColorSpace::Hsl,
        channels: [Some(h_norm), Some(s_pct), Some(l_pct)],
        alpha: Some(a),
    }));
    Ok(Value::Color(c))
}

/// Read an hsl hue value in degrees, converting `rad`/`grad`/`turn` units
/// (matching dart-sass's lenient legacy angle handling). Other/empty units
/// are taken as degrees.
fn hsl_hue(v: &Value, pos: Pos) -> Result<f64, Error> {
    match v {
        Value::Number(num) => Ok(match num.unit() {
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

/// The [`Number`] underlying a legacy color channel for unit inspection: a
/// plain number, the quotient of a slash-division (`6px/2`, whose unit decides
/// the channel's), or a degenerate `calc()` that folded to a unit-bearing
/// number (`calc(infinity * 1px)`). Returns `None` for any non-numeric channel
/// (handled by the "is not a number" / passthrough paths).
fn channel_unit_number(v: &Value) -> Option<&Number> {
    match v {
        Value::Number(n) | Value::Slash(n, _) => Some(n),
        Value::Calc(CalcNode::Number(n)) => Some(n),
        _ => None,
    }
}

/// Fold a degenerate `calc()` channel (`calc(NaN)`, `calc(infinity * 1%)`)
/// to its plain number, keeping the unit; any other value passes through.
fn fold_degenerate(v: &Value) -> Value {
    if let Value::Calc(node) = v {
        if let Some(c) = degenerate_const(node) {
            return Value::Number(Number::unitless(c));
        }
        if let CalcNode::Number(n) = node {
            if !n.value.is_finite() {
                return Value::Number(n.clone());
            }
        }
    }
    v.clone()
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
                text: format!("hsla({hue}, {sat}, {light}, {})", fmt_num(av, false)).into(),
                quoted: false,
            }));
        }
        None => "hsl",
    };
    Ok(Value::Str(crate::value::SassStr {
        text: format!("{name}({hue}, {sat}, {light})").into(),
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
    if let Some(c) = degenerate_value(v) {
        {
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
pub(super) fn fn_hwb(pos_args: &[Value], named: &[(String, Value)], pos: Pos) -> Result<Value, Error> {
    let params = ["channels"];
    let n = pos_args.len() + named.len();
    if n != 1 {
        return Err(Error::at(
            format!("Only 1 argument allowed, but {n} were passed."),
            pos,
        ));
    }
    let channels = require(&params, pos_args, named, 0, "hwb", pos)?.clone();
    // A single channels list must be unbracketed and space/slash-separated; a
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
            let shown = if l.bracketed {
                channels.to_css(false)
            } else {
                list_paren_css(&channels)
            };
            return Err(Error::at(format!("$channels: Expected {kind}, was {shown}"), pos));
        }
    }
    let SplitChannels { comps, alpha, .. } = split_channels(&channels);
    // A relative-color call (`hwb(from … )`) or a special function
    // (`var()`/`calc()`/…) anywhere preserves the *original* spelling verbatim
    // (a bare numeric hue keeps its bare form, the `/` alpha separator stays
    // glued), regardless of channel count.
    let is_relative = comps
        .first()
        .is_some_and(|v| matches!(v, Value::Str(s) if !s.quoted && s.text.eq_ignore_ascii_case("from")));
    // A degenerate `calc()` channel or alpha (`calc(NaN)`, `calc(infinity *
    // 1%)`) folds to its number — dart constructs the color and lets the
    // hwb -> hsl legacy serialization propagate the non-finite values —
    // rather than preserving the call verbatim.
    let comps_func = comps.iter().any(|v| is_special(v) && !is_degenerate_calc(v));
    let alpha_func = alpha
        .as_ref()
        .is_some_and(|v| is_special(v) && !is_degenerate_calc(v));
    if is_relative || comps_func || alpha_func {
        return Ok(verbatim_call("hwb", &channels));
    }
    let comps: Vec<Value> = comps.iter().map(fold_degenerate).collect();
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
        let mut w = modern_channel(&comps[1], 100.0);
        let mut bl = modern_channel(&comps[2], 100.0);
        // dart normalizes at CONSTRUCTION (`_colorFromChannels`): when both
        // whiteness and blackness are present and sum past 100, both scale
        // back to a 100 total. Reads then see the normalized storage.
        if let (Some(wv), Some(bv)) = (w, bl) {
            if wv + bv > 100.0 {
                let t = wv + bv;
                w = Some(wv / t * 100.0);
                bl = Some(bv / t * 100.0);
            }
        }
        let mc = ModernColor {
            space: ColorSpace::Hwb,
            channels: [h, w, bl],
            alpha: modern_alpha(alpha.as_ref()),
        };
        return Ok(Value::Color(make_modern(mc)));
    }
    // Whiteness and blackness must carry a `%` unit (dart-sass), reported per
    // channel before the value is read. The hue may be unitless or an angle.
    for (i, cname) in [(1usize, "whiteness"), (2usize, "blackness")] {
        if let Value::Number(num) = &comps[i] {
            if num.unit() != "%" {
                return Err(Error::at(
                    format!(
                        "${cname}: Expected {} to have unit \"%\".",
                        comps[i].to_css(false)
                    ),
                    pos,
                ));
            }
        }
    }
    let h = hsl_hue(&comps[0], pos)?;
    let mut w_pct = num(&comps[1], pos)?;
    let mut b_pct = num(&comps[2], pos)?;
    let a = match &alpha {
        Some(v) => alpha_value(v, pos)?,
        None => 1.0,
    };
    // dart normalizes at CONSTRUCTION (`_colorFromChannels`): a whiteness +
    // blackness sum past 100 scales both back to a 100 total, and every read
    // path (channel getters, inspect) sees the normalized storage. `change`
    // re-normalizes; `adjust`/`scale` results stay raw.
    if w_pct + b_pct > 100.0 {
        let t = w_pct + b_pct;
        w_pct = w_pct / t * 100.0;
        b_pct = b_pct / t * 100.0;
    }
    let mut out = hwb_to_color(h, w_pct, b_pct, a);
    // Carry the modern Hwb tag (so `color.space`/`color.channel` work);
    // serialization uses the classic hsl comma form via `legacy_css`.
    let h_norm = h.rem_euclid(360.0);
    out.modern = Some(Box::new(ModernColor {
        space: ColorSpace::Hwb,
        channels: [Some(h_norm), Some(w_pct), Some(b_pct)],
        alpha: Some(a),
    }));
    Ok(Value::Color(out))
}

/// `sass:color` members without a global alias. The global `hwb()` is
/// modern-only (`hwb($channels)`), but `sass:color` additionally exposes the
/// Sass-legacy comma form `color.hwb($hue, $whiteness, $blackness, $alpha: 1)`,
/// so it cannot reuse the global dispatch.
pub(crate) fn call_module_member(
    member: &str,
    pos_args: &[Value],
    named: &[(String, Value)],
    pos: Pos,
) -> Option<Result<Value, Error>> {
    match member {
        "hwb" => Some(fn_color_hwb(pos_args, named, pos)),
        _ => None,
    }
}

/// `color.hwb()`: the modern single-argument channels form delegates to the
/// global `hwb()`; the comma form rebuilds an `h w b` (+ ` / alpha`) channels
/// value so the global's none/special/compute paths apply unchanged.
fn fn_color_hwb(pos_args: &[Value], named: &[(String, Value)], pos: Pos) -> Result<Value, Error> {
    let n = pos_args.len() + named.len();
    if n > 4 {
        return Err(Error::at(
            format!("Only 4 arguments allowed, but {n} were passed."),
            pos,
        ));
    }
    if n <= 1 {
        return fn_hwb(pos_args, named, pos);
    }
    let params = ["hue", "whiteness", "blackness", "alpha"];
    let ch = Channels::collect("hwb", &params, pos_args, named, pos)?;
    let space = Value::List(List {
        items: ch.comps.into(),
        sep: ListSep::Space,
        bracketed: false,
        keywords: None,
    });
    let channels = match ch.alpha {
        Some(a) => Value::List(List {
            items: vec![space, a].into(),
            sep: ListSep::Slash,
            bracketed: false,
            keywords: None,
        }),
        None => space,
    };
    fn_hwb(&[channels], &[], pos)
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
pub(super) fn fn_lab_family(
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
        // A slash-separated channels list is the `<channels> / <alpha>` form, so
        // dart-sass allows exactly two slash elements (e.g. via `list.slash`).
        if l.sep == ListSep::Slash && l.items.len() != 2 {
            return Err(Error::at(
                format!(
                    "$channels: Only 2 slash-separated elements allowed, but {} were passed.",
                    l.items.len()
                ),
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
                    let ok = num.is_unitless() || matches!(num.unit(), "deg" | "grad" | "rad" | "turn");
                    if !ok {
                        return Err(Error::at(
                            format!(
                                "$hue: Expected {} to have an angle unit (deg, grad, rad, turn).",
                                num.to_css(false)
                            ),
                            pos,
                        ));
                    }
                } else if !num.is_unitless() && num.unit() != "%" {
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
    let is_polar = matches!(name, "lch" | "oklch");
    // Percentage references per CSS Color 4: lab a/b 100% = 125, oklab a/b
    // 100% = 0.4, lch chroma 100% = 150, oklch chroma 100% = 0.4.
    let (ab_base, chroma_base) = match name {
        "lab" => (125.0, 0.0),
        "lch" => (0.0, 150.0),
        "oklab" => (0.4, 0.0),
        _ => (0.0, 0.4), // oklch
    };
    // A degenerate lightness clamps like dart-sass (NaN -> 0, +infinity -> max,
    // -infinity -> 0); a/b/chroma/hue instead keep their non-finite value, which
    // serializes as `calc(...)` (chroma is additionally floored at 0).
    let l = modern_channel(&comps[0], l_base).map(|v| if v.is_nan() { 0.0 } else { v.clamp(0.0, l_max) });
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
pub(super) fn fn_color(pos_args: &[Value], named: &[(String, Value)], pos: Pos) -> Result<Value, Error> {
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
    // Color-space names are ASCII case-insensitive: match and serialize against
    // the lower-cased form, but keep the original spelling for the "Unknown
    // color space" diagnostic (dart-sass shows `color(BOGUS …)` verbatim there).
    let space_lower = space_name.to_ascii_lowercase();
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
    if !is_known_color_space(&space_lower) {
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
                if !num.is_unitless() && num.unit() != "%" {
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
                space_lower,
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
    let space = match predefined_space(&space_lower) {
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
    Value::Str(crate::value::SassStr {
        text: text.into(),
        quoted: false,
    })
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

pub(super) fn fn_mix(pos_args: &[Value], named: &[(String, Value)], pos: Pos) -> Result<Value, Error> {
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
pub(super) enum HueMethod {
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

pub(super) fn fn_adjust_lightness(
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
    require_legacy_color(&c, name, pos)?;
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

pub(super) fn fn_percentage(pos_args: &[Value], named: &[(String, Value)], pos: Pos) -> Result<Value, Error> {
    let params = ["number"];
    max_positional(pos_args, params.len(), pos)?;
    let arg = require(&params, pos_args, named, 0, "percentage", pos)?;
    if let Value::Number(num) = arg {
        if !num.is_unitless() {
            return Err(Error::at(
                format!("$number: Expected {} to have no units.", num.to_css(false)),
                pos,
            ));
        }
    }
    let n = num(arg, pos)?;
    Ok(Value::Number(Number::with_unit(n * 100.0, "%".to_string())))
}

pub(super) fn fn_channel(
    name: &str,
    pos_args: &[Value],
    named: &[(String, Value)],
    pos: Pos,
) -> Result<Value, Error> {
    let params = ["color"];
    max_positional(pos_args, params.len(), pos)?;
    let c = as_color(require(&params, pos_args, named, 0, name, pos)?, pos)?;
    // The legacy red/green/blue getters only support legacy colors.
    if c.modern.as_ref().is_some_and(|m| !m.space.is_legacy()) {
        return Err(Error::at(
            format!(
                "color.{name}() is only supported for legacy colors. Please use color.channel() \
                 instead with an explicit $space argument."
            ),
            pos,
        ));
    }
    let v = match name {
        "red" => c.r,
        "green" => c.g,
        "blue" => c.b,
        _ => 0.0,
    };
    Ok(Value::Number(Number::unitless(v.round())))
}

/// Whether `text` is a Microsoft `alpha()` filter argument: ASCII letters,
/// optional whitespace, then `=` (dart-sass's `^[a-zA-Z]+\s*=` shape).
fn is_ms_filter_arg(text: &str) -> bool {
    let mut chars = text.char_indices().peekable();
    let mut saw_letter = false;
    // One or more ASCII letters.
    while let Some(&(_, c)) = chars.peek() {
        if c.is_ascii_alphabetic() {
            saw_letter = true;
            chars.next();
        } else {
            break;
        }
    }
    if !saw_letter {
        return false;
    }
    // Optional whitespace, then a `=`.
    while let Some(&(_, c)) = chars.peek() {
        if c.is_whitespace() {
            chars.next();
        } else {
            break;
        }
    }
    matches!(chars.peek(), Some(&(_, '=')))
}

pub(super) fn fn_alpha(pos_args: &[Value], named: &[(String, Value)], pos: Pos) -> Result<Value, Error> {
    let params = ["color"];
    // The proprietary Microsoft `alpha()` filter overload: one or more
    // unquoted-string positional arguments that each match `<identifier>=…`
    // (an IE `alpha(opacity=80)` hack, produced by the single-`=` operator) are
    // passed through verbatim as a CSS function instead of being treated as a
    // color. dart-sass accepts this for `color.alpha()` too (with a deprecation
    // warning to stderr) rather than enforcing the one-argument count. The part
    // before the `=` must be ASCII letters (optionally followed by whitespace),
    // so e.g. `1=c` is rejected as a non-color.
    if named.is_empty()
        && !pos_args.is_empty()
        && pos_args
            .iter()
            .all(|v| matches!(v, Value::Str(s) if !s.quoted && is_ms_filter_arg(&s.text)))
    {
        let inner = pos_args
            .iter()
            .map(|v| v.to_css(false))
            .collect::<Vec<_>>()
            .join(", ");
        return Ok(Value::Str(crate::value::SassStr {
            text: format!("alpha({inner})").into(),
            quoted: false,
        }));
    }
    let n = pos_args.len() + named.len();
    if n > 1 {
        return Err(Error::at(
            format!("Only 1 argument allowed, but {n} were passed."),
            pos,
        ));
    }
    let c = as_color(require(&params, pos_args, named, 0, "alpha", pos)?, pos)?;
    // The legacy alpha getter only supports legacy colors.
    if c.modern.as_ref().is_some_and(|m| !m.space.is_legacy()) {
        return Err(Error::at(
            "color.alpha() is only supported for legacy colors. Please use color.channel() \
             instead."
                .to_string(),
            pos,
        ));
    }
    Ok(Value::Number(Number::unitless(c.a)))
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
