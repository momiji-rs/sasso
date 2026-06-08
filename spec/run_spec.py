#!/usr/bin/env python3
"""
run_spec.py — conformance harness for the OFFICIAL sass-spec suite.

Measures a SCSS->CSS compiler against https://github.com/sass/sass-spec.

It walks the suite (handling BOTH directory-style cases and .hrx archives),
extracts each (input, expected-output-or-error, options) triple, compiles the
input with the compiler named by $SASS_BIN (default target/release/sasso),
normalizes whitespace per sass-spec norms, compares, and categorizes each case:

    PASS            output matched expected output.css
    FAIL            output differed, or we errored where output was expected
                    (or vice-versa)
    ERROR_EXPECTED  spec expects an error AND the compiler errored (a "pass"
                    for error specs)
    SKIP            out-of-scope feature (tagged) or todo/ignore-for-impl

It writes spec/results.json and prints a summary including pass% of attempted
(PASS+ERROR_EXPECTED+FAIL, i.e. non-skipped) and pass% of total.

Run against dart-sass (to validate the harness):
    SASS_BIN=spec/dartsass.sh python3 spec/run_spec.py --limit 250

Run against our binary once it's built:
    SASS_BIN=target/release/sasso python3 spec/run_spec.py

The expectation we validate against is the dart-sass one: when an
implementation-specific expectation (output-dart-sass.css / error-dart-sass /
warning-dart-sass) exists, it overrides the generic file. This matches how a
real Sass implementation is scored. Set --impl to change the implementation
name used for these overrides and for :todo / :ignore_for filtering.
"""
from __future__ import annotations

import argparse
import json
import os
import re
import subprocess
import sys
import tempfile
from dataclasses import dataclass, field, asdict
from pathlib import Path
from typing import Optional

# --------------------------------------------------------------------------- #
# Configuration
# --------------------------------------------------------------------------- #

HERE = Path(__file__).resolve().parent
DEFAULT_SUITE = HERE / "sass-spec" / "spec"
DEFAULT_BIN = "target/release/sasso"

# The HRX file-boundary marker: a line beginning "<===>".
# Form 1:  "<===> path/to/file"      -> start of a virtual file
# Form 2:  "<===>"                    -> a comment / separator (body discarded)
HRX_MARKER = re.compile(r"^<===> ?(.*)$")

# Files we recognise inside a case directory (virtual or physical).
INPUT_NAMES = ("input.scss", "input.sass")


# --------------------------------------------------------------------------- #
# Skip taxonomy — TAGGED, configurable. Each tag has a human reason and a
# predicate over the input text / case metadata. These mark features that are
# out of scope for sasso *today* so the report can show
# "would-attempt vs skipped". Disable any tag with --no-skip <tag>.
# --------------------------------------------------------------------------- #

SKIP_TAGS = {
    "indented-syntax": "uses the .sass indented syntax (input.sass)",
    "use": "uses the @use module-system rule",
    "forward": "uses the @forward module-system rule",
    "extend": "uses @extend / placeholder %selectors",
}

# Regexes used by content-based skip tags. @use "sass:math" etc. all count.
RE_USE = re.compile(r'(?m)^\s*@use\b')
RE_FORWARD = re.compile(r'(?m)^\s*@forward\b')
# @extend rule, or a placeholder selector like %foo used as a selector.
RE_EXTEND = re.compile(r'(?m)(^\s*@extend\b|^\s*%[\w-]+\s*[,{])')


# --------------------------------------------------------------------------- #
# Data model
# --------------------------------------------------------------------------- #

@dataclass
class Case:
    """One spec case: a virtual/physical directory holding an input file."""
    name: str                       # stable id, e.g. spec/css/comment/foo:bar
    input_name: str                 # input.scss | input.sass
    input_text: str
    expected_css: Optional[str]     # None if this is an error spec
    expects_error: bool
    # Expected STDERR text for the --check-stderr metric, independent of the
    # CSS verdict. Populated from a warning/warning-<impl>/error/error-<impl>
    # expectation file when one exists; None when the case has none.
    expected_stderr: Optional[str] = None
    stderr_kind: Optional[str] = None  # "warning" | "error" (which file kind)
    extra_files: dict = field(default_factory=dict)  # other .scss/.css siblings
    options: dict = field(default_factory=dict)      # merged options.yml
    precision: Optional[int] = None
    archive_id: str = ""                             # the case's archive/dir, suite-relative
    archive_files: dict = field(default_factory=dict)  # whole archive {rel: content} — for
    # load-path-relative imports like `@use 'core_functions/.../utils'` that resolve against
    # the suite root rather than the case dir (materialized under <tmp>/<archive_id>/ + -I <tmp>).


