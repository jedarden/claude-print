#!/usr/bin/env bash
# check-claude-version-bump.sh — detection step of the contract-probe
# maintenance workflow (docs/notes/claude-contract-probes.md §Maintenance).
#
# Compares the live `claude --version` against every active version pin this
# repo's contract evidence carries: the **Measured against:** stamp in
# docs/notes/claude-contract-probes.md, the active claude_contracts fixture,
# and the active stream-json golden fixture family. Drift means the measured
# contracts (merge, --setting-sources suppression, once-per-turn Stop,
# max-turns-fires-no-Stop, and the stream-json wire-format goldens) are
# unverified for the installed version and the probe/capture paths must be
# re-run.
#
# It also rejects DIVERGENT active pins outright (claudepr-b590e46d): the doc
# stamp and both active fixture families are one measurement of one Claude
# version, so they must agree with each other before anything is compared
# against the installed binary — divergence is a repo inconsistency, not
# drift, and fails closed (exit 2) without consulting claude. Historical
# fixture files that no contract test references are exempt: only the active
# references parsed out of the test sources are pinned.
#
# Exit codes:
#   0  versions match — evidence is current, nothing to do
#   1  DRIFT — the installed version differs from any active pin; re-run due
#   2  cannot determine (claude missing, a version string unparseable, or the
#      active evidence pins disagree with each other)
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

# Only the fixture references wired into the contract tests are active pins.
# Older claude_contracts_v*.json captures remain in tests/fixtures/ as useful
# measurement history, but must not keep the gate red after a newer fixture is
# selected by tests/claude_contracts.rs. The stream-json contract has three
# files in one family; all three references must carry the same version.
FIXTURE_SOURCES=(
    "$REPO_ROOT/tests/claude_contracts.rs"
    "$REPO_ROOT/tests/stream_json_contract.rs"
)
FIXTURE_CLASSES=(claude_contracts stream_json_golden)
declare -A FIXTURE_PINS=()
FIXTURE_PARSE_ERROR=0

for source in "${FIXTURE_SOURCES[@]}"; do
    if [ ! -f "$source" ]; then
        echo "ERROR: active fixture source is missing: $source" >&2
        FIXTURE_PARSE_ERROR=1
        continue
    fi
    while IFS= read -r fixture; do
        [ -n "$fixture" ] || continue
        fixture_class="${fixture#fixtures/}"
        fixture_class="${fixture_class%%_v*}"
        fixture_version="${fixture##*_v}"
        previous="${FIXTURE_PINS[$fixture_class]-}"
        if [ -n "$previous" ] && [ "$previous" != "$fixture_version" ]; then
            echo "ERROR: active $fixture_class fixture references disagree: $previous and $fixture_version" >&2
            FIXTURE_PARSE_ERROR=1
        else
            FIXTURE_PINS["$fixture_class"]="$fixture_version"
        fi
        if ! compgen -G "$REPO_ROOT/tests/$fixture*" >/dev/null; then
            echo "ERROR: active fixture family is missing for $fixture" >&2
            FIXTURE_PARSE_ERROR=1
        fi
    done < <(grep -hEo 'fixtures/(claude_contracts|stream_json_golden)_v[0-9]+\.[0-9]+\.[0-9]+' "$source" || true)
done

for fixture_class in "${FIXTURE_CLASSES[@]}"; do
    if [ -z "${FIXTURE_PINS[$fixture_class]-}" ]; then
        echo "ERROR: no active $fixture_class fixture reference found" >&2
        FIXTURE_PARSE_ERROR=1
    fi
done

if [ "$FIXTURE_PARSE_ERROR" -ne 0 ]; then
    exit 2
fi

PIN_LINE="$(grep -m1 '^\*\*Measured against:\*\*' "$DOC" || true)"
PIN_VERSION="$(version_token "${PIN_LINE:-}")"
if [ -z "$PIN_VERSION" ]; then
    echo "ERROR: no parsable 'Measured against:' stamp in $DOC" >&2
    exit 2
fi

# Cross-evidence consistency (claudepr-b590e46d): the doc stamp and every
# active fixture family must name ONE version. The per-class loop above only
# enforces agreement within a family; this enforces agreement across the doc
# stamp and both families, before the installed binary is consulted at all —
# so a half-landed re-pin (one family re-pinned, the other left behind) is
# rejected even where claude is absent. Unreferenced fixture files are
# historical captures and are never compared.
ALL_PINS="$PIN_VERSION"
for fixture_class in "${FIXTURE_CLASSES[@]}"; do
    ALL_PINS="$ALL_PINS ${FIXTURE_PINS[$fixture_class]}"
done
if [ "$(printf '%s\n' $ALL_PINS | sort -u | wc -l | tr -d ' ')" -ne 1 ]; then
    echo "ERROR: active version pins disagree across evidence sources:" >&2
    echo "  doc stamp (claude-contract-probes.md): $PIN_VERSION" >&2
    for fixture_class in "${FIXTURE_CLASSES[@]}"; do
        echo "  fixture ($fixture_class): ${FIXTURE_PINS[$fixture_class]}" >&2
    done
    echo "Re-measure and re-pin every active family to one version" >&2
    echo "(docs/notes/claude-contract-probes.md §Re-pin)." >&2
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
for fixture_class in "${FIXTURE_CLASSES[@]}"; do
    echo "fixture ($fixture_class):                    ${FIXTURE_PINS[$fixture_class]}"
done

DRIFT=0
[ "$PIN_VERSION" = "$LIVE_VERSION" ] || DRIFT=1
for fixture_class in "${FIXTURE_CLASSES[@]}"; do
    [ "${FIXTURE_PINS[$fixture_class]}" = "$LIVE_VERSION" ] || DRIFT=1
done

if [ "$DRIFT" -eq 0 ]; then
    echo "CURRENT — contract evidence matches the installed version."
    exit 0
fi

echo "DRIFT — contract evidence does not cover the installed version $LIVE_VERSION."
echo "Re-measure and re-pin every active version-pinned fixture family before CI can pass."
echo "Re-run due (docs/notes/claude-contract-probes.md §Maintenance):"
echo "  1. cargo test --test claude_contracts -- --ignored   # cheap pre-check (~35 s)"
echo "  2. bash scripts/probe-claude-contracts.sh            # merge/suppression/Stop"
echo "  3. bash scripts/probe-stop-toolallowed.sh            # multi-round Stop (print)"
echo "  4. bash scripts/probe-tui-second-turn.sh             # TUI once-per-turn"
echo "  5. Re-pin claude_contracts_v<version>.json and stream_json_golden_v<version>.*.jsonl"
echo "     (update each active test reference and the documented stamp), or file"
echo "     follow-up beads if a contract moved."
exit 1
