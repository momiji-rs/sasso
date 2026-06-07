//! The `sass:selector` built-in functions, global spellings.
//!
//! `selector-nest`, `selector-append`, `selector-extend`, `selector-replace`,
//! `selector-unify`, `is-superselector`, `simple-selectors`, and
//! `selector-parse`. Each accepts and returns *selector values*: a selector
//! list is a comma-separated Sass list of complex selectors, and each complex
//! selector is a space-separated Sass list whose items are compound-selector
//! strings interleaved with combinator strings (`>`/`+`/`~`), all unquoted
//! (dart-sass `ComplexSelector.asSassList`).
//!
//! The heavy lifting (parsing, superselector tests, compound/complex
//! unification, parent weaving, and the extend engine) lives in
//! [`crate::selector`]; this module only marshals Sass values to/from selector
//! strings, resolves `&` parent references for `nest`/`append`, validates
//! arguments, and serializes results.

use crate::error::Error;
use crate::scanner::Pos;
use crate::selector::{self, Complex};
// `Compound` is referenced via the fully-qualified `crate::selector::Compound`
// path in `compound_target`'s signature.
use crate::value::{List, ListSep, SassStr, Value};

pub(super) fn try_call(
    name: &str,
    pos_args: &[Value],
    named: &[(String, Value)],
    pos: Pos,
) -> Option<Result<Value, Error>> {
    Some(match name {
        "selector-nest" => fn_nest(pos_args, named, pos),
        "selector-append" => fn_append(pos_args, named, pos),
        "selector-extend" => fn_extend(pos_args, named, pos),
        "selector-replace" => fn_replace(pos_args, named, pos),
        "selector-unify" => fn_unify(pos_args, named, pos),
        "is-superselector" => fn_is_superselector(pos_args, named, pos),
        "simple-selectors" => fn_simple_selectors(pos_args, named, pos),
        "selector-parse" => fn_parse(pos_args, named, pos),
        _ => return None,
    })
}

// ---- value <-> selector-string marshalling ----------------------------

/// Convert a Sass value into its selector-string form (dart-sass
/// `_selectorString`). Strings contribute their text; a comma list joins its
/// elements with `, ` and a space list with ` `; anything else is an error.
fn value_to_selector_string(v: &Value, pos: Pos) -> Result<String, Error> {
    fn render(v: &Value) -> Option<String> {
        match v {
            Value::Str(s) => Some(s.text.clone()),
            Value::List(l) => {
                if l.items.is_empty() {
                    return None;
                }
                let sep = match l.sep {
                    ListSep::Comma => ", ",
                    ListSep::Space => " ",
                };
                let mut parts = Vec::with_capacity(l.items.len());
                for item in &l.items {
                    parts.push(render(item)?);
                }
                Some(parts.join(sep))
            }
            _ => None,
        }
    }
    render(v).ok_or_else(|| {
        Error::at(
            format!(
                "{} is not a valid selector: it must be a string,\na list of strings, or a list of lists of strings.",
                v.to_css(true)
            ),
            pos,
        )
    })
}

/// Read the selector argument at `i` as a parsed selector list, erroring like
/// dart-sass on a non-selector value, empty selector, or parse failure.
fn selector_list_arg(
    params: &[&str],
    pos_args: &[Value],
    named: &[(String, Value)],
    i: usize,
    fname: &str,
    pos: Pos,
) -> Result<Vec<Complex>, Error> {
    let v = super::require(params, pos_args, named, i, fname, pos)?;
    let pname = params.get(i).copied().unwrap_or("selector");
    let text = value_to_selector_string(v, pos)?;
    parse_selector_text(&text, pname, pos)
}

/// Parse a selector string into a list of complex selectors, erroring with
/// dart-sass's `${pname}: expected selector.` on empty/unparseable input.
fn parse_selector_text(text: &str, pname: &str, pos: Pos) -> Result<Vec<Complex>, Error> {
    if text.trim().is_empty() {
        return Err(Error::at(format!("${pname}: expected selector."), pos));
    }
    selector::parse_list(text).ok_or_else(|| Error::at(format!("${pname}: expected selector."), pos))
}

// ---- serialization back to a selector value ---------------------------

/// Serialize a list of complex selectors into a selector *value*: a comma list
/// of space lists (each a complex selector's compound/combinator parts), all
/// unquoted strings. A single complex selector still produces the comma list
/// wrapper (length 1), matching dart-sass.
fn selectors_to_value(complexes: &[Complex]) -> Value {
    let items: Vec<Value> = complexes.iter().map(complex_to_value).collect();
    Value::List(List {
        items,
        sep: ListSep::Comma,
        bracketed: false,
    })
}