@dataclass
class Result:
    name: str
    status: str                     # PASS|FAIL|ERROR_EXPECTED|SKIP
    skip_tag: Optional[str] = None
    reason: Optional[str] = None
    input_name: str = "input.scss"
    # --check-stderr metric (additive; only set when the flag is on AND the
    # case has a warning/error expectation file). None => not measured.
    #   stderr_status: "MATCH" | "MISMATCH"
    #   stderr_kind:   "warning" | "error"
    stderr_status: Optional[str] = None
    stderr_kind: Optional[str] = None


# --------------------------------------------------------------------------- #
# options.yml parsing (intentionally minimal — only the keys we act on)
# --------------------------------------------------------------------------- #

def parse_options_yml(text: str) -> dict:
    """Parse the tiny subset of YAML used by sass-spec options.yml.

    Recognises:
        :precision: N
        :todo:           (list of impl tokens, '- foo' lines)
        :warning_todo:   (list)
        :ignore_for:     (list)
    Everything else is ignored. We avoid a YAML dependency on purpose.
    """
    opts: dict = {}
    cur_list_key = None
    for raw in text.splitlines():
        line = raw.rstrip("\n")
        if not line.strip() or line.strip() == "---":
            continue
        # list item belonging to the most recent list key
        m = re.match(r'^\s*-\s*(.+?)\s*$', line)
        if m and cur_list_key:
            opts.setdefault(cur_list_key, []).append(m.group(1).strip())
            continue
        # key line
        m = re.match(r'^\s*:(\w+):\s*(.*)$', line)
        if m:
            key, val = m.group(1), m.group(2).strip()
            if key == "precision":
                try:
                    opts["precision"] = int(val)
                except ValueError:
                    pass
                cur_list_key = None
            elif key in ("todo", "warning_todo", "ignore_for"):
                cur_list_key = key
                if val:  # inline value (rare)
                    opts.setdefault(key, []).append(val)
            else:
                cur_list_key = None
    return opts


def impl_in_list(items, impl: str) -> bool:
    """A todo/ignore list entry may be a bare impl name ('dart-sass') or a
    GitHub issue shorthand ('sass/dart-sass#123'). Match the impl substring."""
    if not items:
        return False
    for it in items:
        it = it.strip()
        if it == impl or it.startswith(impl) or ("/" + impl) in it or it.split("#", 1)[0].endswith(impl):
            return True
    return False


# --------------------------------------------------------------------------- #
# HRX parsing
# --------------------------------------------------------------------------- #

def parse_hrx(text: str) -> dict:
    """Parse one .hrx archive into {virtual_path: content}.

    The HRX format: a line "<===> path" starts a virtual file whose body is
    every subsequent line until the next marker. A bare "<===>" marker (no
    path) introduces a comment block whose body is discarded.

    Bodies are stored verbatim except that the single trailing newline that
    HRX inserts before the next marker is stripped (sass-spec's own reader
    treats the file content as ending right before the blank-line + marker).
    """
    files: dict = {}
    cur_path = None
    cur_lines: list[str] = []

    def flush():
        if cur_path is not None:
            body = "\n".join(cur_lines)
            # HRX puts a blank line between a file body and the next marker;
            # that blank line is a separator, not content. Strip exactly one
            # trailing newline if present.
            if body.endswith("\n"):
                body = body[:-1]
            files[cur_path] = body

    for line in text.split("\n"):
        m = HRX_MARKER.match(line)
        if m:
            flush()
            path = m.group(1).strip()
            if path:
                cur_path = path
                cur_lines = []
            else:
                # comment/separator block: discard until next marker
                cur_path = None
                cur_lines = []
        else:
            if cur_path is not None:
                cur_lines.append(line)
    flush()
    return files


# --------------------------------------------------------------------------- #
# Case extraction
# --------------------------------------------------------------------------- #

