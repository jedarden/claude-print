#!/usr/bin/env bash
# billing-canary.sh - Automated AS-4 billing-classification canary
#
# CLAUDE_PRINT_POOL=1 runs the canary session against a warm pool instead of
# a stateless spawn: a `claude-print serve` daemon (pool size 1) is started on
# a private socket in the state dir, the one-turn Haiku session acquires its
# prewarmed worker via --pool-socket, and the daemon is torn down afterwards.
# This proves the ADR-005 invariant that POOLED sessions bill as `cli` too —
# the same transcript check runs either way. The default (no env) stateless
# leg is what the systemd timer exercises daily; the pooled leg is for
# manual/periodic verification of the pool path.

set -eu

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
CLAUDE_PRINT_BIN=${CLAUDE_PRINT_BIN:-claude-print}
CHECK_BILLING=${CLAUDE_PRINT_CHECK_BILLING:-"$SCRIPT_DIR/check-billing.sh"}
TRANSCRIPTS_DIR=${CLAUDE_PRINT_TRANSCRIPTS_DIR:-"$HOME/.claude/projects"}
STATE_HOME=${XDG_STATE_HOME:-"$HOME/.local/state"}
STATE_DIR=${CLAUDE_PRINT_BILLING_STATE_DIR:-"$STATE_HOME/claude-print/billing-canary"}
RESULT_FILE="$STATE_DIR/last-result"
WORK_DIR="$STATE_DIR/workdir"

MODE=stateless
DAEMON_PID=
POOL_SOCKET=
DAEMON_LOG=

mkdir -p "$STATE_DIR" "$WORK_DIR"
chmod 700 "$STATE_DIR"
chmod 700 "$WORK_DIR"

STDOUT_FILE=$(mktemp "$STATE_DIR/.stdout.XXXXXX")
STDERR_FILE=$(mktemp "$STATE_DIR/.stderr.XXXXXX")
START_MARKER=$(mktemp "$STATE_DIR/.started.XXXXXX")
CHECK_OUTPUT=
cleanup() {
    if [ -n "$DAEMON_PID" ]; then
        kill -TERM "$DAEMON_PID" 2>/dev/null || true
        i=0
        while [ "$i" -lt 100 ] && kill -0 "$DAEMON_PID" 2>/dev/null; do
            sleep 0.1
            i=$((i + 1))
        done
        kill -KILL "$DAEMON_PID" 2>/dev/null || true
        wait "$DAEMON_PID" 2>/dev/null || true
    fi
    if [ -n "$POOL_SOCKET" ]; then
        rm -f "$POOL_SOCKET"
    fi
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
    printf '%s timestamp=%s mode=%s %s\n' "$status" "$timestamp" "$MODE" "$*" > "$result_tmp"
    chmod 600 "$result_tmp"
    mv -f "$result_tmp" "$RESULT_FILE"
    printf 'CLAUDE_PRINT_BILLING_CANARY status=%s timestamp=%s mode=%s %s\n' \
        "$status" "$timestamp" "$MODE" "$*"
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

# Pooled leg (CLAUDE_PRINT_POOL=1): start the daemon and wait until its one
# worker is warm — benchmarking a cold pool would test warmup, and acquiring
# before readiness would silently fall back stateless and prove nothing about
# the pool path. The --verbose "settled and ready" line is the readiness
# contract the e2e suites pin; the daemon log is kept for diagnosis. The wait
# is 0.1 s per try, 1200 tries = 120 s by default; CLAUDE_PRINT_POOL_WARMUP_
# TRIES shortens it for hermetic tests of the failure shapes.
if [ "${CLAUDE_PRINT_POOL:-0}" = 1 ]; then
    MODE=pooled
    POOL_SOCKET="$STATE_DIR/pool.sock"
    DAEMON_LOG="$STATE_DIR/pool-daemon.log"
    WARMUP_TRIES=${CLAUDE_PRINT_POOL_WARMUP_TRIES:-1200}
    printf '[INFO] Starting warm-pool daemon (pool size 1)\n'
    (cd "$WORK_DIR" && exec "$CLAUDE_PRINT_BIN" serve \
        --pool-size 1 --socket "$POOL_SOCKET" --verbose) >"$DAEMON_LOG" 2>&1 &
    DAEMON_PID=$!
    warm=0
    i=0
    while [ "$i" -lt "$WARMUP_TRIES" ]; do
        if ! kill -0 "$DAEMON_PID" 2>/dev/null; then
            sed -n '1,20p' "$DAEMON_LOG" >&2
            fail pool_daemon_exited "pid=$DAEMON_PID"
        fi
        settled=$(grep -c 'settled and ready' "$DAEMON_LOG" 2>/dev/null || true)
        if [ "${settled:-0}" -ge 1 ]; then
            warm=1
            break
        fi
        sleep 0.1
        i=$((i + 1))
    done
    if [ "$warm" != 1 ]; then
        sed -n '1,20p' "$DAEMON_LOG" >&2
        fail pool_daemon_warmup_timeout "pid=$DAEMON_PID"
    fi
fi

printf '[INFO] Running one-turn Haiku billing canary (mode=%s)\n' "$MODE"
POOL_ARGS=()
if [ -n "$POOL_SOCKET" ]; then
    POOL_ARGS=(--pool-socket "$POOL_SOCKET")
fi
# ${POOL_ARGS[@]+...} keeps the empty-array expansion legal under `set -u` on
# bash < 4.4 (the daily timer runs the stateless leg, where POOL_ARGS is empty).
if (
    cd "$WORK_DIR"
    "$CLAUDE_PRINT_BIN" \
        ${POOL_ARGS[@]+"${POOL_ARGS[@]}"} \
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
    fail invocation_failed "exit_code=$invocation_status" "mode=$MODE"
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
