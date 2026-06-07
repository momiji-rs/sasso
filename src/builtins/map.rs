//! Map built-in functions (`map-get`, `map-keys`, `map-values`,
//! `map-has-key`, `map-merge`, `map-remove`) plus the map-aware overloads of
//! `length`/`nth`.
//!
//! Sass maps are ordered key/value collections; keys compare by Sass `==`.
//! This family is registered ahead of the list family so that `length`/`nth`
//! on a map are handled here, while the same names on a list fall through to
//! the list family (this `try_call` returns `None` for them unless the first
//! argument is actually a map). Shared argument helpers live in the parent
//! module: `super::{arg, require, num}`.

use crate::error::Error;
use crate::scanner::Pos;
use crate::value::{List, ListSep, Map, Number, Value};

pub(super) fn try_call(
    name: &str,
    pos_args: &[Value],
    named: &[(String, Value)],
    pos: Pos,
) -> Option<Result<Value, Error>> {
    Some(match name {
        "map-get" => fn_map_get(pos_args, named, pos),
        "map-keys" => fn_map_keys(pos_args, named, pos),
        "map-values" => fn_map_values(pos_args, named, pos),
        "map-has-key" => fn_map_has_key(pos_args, named, pos),
        "map-merge" => fn_map_merge(pos_args, named, pos),
        "map-remove" => fn_map_remove(pos_args, named, pos),
        // `length`/`nth` are owned by the list family; this family only claims
        // them when the first argument is a map (so list calls fall through).
        "length" if first_is_map(pos_args, named) => fn_length(pos_args, named, pos),
        "nth" if first_is_map(pos_args, named) => fn_nth(pos_args, named, pos),
        _ => return None,
    })
}

/// Whether the first argument (positional or by the conventional first
/// parameter name) is a map. Used to decide whether `length`/`nth` belong to
/// this family or fall through to the list family.
fn first_is_map(pos_args: &[Value], named: &[(String, Value)]) -> bool {
    if let Some(v) = pos_args.first() {
        return matches!(v, Value::Map(_));
    }
    named
        .iter()
        .any(|(n, v)| (n == "list" || n == "map") && matches!(v, Value::Map(_)))
}

/// Coerce a value into map entries. A map yields its entries; the empty list
/// `()` is the empty map; any other value is not a map (an error).
fn as_map(v: &Value, fname: &str, pos: Pos) -> Result<Vec<(Value, Value)>, Error> {
    match v {
        Value::Map(m) => Ok(m.entries.clone()),
        // The empty list doubles as the empty map.
        Value::List(l) if l.items.is_empty() => Ok(Vec::new()),
        other => Err(Error::at(
            format!("$map: {} is not a map for `{fname}`.", other.to_css(false)),
            pos,
        )),
    }
}

fn unitless(value: f64) -> Value {
    Value::Number(Number {
        value,
        unit: String::new(),
    })
}

/// A comma list of the given items, or the empty (space) list when empty —
/// matching dart-sass `map-keys`/`map-values` serialization.
fn comma_list(items: Vec<Value>) -> Value {
    let sep = if items.is_empty() {
        ListSep::Space
    } else {
        ListSep::Comma
    };
    Value::List(List {
        items,
        sep,
        bracketed: false,
    })
}

/// `map-get($map, $key)`: the value for `$key`, or `null` when absent.
fn fn_map_get(pos_args: &[Value], named: &[(String, Value)], pos: Pos) -> Result<Value, Error> {
    let params = ["map", "key"];
    let map_v = super::require(&params, pos_args, named, 0, "map-get", pos)?;
    let entries = as_map(map_v, "map-get", pos)?;
    let key = super::require(&params, pos_args, named, 1, "map-get", pos)?;
    Ok(entries
        .iter()
        .find(|(k, _)| k.sass_eq(key))
        .map(|(_, v)| v.clone())
        .unwrap_or(Value::Null))
}

