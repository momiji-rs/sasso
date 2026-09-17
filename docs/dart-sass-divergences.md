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
| [sass-spec](https://github.com/sass/sass-spec) suite | **14,107 / 14,258 attempted (98.94%)**, ratcheted in CI — 98.89% of all 14,266, the other 8 being cases tagged `:todo` for dart-sass itself |
| Lichess (lila) corpus, 148 entry points, `--style=expanded` | **147 / 148 byte-identical** |
| the same corpus, `--style=compressed` | **147 / 148 byte-identical** |
| the same corpus, source maps (`--embed-sources`) | **148 / 148 byte-identical** |

The one differing file in both styles is `learn.css`; see
[Deliberately not followed](#deliberately-not-followed).

## How to reproduce a row

```sh
# Pin the oracle: a bare `sass` on PATH is whatever is installed, and a future
# release would silently change every comparison below.
npm install sass@1.104.1 --prefix /tmp/dart1104
DART=/tmp/dart1104/node_modules/.bin/sass

printf '@use "sass:color";\n.a { b: %s; }\n' '<expression>' > t.scss
"$DART" --no-source-map t.scss
sasso   --no-source-map t.scss
```

`tests/parity.rs` runs every expectation in this repo against a real dart-sass
binary when `SASSO_PARITY=1 SASS_BIN=<path>` is set, so an expectation cannot
silently drift away from the reference.

---

## 1. Compiled output

These change the bytes a build emits. All of them are absent from the Lichess
corpus, which is why it still measures 147/148.

### 1.1 An attribute selector's quotes are not re-chosen — and can produce invalid CSS ([#61](https://github.com/momiji-rs/sasso/issues/61))

dart decodes an attribute value's escapes and then re-quotes it, choosing
whichever quote character needs fewer escapes — and dropping the quotes
entirely when the value is identifier-safe. sasso re-emits the value with
double quotes and copies the inner text through verbatim, so an unescaped `"`
inside a single-quoted value survives into the output and breaks it:

```scss
[a='b"c']   { c: d }   // dart: [a='b"c']   sasso: [a="b"c"]    ← invalid CSS
[a="b\"c"]  { c: d }   // dart: [a='b"c']   sasso: [a="b\"c"]   ← cosmetic
[a='b\'c']  { c: d }   // dart: [a="b'c"]   sasso: [a="b\'c"]   ← cosmetic
[a="b\\c"]  { c: d }   // dart: [a=b\\c]     sasso: [a="b\\c"]   ← cosmetic
```

The first row silently corrupts the selector: a browser reads `[a="b"` and then
garbage. `[a="b'c"]`, `[a="b c"]`, `[a="b"]` and `[a='plain']` already match.

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
| `meta.inspect(33.333333333333336%)` | `33.333333333333336%` | `33.3333333333%` |
| `meta.inspect(color.hwb(0, calc(-infinity * 1%), 40%, 0.5))` | `hwb(0 calc(-infinity)% 40% / 0.5)` | `hwb(0 -Infinity% 40% / 0.5)` |
| `color.change(red, $red: calc(NaN))` | `hsl(0, 0%, 0%)` | `black` |
| `color.change(red, $saturation: calc(infinity))` | `hsl(0, 0%, 0%)` | `hsl(0, calc(infinity * 1%), 50%)` |

The first row has the widest blast radius: an unknown **named** argument is
silently ignored by every built-in, so a typo in an argument name compiles
instead of erroring. Fixing it needs a required-vs-optional split for all 48
built-in parameter lists, because dart's precedence is "a required parameter
still unbound wins with `Missing argument`" while an unbound *optional*
parameter does **not** suppress the unknown-name error.

The `meta.inspect` precision row is wide in a different way: it is every
fractional number under `inspect`, and `inspect` output appears throughout the
sass-spec expectations.

### The comma form of `color.hwb()` reports three diagnostics differently

The space form matches; only the legacy comma form diverges, and it does so in
three distinct ways, so each is given its own input:

```scss
color.hwb((1 2), 10%, 20%)
// dart:  Expected hue channel to be a number, was (1 2).
// sasso: $channels: Expected hue channel to be a number, was (1 2).

color.hwb(0, 10%, 20%, (1 2))
// dart:  (1 2) is not a number.
// sasso: $alpha: (1 2) is not a number.

color.hwb(0, 10%)
// dart:  Only 1 argument allowed, but 2 were passed.
// sasso: $channels: The hwb color space has 3 channels but (0 / 10%) has 1.
```

The third is not a wording difference: with two arguments dart binds the call
to the *modern* single-`$channels` overload and reports its arity, where sasso
stays on the comma form and complains about the channel count.

## 3. Diagnostics

Message text or span geometry. The compiler accepts and rejects the same
programs; only what it prints differs.

| input | dart-sass 1.104.1 | sasso |
|---|---|---|
| `string.index("abc" "b", "x")` | `$string: ("abc" "b") is not a string.` | `$string: "abc" "b" is not a string.` |
| a stray `}` after a complete rule | `unmatched "}".` | `unexpected "}"` |
| `red(#abcdef, 1)` | `Only 1 argument allowed, but 2 were passed.` | the same error, preceded by a `[global-builtin]` deprecation warning |
| `@mixin m($x )` included with no argument | `Missing argument $x .` — dart takes the name from the parameter's own span text, which swallowed the trailing space | `Missing argument $x.` |

The three below are classes rather than single inputs, so each gets its own
example.

### The nine removed `sass:color` members ([#65](https://github.com/momiji-rs/sasso/issues/65))

dart names the member and computes a replacement from the call's own arguments;
sasso says only that the function is undefined:

```scss
@use "sass:color";
.a { b: color.opacify(rgba(1, 2, 3, 0.5), 0.1); }
```

```
dart-sass 1.104.1:
Error: The function opacify() isn't in the sass:color module.

Recommendation: color.adjust(rgba(1, 2, 3, 0.5), $alpha: 0.1)

More info: https://sass-lang.com/documentation/functions/color#opacify

sasso:
Error: Undefined function.
```

The recommendation differs per member and per call, so this one example does
not stand in for the rest: `opacify`/`fade-in` suggest `$alpha: <amount>`,
`transparentize`/`fade-out` `$alpha: -<amount>`, `lighten`/`darken`
`$lightness: ±<amount>`, `saturate`/`desaturate` `$saturation: ±<amount>`, and
`adjust-hue` `$hue: <degrees>`. [#65](https://github.com/momiji-rs/sasso/issues/65)
lists each with the exact shape, including the details that are easy to get
wrong (the negation is textual, and no unit conversion happens).

### Two-span messages ([#66](https://github.com/momiji-rs/sasso/issues/66))

sasso *does* have the two-frame renderer — a user-defined callable's arity error
draws the declaration and the invocation exactly as dart does. Two shapes do not
use it.

**A labelled second span on the same line.** dart marks the two halves of the
parent-selector error separately; sasso draws one unlabelled span:

```scss
p > { &.x { a: b } }
```

```
dart-sass 1.104.1:                          sasso:
Error: Selector "p >" can't be used …       Error: Selector "p >" can't be used …
  ╷                                           ╷
1 │ p > { &.x { a: b } }                     1 │ p > { &.x { a: b } }
  │ ^^^ outer selector                         │ ^^^
  │       ━ parent selector                    ╵
  ╵
```

**A declaration frame in another file, for a BUILT-IN.** dart shows where the
built-in is declared; sasso prints only the invocation, because it has no source
text for `sass:color` to point at:

```scss
@use "sass:color";
.a { b: color.lighten(#abcdef); }
```

```
dart-sass 1.104.1:
Error: Missing argument $amount.
  ┌──> t.scss
2 │ .a { b: color.lighten(#abcdef); }
  │         ^^^^^^^^^^^^^^^^^^^^^^ invocation
  ╵
  ┌──> sass:color
1 │ @function lighten($color, $amount) {
  │           ━━━━━━━━━━━━━━━━━━━━━━━━ declaration
  ╵

sasso: the first frame only.
```

The message text itself matches (#56, #58).

### A multi-line span in a CRLF `.sass` file is one column long

Every `.sass` span that crosses a line ending is one byte short per CRLF,
because the transpiler normalises `\r\n` before the spans are taken. It is only
visible where a span covers more than one line:

```sass
@mixin m($a, $b)
  x: $a

.a
  @include m(1,
    2,
    3)
```

Saved with CRLF endings, the invocation's closing marker lands a column late:

```
dart-sass 1.104.1:        sasso:
    │ └─── invocation         │ └────^ invocation
```

With LF endings the same file matches exactly.

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
