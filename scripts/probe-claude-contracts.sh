#!/usr/bin/env bash
# probe-claude-contracts.sh — live probes for the Claude Code hook contracts
# claude-print's plan pins as proof obligations PO-1/PO-2 and open questions
# OQ-1/OQ-2 (docs/plan/plan.md), plus the Stop-per-turn question in the
# glossary's Stop-hook note.
#
# Probes (one claude invocation each):
#   P1  project-source hooks fire in a plain `claude -p` run (sanity baseline)
#   P2  PO-1/OQ-1: `--settings <file>` hook fires ALONGSIDE a project hook
#       (merge, not replace), and the firing ORDER of the two sources
#   P3  OQ-2: `--setting-sources=` (empty) suppresses project/local hooks
#       while the `--settings` hook still fires
#   P4  `--setting-sources=user` loads a user source (flag parses and selects;
#       observed on SessionStart)
#   P5  PO-2 fallback spelling `--setting-sources=none`: accepted or rejected,
#       and does it suppress
#   P6  literal PO-1/OQ-1: user-source Stop hook vs `--settings` relay hook,
#       with a real completed turn — order and merge measured on the event
#       claude-print actually consumes
#   T1  Stop firing count for a multi-round tool-using run (print mode)
#   T3  Stop firing count when --max-turns CUTS a run off mid-work (print
#       mode) — the "first intermediate event" scenario for the Stop poller
#   T2  Stop firing count in the interactive TUI (what claude-print drives):
#       two prompts, the first requiring multi-round tool use
#
# Isolation guarantees:
#   - EVERY claude invocation runs with HOME redirected into a throwaway
#     sandbox. The REAL ~/.claude/settings.json and ~/.claude.json are never
#     read, written, or copied; the sandbox gets a minimal fresh .claude.json
#     with trust pre-seeded for the probe project only. The whole sandbox is
#     removed on exit via trap.
#   - Probe hooks write only inside the probe sandbox; the log holds
#     timestamps, source tags, event names, and claude's own hook payload
#     (session id / transcript path — data claude already wrote to disk in
#     the sandbox). Sanitized evidence (tags + timestamps only) is what the
#     caller records.
#   - Every probe cwd is a fresh mktemp dir; transcripts land under the
#     sandbox HOME, never under the real one.
#   - Auth travels by environment only (the host session's proxy vars are
#     inherited, values never printed); a credentials file is copied
#     file-to-file into the sandbox only if one exists, never through a pipe
#     or the transcript.
#
# Version sensitivity: results are pinned to the claude binary on PATH at run
# time (`claude --version` is stamped into the evidence). Re-run after any
# Claude Code update. Since 2026-09-26 every run is also version-guarded
# (scripts/probe-version-guard.sh, sourced below): the binary is resolved and
# pinned once, and the run aborts as failed unless its version held to the
# end — a mid-run auto-update can no longer straddle a measurement
# (docs/notes/claude-contract-probes.md §Version guard).

set -u

PROBE_ROOT="$(mktemp -d /tmp/ccprobe-XXXXXX)"
PROJ="$PROBE_ROOT/proj"
LOG="$PROBE_ROOT/firings.log"
SANDBOX_HOME="$PROBE_ROOT/home"
SETTINGS_FILE="$PROBE_ROOT/relay-settings.json"

mkdir -p "$PROJ/.claude"

cleanup() {
    rm -rf "$PROBE_ROOT"
}
trap cleanup EXIT

# ---------------------------------------------------------------------------
# Sandbox HOME (used by EVERY probe) — trust pre-seeded, onboarding skipped
# ---------------------------------------------------------------------------
mkdir -p "$SANDBOX_HOME/.claude"
python3 - "$SANDBOX_HOME" "$PROJ" <<'EOF'
import json, os, sys
home, proj = sys.argv[1], sys.argv[2]
path = os.path.join(home, ".claude.json")
data = {}
if os.path.exists(path):
    try:
        with open(path) as fh:
            data = json.load(fh)
    except json.JSONDecodeError:
        data = {}
data.setdefault("hasCompletedOnboarding", True)
data.setdefault("theme", "dark")
projects = data.setdefault("projects", {})
entry = projects.get(proj) or {}
entry.update({
    "hasTrustDialogAccepted": True,
    "hasCompletedProjectOnboarding": True,
})
projects[proj] = entry
with open(path, "w") as fh:
    json.dump(data, fh)
EOF
# OAuth credential file, if the host has one — copied file-to-file, never
# through a pipe/terminal. Env-based auth (inherited) is the primary path.
CREDS="$HOME/.claude/.credentials.json"
[ -f "$CREDS" ] && cp -p "$CREDS" "$SANDBOX_HOME/.claude/.credentials.json"

# ---------------------------------------------------------------------------
# Hook scaffolding
# ---------------------------------------------------------------------------

