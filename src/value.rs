//! The Sass value model and its CSS serialization.
//!
//! Numbers carry a unit, colors keep full `f64` channel precision (so
//! computed colors serialize exactly like current dart-sass, e.g.
//! `rgb(63.75, 127.5, 191.25)`), and colors remember their authored
//! spelling so untransformed literals round-trip unchanged.

/// A fully-evaluated Sass value.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Value {
    /// A number with an optional unit.
    Number(Number),
    /// An sRGB color with an alpha channel.
    Color(Color),
    /// A quoted or unquoted string.
    Str(SassStr),
    /// A space- or comma-separated list.
    List(List),
    /// A boolean.
    Bool(bool),
    /// The `null` value.
    Null,
}

/// A number and its unit (`unit` is empty for unitless numbers).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Number {
    pub value: f64,
    pub unit: String,
}

/// A string value; `quoted` controls whether it serializes with quotes.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct SassStr {
    pub text: String,
    pub quoted: bool,
}

/// A list value.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct List {
    pub items: Vec<Value>,
    pub sep: ListSep,
}

/// List separator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ListSep {
    /// Space-separated (`1px 2px`).
    Space,
    /// Comma-separated (`1px, 2px`).
    Comma,
}

/// An sRGB color. Channels are `0..=255` and may be fractional; alpha is
/// `0..=1`.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Color {
    pub r: f64,
    pub g: f64,
    pub b: f64,
    pub a: f64,
    /// The authored spelling (`"red"`, `"#336699"`). Used verbatim when
    /// the color is emitted unchanged; `None` for computed colors.
    pub repr: Option<String>,
}

impl Value {
    /// Serialize as it would appear in a CSS declaration value.
    pub(crate) fn to_css(&self, compressed: bool) -> String {
        match self {
            Value::Number(n) => n.to_css(compressed),
            Value::Color(c) => c.to_css(compressed),
            Value::Str(s) => {
                if s.quoted {
                    format!("\"{}\"", s.text)
                } else {
                    s.text.clone()
                }
            }
            Value::List(l) => l.to_css(compressed),
            Value::Bool(b) => b.to_string(),
            Value::Null => String::new(),
        }
    }

    /// Serialize as it would appear inside `#{...}` interpolation, where
    /// strings lose their quotes.
    pub(crate) fn to_interp(&self) -> String {
        match self {
            Value::Str(s) => s.text.clone(),
            Value::Null => String::new(),
            Value::List(l) => l.to_interp(),
            other => other.to_css(false),
        }
    }

    pub(crate) fn type_name(&self) -> &'static str {
        match self {
            Value::Number(_) => "number",
            Value::Color(_) => "color",
            Value::Str(_) => "string",
            Value::List(_) => "list",
            Value::Bool(_) => "bool",
            Value::Null => "null",
        }
    }

    /// Sass truthiness: everything except `false` and `null` is truthy.
    pub(crate) fn is_truthy(&self) -> bool {
        !matches!(self, Value::Bool(false) | Value::Null)
    }

    /// Sass `==` equality. Numbers compare by value and unit; strings by
    /// text (quotedness is ignored); colors by channel; lists structurally.
    pub(crate) fn sass_eq(&self, other: &Value) -> bool {
        match (self, other) {
            (Value::Number(a), Value::Number(b)) => a.value == b.value && a.unit == b.unit,
            (Value::Str(a), Value::Str(b)) => a.text == b.text,
            (Value::Color(a), Value::Color(b)) => a.r == b.r && a.g == b.g && a.b == b.b && a.a == b.a,
            (Value::Bool(a), Value::Bool(b)) => a == b,
            (Value::Null, Value::Null) => true,
            (Value::List(a), Value::List(b)) => {
                a.sep == b.sep
                    && a.items.len() == b.items.len()
                    && a.items.iter().zip(&b.items).all(|(x, y)| x.sass_eq(y))
            }
            _ => false,
        }
    }
}

impl Number {
    pub(crate) fn to_css(&self, compressed: bool) -> String {
        format!("{}{}", fmt_num(self.value, compressed), self.unit)
    }
}

