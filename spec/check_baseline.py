#!/usr/bin/env python3
"""sass-spec pass-rate ratchet.

Re-runs the conformance harness against a built `sasso` and fails if the
number of passing cases regresses below the committed baseline. Every feature
that raises the count should bump the baseline file in the same commit.

There are TWO ratchets, one per output style, each with its own baseline:

    expanded    scored against the suite's own output.css (sass-spec ships
                dart-sass's expanded output and nothing else)
    compressed  scored against spec/COMPRESSED_EXPECT.txt, a committed
                manifest of per-case digests of a pinned dart-sass's
                COMPRESSED output. sass-spec has no compressed expectation
                anywhere, so without that manifest a compressed run fails
                nearly every success case on whitespace alone and measures
                nothing. See spec/gen_compressed.py.

Usage:
    cargo build --release
    python3 spec/check_baseline.py                      # expanded
    python3 spec/check_baseline.py --style compressed
    SASS_BIN=/path/to/sasso python3 spec/check_baseline.py
"""
import argparse
import json
import os
import subprocess
import sys
from collections import Counter

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)

# Per-style ratchet wiring. The expanded entry reproduces the original
# invocation exactly, so its verdict is unchanged by this file gaining a second
# style.
STYLES = {
    "expanded": {
        "baseline": "BASELINE.json",
        "results": "results.json",
        "label": "sass-spec",
    },
    "compressed": {
        "baseline": "BASELINE_COMPRESSED.json",
        "results": "results-compressed.json",
        "label": "compressed",
        "expect": "COMPRESSED_EXPECT.txt",
    },
}


def pinned_spec_commit():
    path = os.path.join(HERE, "SPEC_VERSION.txt")
    if not os.path.exists(path):
        return None
    for line in open(path, encoding="utf-8"):
        if line.startswith("commit:"):
            return line.split(":", 1)[1].strip()
    return None


def check_manifest_is_current(expect_path, baseline):
    """A digest manifest is only an oracle for the pins it was generated from.

    Scoring against a manifest built from a different dart-sass, or a different
    sass-spec commit, silently measures the wrong thing — so refuse instead.
    Regenerating is a documented one-liner.
    """
    header = {}
    entries = 0
    for line in open(expect_path, encoding="utf-8"):
        if line.startswith("#"):
            body = line[1:].strip()
            if ":" in body:
                k, v = body.split(":", 1)
                if " " not in k:
                    header[k.strip()] = v.strip()
            continue
        if line.strip():
            entries += 1

    want_dart = baseline.get("dart_sass")
    got_dart = header.get("dart_sass")
    want_commit = pinned_spec_commit()
    got_commit = header.get("spec_commit")

    problems = []
    # A missing entry is a SKIP, by design: a manifest gap must not read as a
    # sasso regression. The cost of that choice is that dropping entries also
    # drops cases out of the denominator, so a truncated manifest could hide
    # the very failures it omits. Two things stop that: the header's own count
    # must match the body, and main() refuses a shrunken `attempted`.
    want_cases = header.get("cases")
    if want_cases and want_cases.isdigit() and int(want_cases) != entries:
        problems.append(f"{entries} digest lines but its header says "
                        f"{want_cases} cases")
    if want_dart and got_dart and want_dart != got_dart:
        problems.append(f"dart-sass {got_dart} in the manifest vs "
                        f"{want_dart} in the baseline")
    if want_commit and got_commit and want_commit != got_commit:
        problems.append(f"sass-spec {got_commit[:12]} in the manifest vs "
                        f"{want_commit[:12]} in SPEC_VERSION.txt")
    if problems:
        print("error: spec/COMPRESSED_EXPECT.txt is stale — "
              + "; ".join(problems), file=sys.stderr)
        print("Regenerate it: python3 spec/gen_compressed.py --jobs 10",
              file=sys.stderr)
        return False
    return True


def main() -> int:
    ap = argparse.ArgumentParser(description="sass-spec pass-rate ratchet")
    ap.add_argument("--style", choices=sorted(STYLES), default="expanded",
                    help="which ratchet to run (default: expanded)")
    args = ap.parse_args()
    cfg = STYLES[args.style]

    baseline = json.load(open(os.path.join(HERE, cfg["baseline"])))
    sass_bin = os.environ.get("SASS_BIN", os.path.join(ROOT, "target", "release", "sasso"))
    if not os.path.exists(sass_bin):
        print(f"error: compiler binary not found at {sass_bin} (run `cargo build --release`)", file=sys.stderr)
        return 2
    if not os.path.isdir(os.path.join(HERE, "sass-spec", "spec")):
        print("error: sass-spec not present — run spec/fetch.sh first", file=sys.stderr)
        return 2

    extra = []
    if "expect" in cfg:
        expect = os.path.join(HERE, cfg["expect"])
        if not os.path.exists(expect):
            print(f"error: {expect} not found — generate it with "
                  "`python3 spec/gen_compressed.py`", file=sys.stderr)
            return 2
        if not check_manifest_is_current(expect, baseline):
            return 2
        extra = [f"--style={args.style}", "--expect-file", expect]

    out = os.path.join(HERE, cfg["results"])
    # The harness exits 1 whenever any case fails, which is the normal state of
    # a ratchet run, so its status cannot simply be trusted. But 2 means it
    # never scored anything (bad flags, a style/manifest mismatch, a missing
    # suite) and an earlier run's results file must not be read as this run's.
    if os.path.exists(out):
        os.remove(out)
    env = {**os.environ, "SASS_BIN": sass_bin}
    proc = subprocess.run(
        [sys.executable, os.path.join(HERE, "run_spec.py"), "--quiet", "--out", out] + extra,
        cwd=ROOT, env=env, check=False,
    )
    if proc.returncode not in (0, 1):
        print(f"error: run_spec.py exited {proc.returncode} without scoring "
              "the suite — see its message above", file=sys.stderr)
        return 2
    if not os.path.exists(out):
        print(f"error: run_spec.py wrote no results to {out}", file=sys.stderr)
        return 2
    cases = json.load(open(out))["cases"]
    c = Counter(x["status"] for x in cases)
    passes, err, fail = c.get("PASS", 0), c.get("ERROR_EXPECTED", 0), c.get("FAIL", 0)
    passing = passes + err
    attempted = passing + fail

    label = cfg["label"]
    print(f"{label:<12}: passing={passing} (pass={passes} error_expected={err}) attempted={attempted}")
    print(f"baseline    : passing={baseline['passing']} (pass={baseline['pass']})")
    delta = passing - baseline["passing"]
    print(f"delta       : {delta:+d} passing")

    want_attempted = baseline.get("attempted")
    if want_attempted is not None and attempted < want_attempted:
        print(f"REGRESSION: {want_attempted - attempted} case(s) left the "
              f"denominator — attempted {attempted} vs {want_attempted} in the "
              f"baseline.", file=sys.stderr)
        print("A case that stops being scored cannot be seen to fail. If the "
              "drop is intended (suite pin moved), bump "
              f"spec/{cfg['baseline']} in the same commit.", file=sys.stderr)
        return 1
    if passing < baseline["passing"] or passes < baseline["pass"]:
        print("REGRESSION: pass count dropped below the committed baseline.", file=sys.stderr)
        return 1
    if delta > 0:
        print(f"NOTE: {delta} new passing case(s) — bump spec/{cfg['baseline']} "
              "in this commit.")
    print("ratchet OK")
    return 0


if __name__ == "__main__":
    sys.exit(main())