/// Serialize one complex selector as a space list of its parts (compounds and
/// combinators) as unquoted strings.
fn complex_to_value(c: &Complex) -> Value {
    let parts = selector::complex_to_list_parts(c);
    let items: Vec<Value> = parts
        .into_iter()
        .map(|text| Value::Str(SassStr { text, quoted: false }))
        .collect();
    // A single-component complex selector is still a (one-element) space list.
    Value::List(List {
        items,
        sep: ListSep::Space,
        bracketed: false,
    })
}

// ---- `&` parent resolution (for nest / append) ------------------------

/// Render a parsed selector list back to its `, `-joined string form.
fn render_list(complexes: &[Complex]) -> String {
    complexes
        .iter()
        .map(Complex::render)
        .collect::<Vec<_>>()
        .join(", ")
}

/// Resolve the child selector string `child` against the parent selector list
/// `parents`, returning the resolved selector list (dart-sass
/// `SelectorList.resolveParentSelectors` over each parent). A child complex that
/// contains a top-level `&` substitutes the parent in place (parent-major
/// order); one without `&` is nested as a descendant of each parent.
fn resolve_parents(child: &str, parents: &[Complex], fname: &str, pos: Pos) -> Result<Vec<Complex>, Error> {
    let parent_str = render_list(parents);
    let child_complexes = split_top_commas(child);
    let mut out: Vec<Complex> = Vec::new();
    for parent in parents {
        let parent_one = parent.render();
        for cc in &child_complexes {
            let resolved = if has_top_parent(cc) {
                // Substitute this single parent complex for each top-level `&`.
                substitute_parent(cc, &parent_one)
            } else {
                // Descendant nesting: parent then child.
                format!("{parent_one} {}", cc.trim())
            };
            let parsed = selector::parse_list(&resolved).ok_or_else(|| {
                Error::at(format!("Invalid selector produced by {fname}(): {resolved}"), pos)
            })?;
            out.extend(parsed);
        }
    }
    if out.is_empty() {
        // No `&` and no parents shouldn't happen (parents is non-empty), but
        // guard against an empty result.
        return parse_selector_text(&parent_str, "selectors", pos);
    }
    Ok(out)
}

/// Whether a complex-selector string has a top-level `&` (outside brackets,
/// parens, and quotes), i.e. a parent reference this resolver substitutes.
fn has_top_parent(s: &str) -> bool {
    let mut paren = 0i32;
    let mut bracket = 0i32;
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                chars.next();
            }
            '"' | '\'' => {
                let q = c;
                for n in chars.by_ref() {
                    if n == '\\' {
                        continue;
                    }
                    if n == q {
                        break;
                    }
                }
            }
            '(' => paren += 1,
            ')' => paren -= 1,
            '[' => bracket += 1,
            ']' => bracket -= 1,
            '&' if paren == 0 && bracket == 0 => return true,
            _ => {}
        }
    }
    false
}

/// Replace every unescaped, unquoted top-level `&` in `s` with `parent`. The
/// text directly following a `&` joins onto the parent's last compound by string
/// adjacency (e.g. `&.y` with parent `.a .b` becomes `.a .b.y`).
fn substitute_parent(s: &str, parent: &str) -> String {
    let mut out = String::with_capacity(s.len() + parent.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                out.push(c);
                if let Some(n) = chars.next() {
                    out.push(n);
                }
            }
            '"' | '\'' => {
                out.push(c);
                let q = c;
                for n in chars.by_ref() {
                    out.push(n);
                    if n == q {
                        break;
                    }
                }
            }
            '&' => out.push_str(parent),
            _ => out.push(c),
        }
    }
    out
}

/// Split a selector-list string on top-level commas (depth-aware), trimming.
fn split_top_commas(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut paren = 0i32;
    let mut bracket = 0i32;
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                cur.push(c);
                if let Some(n) = chars.next() {
                    cur.push(n);
                }
            }
            '"' | '\'' => {
                cur.push(c);
                let q = c;
                for n in chars.by_ref() {
                    cur.push(n);
                    if n == q {
                        break;
                    }
                }
            }
            '(' => {
                paren += 1;
                cur.push(c);
            }
            ')' => {
                paren -= 1;
                cur.push(c);
            }
            '[' => {
                bracket += 1;
                cur.push(c);
            }
            ']' => {
                bracket -= 1;
                cur.push(c);
            }
            ',' if paren == 0 && bracket == 0 => out.push(std::mem::take(&mut cur)),
            _ => cur.push(c),
        }
    }
    out.push(cur);
    out.into_iter().filter(|p| !p.trim().is_empty()).collect()
}