# One append-only log; each hook invocation stamps wall time, its source tag,
# the event name, and claude's payload. flock serializes concurrent hook
# processes so timestamps order correctly even when a matched group runs them
# in parallel.
write_hook_sh() { # <path> <tag>
    local path="$1" tag="$2"
    cat >"$path" <<EOF
#!/bin/sh
payload=\$(cat 2>/dev/null | head -c 4000)
event=\$(printf '%s' "\$payload" | sed -n 's/.*"hook_event_name":"\([A-Za-z]*\)".*/\1/p')
(
  flock 9
  printf '%s|%s|%s|%s\n' "\$(date +%s.%N)" "$tag" "\${event:-unknown}" "\$payload" >> '$LOG'
) 9>>'$LOG'
EOF
    chmod +x "$path"
}

# Wire BOTH SessionStart (source-load evidence, fires before any API call) and
# Stop (the event claude-print consumes) for the given source tag.
install_hooks() { # <settings-json-path> <hook-script-path> <tag>
    local settings="$1" hook="$2" tag="$3"
    write_hook_sh "$hook" "$tag"
    cat >"$settings" <<EOF
{"hooks": {
  "SessionStart": [{"hooks": [{"type": "command", "command": "$hook"}]}],
  "Stop": [{"hooks": [{"type": "command", "command": "$hook"}]}]
}}
EOF
}

install_hooks "$PROJ/.claude/settings.json" "$PROJ/log-project.sh" project
install_hooks "$PROBE_ROOT/relay-settings.json" "$PROBE_ROOT/log-relay.sh" relay-settings
install_hooks "$SANDBOX_HOME/.claude/settings.json" "$SANDBOX_HOME/log-user.sh" user

# ---------------------------------------------------------------------------
# Probe plumbing
# ---------------------------------------------------------------------------

# Version-straddle guard: begin pins CLAUDE_BIN (resolved once) and stamps the
# start version; probe_version_guard_end, as the script's last line, aborts
# the run as failed if that binary's version moved mid-run.
source "$(dirname "$0")/probe-version-guard.sh"
probe_version_guard_begin
CLAUDE_VERSION="$PROBE_VERSION_START_LINE"

header() { # <probe-id> <description>
    printf '\n===== %s: %s\n' "$1" "$2"
}

firings() { # <pattern> — matching log lines, timestamps only
    grep -E "\|($1)\|" "$LOG" 2>/dev/null | cut -d'|' -f1,2,3
}

count_ev() { # <tag-pattern> <event>
    awk -F'|' -v t="^($1)\$" -v e="$2" '$2 ~ t && $3 == e' "$LOG" 2>/dev/null | wc -l
}

# Child-environment contract mirrored from src/pty.rs: probe runs launched
# from inside an agent session inherit CLAUDECODE / CLAUDE_CODE_* session
# markers, which make claude start as a *nested* session (and can switch off
# transcript persistence). Scrub and force exactly what claude-print does.
SCRUB_ENV=(-u CLAUDECODE -u CLAUDE_CODE_SESSION_ID -u CLAUDE_CODE_CHILD_SESSION -u CLAUDE_CODE_SKIP_PROMPT_HISTORY)
FORCED_ENV=(CLAUDE_CODE_ENTRYPOINT=cli CLAUDE_CODE_FORCE_SESSION_PERSISTENCE=1 CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC=1)

run_p() { # <label> <extra args...> — timed `claude -p` run, sandbox HOME
    local label="$1"
    shift
    local out rc
    out="$( cd "$PROJ" && HOME="$SANDBOX_HOME" timeout 120 env "${SCRUB_ENV[@]}" "${FORCED_ENV[@]}" \
        "$CLAUDE_BIN" -p "$@" "Reply with exactly: OK" </dev/null 2>&1 )"
    rc=$?
    printf '  [%s] claude exit=%s reply=%s\n' "$label" "$rc" "$(printf '%s' "$out" | head -c 80 | tr '\n' ' ')"
    if [ $rc -ne 0 ]; then
        printf '  [%s] stderr/stdout tail: %s\n' "$label" "$(printf '%s' "$out" | tail -c 300 | tr '\n' ' ')"
    fi
}

printf 'claude version (run): %s\n' "$CLAUDE_VERSION"

# ---------------------------------------------------------------------------
# P1 — baseline: project hooks fire in print mode
# ---------------------------------------------------------------------------
header P1 "baseline: project-source SessionStart+Stop fire in a plain claude -p run"
run_p P1 --setting-sources=project
printf '  SessionStart: %s  Stop: %s\n' "$(count_ev project SessionStart)" "$(count_ev project Stop)"

# ---------------------------------------------------------------------------
# P2 — PO-1/OQ-1: --settings hook merges with a project hook, and order
# ---------------------------------------------------------------------------
header P2 "merge + order: --settings relay hook alongside project hook"
run_p P2 --setting-sources=project --settings "$SETTINGS_FILE"
echo "  firings in log order (ts|tag|event):"
firings 'project|relay-settings' | awk -F'|' '{print "   ", $1, $2, $3}'

