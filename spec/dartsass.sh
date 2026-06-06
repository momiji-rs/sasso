#!/usr/bin/env bash
#
# dartsass.sh — a SASS_BIN-compatible wrapper around dart-sass via npx.
#
# Used ONLY to validate the harness itself: a correct sass implementation
# should score ~100% on the attempted (non-skipped) cases. If it doesn't,
# our normalization/comparison is wrong.
#
# Contract expected by run_spec.py's SASS_BIN:
#   <bin> [--style=expanded|compressed] <input-file>   -> CSS on stdout
#   deprecation/other warnings go to stderr (we capture stdout only).
#
# --no-source-map keeps stdout pure CSS.
exec npx --yes sass --no-source-map "$@"