def pick_expectation(files: dict, dirpath: str, impl: str):
    """Given the set of files (virtual or physical, keyed by path) and a case
    directory, return (expected_css, expects_error).

    Implementation-specific files override generic ones:
      output-<impl>.css  >  output.css
      error-<impl>       >  error
    If an impl-specific error exists but generic output does not (or vice
    versa) the impl-specific wins — it's even legal for impls to disagree on
    success vs error.
    """
    def get(name):
        key = f"{dirpath}/{name}" if dirpath else name
        return files.get(key)

    out_impl = get(f"output-{impl}.css")
    err_impl = get(f"error-{impl}")
    out_gen = get("output.css")
    err_gen = get("error")

    # Impl-specific expectation wins outright when present.
    if out_impl is not None and err_impl is None:
        return out_impl, False
    if err_impl is not None and out_impl is None:
        return None, True
    if out_impl is not None and err_impl is not None:
        # both impl-specific present (unusual): prefer output
        return out_impl, False

    # No impl-specific: use generic.
    if out_gen is not None:
        return out_gen, False
    if err_gen is not None:
        return None, True
    return None, None  # neither -> not a runnable case


def pick_stderr_expectation(files: dict, dirpath: str, impl: str):
    """Return (expected_stderr_text, kind) for the diagnostics metric, or
    (None, None) if this case has no warning/error expectation file.

    This is INDEPENDENT of pick_expectation (which only yields a bool
    expects_error for the CSS verdict). dart-sass's own runner stores the
    expected stderr in one of four files; impl-specific overrides win:

        error-<impl>   >  error      (kind="error")
        warning-<impl> >  warning    (kind="warning")

    A case can legitimately carry BOTH a warning file (deprecations printed on
    a successful compile) and an error file (only one of which dart-sass would
    actually emit per run); error wins for the purpose of "what's on stderr"
    only when the case errors, but the chosen-expectation precedence here
    mirrors how sass-spec attaches the *expected* stderr: the error spec's
    stderr is the error file; a pure-warning spec's stderr is the warning file.
    We therefore prefer error-* when present (an error case never also emits
    its warning file's body alone), else warning-*.
    """
    def get(name):
        key = f"{dirpath}/{name}" if dirpath else name
        return files.get(key)

    err = get(f"error-{impl}")
    if err is None:
        err = get("error")
    if err is not None:
        return err, "error"

    warn = get(f"warning-{impl}")
    if warn is None:
        warn = get("warning")
    if warn is not None:
        return warn, "warning"

    return None, None


def collect_case_dirs(files: dict):
    """Return sorted list of directories (by '/' prefix) that contain an
    input file. '' denotes the archive root."""
    dirs = set()
    for path in files:
        base = path.rsplit("/", 1)[-1]
        if base in INPUT_NAMES:
            d = path[: -(len(base) + 1)] if "/" in path else ""
            dirs.add(d)
    return sorted(dirs)


def options_for_dir(files: dict, dirpath: str) -> dict:
    """Merge options.yml that applies to a case dir. options.yml applies
    recursively to everything beneath it, so merge root -> ... -> dir."""
    merged: dict = {}
    segments = dirpath.split("/") if dirpath else []
    prefixes = [""]
    acc = ""
    for seg in segments:
        acc = f"{acc}/{seg}" if acc else seg
        prefixes.append(acc)
    for pre in prefixes:
        key = f"{pre}/options.yml" if pre else "options.yml"
        if key in files:
            merged.update(parse_options_yml(files[key]))
    return merged


def cases_from_files(files: dict, archive_id: str):
    """Yield Case objects from a {path: content} mapping (one HRX or one dir)."""
    # Longest-first case dirs so each file is attributed to its NEAREST enclosing
    # case (a partial under d/sub/ belongs to nested case d/sub, not d).
    _by_len = sorted(collect_case_dirs(files), key=len, reverse=True)

    def _owning_case(path: str) -> str:
        for cd in _by_len:
            if cd == "" or path == cd or path.startswith(cd + "/"):
                return cd
        return ""

    for d in collect_case_dirs(files):
        # locate the input file in this dir
        input_name = None
        for nm in INPUT_NAMES:
            key = f"{d}/{nm}" if d else nm
            if key in files:
                input_name = nm
                input_path = key
                break
        if input_name is None:
            continue

        expected_css, expects_error = pick_expectation(files, d, IMPL)
        if expected_css is None and not expects_error:
            continue  # no expectation -> not a runnable conformance case

        expected_stderr, stderr_kind = pick_stderr_expectation(files, d, IMPL)

        opts = options_for_dir(files, d)

        # Import partials for THIS case: every file under d/ that isn't claimed
        # by a deeper nested case, so subdir partials (d/foo/_bar.scss for
        # `@use "foo/bar"`) are materialized too — dart-sass's own runner
        # extracts the whole case tree, not just direct siblings.
        extra = {}
        prefix = f"{d}/" if d else ""
        for path, content in files.items():
            if not path.startswith(prefix):
                continue
            if _owning_case(path) != d:
                continue  # belongs to a deeper nested case
            rel = path[len(prefix):]
            if rel in INPUT_NAMES:
                continue
            # expectation / config files live at the case-dir top level
            if "/" not in rel and (
                rel.startswith("output")
                or rel.startswith("error")
                or rel.startswith("warning")
                or rel == "options.yml"
            ):
                continue
            extra[rel] = content

        name = f"{archive_id}:{d}" if d else archive_id
        yield Case(
            name=name,
            input_name=input_name,
            input_text=files[input_path],
            expected_css=expected_css,
            expects_error=expects_error,
            expected_stderr=expected_stderr,
            stderr_kind=stderr_kind,
            extra_files=extra,
            options=opts,
            precision=opts.get("precision"),
            archive_id=archive_id,
            archive_files=files,
        )


