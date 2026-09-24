#!/usr/bin/env bash
# contract-maintenance-gate.sh — the executable owner of the contract-probe
# maintenance workflow (docs/notes/claude-contract-probes.md §Maintenance).
#
# The doc defines the maintenance step in four parts — detect, re-run, re-pin,
# file follow-ups — and this gate performs them as one command so the step is
# owned and scheduled instead of being an unowned doc-level instruction:
#
#   detect      scripts/check-claude-version-bump.sh (live `claude --version`
#               vs the doc's **Measured against:** stamp)
#   re-run      cargo test --test claude_contracts -- --ignored (cheap live
#               contracts; the tests self-skip without claude/auth) unless
#               --skip-live-tests; the three model-turn probe scripts run only
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
# Drift is an alert, not a hard failure: the full probes cannot run without
# model-turn auth, so CI runs the gate, captures this exit code, and lets the
# follow-up issue carry the hand-off (claude-print-ci-workflowtemplate.yml).
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
PROBES="probe-claude-contracts.sh probe-stop-toolallowed.sh probe-tui-second-turn.sh"

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
  --run-probes         also run the three model-turn probe scripts (API auth +
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

# ── 4. Probes: the three model-turn scripts, SKIPPED unless asked for ────────

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
cutoff) are unverified for ${LIVE_VERSION}. Re-run due — procedure:
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

# ── 6. Status, next steps, exit ───────────────────────────────────────────────

cat > "$EVIDENCE_DIR/contract-status.txt" <<EOF
contract-maintenance: ${VERDICT}
alert: ${ALERT}
pinned: ${PIN_VERSION}
installed: ${LIVE_VERSION}
live-tests: ${LIVE_SUMMARY}
probes: ${PROBES_SUMMARY}
follow-up: ${FOLLOW_UP}
gate-exit: ${GATE_EXIT}
EOF

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
Re-run due — evidence is pinned to ${PIN_VERSION} but ${LIVE_VERSION} is installed.
  1. cargo test --test claude_contracts -- --ignored   # cheap pre-check (~35 s)
  2. bash scripts/probe-claude-contracts.sh            # merge/suppression/Stop
  3. bash scripts/probe-stop-toolallowed.sh            # multi-round Stop (print)
  4. bash scripts/probe-tui-second-turn.sh             # TUI once-per-turn
  5. Re-pin per §Re-pin (doc stamp + fixture + FIXTURE repoint in one change),
     or file one bead per moved contract per §File follow-ups.
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

echo "contract-maintenance gate: ${VERDICT} (exit ${GATE_EXIT}; alert: ${ALERT})"
echo "pinned=${PIN_VERSION} installed=${LIVE_VERSION} live-tests: ${LIVE_SUMMARY}"
echo "evidence: ${EVIDENCE_DIR}/contract-status.txt"
if [ "$GATE_EXIT" -ne 0 ]; then
    echo "ALERT: Claude contract evidence needs re-verification — see ${EVIDENCE_DIR}/next-steps.txt" >&2
fi
exit "$GATE_EXIT"
