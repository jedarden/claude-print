#!/usr/bin/env bash
# check-claude-version-bump.sh — detection step of the contract-probe
# maintenance workflow (docs/notes/claude-contract-probes.md §Maintenance).
#
# Compares the live `claude --version` against the version this repo's
# contract evidence is pinned to (the **Measured against:** stamp in
# docs/notes/claude-contract-probes.md). Drift means the measured contracts
# (merge, --setting-sources suppression, once-per-turn Stop,
# max-turns-fires-no-Stop) are unverified for the installed version and the
# probe scripts must be re-run.
#
# Exit codes:
#   0  versions match — evidence is current, nothing to do
#   1  DRIFT — the installed version differs from the pinned one; re-run due
#   2  cannot determine (claude missing, or a version string unparseable)
#
# Read-only: runs `claude --version` only — no sandbox, no HOME writes, no
# model turns. Safe to run on a schedule or from CI (exit 1 = alert), which
# is the R-2 "CI alert on version change" signal the plan asks for, alongside
# the `target/last-claude-version.txt` artifact written by
# tests/version_compat.rs::test_claude_version_recorded.

set -u

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
DOC="$REPO_ROOT/docs/notes/claude-contract-probes.md"

version_token() {
    # First x.y.z-looking token in the given text, empty if none.
    printf '%s' "$1" | grep -oE '[0-9]+\.[0-9]+\.[0-9]+' | head -1 || true
}

PIN_LINE="$(grep -m1 '^\*\*Measured against:\*\*' "$DOC" || true)"
PIN_VERSION="$(version_token "${PIN_LINE:-}")"
if [ -z "$PIN_VERSION" ]; then
    echo "ERROR: no parsable 'Measured against:' stamp in $DOC" >&2
    exit 2
fi

if ! command -v claude >/dev/null 2>&1; then
    echo "claude not on PATH — cannot compare against pinned $PIN_VERSION"
    exit 2
fi
LIVE_LINE="$(claude --version 2>&1 | head -1)"
LIVE_VERSION="$(version_token "${LIVE_LINE:-}")"
if [ -z "$LIVE_VERSION" ]; then
    echo "ERROR: unparsable claude --version output: ${LIVE_LINE:-<empty>}" >&2
    exit 2
fi

echo "pinned (docs/notes/claude-contract-probes.md): $PIN_VERSION"
echo "live   (claude --version):                    $LIVE_VERSION"

if [ "$PIN_VERSION" = "$LIVE_VERSION" ]; then
    echo "CURRENT — contract evidence matches the installed version."
    exit 0
fi

echo "DRIFT — evidence is pinned to $PIN_VERSION but $LIVE_VERSION is installed."
echo "Re-run due (docs/notes/claude-contract-probes.md §Maintenance):"
echo "  1. cargo test --test claude_contracts -- --ignored   # cheap pre-check (~35 s)"
echo "  2. bash scripts/probe-claude-contracts.sh            # merge/suppression/Stop"
echo "  3. bash scripts/probe-stop-toolallowed.sh            # multi-round Stop (print)"
echo "  4. bash scripts/probe-tui-second-turn.sh             # TUI once-per-turn"
echo "  5. Re-pin doc stamp + fixture, or file follow-up beads if a contract moved."
exit 1