def iter_all_cases(suite: Path, suite_root: Path):
    """Walk the suite producing Case objects from both HRX and dir styles."""
    # 1) .hrx archives
    for hrx in sorted(suite.rglob("*.hrx")):
        rel = hrx.relative_to(suite_root).as_posix()
        archive_id = rel[:-4] if rel.endswith(".hrx") else rel  # strip .hrx
        try:
            text = hrx.read_text(encoding="utf-8")
        except UnicodeDecodeError:
            text = hrx.read_text(encoding="utf-8", errors="replace")
        files = parse_hrx(text)
        # HRX-applicable options.yml may also live as a *physical* sibling.
        yield from cases_from_files(files, archive_id)

    # 2) directory-style cases (physical input.scss / input.sass on disk)
    for inp in sorted(list(suite.rglob("input.scss")) + list(suite.rglob("input.sass"))):
        d = inp.parent
        files = {}
        for f in d.iterdir():
            if f.is_file():
                try:
                    files[f.name] = f.read_text(encoding="utf-8")
                except (UnicodeDecodeError, OSError):
                    files[f.name] = ""
        # physical options.yml in ancestor dirs (recursive). Walk up to suite.
        merged_opts = {}
        chain = []
        p = d
        while True:
            chain.append(p)
            if p == suite or p == suite_root or p.parent == p:
                break
            p = p.parent
        for anc in reversed(chain):
            oy = anc / "options.yml"
            if oy.exists():
                merged_opts.update(parse_options_yml(oy.read_text(encoding="utf-8")))

        expected_css, expects_error = pick_expectation(files, "", IMPL)
        if expected_css is None and not expects_error:
            continue
        expected_stderr, stderr_kind = pick_stderr_expectation(files, "", IMPL)
        rel = d.relative_to(suite_root).as_posix()
        extra = {
            k: v for k, v in files.items()
            if k not in INPUT_NAMES
            and not (k.startswith("output") or k.startswith("error")
                     or k.startswith("warning") or k == "options.yml")
        }
        yield Case(
            name=rel,
            input_name=inp.name,
            input_text=files[inp.name],
            expected_css=expected_css,
            expects_error=expects_error,
            expected_stderr=expected_stderr,
            stderr_kind=stderr_kind,
            extra_files=extra,
            options=merged_opts,
            precision=merged_opts.get("precision"),
            archive_id=rel,
            archive_files=files,
        )


# --------------------------------------------------------------------------- #
# Normalization & comparison
# --------------------------------------------------------------------------- #

def normalize_css(css: str) -> str:
    """Normalize trivial whitespace differences per sass-spec comparison norms.

    sass-spec compares ignoring:
      * leading/trailing whitespace of the whole document,
      * trailing whitespace on each line,
      * blank lines,
      * a possible UTF-8 BOM,
      * \\r\\n vs \\n line endings.
    It does NOT collapse interior significant whitespace (indentation in
    expanded output is meaningful), so we keep per-line indentation intact.
    """
    if css is None:
        return ""
    # strip BOM
    if css.startswith("﻿"):
        css = css[1:]
    css = css.replace("\r\n", "\n").replace("\r", "\n")
    lines = [ln.rstrip() for ln in css.split("\n")]
    # drop blank lines (sass-spec ignores them when comparing)
    lines = [ln for ln in lines if ln != ""]
    return "\n".join(lines).strip()


