//! String built-in functions (`unquote`, `quote`, `str-length`, …).
//!
//! Shared argument helpers live in the parent module:
//! `super::{arg, require, num, as_color, channel, clamp01}`. Return
//! `Some(Ok(..))`/`Some(Err(..))` for a name this family owns, or `None`
//! to let the next family try.
//!
//! Sass counts Unicode characters (not bytes) and indexes strings 1-based,
//! with negative indices counting back from the end.

use crate::error::Error;
use crate::scanner::Pos;
use crate::value::{Number, SassStr, Value};

pub(super) fn try_call(
    name: &str,
    pos_args: &[Value],
    named: &[(String, Value)],
    pos: Pos,
) -> Option<Result<Value, Error>> {
    Some(match name {
        "quote" => fn_set_quoted(pos_args, named, pos, true),
        "unquote" => fn_set_quoted(pos_args, named, pos, false),
        "to-upper-case" => fn_change_case(pos_args, named, pos, true),
        "to-lower-case" => fn_change_case(pos_args, named, pos, false),
        "str-length" => fn_str_length(pos_args, named, pos),
        "str-index" => fn_str_index(pos_args, named, pos),
        "str-slice" => fn_str_slice(pos_args, named, pos),
        "str-insert" => fn_str_insert(pos_args, named, pos),
        _ => return None,
    })
}

/// Extract `(text, quoted)` from a required string argument, erroring with
/// dart-sass's `$<param>: <value> is not a string.` message otherwise.
fn require_string<'v>(
    params: &[&str],
    pos_args: &'v [Value],
    named: &'v [(String, Value)],
    i: usize,
    fname: &str,
    pos: Pos,
) -> Result<(&'v str, bool), Error> {
    let v = super::require(params, pos_args, named, i, fname, pos)?;
    match v {
        Value::Str(SassStr { text, quoted }) => Ok((text.as_str(), *quoted)),
        other => Err(Error::at(
            format!(
                "${}: {} is not a string.",
                params.get(i).copied().unwrap_or(""),
                other.to_css(false)
            ),
            pos,
        )),
    }
}

/// Extract a unitless integer index, matching dart-sass which rejects any
/// unit on string indices.
fn require_index(
    params: &[&str],
    pos_args: &[Value],
    named: &[(String, Value)],
    i: usize,
    fname: &str,
    pos: Pos,
) -> Result<i64, Error> {
    let v = super::require(params, pos_args, named, i, fname, pos)?;
    let pname = params.get(i).copied().unwrap_or("");
    match v {
        Value::Number(Number { value, unit }) => {
            if unit.is_empty() {
                Ok(value.round() as i64)
            } else {
                Err(Error::at(
                    format!("${pname}: Expected {} to have no units.", v.to_css(false)),
                    pos,
                ))
            }
        }
        other => Err(Error::at(
            format!("${pname}: {} is not a number.", other.to_css(false)),
            pos,
        )),
    }
}

fn quoted_str(text: String, quoted: bool) -> Value {
    Value::Str(SassStr { text, quoted })
}

/// `quote($string)` / `unquote($string)`. dart-sass 1.x rejects non-string
/// arguments for both with `$string: ... is not a string.`.
fn fn_set_quoted(
    pos_args: &[Value],
    named: &[(String, Value)],
    pos: Pos,
    quoted: bool,
) -> Result<Value, Error> {
    let fname = if quoted { "quote" } else { "unquote" };
    let (text, _) = require_string(&["string"], pos_args, named, 0, fname, pos)?;
    Ok(quoted_str(text.to_string(), quoted))
}

/// `to-upper-case` / `to-lower-case`: ASCII-only case change, preserving the
/// argument's quotedness.
fn fn_change_case(
    pos_args: &[Value],
    named: &[(String, Value)],
    pos: Pos,
    upper: bool,
) -> Result<Value, Error> {
    let fname = if upper { "to-upper-case" } else { "to-lower-case" };
    let (text, quoted) = require_string(&["string"], pos_args, named, 0, fname, pos)?;
    let mapped: String = if upper {
        text.chars().map(|c| c.to_ascii_uppercase()).collect()
    } else {
        text.chars().map(|c| c.to_ascii_lowercase()).collect()
    };
    Ok(quoted_str(mapped, quoted))
}

