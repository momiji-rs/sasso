//! A structured CSS selector model plus the `@extend` engine.
//!
//! Selectors are parsed from their already-resolved string form (after `&`
//! and interpolation have been substituted by the evaluator) into a small
//! tree — [`ComplexSelector`] → [`ComplexComponent`] → [`CompoundSelector`]
//! → [`SimpleSelector`] — that mirrors dart-sass's model closely enough to
//! port its `@extend` algorithm (extension, unification, transitive chains,
//! and placeholder-rule dropping).

use std::collections::HashSet;

/// A combinator joining two compound selectors.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Combinator {
    Child,            // >
    NextSibling,      // +
    FollowingSibling, // ~
}

impl Combinator {
    fn as_str(self) -> &'static str {
        match self {
            Combinator::Child => ">",
            Combinator::NextSibling => "+",
            Combinator::FollowingSibling => "~",
        }
    }
}

/// One simple selector: the atoms of a compound selector.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) enum Simple {
    /// `*` or `ns|*`. Stores the optional namespace prefix verbatim.
    Universal { ns: Option<String> },
    /// A type/element selector, e.g. `div`, `svg|rect`. Stored verbatim.
    Type(String),
    /// `.foo`
    Class(String),
    /// `#foo`
    Id(String),
    /// `%foo`
    Placeholder(String),
    /// `[attr=val i]` etc. — the whole bracketed text including `[` `]`.
    Attribute(String),
    /// A pseudo class/element. Stored verbatim text including leading colon(s),
    /// e.g. `:hover`, `::before`, `:not(.x)`.
    Pseudo(String),
}

impl Simple {
    fn render(&self) -> String {
        match self {
            Simple::Universal { ns: None } => "*".to_string(),
            Simple::Universal { ns: Some(n) } => format!("{n}|*"),
            Simple::Type(s) => s.clone(),
            Simple::Class(s) => format!(".{s}"),
            Simple::Id(s) => format!("#{s}"),
            Simple::Placeholder(s) => format!("%{s}"),
            Simple::Attribute(s) => s.clone(),
            Simple::Pseudo(s) => s.clone(),
        }
    }

    fn is_placeholder(&self) -> bool {
        matches!(self, Simple::Placeholder(_))
    }
}

/// A compound selector: a non-empty run of simple selectors with no
/// combinator between them, e.g. `.foo.bar:hover`.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) struct Compound {
    pub simples: Vec<Simple>,
}

impl Compound {
    fn render(&self) -> String {
        self.simples.iter().map(Simple::render).collect()
    }

    fn has_placeholder(&self) -> bool {
        self.simples.iter().any(Simple::is_placeholder)
    }
}

/// One component of a complex selector: a compound preceded by an optional
/// combinator that joins it to the previous component.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) struct ComplexComponent {
    pub combinator: Option<Combinator>,
    pub compound: Compound,
}

/// A complex selector: a sequence of compound selectors joined by descendant
/// (whitespace) or explicit combinators, e.g. `.a > .b .c`.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) struct Complex {
    pub components: Vec<ComplexComponent>,
}

impl Complex {
    fn render(&self) -> String {
        let mut out = String::new();
        for (i, comp) in self.components.iter().enumerate() {
            if i > 0 {
                out.push(' ');
            }
            if let Some(c) = comp.combinator {
                out.push_str(c.as_str());
                out.push(' ');
            }
            out.push_str(&comp.compound.render());
        }
        out
    }

    fn has_placeholder(&self) -> bool {
        self.components.iter().any(|c| c.compound.has_placeholder())
    }
}

// ---- parsing -----------------------------------------------------------

/// Parse a selector-list string (already `&`/interpolation-resolved) into
/// complex selectors. Returns `None` if anything is unparseable (the caller
/// then falls back to leaving the selector untouched).
pub(crate) fn parse_list(sel: &str) -> Option<Vec<Complex>> {
    let mut out = Vec::new();
    for part in split_top(sel, ',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        out.push(parse_complex(part)?);
    }
    if out.is_empty() {
        return None;
    }
    Some(out)
}

/// Parse a single complex selector.
fn parse_complex(s: &str) -> Option<Complex> {
    let chars: Vec<char> = s.chars().collect();
    let mut components = Vec::new();
    let mut i = 0;
    let mut pending_combinator: Option<Combinator> = None;
    loop {
        skip_ws(&chars, &mut i);
        if i >= chars.len() {
            break;
        }
        // A combinator (only valid between compounds).
        match chars[i] {
            '>' => {
                pending_combinator = Some(Combinator::Child);
                i += 1;
                continue;
            }
            '+' => {
                pending_combinator = Some(Combinator::NextSibling);
                i += 1;
                continue;
            }
            '~' => {
                pending_combinator = Some(Combinator::FollowingSibling);
                i += 1;
                continue;
            }
            _ => {}
        }
        let compound = parse_compound(&chars, &mut i)?;
        components.push(ComplexComponent {
            combinator: pending_combinator.take(),
            compound,
        });
    }
    if components.is_empty() || pending_combinator.is_some() {
        return None;
    }
    Some(Complex { components })
}

