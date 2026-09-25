#!/usr/bin/env bash
# contract-drift-watch.sh — scheduled contract-drift detection step
# (docs/notes/claude-contract-probes.md §Maintenance → Scheduled watch;
# bead claudepr-e6e54313).
#
# CI runs scripts/contract-maintenance-gate.sh on every push, but Claude Code
# auto-updates independently of repo activity — the dev host moved 2.1.281 →
# 2.1.282 mid-re-pin on 2026-09-24 with nothing pushed in between — so between
# pushes the active fixture pins and the **Measured against:** stamp can be
# stale with nothing red. This watcher closes that blind window: it runs the
# credential-free detection step (scripts/check-claude-version-bump.sh —
# `claude --version` only, no sandbox, no model turns, exactly the gate's
# detect step) on a schedule and files a bead on drift, so the documented
# rule "re-run them after any Claude Code update" has a detector that fires
# even when nobody pushes.
#
# On DRIFT (detector exit 1) it files one bead in the contract repo's bead
# workspace via `bead create --unique-ref claude-contract-drift:live-<version>`
# — the CLI's atomic idempotent-create contract — so daily repeats while the
# drift persists return EXISTING/EXISTING_CLOSED instead of duplicating, the
# bead-level twin of the gate's per-version gh-issue marker. The bead body
# carries the same `claude-contract-drift live=<version>` marker for humans
# and grep.
#
# Exit codes (mirroring the detector and the gate):
#   0  CURRENT        installed claude matches the pinned evidence
#   1  DRIFT          re-run due — follow-up bead filed (or the failure to
#                     file recorded; the red unit + state line still alert)
#   2  INDETERMINATE  version could not be determined — no bead, loud failure
#
# State: one result line, atomically written, in the billing-canary shape —
#   ${XDG_STATE_HOME:-$HOME/.local/state}/claude-print/contract-drift-watch/last-result
#
# Installed as a systemd user timer by scripts/install-contract-drift-watch.sh
# (+ scripts/claude-print-contract-drift-watch.{service,timer}). The unit pins
# CLAUDE_PRINT_CONTRACT_REPO=%h/claude-print — the shared checkout whose pins
# the detector reads and whose .beads/ workspace the follow-up lands in; run
# from the repo itself (or with that env set) the repo resolves from the
# script's own location. The detector always runs from that checkout, never
# from an installed copy, so detection logic never goes stale.
#
# Outside the bead filing the watcher is read-only: it runs the detector and
# writes only inside its state dir.

set -u

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
REPO_ROOT=${CLAUDE_PRINT_CONTRACT_REPO:-$(CDPATH= cd -- "$SCRIPT_DIR/.." && pwd)}
DETECTOR="$REPO_ROOT/scripts/check-claude-version-bump.sh"
STATE_HOME=${XDG_STATE_HOME:-"$HOME/.local/state"}
STATE_DIR=${CLAUDE_PRINT_DRIFT_STATE_DIR:-"$STATE_HOME/claude-print/contract-drift-watch"}
RESULT_FILE="$STATE_DIR/last-result"

mkdir -p "$STATE_DIR" || {
    printf '[ERROR] cannot create state dir %s\n' "$STATE_DIR" >&2
    exit 2
}
chmod 700 "$STATE_DIR"

write_result() {
    # One atomic result line in the billing-canary shape: STATUS key=value ...
    status=$1
    shift
    timestamp=$(date -u +%Y-%m-%dT%H:%M:%SZ)
    result_tmp=$(mktemp "$STATE_DIR/.last-result.XXXXXX")
    printf '%s timestamp=%s %s\n' "$status" "$timestamp" "$*" > "$result_tmp"
    chmod 600 "$result_tmp"
    mv -f "$result_tmp" "$RESULT_FILE"
    printf 'CLAUDE_PRINT_CONTRACT_DRIFT_WATCH status=%s timestamp=%s %s\n' \
        "$status" "$timestamp" "$*"
}

version_token() {
    # First x.y.z-looking token in the given text, empty if none — the same
    # filter scripts/check-claude-version-bump.sh applies.
    printf '%s' "$1" | grep -oE '[0-9]+\.[0-9]+\.[0-9]+' | head -1 || true
}

if [ ! -f "$DETECTOR" ]; then
    printf '[ERROR] detector missing under the contract repo: %s\n' "$DETECTOR" >&2
    printf '[ERROR] set CLAUDE_PRINT_CONTRACT_REPO or reinstall via scripts/install-contract-drift-watch.sh\n' >&2
    write_result INDETERMINATE reason=detector_missing "repo=$REPO_ROOT"
    exit 2