impl List {
    fn to_css(&self, compressed: bool) -> String {
        let sep = match (self.sep, compressed) {
            (ListSep::Space, _) => " ",
            (ListSep::Comma, true) => ",",
            (ListSep::Comma, false) => ", ",
        };
        self.items
            .iter()
            .filter(|v| !matches!(v, Value::Null))
            .map(|v| v.to_css(compressed))
            .collect::<Vec<_>>()
            .join(sep)
    }

    fn to_interp(&self) -> String {
        let sep = match self.sep {
            ListSep::Space => " ",
            ListSep::Comma => ", ",
        };
        self.items
            .iter()
            .filter(|v| !matches!(v, Value::Null))
            .map(|v| v.to_interp())
            .collect::<Vec<_>>()
            .join(sep)
    }
}

impl Color {
    pub(crate) fn rgb(r: f64, g: f64, b: f64, a: f64) -> Self {
        Color {
            r,
            g,
            b,
            a,
            repr: None,
        }
    }

    /// Parse the hex digits following a `#` (3/4/6/8 long). Returns `None`
    /// on an invalid length or digit.
    pub(crate) fn from_hex(digits: &str) -> Option<Color> {
        let parse = |s: &str| u8::from_str_radix(s, 16).ok().map(|v| v as f64);
        let (r, g, b, a) = match digits.len() {
            3 => {
                let d: Vec<char> = digits.chars().collect();
                (
                    parse(&format!("{0}{0}", d[0]))?,
                    parse(&format!("{0}{0}", d[1]))?,
                    parse(&format!("{0}{0}", d[2]))?,
                    255.0,
                )
            }
            4 => {
                let d: Vec<char> = digits.chars().collect();
                (
                    parse(&format!("{0}{0}", d[0]))?,
                    parse(&format!("{0}{0}", d[1]))?,
                    parse(&format!("{0}{0}", d[2]))?,
                    parse(&format!("{0}{0}", d[3]))?,
                )
            }
            6 => (
                parse(&digits[0..2])?,
                parse(&digits[2..4])?,
                parse(&digits[4..6])?,
                255.0,
            ),
            8 => (
                parse(&digits[0..2])?,
                parse(&digits[2..4])?,
                parse(&digits[4..6])?,
                parse(&digits[6..8])?,
            ),
            _ => return None,
        };
        let a = a / 255.0;
        // Opaque hex round-trips as a canonical lowercase 6-digit hex,
        // matching dart-sass; alpha hex falls back to computed rgba().
        let repr = if (a - 1.0).abs() < f64::EPSILON {
            Some(format!(
                "#{:02x}{:02x}{:02x}",
                r.round() as u8,
                g.round() as u8,
                b.round() as u8
            ))
        } else {
            None
        };
        Some(Color { r, g, b, a, repr })
    }

    /// Convert to HSL: hue in degrees `[0,360)`, saturation/lightness in
    /// `[0,1]`.
    pub(crate) fn to_hsl(&self) -> (f64, f64, f64) {
        let r = self.r / 255.0;
        let g = self.g / 255.0;
        let b = self.b / 255.0;
        let max = r.max(g).max(b);
        let min = r.min(g).min(b);
        let l = (max + min) / 2.0;
        let d = max - min;
        let s = if d == 0.0 {
            0.0
        } else {
            d / (1.0 - (2.0 * l - 1.0).abs())
        };
        let h = if d == 0.0 {
            0.0
        } else if max == r {
            60.0 * (((g - b) / d).rem_euclid(6.0))
        } else if max == g {
            60.0 * ((b - r) / d + 2.0)
        } else {
            60.0 * ((r - g) / d + 4.0)
        };
        (h.rem_euclid(360.0), s, l)
    }