/// Parse one compound selector starting at `*i`, advancing `*i` past it.
fn parse_compound(chars: &[char], i: &mut usize) -> Option<Compound> {
    let mut simples = Vec::new();
    while *i < chars.len() {
        let c = chars[*i];
        match c {
            ' ' | '\t' | '\n' | '\r' | '>' | '+' | '~' | ',' => break,
            '.' => {
                *i += 1;
                let name = read_ident(chars, i)?;
                simples.push(Simple::Class(name));
            }
            '#' => {
                *i += 1;
                let name = read_ident(chars, i)?;
                simples.push(Simple::Id(name));
            }
            '%' => {
                *i += 1;
                let name = read_ident(chars, i)?;
                simples.push(Simple::Placeholder(name));
            }
            '[' => {
                let text = read_bracketed(chars, i)?;
                simples.push(Simple::Attribute(text));
            }
            ':' => {
                let text = read_pseudo(chars, i)?;
                simples.push(Simple::Pseudo(text));
            }
            '*' => {
                *i += 1;
                // `*|...` uses `*` as a namespace prefix.
                if *i < chars.len() && chars[*i] == '|' && chars.get(*i + 1) != Some(&'=') {
                    *i += 1;
                    match read_type_after_ns(chars, i)? {
                        // `*|*`
                        Simple::Universal { .. } => simples.push(Simple::Universal {
                            ns: Some("*".to_string()),
                        }),
                        // `*|type`
                        Simple::Type(t) => simples.push(Simple::Type(format!("*|{t}"))),
                        other => simples.push(other),
                    }
                } else {
                    simples.push(Simple::Universal { ns: None });
                }
            }
            _ if is_ident_start(c) || c == '\\' || c == '|' => {
                let (name, is_ns) = read_type_or_ns(chars, i)?;
                if is_ns {
                    let after = read_type_after_ns(chars, i)?;
                    // prepend namespace to the rendered text
                    match after {
                        Simple::Universal { .. } => simples.push(Simple::Universal { ns: Some(name) }),
                        Simple::Type(t) => simples.push(Simple::Type(format!("{name}|{t}"))),
                        other => simples.push(other),
                    }
                } else {
                    simples.push(Simple::Type(name));
                }
            }
            _ => return None,
        }
    }
    if simples.is_empty() {
        return None;
    }
    Some(Compound { simples })
}

/// Read a type selector or a namespace prefix. Returns `(text, is_namespace)`
/// where `is_namespace` is true when the next char is `|` (not `|=`).
fn read_type_or_ns(chars: &[char], i: &mut usize) -> Option<(String, bool)> {
    let name = read_ident(chars, i)?;
    if *i < chars.len() && chars[*i] == '|' && chars.get(*i + 1) != Some(&'=') {
        *i += 1; // consume '|'
        Some((name, true))
    } else {
        Some((name, false))
    }
}

/// After a namespace `ns|`, read either `*` or a type name.
fn read_type_after_ns(chars: &[char], i: &mut usize) -> Option<Simple> {
    if *i < chars.len() && chars[*i] == '*' {
        *i += 1;
        return Some(Simple::Universal { ns: None });
    }
    let t = read_ident(chars, i)?;
    Some(Simple::Type(t))
}

fn read_ident(chars: &[char], i: &mut usize) -> Option<String> {
    let start = *i;
    let mut s = String::new();
    while *i < chars.len() {
        let c = chars[*i];
        if c == '\\' {
            s.push(c);
            *i += 1;
            if *i < chars.len() {
                s.push(chars[*i]);
                *i += 1;
            }
            continue;
        }
        if is_ident_char(c) {
            s.push(c);
            *i += 1;
        } else {
            break;
        }
    }
    if *i == start {
        return None;
    }
    Some(s)
}

/// Read a `[...]` attribute selector verbatim, returning the full text.
fn read_bracketed(chars: &[char], i: &mut usize) -> Option<String> {
    let start = *i;
    *i += 1; // '['
    let mut depth = 1;
    while *i < chars.len() {
        match chars[*i] {
            '\\' => {
                *i += 2;
                continue;
            }
            '"' | '\'' => {
                let q = chars[*i];
                *i += 1;
                while *i < chars.len() && chars[*i] != q {
                    if chars[*i] == '\\' {
                        *i += 1;
                    }
                    *i += 1;
                }
                *i += 1;
                continue;
            }
            '[' => depth += 1,
            ']' => {
                depth -= 1;
                if depth == 0 {
                    *i += 1;
                    return Some(chars[start..*i].iter().collect());
                }
            }
            _ => {}
        }
        *i += 1;
    }
    None
}

/// Read a pseudo-class/element selector verbatim (with any `(...)` argument).
fn read_pseudo(chars: &[char], i: &mut usize) -> Option<String> {
    let start = *i;
    *i += 1; // first ':'
    if *i < chars.len() && chars[*i] == ':' {
        *i += 1; // '::'
    }
    // name
    read_ident(chars, i)?;
    // optional argument
    if *i < chars.len() && chars[*i] == '(' {
        let mut depth = 0;
        while *i < chars.len() {
            match chars[*i] {
                '\\' => {
                    *i += 2;
                    continue;
                }
                '"' | '\'' => {
                    let q = chars[*i];
                    *i += 1;
                    while *i < chars.len() && chars[*i] != q {
                        if chars[*i] == '\\' {
                            *i += 1;
                        }
                        *i += 1;
                    }
                    *i += 1;
                    continue;
                }
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        *i += 1;
                        break;
                    }
                }
                _ => {}
            }
            *i += 1;
        }
    }
    Some(chars[start..*i].iter().collect())
}

