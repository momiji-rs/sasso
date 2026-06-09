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
    pub(crate) fn as_str(self) -> &'static str {
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
    pub(crate) fn render(&self) -> String {
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
    pub(crate) fn render(&self) -> String {
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
    pub(crate) fn render(&self) -> String {
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
            '|' => {
                // A leading `|` is the *empty* namespace: `|c`, `|*`.
                *i += 1;
                match read_type_after_ns(chars, i)? {
                    Simple::Universal { .. } => simples.push(Simple::Universal {
                        ns: Some(String::new()),
                    }),
                    Simple::Type(t) => simples.push(Simple::Type(format!("|{t}"))),
                    other => simples.push(other),
                }
            }
            _ if is_ident_start(c) || c == '\\' => {
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

/// Whether any compound in any complex selector of `list` contains `target`.
pub(crate) fn list_contains_simple(list: &[Complex], target: &Simple) -> bool {
    list.iter().any(|c| {
        c.components
            .iter()
            .any(|comp| comp.compound.simples.contains(target))
    })
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
    // Extenders are source selectors too (dart-sass's `_originals` is store-wide),
    // so a source extender is protected from being trimmed away by a broader
    // generated one — e.g. a transitive `:is(a, b)` must not trim the original
    // `:is(a)` that produced it.
    for ext in extensions {
        for complex in &ext.extenders {
            originals.insert(complex.render());
        }
    }

    let result = extend_to_fixpoint(original, extensions);

    // Remove selectors that are subselectors of another (redundant), keeping
    // originals (dart-sass `_trim`).
    let result = trim(result, &originals);

    // Simplify placeholders inside `:is()/:where()/:not()`-style pseudo
    // arguments, dropping selectors whose pseudo can never match.
    let mut simplified: Vec<Complex> = Vec::new();
    for c in result {
        if let Some(c) = simplify_pseudo_placeholders(&c) {
            simplified.push(c);
        }
    }

    // Drop complex selectors that still contain a (top-level) placeholder.
    let kept: Vec<&Complex> = simplified.iter().filter(|c| !c.has_placeholder()).collect();
    let all_placeholders = kept.is_empty();
    let selectors: Vec<String> = kept.iter().map(|c| c.render()).collect();
    ExtendResult {
        selectors,
        all_placeholders,
    }
}

/// Simplify placeholder selectors inside pseudo-class arguments
/// (`:is()`/`:where()`/`:matches()`/`:not()` etc.): remove placeholder complex
/// selectors from the argument list. For "matches-any" pseudos an empty
/// argument means the whole compound matches nothing (return `None` to drop the
/// selector); for `:not()` an empty argument means the pseudo excludes nothing
/// and is removed (a now-empty compound becomes `*`). Returns the rewritten
/// complex selector, or `None` if it can never match.
fn simplify_pseudo_placeholders(complex: &Complex) -> Option<Complex> {
    let mut components = Vec::new();
    for comp in &complex.components {
        let mut simples: Vec<Simple> = Vec::new();
        for s in &comp.compound.simples {
            match s {
                Simple::Pseudo(text) if text.contains('%') => {
                    match simplify_one_pseudo(text) {
                        PseudoResult::Keep(new) => simples.push(Simple::Pseudo(new)),
                        PseudoResult::Remove => { /* `:not()` with empty arg */ }
                        PseudoResult::NeverMatches => return None,
                    }
                }
                other => simples.push(other.clone()),
            }
        }
        // A compound emptied by removing a `:not()` becomes the universal `*`.
        if simples.is_empty() {
            simples.push(Simple::Universal { ns: None });
        }
        components.push(ComplexComponent {
            combinator: comp.combinator,
            compound: Compound { simples },
        });
    }
    Some(Complex { components })
}

enum PseudoResult {
    /// Keep the pseudo, rewritten to this text.
    Keep(String),
    /// Remove the pseudo entirely (e.g. `:not()` with no remaining args).
    Remove,
    /// The pseudo can never match — drop the whole selector.
    NeverMatches,
}

/// Simplify a single pseudo selector text whose argument contains a
/// `%placeholder`. Only `:is/:where/:matches/:any/:-*-any/:not` take a selector
/// argument we process; others are kept verbatim.
fn simplify_one_pseudo(text: &str) -> PseudoResult {
    // Split into `:name(` ... `)`.
    let Some(open) = text.find('(') else {
        return PseudoResult::Keep(text.to_string());
    };
    if !text.ends_with(')') {
        return PseudoResult::Keep(text.to_string());
    }
    let head = &text[..open]; // e.g. `:not`
    let arg = &text[open + 1..text.len() - 1];
    let name = head.trim_start_matches(':').to_ascii_lowercase();
    let is_matchish = matches!(name.as_str(), "is" | "where" | "matches" | "any") || name.ends_with("-any");
    let is_not = name == "not";
    if !is_matchish && !is_not {
        return PseudoResult::Keep(text.to_string());
    }
    // Parse the argument selector list and drop placeholder-bearing complexes.
    let Some(list) = parse_list(arg) else {
        return PseudoResult::Keep(text.to_string());
    };
    let kept: Vec<&Complex> = list.iter().filter(|c| !c.has_placeholder()).collect();
    if kept.is_empty() {
        return if is_not {
            PseudoResult::Remove
        } else {
            PseudoResult::NeverMatches
        };
    }
    let inner = kept.iter().map(|c| c.render()).collect::<Vec<_>>().join(", ");
    PseudoResult::Keep(format!("{head}({inner})"))
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
/// Operates in the trailing-combinator model (a faithful port of dart-sass
/// `complexIsSuperselector`), where each component carries the combinator that
/// joins it to the *next* component.
fn complex_is_superselector(c1: &Complex, c2: &Complex) -> bool {
    complex_is_superselector_trailing(&to_trailing(&c1.components), &to_trailing(&c2.components))
}

/// dart-sass `complexIsSuperselector` over trailing-combinator component lists.
fn complex_is_superselector_trailing(complex1: &[TComp], complex2: &[TComp]) -> bool {
    // Selectors with trailing operators are neither super- nor subselectors.
    if complex1.last().map(|c| !c.combinators.is_empty()).unwrap_or(true) {
        return false;
    }
    if complex2.last().map(|c| !c.combinators.is_empty()).unwrap_or(true) {
        return false;
    }

    let mut i1 = 0usize;
    let mut i2 = 0usize;
    let mut previous_combinator: Option<Combinator> = None;
    loop {
        let remaining1 = complex1.len() - i1;
        let remaining2 = complex2.len() - i2;
        if remaining1 == 0 || remaining2 == 0 {
            return false;
        }
        if remaining1 > remaining2 {
            return false;
        }
        let component1 = &complex1[i1];
        if component1.combinators.len() > 1 {
            return false;
        }
        if remaining1 == 1 {
            let parents = &complex2[i2..complex2.len() - 1];
            if parents.iter().any(|p| p.combinators.len() > 1) {
                return false;
            }
            let Some(last2) = complex2.last() else {
                return false;
            };
            return compound_is_superselector(&component1.compound, &last2.compound, parents);
        }

        // Find the first index `end` in complex2 whose compound is a subselector
        // of component1's compound.
        let mut end = i2;
        loop {
            let component2 = &complex2[end];
            if component2.combinators.len() > 1 {
                return false;
            }
            if compound_is_superselector(&component1.compound, &component2.compound, &[]) {
                break;
            }
            end += 1;
            if end == complex2.len() - 1 {
                return false;
            }
        }

        // Intervening components (between i2 and end) must be compatible with the
        // previous combinator.
        if !compatible_with_previous_combinator(previous_combinator, &complex2[i2..end]) {
            return false;
        }

        let component2 = &complex2[end];
        let combinator1 = component1.combinators.first().copied();
        let combinator2 = component2.combinators.first().copied();
        if !is_supercombinator(combinator1, combinator2) {
            return false;
        }

        i1 += 1;
        i2 = end + 1;
        previous_combinator = combinator1;

        if complex1.len() - i1 == 1 {
            match combinator1 {
                Some(Combinator::FollowingSibling) => {
                    // `.foo ~ .bar` only supersedes selectors whose intervening
                    // combinators are all subcombinators of `~`.
                    let upto = complex2.len() - 1;
                    if !complex2[i2..upto]
                        .iter()
                        .all(|c| is_supercombinator(combinator1, c.combinators.first().copied()))
                    {
                        return false;
                    }
                }
                Some(_) if complex2.len() - i2 > 1 => return false,
                _ => {}
            }
        }
    }
}

fn compatible_with_previous_combinator(previous: Option<Combinator>, parents: &[TComp]) -> bool {
    if parents.is_empty() {
        return true;
    }
    let Some(prev) = previous else {
        return true;
    };
    // The child and next-sibling combinators require the *immediate* following
    // component be a superselector.
    if prev != Combinator::FollowingSibling {
        return false;
    }
    // The following-sibling combinator allows intermediate components, but only
    // if they're all siblings.
    parents.iter().all(|c| {
        matches!(
            c.combinators.first().copied(),
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

/// Parse a selector pseudo `:name(<selectors>)` of the `:is`/`:matches`/`:any`/
/// `:where`/`:-*-any`/`:has`/`:host`/`:host-context` family into its normalized
/// name and argument selector list. `None` for any other (or non-selector) pseudo.
fn parse_selector_pseudo(text: &str) -> Option<(String, Vec<Complex>)> {
    let open = text.find('(')?;
    if !text.ends_with(')') {
        return None;
    }
    let name = text[..open].trim_start_matches(':').to_ascii_lowercase();
    let known = matches!(
        name.as_str(),
        "is" | "where" | "matches" | "any" | "has" | "host" | "host-context"
    ) || name.ends_with("-any");
    if !known {
        return None;
    }
    let list = parse_list(&text[open + 1..text.len() - 1])?;
    Some((name, list))
}

/// dart-sass `_selectorPseudoIsSuperselector`. A selector pseudo on the super
/// side matches if `compound2` carries a same-name selector pseudo whose
/// argument our list supersedes, OR (for the `:is`/`:matches`/`:any`/`:where`/
/// `:-*-any` family) one of our argument complexes is a superselector of
/// `parents` followed by `compound2`. The relational `:has`/`:host`/
/// `:host-context` use only the same-name rule.
fn selector_pseudo_is_super(name: &str, branches: &[Complex], b: &Compound, parents: &[TComp]) -> bool {
    for s in &b.simples {
        if let Simple::Pseudo(t) = s {
            if let Some((n2, b_branches)) = parse_selector_pseudo(t) {
                // Our list must supersede EVERY branch of `b`'s same-name pseudo:
                // `:is(c)` is NOT a superselector of `:is(c, d)` (it can't match
                // the `d` branch), but `:is(c, d)` IS of `:is(c)`.
                if n2 == name
                    && !b_branches.is_empty()
                    && b_branches
                        .iter()
                        .all(|s2| list_is_superselector(branches, std::slice::from_ref(s2)))
                {
                    return true;
                }
            }
        }
    }
    let matchish = matches!(name, "is" | "where" | "matches" | "any") || name.ends_with("-any");
    if !matchish {
        return false;
    }
    branches.iter().any(|branch| {
        let mut target: Vec<TComp> = parents.to_vec();
        target.push(TComp {
            compound: b.clone(),
            combinators: Vec::new(),
        });
        complex_is_superselector_trailing(&to_trailing(&branch.components), &target)
    })
}

/// Whether compound `a` is a superselector of compound `b` (dart-sass
/// `compoundIsSuperselector`). A pseudo-element effectively changes the target
/// of a compound rather than narrowing it, so if either compound has a
/// pseudo-element they must both have the *same* one (with matching simples on
/// each side of it). `parents` are the components of `b`'s complex that precede
/// its final compound, used by the `:is`-family selector-pseudo rule.
fn compound_is_superselector(a: &Compound, b: &Compound, parents: &[TComp]) -> bool {
    match (find_pseudo_element(a), find_pseudo_element(b)) {
        (Some((pe1, i1)), Some((pe2, i2))) => {
            pe1 == pe2
                && compound_components_is_superselector(&a.simples[..i1], &b.simples[..i2])
                && compound_components_is_superselector(&a.simples[i1 + 1..], &b.simples[i2 + 1..])
        }
        // Exactly one side has a pseudo-element: never a superselector.
        (Some(_), None) | (None, Some(_)) => false,
        (None, None) => a.simples.iter().all(|s1| {
            // A selector pseudo (`:is(...)` etc.) follows the dart-sass pseudo
            // rule; every other simple must match some simple of `b`.
            if let Simple::Pseudo(text) = s1 {
                if let Some((name, branches)) = parse_selector_pseudo(text) {
                    return selector_pseudo_is_super(&name, &branches, b, parents);
                }
            }
            b.simples.iter().any(|s2| simple_is_superselector(s1, s2))
        }),
    }
}

/// Like [`compound_is_superselector`] over raw simple-selector slices, treating
/// an empty `b` as the universal selector (dart-sass
/// `_compoundComponentsIsSuperselector`).
fn compound_components_is_superselector(a: &[Simple], b: &[Simple]) -> bool {
    if a.is_empty() {
        return true;
    }
    let universal = [Simple::Universal { ns: None }];
    let b = if b.is_empty() { &universal[..] } else { b };
    compound_is_superselector(
        &Compound { simples: a.to_vec() },
        &Compound { simples: b.to_vec() },
        &[],
    )
}

/// If `compound` contains a pseudo-element, return it and its index.
fn find_pseudo_element(compound: &Compound) -> Option<(&Simple, usize)> {
    compound
        .simples
        .iter()
        .enumerate()
        .find(|(_, s)| is_pseudo_element(s))
        .map(|(i, s)| (s, i))
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
        // The component changed if there are alternatives, or if its single
        // option differs from the original (a pseudo argument was extended in
        // place, e.g. `:not(.c)` → `:not(.c):not(.a)`).
        if opts.len() > 1 || opts.first().map(|s| s.as_slice()) != Some(std::slice::from_ref(comp)) {
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
/// accumulated prefix (dart-sass `weave`). This faithfully handles combinators
/// (`>`, `+`, `~`) in parent components via a trailing-combinator model and
/// dart-sass's `_mergeTrailingCombinators` logic.
fn weave(path: &[Complex]) -> Vec<Complex> {
    let Some((first, rest)) = path.split_first() else {
        return Vec::new();
    };
    // The whole woven selector inherits the leading combinator of the first path
    // element's first component (a "child selector hack" like `> .foo`). It is
    // re-applied to every result at the end, since the trailing-combinator model
    // can't carry it internally.
    let leading = leading_combinator(first);
    // Work in the trailing-combinator representation throughout the weave; only
    // the prefixes accumulate, so converting once per step is cheap.
    //
    // In the public model the combinator joining component `i-1` to component
    // `i` is stored as the *leading* combinator of `i`. dart-sass stores it as
    // the *trailing* combinator of `i-1`. Each path element is the extension of
    // one original component, so the leading combinator of a later path
    // element's first component must be moved onto the trailing position of the
    // accumulated prefix's last component before weaving.
    let mut prefixes: Vec<Vec<TComp>> = vec![to_trailing(&first.components)];
    for complex in rest {
        let lead = leading_combinator(complex);
        if let Some(comb) = lead {
            for prefix in prefixes.iter_mut() {
                if let Some(last) = prefix.last_mut() {
                    last.combinators = vec![comb];
                }
            }
        }
        if complex.components.len() == 1 {
            // `concatenate`: append the single component. `to_trailing` already
            // drops the first component's leading combinator (moved above).
            let appended = to_trailing(&complex.components);
            for prefix in prefixes.iter_mut() {
                prefix.extend(appended.iter().cloned());
            }
            continue;
        }
        let base = to_trailing(&complex.components);
        let Some((last, parents)) = base.split_last() else {
            continue;
        };
        let mut next: Vec<Vec<TComp>> = Vec::new();
        for prefix in &prefixes {
            for mut woven in weave_parents_trailing(prefix, parents) {
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
        .map(|tcomps| {
            let mut components = from_trailing(&tcomps);
            if let (Some(comb), Some(first)) = (leading, components.first_mut()) {
                first.combinator = Some(comb);
            }
            Complex { components }
        })
        .collect()
}

/// The leading combinator of a complex selector's first component, if any. In
/// the public model this represents how the complex attaches to whatever
/// precedes it.
fn leading_combinator(complex: &Complex) -> Option<Combinator> {
    complex.components.first().and_then(|c| c.combinator)
}

/// A complex-selector component in the *trailing-combinator* representation
/// dart-sass uses for weaving: a compound followed by zero or more combinators
/// that join it to the *next* component. (The current public model attaches a
/// single combinator to the *following* compound; we convert at the boundary.)
#[derive(Clone, PartialEq, Eq, Debug)]
struct TComp {
    compound: Compound,
    combinators: Vec<Combinator>,
}

/// Convert a leading-combinator component slice into the trailing-combinator
/// form. A combinator on leading component `i` becomes a trailing combinator on
/// component `i-1`. The last component never has a trailing combinator.
fn to_trailing(comps: &[ComplexComponent]) -> Vec<TComp> {
    let mut out: Vec<TComp> = comps
        .iter()
        .map(|c| TComp {
            compound: c.compound.clone(),
            combinators: Vec::new(),
        })
        .collect();
    for i in 1..comps.len() {
        if let Some(comb) = comps[i].combinator {
            out[i - 1].combinators.push(comb);
        }
    }
    out
}

/// Convert a trailing-combinator component slice back into the public
/// leading-combinator form. A trailing combinator on component `i` becomes the
/// leading combinator of component `i+1`. A component carrying more than one
/// trailing combinator (a "bogus" run like `> +`) only keeps the last; such
/// selectors are filtered out elsewhere, so the exact rendering is unimportant.
fn from_trailing(tcomps: &[TComp]) -> Vec<ComplexComponent> {
    let mut out: Vec<ComplexComponent> = tcomps
        .iter()
        .map(|t| ComplexComponent {
            combinator: None,
            compound: t.compound.clone(),
        })
        .collect();
    for i in 0..tcomps.len() {
        if let Some(comb) = tcomps[i].combinators.last() {
            if i + 1 < out.len() {
                out[i + 1].combinator = Some(*comb);
            }
        }
    }
    out
}

/// Interweave `prefix`'s components with `parents` (the parent components of a
/// multi-component extender) in the trailing-combinator model, returning all
/// order-preserving interleavings (with unification of equal/superselector
/// groups and combinator merging). A faithful port of dart-sass `_weaveParents`.
/// Returns an empty list when the two can't be woven (dart-sass returns null).
fn weave_parents_trailing(prefix: &[TComp], parents: &[TComp]) -> Vec<Vec<TComp>> {
    // `_mergeLeadingCombinators`: our complexes never carry leading combinators,
    // so this always succeeds with an empty list — nothing to do.

    let mut queue1: std::collections::VecDeque<TComp> = prefix.iter().cloned().collect();
    let mut queue2: std::collections::VecDeque<TComp> = parents.iter().cloned().collect();

    let Some(trailing_combinators) = merge_trailing_combinators(&mut queue1, &mut queue2) else {
        return Vec::new();
    };

    // `_firstIfRootish`: ensure rootish selectors (`:root` etc.) are unified and
    // pinned to the front.
    let rootish1 = first_if_rootish(&mut queue1);
    let rootish2 = first_if_rootish(&mut queue2);
    match (rootish1, rootish2) {
        (Some(r1), Some(r2)) => {
            let Some(rootish) = unify_compounds(&r1.compound.simples, &r2.compound.simples) else {
                return Vec::new();
            };
            let comp = Compound { simples: rootish };
            queue1.push_front(TComp {
                compound: comp.clone(),
                combinators: r1.combinators,
            });
            queue2.push_front(TComp {
                compound: comp,
                combinators: r2.combinators,
            });
        }
        (Some(r), None) | (None, Some(r)) => {
            queue1.push_front(r.clone());
            queue2.push_front(r);
        }
        (None, None) => {}
    }

    let mut groups1 = group_selectors(queue1.iter().cloned());
    let mut groups2 = group_selectors(queue2.iter().cloned());

    // LCS of the two group lists (dart-sass passes groups2, groups1).
    let groups1_vec: Vec<Vec<TComp>> = groups1.iter().cloned().collect();
    let groups2_vec: Vec<Vec<TComp>> = groups2.iter().cloned().collect();
    let lcs = lcs_groups(&groups2_vec, &groups1_vec);

    let mut choices: Vec<Vec<Vec<TComp>>> = Vec::new();
    for group in &lcs {
        let chunk = chunks_groups(&mut groups1, &mut groups2, |seq| {
            seq.front()
                .map(|g| complex_is_parent_superselector(g, group))
                .unwrap_or(false)
        });
        // Flatten each chunk (a list of groups) into a flat component list.
        let flattened: Vec<Vec<TComp>> = chunk.into_iter().map(flatten_groups).collect();
        choices.push(flattened);
        choices.push(vec![group.clone()]);
        groups1.pop_front();
        groups2.pop_front();
    }
    let tail = chunks_groups(&mut groups1, &mut groups2, |seq| seq.is_empty());
    choices.push(tail.into_iter().map(flatten_groups).collect());
    for tc in trailing_combinators {
        choices.push(tc);
    }

    // Cartesian product of the non-empty choices, flattening each path. The
    // iteration order matches dart-sass `paths`: for each choice, the option is
    // the outer loop and the accumulated paths the inner loop.
    let mut results: Vec<Vec<TComp>> = vec![Vec::new()];
    for choice in choices.iter().filter(|c| !c.is_empty()) {
        let mut next = Vec::new();
        for option in choice {
            for path in &results {
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

/// Flatten a list of groups (each itself a component list) into a single
/// component list.
fn flatten_groups(groups: Vec<Vec<TComp>>) -> Vec<TComp> {
    groups.into_iter().flatten().collect()
}

/// dart-sass `_firstIfRootish`: if the first queue element's compound contains a
/// rootish pseudo-class (`:root`/`:scope`/`:host`/`:host-context`), remove and
/// return it.
fn first_if_rootish(queue: &mut std::collections::VecDeque<TComp>) -> Option<TComp> {
    let first = queue.front()?;
    let is_rootish = first.compound.simples.iter().any(|s| {
        if let Simple::Pseudo(text) = s {
            is_rootish_pseudo_class(text)
        } else {
            false
        }
    });
    if is_rootish {
        queue.pop_front()
    } else {
        None
    }
}

/// Whether a pseudo text is a rootish *class* (single colon) named one of
/// `root`/`scope`/`host`/`host-context`.
fn is_rootish_pseudo_class(text: &str) -> bool {
    if text.starts_with("::") {
        return false;
    }
    let name = text.trim_start_matches(':');
    let base = name.split(['(', ' ']).next().unwrap_or(name).to_ascii_lowercase();
    matches!(base.as_str(), "root" | "scope" | "host" | "host-context")
}

/// dart-sass `_mergeTrailingCombinators`: extract trailing combinators from the
/// ends of `components1`/`components2` and merge them into a list of choice
/// groups (each a list of component lists). Returns `None` if they can't be
/// merged. Iterative port of the recursive Dart original.
#[allow(clippy::type_complexity)]
fn merge_trailing_combinators(
    components1: &mut std::collections::VecDeque<TComp>,
    components2: &mut std::collections::VecDeque<TComp>,
) -> Option<Vec<Vec<Vec<TComp>>>> {
    let mut result: std::collections::VecDeque<Vec<Vec<TComp>>> = std::collections::VecDeque::new();
    loop {
        let combinators1 = components1
            .back()
            .map(|c| c.combinators.clone())
            .unwrap_or_default();
        let combinators2 = components2
            .back()
            .map(|c| c.combinators.clone())
            .unwrap_or_default();
        if combinators1.is_empty() && combinators2.is_empty() {
            return Some(result.into_iter().collect());
        }
        if combinators1.len() > 1 || combinators2.len() > 1 {
            return None;
        }
        let c1 = combinators1.first().copied();
        let c2 = combinators2.first().copied();
        // Each popped component retains its trailing combinators (mirroring
        // dart-sass `removeLast()`), so a sibling/child combinator that "wins" a
        // case is preserved on the resulting component.
        match (c1, c2) {
            (Some(Combinator::FollowingSibling), Some(Combinator::FollowingSibling)) => {
                let component1 = components1.pop_back()?;
                let component2 = components2.pop_back()?;
                if compound_is_superselector(&component1.compound, &component2.compound, &[]) {
                    result.push_front(vec![vec![component2]]);
                } else if compound_is_superselector(&component2.compound, &component1.compound, &[]) {
                    result.push_front(vec![vec![component1]]);
                } else {
                    let mut choices = vec![
                        vec![component1.clone(), component2.clone()],
                        vec![component2.clone(), component1.clone()],
                    ];
                    if let Some(unified) =
                        unify_compounds(&component1.compound.simples, &component2.compound.simples)
                    {
                        choices.push(vec![TComp {
                            compound: Compound { simples: unified },
                            combinators: vec![Combinator::FollowingSibling],
                        }]);
                    }
                    result.push_front(choices);
                }
            }
            (Some(Combinator::FollowingSibling), Some(Combinator::NextSibling))
            | (Some(Combinator::NextSibling), Some(Combinator::FollowingSibling)) => {
                // Identify which side is `+` (next) and which is `~` (following).
                let following_first = c1 == Some(Combinator::FollowingSibling);
                let following = if following_first {
                    components1.pop_back()?
                } else {
                    components2.pop_back()?
                };
                let next = if following_first {
                    components2.pop_back()?
                } else {
                    components1.pop_back()?
                };
                if compound_is_superselector(&following.compound, &next.compound, &[]) {
                    result.push_front(vec![vec![next]]);
                } else {
                    let mut choices = vec![vec![following.clone(), next.clone()]];
                    if let Some(unified) =
                        unify_compounds(&following.compound.simples, &next.compound.simples)
                    {
                        choices.push(vec![TComp {
                            compound: Compound { simples: unified },
                            combinators: next.combinators.clone(),
                        }]);
                    }
                    result.push_front(choices);
                }
            }
            (Some(Combinator::Child), Some(Combinator::NextSibling))
            | (Some(Combinator::Child), Some(Combinator::FollowingSibling)) => {
                // The sibling component wins (kept with its combinator); the
                // child component is dropped.
                let sibling = components2.pop_back()?;
                result.push_front(vec![vec![sibling]]);
            }
            (Some(Combinator::NextSibling), Some(Combinator::Child))
            | (Some(Combinator::FollowingSibling), Some(Combinator::Child)) => {
                let sibling = components1.pop_back()?;
                result.push_front(vec![vec![sibling]]);
            }
            (Some(comb1), Some(comb2)) if comb1 == comb2 => {
                let comp1 = components1.pop_back()?;
                let comp2 = components2.pop_back()?;
                let unified = unify_compounds(&comp1.compound.simples, &comp2.compound.simples)?;
                result.push_front(vec![vec![TComp {
                    compound: Compound { simples: unified },
                    combinators: vec![comb1],
                }]]);
            }
            (Some(combinator), None) => {
                if combinator == Combinator::Child
                    && components2
                        .back()
                        .map(|d| {
                            components1
                                .back()
                                .map(|c| compound_is_superselector(&d.compound, &c.compound, &[]))
                                .unwrap_or(false)
                        })
                        .unwrap_or(false)
                {
                    components2.pop_back();
                }
                let comp = components1.pop_back()?;
                result.push_front(vec![vec![comp]]);
            }
            (None, Some(combinator)) => {
                if combinator == Combinator::Child
                    && components1
                        .back()
                        .map(|d| {
                            components2
                                .back()
                                .map(|c| compound_is_superselector(&d.compound, &c.compound, &[]))
                                .unwrap_or(false)
                        })
                        .unwrap_or(false)
                {
                    components1.pop_back();
                }
                let comp = components2.pop_back()?;
                result.push_front(vec![vec![comp]]);
            }
            _ => return None,
        }
    }
}

/// dart-sass `_groupSelectors`: group components into the longest possible
/// sub-lists such that components without trailing combinators only appear at
/// the end of a sub-list. E.g. `A B > C D + E ~ G` → `[(A) (B > C) (D + E ~ G)]`.
fn group_selectors<I: IntoIterator<Item = TComp>>(components: I) -> std::collections::VecDeque<Vec<TComp>> {
    let mut groups: std::collections::VecDeque<Vec<TComp>> = std::collections::VecDeque::new();
    let mut group: Vec<TComp> = Vec::new();
    for component in components {
        let ends_group = component.combinators.is_empty();
        group.push(component);
        if ends_group {
            groups.push_back(std::mem::take(&mut group));
        }
    }
    if !group.is_empty() {
        groups.push_back(group);
    }
    groups
}

/// dart-sass `_mustUnify`: whether two component lists share a unique simple
/// selector (an id or pseudo-element) and so must be unified.
fn must_unify(complex1: &[TComp], complex2: &[TComp]) -> bool {
    let mut unique: Vec<&Simple> = Vec::new();
    for component in complex1 {
        for simple in &component.compound.simples {
            if is_unique_simple(simple) && !unique.contains(&simple) {
                unique.push(simple);
            }
        }
    }
    if unique.is_empty() {
        return false;
    }
    complex2.iter().any(|component| {
        component
            .compound
            .simples
            .iter()
            .any(|simple| is_unique_simple(simple) && unique.contains(&simple))
    })
}

/// dart-sass `_isUnique`: a compound may contain only one of these per type — an
/// id selector or a pseudo-element.
fn is_unique_simple(simple: &Simple) -> bool {
    matches!(simple, Simple::Id(_)) || is_pseudo_element(simple)
}

/// dart-sass `_chunks` over group lists: drain the leading subsequence of each
/// queue (up to where `done` first holds) and return the two orderings of the
/// combined drained groups, or a single ordering when one side is empty.
fn chunks_groups<F>(
    q1: &mut std::collections::VecDeque<Vec<TComp>>,
    q2: &mut std::collections::VecDeque<Vec<TComp>>,
    done: F,
) -> Vec<Vec<Vec<TComp>>>
where
    F: Fn(&std::collections::VecDeque<Vec<TComp>>) -> bool,
{
    let mut chunk1: Vec<Vec<TComp>> = Vec::new();
    while !done(q1) {
        match q1.pop_front() {
            Some(g) => chunk1.push(g),
            None => break,
        }
    }
    let mut chunk2: Vec<Vec<TComp>> = Vec::new();
    while !done(q2) {
        match q2.pop_front() {
            Some(g) => chunk2.push(g),
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

/// LCS over group lists with dart-sass's `select`: two groups match if they're
/// equal, one is a parent-superselector of the other (then the more specific is
/// chosen), or they must-unify and unify to a single complex.
fn lcs_groups(list1: &[Vec<TComp>], list2: &[Vec<TComp>]) -> Vec<Vec<TComp>> {
    let n = list1.len();
    let m = list2.len();
    let mut lengths = vec![vec![0usize; m + 1]; n + 1];
    let mut selections: Vec<Vec<Option<Vec<TComp>>>> = vec![vec![None; m]; n];
    for i in 0..n {
        for j in 0..m {
            let sel = lcs_select_groups(&list1[i], &list2[j]);
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
    backtrack_groups(n as isize - 1, m as isize - 1, &lengths, &selections, &mut out);
    out
}

fn backtrack_groups(
    i: isize,
    j: isize,
    lengths: &[Vec<usize>],
    selections: &[Vec<Option<Vec<TComp>>>],
    out: &mut Vec<Vec<TComp>>,
) {
    if i == -1 || j == -1 {
        return;
    }
    let (ui, uj) = (i as usize, j as usize);
    if let Some(sel) = &selections[ui][uj] {
        backtrack_groups(i - 1, j - 1, lengths, selections, out);
        out.push(sel.clone());
        return;
    }
    if lengths[ui + 1][uj] > lengths[ui][uj + 1] {
        backtrack_groups(i, j - 1, lengths, selections, out);
    } else {
        backtrack_groups(i - 1, j, lengths, selections, out);
    }
}

/// The LCS selection for two groups (component lists), per dart-sass.
fn lcs_select_groups(group1: &[TComp], group2: &[TComp]) -> Option<Vec<TComp>> {
    if group1 == group2 {
        return Some(group1.to_vec());
    }
    if complex_is_parent_superselector(group1, group2) {
        return Some(group2.to_vec());
    }
    if complex_is_parent_superselector(group2, group1) {
        return Some(group1.to_vec());
    }
    if !must_unify(group1, group2) {
        return None;
    }
    // Unify the two groups as complete complex selectors; keep only if the
    // unification yields a single complex selector.
    let c1 = Complex {
        components: from_trailing(group1),
    };
    let c2 = Complex {
        components: from_trailing(group2),
    };
    let unified = unify_complex(&c1, &c2)?;
    if unified.len() == 1 {
        Some(to_trailing(&unified[0].components))
    } else {
        None
    }
}

/// dart-sass `_complexIsParentSuperselector`: like `complexIsSuperselector` but
/// as though both shared an implicit trailing base compound. Implemented by
/// appending a shared placeholder component to each and testing superselection.
fn complex_is_parent_superselector(complex1: &[TComp], complex2: &[TComp]) -> bool {
    if complex1.len() > complex2.len() {
        return false;
    }
    let base = ComplexComponent {
        combinator: None,
        compound: Compound {
            simples: vec![Simple::Placeholder("<temp>".to_string())],
        },
    };
    let mut c1 = from_trailing(complex1);
    c1.push(base.clone());
    let mut c2 = from_trailing(complex2);
    c2.push(base);
    complex_is_superselector(&Complex { components: c1 }, &Complex { components: c2 })
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
/// A parsed selector-bearing pseudo: `:name(arg)`.
struct PseudoParts {
    /// The verbatim head including the leading colon(s), e.g. `:not` or `::is`.
    head: String,
    /// The lowercased name without colons, e.g. `not`, `is`.
    name: String,
    /// The raw argument text inside the parentheses.
    arg: String,
}

/// Parse a pseudo simple's text into its head/name/argument if it carries a
/// selector argument. Returns `None` for argument-less pseudos or non-pseudos.
fn parse_pseudo_parts(text: &str) -> Option<PseudoParts> {
    let open = text.find('(')?;
    if !text.ends_with(')') {
        return None;
    }
    let head = text[..open].to_string();
    let name = head.trim_start_matches(':').to_ascii_lowercase();
    let arg = text[open + 1..text.len() - 1].to_string();
    Some(PseudoParts { head, name, arg })
}

/// Whether any simple in `complex` is a selector-bearing pseudo (`:not(...)`,
/// `:is(...)`, etc.) whose argument we might further extend on a later pass.
fn complex_has_selector_pseudo(complex: &Complex) -> bool {
    complex.components.iter().any(|comp| {
        comp.compound.simples.iter().any(|s| {
            let Simple::Pseudo(text) = s else { return false };
            parse_pseudo_parts(text).is_some_and(|p| is_selector_pseudo(&p.name))
        })
    })
}

/// Whether a pseudo name takes a selector list we should extend.
fn is_selector_pseudo(name: &str) -> bool {
    matches!(
        name,
        "not" | "is" | "matches" | "where" | "any" | "current" | "has" | "host" | "host-context"
    ) || name.ends_with("-any")
}

/// dart-sass `_extendList`: recursively extend a list of complex selectors,
/// dedup, and trim redundant superselectors. Used for pseudo arguments.
fn extend_list(list: &[Complex], extensions: &[Extension]) -> Vec<Complex> {
    let mut originals: HashSet<String> = HashSet::new();
    for complex in list {
        originals.insert(complex.render());
    }
    let result = extend_to_fixpoint(list, extensions);
    trim(result, &originals)
}

/// Run the extension to a fixpoint: extend each selector, then feed every
/// newly-produced selector back through extension until nothing new appears.
/// This realizes dart-sass's extension-graph behavior where an extender produced
/// by one `@extend` can itself be extended by another (transitively, including
/// targets buried in pseudo arguments). Bounded to guarantee termination.
fn extend_to_fixpoint(list: &[Complex], extensions: &[Extension]) -> Vec<Complex> {
    let mut result: Vec<Complex> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    // Worklist of selectors still to extend (originals first).
    let mut queue: std::collections::VecDeque<Complex> = list.iter().cloned().collect();
    let mut iterations = 0usize;
    while let Some(complex) = queue.pop_front() {
        iterations += 1;
        if iterations > 100_000 || result.len() > 100_000 {
            break;
        }
        for c in extend_complex(&complex, extensions) {
            let rendered = c.render();
            if seen.insert(rendered) {
                // Re-extend a freshly-produced selector only when it carries a
                // selector-bearing pseudo: that's the sole case where a second
                // pass can reveal *more* extensions (a target buried in a
                // pseudo argument that became extendable, or an extender pseudo
                // that is itself a target). Plain class/placeholder/type chains
                // are already resolved transitively in a single pass by
                // `collect_extenders`, so re-feeding them would only risk
                // re-deriving cyclic self-extends without producing anything new.
                if complex_has_selector_pseudo(&c) {
                    queue.push_back(c.clone());
                }
                result.push(c);
            }
        }
    }
    result
}

/// dart-sass `_extendPseudo`: extend the selector argument of a selector-bearing
/// pseudo. Returns the replacement simple selectors for this pseudo position, or
/// `None` if nothing changed (keep the original simple unchanged).
///
/// For `:not()` with a single-complex original argument, the result is the set of
/// `:not()` simples to merge into the surrounding compound (older browsers can't
/// parse a complex/compound inside `:not`, so each becomes its own `:not`). For
/// every other pseudo (and `:not` whose argument was already a list), the result
/// is a single rewritten pseudo carrying the extended argument list.
// Recursion guard for nested pseudo-argument extension. A target that is itself
// a selector-pseudo containing the extended selector (`:not(.c) {@extend .c}`)
// would otherwise recurse without bound through `extend_pseudo` → `extend_list`
// → ... → `extend_pseudo`. dart-sass bounds this implicitly via its
// target-tracking; a small fixed depth covers every real case while keeping the
// engine total.
const MAX_PSEUDO_DEPTH: usize = 8;
thread_local! {
    static PSEUDO_DEPTH: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

fn extend_pseudo(text: &str, extensions: &[Extension]) -> Option<Vec<Simple>> {
    let parts = parse_pseudo_parts(text)?;
    if !is_selector_pseudo(&parts.name) {
        return None;
    }
    if PSEUDO_DEPTH.with(|d| d.get()) >= MAX_PSEUDO_DEPTH {
        return None;
    }
    let original = parse_list(&parts.arg)?;
    PSEUDO_DEPTH.with(|d| d.set(d.get() + 1));
    let extended = extend_list(&original, extensions);
    PSEUDO_DEPTH.with(|d| d.set(d.get() - 1));
    // Nothing changed.
    if extended.len() == original.len()
        && extended
            .iter()
            .zip(original.iter())
            .all(|(a, b)| a.render() == b.render())
    {
        return None;
    }

    // For `:not`, drop complex selectors from the extension result unless the
    // original argument itself contained a complex selector (otherwise we'd
    // produce a `:not()` no browser can parse). We can keep them if every
    // extension result is complex, since then nothing already-working breaks.
    let original_has_complex = original.iter().any(|c| c.components.len() > 1);
    let mut complexes: Vec<Complex> =
        if parts.name == "not" && !original_has_complex && extended.iter().any(|c| c.components.len() == 1) {
            extended.into_iter().filter(|c| c.components.len() <= 1).collect()
        } else {
            extended
        };

    // Unwrap nested matching pseudos in single-compound results, mirroring
    // dart-sass: e.g. for `:not`, a result `:is(...)`/`:matches`/`:where` is
    // unwrapped to its inner selectors; for matchish pseudos, a result of the
    // same name+argument is likewise unwrapped.
    let mut unwrapped: Vec<Complex> = Vec::new();
    for complex in complexes.drain(..) {
        let inner = single_pseudo_inner(&complex, &parts.name);
        match inner {
            PseudoUnwrap::Keep => unwrapped.push(complex),
            PseudoUnwrap::Drop => {}
            PseudoUnwrap::Replace(list) => unwrapped.extend(list),
        }
    }
    let complexes = unwrapped;

    // `:not` with a single-complex original argument splits into separate
    // `:not()` simples merged into the surrounding compound.
    if parts.name == "not" && original.len() == 1 {
        let result: Vec<Simple> = complexes
            .into_iter()
            .map(|c| Simple::Pseudo(format!("{}({})", parts.head, c.render())))
            .collect();
        if result.is_empty() {
            return None;
        }
        return Some(result);
    }

    if complexes.is_empty() {
        return None;
    }
    let inner = complexes
        .iter()
        .map(|c| c.render())
        .collect::<Vec<_>>()
        .join(", ");
    Some(vec![Simple::Pseudo(format!("{}({})", parts.head, inner))])
}

enum PseudoUnwrap {
    /// Keep the complex as-is.
    Keep,
    /// Drop the complex (nested pseudo we can't safely unwrap).
    Drop,
    /// Replace with these inner complex selectors.
    Replace(Vec<Complex>),
}

/// For a single-compound complex whose sole simple is a nested selector-pseudo,
/// decide how `_extendPseudo` should treat it relative to the outer pseudo
/// `outer_name`.
fn single_pseudo_inner(complex: &Complex, outer_name: &str) -> PseudoUnwrap {
    if complex.components.len() != 1 {
        return PseudoUnwrap::Keep;
    }
    let compound = &complex.components[0].compound;
    if compound.simples.len() != 1 {
        return PseudoUnwrap::Keep;
    }
    let Simple::Pseudo(text) = &compound.simples[0] else {
        return PseudoUnwrap::Keep;
    };
    let Some(inner) = parse_pseudo_parts(text) else {
        return PseudoUnwrap::Keep;
    };
    let Some(inner_list) = parse_list(&inner.arg) else {
        return PseudoUnwrap::Keep;
    };
    match outer_name {
        "not" => {
            // `:not(:is(...))` etc. unwraps; other nested pseudos can't be
            // expanded (each layer adds semantics) so the selector is dropped.
            if matches!(inner.name.as_str(), "is" | "matches" | "where") {
                PseudoUnwrap::Replace(inner_list)
            } else {
                PseudoUnwrap::Drop
            }
        }
        "is" | "matches" | "where" | "any" | "current" | "nth-child" | "nth-last-child" => {
            if inner.name == outer_name {
                PseudoUnwrap::Replace(inner_list)
            } else {
                PseudoUnwrap::Drop
            }
        }
        _ => PseudoUnwrap::Keep,
    }
}

/// Expand selector-pseudo arguments inside a compound in place, returning the
/// effective compound. `:not()` (single-complex arg) contributes extra `:not()`
/// simples merged at the pseudo's position; other matchish pseudos rewrite their
/// argument list. Non-pseudo simples and argument-less pseudos pass through.
fn expand_pseudos_in_compound(compound: &Compound, extensions: &[Extension]) -> Compound {
    let mut simples: Vec<Simple> = Vec::new();
    // A `:not()` expanded from one simple may collide with another `:not()`
    // already present in the compound (e.g. `:not(.c)` extends `.c`→`.b` while a
    // sibling `:not(.b)` is also present). Dedup pseudo simples so the expansion
    // is idempotent and the fixpoint terminates.
    for s in &compound.simples {
        match s {
            Simple::Pseudo(text) => {
                let replacement = match extend_pseudo(text, extensions) {
                    Some(r) => r,
                    None => vec![s.clone()],
                };
                for r in replacement {
                    if !simples.contains(&r) {
                        simples.push(r);
                    }
                }
            }
            other => simples.push(other.clone()),
        }
    }
    Compound { simples }
}

fn extend_component(comp: &ComplexComponent, extensions: &[Extension]) -> Vec<Vec<ComplexComponent>> {
    // First, extend any selector-pseudo arguments (`:not(...)`, `:is(...)`,
    // etc.) in place, producing an "effective" compound. For `:not()` with a
    // single-complex argument this *adds* simples to the compound (dart-sass
    // `_extendPseudo` merges them rather than creating an alternative);
    // matchish pseudos (`:is`/`:matches`/`:where`/...) rewrite their argument
    // list in place. The resulting compound is then run through the normal
    // per-simple extension below.
    let effective = expand_pseudos_in_compound(&comp.compound, extensions);
    let comp = ComplexComponent {
        combinator: comp.combinator,
        compound: effective,
    };
    let comp = &comp;
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

    // Cartesian product of per-simple choices (path outer, option inner) so the
    // first simple's choice varies slowest — matching dart-sass's observed
    // output order for within-compound extension.
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
        for seq in build_extended_compound(comp, simples, path) {
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
            } else if extender.components.len() == 1 {
                // A single multi-simple compound extender (e.g. `%y::fblthp`):
                // expand any of its simples that are themselves extension
                // targets, unifying each chain's trailing compound back into the
                // extender's compound. This handles transitive extends through a
                // multi-simple extender (dart-sass resolves these via its
                // extension graph).
                stack.push(target.clone());
                out.extend(expand_compound_extender(
                    &extender.components[0].compound,
                    extensions,
                    stack,
                ));
                stack.pop();
            }
        }
    }
    out
}

/// Expand a single-component, multi-simple extender compound by transitively
/// extending each of its simple selectors. For every simple that is itself an
/// extension target, the simple is replaced by each of its (transitively
/// collected) extenders, unifying the extender's trailing compound back into the
/// remaining simples. Returns the extra complex selectors so produced (the
/// original compound is emitted by the caller).
fn expand_compound_extender(
    compound: &Compound,
    extensions: &[Extension],
    stack: &mut Vec<Simple>,
) -> Vec<Complex> {
    let mut out: Vec<Complex> = Vec::new();
    let simples = &compound.simples;
    for (i, simple) in simples.iter().enumerate() {
        for inner_extender in collect_extenders(simple, extensions, stack) {
            // Only fold in atomic chains: a single-component, single-simple
            // inner extender (e.g. `%y` -> `a`). Folding in multi-simple
            // extenders here would re-expand compounds that are themselves
            // targets (a self-recursive `.a.mod1 {@extend .a}` family) and blow
            // up combinatorially; dart-sass resolves those through its full
            // extension graph + trimming, not this localized fold.
            if inner_extender.components.len() != 1 {
                continue;
            }
            let inner_compound = &inner_extender.components[0].compound;
            if inner_compound.simples.len() != 1 {
                continue;
            }
            // The remaining simples (all but the one being replaced), unified
            // with the inner extender's single simple.
            let remaining: Vec<Simple> = simples
                .iter()
                .enumerate()
                .filter(|(j, _)| *j != i)
                .map(|(_, s)| s.clone())
                .collect();
            if let Some(unified) = unify_compounds(&remaining, &inner_compound.simples) {
                out.push(Complex {
                    components: vec![ComplexComponent {
                        combinator: None,
                        compound: Compound { simples: unified },
                    }],
                });
            }
        }
    }
    out
}

/// Build the extended component sequences for one within-compound product path.
/// `path[i]` is `None` to keep `simples[i]`, or `Some(extender)` to replace it.
/// The self-kept simples form an "original" compound that is unified together
/// with every chosen extender via dart-sass `_unifyExtenders`/`unifyComplex`,
/// which weaves any multi-component extenders (including several at once). The
/// original component's incoming combinator is attached to each result's first
/// component. May return several sequences (one per woven unification).
fn build_extended_compound(
    comp: &ComplexComponent,
    simples: &[Simple],
    path: &[&Option<Complex>],
) -> Vec<Vec<ComplexComponent>> {
    // Self-kept simples (originals not being extended) and the chosen extenders.
    let mut base: Vec<Simple> = Vec::new();
    let mut extenders: Vec<&Complex> = Vec::new();
    for (i, choice) in path.iter().enumerate() {
        match choice {
            None => base.push(simples[i].clone()),
            Some(ext) => extenders.push(ext),
        }
    }

    // `_unifyExtenders`: the self-kept base compound (if any) is an "original"
    // selector unified first, then each extender complex.
    let mut to_unify: Vec<Complex> = Vec::new();
    if !base.is_empty() {
        to_unify.push(Complex {
            components: vec![ComplexComponent {
                combinator: None,
                compound: Compound {
                    simples: base.clone(),
                },
            }],
        });
    }
    for ext in &extenders {
        to_unify.push((*ext).clone());
    }

    let Some(unified) = unify_complex_multi(&to_unify) else {
        return Vec::new();
    };

    // Attach the original component's incoming combinator to the first component
    // of each unified result.
    unified
        .into_iter()
        .filter_map(|complex| {
            let mut components = complex.components;
            let first = components.first_mut()?;
            first.combinator = comp.combinator;
            Some(components)
        })
        .collect()
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
    // A `:host`/`:host-context` pseudo can't share its compound with an
    // incompatible simple (a class, type, universal, ordinary pseudo-class, …),
    // so such a pair can't unify (dart-sass).
    if host_unify_invalid(base, extra) {
        return None;
    }
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
            // Two distinct ids can never share a compound: an id won't unify
            // into a compound that already holds a different id (dart-sass
            // IDSelector.unify), so the whole unification fails.
            if let Simple::Id(id) = this {
                if compound
                    .iter()
                    .any(|s| matches!(s, Simple::Id(other) if other != id))
                {
                    return None;
                }
            }
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
/// The lowercased base name of a pseudo selector (`:host(.c)` → `"host"`), or
/// `None` for a non-pseudo simple.
fn pseudo_base(s: &Simple) -> Option<String> {
    let Simple::Pseudo(text) = s else {
        return None;
    };
    let name = text.trim_start_matches(':');
    Some(name.split(['(', ' ']).next().unwrap_or(name).to_ascii_lowercase())
}

/// Whether `s` is a `:host` / `:host-context` pseudo.
fn is_host_pseudo(s: &Simple) -> bool {
    matches!(pseudo_base(s).as_deref(), Some("host" | "host-context"))
}

/// Whether a simple selector may share a compound with a `:host` /
/// `:host-context` pseudo: only other host pseudos, the selector-list pseudos
/// (`:is`/`:where`/`:matches`/`:any`/`:-*-any`), or pseudo-elements — never a
/// type/class/id/universal/attribute or an ordinary pseudo-class.
fn host_compatible(s: &Simple) -> bool {
    is_host_pseudo(s)
        || is_pseudo_element(s)
        || matches!(
            pseudo_base(s).as_deref(),
            Some("is" | "where" | "matches" | "any" | "-moz-any" | "-webkit-any")
        )
}

/// Whether unifying `base` and `extra` would put a `:host`/`:host-context`
/// pseudo in a compound with a simple it can't combine with (checked across
/// both inputs, before a universal selector is dropped).
fn host_unify_invalid(base: &[Simple], extra: &[Simple]) -> bool {
    let all = || base.iter().chain(extra);
    all().any(is_host_pseudo) && all().any(|s| !host_compatible(s))
}

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

// ---- public helpers for the `sass:selector` builtin family -------------
//
// These are thin, additive re-exports of the engine internals above. They add
// no new behavior to the `@extend` directive path; they only expose the
// algorithms (superselector test, compound/complex unification, parent
// weaving) to `crate::builtins::selector`.

/// Whether selector list `sup` is a superselector of `sub`: every complex in
/// `sub` must be matched by some complex in `sup` (dart-sass `listIsSuperselector`).
pub(crate) fn list_is_superselector(sup: &[Complex], sub: &[Complex]) -> bool {
    sub.iter()
        .all(|c2| sup.iter().any(|c1| complex_is_superselector(c1, c2)))
}

/// Unify two complex selectors into the (possibly several) complex selectors
/// they jointly match, or `None` if their trailing compounds can't unify
/// (dart-sass `unifyComplex` for the two-selector case). The parents are woven
/// with full combinator support via the trailing-combinator weave.
pub(crate) fn unify_complex(c1: &Complex, c2: &Complex) -> Option<Vec<Complex>> {
    // dart-sass tracks a complex selector's leading combinator (e.g. `> .c`)
    // separately from its components. It is preserved in the unified result;
    // two *different* leading combinators can't unify (`> .c` and `+ .d`).
    let lc1 = c1.components.first().and_then(|c| c.combinator);
    let lc2 = c2.components.first().and_then(|c| c.combinator);
    let leading = match (lc1, lc2) {
        (Some(a), Some(b)) if a != b => return None,
        (a, b) => a.or(b),
    };
    let t1 = to_trailing(&c1.components);
    let t2 = to_trailing(&c2.components);
    let (last1, parents1) = t1.split_last()?;
    let (last2, parents2) = t2.split_last()?;
    // The base (final) compounds must unify; the trailing components in our
    // model never carry combinators, so the base has none either.
    let unified = unify_compounds(&last1.compound.simples, &last2.compound.simples)?;
    let base = TComp {
        compound: Compound { simples: unified },
        combinators: Vec::new(),
    };
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for mut woven in weave_parents_trailing(parents1, parents2) {
        woven.push(base.clone());
        let mut components = from_trailing(&woven);
        // Re-attach the preserved leading combinator to the very first
        // component (dart-sass's `leadingCombinators`).
        if let (Some(comb), Some(first)) = (leading, components.first_mut()) {
            first.combinator = Some(comb);
        }
        let complex = Complex { components };
        if seen.insert(complex.render()) {
            out.push(complex);
        }
    }
    if out.is_empty() {
        return None;
    }
    Some(out)
}

/// Unify a list of complex selectors into the complex selectors matched by all
/// of them (dart-sass `unifyComplex(List<ComplexSelector>)`). All final
/// compounds are unified into a single base compound; the remaining parent
/// components are woven together. Returns `None` if any pair can't unify.
fn unify_complex_multi(complexes: &[Complex]) -> Option<Vec<Complex>> {
    if complexes.is_empty() {
        return None;
    }
    if complexes.len() == 1 {
        return Some(complexes.to_vec());
    }

    // Accumulate the unified base (all final compounds unified together).
    let mut unified_base: Option<Vec<Simple>> = None;
    for complex in complexes {
        let base = complex.components.last()?;
        match &mut unified_base {
            None => unified_base = Some(base.compound.simples.clone()),
            Some(acc) => {
                let mut next = acc.clone();
                for simple in &base.compound.simples {
                    next = simple_unify(simple, &next)?;
                }
                *acc = next;
            }
        }
    }
    let unified_base = unified_base?;

    // The parents of each multi-component complex (all but the last component).
    // Build them in the trailing-combinator form first so the combinator that
    // joined the (removed) last component stays attached to the new last parent,
    // then convert back to leading form for the weave.
    let mut without_bases: Vec<Vec<TComp>> = Vec::new();
    for complex in complexes {
        if complex.components.len() > 1 {
            let trailing = to_trailing(&complex.components);
            without_bases.push(trailing[..trailing.len() - 1].to_vec());
        }
    }

    let base_tcomp = TComp {
        compound: Compound {
            simples: unified_base,
        },
        combinators: Vec::new(),
    };

    // `weave(withoutBases.isEmpty ? [base] : [...exceptLast, last.concat(base)])`.
    // `concatenate` appends the base as a descendant onto the last parents
    // complex; the last parent keeps its own trailing combinator.
    let path: Vec<Complex> = if without_bases.is_empty() {
        vec![Complex {
            components: from_trailing(&[base_tcomp]),
        }]
    } else {
        let mut path = Vec::new();
        let last_idx = without_bases.len() - 1;
        for (i, parents) in without_bases.iter().enumerate() {
            if i == last_idx {
                let mut concatenated = parents.clone();
                concatenated.push(base_tcomp.clone());
                path.push(Complex {
                    components: from_trailing(&concatenated),
                });
            } else {
                path.push(Complex {
                    components: from_trailing(parents),
                });
            }
        }
        path
    };

    let woven = weave(&path);
    if woven.is_empty() {
        None
    } else {
        Some(woven)
    }
}

/// Unify two selector lists: the cartesian product of `unify_complex` over each
/// pair of complex selectors, dropping pairs that don't unify (dart-sass
/// `SelectorList.unify`). Returns `None` if nothing unifies.
pub(crate) fn unify_lists(list1: &[Complex], list2: &[Complex]) -> Option<Vec<Complex>> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for c1 in list1 {
        for c2 in list2 {
            if let Some(unified) = unify_complex(c1, c2) {
                for c in unified {
                    if seen.insert(c.render()) {
                        out.push(c);
                    }
                }
            }
        }
    }
    if out.is_empty() {
        return None;
    }
    Some(out)
}

/// Render a complex selector as the items of a Sass space-separated list:
/// compound strings interleaved with combinator strings (`>`/`+`/`~`), matching
/// dart-sass's `ComplexSelector.asSassList`.
pub(crate) fn complex_to_list_parts(c: &Complex) -> Vec<String> {
    let mut parts = Vec::new();
    for comp in &c.components {
        if let Some(comb) = comp.combinator {
            parts.push(comb.as_str().to_string());
        }
        parts.push(comp.compound.render());
    }
    parts
}

/// Parse a single compound selector (no combinators, no commas). Returns the
/// list of simple-selector strings, or `None` if the text isn't a single valid
/// compound (dart-sass `simple-selectors` parses a `CompoundSelector`).
pub(crate) fn parse_compound_simples(s: &str) -> Option<Vec<String>> {
    let chars: Vec<char> = s.trim().chars().collect();
    let mut i = 0;
    let compound = parse_compound(&chars, &mut i)?;
    skip_ws(&chars, &mut i);
    if i != chars.len() {
        return None; // trailing combinator / second compound / garbage
    }
    Some(compound.simples.iter().map(Simple::render).collect())
}

/// Extend a selector list against one or more *compound* targets (used by the
/// `selector-extend` / `selector-replace` builtins, which — unlike the
/// `@extend` directive — allow a multi-simple compound extendee, and also a
/// list of such compounds). Wherever a `target` compound is a subselector of a
/// component's compound, the `extender` selectors are woven in. With `replace`
/// true the matched original is dropped (`selector-replace`); otherwise it is
/// kept (`selector-extend`). This mirrors dart-sass `_extendComplex`/
/// `_extendCompound`: every target is applied simultaneously, and freshly
/// produced selectors are re-extended to a fixpoint so a selector matching two
/// targets collapses correctly (e.g. extending `c.d` by `c, .d` with `.e`).
pub(crate) fn extend_compound_target(
    selectors: &[Complex],
    targets: &[Compound],
    extenders: &[Complex],
    replace: bool,
) -> Vec<Complex> {
    // Originals are never trimmed away (dart-sass keeps the input selectors so a
    // rule still matches what it always matched). In replace mode the matched
    // originals are dropped before this point, so the set is the surviving ones.
    let mut originals: HashSet<String> = HashSet::new();
    if !replace {
        for complex in selectors {
            originals.insert(complex.render());
        }
    }

    let mut result: Vec<Complex> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    // Each input complex is extended independently and its results emitted
    // consecutively (dart-sass groups a selector's extensions right after it),
    // so the per-complex fixpoint stays local to its input.
    for complex in selectors {
        // Worklist so that a selector produced by one target can itself be
        // re-extended by another target (dart-sass applies all extensions
        // simultaneously). Bounded to guarantee termination.
        let mut queue: std::collections::VecDeque<Complex> = std::collections::VecDeque::new();
        queue.push_back(complex.clone());
        let mut local_seen: HashSet<String> = HashSet::new();
        let mut iterations = 0usize;
        while let Some(cur) = queue.pop_front() {
            iterations += 1;
            if iterations > 100_000 || result.len() > 100_000 {
                break;
            }
            let cur_rendered = cur.render();
            let extended = extend_complex_compound(&cur, targets, extenders, replace);
            // Whether this complex produced anything other than itself: only
            // then do we re-feed the new selectors (re-feeding an unchanged
            // selector would loop forever).
            let is_self_only = extended.len() == 1
                && extended.first().map(Complex::render).as_deref() == Some(cur_rendered.as_str());
            for c in extended {
                let rendered = c.render();
                if !is_self_only && rendered != cur_rendered && local_seen.insert(rendered.clone()) {
                    queue.push_back(c.clone());
                }
                if seen.insert(rendered) {
                    result.push(c);
                }
            }
        }
    }
    // Drop selectors made redundant by a superselector elsewhere in the list
    // (dart-sass `_trim`), keeping originals.
    trim(result, &originals)
}

/// Extend one complex selector against one or more compound targets: compute
/// each component's options (original first unless replaced), take the Cartesian
/// product, and `weave` each path — the same shape as `extend_complex`.
fn extend_complex_compound(
    complex: &Complex,
    targets: &[Compound],
    extenders: &[Complex],
    replace: bool,
) -> Vec<Complex> {
    let mut per_component: Vec<Vec<Complex>> = Vec::new();
    let mut any_extended = false;
    for comp in &complex.components {
        let opts = extend_component_compound(comp, targets, extenders, replace);
        if opts.len() != 1 || opts.first().map(|c| &c.components) != Some(&vec![comp.clone()]) {
            any_extended = true;
        }
        per_component.push(opts);
    }
    if !any_extended {
        return vec![complex.clone()];
    }

    // dart-sass `paths`: for each component's options, the *option* is the outer
    // loop and the accumulated paths the inner loop, so the first component's
    // choice varies fastest in the output order.
    let mut combos: Vec<Vec<Complex>> = vec![Vec::new()];
    for opts in &per_component {
        let mut next: Vec<Vec<Complex>> = Vec::new();
        for opt in opts {
            for combo in &combos {
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

/// Options for one component against a compound target. The original component
/// is option 0 in extend mode (and in replace mode when it does not match);
/// each matching extender contributes the woven replacement.
fn extend_component_compound(
    comp: &ComplexComponent,
    targets: &[Compound],
    extenders: &[Complex],
    replace: bool,
) -> Vec<Complex> {
    // The original component (as a one-component complex).
    let original = Complex {
        components: vec![comp.clone()],
    };
    // Targets whose compound is a subselector of this component's compound: each
    // matching target contributes its own woven extensions (dart-sass applies
    // every extension simultaneously).
    let matching: Vec<&Compound> = targets
        .iter()
        .filter(|t| compound_is_superselector(t, &comp.compound, &[]))
        .collect();
    if matching.is_empty() {
        return vec![original];
    }

    let mut options: Vec<Complex> = Vec::new();
    if !replace {
        options.push(original);
    }
    let mut seen: HashSet<String> = HashSet::new();
    for target in matching {
        // The simples of this compound not covered by this target.
        let remaining: Vec<Simple> = comp
            .compound
            .simples
            .iter()
            .filter(|s| !target.simples.contains(s))
            .cloned()
            .collect();
        for ext in extenders {
            let Some(last) = ext.components.last() else {
                continue;
            };
            let Some(unified) = unify_compounds(&remaining, &last.compound.simples) else {
                continue;
            };
            // Build the extended component sequence: the extender's leading
            // components, then the unified trailing compound (carrying this
            // component's incoming combinator on its first element).
            let mut components: Vec<ComplexComponent> = Vec::new();
            let lead = &ext.components[..ext.components.len() - 1];
            for (k, l) in lead.iter().enumerate() {
                let combinator = if k == 0 { comp.combinator } else { l.combinator };
                components.push(ComplexComponent {
                    combinator,
                    compound: l.compound.clone(),
                });
            }
            let trail_combinator = if lead.is_empty() {
                comp.combinator
            } else {
                last.combinator
            };
            components.push(ComplexComponent {
                combinator: trail_combinator,
                compound: Compound { simples: unified },
            });
            let candidate = Complex { components };
            let key = candidate.render();
            if seen.insert(key) {
                options.push(candidate);
            }
        }
    }
    if options.is_empty() {
        // Replace mode with no successful unification: keep the original so the
        // selector isn't silently dropped (dart-sass leaves an unmatched
        // component intact).
        options.push(Complex {
            components: vec![comp.clone()],
        });
    }
    options
}