    /// Build a color from HSL (hue degrees, sat/light `[0,1]`) + alpha.
    pub(crate) fn from_hsl(h: f64, s: f64, l: f64, a: f64) -> Color {
        let h = h.rem_euclid(360.0);
        let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
        let x = c * (1.0 - (((h / 60.0) % 2.0) - 1.0).abs());
        let m = l - c / 2.0;
        let (r1, g1, b1) = if h < 60.0 {
            (c, x, 0.0)
        } else if h < 120.0 {
            (x, c, 0.0)
        } else if h < 180.0 {
            (0.0, c, x)
        } else if h < 240.0 {
            (0.0, x, c)
        } else if h < 300.0 {
            (x, 0.0, c)
        } else {
            (c, 0.0, x)
        };
        Color::rgb((r1 + m) * 255.0, (g1 + m) * 255.0, (b1 + m) * 255.0, a)
    }

    fn channels_are_int(&self) -> bool {
        let int = |v: f64| (v - v.round()).abs() < 1e-9;
        int(self.r) && int(self.g) && int(self.b)
    }

    pub(crate) fn to_css(&self, compressed: bool) -> String {
        if !compressed {
            if let Some(repr) = &self.repr {
                return repr.clone();
            }
        }
        let opaque = (self.a - 1.0).abs() < f64::EPSILON;
        if opaque && self.channels_are_int() {
            let hex = format!(
                "#{:02x}{:02x}{:02x}",
                self.r.round().clamp(0.0, 255.0) as u8,
                self.g.round().clamp(0.0, 255.0) as u8,
                self.b.round().clamp(0.0, 255.0) as u8
            );
            if compressed {
                return shorten_hex(&hex);
            }
            return hex;
        }
        let (r, g, b) = (
            fmt_num(self.r, compressed),
            fmt_num(self.g, compressed),
            fmt_num(self.b, compressed),
        );
        if opaque {
            if compressed {
                format!("rgb({r},{g},{b})")
            } else {
                format!("rgb({r}, {g}, {b})")
            }
        } else {
            let a = fmt_num(self.a, compressed);
            if compressed {
                format!("rgba({r},{g},{b},{a})")
            } else {
                format!("rgba({r}, {g}, {b}, {a})")
            }
        }
    }
}

fn shorten_hex(hex: &str) -> String {
    // `#aabbcc` -> `#abc` when each channel's nibbles match.
    let b = hex.as_bytes();
    if b.len() == 7 && b[1] == b[2] && b[3] == b[4] && b[5] == b[6] {
        format!("#{}{}{}", b[1] as char, b[3] as char, b[5] as char)
    } else {
        hex.to_string()
    }
}

/// Format a number the way dart-sass does: round to 10 decimal places,
/// trim trailing zeros, and (when compressed) drop a leading `0`.
pub(crate) fn fmt_num(n: f64, compressed: bool) -> String {
    if n.is_nan() {
        return "NaN".to_string();
    }
    if n.is_infinite() {
        return if n > 0.0 {
            "Infinity".to_string()
        } else {
            "-Infinity".to_string()
        };
    }
    let rounded = (n * 1e10).round() / 1e10;
    let mut s = format!("{rounded:.10}");
    if s.contains('.') {
        while s.ends_with('0') {
            s.pop();
        }
        if s.ends_with('.') {
            s.pop();
        }
    }
    if s == "-0" {
        s = "0".to_string();
    }
    if compressed {
        if let Some(rest) = s.strip_prefix("0.") {
            s = format!(".{rest}");
        } else if let Some(rest) = s.strip_prefix("-0.") {
            s = format!("-.{rest}");
        }
    }
    s
}