fn skip_ws(chars: &[char], i: &mut usize) {
    while *i < chars.len() && chars[*i].is_whitespace() {
        *i += 1;
    }
}

fn is_ident_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_' || c == '-' || !c.is_ascii()
}

fn is_ident_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '-' || !c.is_ascii()
}

/// Split `s` on the top-level (paren/bracket depth 0) occurrences of `sep`.
fn split_top(s: &str, sep: char) -> Vec<String> {
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
            _ if c == sep && paren == 0 && bracket == 0 => {
                out.push(std::mem::take(&mut cur));
            }
            _ => cur.push(c),
        }
    }
    out.push(cur);
    out
}

// ---- the extend engine -------------------------------------------------

/// One registered extension: `extender` selectors should be added wherever
/// `target` (a single simple selector) is matched. `target` is `None` when the
/// extend target couldn't be parsed as a single simple selector (it then never
/// matches, but still records "not found" for the !optional check).
pub(crate) struct Extension {
    pub target: Option<Simple>,
    /// The extending selector list (the rule body containing `@extend`).
    pub extenders: Vec<Complex>,
    pub optional: bool,
    /// Whether this extension's target was ever found in the stylesheet.
    pub matched: std::cell::Cell<bool>,
}

/// Parse a single complex selector (one comma-free selector). Returns `None`
/// on any parse failure.
pub(crate) fn parse_complex_one(s: &str) -> Option<Complex> {
    parse_complex(s.trim())
}

/// How an `@extend` target string classifies.
pub(crate) enum TargetClass {
    /// A valid single-simple-selector target.
    Simple(Simple),
    /// `@extend a b` — complex selectors may not be extended.
    Complex,
    /// `@extend a.b` / `a:hover` — compound (multi-simple) selectors may no
    /// longer be extended.
    Compound,
    /// Unparseable.
    Invalid,
}

/// Classify an `@extend` target string (already interpolation-resolved).
pub(crate) fn classify_target(s: &str) -> TargetClass {
    let s = s.trim();
    if s.is_empty() {
        return TargetClass::Invalid;
    }
    let Some(complex) = parse_complex(s) else {
        return TargetClass::Invalid;
    };
    if complex.components.len() != 1 || complex.components[0].combinator.is_some() {
        return TargetClass::Complex;
    }
    let simples = &complex.components[0].compound.simples;
    if simples.len() != 1 {
        return TargetClass::Compound;
    }
    TargetClass::Simple(simples[0].clone())
}

/// The result of running the extend engine on a selector list.
pub(crate) struct ExtendResult {
    /// The rewritten, comma-separated selector strings.
    pub selectors: Vec<String>,
    /// True if every component still contains a placeholder (rule should drop).
    pub all_placeholders: bool,
}

/// Apply `extensions` to the parsed selector list `original`, returning the
/// extended selector list (original selectors first, then generated ones, in
/// dart-sass order). Placeholder-only complex selectors are dropped from the
/// output.
pub(crate) fn extend_selectors(original: &[Complex], extensions: &[Extension]) -> ExtendResult {
    // The set of "original" rendered selectors — the unextended input. Original
    // selectors are never trimmed (dart-sass keeps them so the rule still
    // matches what it always matched).
    let mut originals: HashSet<String> = HashSet::new();
    for complex in original {
        originals.insert(complex.render());
    }

    let mut result: Vec<Complex> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for complex in original {
        for c in extend_complex(complex, extensions) {
            let rendered = c.render();
            if seen.insert(rendered) {
                result.push(c);
            }
        }
    }

    // Remove selectors that are subselectors of another (redundant), keeping
    // originals (dart-sass `_trim`).
    let result = trim(result, &originals);

    // Drop complex selectors that still contain a placeholder.
    let kept: Vec<&Complex> = result.iter().filter(|c| !c.has_placeholder()).collect();
    let all_placeholders = kept.is_empty();
    let selectors: Vec<String> = kept.iter().map(|c| c.render()).collect();
    ExtendResult {
        selectors,
        all_placeholders,
    }
}

/// Remove complex selectors that are subselectors of another in the list,
/// preserving original selectors. Mirrors dart-sass `ExtensionStore._trim`:
/// iterate last-to-first, dropping a selector when an already-kept (or
/// later-in-input) selector is its superselector. Originals are always kept.
fn trim(selectors: Vec<Complex>, originals: &HashSet<String>) -> Vec<Complex> {
    // Quadratic; dart-sass bails above 100 to avoid pathological cost.
    if selectors.len() > 100 {
        return selectors;
    }
    let mut result: Vec<Complex> = Vec::new();
    let n = selectors.len();
    for i in (0..n).rev() {
        let c1 = &selectors[i];
        if originals.contains(&c1.render()) {
            // Keep originals, avoiding duplicate originals.
            if !result.iter().any(|c| c.render() == c1.render()) {
                result.insert(0, c1.clone());
            }
            continue;
        }
        // Drop c1 if any already-kept selector is its superselector.
        let superseded = result.iter().any(|c2| complex_is_superselector(c2, c1))
            || selectors[..i].iter().any(|c2| complex_is_superselector(c2, c1));
        if !superseded {
            result.insert(0, c1.clone());
        }
    }
    result
}