# ---------------------------------------------------------------------------
# P3 — OQ-2 primary: --setting-sources= (empty) suppresses standard sources
# ---------------------------------------------------------------------------
header P3 "suppression: --setting-sources= (empty) vs project hook"
run_p P3 --setting-sources= --settings "$SETTINGS_FILE"
printf '  SessionStart project: %s relay: %s   Stop project: %s relay: %s\n' \
    "$(count_ev project SessionStart)" "$(count_ev relay-settings SessionStart)" \
    "$(count_ev project Stop)" "$(count_ev relay-settings Stop)"

# ---------------------------------------------------------------------------
# P4 — the flag parses and selects: --setting-sources=user (sandboxed user)
# ---------------------------------------------------------------------------
header P4 "selection: --setting-sources=user loads the (sandboxed) user source"
( cd "$PROJ" && HOME="$SANDBOX_HOME" timeout 120 env "${SCRUB_ENV[@]}" "${FORCED_ENV[@]}" \
    "$CLAUDE_BIN" -p --setting-sources=user "Reply with exactly: OK" </dev/null >/dev/null 2>&1 )
echo "  claude exit=$?"
printf '  SessionStart user: %s project: %s\n' "$(count_ev user SessionStart)" "$(count_ev project SessionStart)"

# ---------------------------------------------------------------------------
# P5 — PO-2 fallback spelling: --setting-sources=none
# ---------------------------------------------------------------------------
header P5 "fallback spelling: --setting-sources=none"
run_p P5 --setting-sources=none --settings "$SETTINGS_FILE"
printf '  SessionStart project: %s relay: %s   Stop project: %s relay: %s\n' \
    "$(count_ev project SessionStart)" "$(count_ev relay-settings SessionStart)" \
    "$(count_ev project Stop)" "$(count_ev relay-settings Stop)"

# ---------------------------------------------------------------------------
# P6 — literal PO-1/OQ-1: user Stop hook vs relay Stop hook, real turn
# ---------------------------------------------------------------------------
header P6 "user source + relay Stop merge/order (sandbox HOME, real turn)"
run_p P6 --settings "$SETTINGS_FILE"
echo "  firings in log order (ts|tag|event):"
firings 'user|relay-settings' | awk -F'|' '{print "   ", $1, $2, $3}'

# ---------------------------------------------------------------------------
# T1 — Stop count: multi-round tool-using run (print mode)
# ---------------------------------------------------------------------------
header T1 "Stop count: multi-round tool-using run, print mode, --max-turns 6"
cat >"$PROJ/toolwork.sh" <<'EOF'
#!/bin/sh
echo "step-one-done"
EOF
chmod +x "$PROJ/toolwork.sh"
( cd "$PROJ" && HOME="$SANDBOX_HOME" timeout 240 env "${SCRUB_ENV[@]}" "${FORCED_ENV[@]}" "$CLAUDE_BIN" -p \
    --setting-sources=project \
    --max-turns 6 \
    "Use the Bash tool to run $PROJ/toolwork.sh. Only after you see its output, run 'echo step-two-done' with the Bash tool — do not run the two commands in parallel, the second must start only after the first finished. Then reply DONE." </dev/null >/dev/null 2>&1 )
echo "  claude exit=$?"
echo "  Stop firings (project): $(count_ev project Stop)"

# ---------------------------------------------------------------------------
# T3 — Stop count when --max-turns cuts the run off mid-work (print mode)
# ---------------------------------------------------------------------------
header T3 "Stop count: --max-turns 2 cuts off a 4-round task (print mode)"
( cd "$PROJ" && HOME="$SANDBOX_HOME" timeout 240 env "${SCRUB_ENV[@]}" "${FORCED_ENV[@]}" "$CLAUDE_BIN" -p \
    --setting-sources=project \
    --max-turns 2 \
    "Use the Bash tool to run 'echo r1'. Only after seeing its output run 'echo r2', then only after that 'echo r3', then 'echo r4' — four separate Bash calls, strictly one at a time, never parallel, then reply ALLDONE." </dev/null >/dev/null 2>&1 )
echo "  claude exit=$?"
echo "  Stop firings (project): $(count_ev project Stop)"

# ---------------------------------------------------------------------------
# T2 — Stop count in the interactive TUI (what claude-print drives)
# ---------------------------------------------------------------------------
header T2 "TUI: two prompts, first with multi-round tool use — Stop count per turn"
if [ -f "$(dirname "$0")/probe-tui-stop.py" ]; then
    python3 "$(dirname "$0")/probe-tui-stop.py" \
        "$CLAUDE_BIN" "$PROJ" "$SANDBOX_HOME" \
        "$PROJ/log-project.sh" "$LOG" "--max-turns=6" || true
else
    echo "  skipped (probe-tui-stop.py unavailable)"
fi

# ---------------------------------------------------------------------------
# Evidence summary
# ---------------------------------------------------------------------------
printf '\n===== claude %s — raw firing log (ts|tag|event only)\n' "$CLAUDE_VERSION"
cut -d'|' -f1,2,3 "$LOG" 2>/dev/null
printf '\n(probe root %s removed on exit)\n' "$PROBE_ROOT"

# Last line: the version-straddle bracket closes here — a version change since
# begin aborts the run as failed (exit 1) so its evidence cannot be pinned.
probe_version_guard_end
