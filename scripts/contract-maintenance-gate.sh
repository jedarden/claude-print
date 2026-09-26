#!/usr/bin/env bash
# contract-maintenance-gate.sh — the executable owner of the contract-probe
# maintenance workflow (docs/notes/claude-contract-probes.md §Maintenance).
#
# The doc defines the maintenance step in four parts — detect, re-run, re-pin,
# file follow-ups — and this gate performs them as one command so the step is
# owned and scheduled instead of being an unowned doc-level instruction:
#
#   detect      scripts/check-claude-version-bump.sh (live `claude --version`
#               vs the doc stamp and every active version-pinned fixture
#               family: claude_contracts and stream_json_golden)
#   re-run      cargo test --test claude_contracts -- --ignored (cheap live
#               contracts; the tests self-skip without claude/auth) unless
#               --skip-live-tests; the four model-turn probe scripts run only
#               with --run-probes (API auth + 1–6 min each) and are otherwise
#               recorded as SKIPPED — never silently absent
#   evidence    a bundle under --evidence-dir: detection.txt,
#               live-contract-tests.txt (only when the tests actually ran),
#               probes/<script>.txt, contract-status.txt, next-steps.txt
#   follow-up   with --file-follow-up, DRIFT files (or updates — idempotent
#               per installed version, matched on a
#               `claude-contract-drift live=<version>` body marker searched
#               before create) a GitHub issue via the gh CLI
#
# Every run also refreshes the CI drift artifact target/last-claude-version.txt
# — the same file, in the same full-line format, that
# tests/version_compat.rs::test_claude_version_recorded writes — so the
# release asset stays real and consistently formatted even when cargo did not
# run first. "unknown" is recorded only when the version cannot be determined.
#
# Exit codes (mirrored by the `alert:` line in contract-status.txt):
#   0  CURRENT        installed claude matches the pinned evidence
#   1  DRIFT          re-run due — follow-up filed when --file-follow-up
#   2  INDETERMINATE  version could not be determined (claude missing,
#                     unparsable output, or a missing doc stamp)
#
# Drift is a hard failure where CI runs this gate (claudepr-3094ab2e): the
# claude-print-ci workflow invokes it as the FIRST quality gate and lets a
# non-zero exit fail the run, so a Claude version change requires the
# documented maintenance step — re-run the probes, record updated evidence,
# land the re-pin commit (doc stamp + every active fixture family + test
# references) — before CI goes green again. The full probes still cannot run
# inside CI (no
# model-turn auth there), which is why the gate itself keeps running to
# completion first: it writes the evidence bundle and files/updates the
# follow-up issue, so the hand-off survives the red build it then raises.
#
# Since 2026-09-26 (claudepr-9fe76ef4) the gate also brackets ITSELF against
# a mid-run Claude update — the 2026-09-24 straddle shape: it captures the
# live version before detection and re-captures before writing its status,
# and a mismatch voids the verdict (version-stability: straddled in
# contract-status.txt, exit 2 INDETERMINATE, fail closed), because neither
# CURRENT nor DRIFT is certifiable against a binary that stopped being the
# installed one mid-gate. The probes it runs under --run-probes guard
# themselves the same way (scripts/probe-version-guard.sh; a straddled probe
# exits 1 and its evidence must be discarded).
#
# Outside of the steps above the gate is read-only: it runs `claude --version`,
# the detector, (optionally) cargo test / the probe scripts, and — on drift
# with --file-follow-up — a read-only `gh issue list` plus one issue
# create-or-comment. It never writes outside --evidence-dir, the version file,
# and the repo's own target/.

set -u

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
DOC="$REPO_ROOT/docs/notes/claude-contract-probes.md"
DETECTOR="$REPO_ROOT/scripts/check-claude-version-bump.sh"
GH_REPO="jedarden/claude-print"
PROBES="probe-claude-contracts.sh probe-stop-toolallowed.sh probe-tui-second-turn.sh probe-stop-edge-contracts.sh"

EVIDENCE_DIR="$REPO_ROOT/target/contract-maintenance"
VERSION_FILE="$REPO_ROOT/target/last-claude-version.txt"
FILE_FOLLOW_UP=0
SKIP_LIVE_TESTS=0
RUN_PROBES=0

usage() {
    cat <<'USAGE'
usage: scripts/contract-maintenance-gate.sh [options]
  --evidence-dir DIR   where to write the evidence bundle
                       (default: target/contract-maintenance)
  --version-file PATH  CI drift artifact to refresh
                       (default: target/last-claude-version.txt)
  --file-follow-up     file/update a GitHub follow-up issue on drift (gh CLI)
  --skip-live-tests    do not run cargo test --test claude_contracts -- --ignored
  --run-probes         also run the four model-turn probe scripts (API auth +
                       1-6 min each); without it they are recorded as SKIPPED
  -h, --help           this text
USAGE
}