// ---- superselector checks ---------------------------------------------

/// Whether `c1` is a superselector of `c2` (matches every element `c2` does).
fn complex_is_superselector(c1: &Complex, c2: &Complex) -> bool {
    let comps1 = &c1.components;
    let comps2 = &c2.components;
    // Trailing combinators make a selector neither super- nor subselector; our
    // model never has trailing combinators, so skip that check.
    let mut i1 = 0usize;
    let mut i2 = 0usize;
    let mut previous_combinator: Option<Combinator> = None;
    loop {
        let remaining1 = comps1.len() - i1;
        let remaining2 = comps2.len() - i2;
        if remaining1 == 0 || remaining2 == 0 {
            return false;
        }
        if remaining1 > remaining2 {
            return false;
        }
        let component1 = &comps1[i1];
        if remaining1 == 1 {
            let Some(last2) = comps2.last() else {
                return false;
            };
            return compound_is_superselector(&component1.compound, &last2.compound);
        }

        // Find the first index in comps2 whose compound is a subselector of
        // component1's compound.
        let mut end = i2;
        loop {
            let component2 = &comps2[end];
            if compound_is_superselector(&component1.compound, &component2.compound) {
                break;
            }
            end += 1;
            if end == comps2.len() - 1 {
                return false;
            }
        }

        // Intervening components (between i2 and end) must be compatible with
        // the previous combinator.
        if !compatible_with_previous_combinator(previous_combinator, &comps2[i2..end]) {
            return false;
        }

        let component2 = &comps2[end];
        let combinator1 = component1.combinator;
        let combinator2 = component2.combinator;
        if !is_supercombinator(combinator1, combinator2) {
            return false;
        }

        i1 += 1;
        i2 = end + 1;
        previous_combinator = combinator1;

        if comps1.len() - i1 == 1 {
            match combinator1 {
                Some(Combinator::FollowingSibling) => {
                    // `.foo ~ .bar` only supersedes selectors whose intervening
                    // combinators are all sibling combinators.
                    let upto = comps2.len() - 1;
                    if !comps2[i2..upto]
                        .iter()
                        .all(|c| is_supercombinator(combinator1, c.combinator))
                    {
                        return false;
                    }
                }
                Some(_) if comps2.len() - i2 > 1 => return false,
                _ => {}
            }
        }
    }
}

fn compatible_with_previous_combinator(previous: Option<Combinator>, parents: &[ComplexComponent]) -> bool {
    if parents.is_empty() {
        return true;
    }
    let Some(prev) = previous else {
        return true;
    };
    if prev != Combinator::FollowingSibling {
        return false;
    }
    parents.iter().all(|c| {
        matches!(
            c.combinator,
            Some(Combinator::FollowingSibling) | Some(Combinator::NextSibling)
        )
    })
}

/// Whether `combinator1` is a supercombinator of `combinator2`.
fn is_supercombinator(c1: Option<Combinator>, c2: Option<Combinator>) -> bool {
    c1 == c2
        || (c1.is_none() && c2 == Some(Combinator::Child))
        || (c1 == Some(Combinator::FollowingSibling) && c2 == Some(Combinator::NextSibling))
}

/// Whether compound `a` is a superselector of compound `b`.
fn compound_is_superselector(a: &Compound, b: &Compound) -> bool {
    if a.simples.len() > b.simples.len() {
        return false;
    }
    a.simples
        .iter()
        .all(|s1| b.simples.iter().any(|s2| simple_is_superselector(s1, s2)))
}

/// Whether simple `a` is a superselector of simple `b`.
fn simple_is_superselector(a: &Simple, b: &Simple) -> bool {
    if a == b {
        return true;
    }
    match a {
        // `*` (no namespace) matches everything.
        Simple::Universal { ns: None } => true,
        // `*|*` matches everything.
        Simple::Universal { ns: Some(n) } if n == "*" => true,
        // `ns|*` matches `ns|type` and `ns|*` (same namespace).
        Simple::Universal { ns: Some(n) } => match b {
            Simple::Type(t) => type_namespace(t).as_deref() == Some(n.as_str()),
            Simple::Universal { ns: Some(m) } => n == m,
            _ => false,
        },
        // A type selector `t` supersedes a matching type selector, honoring
        // a `*` namespace wildcard.
        Simple::Type(t) => match b {
            Simple::Type(u) => {
                let (n1, name1) = split_type(t);
                let (n2, name2) = split_type(u);
                name1 == name2 && (n1.as_deref() == Some("*") || n1 == n2)
            }
            _ => false,
        },
        _ => false,
    }
}

/// Split a (possibly namespaced) type name into `(namespace, local)`.
fn split_type(t: &str) -> (Option<String>, String) {
    match t.split_once('|') {
        Some((ns, name)) => (Some(ns.to_string()), name.to_string()),
        None => (None, t.to_string()),
    }
}

/// The namespace component of a type selector string, if any.
fn type_namespace(t: &str) -> Option<String> {
    t.split_once('|').map(|(ns, _)| ns.to_string())
}

