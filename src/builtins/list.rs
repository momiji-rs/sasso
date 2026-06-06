//! List built-in functions (`length`, `nth`, `set-nth`, `join`, `append`,
//! `index`, `list-separator`).
//!
//! A non-list value behaves as a single-element list (length 1); the empty
//! list `()` has length 0. Sass indexes lists 1-based, with negative indices
//! counting back from the end. Shared argument helpers live in the parent
//! module: `super::{arg, require, num, as_color, channel, clamp01}`. Return
//! `Some(Ok(..))`/`Some(Err(..))` for a name this family owns, or `None` to
//! let the next family try.

use crate::error::Error;
use crate::scanner::Pos;
use crate::value::{List, ListSep, Number, SassStr, Value};

pub(super) fn try_call(
    name: &str,
    pos_args: &[Value],
    named: &[(String, Value)],
    pos: Pos,
) -> Option<Result<Value, Error>> {
    Some(match name {
        "length" => fn_length(pos_args, named, pos),
        "nth" => fn_nth(pos_args, named, pos),
        "set-nth" => fn_set_nth(pos_args, named, pos),
        "join" => fn_join(pos_args, named, pos),
        "append" => fn_append(pos_args, named, pos),
        "index" => fn_index(pos_args, named, pos),
        "list-separator" => fn_list_separator(pos_args, named, pos),
        _ => return None,
    })
}

/// Borrow a value as a list of elements and its separator. A list yields its
/// own items and separator; any other value (including `null`) is a
/// single-element list. dart-sass reports a lone non-list as space-separated.
fn as_items(v: &Value) -> (Vec<Value>, ListSep) {
    match v {
        Value::List(l) => (l.items.clone(), l.sep),
        other => (vec![other.clone()], ListSep::Space),
    }
}

/// The element count of a value treated as a list.
fn list_len(v: &Value) -> usize {
    match v {
        Value::List(l) => l.items.len(),
        _ => 1,
    }
}

fn unitless(value: f64) -> Value {
    Value::Number(Number {
        value,
        unit: String::new(),
    })
}

/// Resolve a 1-based (possibly negative) Sass index against a list of `len`
/// elements into a 0-based offset, erroring exactly like dart-sass on a
/// non-integer, zero, or out-of-range index. `$n` may carry a unit, which is
/// ignored (matching dart-sass).
fn resolve_index(
    params: &[&str],
    pos_args: &[Value],
    named: &[(String, Value)],
    i: usize,
    fname: &str,
    len: usize,
    pos: Pos,
) -> Result<usize, Error> {
    let v = super::require(params, pos_args, named, i, fname, pos)?;
    let pname = params.get(i).copied().unwrap_or("");
    let raw = match v {
        Value::Number(n) => n.value,
        other => {
            return Err(Error::at(
                format!("${pname}: {} is not a number.", other.to_css(false)),
                pos,
            ))
        }
    };
    if raw.fract() != 0.0 {
        return Err(Error::at(
            format!("${pname}: {} is not an int.", crate::value::fmt_num(raw, false)),
            pos,
        ));
    }
    let index = raw as i64;
    if index == 0 {
        return Err(Error::at(format!("${pname}: List index may not be 0."), pos));
    }
    let len_i = len as i64;
    if index.abs() > len_i {
        return Err(Error::at(
            format!(
                "${pname}: Invalid index {} for a list with {} element{}.",
                crate::value::fmt_num(raw, false),
                len,
                if len == 1 { "" } else { "s" }
            ),
            pos,
        ));
    }
    // Map 1-based / negative index to a 0-based offset.
    let zero_based = if index > 0 { index - 1 } else { len_i + index };
    Ok(zero_based as usize)
}

/// Parse a `$separator` argument into a concrete `ListSep`, or `None` for the
/// keyword `auto`. dart-sass also accepts `slash`, which this list model
/// cannot represent; it is rejected with the standard message.
fn parse_separator(v: &Value, pos: Pos) -> Result<Option<ListSep>, Error> {
    if let Value::Str(SassStr { text, .. }) = v {
        match text.as_str() {
            "auto" => return Ok(None),
            "comma" => return Ok(Some(ListSep::Comma)),
            "space" => return Ok(Some(ListSep::Space)),
            _ => {}
        }
    }
    Err(Error::at(
        "$separator: Must be \"space\", \"comma\", \"slash\", or \"auto\".",
        pos,
    ))
}

