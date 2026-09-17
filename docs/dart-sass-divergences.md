# Known divergences from dart-sass

sasso aims for byte-for-byte parity with dart-sass. This file lists every
divergence we currently know about, so that "99.9% compatible" has a checkable
meaning rather than being a number without a denominator.

**Reference version: dart-sass 1.104.1.** Every row below was produced by
running both compilers on the same input. Where a row says "dart", that is the
observed output of 1.104.1 — not a reading of the specification.

Last verified: 2026-09-17. The six that need design work or affect compiled
output are tracked as issues
([#61](https://github.com/momiji-rs/sasso/issues/61),
[#62](https://github.com/momiji-rs/sasso/issues/62),
[#63](https://github.com/momiji-rs/sasso/issues/63),
[#64](https://github.com/momiji-rs/sasso/issues/64),
[#65](https://github.com/momiji-rs/sasso/issues/65),
[#66](https://github.com/momiji-rs/sasso/issues/66)); the rest live here, and
are fixed as they come up.

## Where we stand

| measurement | result |
|---|---|
| [sass-spec](https://github.com/sass/sass-spec) suite | **14107 / 14266 passing (98.89%)**, ratcheted in CI |
| Lichess (lila) corpus, 148 entry points, `--style=expanded` | **147 / 148 byte-identical** |
| the same corpus, `--style=compressed` | **147 / 148 byte-identical** |
| the same corpus, source maps (`--embed-sources`) | **148 / 148 byte-identical** |

The one differing file in both styles is `learn.css`; see
[Deliberately not followed](#deliberately-not-followed).

## How to reproduce a row

```sh
printf '@use "sass:color";\n.a { b: %s; }\n' '<expression>' > t.scss
sass  --no-source-map t.scss   # dart-sass 1.104.1
sasso --no-source-map t.scss
```

`tests/parity.rs` runs every expectation in this repo against a real dart-sass
binary when `SASSO_PARITY=1 SASS_BIN=<path>` is set, so an expectation cannot
silently drift away from the reference.

---

## 1. Compiled output

These change the bytes a build emits. All of them are absent from the Lichess
corpus, which is why it still measures 147/148.

### 1.1 An attribute selector's quotes are not re-chosen — and can produce invalid CSS ([#61](https://github.com/momiji-rs/sasso/issues/61))

dart decodes an attribute value's escapes and re-quotes it with whichever quote
character needs fewer of them. sasso keeps the written form, and for one input
shape that is not merely cosmetic:

```scss
[a='b"c'] { c: d }     // dart: [a='b"c']     sasso: [a="b"c"]   ← invalid CSS
[a="b\"c"] { c: d }    // dart: [a='b"c']     sasso: [a="b\"c"]  ← cosmetic
```

The first row silently corrupts the selector: a browser reads `[a="b"` and then
garbage. `[a="b'c"]`, `[a="b c"]`, `[a="b"]` and `[a='b']` all already match.

### 1.2 An interpolated at-rule name loses a space when compressed

```scss
@#{"media"} (a: 1) { a { b: c } }
// dart:  @media (a: 1){a{b:c}}
// sasso: @media(a: 1){a{b:c}}
```

The compressed no-space rule is keyed on the at-rule *name*, but in dart an
interpolated name produces a generic node that never reaches the media-rule
writer. A literal `@media` and an interpolated one resolve to the same string,
so the distinction has to be carried as a field on the node rather than
inferred from its name.

### 1.3 A literal property inside a custom `@function --foo()` is not evaluated

```scss
@function --foo() { q: 1 + 2; }
// dart:  q: 3;
// sasso: q: 1 + 2;
```

dart parses the body of a custom-property-named function as SassScript.

### 1.4 A degenerate calculation inside `@supports` is evaluated instead of preserved

```scss
@supports (a: lab(calc(infinity) 1 2)) { a { b: c } }
// dart:  @supports (a: lab(calc(infinity) 1 2))
// sasso: @supports (a: lab(100% 1 2))
```

### 1.5 `meta.call()` with an unknown name compiles instead of erroring ([#63](https://github.com/momiji-rs/sasso/issues/63))

```scss
@use "sass:meta";
.a { b: meta.call("nope"); }
// dart:  Error: () isn't a valid CSS value.
// sasso: emits `nope()` into the CSS, no error
```

## 2. Values and built-in semantics

A wrong value, or a missing error, rather than a wrong message.

| input | dart-sass 1.104.1 | sasso |
|---|---|---|
| `rgb(1, 2, 3, $nope: 4)` ([#62](https://github.com/momiji-rs/sasso/issues/62)) | `No parameter named $nope.` | returns `rgb(1, 2, 3)` |
| `color.change(hsl(240 none 50%), $alpha: 0.5)` | keeps the missing channel | fills it in |
| `meta.inspect(33.333333333333336%)` | `33.333333333333336%` | `33.3333333333%` |
| `meta.inspect(color.hwb(0, calc(-infinity * 1%), 40%, 0.5))` | `hwb(0 calc(-infinity)% 40% / 0.5)` | `hwb(0 -Infinity% 40% / 0.5)` |
| `color.change(red, $red: calc(NaN))` | `hsl(0, 0%, 0%)` | `black` |
| `color.change(red, $saturation: calc(infinity))` | `hsl(0, 0%, 0%)` | `hsl(0, calc(infinity * 1%), 50%)` |
| comma-form `color.hwb()` diagnostics | three message differences vs the space form | — |

The first row has the widest blast radius: an unknown **named** argument is
silently ignored by every built-in, so a typo in an argument name compiles
instead of erroring. Fixing it needs a required-vs-optional split for all 48
built-in parameter lists, because dart's precedence is "a required parameter
still unbound wins with `Missing argument`" while an unbound *optional*
parameter does **not** suppress the unknown-name error.

The `meta.inspect` precision row is wide in a different way: it is every
fractional number under `inspect`, and `inspect` output appears throughout the
sass-spec expectations.

## 3. Diagnostics

Message text or span geometry. The compiler accepts and rejects the same
programs; only what it prints differs.

| input | dart-sass 1.104.1 | sasso |
|---|---|---|
| `color.opacify(c, 0.1)` and the other eight removed `sass:color` members ([#65](https://github.com/momiji-rs/sasso/issues/65)) | names the member, recommends a replacement computed from the call's own arguments, links the docs | `Undefined function.` |
| `red(#abcdef, 1)` | the arity error alone | the arity error **plus** a `[global-builtin]` deprecation |
| `string.index("abc" "b", "x")` | `$string: ("abc" "b") is not a string.` | drops the parentheses |
| a stray `}` | `unmatched "}".` | `unexpected "}"` |
| `p > { &.x }` ([#66](https://github.com/momiji-rs/sasso/issues/66)) | draws two spans (`outer selector` / `parent selector`) | one span |
| `Missing argument $x.` where the parameter was written with surrounding space | takes the name from the parameter's own span text | normalises it |
| any `.sass` span crossing a CRLF line ending | correct | one byte short per CRLF |
| every builtin arity/missing-argument error ([#66](https://github.com/momiji-rs/sasso/issues/66)) | two frames: the invocation, then the declaration | one frame |

The last two rows share a cause worth naming: sasso has no dual-span diagnostic
renderer, so any message dart draws with two frames is drawn with one. dart's
`p > { &.x }` error, for instance:

```
Error: Selector "p >" can't be used as a parent in a compound selector.
  ╷
1 │ p > { &.x { a: b } }
  │ ^^^ outer selector
  │       ━ parent selector
  ╵
```

## 4. Module member enumeration ([#64](https://github.com/momiji-rs/sasso/issues/64))

`meta.module-functions()`, `meta.module-mixins()` and `meta.module-variables()`
list members only for `sass:meta`; every other built-in module answers with an
empty map. sasso has no member *table* for a built-in module — the lookup is a
predicate, so it can answer "is `get` in `sass:map`?" but not "what is in
`sass:map`?". The same table is what dart's eager `Two forwarded modules both
define a function named length.` check needs, and dart returns these members in
**source** order rather than sorted.

## Deliberately not followed

- **`learn.css` in the Lichess corpus.** An extender that reaches a placeholder
  through two chains is listed twice by dart (`.w, .w`), and which duplicate
  survives depends on `ExtensionStore` bookkeeping order. We emit it once.
- **`color.alpha(red, 1)`** reports `Only 1 argument allowed, but 1 were
  passed.` in dart — two arguments reported as one, with the verb disagreeing
  with the count. It is an artifact of dart's overloaded declaration of
  `alpha()`. We report the real count. (The *wording* of that message is
  matched: `alpha()` does not say "positional" where other members do.)

## Version skew worth knowing

Lichess's own build bundles **dart-sass 1.100.0**, not 1.104.1. Nine files in
that corpus differ between those two dart versions before sasso is involved
(`rgb()` channel serialisation changed). Any comparison should state which dart
it was measured against.