/// Extend a single complex selector: compute, for each component, the list of
/// possible replacements (each a sequence of components), then take the
/// Cartesian product and weave them into complete complex selectors. The first
/// option of every component is the original, so the unextended selector comes
/// out first. (dart-sass `Extender._extendComplex`.)
fn extend_complex(complex: &Complex, extensions: &[Extension]) -> Vec<Complex> {
    // For each component, options are sequences of components (the original
    // first, followed by any extension replacements, transitively resolved).
    let mut per_component: Vec<Vec<Vec<ComplexComponent>>> = Vec::new();
    let mut any_extended = false;
    for comp in &complex.components {
        let opts = extend_component(comp, extensions);
        if opts.len() > 1 {
            any_extended = true;
        }
        per_component.push(opts);
    }
    if !any_extended {
        return vec![complex.clone()];
    }

    // Each component's options become candidate complex selectors. Take the
    // Cartesian product across components and `weave` each path into one or
    // more complete complex selectors (dart-sass `paths` + `weave`).
    let per_component_complex: Vec<Vec<Complex>> = per_component
        .iter()
        .map(|opts| {
            opts.iter()
                .map(|seq| Complex {
                    components: seq.clone(),
                })
                .collect()
        })
        .collect();

    let mut combos: Vec<Vec<Complex>> = vec![Vec::new()];
    for opts in &per_component_complex {
        let mut next: Vec<Vec<Complex>> = Vec::new();
        for combo in &combos {
            for opt in opts {
                let mut c = combo.clone();
                c.push(opt.clone());
                next.push(c);
            }
        }
        combos = next;
        if combos.len() > 100_000 {
            break;
        }
    }

    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for path in combos {
        for c in weave(&path) {
            let r = c.render();
            if seen.insert(r) {
                out.push(c);
            }
        }
    }
    out
}

/// Weave a path of complex selectors (one per original component) into complete
/// complex selectors. Single-component path elements are simply appended;
/// multi-component ones have their parent components interwoven with the
/// accumulated prefix (dart-sass `weave`). Combinators in parent components are
/// not woven (we fall back to plain concatenation), but the common
/// descendant-only case is handled fully.
fn weave(path: &[Complex]) -> Vec<Complex> {
    let Some((first, rest)) = path.split_first() else {
        return Vec::new();
    };
    let mut prefixes: Vec<Vec<ComplexComponent>> = vec![first.components.clone()];
    for complex in rest {
        if complex.components.len() == 1 {
            for prefix in prefixes.iter_mut() {
                prefix.extend(complex.components.iter().cloned());
            }
            continue;
        }
        let last = match complex.components.last() {
            Some(l) => l.clone(),
            None => continue,
        };
        let base_parents = &complex.components[..complex.components.len() - 1];
        let mut next: Vec<Vec<ComplexComponent>> = Vec::new();
        for prefix in &prefixes {
            for mut woven in weave_parents(prefix, base_parents) {
                woven.push(last.clone());
                next.push(woven);
            }
        }
        prefixes = next;
        if prefixes.len() > 100_000 {
            break;
        }
    }
    prefixes
        .into_iter()
        .map(|components| Complex { components })
        .collect()
}

/// Interweave `prefix`'s components with `parents` (the parent components of a
/// multi-component extender), returning all order-preserving interleavings
/// (with unification of equal/superselector groups), per dart-sass
/// `_weaveParents`. Only the descendant-combinator case is supported; if any
/// component carries a combinator, fall back to a single concatenation.
fn weave_parents(prefix: &[ComplexComponent], parents: &[ComplexComponent]) -> Vec<Vec<ComplexComponent>> {
    if parents.is_empty() {
        return vec![prefix.to_vec()];
    }
    // Fall back to plain concatenation when combinators are involved (the full
    // grouping/trailing-combinator logic isn't ported).
    let has_combinator = prefix
        .iter()
        .chain(parents.iter())
        .any(|c| c.combinator.is_some());
    if has_combinator {
        let mut out = prefix.to_vec();
        out.extend(parents.iter().cloned());
        return vec![out];
    }

    // Each component is its own group (descendant-only). Compute the longest
    // common subsequence of the two component lists, unifying equal /
    // superselector groups, then interleave the remaining chunks around the LCS.
    let lcs = longest_common_subsequence(parents, prefix);

    let mut q1: std::collections::VecDeque<ComplexComponent> = prefix.iter().cloned().collect();
    let mut q2: std::collections::VecDeque<ComplexComponent> = parents.iter().cloned().collect();

    let mut choices: Vec<Vec<Vec<ComplexComponent>>> = Vec::new();
    for group in &lcs {
        // Chunk: drain from each queue until we reach the LCS group, then
        // produce the two orderings of the drained prefixes.
        let chunk = chunks(&mut q1, &mut q2, |q| {
            q.front()
                .map(|c| component_is_superselector(c, group))
                .unwrap_or(false)
        });
        if !chunk.is_empty() {
            choices.push(chunk);
        }
        choices.push(vec![vec![group.clone()]]);
        q1.pop_front();
        q2.pop_front();
    }
    let tail = chunks(&mut q1, &mut q2, |q| q.is_empty());
    if !tail.is_empty() {
        choices.push(tail);
    }

    // Cartesian product of the choices, flattening each path.
    let mut results: Vec<Vec<ComplexComponent>> = vec![Vec::new()];
    for choice in &choices {
        let mut next = Vec::new();
        for path in &results {
            for option in choice {
                let mut p = path.clone();
                p.extend(option.iter().cloned());
                next.push(p);
            }
        }
        results = next;
        if results.len() > 100_000 {
            break;
        }
    }
    results
}