while [ $# -gt 0 ]; do
    case "$1" in
        --evidence-dir|--version-file)
            [ $# -ge 2 ] || { echo "ERROR: $1 needs a value" >&2; exit 2; }
            if [ "$1" = "--evidence-dir" ]; then EVIDENCE_DIR="$2"; else VERSION_FILE="$2"; fi
            shift 2
            ;;
        --file-follow-up) FILE_FOLLOW_UP=1; shift ;;
        --skip-live-tests) SKIP_LIVE_TESTS=1; shift ;;
        --run-probes) RUN_PROBES=1; shift ;;
        -h|--help) usage; exit 0 ;;
        *) echo "ERROR: unknown argument: $1 (see --help)" >&2; exit 2 ;;
    esac
done

version_token() {
    # First x.y.z-looking token in the given text, empty if none.
    printf '%s' "$1" | grep -oE '[0-9]+\.[0-9]+\.[0-9]+' | head -1 || true
}

mkdir -p "$EVIDENCE_DIR/probes" || { echo "ERROR: cannot create evidence dir $EVIDENCE_DIR" >&2; exit 2; }

# ── 0. Version-straddle bracket, start capture ───────────────────────────────
#
# Taken BEFORE detection so the bracket spans everything the verdict rests on
# (detector, version artifact, live tests, probes). Step 6 re-captures and a
# mismatch voids the verdict — the same guard the probe scripts run through
# scripts/probe-version-guard.sh, applied to the gate itself.

GATE_VERSION_START_LINE=""
if command -v claude >/dev/null 2>&1; then
    GATE_VERSION_START_LINE="$(claude --version 2>&1 | head -1)"
fi
GATE_VERSION_START="$(version_token "${GATE_VERSION_START_LINE:-}")"

# ── 1. Detect ────────────────────────────────────────────────────────────────

DET_OUT="$(bash "$DETECTOR" 2>&1)"
DET_EXIT=$?
{
    printf '%s\n' "$DET_OUT"
    printf 'detector-exit: %s\n' "$DET_EXIT"
} > "$EVIDENCE_DIR/detection.txt"

case "$DET_EXIT" in
    0) VERDICT=CURRENT;     ALERT=none;         GATE_EXIT=0 ;;
    1) VERDICT=DRIFT;       ALERT=re-run-due;   GATE_EXIT=1 ;;
    *) VERDICT=INDETERMINATE; ALERT=indeterminate; GATE_EXIT=2 ;;
esac

# ── 2. Refresh the CI drift artifact (target/last-claude-version.txt) ────────

PIN_VERSION="$(version_token "$(grep -m1 '^\*\*Measured against:\*\*' "$DOC" || true)")"
[ -n "$PIN_VERSION" ] || PIN_VERSION="unknown"

LIVE_LINE=""
if command -v claude >/dev/null 2>&1; then
    RAW_LINE="$(claude --version 2>&1 | head -1)"
    # Keep the line only when it actually carries a version — the same shape
    # test_claude_version_recorded records; otherwise the honest value is
    # "unknown", not a raw error string.
    [ -n "$(version_token "$RAW_LINE")" ] && LIVE_LINE="$RAW_LINE"
fi
[ -n "$LIVE_LINE" ] || LIVE_LINE="unknown"
LIVE_VERSION="$(version_token "$LIVE_LINE")"
[ -n "$LIVE_VERSION" ] || LIVE_VERSION="unknown"
FIXTURE_PINS="$(printf '%s\n' "$DET_OUT" | grep '^fixture (' || true)"

mkdir -p "$(dirname "$VERSION_FILE")"
printf '%s\n' "$LIVE_LINE" > "$VERSION_FILE"

# ── 3. Re-run: cheap live contracts (self-skipping without auth) ─────────────

if [ "$SKIP_LIVE_TESTS" -eq 1 ]; then
    LIVE_SUMMARY="skipped (--skip-live-tests)"
elif ! command -v claude >/dev/null 2>&1; then
    LIVE_SUMMARY="skipped (claude not on PATH)"
elif ! command -v cargo >/dev/null 2>&1; then
    LIVE_SUMMARY="skipped (cargo not on PATH)"
