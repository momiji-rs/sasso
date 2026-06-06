#!/usr/bin/env bash
#
# fetch.sh — clone the OFFICIAL sass-spec suite into spec/sass-spec/
# and record the cloned commit SHA into spec/SPEC_VERSION.txt.
#
# We do a shallow clone (--depth 1) because we only ever score against a
# single snapshot at a time; the SHA in SPEC_VERSION.txt pins reproducibility.
#
# The upstream suite is large and is .gitignore'd — we never commit it.
#
# Usage:  bash spec/fetch.sh
#
set -euo pipefail

REPO_URL="https://github.com/sass/sass-spec.git"
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DEST="${HERE}/sass-spec"
VERSION_FILE="${HERE}/SPEC_VERSION.txt"

echo "==> Fetching sass-spec into: ${DEST}"

if [ -d "${DEST}/.git" ]; then
  echo "==> ${DEST} already exists; leaving it in place."
  echo "    (remove it and re-run to re-clone.)"
else
  if ! git clone --depth 1 "${REPO_URL}" "${DEST}" 2>clone.err; then
    echo "!! CLONE FAILED (likely no network). stderr:" >&2
    sed 's/^/   /' clone.err >&2 || true
    echo "" >&2
    echo "!! Could not fetch the official suite." >&2
    echo "!! The harness still works: a tiny hand-made sample lives in" >&2
    echo "!!   spec/sample-spec/  — point the runner at it with:" >&2
    echo "!!     python3 spec/run_spec.py --suite spec/sample-spec" >&2
    rm -f clone.err
    exit 1
  fi
  rm -f clone.err
fi

# Record the exact commit we cloned.
SHA="$(git -C "${DEST}" rev-parse HEAD)"
{
  echo "repo:   ${REPO_URL}"
  echo "commit: ${SHA}"
  echo "date:   $(git -C "${DEST}" log -1 --format=%cI)"
  echo "fetched_at: $(date -u +%Y-%m-%dT%H:%M:%SZ)"
} > "${VERSION_FILE}"

echo "==> Recorded version:"
cat "${VERSION_FILE}"

# Quick census so the operator sees what landed.
N_DIR=$(find "${DEST}/spec" -type f \( -name 'input.scss' -o -name 'input.sass' \) 2>/dev/null | wc -l | tr -d ' ')
N_HRX=$(find "${DEST}/spec" -type f -name '*.hrx' 2>/dev/null | wc -l | tr -d ' ')
echo "==> Directory-style inputs: ${N_DIR}"
echo "==> .hrx archives:          ${N_HRX}"
echo "==> Done."