/// `length($list)`: the unitless element count.
fn fn_length(pos_args: &[Value], named: &[(String, Value)], pos: Pos) -> Result<Value, Error> {
    let v = super::require(&["list"], pos_args, named, 0, "length", pos)?;
    Ok(unitless(list_len(v) as f64))
}

/// `nth($list, $n)`: the element at the 1-based (negative-from-end) index.
fn fn_nth(pos_args: &[Value], named: &[(String, Value)], pos: Pos) -> Result<Value, Error> {
    let params = ["list", "n"];
    let list = super::require(&params, pos_args, named, 0, "nth", pos)?;
    let (items, _) = as_items(list);
    let idx = resolve_index(&params, pos_args, named, 1, "nth", items.len(), pos)?;
    items
        .get(idx)
        .cloned()
        .ok_or_else(|| Error::at("Internal index error.", pos))
}

/// `set-nth($list, $n, $value)`: a copy of the list with one element replaced,
/// preserving the original separator.
fn fn_set_nth(pos_args: &[Value], named: &[(String, Value)], pos: Pos) -> Result<Value, Error> {
    let params = ["list", "n", "value"];
    let list = super::require(&params, pos_args, named, 0, "set-nth", pos)?;
    let (mut items, sep) = as_items(list);
    let idx = resolve_index(&params, pos_args, named, 1, "set-nth", items.len(), pos)?;
    let value = super::require(&params, pos_args, named, 2, "set-nth", pos)?.clone();
    if let Some(slot) = items.get_mut(idx) {
        *slot = value;
    }
    Ok(Value::List(List { items, sep }))
}

/// The "settled" separator of a value, or `None` when undecided. A `List`
/// value carries its own separator (even when short); a bare value or an empty
/// list has no settled separator, which dart-sass defaults to space.
fn settled_sep(v: &Value) -> Option<ListSep> {
    match v {
        Value::List(l) if !l.items.is_empty() => Some(l.sep),
        _ => None,
    }
}

/// `join($list1, $list2, $separator: auto, $bracketed: auto)`: concatenated
/// list. With `auto`, the separator is list1's settled separator (or list2's
/// when list1 has none), defaulting to space when neither is settled.
/// `$bracketed` is accepted but ignored (this list model has no bracket flag).
fn fn_join(pos_args: &[Value], named: &[(String, Value)], pos: Pos) -> Result<Value, Error> {
    let params = ["list1", "list2", "separator", "bracketed"];
    let list1 = super::require(&params, pos_args, named, 0, "join", pos)?;
    let list2 = super::require(&params, pos_args, named, 1, "join", pos)?;
    let (items1, _) = as_items(list1);
    let (items2, _) = as_items(list2);

    let sep = match super::arg(&params, pos_args, named, 2) {
        Some(v) => match parse_separator(v, pos)? {
            Some(s) => s,
            None => join_auto_separator(list1, list2),
        },
        None => join_auto_separator(list1, list2),
    };

    let mut items = items1;
    items.extend(items2);
    Ok(Value::List(List { items, sep }))
}

/// `join`'s `auto` rule: list1's settled separator, else list2's, else space.
fn join_auto_separator(list1: &Value, list2: &Value) -> ListSep {
    settled_sep(list1)
        .or_else(|| settled_sep(list2))
        .unwrap_or(ListSep::Space)
}

/// `append($list, $val, $separator: auto)`: the list with `$val` appended.
/// With `auto`, the existing list's settled separator is kept, defaulting to
/// space for a bare value or empty list.
fn fn_append(pos_args: &[Value], named: &[(String, Value)], pos: Pos) -> Result<Value, Error> {
    let params = ["list", "val", "separator"];
    let list = super::require(&params, pos_args, named, 0, "append", pos)?;
    let val = super::require(&params, pos_args, named, 1, "append", pos)?.clone();
    let (mut items, _) = as_items(list);

    let sep = match super::arg(&params, pos_args, named, 2) {
        Some(v) => match parse_separator(v, pos)? {
            Some(s) => s,
            None => settled_sep(list).unwrap_or(ListSep::Space),
        },
        None => settled_sep(list).unwrap_or(ListSep::Space),
    };

    items.push(val);
    Ok(Value::List(List { items, sep }))
}