// ---- nest -------------------------------------------------------------

/// `selector-nest($selectors...)`: nest each selector within the previous,
/// resolving `&` against the accumulated result (dart-sass `nest`).
fn fn_nest(pos_args: &[Value], named: &[(String, Value)], pos: Pos) -> Result<Value, Error> {
    let mut all: Vec<Value> = pos_args.to_vec();
    for (_, v) in named {
        all.push(v.clone());
    }
    if all.is_empty() {
        return Err(Error::at(
            "$selectors: At least one selector must be passed.".to_string(),
            pos,
        ));
    }
    // The first selector is the base (it may not contain `&`).
    let mut acc = parse_arg_selector(&all[0], pos)?;
    for v in &all[1..] {
        let child = value_to_selector_string(v, pos)?;
        acc = resolve_parents(&child, &acc, "selector-nest", pos)?;
    }
    Ok(selectors_to_value(&acc))
}

/// Parse a single selector value argument into a complex-selector list.
fn parse_arg_selector(v: &Value, pos: Pos) -> Result<Vec<Complex>, Error> {
    let text = value_to_selector_string(v, pos)?;
    parse_selector_text(&text, "selectors", pos)
}

// ---- append -----------------------------------------------------------

/// `selector-append($selectors...)`: append each selector to the previous with
/// no descendant combinator — the leading compound of each subsequent complex
/// merges onto the trailing compound of the accumulator (dart-sass `append`).
fn fn_append(pos_args: &[Value], named: &[(String, Value)], pos: Pos) -> Result<Value, Error> {
    let mut all: Vec<Value> = pos_args.to_vec();
    for (_, v) in named {
        all.push(v.clone());
    }
    if all.is_empty() {
        return Err(Error::at(
            "$selectors: At least one selector must be passed.".to_string(),
            pos,
        ));
    }
    let mut acc = parse_arg_selector(&all[0], pos)?;
    for v in &all[1..] {
        let suffix = parse_arg_selector(v, pos)?;
        acc = append_lists(&acc, &suffix, pos)?;
    }
    Ok(selectors_to_value(&acc))
}

/// Append `suffix` to `prefix`: for each prefix complex × suffix complex, the
/// suffix's leading compound (which must carry no combinator) is concatenated
/// onto the prefix's trailing compound; the rest of the suffix follows
/// (dart-sass `append`/`_prependParent`).
fn append_lists(prefix: &[Complex], suffix: &[Complex], pos: Pos) -> Result<Vec<Complex>, Error> {
    let mut out: Vec<Complex> = Vec::new();
    for p in prefix {
        let p_one = p.render();
        for s in suffix {
            // The first component of the suffix must have no combinator.
            let s_str = s.render();
            if let Some(first) = s.components.first() {
                if first.combinator.is_some() {
                    return Err(Error::at(format!("Can't append {s_str} to {p_one}."), pos));
                }
            }
            // Concatenate the prefix and the suffix's leading compound by string
            // adjacency, so `.a` + `.b.c` → `.a.b.c` and `ul` + `li` → `ulli`.
            let combined = format!("{p_one}{s_str}");
            let parsed = selector::parse_list(&combined)
                .ok_or_else(|| Error::at(format!("Can't append {s_str} to {p_one}."), pos))?;
            out.extend(parsed);
        }
    }
    Ok(out)
}

// ---- extend / replace -------------------------------------------------

/// `selector-extend($selector, $extendee, $extender)`: extend `$selector` so
/// that wherever `$extendee` matches, `$extender` is added too (keeping the
/// original). Mirrors `@extend` (normal mode).
fn fn_extend(pos_args: &[Value], named: &[(String, Value)], pos: Pos) -> Result<Value, Error> {
    extend_or_replace(pos_args, named, pos, false, "selector-extend")
}

/// `selector-replace($selector, $original, $replacement)`: like `selector-extend`
/// but the matched selector is replaced (the original is dropped).
fn fn_replace(pos_args: &[Value], named: &[(String, Value)], pos: Pos) -> Result<Value, Error> {
    extend_or_replace(pos_args, named, pos, true, "selector-replace")
}