def normalize_stderr(text: str, input_basename: str, abs_input_path: str) -> str:
    """Normalize stderr the way dart-sass's OWN spec runner does before
    comparing diagnostics — and ONLY that, so the metric is faithful.

    sass-spec's runner runs the compiler with the input written at a path it
    controls, then rewrites that absolute path back to the relative
    placeholder ('input.scss' / 'input.sass') that the checked-in expectation
    files use, and compares ignoring trailing whitespace / line-ending /
    trailing-blank differences. We replicate exactly that:

      * the absolute temp input path  ->  the bare placeholder basename
        (both the full path and, defensively, its containing dir prefix),
      * CRLF/CR -> LF,
      * a possible UTF-8 BOM stripped,
      * trailing whitespace stripped from every line,
      * trailing blank lines / surrounding blank space stripped.

    We do NOT touch glyphs, gutter widths, caret columns, message wording or
    the deprecation [id] tags — those ARE the diagnostics under test and must
    match byte-for-byte (post-path-normalization) to count.
    """
    if text is None:
        return ""
    if text.startswith("﻿"):
        text = text[1:]
    text = text.replace("\r\n", "\n").replace("\r", "\n")
    # Map the absolute path the compiler actually saw back to the placeholder
    # the expectation files use. Replace the full path first (longest match),
    # then the containing directory prefix so 'dir/input.scss' -> 'input.scss'.
    if abs_input_path:
        text = text.replace(abs_input_path, input_basename)
        parent = os.path.dirname(abs_input_path)
        if parent:
            text = text.replace(parent + os.sep, "")
    lines = [ln.rstrip() for ln in text.split("\n")]
    return "\n".join(lines).strip()


# --------------------------------------------------------------------------- #
# Skip decision
# --------------------------------------------------------------------------- #

def decide_skip(case: Case, enabled_tags: set, impl: str):
    """Return (skip_tag, reason) if the case should be skipped, else None.

    Order: out-of-scope feature tags first (these are the sasso scope
    gate), then upstream :ignore_for / :todo for our impl.
    """
    text = case.input_text

    if "indented-syntax" in enabled_tags and case.input_name == "input.sass":
        return "indented-syntax", SKIP_TAGS["indented-syntax"]
    if "use" in enabled_tags and RE_USE.search(text):
        return "use", SKIP_TAGS["use"]
    if "forward" in enabled_tags and RE_FORWARD.search(text):
        return "forward", SKIP_TAGS["forward"]
    if "extend" in enabled_tags and RE_EXTEND.search(text):
        return "extend", SKIP_TAGS["extend"]

    # upstream metadata: never-expected-to-pass for this impl
    if impl_in_list(case.options.get("ignore_for"), impl):
        return "ignore_for", f"options.yml :ignore_for includes {impl}"
    # :todo means "not yet implemented for this impl" -> skip by default,
    # exactly as sass-spec.rb does without --run-todo.
    if impl_in_list(case.options.get("todo"), impl):
        return "todo", f"options.yml :todo includes {impl}"

    return None


# --------------------------------------------------------------------------- #
# Compilation
# --------------------------------------------------------------------------- #