/// `str-length($string)`: unitless character count.
fn fn_str_length(pos_args: &[Value], named: &[(String, Value)], pos: Pos) -> Result<Value, Error> {
    let (text, _) = require_string(&["string"], pos_args, named, 0, "str-length", pos)?;
    Ok(Value::Number(Number {
        value: text.chars().count() as f64,
        unit: String::new(),
    }))
}

/// `str-index($string, $substring)`: 1-based char index of the first
/// occurrence, or `null` when absent.
fn fn_str_index(pos_args: &[Value], named: &[(String, Value)], pos: Pos) -> Result<Value, Error> {
    let params = ["string", "substring"];
    let (text, _) = require_string(&params, pos_args, named, 0, "str-index", pos)?;
    let (sub, _) = require_string(&params, pos_args, named, 1, "str-index", pos)?;
    match text.find(sub) {
        // `find` gives a byte offset; convert to a 1-based char index.
        Some(byte) => Ok(Value::Number(Number {
            value: (text[..byte].chars().count() + 1) as f64,
            unit: String::new(),
        })),
        None => Ok(Value::Null),
    }
}

/// Normalize a 1-based (possibly negative) index into a 1-based position used
/// for slicing. Index `0` stays `0`; negatives count from the end.
fn normalize_index(index: i64, len: i64) -> i64 {
    if index > 0 {
        index
    } else if index == 0 {
        0
    } else {
        len + index + 1
    }
}

/// `str-slice($string, $start-at, $end-at: -1)`: 1-based inclusive slice with
/// negative indices counting from the end; preserves quotedness.
fn fn_str_slice(pos_args: &[Value], named: &[(String, Value)], pos: Pos) -> Result<Value, Error> {
    let params = ["string", "start-at", "end-at"];
    let (text, quoted) = require_string(&params, pos_args, named, 0, "str-slice", pos)?;
    let chars: Vec<char> = text.chars().collect();
    let len = chars.len() as i64;

    let start_at = require_index(&params, pos_args, named, 1, "str-slice", pos)?;
    let end_at = match super::arg(&params, pos_args, named, 2) {
        Some(_) => require_index(&params, pos_args, named, 2, "str-slice", pos)?,
        None => -1,
    };

    // Resolve to 1-based positions, then clamp into range.
    let start = normalize_index(start_at, len).max(1);
    let end = normalize_index(end_at, len).min(len);

    if start > end || start > len || end < 1 {
        return Ok(quoted_str(String::new(), quoted));
    }
    // Indices are valid 1..=len here; convert to 0-based slice bounds.
    let lo = (start - 1) as usize;
    let hi = end as usize;
    let slice: String = chars[lo..hi].iter().collect();
    Ok(quoted_str(slice, quoted))
}