fn extend_or_replace(
    pos_args: &[Value],
    named: &[(String, Value)],
    pos: Pos,
    replace: bool,
    fname: &str,
) -> Result<Value, Error> {
    let params: &[&str] = if replace {
        &["selector", "original", "replacement"]
    } else {
        &["selector", "extendee", "extender"]
    };
    let selector = selector_list_arg(params, pos_args, named, 0, fname, pos)?;
    let target_list = selector_list_arg(params, pos_args, named, 1, fname, pos)?;
    let extender = selector_list_arg(params, pos_args, named, 2, fname, pos)?;

    // The extendee/original must be a single compound selector (dart-sass rejects
    // a complex selector here).
    let target = compound_target(&target_list, pos)?;

    let complexes = selector::extend_compound_target(&selector, &target, &extender, replace);
    if complexes.is_empty() {
        return Err(Error::at(format!("{fname}() produced an empty selector."), pos));
    }
    Ok(selectors_to_value(&complexes))
}

/// Extract the single compound target of an extendee/original selector list. It
/// must be a single complex selector with a single compound (dart-sass rejects a
/// complex selector with `Can't extend complex selector <rendered>.`).
fn compound_target(list: &[Complex], pos: Pos) -> Result<crate::selector::Compound, Error> {
    if list.len() == 1 && list[0].components.len() == 1 && list[0].components[0].combinator.is_none() {
        return Ok(list[0].components[0].compound.clone());
    }
    let rendered = list.iter().map(Complex::render).collect::<Vec<_>>().join(", ");
    Err(Error::at(
        format!("Can't extend complex selector {rendered}."),
        pos,
    ))
}

// ---- unify ------------------------------------------------------------

/// `selector-unify($selector1, $selector2)`: the selectors matching both inputs,
/// or `null` when no combination unifies (dart-sass `unify`).
fn fn_unify(pos_args: &[Value], named: &[(String, Value)], pos: Pos) -> Result<Value, Error> {
    let params = &["selector1", "selector2"];
    let s1 = selector_list_arg(params, pos_args, named, 0, "selector-unify", pos)?;
    let s2 = selector_list_arg(params, pos_args, named, 1, "selector-unify", pos)?;
    match selector::unify_lists(&s1, &s2) {
        Some(unified) => Ok(selectors_to_value(&unified)),
        None => Ok(Value::Null),
    }
}

// ---- is-superselector -------------------------------------------------

/// `is-superselector($super, $sub)`: whether `$super` matches every element
/// `$sub` matches (dart-sass `is-superselector`).
fn fn_is_superselector(pos_args: &[Value], named: &[(String, Value)], pos: Pos) -> Result<Value, Error> {
    let params = &["super", "sub"];
    let sup = selector_list_arg(params, pos_args, named, 0, "is-superselector", pos)?;
    let sub = selector_list_arg(params, pos_args, named, 1, "is-superselector", pos)?;
    Ok(Value::Bool(selector::list_is_superselector(&sup, &sub)))
}

// ---- simple-selectors -------------------------------------------------

/// `simple-selectors($selector)`: the simple selectors of a single compound
/// selector, as a comma list of unquoted strings (dart-sass `simple-selectors`).
fn fn_simple_selectors(pos_args: &[Value], named: &[(String, Value)], pos: Pos) -> Result<Value, Error> {
    let params = &["selector"];
    let v = super::require(params, pos_args, named, 0, "simple-selectors", pos)?;
    let text = value_to_selector_string(v, pos)?;
    if text.trim().is_empty() {
        return Err(Error::at("$selector: expected selector.".to_string(), pos));
    }
    let simples = selector::parse_compound_simples(&text)
        .ok_or_else(|| Error::at("$selector: expected selector.".to_string(), pos))?;
    let items: Vec<Value> = simples
        .into_iter()
        .map(|text| Value::Str(SassStr { text, quoted: false }))
        .collect();
    Ok(Value::List(List {
        items,
        sep: ListSep::Comma,
        bracketed: false,
    }))
}

// ---- parse ------------------------------------------------------------

/// `selector-parse($selector)`: parse a selector string/value into a selector
/// value (dart-sass `parse`).
fn fn_parse(pos_args: &[Value], named: &[(String, Value)], pos: Pos) -> Result<Value, Error> {
    let params = &["selector"];
    let list = selector_list_arg(params, pos_args, named, 0, "selector-parse", pos)?;
    Ok(selectors_to_value(&list))
}