def compile_case(case: Case, sass_bin: str, style: str,
                 capture_stderr: bool = False, extra_args=None):
    """Compile a case. Returns (stdout, returncode, stderr, abs_input_path).

    We write the input (and any extra sibling files, needed for @import) to a
    temp dir and pass the input path positionally. stdout is always captured.

    stderr is DISCARDED by default (capture_stderr=False) — this preserves the
    historical behavior exactly, so the default CSS verdict is byte-identical
    to before. When capture_stderr=True (the --check-stderr metric) we capture
    stderr too, and return the absolute input path so the caller can normalize
    that path back to the spec's 'input.scss' placeholder.

    extra_args: extra CLI flags appended ONLY when supplied (used by
    --check-stderr to pass e.g. --no-unicode, the flag sass-spec generated its
    ASCII-glyph expectation files with). Left empty by default so the default
    CSS run is unchanged."""
    with tempfile.TemporaryDirectory(prefix="sass-spec-") as td:
        tdp = Path(td)
        in_path = tdp / case.input_name
        in_path.write_text(case.input_text, encoding="utf-8")
        for rel, content in case.extra_files.items():
            fp = tdp / rel
            fp.parent.mkdir(parents=True, exist_ok=True)
            fp.write_text(content, encoding="utf-8")

        # Also reproduce the whole archive under <tmp>/<archive_id>/ and add it as
        # a load path, so suite-root-relative imports (e.g. shared fixtures like
        # `@use 'core_functions/.../utils'` that live outside the case dir) resolve
        # the way dart-sass's runner sees them. The flat layout above keeps
        # sibling/relative imports byte-identical, so currently-passing cases are
        # unaffected.
        load_paths = []
        if case.archive_files:
            base = tdp / case.archive_id if case.archive_id else tdp
            for rel, content in case.archive_files.items():
                fp = base / rel
                fp.parent.mkdir(parents=True, exist_ok=True)
                if not fp.exists():
                    fp.write_text(content, encoding="utf-8")
            load_paths = [tdp] + ([base] if case.archive_id else [])

        cmd = [sass_bin]
        # style flag (dart-sass accepts --style=...; sasso should too)
        cmd.append(f"--style={style}")
        for a in (extra_args or []):
            cmd.append(a)
        for lp in load_paths:
            cmd += ["-I", str(lp)]
        if case.precision is not None:
            # dart-sass ignores --precision (fixed at 10); sasso may use it.
            # Pass it only if the bin is not our dart wrapper to avoid noise.
            pass
        cmd.append(str(in_path))

        abs_input = str(in_path)
        try:
            proc = subprocess.run(
                cmd,
                stdout=subprocess.PIPE,
                stderr=(subprocess.PIPE if capture_stderr
                        else subprocess.DEVNULL),
                timeout=60,
            )
            out = proc.stdout.decode("utf-8", errors="replace")
            err = (proc.stderr.decode("utf-8", errors="replace")
                   if capture_stderr and proc.stderr is not None else "")
            return out, proc.returncode, err, abs_input
        except subprocess.TimeoutExpired:
            return "", 124, "", abs_input
        except FileNotFoundError:
            print(f"ERROR: SASS_BIN not found: {sass_bin}", file=sys.stderr)
            sys.exit(2)


def evaluate(case: Case, sass_bin: str, style: str,
             check_stderr: bool = False, stderr_args=None) -> Result:
    # The CSS-verdict run is ALWAYS flag-clean (no stderr_args), so the
    # PASS/FAIL/ERROR_EXPECTED classification — and therefore the baseline
    # ratchet — is byte-identical whether or not --check-stderr is on. We only
    # capture stderr here when no extra stderr_args are requested (the common
    # case), to avoid a second subprocess. When stderr_args ARE requested
    # (e.g. --no-unicode), we run a SEPARATE measurement compile below so the
    # extra flags can never perturb the CSS verdict.
    want_inline_stderr = check_stderr and not stderr_args
    stdout, rc, stderr_out, abs_input = compile_case(
        case, sass_bin, style, capture_stderr=want_inline_stderr,
        extra_args=None)
    errored = rc != 0

    # ---- CSS verdict (UNCHANGED — this drives the baseline ratchet) -------- #
    if case.expects_error:
        if errored:
            res = Result(case.name, "ERROR_EXPECTED", input_name=case.input_name)
        else:
            res = Result(case.name, "FAIL",
                         reason="expected an error but compiled successfully",
                         input_name=case.input_name)
    elif errored:
        res = Result(case.name, "FAIL",
                     reason=f"compiler errored (rc={rc}) on a success spec",
                     input_name=case.input_name)
    else:
        got = normalize_css(stdout)
        want = normalize_css(case.expected_css)
        if got == want:
            res = Result(case.name, "PASS", input_name=case.input_name)
        else:
            res = Result(case.name, "FAIL", reason="output mismatch",
                         input_name=case.input_name)

    # ---- stderr-conformance metric (ADDITIVE — never affects status) ------- #
    if check_stderr and case.expected_stderr is not None:
        if want_inline_stderr:
            measured_err, measured_abs = stderr_out, abs_input
        else:
            # separate, flag-carrying compile purely for the stderr metric
            _, _, measured_err, measured_abs = compile_case(
                case, sass_bin, style, capture_stderr=True,
                extra_args=stderr_args)
        got_err = normalize_stderr(measured_err, case.input_name, measured_abs)
        want_err = normalize_stderr(case.expected_stderr, case.input_name,
                                    measured_abs)
        res.stderr_kind = case.stderr_kind
        res.stderr_status = "MATCH" if got_err == want_err else "MISMATCH"

    return res


# --------------------------------------------------------------------------- #
# Main
# --------------------------------------------------------------------------- #

IMPL = "dart-sass"  # set from args in main(); used during case extraction


