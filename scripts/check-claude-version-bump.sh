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
# And since 2026-09-26 (claudepr-893bfc4e) the one-pin invariant covers the
# doc's PROSE, not only the fixtures: every Claude version cited on a checked
# line of the doc — the **Measured against:** stamp itself, any markdown
# table row, or an "Evidence (" preamble line — must be the agreed pin or
# carry the explicit historical-attribution marker (the whole word
# "historical" on the same line, the convention the doc already keeps
# superseded numbers under). A citation that is neither is mixed-version
# evidence — the fixtures-move/prose-lags shape of the incomplete 2.1.283
# re-pin that had to be reverted (claudepr-2e8c3884) — and fails closed
# (exit 2) before claude is consulted. Fenced code blocks are skipped, and
# narrative paragraphs outside the evidence tables stay outside the
# mechanical scope (§Re-measurement history attributes its own numbers).
#
# Exit codes:
#   0  versions match — evidence is current, nothing to do
#   1  DRIFT — the installed version differs from any active pin; re-run due
#   2  cannot determine (claude missing, a version string unparseable, the
#      active evidence pins disagreeing with each other, or doc evidence
#      prose citing a version that is neither the pin nor marked historical)
#
# Read-only: runs `claude --version` only — no sandbox, no HOME writes, no
# model turns. Safe to run on a schedule or from CI (exit 1 = alert), which
# is the R-2 "CI alert on version change" signal the plan asks for, alongside
# the last-claude-version.txt artifact written by
# tests/version_compat.rs::test_claude_version_recorded into the resolved
# artifact dir ($CLAUDE_PRINT_VERSION_ARTIFACT_DIR, else the cargo-metadata
# target directory, else the stock target/) — the same resolution
# contract-maintenance-gate.sh applies when it refreshes the artifact.

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

# ── Doc-prose one-pin check (claudepr-893bfc4e) ───────────────────────────────
#
# The agreement checks above bind the stamp and the active fixtures as one
# measurement, but the doc's evidence prose — the per-evidence-table version
# citations and the superseded numbers quoted inside them — used to be kept
# aligned only by hand. That is exactly the fixtures-move/prose-lags shape of
# the incomplete 2.1.283 re-pin that landed mixed-version evidence and had to
# be reverted (claudepr-2e8c3884): the fixtures and the stamp said one
# version while the tables still cited another. So the same pin is enforced
# on the doc body: on every checked line — the **Measured against:** stamp,
# any markdown table row, or an "Evidence (" preamble — each cited x.y.z is
# either the agreed pin or the line carries the explicit
# historical-attribution marker (the whole word "historical", the convention
# the doc's superseded numbers already keep). Anything else is an error and
# the detector fails closed here, before claude is consulted. Fenced code
# blocks are not prose and are skipped.

PROSE_STATUS=0
prose_lineno=0
prose_in_fence=0
while IFS= read -r prose_line || [ -n "$prose_line" ]; do
    prose_lineno=$((prose_lineno + 1))
    # Fences and table rows are matched on the whitespace-trimmed line (the
    # doc fences one block inside a list item at a two-space indent, and
    # markdown still counts that as a fence) — the same trim
    # tests/contract_maintenance.rs::prose_line_kind applies, so the two
    # implementations cannot disagree about which lines are in scope.
    prose_trim="${prose_line#"${prose_line%%[![:space:]]*}"}"
    case "$prose_trim" in
        '```'*) prose_in_fence=$(( 1 - prose_in_fence )); continue ;;
    esac
    [ "$prose_in_fence" -eq 0 ] || continue
    prose_kind=""
    case "$prose_line" in
        '**Measured against:**'*) prose_kind="the Measured-against stamp" ;;
        'Evidence '*) prose_kind="an evidence preamble" ;;
        *)
            case "$prose_trim" in
                '|'*) prose_kind="an evidence-table row" ;;
            esac
            ;;
    esac
    [ -n "$prose_kind" ] || continue
    while IFS= read -r cited; do
        [ -n "$cited" ] || continue
        [ "$cited" = "$PIN_VERSION" ] && continue
        if printf '%s\n' "$prose_line" | grep -qi -w 'historical'; then
            continue
        fi
        echo "ERROR: $DOC:$prose_lineno: $prose_kind cites $cited, which is" \
             "neither the active pin $PIN_VERSION nor marked historical on that" \
             "line — mixed-version evidence; re-pin the citations or attribute" \
             "them per docs/notes/claude-contract-probes.md §Re-pin" >&2
        PROSE_STATUS=1
    done < <(printf '%s\n' "$prose_line" | grep -oE '[0-9]+\.[0-9]+\.[0-9]+' || true)
done < "$DOC"
if [ "$PROSE_STATUS" -ne 0 ]; then
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
echo "  5. bash scripts/probe-stop-edge-contracts.sh         # concurrency + degraded-path + hook-timeout (Arm T)"
echo "  6. Re-pin claude_contracts_v<version>.json and stream_json_golden_v<version>.*.jsonl"
echo "     (update each active test reference and the documented stamp), or file"
echo "     follow-up beads if a contract moved."
exit 1