else
    # Their failure is evidence about the installed claude, not a gate verdict:
    # the exit is driven by version detection above, and the result is recorded
    # here and summarized into contract-status.txt.
    ( cd "$REPO_ROOT" && cargo test --test claude_contracts -- --ignored ) \
        > "$EVIDENCE_DIR/live-contract-tests.txt" 2>&1
    LIVE_EXIT=$?
    printf 'live-tests-exit: %s\n' "$LIVE_EXIT" >> "$EVIDENCE_DIR/live-contract-tests.txt"
    LIVE_SUMMARY="ran (exit $LIVE_EXIT)"
fi

# ── 4. Probes: the four model-turn scripts, SKIPPED unless asked for ────────

PROBES_SUMMARY="skipped (use --run-probes on an authed host; recorded per script)"
for probe in $PROBES; do
    out="$EVIDENCE_DIR/probes/$probe.txt"
    if [ "$RUN_PROBES" -eq 1 ] && [ -f "$REPO_ROOT/scripts/$probe" ]; then
        ( cd "$REPO_ROOT" && bash "scripts/$probe" ) > "$out" 2>&1
        probe_exit=$?
        printf 'probe-exit: %s\n' "$probe_exit" >> "$out"
        PROBES_SUMMARY="ran (see probes/*.txt)"
    else
        cat > "$out" <<EOF
SKIPPED — model-turn probe (API auth + 1-6 min each;
docs/notes/claude-contract-probes.md §Re-running).
Run by hand on an authed host: bash scripts/$probe
EOF
    fi
done

# ── 5. Follow-up issue (DRIFT + --file-follow-up; idempotent per version) ────

file_follow_up() {
    local marker title body existing number url
    marker="claude-contract-drift live=${LIVE_VERSION}"
    title="Claude contract drift: ${LIVE_VERSION} installed, evidence pinned to ${PIN_VERSION}"
    body="The contract-maintenance gate detected version drift.

${marker}
installed: ${LIVE_VERSION}
pinned:    ${PIN_VERSION} (docs/notes/claude-contract-probes.md **Measured against:**)

The measured contracts (merge, suppression, once-per-turn Stop, max-turns
cutoff, and the stream-json golden wire format) plus the active
version-pinned fixture families are unverified for ${LIVE_VERSION}. Re-run due — procedure:
docs/notes/claude-contract-probes.md §Maintenance (detect, re-run, re-pin,
file follow-ups). Evidence bundle: target/contract-maintenance/ (CI artifact).
"
    existing="$(gh issue list --repo "$GH_REPO" --state open --search "$marker" --json number --limit 5 2>&1)" || {
        echo "gh issue list failed: $existing" >&2
        return 1
    }
    number="$(printf '%s' "$existing" | grep -oE '"number"[: ]+[0-9]+' | grep -oE '[0-9]+' | head -1 || true)"
    if [ -n "$number" ]; then
        gh issue comment "$number" --repo "$GH_REPO" --body "$body" >/dev/null || {
            echo "gh issue comment #$number failed" >&2
            return 1
        }
        echo "updated issue #${number}"
        return 0
    fi
    url="$(gh issue create --repo "$GH_REPO" --title "$title" --body "$body" 2>&1)" || {
        echo "gh issue create failed: $url" >&2
        return 1
    }
    echo "filed ${url}"
}

FOLLOW_UP="n/a (no drift)"
if [ "$GATE_EXIT" -eq 1 ]; then
    if [ "$FILE_FOLLOW_UP" -eq 1 ]; then
        if command -v gh >/dev/null 2>&1; then
            if ! FOLLOW_UP="$(file_follow_up)"; then
                FOLLOW_UP="failed (gh error — see the gate's stderr above)"
            fi
        else
            FOLLOW_UP="not-filed (gh CLI not on PATH — file manually)"
            echo "WARNING: --file-follow-up given but gh is not on PATH" >&2
        fi
    else
        FOLLOW_UP="not-requested (pass --file-follow-up)"
    fi
fi

# ── 6. Version-straddle bracket, end capture ─────────────────────────────────
#
# A claude that changed version between the start capture and now means the
# detector's verdict was computed against a binary that stopped being the
# installed one mid-gate: neither CURRENT nor DRIFT is certifiable, so the
# verdict is voided and the gate fails closed as INDETERMINATE. (The
# follow-up above, if any, was filed against the start version before the
# straddle was knowable; the next gate run re-files idempotently for the
# version the host settled on.)

GATE_VERSION_END_LINE=""
if command -v claude >/dev/null 2>&1; then
    GATE_VERSION_END_LINE="$(claude --version 2>&1 | head -1)"
fi
GATE_VERSION_END="$(version_token "${GATE_VERSION_END_LINE:-}")"

