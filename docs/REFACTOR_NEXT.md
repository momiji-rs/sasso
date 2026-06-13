# sasso refactor plan — round 2 (post-campaign)

Status snapshot: master @`40f50a4`, **13895/13896 sass-spec**, zero runtime deps,
`unsafe` only in `src/arena.rs`, byte-exact dart-sass output.

The original [`REFACTOR_PLAN.md`](./REFACTOR_PLAN.md) is **fully shipped** — every
sequenced step landed (eval.rs split, Color boxed, the whole selector campaign,
OutNode constructors, typed hoist markers, CartesianOrder rename, Cow arg-names,
`Rc<str>`/Rc List·Map, color.rs split), plus an extra round of `@extend`
incrementalisation and the arena in-place realloc. This round catalogues what
that campaign **left behind** — derived from a fresh 4-subsystem re-analysis of
the current tree (eval / front-end / value+arena+emit / selector+builtins).

## Round-2 status (2026-06-13)

- **T1.1 LineScanner** — ✅ shipped `0e282dd` (byte-identity parity oracle).
- **T1.2 eval split** — ✅ shipped `35ae035` (eval/mod.rs 7084→6108; base==refactor 13904 cases, 0 diff).
- **T2.3 string-serializer fast-path** — ✅ shipped `39f3c51` (string-dense −4.0%, general flat; byte-identical).
- **T1.3 is_builtin single source** — ✅ shipped `dc189b9` (per-family `NAMES` consts; equivalence test proved no pre-existing drift).
- **T2.1 Rc-back callable env** — ❌ **CLOSED (bench-first showed flat).** A 600k-`@function` profile (`sample`) put `capture_callable` at ~1% of top-of-stack even on that pathological corpus; the dominant definition-time cost is the intrinsic `HashMap` insert/rehash + arena alloc, which T2.1's Vec-clone optimization does not touch. The maximal design (Rc-ifying the live scope stack + `Rc::make_mut` COW, which ~negates the call-path save) carries a large blast radius and the scope-lazy-frame correctness hazard for a ~0% real-world gain. Per the tier's "if a bench is flat, close it" rule — the general path has no remaining big lever.
- **T1.4 extract selector/parse.rs** — ✅ shipped `c84e9a4` (selector.rs 5175→4634 + selector/parse.rs 551; the @extend/weave core stays in mod.rs; byte-identity over 1232 cases incl 581 @extend). The `parser.rs` domain split is deliberately NOT done standalone — per this doc it folds into the next PR that already touches parser.rs (a standalone split is pure churn).
- **T2.2 Slash → Rc<str>** — ❌ **CLOSED (bench-first showed flat).** `Value::Slash` never surfaced in a `sample` profile of a 23 MB slash-dense corpus; Slash is rare and its `String` clone is not a measurable cost. Consistency-only motivation does not justify the churn.
- **T2.4 borrowed-buffer compressed emit** — ❌ **CLOSED (bench-first showed flat).** On a pure-compressed 11 MB corpus (T2.4's best case), the `join_generic_copy` it removes was ~0.8% of total and the whole compressed serializer only ~7.4%; the run is dominated by eval + parser. Expanded output (the default, common case) already streams into `&mut String` so T2.4 does nothing there. A ~0.8%-on-compressed-only win against the don't-touch `emit.rs` at med risk is negative ROI.

**Round-2 is complete: 5 items shipped (T1.1, T1.2, T2.3, T1.3, T1.4-selector), 3 closed bench-first (T2.1, T2.2, T2.4). The doc's thesis holds — the general path has no remaining big lever; the one real perf win this round was T2.3 (string-dense −4%).**

---

## The honest framing

The engine is structurally healthy. The big **memory** levers (Value size,
read-only clone cost, extend-path malloc storm) and the big **perf** levers are
already pulled. The project's own measured verdict still holds:

> **The general (non-extend) path has no remaining big lever.** Do not
> micro-tune `ryu` / `fmt_num` / `eval_expr_inner` — they are intrinsic
> byte-exact costs.

So round 2 is **mostly maintainability** (collapse the last oversized files and
the duplicated code the campaign couldn't reach), with a short list of
**marginal perf/memory items gated behind a bench** — none are merged on a
hunch.

## Hard constraints (unchanged, every proposal respects these)

1. **Zero runtime dependencies** — `std` + the in-house `src/fxhash.rs` only.
2. **No new `unsafe`** — `arena.rs` keeps the sole `unsafe`.
3. **Byte-exact output is the contract** — gated by `spec/check_baseline.py`
   (≥13895) + `tests/parity.rs`.
4. **Preserve provenance** — keep the dart-sass method-name doc comments verbatim
   through any move/rename.
5. **No `Co-Authored-By` trailer** on commits.

## Don't-do list (verified low/negative ROI)

- **Scanner `Vec<char>` → byte-cursor.** Correctly deferred. The scanner runs
  once per file, off the hot path; the eval arena dominates peak RSS. Byte-offset
  line-tracking over UTF-8 is error-prone for a win that doesn't move the needle.
  Revisit only if a `massif` run shows the scanner >5% of peak RSS.
- **`parser.rs` value hot path** (lines ~4248–5722). Clone density ~0.14%;
  already a clean single-pass recursive descent. Splitting it buys 0 perf.
- **`diag.rs`.** Error-path only — never on a success compile. No hot-path cost.

---

## Tier 1 — maintainability (high confidence, low risk; do these first)

### T1.1 — Collapse the 8 duplicate `.sass` scanners into one `LineScanner`
`[maintainability (+small memory) · S · low risk]`
`sass_parser.rs` (~1026–1515) hand-rolls **8 near-identical ad-hoc scanners**
(`split_top_level_commas`, `strip_silent_comment`, `custom_value_open`,
`interp_open_anywhere`, `scan_state`, `find_decl_colon`, `has_top_level_using`,
`find_top_level_semicolon`). Each does `s.chars().collect()` then re-runs the
same quote / bracket-depth / `#{}` / `/* */` state machine, differing only in the
final decision. Extract one borrowed-`&[char]` `LineScanner` with a
predicate-based `find_top_level` + state queries; each function becomes a
one-line wrapper. Pure refactor — the state machine is identical. This is the
single clearest code smell left in the tree. (`.sass` is off the `.scss` hot
path, so the allocation saving is a bonus, not the motivation.)

### T1.2 — Finish the `eval/mod.rs` split (still 7084 lines, the biggest file)
`[maintainability · S · negligible risk]`
The Phase-0a split stopped at the method level; `impl Evaluator` is still one
~2700-line block. Carve two more pure-code-move modules, no behaviour change:
- `eval/expr.rs` — `eval_expr_inner` (~2686–3283) + its expression-only helpers
  (`eval_if_function`, `eval_supports_calc_func`, module-call dispatch).
- `eval/scope.rs` — the environment layer (`lookup`/`var`/`assign`/`set_local`/
  `bind_each`/`assign_module_var`/scope-stack ops, ~1485–1774).
This is the prerequisite that keeps T2.1 (the one perf item that touches eval) a
reviewable diff.

### T1.3 — Kill the `is_builtin()` two-point sync hazard
`[maintainability · correctness hazard · M · med risk]`
`builtins/mod.rs` (~92–167) hardcodes a builtin-name match that **must** be kept
in sync with each family's `try_call` arms *and* with `is_math_builtin_name()`.
Adding a builtin and forgetting `is_builtin()` silently mis-routes a name between
builtin and plain-CSS-function handling. Drive all three from one declarative
source (a table or macro that mirrors the family dispatch) so a new builtin is a
single-site change. Gate on the full spec — mis-classification is byte-observable.

### T1.4 — Domain-split `parser.rs` (6724) and extract `selector/parse.rs`
`[maintainability · M · low risk]`
Pure file moves, no behaviour change. `parser.rs` has clean seams:
`statements` / `declarations` / `at_rules` (~1105–2680, the largest) /
`callables` / `control_flow` / `value`. `selector.rs` (5175) can shed its parser
(~685–1224) into `selector/parse.rs`; the `@extend`/superselector/weave core is
mutually recursive and should **stay together** (splitting it would force
internal `DComplex`/`TComp`/`CartesianOrder` types across module lines for no
gain). **Lower urgency** than T1.1–T1.3 — best folded into the next PR that
already touches these files, rather than a standalone churn-PR.

---

## Tier 2 — perf / memory (marginal; each gated behind a before/after bench)

> Merge only with a `bench/scripts/run_bench.sh` before/after showing a real win
> (instructions-retired via `/usr/bin/time -l`, plus interleaved min-of-N wall).
> If a bench is flat, close it — the general path is already optimal.

### T2.1 — `Rc`-back the captured callable environment `[perf+memory · M]`
**The one lever the original plan missed.** `capture_callable`
(`eval/mod.rs` ~1497) clones **four `Vec`s** (`scopes`, `scope_semi_global`,
`functions`, `mixins`) on *every* `@function`/`@mixin` definition, and the same
4-Vec `mem::replace`/restore dance repeats at ~9 call sites in `control_flow.rs`
+ `meta.rs`. Change `UserCallable`'s env fields to `Rc<Vec<…>>` so capture +
apply become refcount bumps; collapse the restore boilerplate into a `SavedEnv`
helper. Meaningful for function/mixin-dense or deeply-nested sheets.
⚠️ **Risk:** the scope-lazy-frame interaction the original plan flagged —
`capture_callable` must still see *later* sibling definitions. Needs a targeted
parity test, not just the ratchet.

### T2.2 — `Value::Slash` repr → `Rc<str>` `[perf · S]`
`Slash(Number, String)` (`value.rs:34`) clones its CSS repr on every serialize
(read-many, write-once). Align with the already-`Rc`-backed Str/List/Map.

### T2.3 — Fast-path the string serializers `[perf · S]`
`serialize_quoted`/`serialize_unquoted` (`value.rs` ~452/487) `chars().collect()`
into a `Vec<char>` *before* the no-escape early-out. Do a byte scan for the
escape triggers first; only collect when an escape is actually present.

### T2.4 — Borrowed-buffer compressed emit `[perf · M · med risk]`
The compressed path (`emit.rs` ~477–500) builds `Vec<String>` then `join`s twice
per nested rule; the expanded path already writes incrementally into `&mut
String`. Thread the buffer through the compressed recursion to match. Lowest
priority of the tier — touches `emit.rs`, which the original plan put on the
don't-touch list, so only if the bench justifies it.

---

## Suggested sequence

1. **T1.1** (LineScanner) — isolated, satisfying, zero risk.
2. **T1.2** (eval split) — unblocks T2.1's reviewability.
3. **T2.1** (Rc callable env) — bench first; the only real perf/memory candidate.
4. **T1.3** (is_builtin source-of-truth) — removes a live footgun.
5. T1.4 / T2.2 / T2.3 / T2.4 — opportunistic, fold into adjacent work.

## Method (per the project's discipline)

Each step = atomic commit + a new `tests/parity.rs` case byte-verified vs
dart-sass + `spec/check_baseline.py` (≥13895, no regression) + clippy/fmt + a
`bench/` before/after for any T2 item. No `Co-Authored-By` trailer.