fi

# ── Detect (the gate's credential-free step, nothing heavier) ─────────────────

DET_OUT="$(cd "$REPO_ROOT" && bash "$DETECTOR" 2>&1)"
DET_EXIT=$?
printf '%s\n' "$DET_OUT"

case "$DET_EXIT" in
    0)
        write_result PASS verdict=current
        exit 0
        ;;
    2)
        # Cannot determine: there is no version to key a bead on and nothing
        # to re-pin yet — fail the unit loudly instead of filing speculative
        # work (the same fail-closed shape the CI gate gives exit 2).
        write_result INDETERMINATE verdict=cannot-determine
        printf 'ALERT: drift detection could not determine the installed version — see the detector output above.\n' >&2
        exit 2
        ;;
esac

# DET_EXIT = 1 → DRIFT: parse the pin/live pair the detector printed before
# its DRIFT verdict (both lines are unconditional on the drift path).
PIN_VERSION=$(version_token "$(printf '%s\n' "$DET_OUT" | sed -n 's/^pinned ([^)]*): *//p' | tail -1)")
LIVE_VERSION=$(version_token "$(printf '%s\n' "$DET_OUT" | sed -n 's/^live *(claude --version): *//p' | tail -1)")

# ── Follow-up bead (idempotent per installed version) ────────────────────────

FOLLOW_UP="not-filed"

file_drift_bead() {
    # `bead create --unique-ref` is atomic idempotent creation: a repeat while
    # the drift persists prints `EXISTING <id>` (nothing new filed); a binding
    # that already points at a closed bead prints `EXISTING_CLOSED <id>`.
    local out create_exit last id
    out="$(cd "$REPO_ROOT" && bead create \
        --title "Claude contract drift: live ${LIVE_VERSION}, evidence pinned to ${PIN_VERSION}" \
        --description "Scheduled drift watch (scripts/contract-drift-watch.sh): the installed Claude Code is ${LIVE_VERSION} while the repo's contract evidence is pinned to ${PIN_VERSION}, so the measured contracts (merge, --setting-sources suppression, once-per-turn Stop, max-turns cutoff, stream-json goldens) are unverified for ${LIVE_VERSION}. Re-run due — procedure: docs/notes/claude-contract-probes.md §Maintenance (detect, re-run, re-pin, file follow-ups); the CI gate stays red until the re-pin lands. claude-contract-drift live=${LIVE_VERSION}" \
        --priority 2 \
        --issue-type task \
        --label contract-drift \
        --unique-ref "claude-contract-drift:live-${LIVE_VERSION}" 2>&1)"
    create_exit=$?
    if [ "$create_exit" -ne 0 ]; then
        printf '%s\n' "$out" >&2
        printf 'WARNING: bead create failed (exit %s) — file the drift follow-up manually\n' "$create_exit" >&2
        FOLLOW_UP="failed (bead create exit ${create_exit} — see stderr above)"
        return 0
    fi
    last="$(printf '%s\n' "$out" | tail -1)"
    id="${last##* }"
    case "$last" in
        EXISTING_CLOSED*) FOLLOW_UP="existing-closed ${id}" ;;
        EXISTING*)        FOLLOW_UP="existing ${id}" ;;
        *)                FOLLOW_UP="filed ${id}" ;;
    esac
}

if [ -z "$LIVE_VERSION" ] || [ -z "$PIN_VERSION" ]; then
    # The detector's drift exit promises both lines; if they ever stop
    # parsing, say so rather than keying a bead on a blank version.
    printf 'WARNING: could not parse pin/live versions from the detector output — not filing\n' >&2
    FOLLOW_UP="not-filed (unparsable detector output)"
elif ! command -v bead >/dev/null 2>&1; then
    printf 'WARNING: bead CLI not on PATH — file the drift follow-up manually\n' >&2
    FOLLOW_UP="not-filed (bead CLI not on PATH — file manually)"
else
    file_drift_bead
fi

write_result DRIFT verdict=re-run-due "pinned=$PIN_VERSION" "live=$LIVE_VERSION" "follow-up=$FOLLOW_UP"
printf 'ALERT: Claude contract evidence does not cover the installed version %s — re-pin due (docs/notes/claude-contract-probes.md §Maintenance); follow-up: %s\n' \
    "${LIVE_VERSION:-unknown}" "$FOLLOW_UP" >&2
exit 1
