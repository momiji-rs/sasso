#!/usr/bin/env python3
"""sass-spec pass-rate ratchet.

Re-runs the conformance harness against a built `sasso` and fails if the
number of passing cases regresses below spec/BASELINE.json. Every feature
that raises the count should bump BASELINE.json in the same commit.

Usage:
    cargo build --release
    python3 spec/check_baseline.py            # uses target/release/sasso
    SASS_BIN=/path/to/sasso python3 spec/check_baseline.py
"""
import json
import os
import subprocess
import sys
from collections import Counter

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)


def main() -> int:
    baseline = json.load(open(os.path.join(HERE, "BASELINE.json")))
    sass_bin = os.environ.get("SASS_BIN", os.path.join(ROOT, "target", "release", "sasso"))
    if not os.path.exists(sass_bin):
        print(f"error: compiler binary not found at {sass_bin} (run `cargo build --release`)", file=sys.stderr)
        return 2
    if not os.path.isdir(os.path.join(HERE, "sass-spec", "spec")):
        print("error: sass-spec not present — run spec/fetch.sh first", file=sys.stderr)
        return 2

    out = os.path.join(HERE, "results.json")
    env = {**os.environ, "SASS_BIN": sass_bin}
    subprocess.run(
        [sys.executable, os.path.join(HERE, "run_spec.py"), "--quiet", "--out", out],
        cwd=ROOT, env=env, check=False,
    )
    cases = json.load(open(out))["cases"]
    c = Counter(x["status"] for x in cases)
    passes, err, fail = c.get("PASS", 0), c.get("ERROR_EXPECTED", 0), c.get("FAIL", 0)
    passing = passes + err
    attempted = passing + fail

    print(f"sass-spec   : passing={passing} (pass={passes} error_expected={err}) attempted={attempted}")
    print(f"baseline    : passing={baseline['passing']} (pass={baseline['pass']})")
    delta = passing - baseline["passing"]
    print(f"delta       : {delta:+d} passing")

    if passing < baseline["passing"] or passes < baseline["pass"]:
        print("REGRESSION: pass count dropped below the committed baseline.", file=sys.stderr)
        return 1
    if delta > 0:
        print(f"NOTE: {delta} new passing case(s) — bump spec/BASELINE.json in this commit.")
    print("ratchet OK")
    return 0


if __name__ == "__main__":
    sys.exit(main())