/// `str-insert($string, $insert, $index)`: insert at a 1-based index
/// (negative counts from the end); preserves the host string's quotedness.
fn fn_str_insert(pos_args: &[Value], named: &[(String, Value)], pos: Pos) -> Result<Value, Error> {
    let params = ["string", "insert", "index"];
    let (text, quoted) = require_string(&params, pos_args, named, 0, "str-insert", pos)?;
    let (insert, _) = require_string(&params, pos_args, named, 1, "str-insert", pos)?;
    let chars: Vec<char> = text.chars().collect();
    let len = chars.len() as i64;
    let index = require_index(&params, pos_args, named, 2, "str-insert", pos)?;

    // Compute the 0-based offset at which `insert` is placed.
    let offset: usize = if index > 0 {
        (index - 1).min(len).max(0) as usize
    } else if index == 0 {
        0
    } else {
        (len + index + 1).clamp(0, len) as usize
    };

    let mut out = String::new();
    out.extend(chars[..offset].iter());
    out.push_str(insert);
    out.extend(chars[offset..].iter());
    Ok(quoted_str(out, quoted))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pos() -> Pos {
        Pos { line: 1, col: 1 }
    }

    fn s(text: &str, quoted: bool) -> Value {
        Value::Str(SassStr {
            text: text.to_string(),
            quoted,
        })
    }

    fn n(value: f64) -> Value {
        Value::Number(Number {
            value,
            unit: String::new(),
        })
    }

    fn call(name: &str, args: &[Value]) -> Value {
        try_call(name, args, &[], pos())
            .expect("name owned by string family")
            .expect("no error")
    }

    #[test]
    fn quote_and_unquote_flip_quotedness() {
        assert_eq!(call("quote", &[s("hello", false)]).to_css(false), "\"hello\"");
        assert_eq!(call("unquote", &[s("foo", true)]).to_css(false), "foo");
    }

    #[test]
    fn quote_rejects_non_strings() {
        let err = try_call("quote", &[n(42.0)], &[], pos());
        assert!(err.expect("owned").is_err());
        let err = try_call("unquote", &[Value::Bool(true)], &[], pos());
        assert!(err.expect("owned").is_err());
    }

    #[test]
    fn case_change_is_ascii_and_preserves_quoting() {
        assert_eq!(
            call("to-upper-case", &[s("aBc-Def", true)]).to_css(false),
            "\"ABC-DEF\""
        );
        assert_eq!(call("to-lower-case", &[s("ABC", false)]).to_css(false), "abc");
    }

    #[test]
    fn str_length_counts_unicode_chars() {
        assert_eq!(call("str-length", &[s("hello", true)]).to_css(false), "5");
        assert_eq!(call("str-length", &[s("café", true)]).to_css(false), "4");
    }

    #[test]
    fn str_index_is_one_based_or_null() {
        assert_eq!(
            call("str-index", &[s("Hello World", true), s("o", true)]).to_css(false),
            "5"
        );
        assert!(matches!(
            call("str-index", &[s("abc", true), s("z", true)]),
            Value::Null
        ));
    }

    #[test]
    fn str_slice_handles_negatives_and_defaults() {
        assert_eq!(
            call("str-slice", &[s("abcdef", true), n(2.0), n(4.0)]).to_css(false),
            "\"bcd\""
        );
        assert_eq!(
            call("str-slice", &[s("abcdef", true), n(2.0)]).to_css(false),
            "\"bcdef\""
        );
        assert_eq!(
            call("str-slice", &[s("abcdef", true), n(-3.0), n(-1.0)]).to_css(false),
            "\"def\""
        );
        // start > end yields empty; start 0 acts like 1.
        assert_eq!(
            call("str-slice", &[s("abcdef", true), n(4.0), n(2.0)]).to_css(false),
            "\"\""
        );
        assert_eq!(
            call("str-slice", &[s("abcde", false), n(0.0), n(3.0)]).to_css(false),
            "abc"
        );
        // end 0 resolves before the first char -> empty.
        assert_eq!(
            call("str-slice", &[s("abcde", true), n(2.0), n(0.0)]).to_css(false),
            "\"\""
        );
    }

    #[test]
    fn str_insert_positions_and_clamps() {
        assert_eq!(
            call("str-insert", &[s("abc", true), s("X", true), n(2.0)]).to_css(false),
            "\"aXbc\""
        );
        assert_eq!(
            call("str-insert", &[s("abc", true), s("X", true), n(-1.0)]).to_css(false),
            "\"abcX\""
        );
        assert_eq!(
            call("str-insert", &[s("abc", true), s("X", true), n(100.0)]).to_css(false),
            "\"abcX\""
        );
        assert_eq!(
            call("str-insert", &[s("abc", false), s("X", true), n(-5.0)]).to_css(false),
            "Xabc"
        );
    }

    #[test]
    fn rejects_unknown_names_and_units() {
        assert!(try_call("frobnicate", &[s("x", true)], &[], pos()).is_none());
        let unit_idx = Value::Number(Number {
            value: 2.0,
            unit: "px".to_string(),
        });
        let err = try_call("str-slice", &[s("abc", true), unit_idx], &[], pos());
        assert!(err.expect("owned").is_err());
    }
}