/// `index($list, $value)`: the 1-based position of the first element equal to
/// `$value` (by Sass `==`), or `null` when absent.
fn fn_index(pos_args: &[Value], named: &[(String, Value)], pos: Pos) -> Result<Value, Error> {
    let params = ["list", "value"];
    let list = super::require(&params, pos_args, named, 0, "index", pos)?;
    let value = super::require(&params, pos_args, named, 1, "index", pos)?;
    let (items, _) = as_items(list);
    match items.iter().position(|item| item.sass_eq(value)) {
        Some(i) => Ok(unitless((i + 1) as f64)),
        None => Ok(Value::Null),
    }
}

/// `list-separator($list)`: the unquoted keyword `comma` or `space`. An empty
/// list or a bare value is "undecided" and reports `space`.
fn fn_list_separator(pos_args: &[Value], named: &[(String, Value)], pos: Pos) -> Result<Value, Error> {
    let v = super::require(&["list"], pos_args, named, 0, "list-separator", pos)?;
    // A bare value or empty list is "undecided", which dart-sass reports as
    // space; otherwise the list's own separator is authoritative.
    let sep = settled_sep(v).unwrap_or(ListSep::Space);
    let text = match sep {
        ListSep::Comma => "comma",
        ListSep::Space => "space",
    };
    Ok(Value::Str(SassStr {
        text: text.to_string(),
        quoted: false,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pos() -> Pos {
        Pos { line: 1, col: 1 }
    }

    fn n(value: f64) -> Value {
        Value::Number(Number {
            value,
            unit: String::new(),
        })
    }

    fn num_unit(value: f64, unit: &str) -> Value {
        Value::Number(Number {
            value,
            unit: unit.to_string(),
        })
    }

    fn s(text: &str) -> Value {
        Value::Str(SassStr {
            text: text.to_string(),
            quoted: false,
        })
    }

    fn list(items: Vec<Value>, sep: ListSep) -> Value {
        Value::List(List { items, sep })
    }

    fn call(name: &str, args: &[Value]) -> Value {
        try_call(name, args, &[], pos())
            .expect("name owned by list family")
            .expect("no error")
    }

    fn call_err(name: &str, args: &[Value]) -> Error {
        try_call(name, args, &[], pos())
            .expect("name owned by list family")
            .expect_err("expected error")
    }

    #[test]
    fn length_counts_elements() {
        assert_eq!(call("length", &[list(vec![], ListSep::Space)]).to_css(false), "0");
        assert_eq!(call("length", &[num_unit(5.0, "px")]).to_css(false), "1");
        assert_eq!(
            call(
                "length",
                &[list(
                    vec![num_unit(1.0, "px"), num_unit(2.0, "px"), num_unit(3.0, "px")],
                    ListSep::Space
                )]
            )
            .to_css(false),
            "3"
        );
    }

    #[test]
    fn nth_one_based_and_negative() {
        let l = list(vec![s("a"), s("b"), s("c")], ListSep::Space);
        assert_eq!(call("nth", &[l.clone(), n(1.0)]).to_css(false), "a");
        assert_eq!(call("nth", &[l.clone(), n(-1.0)]).to_css(false), "c");
        // Single non-list value behaves as a one-element list.
        assert_eq!(call("nth", &[s("solo"), n(1.0)]).to_css(false), "solo");
    }

    #[test]
    fn nth_index_errors() {
        let l = list(vec![s("a"), s("b"), s("c")], ListSep::Space);
        assert_eq!(
            call_err("nth", &[l.clone(), n(0.0)]).message,
            "$n: List index may not be 0."
        );
        assert_eq!(
            call_err("nth", &[l.clone(), n(5.0)]).message,
            "$n: Invalid index 5 for a list with 3 elements."
        );
        assert_eq!(call_err("nth", &[l, n(1.5)]).message, "$n: 1.5 is not an int.");
    }

    #[test]
    fn set_nth_replaces_and_keeps_separator() {
        let l = list(vec![s("a"), s("b"), s("c")], ListSep::Space);
        assert_eq!(
            call("set-nth", &[l.clone(), n(2.0), s("x")]).to_css(false),
            "a x c"
        );
        assert_eq!(call("set-nth", &[l, n(-1.0), s("z")]).to_css(false), "a b z");
        // Single value -> one-element list.
        assert_eq!(call("set-nth", &[s("a"), n(1.0), s("z")]).to_css(false), "z");
    }

    #[test]
    fn join_default_uses_list1_separator() {
        let a = list(vec![num_unit(1.0, "px"), num_unit(2.0, "px")], ListSep::Space);
        let b = list(vec![num_unit(3.0, "px"), num_unit(4.0, "px")], ListSep::Space);
        assert_eq!(call("join", &[a, b]).to_css(false), "1px 2px 3px 4px");

        let c = list(vec![s("a"), s("b")], ListSep::Comma);
        let d = list(vec![s("c"), s("d")], ListSep::Comma);
        assert_eq!(call("join", &[c, d]).to_css(false), "a, b, c, d");
    }

    #[test]
    fn join_single_list_borrows_list2_separator() {
        // list1 has <2 items, so list2's comma wins.
        let a = s("a");
        let b = list(vec![s("b"), s("c")], ListSep::Comma);
        assert_eq!(call("join", &[a, b]).to_css(false), "a, b, c");
    }

    #[test]
    fn join_explicit_separator() {
        let a = list(vec![num_unit(1.0, "px"), num_unit(2.0, "px")], ListSep::Space);
        assert_eq!(
            call("join", &[a, num_unit(3.0, "px"), s("comma")]).to_css(false),
            "1px, 2px, 3px"
        );
    }

    #[test]
    fn append_default_and_explicit() {
        let a = list(vec![num_unit(1.0, "px"), num_unit(2.0, "px")], ListSep::Space);
        assert_eq!(
            call("append", &[a.clone(), num_unit(3.0, "px")]).to_css(false),
            "1px 2px 3px"
        );
        let c = list(vec![s("a"), s("b")], ListSep::Comma);
        assert_eq!(call("append", &[c, s("c")]).to_css(false), "a, b, c");
        assert_eq!(
            call("append", &[a, num_unit(3.0, "px"), s("comma")]).to_css(false),
            "1px, 2px, 3px"
        );
    }

    #[test]
    fn append_to_empty_list() {
        assert_eq!(
            call("append", &[list(vec![], ListSep::Space), s("x")]).to_css(false),
            "x"
        );
    }

    #[test]
    fn index_finds_or_null() {
        let l = list(vec![s("a"), s("b"), s("c")], ListSep::Space);
        assert_eq!(call("index", &[l.clone(), s("b")]).to_css(false), "2");
        assert!(matches!(call("index", &[l, s("z")]), Value::Null));
        // Number equality is by value+unit.
        let nums = list(
            vec![num_unit(1.0, "px"), num_unit(2.0, ""), num_unit(3.0, "px")],
            ListSep::Space,
        );
        assert!(matches!(call("index", &[nums, num_unit(2.0, "px")]), Value::Null));
    }

    #[test]
    fn list_separator_reports_keyword() {
        assert_eq!(
            call("list-separator", &[list(vec![s("a"), s("b")], ListSep::Comma)]).to_css(false),
            "comma"
        );
        assert_eq!(
            call("list-separator", &[list(vec![s("a"), s("b")], ListSep::Space)]).to_css(false),
            "space"
        );
        // <2 elements -> space.
        assert_eq!(call("list-separator", &[s("a")]).to_css(false), "space");
        assert_eq!(
            call("list-separator", &[list(vec![], ListSep::Comma)]).to_css(false),
            "space"
        );
    }

    #[test]
    fn rejects_unknown_names_and_bad_separator() {
        assert!(try_call("frobnicate", &[s("x")], &[], pos()).is_none());
        let err = call_err("join", &[s("a"), s("b"), s("dash")]);
        assert_eq!(
            err.message,
            "$separator: Must be \"space\", \"comma\", \"slash\", or \"auto\"."
        );
    }
}
