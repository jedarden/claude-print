#!/usr/bin/env bash
# billing-canary.sh - Automated AS-4 billing-classification canary

set -eu

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
CLAUDE_PRINT_BIN=${CLAUDE_PRINT_BIN:-claude-print}
CHECK_BILLING=${CLAUDE_PRINT_CHECK_BILLING:-"$SCRIPT_DIR/check-billing.sh"}
TRANSCRIPTS_DIR=${CLAUDE_PRINT_TRANSCRIPTS_DIR:-"$HOME/.claude/projects"}
STATE_HOME=${XDG_STATE_HOME:-"$HOME/.local/state"}
STATE_DIR=${CLAUDE_PRINT_BILLING_STATE_DIR:-"$STATE_HOME/claude-print/billing-canary"}
RESULT_FILE="$STATE_DIR/last-result"
WORK_DIR="$STATE_DIR/workdir"

mkdir -p "$STATE_DIR" "$WORK_DIR"
chmod 700 "$STATE_DIR"
chmod 700 "$WORK_DIR"

STDOUT_FILE=$(mktemp "$STATE_DIR/.stdout.XXXXXX")
STDERR_FILE=$(mktemp "$STATE_DIR/.stderr.XXXXXX")
START_MARKER=$(mktemp "$STATE_DIR/.started.XXXXXX")
CHECK_OUTPUT=
cleanup() {
    rm -f "$STDOUT_FILE" "$STDERR_FILE" "$START_MARKER"
    if [ -n "$CHECK_OUTPUT" ]; then
        rm -f "$CHECK_OUTPUT"
    fi
}
trap cleanup EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM

write_result() {
    status=$1
    shift
    timestamp=$(date -u +%Y-%m-%dT%H:%M:%SZ)
    result_tmp=$(mktemp "$STATE_DIR/.last-result.XXXXXX")
    printf '%s timestamp=%s %s\n' "$status" "$timestamp" "$*" > "$result_tmp"
    chmod 600 "$result_tmp"
    mv -f "$result_tmp" "$RESULT_FILE"
    printf 'CLAUDE_PRINT_BILLING_CANARY status=%s timestamp=%s %s\n' \
        "$status" "$timestamp" "$*"
}

fail() {
    reason=$1
    shift
    printf '[ERROR] Billing canary failed: %s\n' "$reason" >&2
    write_result FAIL "reason=$reason" "$@"
    exit 1
}

if ! command -v "$CLAUDE_PRINT_BIN" >/dev/null 2>&1; then
    fail claude_print_not_found "binary=$CLAUDE_PRINT_BIN"
fi

if [ ! -x "$CHECK_BILLING" ]; then
    fail billing_check_not_executable "path=$CHECK_BILLING"
fi

printf '[INFO] Running one-turn Haiku billing canary\n'
if (
    cd "$WORK_DIR"
    "$CLAUDE_PRINT_BIN" \
        --model haiku \
        --max-turns 1 \
        --timeout 300 \
        --no-inherit-hooks \
        --output-format json \
        'Reply with exactly: OK'
) >"$STDOUT_FILE" 2>"$STDERR_FILE"; then
    :
else
    invocation_status=$?
    sed -n '1,20p' "$STDERR_FILE" >&2
    fail invocation_failed "exit_code=$invocation_status"
fi

SESSION_ID=
if command -v jq >/dev/null 2>&1; then
    SESSION_ID=$(jq -r \
        'select(.type == "result") | .session_id // empty' \
        "$STDOUT_FILE" 2>/dev/null | tail -n 1 || true)
else
    SESSION_ID=$(sed -n \
        's/.*"session_id"[[:space:]]*:[[:space:]]*"\([A-Za-z0-9._-]*\)".*/\1/p' \
        "$STDOUT_FILE" | tail -n 1)
fi

if [ ! -d "$TRANSCRIPTS_DIR" ]; then
    fail transcripts_directory_missing "path=$TRANSCRIPTS_DIR" "session_id=$SESSION_ID"
fi

# Match the returned session id rather than the newest transcript. NEEDLE may
# write newer transcripts while this canary is running. Older claude-print
# builds may emit session_id:null; in that case the canary's dedicated working
# directory lets us identify the one transcript created after START_MARKER.
TRANSCRIPT=
case "$SESSION_ID" in
    ''|*[!A-Za-z0-9._-]*)
        SESSION_ID=
        ;;
    *)
        TRANSCRIPT=$(find "$TRANSCRIPTS_DIR" -type f -name "$SESSION_ID.jsonl" \
            -print -quit 2>/dev/null || true)
        ;;
esac

if [ -z "$TRANSCRIPT" ]; then
    slug_with_leading_dash=${WORK_DIR//\//-}
    slug_without_leading_dash=${WORK_DIR#/}
    slug_without_leading_dash=${slug_without_leading_dash//\//-}
    slug_sanitized=$(printf '%s' "$WORK_DIR" | sed 's/[^A-Za-z0-9_-]/-/g')
    candidates=()
    for project_dir in \
        "$TRANSCRIPTS_DIR/$slug_with_leading_dash" \
        "$TRANSCRIPTS_DIR/$slug_without_leading_dash" \
        "$TRANSCRIPTS_DIR/$slug_sanitized"; do
        if [ -d "$project_dir" ]; then
            while IFS= read -r -d '' candidate; do
                duplicate=false
                for existing in "${candidates[@]}"; do
                    if [ "$existing" = "$candidate" ]; then
                        duplicate=true
                        break
                    fi
                done
                if [ "$duplicate" = false ]; then
                    candidates+=("$candidate")
                fi
            done < <(find "$project_dir" -maxdepth 1 -type f -name '*.jsonl' \
                -newer "$START_MARKER" -print0 2>/dev/null)
        fi
    done

    if [ "${#candidates[@]}" -ne 1 ]; then
        fail canary_transcript_ambiguous "candidate_count=${#candidates[@]}"
    fi
    TRANSCRIPT=${candidates[0]}
    transcript_name=$(basename "$TRANSCRIPT")
    SESSION_ID=${transcript_name%.jsonl}
fi

if [ -z "$TRANSCRIPT" ]; then
    fail canary_transcript_missing "session_id=$SESSION_ID"
fi

CHECK_OUTPUT=$(mktemp "$STATE_DIR/.check.XXXXXX")
if "$CHECK_BILLING" "$TRANSCRIPT" >"$CHECK_OUTPUT" 2>&1; then
    sed -n '1,20p' "$CHECK_OUTPUT"
    rm -f "$CHECK_OUTPUT"
    CHECK_OUTPUT=
    write_result PASS entrypoint=cli "session_id=$SESSION_ID" "transcript=$TRANSCRIPT"
    exit 0
else
    check_status=$?
fi

sed -n '1,40p' "$CHECK_OUTPUT" >&2
entrypoint=$(sed -n 's/^entrypoint: //p' "$CHECK_OUTPUT" | tail -n 1)
rm -f "$CHECK_OUTPUT"
CHECK_OUTPUT=
if [ -n "$entrypoint" ]; then
    fail billing_classification "entrypoint=$entrypoint" "session_id=$SESSION_ID" \
        "check_exit=$check_status"
fi
fail billing_classification "entrypoint=missing" "session_id=$SESSION_ID" \
    "check_exit=$check_status"
