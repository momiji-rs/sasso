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
                // namespace: `*|name` is unusual; handle `*` then optional `|`.
                if *i < chars.len() && chars[*i] == '|' && chars.get(*i + 1) != Some(&'=') {
                    // `*|type`
                    *i += 1;
                    let rest = read_type_after_ns(chars, i)?;
                    simples.push(rest);
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

    // Drop complex selectors that still contain a placeholder.
    let kept: Vec<&Complex> = result.iter().filter(|c| !c.has_placeholder()).collect();
    let all_placeholders = kept.is_empty();
    let selectors: Vec<String> = kept.iter().map(|c| c.render()).collect();
    ExtendResult {
        selectors,
        all_placeholders,
    }
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

    // Cartesian product of the per-component option lists, concatenated.
    let mut combos: Vec<Vec<ComplexComponent>> = vec![Vec::new()];
    for opts in &per_component {
        let mut next: Vec<Vec<ComplexComponent>> = Vec::new();
        for combo in &combos {
            for opt in opts {
                let mut c = combo.clone();
                c.extend(opt.iter().cloned());
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
    for components in combos {
        let c = Complex { components };
        let r = c.render();
        if seen.insert(r) {
            out.push(c);
        }
    }
    out
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
    let mut found = false;
    for ext in extensions {
        let Some(t) = &ext.target else { continue };
        if t != target {
            continue;
        }
        ext.matched.set(true);
        found = true;
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
    let _ = found;
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

/// Unify two compound selectors into one, returning `None` if they can't be
/// combined into a single valid compound (e.g. two different type selectors,
/// two different ids). The result orders type/universal first, then the
/// existing simples, then the new ones (deduplicated), matching dart-sass's
/// rendered output for the common cases.
fn unify_compounds(base: &[Simple], extra: &[Simple]) -> Option<Vec<Simple>> {
    // Collect type/universal selectors from both; they must unify.
    let mut type_sel: Option<Simple> = None;
    let mut id_sel: Option<Simple> = None;
    let mut rest: Vec<Simple> = Vec::new();

    let push = |s: &Simple,
                type_sel: &mut Option<Simple>,
                id_sel: &mut Option<Simple>,
                rest: &mut Vec<Simple>|
     -> Option<()> {
        match s {
            Simple::Type(_) | Simple::Universal { .. } => match type_sel {
                None => *type_sel = Some(s.clone()),
                Some(existing) => {
                    let merged = unify_type(existing, s)?;
                    *type_sel = Some(merged);
                }
            },
            Simple::Id(_) => match id_sel {
                None => *id_sel = Some(s.clone()),
                Some(existing) => {
                    if existing != s {
                        return None; // two different ids can't unify
                    }
                }
            },
            _ => {
                if !rest.contains(s) {
                    rest.push(s.clone());
                }
            }
        }
        Some(())
    };

    for s in base {
        push(s, &mut type_sel, &mut id_sel, &mut rest)?;
    }
    for s in extra {
        push(s, &mut type_sel, &mut id_sel, &mut rest)?;
    }

    let mut out = Vec::new();
    if let Some(t) = type_sel {
        // A bare universal `*` is dropped when other simples are present.
        if !(matches!(t, Simple::Universal { ns: None }) && (!rest.is_empty() || id_sel.is_some())) {
            out.push(t);
        }
    }
    out.extend(rest);
    if let Some(id) = id_sel {
        out.push(id);
    }
    if out.is_empty() {
        return None;
    }
    Some(out)
}

/// Unify two type/universal selectors. `*` unifies with anything (yielding the
/// more specific). Two distinct named types don't unify.
fn unify_type(a: &Simple, b: &Simple) -> Option<Simple> {
    match (a, b) {
        (Simple::Universal { ns: None }, other) | (other, Simple::Universal { ns: None }) => {
            Some(other.clone())
        }
        (Simple::Type(x), Simple::Type(y)) if x == y => Some(a.clone()),
        (Simple::Universal { ns: x }, Simple::Universal { ns: y }) if x == y => Some(a.clone()),
        _ => None,
    }
}
