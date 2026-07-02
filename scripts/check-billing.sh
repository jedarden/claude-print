#!/bin/bash
# check-billing.sh - AS-4 billing conformance script
# Inspects the most recent transcript JSONL under ~/.claude/projects/ and asserts
# the session has entrypoint 'cli' (subscription pool), not 'sdk-cli'.
# This is the AS-4 pre-release gate - run before every release.

set -eu

# ANSI color codes for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m'

log_info() {
    echo -e "${GREEN}[INFO]${NC} $1"
}

log_error() {
    echo -e "${RED}[ERROR]${NC} $1" >&2
}

log_warn() {
    echo -e "${YELLOW}[WARN]${NC} $1" >&2
}

# Find the newest JSONL file under ~/.claude/projects/
TRANSCRIPTS_DIR="$HOME/.claude/projects"

if [ ! -d "$TRANSCRIPTS_DIR" ]; then
    log_error "Claude projects directory not found: $TRANSCRIPTS_DIR"
    log_error "Has claude or claude-print been run on this machine?"
    exit 1
fi

# Find the most recently modified .jsonl file (recursive)
NEWEST_JSONL=$(find "$TRANSCRIPTS_DIR" -type f -name "*.jsonl" -printf '%T@ %p\n' 2>/dev/null | sort -rn | head -1 | cut -d' ' -f2-)

if [ -z "$NEWEST_JSONL" ]; then
    log_error "No transcript JSONL files found under: $TRANSCRIPTS_DIR"
    log_error "Run claude or claude-print first to generate a transcript."
    exit 1
fi

log_info "Inspecting most recent transcript: $NEWEST_JSONL"

# Scan for the entrypoint field in the JSONL
# We use jq if available, fallback to grep+sed
ENTRYPOINT=

if command -v jq >/dev/null 2>&1; then
    # Use jq for robust JSON parsing
    ENTRYPOINT=$(grep -m1 '"entrypoint"' "$NEWEST_JSONL" | jq -r '.entrypoint // empty' 2>/dev/null || echo "")
else
    # Fallback: extract with grep and sed
    # Matches: "entrypoint": "cli" or "entrypoint":"cli"
    ENTRYPOINT=$(grep -m1 '"entrypoint"' "$NEWEST_JSONL" | sed -n 's/.*"entrypoint"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' 2>/dev/null || echo "")
fi

if [ -z "$ENTRYPOINT" ]; then
    log_error "No entrypoint field found in transcript: $NEWEST_JSONL"
    log_error "The transcript may be from an incompatible Claude Code version."
    exit 1
fi

# Print the entrypoint value
echo "entrypoint: $ENTRYPOINT"

# Assert it equals 'cli'
if [ "$ENTRYPOINT" = "cli" ]; then
    log_info "Billing classification: SUBSCRIPTION (cli) - PASS"
    exit 0
else
    log_error "Billing classification: AGENT SDK CREDIT POOL ($ENTRYPOINT) - FAIL"
    log_error "Expected: cli (subscription pool)"
    log_error "Actual: $ENTRYPOINT"
    log_error "File inspected: $NEWEST_JSONL"
    exit 1
fi