/// `_chunks`: drain the leading subsequence of each queue (up to where `done`
/// first holds) and return the two orderings of the combined drained items, or
/// a single ordering when one side is empty.
fn chunks<F>(
    q1: &mut std::collections::VecDeque<ComplexComponent>,
    q2: &mut std::collections::VecDeque<ComplexComponent>,
    done: F,
) -> Vec<Vec<ComplexComponent>>
where
    F: Fn(&std::collections::VecDeque<ComplexComponent>) -> bool,
{
    let mut chunk1 = Vec::new();
    while !done(q1) {
        match q1.pop_front() {
            Some(c) => chunk1.push(c),
            None => break,
        }
    }
    let mut chunk2 = Vec::new();
    while !done(q2) {
        match q2.pop_front() {
            Some(c) => chunk2.push(c),
            None => break,
        }
    }
    match (chunk1.is_empty(), chunk2.is_empty()) {
        (true, true) => Vec::new(),
        (true, false) => vec![chunk2],
        (false, true) => vec![chunk1],
        (false, false) => {
            let mut a = chunk1.clone();
            a.extend(chunk2.clone());
            let mut b = chunk2;
            b.extend(chunk1);
            vec![a, b]
        }
    }
}

/// Longest common subsequence of two component lists, where two components are
/// "equal" for LCS purposes if they're identical or one is a superselector of
/// the other (the more specific is selected). Mirrors dart-sass's `select`.
fn longest_common_subsequence(
    list1: &[ComplexComponent],
    list2: &[ComplexComponent],
) -> Vec<ComplexComponent> {
    let n = list1.len();
    let m = list2.len();
    let mut lengths = vec![vec![0usize; m + 1]; n + 1];
    let mut selections: Vec<Vec<Option<ComplexComponent>>> = vec![vec![None; m]; n];
    for i in 0..n {
        for j in 0..m {
            let sel = lcs_select(&list1[i], &list2[j]);
            let has = sel.is_some();
            selections[i][j] = sel;
            lengths[i + 1][j + 1] = if has {
                lengths[i][j] + 1
            } else {
                lengths[i + 1][j].max(lengths[i][j + 1])
            };
        }
    }
    let mut out = Vec::new();
    backtrack(n as isize - 1, m as isize - 1, &lengths, &selections, &mut out);
    out
}

fn backtrack(
    i: isize,
    j: isize,
    lengths: &[Vec<usize>],
    selections: &[Vec<Option<ComplexComponent>>],
    out: &mut Vec<ComplexComponent>,
) {
    if i == -1 || j == -1 {
        return;
    }
    let (ui, uj) = (i as usize, j as usize);
    if let Some(sel) = &selections[ui][uj] {
        backtrack(i - 1, j - 1, lengths, selections, out);
        out.push(sel.clone());
        return;
    }
    if lengths[ui + 1][uj] > lengths[ui][uj + 1] {
        backtrack(i, j - 1, lengths, selections, out);
    } else {
        backtrack(i - 1, j, lengths, selections, out);
    }
}

/// The LCS selection function for two parent components.
fn lcs_select(a: &ComplexComponent, b: &ComplexComponent) -> Option<ComplexComponent> {
    if a == b {
        return Some(a.clone());
    }
    if component_is_superselector(a, b) {
        return Some(b.clone());
    }
    if component_is_superselector(b, a) {
        return Some(a.clone());
    }
    None
}

/// Whether one parent component is a superselector of another (compound-level,
/// descendant context).
fn component_is_superselector(a: &ComplexComponent, b: &ComplexComponent) -> bool {
    a.combinator.is_none() && b.combinator.is_none() && compound_is_superselector(&a.compound, &b.compound)
}

/// Compute the replacement options for one component. The first option is the
/// original component (as a one-element sequence). For a compound with multiple
/// simple selectors, each simple is extended independently and the
/// within-compound Cartesian product is taken (so `.a.b` with `.a`→`.x` and
/// `.b`→`.y` yields `.a.b`, `.a.y`, `.b.x`, `.x.y`), then unified. Chains
/// (`.a` → `.b` → `.c`) expand transitively: each extender is fully extended in
/// isolation first, so the per-simple option set already contains the whole
/// chain. The within-compound product is then computed once (no re-extension of
/// the original simples, which would spuriously double-count combinations).
fn extend_component(comp: &ComplexComponent, extensions: &[Extension]) -> Vec<Vec<ComplexComponent>> {
    let simples = &comp.compound.simples;
    // Per-simple option list: index 0 is "keep self" (None); the rest are
    // (transitively expanded) extender complex selectors targeting this simple.
    let mut per_simple: Vec<Vec<Option<Complex>>> = Vec::new();
    let mut any = false;
    for s in simples {
        let mut opts: Vec<Option<Complex>> = vec![None];
        let mut seen: HashSet<String> = HashSet::new();
        for extender in collect_extenders(s, extensions, &mut Vec::new()) {
            let key = extender.render();
            if seen.insert(key) {
                opts.push(Some(extender));
                any = true;
            }
        }
        per_simple.push(opts);
    }
    let mut options: Vec<Vec<ComplexComponent>> = vec![vec![comp.clone()]];
    if !any {
        return options;
    }

    // Cartesian product of per-simple choices.
    let mut paths: Vec<Vec<&Option<Complex>>> = vec![Vec::new()];
    for opts in &per_simple {
        let mut next = Vec::new();
        for path in &paths {
            for opt in opts {
                let mut p = path.clone();
                p.push(opt);
                next.push(p);
            }
        }
        paths = next;
        if paths.len() > 100_000 {
            break;
        }
    }

    let mut seen: HashSet<String> = HashSet::new();
    seen.insert(render_components(std::slice::from_ref(comp)));
    for path in &paths {
        // Skip the all-self path (the original compound, already option 0).
        if path.iter().all(|o| o.is_none()) {
            continue;
        }
        if let Some(seq) = build_extended_compound(comp, simples, path) {
            let key = render_components(&seq);
            if seen.insert(key) {
                options.push(seq);
            }
        }
    }
    options
}