/// `map-keys($map)`: a comma list of the map's keys in order.
fn fn_map_keys(pos_args: &[Value], named: &[(String, Value)], pos: Pos) -> Result<Value, Error> {
    let map_v = super::require(&["map"], pos_args, named, 0, "map-keys", pos)?;
    let entries = as_map(map_v, "map-keys", pos)?;
    Ok(comma_list(entries.into_iter().map(|(k, _)| k).collect()))
}

/// `map-values($map)`: a comma list of the map's values in order.
fn fn_map_values(pos_args: &[Value], named: &[(String, Value)], pos: Pos) -> Result<Value, Error> {
    let map_v = super::require(&["map"], pos_args, named, 0, "map-values", pos)?;
    let entries = as_map(map_v, "map-values", pos)?;
    Ok(comma_list(entries.into_iter().map(|(_, v)| v).collect()))
}

/// `map-has-key($map, $key)`: whether the map contains `$key`.
fn fn_map_has_key(pos_args: &[Value], named: &[(String, Value)], pos: Pos) -> Result<Value, Error> {
    let params = ["map", "key"];
    let map_v = super::require(&params, pos_args, named, 0, "map-has-key", pos)?;
    let entries = as_map(map_v, "map-has-key", pos)?;
    let key = super::require(&params, pos_args, named, 1, "map-has-key", pos)?;
    Ok(Value::Bool(entries.iter().any(|(k, _)| k.sass_eq(key))))
}

/// `map-merge($map1, $map2)`: `$map1` with `$map2`'s entries added/overwriting,
/// keeping `$map1`'s ordering for shared keys.
fn fn_map_merge(pos_args: &[Value], named: &[(String, Value)], pos: Pos) -> Result<Value, Error> {
    let params = ["map1", "map2"];
    let map1_v = super::require(&params, pos_args, named, 0, "map-merge", pos)?;
    let map2_v = super::require(&params, pos_args, named, 1, "map-merge", pos)?;
    let mut map = Map {
        entries: as_map(map1_v, "map-merge", pos)?,
    };
    for (k, v) in as_map(map2_v, "map-merge", pos)? {
        map.insert(k, v);
    }
    Ok(Value::Map(map))
}

/// `map-remove($map, $keys...)`: the map without any entries whose key matches
/// one of `$keys`.
fn fn_map_remove(pos_args: &[Value], named: &[(String, Value)], pos: Pos) -> Result<Value, Error> {
    let map_v = super::require(&["map"], pos_args, named, 0, "map-remove", pos)?;
    let mut entries = as_map(map_v, "map-remove", pos)?;
    // Every argument after the map is a key to remove (the `$keys...` rest).
    let keys: Vec<Value> = pos_args.iter().skip(1).cloned().collect();
    entries.retain(|(k, _)| !keys.iter().any(|rk| rk.sass_eq(k)));
    Ok(Value::Map(Map { entries }))
}

/// `length($map)`: the number of entries.
fn fn_length(pos_args: &[Value], named: &[(String, Value)], pos: Pos) -> Result<Value, Error> {
    let map_v = super::require(&["map"], pos_args, named, 0, "length", pos)?;
    let entries = as_map(map_v, "length", pos)?;
    Ok(unitless(entries.len() as f64))
}

