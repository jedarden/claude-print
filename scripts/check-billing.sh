#!/usr/bin/env bash
# check-billing.sh - AS-4 billing conformance check
#
# With no argument, inspect the newest Claude transcript (the manual release
# gate). With a transcript path, inspect exactly that file (used by the
# automated canary so concurrent NEEDLE sessions cannot cause a false result).

set -eu

RED='\033[0;31m'
GREEN='\033[0;32m'
NC='\033[0m'

log_info() {
    printf '%b[INFO]%b %s\n' "$GREEN" "$NC" "$1"
}

log_error() {
    printf '%b[ERROR]%b %s\n' "$RED" "$NC" "$1" >&2
}

usage() {
    printf 'Usage: %s [TRANSCRIPT.jsonl]\n' "$0" >&2
}

if [ "$#" -gt 1 ]; then
    usage
    exit 2
fi

if [ "$#" -eq 1 ]; then
    TRANSCRIPT=$1
    if [ ! -f "$TRANSCRIPT" ]; then
        log_error "Transcript not found: $TRANSCRIPT"
        exit 1
    fi
else
    TRANSCRIPTS_DIR=${CLAUDE_PRINT_TRANSCRIPTS_DIR:-"$HOME/.claude/projects"}

    if [ ! -d "$TRANSCRIPTS_DIR" ]; then
        log_error "Claude projects directory not found: $TRANSCRIPTS_DIR"
        log_error "Has claude or claude-print been run on this machine?"
        exit 1
    fi

    TRANSCRIPT=$(find "$TRANSCRIPTS_DIR" -type f -name '*.jsonl' \
        -printf '%T@ %p\n' 2>/dev/null | sort -rn | head -n 1 | cut -d' ' -f2-)

    if [ -z "$TRANSCRIPT" ]; then
        log_error "No transcript JSONL files found under: $TRANSCRIPTS_DIR"
        log_error "Run claude or claude-print first to generate a transcript."
        exit 1
    fi
fi

log_info "Inspecting transcript: $TRANSCRIPT"

# The entrypoint is carried on one JSONL event. Isolating that line first keeps
# jq from rejecting an otherwise useful transcript if a later line is partial.
ENTRYPOINT_LINE=$(grep -m1 '"entrypoint"' "$TRANSCRIPT" 2>/dev/null || true)
ENTRYPOINT=

if [ -n "$ENTRYPOINT_LINE" ]; then
    if command -v jq >/dev/null 2>&1; then
        ENTRYPOINT=$(printf '%s\n' "$ENTRYPOINT_LINE" \
            | jq -r '.entrypoint // empty' 2>/dev/null || true)
    else
        ENTRYPOINT=$(printf '%s\n' "$ENTRYPOINT_LINE" \
            | sed -n 's/.*"entrypoint"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p')
    fi
fi

if [ -z "$ENTRYPOINT" ]; then
    log_error "No entrypoint field found in transcript: $TRANSCRIPT"
    log_error "The transcript may be from an incompatible Claude Code version."
    exit 1
fi

printf 'entrypoint: %s\n' "$ENTRYPOINT"

if [ "$ENTRYPOINT" = cli ]; then
    log_info 'Billing classification: SUBSCRIPTION (cli) - PASS'
    exit 0
fi

log_error "Billing classification: AGENT SDK CREDIT POOL ($ENTRYPOINT) - FAIL"
log_error 'Expected: cli (subscription pool)'
log_error "Actual: $ENTRYPOINT"
log_error "File inspected: $TRANSCRIPT"
exit 1