/// Collect every extender complex selector for `target`, transitively: a direct
/// extender that is itself a target of another extension is expanded into its
/// own extenders too. `stack` guards against extension cycles.
fn collect_extenders(target: &Simple, extensions: &[Extension], stack: &mut Vec<Simple>) -> Vec<Complex> {
    if stack.contains(target) {
        return Vec::new();
    }
    let mut out: Vec<Complex> = Vec::new();
    // dart-sass emits same-target extenders in reverse registration order.
    for ext in extensions.iter().rev() {
        let Some(t) = &ext.target else { continue };
        if t != target {
            continue;
        }
        ext.matched.set(true);
        for extender in &ext.extenders {
            // The direct extender selector itself.
            out.push(extender.clone());
            // If the extender is a single simple that is itself a target,
            // expand transitively (chains).
            if extender.components.len() == 1 && extender.components[0].compound.simples.len() == 1 {
                let inner = &extender.components[0].compound.simples[0];
                stack.push(target.clone());
                let deeper = collect_extenders(inner, extensions, stack);
                stack.pop();
                out.extend(deeper);
            }
        }
    }
    out
}

/// Build the extended component sequence for one within-compound product path.
/// `path[i]` is `None` to keep `simples[i]`, or `Some(extender)` to replace it.
/// Self-kept simples come first (in order), then each extender's trailing
/// compound is unified in. Multi-component extenders are supported only when
/// exactly one simple is extended.
fn build_extended_compound(
    comp: &ComplexComponent,
    simples: &[Simple],
    path: &[&Option<Complex>],
) -> Option<Vec<ComplexComponent>> {
    // Self-kept simples (originals not being extended).
    let mut base: Vec<Simple> = Vec::new();
    let mut extenders: Vec<&Complex> = Vec::new();
    for (i, choice) in path.iter().enumerate() {
        match choice {
            None => base.push(simples[i].clone()),
            Some(ext) => extenders.push(ext),
        }
    }

    if extenders.len() == 1 && extenders[0].components.len() > 1 {
        // A single multi-component extender: splice its leading components in
        // and unify its trailing compound with the base.
        let extender = extenders[0];
        let ext_last = extender.components.last()?;
        let ext_lead = &extender.components[..extender.components.len() - 1];
        let unified = unify_compounds(&base, &ext_last.compound.simples)?;
        let mut seq: Vec<ComplexComponent> = Vec::new();
        for (k, lead) in ext_lead.iter().enumerate() {
            let combinator = if k == 0 { comp.combinator } else { lead.combinator };
            seq.push(ComplexComponent {
                combinator,
                compound: lead.compound.clone(),
            });
        }
        seq.push(ComplexComponent {
            combinator: ext_last.combinator,
            compound: Compound { simples: unified },
        });
        return Some(seq);
    }

    // All single-component extenders: unify their compounds into the base, in
    // order, base simples first.
    let mut acc = base;
    for ext in &extenders {
        if ext.components.len() != 1 {
            return None; // can't combine multiple multi-component extenders
        }
        acc = unify_compounds(&acc, &ext.components[0].compound.simples)?;
    }
    if acc.is_empty() {
        return None;
    }
    Some(vec![ComplexComponent {
        combinator: comp.combinator,
        compound: Compound { simples: acc },
    }])
}

/// Render a component sequence to a stable string key (for dedup).
fn render_components(seq: &[ComplexComponent]) -> String {
    Complex {
        components: seq.to_vec(),
    }
    .render()
}

/// Unify a `base` compound with `extra` (the extender's trailing compound),
/// returning the combined compound or `None` if they can't unify. A faithful
/// port of dart-sass `unifyCompound`: start from `base`, then fold each `extra`
/// simple in via `simple_unify`, keeping pseudo-classes after a pseudo-element
/// in `pseudo_result` to preserve their relative order.
fn unify_compounds(base: &[Simple], extra: &[Simple]) -> Option<Vec<Simple>> {
    let mut result: Vec<Simple> = base.to_vec();
    let mut pseudo_result: Vec<Simple> = Vec::new();
    let mut pseudo_element_found = false;
    for simple in extra {
        if pseudo_element_found && matches!(simple, Simple::Pseudo(_)) {
            pseudo_result = simple_unify(simple, &pseudo_result)?;
        } else {
            if is_pseudo_element(simple) {
                pseudo_element_found = true;
            }
            result = simple_unify(simple, &result)?;
        }
    }
    result.extend(pseudo_result);
    if result.is_empty() {
        return None;
    }
    Some(result)
}