def main():
    global IMPL
    ap = argparse.ArgumentParser(description="sass-spec conformance harness")
    ap.add_argument("--suite", default=str(DEFAULT_SUITE),
                    help="path to the spec/ dir of a sass-spec checkout "
                         "(default: spec/sass-spec/spec), or any dir/sample")
    ap.add_argument("--filter", default=None,
                    help="only run cases whose name contains this substring")
    ap.add_argument("--limit", type=int, default=None,
                    help="run at most N cases (after filtering)")
    ap.add_argument("--style", choices=["expanded", "compressed"],
                    default="expanded")
    ap.add_argument("--impl", default="dart-sass",
                    help="implementation name for impl-specific expectations "
                         "and :todo/:ignore_for filtering")
    ap.add_argument("--no-skip", action="append", default=[],
                    metavar="TAG",
                    help="disable a scope skip tag (repeatable): "
                         + ", ".join(SKIP_TAGS))
    ap.add_argument("--run-skipped", action="store_true",
                    help="attempt skipped cases too (don't skip anything)")
    ap.add_argument("--out", default=str(HERE / "results.json"),
                    help="path for results.json")
    ap.add_argument("--quiet", action="store_true",
                    help="don't print per-FAIL diagnostics")
    ap.add_argument("--check-stderr", action="store_true",
                    help="ADDITIVE diagnostics metric: for cases that ship a "
                         "warning/warning-<impl>/error/error-<impl> expectation "
                         "file, also compare the compiler's STDERR to it (path "
                         "normalized to the spec placeholder + trailing-ws), and "
                         "report a separate stderr-conformance count. Does NOT "
                         "change the PASS/FAIL/ERROR_EXPECTED verdict or the "
                         "process exit status.")
    ap.add_argument("--stderr-arg", action="append", default=[],
                    metavar="FLAG",
                    help="extra CLI flag passed to the compiler ONLY for the "
                         "--check-stderr measurement run (repeatable). The "
                         "checked-in sass-spec expectation files use the ASCII "
                         "glyph set, i.e. they were generated with dart-sass "
                         "--no-unicode; pass --stderr-arg=--no-unicode for a "
                         "compiler that supports it to compare faithfully. "
                         "Carried on a SEPARATE compile so it never affects the "
                         "CSS verdict.")
    args = ap.parse_args()

    IMPL = args.impl
    sass_bin = os.environ.get("SASS_BIN", DEFAULT_BIN)
    # resolve relative SASS_BIN against repo root (parent of spec/)
    if not os.path.isabs(sass_bin):
        cand = (HERE.parent / sass_bin)
        if cand.exists():
            sass_bin = str(cand)

    suite = Path(args.suite)
    if not suite.is_absolute():
        suite = (Path.cwd() / suite).resolve()
    # allow passing either .../sass-spec or .../sass-spec/spec or a sample dir
    if (suite / "spec").is_dir() and not any(suite.glob("*.hrx")):
        suite_scan = suite / "spec"
    else:
        suite_scan = suite
    if not suite_scan.exists():
        print(f"ERROR: suite not found: {suite_scan}", file=sys.stderr)
        print("Run spec/fetch.sh first, or pass --suite spec/sample-spec",
              file=sys.stderr)
        sys.exit(2)

    # `extend` is implemented now, so its cases are attempted by default (the
    # tag remains available via --run-skipped / for opting back in, but is no
    # longer enabled out of the box).
    enabled_tags = set(SKIP_TAGS) - {"extend", "use", "forward", "indented-syntax"}
    for t in args.no_skip:
        enabled_tags.discard(t)
    if args.run_skipped:
        enabled_tags = set()

    print(f"Suite:    {suite_scan}")
    print(f"SASS_BIN: {sass_bin}")
    print(f"Impl:     {IMPL}   Style: {args.style}")
    print(f"Scope-skip tags enabled: "
          f"{sorted(enabled_tags) if enabled_tags else '(none)'}")
    print("-" * 70)

    # gather
    all_cases = list(iter_all_cases(suite_scan, suite_scan))
    if args.filter:
        all_cases = [c for c in all_cases if args.filter in c.name]

    results: list[Result] = []
    skip_breakdown: dict = {}
    attempted = 0

    for case in all_cases:
        if args.limit is not None and attempted >= args.limit:
            break

        skip = None if args.run_skipped else decide_skip(case, enabled_tags, IMPL)
        if skip:
            tag, reason = skip
            results.append(Result(case.name, "SKIP", skip_tag=tag,
                                  reason=reason, input_name=case.input_name))
            skip_breakdown[tag] = skip_breakdown.get(tag, 0) + 1
            continue

        attempted += 1
        res = evaluate(case, sass_bin, args.style,
                       check_stderr=args.check_stderr,
                       stderr_args=args.stderr_arg)
        results.append(res)
        if res.status == "FAIL" and not args.quiet:
            print(f"FAIL  {res.name}\n        ({res.reason})")

    # tally
    counts = {"PASS": 0, "FAIL": 0, "ERROR_EXPECTED": 0, "SKIP": 0}
    for r in results:
        counts[r.status] += 1

    total = len(results)
    n_attempted = counts["PASS"] + counts["FAIL"] + counts["ERROR_EXPECTED"]
    n_pass = counts["PASS"] + counts["ERROR_EXPECTED"]
    pass_pct_attempted = (100.0 * n_pass / n_attempted) if n_attempted else 0.0
    pass_pct_total = (100.0 * n_pass / total) if total else 0.0

    # ---- additive stderr-conformance tally (only when --check-stderr) ------ #
    stderr_summary = None
    if args.check_stderr:
        measured = [r for r in results if r.stderr_status is not None]
        s_match = sum(1 for r in measured if r.stderr_status == "MATCH")
        s_total = len(measured)
        by_kind: dict = {}
        for r in measured:
            k = r.stderr_kind or "unknown"
            slot = by_kind.setdefault(k, {"total": 0, "match": 0})
            slot["total"] += 1
            if r.stderr_status == "MATCH":
                slot["match"] += 1
        stderr_summary = {
            "cases_with_expectation": s_total,
            "stderr_match": s_match,
            "stderr_mismatch": s_total - s_match,
            "stderr_match_pct": round(100.0 * s_match / s_total, 2)
            if s_total else 0.0,
            "by_kind": by_kind,
        }

    # write results.json
    out = {
        "suite": str(suite_scan),
        "sass_bin": sass_bin,
        "impl": IMPL,
        "style": args.style,
        "filter": args.filter,
        "limit": args.limit,
        "summary": {
            "total": total,
            "attempted": n_attempted,
            "pass": counts["PASS"],
            "error_expected": counts["ERROR_EXPECTED"],
            "fail": counts["FAIL"],
            "skip": counts["SKIP"],
            "pass_including_error_expected": n_pass,
            "pass_pct_of_attempted": round(pass_pct_attempted, 2),
            "pass_pct_of_total": round(pass_pct_total, 2),
            "skip_breakdown": skip_breakdown,
        },
        "cases": [asdict(r) for r in results],
    }
    if stderr_summary is not None:
        out["stderr_summary"] = stderr_summary
    Path(args.out).write_text(json.dumps(out, indent=2), encoding="utf-8")

    # summary
    print("-" * 70)
    print(f"total            {total}")
    print(f"attempted        {n_attempted}   (PASS+FAIL+ERROR_EXPECTED)")
    print(f"  pass           {counts['PASS']}")
    print(f"  error_expected {counts['ERROR_EXPECTED']}   (error specs the compiler correctly rejected)")
    print(f"  fail           {counts['FAIL']}")
    print(f"skip             {counts['SKIP']}")
    if skip_breakdown:
        for tag in sorted(skip_breakdown):
            print(f"    {tag:16s} {skip_breakdown[tag]}")
    print("-" * 70)
    print(f"PASS% of attempted : {pass_pct_attempted:6.2f}%   "
          f"({n_pass}/{n_attempted})")
    print(f"PASS% of total     : {pass_pct_total:6.2f}%   ({n_pass}/{total})")
    if stderr_summary is not None:
        s = stderr_summary
        print("-" * 70)
        print("STDERR-conformance metric (additive; does not affect verdict)")
        print(f"  cases w/ warning|error expectation : "
              f"{s['cases_with_expectation']}")
        print(f"  stderr byte-match                  : {s['stderr_match']}")
        print(f"  stderr mismatch                    : {s['stderr_mismatch']}")
        print(f"  stderr match%                      : "
              f"{s['stderr_match_pct']:.2f}%")
        for kind in sorted(s["by_kind"]):
            slot = s["by_kind"][kind]
            print(f"    {kind:8s} {slot['match']}/{slot['total']}")
    print(f"results -> {args.out}")

    # exit non-zero if any real failures (useful for CI / ratchet)
    sys.exit(1 if counts["FAIL"] else 0)


if __name__ == "__main__":
    main()