STRADDLED=0
VERSION_STABILITY="unknown (claude version not determinable at gate start or end)"
if [ -n "$GATE_VERSION_START" ] && [ -n "$GATE_VERSION_END" ]; then
    if [ "$GATE_VERSION_START" = "$GATE_VERSION_END" ]; then
        VERSION_STABILITY="stable ($GATE_VERSION_START)"
    else
        STRADDLED=1
        VERSION_STABILITY="straddled (start=$GATE_VERSION_START end=$GATE_VERSION_END)"
    fi
elif [ -n "$GATE_VERSION_START" ] || [ -n "$GATE_VERSION_END" ]; then
    VERSION_STABILITY="indeterminate (start=${GATE_VERSION_START:-unparsable} end=${GATE_VERSION_END:-unparsable})"
fi

if [ "$STRADDLED" -eq 1 ]; then
    VERDICT=INDETERMINATE
    ALERT=indeterminate
    GATE_EXIT=2
fi

# ── 7. Status, next steps, exit ───────────────────────────────────────────────

cat > "$EVIDENCE_DIR/contract-status.txt" <<EOF
contract-maintenance: ${VERDICT}
alert: ${ALERT}
pinned: ${PIN_VERSION}
installed: ${LIVE_VERSION}
version-stability: ${VERSION_STABILITY}
fixture-pins:
${FIXTURE_PINS:-unknown}
live-tests: ${LIVE_SUMMARY}
probes: ${PROBES_SUMMARY}
follow-up: ${FOLLOW_UP}
gate-exit: ${GATE_EXIT}
EOF

if [ "$STRADDLED" -eq 1 ]; then
    cat > "$EVIDENCE_DIR/next-steps.txt" <<EOF
This gate run itself was STRADDLED by a Claude Code update mid-run
(start=${GATE_VERSION_START} -> end=${GATE_VERSION_END}): the detection verdict
compared the pins against a binary that stopped being the installed one before
the gate finished, so neither CURRENT nor DRIFT is certifiable. Re-run the gate
— the re-run compares against ${GATE_VERSION_END}, the version the host
settled on. If this run used --run-probes, discard its probe evidence too:
each probe's own version guard aborts on the same straddle (non-zero
probe-exit in probes/*.txt) and its evidence must not be pinned.
Guard mechanism: docs/notes/claude-contract-probes.md §Version guard
EOF
else
    case "$VERDICT" in
    CURRENT)
        cat > "$EVIDENCE_DIR/next-steps.txt" <<EOF
Contract evidence is current: installed ${LIVE_VERSION} matches the pinned
stamp ${PIN_VERSION}. Nothing to do.
Live-contract confirmation this run: ${LIVE_SUMMARY}.
EOF
        ;;
    DRIFT)
        cat > "$EVIDENCE_DIR/next-steps.txt" <<EOF
Re-run due — one or more active evidence or fixture pins do not cover
installed ${LIVE_VERSION} (documentation stamp: ${PIN_VERSION}).
  1. cargo test --test claude_contracts -- --ignored   # cheap pre-check (~35 s)
  2. bash scripts/probe-claude-contracts.sh            # merge/suppression/Stop
  3. bash scripts/probe-stop-toolallowed.sh            # multi-round Stop (print)
  4. bash scripts/probe-tui-second-turn.sh             # TUI once-per-turn
  5. bash scripts/probe-stop-edge-contracts.sh         # sleeping-hook concurrency
                                                       # + degraded-path Stop counts
  6. Re-pin per §Re-pin (doc stamp + claude_contracts_v*.json +
     stream_json_golden_v* family + active test references in one change), or
     file one bead per moved contract per §File follow-ups.
Procedure: docs/notes/claude-contract-probes.md §Maintenance
Follow-up: ${FOLLOW_UP}
EOF
        ;;
    *)
        cat > "$EVIDENCE_DIR/next-steps.txt" <<EOF
Version could not be determined (claude missing or unparsable, or no
**Measured against:** stamp in the doc). Detection output:
${DET_OUT}
Fix the environment (install claude: curl -fsSL https://claude.ai/install.sh | bash)
and re-run the gate; until then the evidence's currency is unknown.
EOF
        ;;
    esac
fi

echo "contract-maintenance gate: ${VERDICT} (exit ${GATE_EXIT}; alert: ${ALERT})"
echo "pinned=${PIN_VERSION} installed=${LIVE_VERSION} live-tests: ${LIVE_SUMMARY}"
echo "evidence: ${EVIDENCE_DIR}/contract-status.txt"
if [ "$GATE_EXIT" -ne 0 ]; then
    echo "ALERT: Claude contract evidence needs re-verification — see ${EVIDENCE_DIR}/next-steps.txt" >&2
fi
exit "$GATE_EXIT"
