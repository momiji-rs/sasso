# Changelog

All notable changes to **sasso** are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

Conformance is tracked separately as a ratchet against the official
[sass-spec](https://github.com/sass/sass-spec) suite — see the
[Conformance](README.md#conformance) section for the current pass rate.

## [Unreleased]

### Added

- **dart-sass-compatible CLI.** `sasso` now takes the same arguments as
  `sass`, so a build script written for dart-sass runs unchanged (#24, the
  Lichess build spawns one process with ~150 `src:out` pairs):
  - the positional grammar is `<input> [output]` (a second positional is the
    output file; `--stdin [output]`; an input of `-` is standard input, and a
    `-:<out>` pair is standard input to a file alongside other pairs — dart's
    rules, including `Duplicate source` for a repeated one; a bare directory
    positional compiles in place, `sass dir` being `dir:dir`; `--` ends option
    parsing so a name starting with `-` can be an input or output), and
    `<in>:<out>` pairs — files or a whole directory tree (`scss/:css/`,
    partials skipped, `.sass` and `.css` inputs by exact lowercase suffix as
    in dart, a `.css` whose destination would be itself skipped as in dart;
    two spellings of one source — explicit or directory-expanded — coalesce
    to the later destination) — compile many stylesheets in one process;
  - **several inputs compile in parallel**, one worker per CPU (`-j/--jobs N`
    to cap it), with diagnostics and exit status reported in command-line
    order; `--stop-on-error` stops scheduling more files after a failure.
    The full Lichess corpus (148 entry points, source maps with embedded
    sources) builds in 0.79 s against dart-sass's 1.40 s on arm64 macOS;
  - `--[no-]source-map` (on by default when writing a file, like dart; the
    `.map` is written before the CSS that references it, and the map's `file`
    and the footer URL are percent-encoded like dart's),
    `--[no-]embed-source-map` (the map inlined as a `data:` URI, byte-exact
    to dart including its percent-encoding; absolute `file://` sources and no
    `file` field on stdout), `--[no-]embed-sources`, `--source-map-urls`; the
    combinations dart rejects are usage errors here too, with dart's wording;
  - `--[no-]error-css`: on a compile error, a file target receives dart's
    error stylesheet (the diagnostic as a comment plus a `body::before` that
    shows it in the browser), byte-exact to dart-sass; with it off, a stale
    output from an earlier build is removed instead, as dart does. Invalid
    UTF-8 input counts as a compile error here too. `--error-css` also
    prints the stylesheet to stdout;
  - `-q/--quiet` (no warnings), `--quiet-deps` (no compiler warnings from
    dependencies — files resolved through a load path, by how they were
    resolved rather than where they live; their `@warn` still prints),
    `--[no-]charset`, `-c/--[no-]color` (accepted; sasso never colors),
    `--no-css` (compile, report diagnostics, write nothing);
  - dart's exit codes: 64 for a usage error, 65 for a compile error, 66 for
    an unreadable input (`Error reading <path>: Cannot open file.`) or an
    output that cannot be written (invalid UTF-8 on stdin is a compile error
    with the same error-CSS handling as a file's — dart crashes there);
    missing output directories are created; a CSS file always ends in one
    newline, an empty stylesheet included (stdout gets nothing for empty
    output), as dart writes it; one unit's diagnostics are separated from the
    next by a blank line, as dart prints them.
- **`WarnEvent::path`**: the stylesheet a `@warn`/`@debug`/deprecation came
  from, as an identity rather than dart's short display form in `url` (a
  load-path file's basename): for a file the importer loaded, the importer's
  canonical URL (an absolute path with `FsImporter`); for the entry
  stylesheet, `Options::url` exactly as supplied. Lets an embedder tell a
  dependency from the entry, which is what `--quiet-deps` needs.
- **`FsImporter::dependencies()`** returns a shared `DependencySet` that fills
  in as the compile resolves imports: which files were reached through a load
  path (or loaded relatively from one that was) — dart-sass's "dependencies".
  Keyed by canonical URL, so it pairs with `WarnEvent::path`.
  **`Options::with_quiet_deps(set)`** (dart-sass `quietDeps`) drops
  deprecation warnings raised inside those files before they are counted, so
  they neither use up the five visible repetitions nor surface as "N
  repetitive deprecation warnings omitted". The record is per compilation:
  a compile scoped on the set starts it empty, and a nested compile (a warn
  handler running `compile` with the same importer) hands the enclosing
  record back when it ends, so an importer reused sequentially or nested
  cannot leak one compile's classification into another — concurrent
  compiles sharing one set are not supported (`DependencySet::clear` for
  embedders reading the set by hand). Provenance
  also follows the evaluator's `@import` cache: a file a dependency loads is
  a dependency even when its resolution was cached by an earlier load.

### Changed (dart-sass 1.104.x alignment)

- **The reference pins move to dart-sass 1.104.1** — the CI parity binary
  (`sass@1.103.1` → `sass@1.104.1`), the sass-spec pin (`4a9eea66` →
  `b39c3276`, 2026-09-08) and `spec/BASELINE.json` (14061 → 14107 passing).
  Every behaviour change in 1.104.0 and 1.104.1 is implemented below, except
  two: the indented-syntax parser crash 1.104.1 fixes has no reproduction
  here, and the output file's modification time now reflecting when
  compilation STARTED is a watch-mode rebuild concern that sasso, having no
  watch mode, has nothing to rebuild from. On the Lichess corpus sasso stays
  byte-identical to dart on 147 of the 148 entry points in BOTH output
  styles.
- **A negative zero keeps its sign.** `0 * -1`, `-0` and `math.div(0, -1)`
  now serialize as `-0`. The sign is the IEEE sign bit, not the way the
  number was written, so `0 - 0`, `0 + -0` and `-0 * -1` stay `0`, and a tiny
  negative that merely rounds to zero (`-1e-11`) was never a zero at all. A
  rounding RESULT is an integer and so is never a negative zero:
  `math.round(-0.4)`, `math.ceil(-0.4)` and `round(to-zero, -0.4, 1)` are `0`.
- **Colors convert their degenerate channels.** A `NaN` channel becomes `0`
  in every color function — it stops being degenerate at all, so the call
  parses into an ordinary color instead of a preserved `calc()` spelling —
  and a polar HUE converts every non-finite value: `hsl(calc(NaN), 50%, 50%)`
  is `hsl(0, 50%, 50%)`, `lch(1% 2 calc(infinity))` is `lch(1% 2 0deg)`,
  `color(srgb calc(NaN) 0 0)` is `color(srgb 0 0 0)`. An infinite non-hue
  channel is the only one still written as a `calc()`
  (`hsl(0, calc(infinity * 1%), 50%)`). The conversion runs wherever a color
  is built, so it also reaches a channel a computation produced — an hwb
  color with an infinite whiteness, or `color.change(red, $hue: NaN)`, which
  is red. A color channel also drops a negative zero, the alpha included
  (`color(srgb 0 0 0 / -0)` is `… / 0`), which is the exception to the rule
  above.
- **A comment before `@use` is emitted exactly once.** Up to 1.104.0 a REPEAT
  edge into an already-loaded module re-emitted the comments that had
  preceded its first load — a second `@use` of it from the same file, or a
  sibling or nested module's, each wrote another copy. 1.104.1 removed the
  behaviour, and so does this release, along with the machinery that mirrored
  it.
- **A many-to-many compile skips sources inside a nested output directory.**
  `sasso .:css` run twice used to mirror `css/` into `css/css/`. Nesting is
  strict — `sasso dir` (which is `dir:dir`) still compiles every file — and
  it is the destination that counts, not an intermediate directory.

### Changed

- **Library:** `WarnEvent` is now `#[non_exhaustive]` and gained the `path`
  field. Code that receives events (every handler in this repo and its
  napi/wasm bridges) is unaffected; code that built a `WarnEvent` with a
  struct literal or matched it exhaustively must adapt.
- **CLI (breaking):** `-q/--quiet` now means "don't print warnings", as in
  dart-sass. The old meaning — compile but discard the CSS, for timing runs —
  is `--no-css`. Writing to a file (`-o`, a second positional, or a pair) now
  produces a source map by default; pass `--no-source-map` to opt out. More
  than one positional input is no longer a multi-file stdout batch (dart
  reads a second positional as the output); use `in:out` pairs. `--source-map`
  to stdout is a usage error unless `--embed-source-map` is given. Usage
  errors exit 64 (was 1).

### Fixed

- **`calc(#{-$x} …)` no longer errors** with "This expression can't be used
  in a calculation." Interpolation is a full SassScript context even inside a
  calculation, so the calc-only grammar restrictions (no `-$x`, no `- 1px`)
  are now suspended for the duration of a `#{…}` — matching dart-sass, which
  evaluates the expression and splices its text into the calc (#24, reported
  from the Lichess stylesheets).
- **A mixin whose `@content` sits inside `@supports` now accepts a content
  block.** The "does this mixin use `@content`" scan descended into `@media`,
  `@at-root`, `@keyframes` and generic at-rules but skipped `@supports`, so
  `@include` with a block hit "Mixin doesn't accept a content block." (#24).
- **Source maps name imported files by their resolved path and list only
  mapped files.** `sources` were keyed by a file's display url — an imported
  partial's basename — so `../_dep.scss` stood where dart writes
  `../lp/_dep.scss`, and two partials sharing a basename collapsed into one
  entry with the second one's mappings pointing into the first. The file
  table is now keyed by the canonical URL (the resolved path with
  `FsImporter`; the entry's `url` as given), and `sources` holds exactly the
  files the mappings reference, in order of first appearance: an entry that
  only imports or only declares variables is not a source, and an empty
  stylesheet has `"sources":[]`, as in dart-sass. Library API note: an
  embedder now sees absolute paths for imported files in
  `SourceMap::sources` (the CLI relativizes them to the map).
- **Stack frames name a loaded file by its path from the current directory**
  (`src/sub/_partial.scss`, `lp/_dep.scss`), as dart-sass does, instead of its
  bare basename — so two partials that share a name are told apart in a
  trace. A file outside the tree whose relative spelling would be longer than
  its absolute path is shown absolute (dart's `prettyUri`); the entry stays
  as given. `WarnEvent::url` carries the same spelling.
- **`FsImporter` canonical URLs leave symlinks unresolved, as dart-sass
  does.** The canonical URL was the file's `realpath`, so a stylesheet reached
  through a symlinked directory — a pnpm `node_modules/<pkg>` link — was named
  by its `.pnpm` store path in a source map's `sources`, in `WarnEvent::path`
  and in `DependencySet`, and two links to one file were one module. dart's
  `p.canonicalize` is the absolute, lexically normalized path with links kept;
  sasso now matches (10 of 148 Lichess bundles had `sources` differing from
  dart's for this alone). Library note: imported files now appear as the
  absolute path they were reached by, not their realpath.
- **Indented-syntax (`.sass`) diagnostics and source maps point at the `.sass`
  file.** The front-end rebuilds SCSS from the indentation-structured source,
  and it used to rebuild it compactly: statements were re-indented by nesting
  depth, blank and comment-only lines vanished, every block's `}` took a line
  of its own. Every line and column downstream — an error, a `@warn`, a
  deprecation span, a source-map entry — was therefore a position in that
  reconstruction rather than in the file the user wrote. The reconstruction is
  now position-preserving (one output line per source line, the source
  indentation kept, a block's `}` riding on its last line), and the two
  constructs that cannot be rewritten without moving columns are read by the
  parser in place instead:
  - the mixin shorthands `=name`/`+name`, which used to be expanded to
    `@mixin`/`@include` — eight columns wider, so `+mx(1)`'s argument mapped
    eight columns to the right of where it is;
  - unquoted `@import` urls, which used to be quoted (two bytes wider, moving
    the deprecation caret). dart reads such a url to the next top-level comma,
    spaces included, so `@import foo screen` is one url named `foo screen`;
    a `.css`/protocol url is a plain-CSS import written back quoted.

  On the shapes dart-sass 1.103.1 was measured against — errors, warnings,
  `[import]` deprecations, and six source maps covering comments, blank lines,
  multi-line selector lists, `@media`, custom properties and the shorthands —
  every line, column and `mappings` string now matches it exactly.
- **An escape is one token in a custom-property value, and a `}` inside a
  string does not close an interpolation.** dart-sass captures a custom
  property's value with `_interpolatedDeclarationValue`, which consumes a `\`
  escape whole and writes it back canonically; sasso weighed the escaped
  character itself, so an escaped delimiter changed what the value meant.
  `--x: \{` left a brace open and swallowed every line after it (`unexpected
  end of input, expected "}"`), `--x: \"` opened a string, and `a\;b` ended the
  declaration early. An escape now round-trips the way dart spells it (`\7b`
  and `\{` both print `\{`, `\61 b` prints `ab`, an invalid code point becomes
  U+FFFD), each closer is matched against the bracket it opened (`--x: (]` is
  `expected ")".`), and a closer with no opener ends the value (`--x: ];` is
  `expected ";".`). The `@supports` reader and the body of a plain-CSS custom
  `@function`/`@mixin` capture values the same way and follow the same rules.
  In the indented syntax the line scanners that decide where a value ends now
  read a string inside `#{ … }` as text, so `b: #{"} // not a comment"}` is one
  declaration rather than a parse error, and they track which bracket each
  closer closes: a mismatched one (`--x: (]`) is left to the parser, which
  reports dart's `expected ")".` at the closer instead of the front-end
  complaining about the line indented beneath it.
- **Interpolation resolves inside a quoted string in every verbatim value.** A
  custom-property value, a `@supports` declaration and the body of a plain-CSS
  custom `@function` all copy their text verbatim, but `#{…}` is not part of
  that text. The custom-property reader resolved it inside quotes; the other
  two copied the string whole, so `@supports (--a: "#{$v}")` and
  `@function --f() { result: "#{$v}"; }` emitted the interpolation literally.
  The string's own text, escapes and line continuations included, still passes
  through untouched — dart does not re-serialize a verbatim value's string.
- **The `[global-builtin]` deprecation.** dart warns at every call to a global
  built-in that has a `sass:*` equivalent — `map-get`, `nth`, `percentage`,
  `str-length`, `lighten`, `type-of`, `selector-parse` and some seventy more —
  naming the member to use instead and underlining the whole call. sasso
  emitted nothing, so a build that dart floods with migration warnings was
  silent. The mapping is not mechanical and every entry was measured against
  dart-sass 1.103.1: the legacy colour adjusters all point at `color.adjust`,
  `unitless` at `math.is-unitless`, `comparable` at `math.compatible`,
  `list-separator` at `list.separator`. A global dart KEEPS is left alone — a
  CSS function it shares a name with (`abs`, `round`, `min`, `sqrt`, …),
  `ie-hex-str`, and `if()`, which has a deprecation of its own. The per-id cap
  of five and the "N repetitive deprecation warnings omitted" footer apply as
  they do to `[import]`.
- **A span that crosses lines is drawn the way dart draws it.** dart puts the
  arm glyph in the GUTTER beside the source when the span begins at its line's
  first non-whitespace character, and likewise when it ends at its line's last;
  it draws an `┌─…─^` / `└─…─^` arrow row only for an end that starts or stops
  mid-line. sasso always drew the arrow rows, so every multi-line diagnostic
  differed from dart:

  ```
  2 │ ┌   @include nope {        2 │     @include nope {
  3 │ │     c: d;                  │ ┌───^
  4 │ └   }                      3 │ │     c: d;
                                 4 │ │   }
                                   │ └───^
  ```

  The two ends are decided separately, so `.a { @include m {` … `  }` opens
  with an arrow row and closes in the gutter.
- **An error at the end of a file points at the last line with content.** dart's
  scanner never advances into a file's trailing whitespace, so `.a { b: c` (no
  closing brace) reports at the end of that line; sasso reported on the blank
  line after it, drawing an empty snippet. The position now walks back to the
  end of the last line that says something, trailing spaces on that line
  included — in the frame trace as well as the snippet.
- **Two parser messages name what was expected, as dart's do**: a file that ends
  inside a block is `expected "}".` rather than `unexpected end of input,
  expected "}"`, and anything that cannot begin a value — a stray `;`, a `)`, an
  empty `@if` condition, the end of the file — is `Expected expression.`
- **A module diagnostic carets the construct it is about.** dart underlines the
  whole rule, call or reference a diagnostic belongs to; sasso drew a single
  caret, or — for an `@include` — reported the error with no snippet at all:

  | | dart | sasso |
  |---|---|---|
  | `@use "nope"` (missing) | the whole rule | one caret |
  | `@use` after other rules | the whole rule | one caret |
  | `@include nope` | the whole call | no position |
  | `@include ns.nope` | the whole call | no position |
  | `ns.nope(1)` | the whole call | one caret |
  | `ns.$nope` | the reference | line 1, column 1 |
  | `@include ns.-private` | the member name | one caret |

  A namespaced variable reference carried no position at all, so its error
  landed on the file's first line rather than on the reference. The messages
  match dart now too: `Undefined mixin.` rather than `Undefined mixin nope.`,
  and `Can't find stylesheet to import.` without the url the span already
  points at. (A `Missing argument`/content-block error still shows only its
  primary span; dart adds a second one for the declaration.)
- **A string is quoted the way dart quotes it, wherever it is written back.**
  `meta.inspect` and `@error` wrapped the text in `"` unconditionally, so a
  string containing a quote or a backslash came out as invalid CSS:
  `b: meta.inspect("a\"b")` emitted `b: "a"b";` where dart emits `b: 'a"b';`,
  and `meta.inspect("a\\b")` lost the escape. dart picks single quotes when the
  text holds a `"` and no `'`, and escapes the chosen quote and every
  backslash. The same wrapper produced a plain-CSS import's url in the
  indented syntax, so `@import h\74 tps://x/y.css` emitted a url whose escape
  no longer survived a round trip; it is now serialized as the string it is.
- **A hex escape's terminator is one line break.** One whitespace character
  ends a hex escape, and a CRLF is one character's worth of line break where
  the text is captured VERBATIM: `--x: \61` + CRLF + `b` is `ab`, with no line
  break left in the value. Where the text is SassScript only the `\r` is
  consumed, so the `\n` still separates two identifiers (`b: \61` + CRLF + `b`
  is `a b`). The two readers had it backwards from each other. A vertical tab
  never terminates an escape — dart's whitespace set is space, tab and the
  three CSS newlines — which the indented front-end's own decoder now matches.
- **A form feed is a newline to the escape reader.** dart counts U+000C as a
  newline, so a backslash cannot escape it; the identifier and verbatim-value
  readers accepted the pair and serialized it, where the ordinary value reader
  (and dart) report `Expected escape sequence.`
- **A `.sass` custom-property value continues past a trailing backslash.** The
  front-end decides where such a value ends, and it ended one at the line break
  even when the last character was an unpaired `\`. A string continuation
  (`--x: "a\` then an indented `b"`) was rejected as a stray indented child,
  and `--x: c\` reported that same front-end message instead of dart's
  `Expected escape sequence.` at the backslash. An even number of trailing
  backslashes still ends the value: the last one is escaped, not an escape.
- **A backslash before a newline is an escape only inside a string.** dart's
  `escape()` fails on a newline, and only its string reader drops the pair
  first — which is what makes a CSS line continuation legal inside quotes and
  nowhere else. sasso accepted it everywhere and treated it as a line wrap, so
  `b: c\` followed by `d` compiled as `b: c d`, `.a,\` + `.b` became a
  selector with an escaped line break (`\a `), and a url kept reading. All of
  them now fail where dart fails, with dart's message and column. In the
  indented syntax the front-end had the same leniency of its own — it dropped
  the trailing backslash and joined the next line with a space — so
  `@import url(foo\` + `bar.css)` imported `url(foo bar.css)`; the pair now
  reaches the parser verbatim. A continuation inside quotes still works in a
  value and in a selector: `[a="x\` + `y"]` is `[a=xy]`.
- **Two statements on one line are reported at the second statement.** The
  indented syntax forbids `b: c; d: e`, and dart carets the `d` — the first
  character after the `;` and the whitespace following it — where sasso
  pointed at the `;` itself. The position is counted from the last line break
  inside the statement, so a `;` below a bracket continuation is reported on
  the source line it is written on rather than on the line the statement
  started on.
- **An `@import`'s `url()` drops its padding and decodes its escapes.** dart
  reads the token with `_tryUrlContents`: the whitespace after the `(` and
  before the `)` is not part of the url, and a `\` escape is consumed whole
  and written back canonically. sasso kept the text exactly as written, so
  `@import url(  x.css  )` — and a `url(` opened on one line and closed on
  another, which the indented syntax invites — emitted the padding, and
  `url(\61 b.css)` stayed escaped where dart prints `url(ab.css)`. The
  declaration-value reader already followed both rules; the import reader now
  does too — it runs the same trial, and falls back the same way when the
  contents are not a url token at all. dart parses `url(…)` as an ordinary
  FUNCTION CALL then, so its arguments evaluate: `@import url(foo + bar)` is
  `url(foobar)` and `@import url($base + ".css")` imports the computed url,
  where sasso emitted the SassScript verbatim — a `$variable` reaching the
  CSS. A quoted url is such a function call too, which is why it keeps its
  own spacing.
- **`//` inside a `url()` is no longer a comment in the indented syntax.**
  `url(http://x/y)`, `url(//cdn/x.png)` and `@import http://x/y.css` were
  truncated at the `//` (`url(http:` — "expected \")\""), because the front-end
  stripped silent comments before the url token was recognized. The scanners
  that decide where a logical line ENDS skip the token too, so a declaration
  after one (`b: url(http://x/y)` then `c: red`) is still its own statement
  rather than being joined into the value. dart scans
  `url(` and its contents as one token; only the exact `url` function
  qualifies, so `my-url(//y)` still starts a comment, as it does for dart.
- **Compressed output no longer writes a stray `;` after a nested rule, and
  maps that rule's children.** A loaded `.css` file that uses CSS nesting
  rendered its nested blocks into one pre-built string, so `.a { .b { x: 1 } z:
  3 }` came out as `.a{.b{x:1};z:3}` — dart writes `.a{.b{x:1}z:3}`, a block's
  `}` being its own separator (`_requiresSemicolon`) — and no declaration
  inside the nested block carried a source-map entry. The block's items now go
  through the mapping-aware serializer. A block at-rule nested inside such a
  rule — which dart keeps in place rather than bubbling, once CSS nesting is in
  play — carries its own position too, so it maps to its `@` keyword in both
  output styles.
- **An at-rule whose name is interpolated (`@#{"media"} screen`) carries its
  source span**, so it maps like the plain spelling and joins a trailing
  comment the same way (`@#{"media"} screen { /* t */` used to break the
  comment onto its own line).
- **Source maps: every generated line of a line-spanning construct maps, a
  comment maps from column 0, a rule maps to its selector's line, and
  passed-through `@import`s and nested plain-CSS rules map at all.** dart-sass
  keeps a mapping span open while it writes a construct, so each newline
  inside a multi-line selector list, comment, at-rule prelude or re-indented
  custom-property value adds an entry at the start of the new line that points
  back at the construct; sasso mapped only the first line. A rule mapped to its
  opening-brace line — a selector list written over several lines maps to its
  first line (`node.selector.span.start`) — and so did a block at-rule whose
  prelude spans lines, which maps to its `@` line. A nested comment mapped after its
  indentation where dart opens the span before indenting. A passed-through
  plain-CSS `@import` (mapped to its URL token) and a nested rule of a loaded
  `.css` file that uses CSS nesting (mapped to its selector) had no mapping.
  Verified byte-for-byte against dart-sass 1.103.1; on the Lichess corpus,
  bundles whose `mappings` are identical to dart's went from 2 to 148 of 148.
- **Blank lines between top-level groups survive dropped placeholders.** An
  unextended placeholder rule with a declaration and an empty nested rule
  (`%video { width: 100%; > * {} }`) produced two invisible nodes, and
  dropping each removed one blank-line separator: the separator after the
  previous group as well as the placeholder's own. The next comment or rule
  then packed tight where dart-sass keeps the blank line. On the Lichess corpus this was the difference in the
  blank-line placement of 123 of 148 bundles.
- **A loaded plain-CSS file's `@charset` is dropped**, as dart-sass drops it:
  a `.css` file reached through `@use` or `@import` that begins with
  `@charset "utf-8";` used to leave that line in the middle of the output
  (the Lichess `bits.cms` and `bits.ublog.form` bundles, via a vendored
  editor theme). The output's own `@charset "UTF-8";` is still derived from
  its content.
- **Nested rules in a loaded plain-CSS file keep their selector lines.** A
  `.css` file's nested rule whose selectors were written one per line
  (`.swiper-slide,\n  .swiper-cube-shadow {`) was emitted on one line;
  dart-sass keeps the source line structure for nested rules as it already
  did for top-level ones (the Lichess `recap` bundle, via the swiper
  stylesheet).
- **`@import` deprecation warnings fire when a file is parsed**, as in
  dart-sass, not when each rule is evaluated: all of a file's import
  deprecations now precede anything its body prints (its `@warn`s, the
  warnings of the files it imports), a file the import cache already parsed
  warns once however often it is imported, and a `@use`d module's own
  `@import` warns under the `@use` frame before the module body runs. This
  was the last difference in stderr ordering against dart-sass 1.103.1 on
  the `@import` chains tested here. In the same move a misplaced `@import`
  — in a mixin or function body, a control directive, a property set, or an
  at-rule nested in one of those (interpolated at-rules included) — is
  rejected as dart-sass rejects it, in loaded files as well as the entry
  (sasso used to accept it in property sets and in loaded files), and the
  "This at-rule is not allowed here." error now carries dart's snippet and
  frames. A property set admits no `@import` of any kind, plain-CSS ones
  included. And a plain-CSS entry (a `.css` input) is now evaluated as plain
  CSS like a `.css` module: its `@import "theme";` is passed through instead
  of being loaded as Sass (which warned and then failed with "Can't find
  stylesheet to import").
- **A mixin, function, or `@content` block runs against the file that wrote
  it**, as in dart-sass. A callable defined in a textually `@import`ed file
  (or reached through `meta.apply`/`meta.call`) used to be evaluated as if it
  were written in the caller's file: its output mapped to the caller's
  `sources` entry with the callable's line numbers, its warnings and errors
  named the caller's file and rendered the caller's source, and a `@content`
  block passed to a `@use`d module's mixin was attributed to the module. Now
  the body's output maps to its own file, stack frames read as dart prints
  them — `src/_dep.scss 2:3  m()`, the block's statements under a `@content`
  member in the includer's file with the mixin's `@content;` statement as a
  call site, `f()` (not `call()`) inside a function invoked through
  `meta.call`, with the `meta.call(...)` expression as a call site — and an
  error inside the body shows the body's source. A function invoked through
  `meta.call` also resolves `ns.member` against its own `@use` namespaces
  now, as a direct call did. A member `@forward`ed by a file that is then
  `@import`ed keeps its defining file too — and its own `@use` namespaces,
  so a forwarded function using `sass:math` no longer fails with "There is
  no module with the namespace "math"" when reached through `@import`. And an `@error` raised directly
  in a content block carets the nearest `@include` whose mixin is running
  (the name and arguments, as dart does — through any number of forwarded
  content blocks), not a `@content;` statement. Module
  callables also track whether they are a mixin now: `meta.content-exists()`
  inside a `@use`d mixin answers for the include at hand instead of erroring,
  and inside a module function it errors as it does in a plain one. A
  default in a content block's `using (...)` clause evaluates where the
  block was written (`using ($y: $caller)` sees the includer's `$caller`, as
  in dart-sass), not in the mixin's module. On the Lichess corpus this
  takes the source maps whose `sources` match dart-sass from 14 to 138 of
  148. (`WarnEvent::url`/`path` follow.)
- **`--quiet-deps` no longer aborts after a compile error.** The dependency
  record's mutex was allocated lazily on its first lock — from inside the
  compile, so in the CLI's bump arena, which the compile resets on the way
  out — and the error-CSS re-render (a second compile in the same process)
  then locked freed memory: `failed to lock mutex: Invalid argument` and exit
  code 134 instead of the error report and exit code 65. Every access to a
  `DependencySet` now runs with the arena paused, so the mutex and the keys
  it records live in system memory for the set's whole lifetime. Host
  functions (`Options::with_function`) now run with the arena paused as
  well, like importers and warn handlers already did, so anything a host
  keeps across calls is never arena-resident.
- **Warn handlers may retain event data.** The library now pauses its
  bump-arena scope while calling an embedder's `WarnHandler`, so a handler
  that appends the event to a buffer no longer ends up with memory the
  arena frees at the end of the compile (which surfaced as garbled or
  cross-contaminated warnings under parallel compiles). Importer calls were
  already paused the same way. The pause itself is now a counter behind an
  RAII guard rather than a zeroed scope depth: a `compile` run from inside an
  importer or warn callback nests instead of resetting the arena under its
  caller, and a callback that panics no longer leaves the thread paused.

## [0.9.1] - 2026-09-01

_C-ABI release fixes — no compiler changes. Ships musl c-api tarballs for
Alpine (#18) and corrects the version string the C ABI reports._

### Added

- **musl c-api tarballs** (`x86_64-unknown-linux-musl`,
  `aarch64-unknown-linux-musl`) for Alpine and distro-agnostic containers
  (#18). musl artifacts are STATIC-ONLY by design: a musl target defaults to
  `+crt-static`, under which rustc cannot build a cdylib, so the tarball
  ships `libsasso.a` (and `sasso.h`) without a `.so` — link it into your own
  binary or shared object. Verified end-to-end on Alpine 3.24 (gcc +
  musl-dev, dynamic and fully-static links both pass the C smoke test).
  Adapted from @shyim's fork.

### Fixed

- **`sasso_version()` reports the bundled compiler version** (now `0.9.1`),
  not the FFI wrapper crate's own version (which had drifted to `0.6.1` —
  the string the v0.9.0 c-api artifacts report). The core crate now exposes
  `sasso::VERSION` so wrappers cannot drift again. Contributed by @shyim.

## [0.9.0] - 2026-09-01

_Crate release `v0.9.0`; ships on npm as `sasso@0.12.0`. **Output-format
alignment with dart-sass 1.103.1** — the reference pins (CI parity binary,
sass-spec, baseline) all move together. If you byte-compare sasso's output
(snapshot tests, build caches), expect diffs on legacy colors and plain-CSS
`if()`; the CSS is equivalent, only its spelling changed. sass-spec passing
rose to **14061** (98.95% of attempted, +165 on a newer, larger suite).
Minor — not patch — for the serialization changes and one removal below._

### Changed (dart-sass 1.101.4–1.103.x alignment)

- **A legacy color with any fractional channel serializes its rgb triple as
  percentages** (dart 1.101.4): `rgb(127.5, 0, 127.5)` is now
  `rgb(50%, 0%, 50%)`. Older browsers only support integer or percentage
  channels in `rgb()`/`rgba()`, so this preserves backwards compatibility
  without losing precision. Contributed by @shyim (#19).
- **Plain-CSS `if()` values emit in CSS serialization format** (dart
  1.101.4), not `meta.inspect()` format: lists lose their parens
  (`if(css(): 1 2 3)`, invisible items drop out), `null` serializes to
  nothing, a preserved slash-division keeps its slash, and a value with no
  CSS form (`()`, a map, a function/mixin reference) is an error.
- **rec2020 uses the pure 2.4 gamma transfer function** (dart 1.102.0, per
  the latest CSS Color 4 draft), replacing the BT.2020 piecewise curve.
- **Compressed hsl/hwb serialization routes through rgb** like every legacy
  space (dart `_writeLegacyColor`): hex/named first, then the shorter of the
  percent rgb form and the hsl form *derived from that rgb* under dart's
  two-character hsl handicap. A powerless (zero-saturation) hue collapses to
  0, and a non-opaque hwb can emit `rgba()`; only an out-of-gamut color
  keeps its hsl form.
- **Reference pins**: sass-spec `1b03109a` → `4a9eea66`, CI parity binary
  `sass@1.101.3` → `sass@1.103.1`, `spec/BASELINE.json` 13896 → 14061
  passing. The parity binary is now pinned (previously floating `latest`),
  so upstream dart-sass releases can no longer redden unrelated PRs.

### Removed

- **Global `whiteness()` and `blackness()`**: like dart-sass, these are
  `sass:color`-only (`color.whiteness()` / `color.blackness()`); the global
  spellings now error. Contributed by @shyim (#21).

### Fixed

- **The indented syntax no longer panics on an overlapping `/*/`** comment
  terminator. Contributed by @shyim (#17).
- **Unknown-channel errors render the channel list with `inspect`**, not
  `to_css`, matching dart-sass message spelling. Contributed by @shyim (#20).
- **Source maps: a declaration whose value is a bare `$name` maps back to
  the variable's definition** (dart `Environment._variableNodes`),
  transitively through `$b: $a` chains, module members, and
  mixin/function parameters (which resolve to the call-site argument).
  Previously the segment was omitted entirely, which also renumbered every
  following delta-encoded segment. Contributed by @shyim (#22).

### Performance

- The definition-span bookkeeping behind the source-map fix is gated on an
  actual source-map compile — the plain `compile()` path pays nothing for it.
- The percent channel formatter appends `%` in place instead of allocating a
  second string per channel, keeping the `colors` benchmark at its
  pre-percent-serialization baseline (CodSpeed gate green).

## [0.8.1] - 2026-07-17

_Release-tooling only — no compiler changes._

### Changed

- Releases now attach a conventionally-named source tarball
  (`sasso-<version>.tar.xz`, extracting into `sasso-<version>/`) plus a
  `.sha256`, built deterministically with `git archive`, replacing
  cargo-dist's generic `source.tar.gz`. Requested by downstream packagers
  (FreeBSD ports).
- CI: bumped `moonrepo/setup-rust` v0 → v1 (Node 24 runtime, current cache
  backend — fixes the deprecation warning and cache-service 400s in
  benchmark runs).

## [0.8.0] - 2026-07-06

_Crate release `v0.8.0`; ships on npm as `sasso@0.11.0` (wasm + native — same
core). Byte-identity, complete: **all 20 projects in the real-world corpus
compile byte-identical to dart-sass 1.101**
([`bench/real-world/real_world.md`](bench/real-world/real_world.md), 2.9–51×
faster end-to-end). The corpus doubled this release (tabler, AdminLTE,
reveal.js, Font Awesome, video.js, forem, nextcloud server,
jekyll-theme-chirpy, grafana, wagtail) and every divergence it surfaced was
fixed against dart-sass semantics — each pinned by a parity test._

### Fixed (`@extend` engine — dart's per-registration model)

- **`@each` over a single value — `null` included — iterates once** (dart
  `Value.asList`). Bootstrap's `valid-radius(null)` relies on it; previously
  a css-less `border-radius` map key ERRORED the whole compile (tabler).
- **The application fold runs each registration exactly once — no fixpoint.**
  Extend cycles resolve the way dart's do: `_extensionsByExtender[target]` is
  a LIVE list, so an extender that itself contains the target self-derives
  within its own registration. The old fixpoint chained one extender of an
  `@extend` through its sibling of the same rule, emitting selectors dart
  never produces.
- **Original selectors are identity-tracked** (dart `_originals` is a
  `Set.identity()`): an extension product value-equal to one of the rule's
  own selectors is coverage-trimmed in the original's favor, keeping the
  original's position and source line break (`.btn-sm,\n.btn-group-sm > .btn`;
  `h1, .h1` heading runs). dart's bare fast path (a single-simple compound
  replaced wholesale returns the extender object) carries identity too,
  scope-gated per module store — dart#1297 keeps the in-store `:is()` rewrite
  alongside its source while the cross-module variant replaces it.
- **Foreign extensions apply in dart's cross-module store-merge order**
  (downstream stores merge transitively; sibling stores reverse-first-load;
  derived entries follow their trigger's absorption order) — bulma's `%block`
  extender interleaving.
- **Identical `(target, extender)` registrations merge per module store, not
  globally**, so each keeps its own store-merge rank (chirpy's
  `#access-lastmod a:hover` position and line break).
- **One `@extend` hitting several compounds of the same complex uses dart's
  `paths` order** (first component varies fastest) on the incremental path
  too (forem's `.crayons-btn + .crayons-btn` file-selector products).
- **Pre-rule extensions apply one-shot per dart's `addSelector` timing**, and
  the one-shot gate compares registration indices, not counts.
- **A placeholder-only module whose rules were all dropped counts as empty**,
  so its group-separator blank collapses (uswds `placeholders/`).

### Fixed (module system & output fidelity)

- **The pre-module comment engine matches dart on three fronts** (uswds):
  registration deep-scans pending comments through invisible module-scope
  placeholders; re-emitted clones are fenced and never re-register onto new
  module keys; a css-less module built while the shared comment map is
  non-empty absorbs pending registrations (dart's `transitivelyContainsCss`
  includes `preModuleComments.isNotEmpty`).
- **Re-emitted pre-module clones stay out of the loader's import-run sweep**
  (nextcloud's SPDX header no longer jumps to the top of the document).
- **CSS `@import` hoisting keeps the css flow's eval-time grouping** — dart's
  blank lines come solely from group-end flags, never source gaps; only the
  seam the pulled import run vacated is re-derived (forem).
- **`meta.load-css` copies re-acquire per-rule group separators** — dart
  re-visits the combined css node-by-node (reveal.js print styles).
- **A trailing invisible chain owns the enclosing group's end at any nesting
  depth**, packing the next group tight (chirpy).
- **`@at-root` separators follow dart's `_styleRule == null` group-end gate**
  — rules flattened out of a wrapper rule pack tight (wagtail's sidebar).
- **Selector line-break parity with dart** (pseudo-arg newlines,
  parent-resolution flags), **loud comment re-indentation** per dart's
  `_loudComment`, **invisible-last-child groups pack the next group tight**,
  and **an empty module scope no longer anchors a group separator**
  (bootstrap, quasar, mastodon, govuk-frontend byte-identity).

### Added

- **`bench/real-world/`: the vetted corpus doubled to 20 projects** — every
  one compiled standalone with both engines and byte-compared on every run
  (`node bench/real-world/run.mjs check` is the regression gate).

## [0.7.0] - 2026-07-04

_Crate release `v0.7.0`; ships on npm as `sasso@0.10.0` (wasm + native — same
core). The dart-sass byte-parity campaign: every project in the new
real-world corpus ([`bench/real-world/real_world.md`](bench/real-world/real_world.md))
now compiles, and most match dart-sass 1.101 byte-for-byte._

### Added (npm package — `sasso/native`; released as `sasso@0.9.0` on npm)

- **`sasso/native` subpath: the native addon as a first-class npm entry.**
  Prebuilt binaries publish as exact-version-pinned `optionalDependencies`
  (`sasso-native-{darwin-arm64, darwin-x64, linux-x64-gnu, linux-arm64-gnu}`) —
  npm installs only the matching platform, the release workflow builds and
  byte-parity-tests every binary against the wasm reference before publishing,
  and unsupported platforms get a clear error pointing back at the wasm
  entries. Resolution order: `SASSO_NATIVE_BINARY` override → platform
  package → repo-local build.

### Added (repo — native Node addon)

- **`napi/`: a native Node addon binding the core crate directly** (F4 of
  `docs/ASYNC_PERF_ARCHITECTURE.md`) — no wasm, no asyncify. Same dart-sass
  modern API as the `sasso` npm package, verified **byte-identical** to the
  wasm engine's output across the in-repo corpora (which is itself dart-sass
  byte-exact). Async compiles each run on their own OS thread: ~3× engine
  speed over the wasm modules, and a concurrent 8-entry cold build finishes
  in ~1.14× a single compile's time (true multi-core parallelism; the wasm
  engine's single JS thread serializes CPU-bound fan-out). User importers, custom functions,
  and loggers bridge to JS; `loadPaths`/relative resolution run natively.
  Repo-buildable (`bash napi/build.sh`, `node napi/test.mjs`); publishing
  waits on a per-platform prebuild matrix.

### Changed (npm package — async path performance; released as `sasso@0.8.0` on npm)

- **Concurrent `compileStringAsync`/`compileAsync` calls no longer serialize.**
  The single asyncify-instance lock is replaced by a lazily-grown pool of
  asyncify engines (default cap: `min(4, cpu cores)`, tunable via the new
  `configure({ asyncInstances })`). While one compile awaits an asynchronous
  importer, other compiles run on other engines — a bundler fanning out N
  sass entries no longer queues them end-to-end (measured: N=8 fan-out with
  2 ms importer latency, makespan −75.6%). A process that never overlaps
  async compiles still pays for exactly one instance; each additional engine
  reserves its own wasm memory (incl. the arena) plus a 1 MiB asyncify stack.
- **Synchronously-resolving importers and custom functions no longer pay the
  asyncify suspension on the async APIs.** Results that settle synchronously
  (plain return values — including the built-in `loadPaths` filesystem chain
  and sass-loader's cache-hit resolutions) are delivered without an
  unwind/rewind cycle. A `loadPaths`-only `compileStringAsync` now suspends
  zero times. Genuinely-async importers behave exactly as before.
- **`sasso/speed`'s async APIs now run a speed-optimized (`-O3`) asyncify
  module** (`sasso.speed.async.wasm`, ~3.2 MB / 1.0 MB gzip) instead of
  sharing the size-optimized one — ~2× engine throughput at v8 steady state
  (long-lived processes; one-shot CLI-style runs are dominated by v8 tiering
  and see little change). The default `sasso` entry is unchanged.
- Degraded async modules (built without `wasm-opt`) previously crashed on the
  first importer callback; they now work for synchronously-resolving chains
  and reject genuinely-async importers with a clear error.

### Fixed

- **Real-world corpora now compile — four previously failed.** Callable
  closures capture the defining file's `@use` namespace tables (dart
  `Environment.closure()`), fixing "There is no module with the namespace
  X" for functions/mixins reached via `@import` or multi-hop `@forward`
  (uswds's `units()`, quasar's `str-fe()`). CSS escapes are literal
  identifier text in selector scans (`.govuk-\!-font-size-19`,
  govuk-frontend). In the indented syntax, `as *` terminates a
  `@use`/`@forward` prelude instead of reading as a pending multiplication
  (vuetify), and a trailing-comma selector line with a pseudo-glued colon
  (`&:active,` / `i[type="s"]::-webkit-x,`) continues the list instead of
  being **silently dropped** (quasar — a correctness bug, not formatting).
- **Byte-parity with dart-sass across the serialization surface.** Loud
  comments dedent at serialize time (interpolated banners included) and an
  indented `/**` opener stays glued; comments registered before a module's
  first load re-emit at every dependency edge (bulma's `/* Bulma Form */`);
  `@import`ed files carry their own file identity (no cross-file trailing
  -comment gluing); invisible `@extend`-only rules leave no blank-line group
  end; selector lists keep their authored line structure inside nested
  at-rule wraps and plain-CSS imports (which also unquote identifier
  attribute values like dart's parser); multi-`&` parent expansion
  interleaves column-major (mastodon's adjacent-state selectors); a `&`
  nested in pseudo parens substitutes inside multi-`&` parts
  (`:not(&--mini-animate)`).
- **Chained `@extend` products keep dart's registration order.** Each
  `@extend`'s extender list is pre-extended by the store accumulated so far
  (dart's `addSelector`), so `.navbar > .container, … .container-xxl` comes
  out in forward order instead of reversed.
- **Errors inside loaded files are attributed to that file.** The snippet
  renders from the erring file and the trace stacks one frame per loader
  (`_mod.scss 1:13  @use` / `main.scss 1:1  root stylesheet`), matching
  dart for `@use`, `@forward`, and `@import` chains — parse errors
  included. Previously the root file's name and snippet were shown with the
  inner file's line numbers.
- **`@media` nested inside an unknown at-rule now compiles.** dart's
  `_inUnknownAtRule` context legalizes bare declarations without an enclosing
  style rule, so the canonical Tailwind v4 idiom
  `@utility container { @media (width >= 96rem) { max-width: 87.5rem; } }`
  parses and emits verbatim (byte-matched to dart-sass 1.101, including the
  classic `min-width` syntax, interpolated queries, and mixed
  declaration-plus-`@media` bodies). A bare declaration in a top-level
  `@media` still errors like dart. Previously: `Error: expected "{".`
- **Keyframe selector lists now join on one line, matching dart-sass.** A
  multi-line authored frame selector (`0%,\n60%,\n100% {`) was emitted with
  the author's line breaks preserved, as style-rule selector lists are; dart
  re-serializes keyframe stops joined with `", "` and drops the breaks
  (`0%, 60%, 100% {`). Found compiling a real-world Rails corpus (a Bootstrap
  → Tailwind compat layer) where this was the only byte difference across
  ~132 KB of output.

## [0.6.3] - 2026-06-25

### Fixed

- **Trailing-newline parity with dart-sass, split correctly between the library
  API and the CLI.** The library API (`compile`, `compile_with_source_map`, and
  thus the wasm `compileString().css` and the Ruby gem) now returns the
  serialized stylesheet with **no trailing newline** — byte-for-byte what
  dart-sass's library API returns. Previously expanded output carried a stray
  trailing newline, so a `sass-loader`/Vite integration saw one extra byte per
  stylesheet versus dart-sass. The CLI front-ends (the `sasso` binary and the
  wasm `sasso` CLI) now append the single trailing newline dart-sass's CLI adds
  to **non-empty** output in **both** styles — compressed CLI output previously
  omitted it — while empty output stays empty (0 bytes), also matching
  dart-sass. Verified byte-for-byte against dart-sass 1.101.0 on both the
  library and CLI paths (expanded, compressed, and empty output).
- **The wasm package now reports `dart-sass 1.101.0` in `info`** (was
  `1.89.0`), reflecting the modern-API surface it's compatible with. Tools such
  as `sass-loader` and Vite parse this version for feature gating, so the stale
  value could gate off newer behavior.

## [0.6.2] - 2026-06-25

### Fixed

- **Compressed output now emits the shortest equivalent legacy-color form**,
  matching dart-sass 1.101.0 byte-for-byte. A computed color such as
  `darken(#336699, 10%)` is written as `hsl(210,50%,30%)` instead of the longer
  `rgb(38.25,76.5,114.75)`, and an integer-rgb-equivalent hsl literal
  (`hsl(210, 50%, 40%)`) collapses to `#369`. The serializer now compares the
  hex/name, `rgb()`/`rgba()`, and `hsl()`/`hsla()` candidates and keeps the
  shortest — the rgb form winning ties — while a powerless (zero-saturation)
  hue is preserved. Expanded output is unchanged. This compressed path had no
  cross-check before (the conformance ratchet and the parity suite were both
  expanded-only), so a compressed dart-sass parity battery now runs in
  `tests/parity.rs`.

## [0.6.1] - 2026-06-16

### Fixed

- A relative `meta.load-css` inside a **first-class mixin** (captured with
  `meta.get-mixin` and invoked via `meta.apply`) now resolves against the
  mixin's **defining** file, not the caller's (issue #8). The regular
  namespaced include path was already correct.

### Added

- A **C ABI** (`ffi/`, `libsasso` + `sasso.h`) — drive sasso in-process from any
  language with a C FFI, with a userland importer callback. Releases now attach a
  per-target `sasso-<version>-<target>-c-api.{tar.xz,zip}` (prebuilt static +
  dynamic library + header). See the "C ABI" section in the README.

## [0.6.0] - 2026-06-15

### Changed (breaking)

- **The `Importer` trait is now dart-sass's two-phase `canonicalize`/`load`**
  (issue #4, RFC in `docs/IMPORTER_REDESIGN.md`). It replaces the old
  `resolve(path) -> Option<String>` plus the accreted `resolve_*` overloads:

  ```rust
  fn canonicalize(&self, url: &str, ctx: &CanonicalizeContext)
      -> Result<Option<CanonicalUrl>, ImporterError>;
  fn load(&self, canonical: &CanonicalUrl)
      -> Result<Option<ImporterResult>, ImporterError>;
  ```

  `canonicalize` resolves a URL to a stable identity without loading (its result
  is the module-cache key); `load` fetches the source as an
  `ImporterResult { contents, syntax, source_map_url }`. Three outcomes:
  `Ok(Some)` = handled, `Ok(None)` = not handled, `Err(ImporterError)` =
  handled-but-failed (an actionable compile error rather than a silent miss).
  New public types: `CanonicalUrl`, `ImporterResult`, `ImporterError`,
  `CanonicalizeContext`. A clean break with no compatibility shim (pre-1.0).
  `FsImporter` and the built-in resolution are unchanged in behavior (sass-spec
  ratchet delta +0); only custom `Importer` implementations must migrate.

### Added

- `ImporterResult.source_map_url` lets an importer set the URL recorded for a
  loaded file in generated source maps (dart-sass `ImporterResult.sourceMapUrl`).

## [0.5.3] - 2026-06-15

### Fixed

- **`!default` no longer evaluates its right-hand side when the variable is
  already set.** dart-sass short-circuits a guarded (`!default`) assignment
  *before* evaluating the RHS, so an expression that would otherwise error is
  harmless once the variable already holds a non-null value; sasso evaluated the
  RHS first. This surfaced in Bootstrap-on-Shopware setups where, after an
  override sets `$w: 1rem`, a later `$p: $w + .5em !default` raised an
  "incompatible units" error instead of being skipped. Thanks to
  [@shyim](https://github.com/shyim) (#2).
- **Legacy `rgb()`/`hsl()` preserve the caller's `rgba`/`hsla` spelling in
  special-value and relative-color passthroughs.** When a call can't resolve to
  a concrete color (a channel or alpha is a `var()`/`env()`/non-foldable
  `calc()`), dart-sass keeps the call *and* the exact function name written;
  sasso normalized `rgba`/`hsla` down to `rgb`/`hsl`, breaking Bootstrap's
  `rgba(var(--bs-body-color-rgb), …)` output. The called name is now threaded
  through every passthrough, keeping dart's carve-out that a `none`-only call
  still normalizes to the canonical `rgb`/`hsl` (and a `calc()` alpha that folds
  to a number resolves to a real color rather than a passthrough). Thanks to
  [@shyim](https://github.com/shyim) (#3).

## [0.5.2] - 2026-06-14

### Fixed

- **Expanded `@at-root` group-separation blank lines.** dart-sass writes one
  blank line at an `@at-root` hoist→resume boundary when the hoisted chunk ends
  in a style rule, while keeping a nested-`@at-root` chain and a rule + its own
  bubbled `@media` contiguous. sasso previously diverged BOTH ways — it never
  emitted the blank before a resumed parent rule, and it over-emitted (three
  blanks between top-level bare-`@at-root` siblings, spurious blanks between a
  nested-`@at-root` chain's rules / between a rule and its own bubbled `@media` /
  before an `@at-root` body's trailing comment). Now byte-exact vs dart-sass
  1.101 across a dedicated 54-shape group-separation sweep, with non-`@at-root`
  output byte-identical. Compressed output is unaffected (no blank lines).
  (sass-spec does not cover these `@at-root`-resume blanks.)

## [0.5.1] - 2026-06-14

Source-map fidelity + compressed-output corrections, all byte-exact vs
dart-sass 1.101.

### Fixed

- **Source maps: `@media`/`@at-root`/`@supports` bubbled parent selector.** When
  one of these at-rules nested in a style rule bubbles a copy of the enclosing
  selector out (`@media screen { .a { … } }`), that copy now maps back to the
  ORIGINAL rule's source position, matching dart-sass. It previously had no
  mapping at all — which in compressed output also let the consecutive-same-
  source-line coalescing drop a following declaration's mapping (a 0.5.0
  regression vs 0.4.0 for `@media`/`@at-root`-bubbled rules). CSS is unchanged.
- **Source maps: `@supports` header.** The `@supports (…)` at-rule header now
  maps to its `@supports` keyword (as `@media` already did); previously it had
  no mapping. CSS is unchanged.
- **Compressed `@media`/`@supports` whitespace.** Compressed output now omits the
  space before a prelude beginning with `(` for `@media`/`@supports`
  (`@media(min-width: 1px)`), and within a `@media` query drops the space before
  `and`/`or` after a `)` (`(a)and (b)`) and after the comma between queries
  (`(a),(b)`) — matching dart-sass. Other at-rules (`@container`) and `@supports`
  conditions keep their spaces. (Compressed CSS output change; expanded
  unchanged.)

## [0.5.0] - 2026-06-14

### Added

- **wasm: source maps.** The `@momiji-rs/sasso` package's `compile(scss, {
  sourceMap: true [, sourceMapIncludeSources: true] })` now returns
  `{ css, sourceMap }` (the v3 map as a parsed object) instead of a bare CSS
  string; without `sourceMap` it still returns the string (backwards
  compatible). New `sasso_compile_map` export returns a framed `[u32 css_len][css]
  [map json]` buffer. Source maps are now exposed on every surface (lib, CLI,
  wasm).

### Fixed

- **Compressed source maps** now emit one segment per source line, matching
  dart-sass (compressed packs many tokens onto a line; dart maps only the first
  per source line). Expanded maps are unchanged. The map's CSS is unaffected.

## [0.4.0] - 2026-06-14

### Added

- **Source map (v3) support.** New `compile_with_source_map(source, &Options)
  -> CompileResult { css, source_map: SourceMap }`, with `SourceMap::to_json()`
  and `Options::with_source_map_include_sources(bool)`. The CLI gains
  `-o/--output <file>` (write CSS to a file), `--source-map` (also write a
  `<output>.map` sidecar + append the `sourceMappingURL` footer),
  `--embed-sources`, and `--source-map-urls=relative|absolute`. Output is
  byte-for-byte identical to dart-sass for the common cases (selector +
  declaration-name mappings; expanded + compressed). The plain `compile` path
  and stdout output are unchanged. (Deferred for now: declaration-value-start
  mappings, the inline `--embed-source-map` data URI.)

### Changed

- Internal maintainability refactors only (no behaviour change, byte-identical
  output): the `.sass` line scanners, the `is_builtin` name table, and the
  oversized `eval`/`selector`/`parser` files were split into domain modules;
  the string serializers gained a no-escape fast path.

## [0.3.1] - 2026-06-13

### Fixed

- Compressed output now emits a color's canonical CSS name when it is no longer
  than the shortest hex, matching dart-sass (`red` not `#f00`, `aqua` not
  `#0ff`; duplicate names resolve to dart's canonical pick — `cyan`/`grey` →
  `aqua`/`gray`). Expanded output (which preserves the authored spelling) is
  unchanged.

## [0.3.0] - 2026-06-13

Since `0.2.0`. Conformance holds at **100% of the attempted sass-spec suite**
(13,896 / 13,896) — but that suite covers *valid* inputs plus the errors it
expects; this cycle hardened sasso to reject the same *malformed* inputs
dart-sass rejects, and cut more of the `@extend` and value hot paths.

### Changed

- **Strict input validation.** Beyond matching dart-sass's output, sasso now
  *errors* — rather than silently accepting — on malformed input, each with
  dart-sass's exact message: an invalid hex literal (`#00000`, `#0g`),
  out-of-grammar `rgb()`/`hsl()` channel units and legacy-vs-modern argument
  shapes, a duplicate `@mixin`/`@function` parameter, a malformed number
  exponent (`1e-`), a non-identifier `@use`/`@forward` namespace, a misplaced
  `@content`/`@extend`, a style rule / declaration / `@extend` in a `@function`
  body, a map or empty list used as a CSS value (`#{(a:1)}`, `-()`), a
  malformed `:nth-child()` An+B or empty `:not()` selector, a stray `!` in a
  selector, a leading-empty `@extend` target, and a malformed `@charset` /
  `@at-root (…)` query. Found by a leniency-mining sweep that diffed every
  category against dart-sass; the fixes are uncovered by the spec, so the
  ratchet is unchanged.

### Performance

- **Transitive `@extend`** went from ~151× *slower* than dart-sass to *faster*
  on a deep extend chain: a match pre-filter with typed dedup, an incremental
  per-rule fold (killing the O(N²) closure re-derivation), borrowed
  scope-originals, and cached typed selector hashes — the `@extend` maps are
  now FxHash + typed `Complex`/`Simple` keys, guarded by a render-injectivity
  parity proof. Byte-identical output throughout.
- **Reference-counted composite values** — `Str`/`List`/`Map` are `Rc`-backed,
  so cloning a read-only `$variable` is an O(1) refcount bump instead of a deep
  copy (copy-on-write for the mutating builtins): ~7× fewer instructions and
  ~13× less peak memory when a large list/map is passed through a call chain.
- **`Cow`-borrowed argument-name normalization** (called 4–6× per function
  call) plus trimmed function-call-path allocations — ~15% fewer instructions
  on a function-heavy compile.
- **Arena in-place `realloc`** — the scoped bump arena extends its tail
  allocation in place instead of stranding a dead buffer on every `Vec`
  doubling, trimming peak memory on parse-heavy compiles.
- Net: pure-compile throughput ~7.4 ms on the large benchmark (was ~9–10),
  ~2.3–2.9× faster than `grass` and ~19–30× faster than the dart-sass JS bin.

### Internal

- `eval.rs` split into an `eval/` module directory and `color.rs` into a
  `color/` directory (pure code moves); the typed selector model gained a
  parity-proof harness; the stringly hoist markers became typed `OutNode`
  variants; the `@extend` cartesian-order bool became a `CartesianOrder` enum;
  `OutNode` rule/at-rule constructors collapsed duplicated construction sites.
  All byte-identical, each verified base-binary-vs-refactor.

### Tooling

- The WebAssembly npm package publishes via OIDC Trusted Publishing (no token).
- Benchmark harness uses portable temp-file handling (`mktemp -d`).

## [0.2.0] - 2026-06-11

Everything since the initial `0.1.0` crates.io publish. This grew the compiler
from an early vertical slice to **100% of the *attempted* official sass-spec
suite** (13,896 / 13,896, zero failures — 11,405 byte-exact CSS outputs plus
2,491 error specs correctly rejected; the 8 remaining cases are tagged `:todo`
for dart-sass itself upstream), matching current dart-sass (1.100) byte-for-byte.
The pass is measured against a conformance harness tightened to reproduce the
official sass-spec comparator (`normalizeOutput`) exactly — collapse newline
runs only, no extra whitespace leniency — so the count holds under the upstream
comparator, not just a looser local one.

### Added

- **Byte-exact diagnostics** — errors, `@error`, `@warn`, and `@debug` now
  reproduce dart-sass's stderr byte-for-byte: source-span `╷│╵` snippets with
  carets and right-aligned gutters (tab→4 spaces), aligned stack frames
  (`root stylesheet` / `name()` / `@import`), a `--no-unicode` flag, and the
  `@import` deprecation warning (with a per-id cap/dedup deprecation registry).
  238 of the suite's 3,256 stderr expectations now match byte-for-byte (a
  `spec/run_spec.py --check-stderr` metric tracks it); the rest (other
  deprecations, multi-span layouts) build on this foundation.
- **`@use` / `@forward` module system** — built-in `sass:*` modules and user
  files, `with` configuration, namespacing, `@forward` prefix/`show`/`hide`,
  dash-insensitive member access, forward conflict resolution, and star
  (`as *`) modules.
- **Indented `.sass` syntax** — a full front-end (`Options::with_syntax`, the
  CLI `--indented` flag, `.sass` extension inference), including cross-syntax
  `@import` of partials by file extension.
- **CSS Color 4 color spaces** — `srgb`/`display-p3`/`lab`/`lch`/`oklab`/
  `oklch`/`xyz` via `color()`, with modern color serialization.
- **`@extend` and `%placeholder`s** — a faithful port of dart-sass's
  `ExtensionStore` engine: registration-order extension folding, selector
  weaving/unification/trimming, `@use`/`@forward` cross-module visibility, and
  the self-referential `:not`/`:has` pseudo cases — closing the suite's
  `@extend` family to byte-exact parity.
- **Built-in function modules** — `meta` (first-class function references via
  `get-function`/`call`, existence predicates), `math` (`clamp`/`min`/`max`/
  `round`/`log` and friends), `list` (bracket-preserving `join`/`append`),
  `map` (nested key paths, `deep-merge`/`deep-remove`), `string`
  (`split`/`unique-id`), and `selector` functions.
- **First-class mixins** — `meta.get-mixin` returns a mixin value and
  `meta.apply` invokes it (with `@content` support).
- **CLI** — compile multiple input files in one process (`sasso a.scss b.scss`,
  startup shared across files); `--loop <N>` for in-process throughput and
  `-q`/`--quiet` to suppress stdout (used by the benchmark harness).
- **Benchmark harness** — sasso registered as a first-class engine in `bench/`;
  three-way report [`bench/three_way.md`](bench/three_way.md) (sasso vs
  dart-sass vs grass).
- **`CODE_OF_CONDUCT.md`** adopting the
  [Sass Community Guidelines](https://sass-lang.com/community-guidelines/), as
  the Sass project asks every implementation to do.
- **This CHANGELOG.**

### Changed

- Selector resolution now matches dart-sass on combinator normalization,
  adjacent-compound separation, and bogus-combinator omission.
- `color` functions match dart-sass strictness: channel-unit leniency in
  `adjust`/`change`, missing/powerless-channel errors, the Microsoft `alpha()`
  filter overload, and `adjust-hue` rejecting non-legacy colors.
- `selector` functions coerce string/list arguments and validate arity, and
  accept a list of extendees in `extend`/`replace`.
- `list` builtins validate fixed-arity arguments and preserve list shape.
- Unquoted string serialization collapses newlines to spaces; custom-property
  values are emitted verbatim — both matching dart-sass.
- Control-flow blocks use semi-global scoping with a global-write guard.

### Performance

Profiling showed the compiler is allocation- and hashing-bound; a series of
hot-path cuts followed, with no behavior change (cumulative **~2× faster** on the
large benchmark vs. the original, lifting the lead over `grass` to ~1.9–2.4× and
over dart-sass to ~16–25×):

- Selector helpers `split_commas`/`tokenize_complex` return borrowed `&str`
  slices, and `copy_name`/`normalize_selector` avoid their intermediate
  `String`/`Vec` — no per-part/per-token/per-name heap allocation on the hot
  selector-resolution path.
- The compiler's internal `String`-keyed maps (variable scope, function/mixin
  tables, module maps) use a small inline FxHash hasher instead of std's
  DoS-resistant-but-slow SipHash. (Still zero runtime dependencies.)
- A **scoped bump-arena allocator** (`ScopedAlloc`): within each `compile()` a
  per-thread arena turns every allocation into a pointer bump and frees them
  wholesale (reset) at the end — a further ~1.5×. It is installed as the CLI's
  `#[global_allocator]`; library/wasm embedders can opt in the same way (it
  forwards to the system allocator outside a compile, so it's safe to install
  unconditionally). This is the library's one audited `unsafe` module —
  verified by unit tests, Miri (no UB), AddressSanitizer, and the full sass-spec
  suite run through it (zero crashes, byte-identical output); the
  rest of the crate is `deny(unsafe_code)`. Still zero runtime dependencies.
- **Smaller `Value`** — the `Color` variant's modern-color payload is boxed, so
  the `Value` enum drops from 128 to **64 bytes** (a compile-time `size_of`
  guard prevents regressions). Halves every scope-map slot, `Vec<Value>` element
  and lookup clone, with byte-identical output.
- **Zero-dependency Ryū float formatter** — a from-scratch `d2s` shortest-round-
  trip formatter on the float-to-string hot path, replacing `core::fmt`, with a
  differential fuzz test against a reference.

### Tooling

- The conformance ratchet pins the sass-spec commit (`spec/SPEC_VERSION.txt`)
  for reproducibility, with a `--latest`/`--canary` drift-detection mode.
- The conformance harness now reproduces the official sass-spec comparator
  (`sass-spec/lib/test-case/compare.ts` `normalizeOutput`) exactly, so a "pass"
  means byte-identical under the upstream comparator — no extra local leniency.

## [0.1.0] - 2026-06-06

Initial crates.io publish — an early vertical slice that already compiled
real-world SCSS byte-identically to dart-sass.

### Added

- `$variables` with lexical scoping, `!default` and `!global`.
- Nesting, the `&` parent selector with selector-list multiplication, and
  combinator normalization (`>`, `+`, `~`).
- `#{}` interpolation in selectors, property names and values.
- `//` (stripped) and `/* */` (preserved) comments.
- Numbers with units and unit arithmetic.
- A color model with fractional channels and author-spelling preservation;
  color functions (`rgb`/`rgba`/`hsl`/`hsla`/`mix`/`lighten`/`darken`/
  `percentage`/`red`/`green`/`blue`/`alpha`).
- `@import` partial inlining through a pluggable `Importer` (CSS imports pass
  through); a ready-made `FsImporter`.
- `expanded` and `compressed` output styles.
- Distribution: CLI binary (prebuilt via cargo-dist), library crate, and a
  zero-dependency WebAssembly build published to npm as `@momiji-rs/sasso`.

[Unreleased]: https://github.com/momiji-rs/sasso/compare/v0.9.1...HEAD
[0.9.1]: https://github.com/momiji-rs/sasso/compare/v0.9.0...v0.9.1
[0.9.0]: https://github.com/momiji-rs/sasso/compare/v0.8.1...v0.9.0
[0.8.1]: https://github.com/momiji-rs/sasso/compare/v0.8.0...v0.8.1
[0.8.0]: https://github.com/momiji-rs/sasso/compare/v0.7.0...v0.8.0
[0.7.0]: https://github.com/momiji-rs/sasso/compare/v0.6.3...v0.7.0
[0.3.0]: https://github.com/momiji-rs/sasso/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/momiji-rs/sasso/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/momiji-rs/sasso/releases/tag/v0.1.0