/// Look up a CSS named color. Covers the basic set plus the most common
/// extended names; unknown identifiers stay unquoted strings.
pub(crate) fn named_color(name: &str) -> Option<Color> {
    let (r, g, b, a) = match name.to_ascii_lowercase().as_str() {
        "transparent" => (0, 0, 0, 0.0),
        "black" => (0, 0, 0, 1.0),
        "silver" => (192, 192, 192, 1.0),
        "gray" | "grey" => (128, 128, 128, 1.0),
        "white" => (255, 255, 255, 1.0),
        "maroon" => (128, 0, 0, 1.0),
        "red" => (255, 0, 0, 1.0),
        "purple" => (128, 0, 128, 1.0),
        "fuchsia" | "magenta" => (255, 0, 255, 1.0),
        "green" => (0, 128, 0, 1.0),
        "lime" => (0, 255, 0, 1.0),
        "olive" => (128, 128, 0, 1.0),
        "yellow" => (255, 255, 0, 1.0),
        "navy" => (0, 0, 128, 1.0),
        "blue" => (0, 0, 255, 1.0),
        "teal" => (0, 128, 128, 1.0),
        "aqua" | "cyan" => (0, 255, 255, 1.0),
        "orange" => (255, 165, 0, 1.0),
        "pink" => (255, 192, 203, 1.0),
        "brown" => (165, 42, 42, 1.0),
        "gold" => (255, 215, 0, 1.0),
        "coral" => (255, 127, 80, 1.0),
        "salmon" => (250, 128, 114, 1.0),
        "tomato" => (255, 99, 71, 1.0),
        "orchid" => (218, 112, 214, 1.0),
        "indigo" => (75, 0, 130, 1.0),
        "violet" => (238, 130, 238, 1.0),
        "khaki" => (240, 230, 140, 1.0),
        "crimson" => (220, 20, 60, 1.0),
        "skyblue" => (135, 206, 235, 1.0),
        "tan" => (210, 180, 140, 1.0),
        "turquoise" => (64, 224, 208, 1.0),
        _ => return None,
    };
    Some(Color {
        r: r as f64,
        g: g as f64,
        b: b as f64,
        a,
        repr: Some(name.to_string()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fmt_num_trims_and_rounds() {
        assert_eq!(fmt_num(153.0, false), "153");
        assert_eq!(fmt_num(178.5, false), "178.5");
        assert_eq!(fmt_num(0.5, false), "0.5");
        assert_eq!(fmt_num(-0.0, false), "0");
        assert_eq!(fmt_num(16.0, false), "16");
    }

    #[test]
    fn fmt_num_compressed_drops_leading_zero() {
        assert_eq!(fmt_num(0.5, true), ".5");
        assert_eq!(fmt_num(-0.25, true), "-.25");
        assert_eq!(fmt_num(2.0, true), "2");
    }

    #[test]
    fn hex_parsing_and_serialization() {
        let c = Color::from_hex("336699").expect("valid hex");
        assert_eq!(c.to_css(false), "#336699");
        // 3-digit expands to canonical 6-digit lowercase.
        let short = Color::from_hex("369").expect("valid hex");
        assert_eq!(short.to_css(false), "#336699");
    }

    #[test]
    fn hsl_roundtrip_is_exact_for_integer_rgb() {
        // #336699 == hsl(210, 50%, 40%)
        let c = Color::from_hex("336699").expect("valid hex");
        let (h, s, l) = c.to_hsl();
        assert!((h - 210.0).abs() < 1e-9);
        assert!((s - 0.5).abs() < 1e-9);
        assert!((l - 0.4).abs() < 1e-9);
        // lighten by 10% -> exactly dart-sass's fractional rgb.
        let lit = Color::from_hsl(h, s, l + 0.1, 1.0);
        assert_eq!(lit.to_css(false), "rgb(63.75, 127.5, 191.25)");
    }

    #[test]
    fn computed_fractional_channels_serialize_as_rgb() {
        let c = Color::rgb(153.0, 178.5, 204.0, 1.0);
        assert_eq!(c.to_css(false), "rgb(153, 178.5, 204)");
    }

    #[test]
    fn alpha_color_serializes_as_rgba() {
        let c = Color::rgb(0.0, 0.0, 0.0, 0.5);
        assert_eq!(c.to_css(false), "rgba(0, 0, 0, 0.5)");
    }

    #[test]
    fn named_colors_resolve_and_preserve_spelling() {
        let red = named_color("red").expect("named");
        assert_eq!(red.to_css(false), "red");
        assert!(named_color("definitely-not-a-color").is_none());
    }
}