/// `nth($map, $n)`: the 1-based (negative-from-end) entry as a `key value`
/// space list.
fn fn_nth(pos_args: &[Value], named: &[(String, Value)], pos: Pos) -> Result<Value, Error> {
    let params = ["list", "n"];
    let map_v = super::require(&params, pos_args, named, 0, "nth", pos)?;
    let entries = as_map(map_v, "nth", pos)?;
    let n = super::require(&params, pos_args, named, 1, "nth", pos)?;
    let raw = super::num(n, pos)?;
    if raw.fract() != 0.0 {
        return Err(Error::at(
            format!("$n: {} is not an int.", crate::value::fmt_num(raw, false)),
            pos,
        ));
    }
    let len = entries.len() as i64;
    let index = raw as i64;
    if index == 0 {
        return Err(Error::at("$n: List index may not be 0.", pos));
    }
    if index.abs() > len {
        return Err(Error::at(
            format!(
                "$n: Invalid index {} for a list with {} element{}.",
                crate::value::fmt_num(raw, false),
                len,
                if len == 1 { "" } else { "s" }
            ),
            pos,
        ));
    }
    let zero_based = if index > 0 { index - 1 } else { len + index } as usize;
    let (k, v) = entries[zero_based].clone();
    Ok(Value::List(List {
        items: vec![k, v],
        sep: ListSep::Space,
        bracketed: false,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value::SassStr;

    fn pos() -> Pos {
        Pos { line: 1, col: 1 }
    }

    fn s(text: &str) -> Value {
        Value::Str(SassStr {
            text: text.to_string(),
            quoted: false,
        })
    }

    fn n(value: f64) -> Value {
        unitless(value)
    }

    fn map(pairs: &[(&str, Value)]) -> Value {
        Value::Map(Map {
            entries: pairs.iter().map(|(k, v)| (s(k), v.clone())).collect(),
        })
    }

    fn call(name: &str, args: &[Value]) -> Value {
        try_call(name, args, &[], pos())
            .expect("name owned by map family")
            .expect("no error")
    }

    #[test]
    fn map_get_returns_value_or_null() {
        let m = map(&[("c", s("d"))]);
        assert_eq!(call("map-get", &[m.clone(), s("c")]).to_css(false), "d");
        assert!(matches!(call("map-get", &[m, s("z")]), Value::Null));
    }

    #[test]
    fn map_keys_and_values_are_comma_lists() {
        let m = map(&[("c", n(1.0)), ("d", n(2.0))]);
        assert_eq!(call("map-keys", std::slice::from_ref(&m)).to_css(false), "c, d");
        assert_eq!(call("map-values", &[m]).to_css(false), "1, 2");
    }

    #[test]
    fn map_has_key_reports_presence() {
        let m = map(&[("c", s("d"))]);
        assert_eq!(call("map-has-key", &[m.clone(), s("c")]).to_css(false), "true");
        assert_eq!(call("map-has-key", &[m, s("z")]).to_css(false), "false");
    }

    #[test]
    fn map_merge_overwrites_and_appends() {
        let a = map(&[("c", s("d"))]);
        let b = map(&[("e", s("f"))]);
        assert_eq!(call("map-merge", &[a, b]).to_css(false), "(c: d, e: f)");
        let a = map(&[("c", n(1.0))]);
        let b = map(&[("c", n(2.0))]);
        assert_eq!(call("map-merge", &[a, b]).to_css(false), "(c: 2)");
    }

    #[test]
    fn map_remove_drops_keys() {
        let m = map(&[("c", s("d")), ("x", s("y"))]);
        assert_eq!(call("map-remove", &[m, s("c")]).to_css(false), "(x: y)");
    }

    #[test]
    fn length_and_nth_on_maps() {
        let m = map(&[("c", n(1.0)), ("d", n(2.0))]);
        assert_eq!(call("length", std::slice::from_ref(&m)).to_css(false), "2");
        assert_eq!(call("nth", &[m, n(1.0)]).to_css(false), "c 1");
    }

    #[test]
    fn length_and_nth_decline_non_maps() {
        // A non-map first argument leaves these for the list family.
        assert!(try_call("length", &[s("x")], &[], pos()).is_none());
        assert!(try_call("nth", &[s("x"), n(1.0)], &[], pos()).is_none());
    }

    #[test]
    fn empty_list_acts_as_empty_map() {
        let empty = Value::List(List {
            items: Vec::new(),
            sep: ListSep::Space,
            bracketed: false,
        });
        assert_eq!(call("map-keys", std::slice::from_ref(&empty)).to_css(false), "");
        assert!(matches!(call("map-get", &[empty, s("a")]), Value::Null));
    }
}