/// Unify a single simple selector into a compound (`SimpleSelector.unify`).
fn simple_unify(this: &Simple, compound: &[Simple]) -> Option<Vec<Simple>> {
    match this {
        Simple::Universal { .. } => match compound.split_first() {
            Some((first @ (Simple::Universal { .. } | Simple::Type(_)), rest)) => {
                let unified = unify_universal_and_element(this, first)?;
                let mut out = vec![unified];
                out.extend_from_slice(rest);
                Some(out)
            }
            None => Some(vec![this.clone()]),
            Some(_) => {
                // A `null` or `*` namespace adds nothing; drop the universal.
                if universal_ns_droppable(this) {
                    Some(compound.to_vec())
                } else {
                    let mut out = vec![this.clone()];
                    out.extend_from_slice(compound);
                    Some(out)
                }
            }
        },
        Simple::Type(_) => match compound.first() {
            Some(first @ (Simple::Universal { .. } | Simple::Type(_))) => {
                let unified = unify_universal_and_element(this, first)?;
                let mut out = vec![unified];
                out.extend_from_slice(&compound[1..]);
                Some(out)
            }
            _ => {
                let mut out = vec![this.clone()];
                out.extend_from_slice(compound);
                Some(out)
            }
        },
        // A pseudo selector inserts before the first pseudo-ELEMENT (so pseudo
        // classes stay ahead of pseudo elements); two distinct pseudo elements
        // can't share a compound.
        Simple::Pseudo(_) => {
            if compound.len() == 1 && matches!(compound[0], Simple::Universal { .. }) {
                return simple_unify(&compound[0], std::slice::from_ref(this));
            }
            if compound.contains(this) {
                return Some(compound.to_vec());
            }
            let this_is_element = is_pseudo_element(this);
            let mut out = Vec::new();
            let mut added = false;
            for s in compound {
                if !added && is_pseudo_element(s) {
                    // Only one pseudo-element allowed per compound.
                    if this_is_element {
                        return None;
                    }
                    out.push(this.clone());
                    added = true;
                }
                out.push(s.clone());
            }
            if !added {
                out.push(this.clone());
            }
            Some(out)
        }
        // Class / id / attribute / placeholder: insert before the first pseudo.
        _ => {
            if compound.len() == 1 && matches!(compound[0], Simple::Universal { .. }) {
                // `other.unify([this])` where other is the universal.
                return simple_unify(&compound[0], std::slice::from_ref(this));
            }
            if compound.contains(this) {
                return Some(compound.to_vec());
            }
            // Insert `this` before the first pseudo selector.
            let mut out = Vec::new();
            let mut added = false;
            for s in compound {
                if !added && matches!(s, Simple::Pseudo(_)) {
                    out.push(this.clone());
                    added = true;
                }
                out.push(s.clone());
            }
            if !added {
                out.push(this.clone());
            }
            Some(out)
        }
    }
}

/// Whether a universal selector contributes nothing to a compound that already
/// has other simples (namespace `null` or `*`).
fn universal_ns_droppable(s: &Simple) -> bool {
    matches!(s, Simple::Universal { ns } if ns.is_none() || ns.as_deref() == Some("*"))
}

/// Unify two universal/type selectors (`unifyUniversalAndElement`). Each is a
/// `(namespace, name)` pair where a universal has `name == None`.
fn unify_universal_and_element(a: &Simple, b: &Simple) -> Option<Simple> {
    let (ns1, name1) = namespace_and_name(a)?;
    let (ns2, name2) = namespace_and_name(b)?;

    let namespace = if ns1 == ns2 || ns2.as_deref() == Some("*") {
        ns1.clone()
    } else if ns1.as_deref() == Some("*") {
        ns2.clone()
    } else {
        return None;
    };

    let name = if name1 == name2 || name2.is_none() {
        name1.clone()
    } else if name1.is_none() || name1.as_deref() == Some("*") {
        name2.clone()
    } else {
        return None;
    };

    Some(match name {
        None => Simple::Universal { ns: namespace },
        Some(n) => match namespace {
            Some(ns) => Simple::Type(format!("{ns}|{n}")),
            None => Simple::Type(n),
        },
    })
}

/// Decompose a universal/type selector into `(namespace, name)`.
fn namespace_and_name(s: &Simple) -> Option<(Option<String>, Option<String>)> {
    match s {
        Simple::Universal { ns } => Some((ns.clone(), None)),
        Simple::Type(t) => {
            let (ns, name) = split_type(t);
            Some((ns, Some(name)))
        }
        _ => None,
    }
}

/// Whether a pseudo selector is a pseudo-element (`::name` or a legacy
/// single-colon pseudo-element).
fn is_pseudo_element(s: &Simple) -> bool {
    let Simple::Pseudo(text) = s else {
        return false;
    };
    if text.starts_with("::") {
        return true;
    }
    // Legacy single-colon pseudo-elements.
    let name = text.trim_start_matches(':');
    let base = name.split(['(', ' ']).next().unwrap_or(name).to_ascii_lowercase();
    matches!(base.as_str(), "before" | "after" | "first-line" | "first-letter")
}
