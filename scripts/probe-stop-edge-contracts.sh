#!/usr/bin/env bash
# probe-stop-edge-contracts.sh — the Stop/OQ-1 edge measurements the original
# 2.1.270 run measured once (claudepr-6ef2541c) and the 2.1.281 / 2.1.282
# re-pins did not repeat: the sleeping-hook cross-source concurrency probe
# (OQ-1's direct proof that hooks from different sources execute CONCURRENTLY,
# not sequentially — timestamps alone cannot show this) and the degraded-path
# Stop count (headless run whose tool calls are permission-denied — the
# hazard that produced one EXTRA Stop firing in 1 of 2 runs on 2.1.270).
# Re-measured against 2.1.282 on 2026-09-25 (claudepr-d9553d38). Arm T (added
# 2026-09-26, claudepr-352cf1df, measured against the same pinned 2.1.282 —
# via the persisted versions-dir binary on a PATH shim while the host had
# drifted to 2.1.283; see claude-contract-probes.md §Reproducing)
# extends the suite to hook-timeout enforcement — the relay contract
# hook-design.md relies on when it configures `"timeout": 10` and states
# Claude Code "does not wait beyond the 10s timeout". This script is the
# reproducible owner of all three arms and is re-run by the maintenance
# gate's --run-probes set like the other model-turn probes.
#
# Arm S (sleeping hooks — concurrency): project-source hook sleeps 300 ms and
#   logs start/end timestamps; the `--settings` relay hook sleeps 0 ms and
#   logs start/end. N real single-turn runs. Per event (SessionStart, Stop)
#   and run: did the relay START before the project hook started (start-order
#   flip), and did it start before the project hook ENDED (concurrent
#   execution — the property the read-race note depends on)?
#
# Arm D (degraded path — permission-denied tools): the multi-round sequential
#   tool prompt with NO allowlist, headless `-p`, `--max-turns 6`; N runs.
#   Counts Stop firings per run and classifies each run: completed (exit 0)
#   vs cut off by --max-turns (exit 1, zero firings — the measured cutoff
#   contract, a re-run shape, not a finding) — an extra Stop would be >1
#   firing per loaded source on a completed run.
#
# Arm T (relay-hook timeout enforcement): the `--settings` hook — the relay
#   position, run with `--setting-sources=` (empty) so it is the ONLY loaded
#   hook, claude-print's isolation-mode shape — is configured with a per-hook
#   `timeout` of N seconds but sleeps far past it. Per event (SessionStart,
#   Stop) and run: did the hook log `end` after its `start` (it outlived the
#   timeout) or was it killed first (`end` never appears), did the session
#   proceed (exit 0, reply rendered), and how long after the Stop hook's
#   start did the claude process exit (≈ the timeout, not the sleep)?
#
# Isolation guarantees (identical to probe-claude-contracts.sh): every claude
# invocation runs with HOME redirected into a throwaway sandbox; the real
# ~/.claude is never read, written, or copied; trust is pre-seeded for the
# probe cwd only; CLAUDECODE* session markers are scrubbed and
# CLAUDE_CODE_ENTRYPOINT=cli + CLAUDE_CODE_FORCE_SESSION_PERSISTENCE=1 forced
# (mirroring src/pty.rs's child-environment contract); auth travels by
# inherited environment only. Probe hooks write only inside the sandbox; the
# recorded evidence is timestamps, source tags, event names, exit codes, and
# the synthetic prompt's own reply text — no user data.
#
# Version sensitivity: results are pinned to the claude binary on PATH at run
# time (`claude --version` is stamped into the evidence). Re-run after any
# Claude Code update (docs/notes/claude-contract-probes.md §Maintenance).
# Since 2026-09-26 every run is also version-guarded
# (scripts/probe-version-guard.sh, sourced below): the binary is resolved and
# pinned once, and the run aborts as failed unless its version held to the
# end — a mid-run auto-update can no longer straddle a measurement
# (claude-contract-probes.md §Version guard; this probe's Arm T evidence was
# itself gathered across a 2.1.283 drift window on a pinned 2.1.282 shim).

set -u

PROBE_ROOT="$(mktemp -d /tmp/ccprobe-edge-XXXXXX)"
PROJ="$PROBE_ROOT/proj"
LOG="$PROBE_ROOT/firings.log"
SANDBOX_HOME="$PROBE_ROOT/home"
SETTINGS_FILE="$PROBE_ROOT/relay-settings.json"

ARM_S_RUNS="${ARM_S_RUNS:-6}"
ARM_D_RUNS="${ARM_D_RUNS:-5}"
ARM_T_RUNS="${ARM_T_RUNS:-4}"
ARM_T_TIMEOUT="${ARM_T_TIMEOUT:-5}"
ARM_T_SLEEP="${ARM_T_SLEEP:-30}"

mkdir -p "$PROJ/.claude" "$SANDBOX_HOME/.claude"
: >"$LOG"

cleanup() {
    rm -rf "$PROBE_ROOT"
}
trap cleanup EXIT

# ---------------------------------------------------------------------------
# Sandbox HOME — trust pre-seeded, onboarding skipped
# ---------------------------------------------------------------------------
python3 - "$SANDBOX_HOME" "$PROJ" <<'EOF'
import json, os, sys
home, proj = sys.argv[1], sys.argv[2]
path = os.path.join(home, ".claude.json")
data = {"hasCompletedOnboarding": True, "theme": "dark",
        "projects": {proj: {"hasTrustDialogAccepted": True,
                             "hasCompletedProjectOnboarding": True}}}
with open(path, "w") as fh:
    json.dump(data, fh)
EOF
CREDS="$HOME/.claude/.credentials.json"
[ -f "$CREDS" ] && cp -p "$CREDS" "$SANDBOX_HOME/.claude/.credentials.json"

# ---------------------------------------------------------------------------
# Hook scaffolding
# ---------------------------------------------------------------------------

# Arm S hooks: log `ts|tag|event|start`, (optionally sleep), log
# `ts|tag|event|end`. The two log writes lock independently so a sleeping
# hook never holds the log lock across its sleep (that would serialize the
# very concurrency being measured).
write_sleeping_hook() { # <path> <tag> <sleep-seconds>
    local path="$1" tag="$2" sleep_secs="$3"
    cat >"$path" <<EOF
#!/bin/sh
payload=\$(cat 2>/dev/null | head -c 4000)
event=\$(printf '%s' "\$payload" | sed -n 's/.*"hook_event_name":"\([A-Za-z]*\)".*/\1/p')
(
  flock 9
  printf '%s|%s|%s|%s\n' "\$(date +%s.%N)" "$tag" "\${event:-unknown}" "start" >> '$LOG'
) 9>>'$LOG'
sleep $sleep_secs
(
  flock 9
  printf '%s|%s|%s|%s\n' "\$(date +%s.%N)" "$tag" "\${event:-unknown}" "end" >> '$LOG'
) 9>>'$LOG'
EOF
    chmod +x "$path"
}

install_pair() { # <settings-json-path> <hook-script-path>
    local settings="$1" hook="$2"
    cat >"$settings" <<EOF
{"hooks": {
  "SessionStart": [{"hooks": [{"type": "command", "command": "$hook"}]}],
  "Stop": [{"hooks": [{"type": "command", "command": "$hook"}]}]
}}
EOF
}

# Arm D hook: the probe-stop-toolallowed.sh layout — ts|payload, so per-firing
# payload fields (session id, stop_hook_active, last_assistant_message) can be
# summarized. SessionStart is wired too: on a cutoff run its firing proves the
# source loaded and Stop legitimately did not fire.
write_payload_hook() { # <path>
    local path="$1"
    cat >"$path" <<EOF
#!/bin/sh
payload=\$(cat 2>/dev/null | head -c 4000)
(
  flock 9
  printf '%s|%s\n' "\$(date +%s.%N)" "\$payload" >> '$LOG'
) 9>>'$LOG'
EOF
    chmod +x "$path"
}

# ---------------------------------------------------------------------------
# Probe plumbing
# ---------------------------------------------------------------------------

# Version-straddle guard: begin pins CLAUDE_BIN (resolved once) and stamps the
# start version; probe_version_guard_end, as the script's last line, aborts
# the run as failed if that binary's version moved mid-run.
source "$(dirname "$0")/probe-version-guard.sh"
probe_version_guard_begin
CLAUDE_VERSION="$PROBE_VERSION_START_LINE"

SCRUB_ENV=(-u CLAUDECODE -u CLAUDE_CODE_SESSION_ID -u CLAUDE_CODE_CHILD_SESSION -u CLAUDE_CODE_SKIP_PROMPT_HISTORY)
FORCED_ENV=(CLAUDE_CODE_ENTRYPOINT=cli CLAUDE_CODE_FORCE_SESSION_PERSISTENCE=1 CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC=1)

loglines() { wc -l <"$LOG" 2>/dev/null || echo 0; }

printf 'claude version (run): %s\n' "$CLAUDE_VERSION"
printf 'arm S runs: %s   arm D runs: %s   arm T runs: %s (timeout %ss, sleep %ss)\n' \
    "$ARM_S_RUNS" "$ARM_D_RUNS" "$ARM_T_RUNS" "$ARM_T_TIMEOUT" "$ARM_T_SLEEP"

# ---------------------------------------------------------------------------
# Arm S — sleeping-hook cross-source concurrency (OQ-1 direct proof)
# ---------------------------------------------------------------------------
echo "===== Arm S: project hook sleeps 300 ms (start/end logged), relay hook 0 ms"

write_sleeping_hook "$PROJ/log-project.sh" project 0.3
write_sleeping_hook "$PROBE_ROOT/log-relay.sh" relay 0
install_pair "$PROJ/.claude/settings.json" "$PROJ/log-project.sh"
install_pair "$SETTINGS_FILE" "$PROBE_ROOT/log-relay.sh"

analyze_arm_s_run() { # <base-line-count> <run-label>
    python3 - "$LOG" "$1" "$2" <<'EOF'
import sys

log_path, base_s, label = sys.argv[1], int(sys.argv[2]), sys.argv[3]
entries = {}
with open(log_path) as fh:
    for i, line in enumerate(fh, 1):
        if i <= base_s:
            continue
        parts = line.rstrip("\n").split("|")
        if len(parts) != 4:
            continue
        try:
            ts = float(parts[0])
        except ValueError:
            continue
        entries[(parts[2], parts[1], parts[3])] = ts

for event in ("SessionStart", "Stop"):
    ps = entries.get((event, "project", "start"))
    pe = entries.get((event, "project", "end"))
    rs = entries.get((event, "relay", "start"))
    re = entries.get((event, "relay", "end"))
    if None in (ps, pe, rs):
        print(f"  {event}: INCOMPLETE (project {ps}->{pe}, relay {rs}->{re})")
        print(f"STAT-S {label} {event} incomplete")
        continue
    flip = rs < ps
    conc = rs < pe
    dur = pe - ps
    print(f"  {event}: project {ps:.6f}->{pe:.6f} (dur {dur*1000:.1f} ms)  "
          f"relay {rs:.6f}->{re:.6f}")
    print(f"  {event}: relay-start-before-project-start={'yes' if flip else 'no'}  "
          f"relay-start-before-project-end={'yes' if conc else 'no'}")
    print(f"STAT-S {label} {event} flip={1 if flip else 0} conc={1 if conc else 0}")
EOF
}

i=1
while [ "$i" -le "$ARM_S_RUNS" ]; do
    BASE="$(loglines)"
    out="$( cd "$PROJ" && HOME="$SANDBOX_HOME" timeout 120 env "${SCRUB_ENV[@]}" "${FORCED_ENV[@]}" \
        "$CLAUDE_BIN" -p --setting-sources=project --settings "$SETTINGS_FILE" \
        "Reply with exactly: OK" </dev/null 2>&1 )"
    rc=$?
    printf '  [S%d] claude exit=%s reply=%s\n' "$i" "$rc" "$(printf '%s' "$out" | head -c 60 | tr '\n' ' ')"
    if [ "$rc" -ne 0 ]; then
        printf '  [S%d] output tail: %s\n' "$i" "$(printf '%s' "$out" | tail -c 300 | tr '\n' ' ')"
    fi
    analyze_arm_s_run "$BASE" "S$i"
    i=$((i + 1))
done

# Swap in a payload-logging project hook for Arm D (fresh wiring below).
rm -f "$SETTINGS_FILE"

# ---------------------------------------------------------------------------
# Arm D — degraded path: multi-round tool prompt, NO allowlist (permission-
# denied), headless print mode
# ---------------------------------------------------------------------------
echo
echo "===== Arm D: degraded runs (no allowlist, headless -p), Stop counts per run"

write_payload_hook "$PROJ/log-project.sh"
cat >"$PROJ/.claude/settings.json" <<EOF
{"hooks": {
  "SessionStart": [{"hooks": [{"type": "command", "command": "$PROJ/log-project.sh"}]}],
  "Stop": [{"hooks": [{"type": "command", "command": "$PROJ/log-project.sh"}]}]
}}
EOF

cat >"$PROJ/toolwork.sh" <<'EOF'
#!/bin/sh
echo "step-one-done"
EOF
chmod +x "$PROJ/toolwork.sh"

summarize_arm_d_run() { # <base-line-count>
    python3 - "$LOG" "$1" <<'EOF'
import json, sys

base = int(sys.argv[2])
rows = []
with open(sys.argv[1]) as fh:
    for i, line in enumerate(fh, 1):
        if i <= base:
            continue
        ts, _, payload = line.rstrip("\n").partition("|")
        try:
            p = json.loads(payload)
        except json.JSONDecodeError:
            rows.append((ts, "?", "", ""))
            continue
        rows.append((
            ts,
            p.get("hook_event_name") or "?",
            str(p.get("stop_hook_active")),
            (p.get("last_assistant_message") or "").replace("\n", " ")[:60],
        ))
for ts, ev, sha, lam in rows:
    print(f"  ts={ts} event={ev} stop_hook_active={sha}")
    if lam:
        print(f"      last_msg: {lam}")
print(f"  firings this run: {len(rows)} "
      f"(Stop: {sum(1 for r in rows if r[1] == 'Stop')}, "
      f"SessionStart: {sum(1 for r in rows if r[1] == 'SessionStart')})")
EOF
}

i=1
while [ "$i" -le "$ARM_D_RUNS" ]; do
    BASE="$(loglines)"
    out="$( cd "$PROJ" && HOME="$SANDBOX_HOME" timeout 240 env "${SCRUB_ENV[@]}" "${FORCED_ENV[@]}" \
        "$CLAUDE_BIN" -p --setting-sources=project --max-turns 6 \
        "Use the Bash tool to run $PROJ/toolwork.sh. Only after you see its output, run 'echo step-two-done' with the Bash tool — do not run the two commands in parallel, the second must start only after the first finished. Then reply DONE." \
        </dev/null 2>&1 )"
    rc=$?
    printf '  [D%d] claude exit=%s reply=%s\n' "$i" "$rc" "$(printf '%s' "$out" | head -c 100 | tr '\n' ' ')"
    if [ "$rc" -ne 0 ]; then
        printf '  [D%d] output tail: %s\n' "$i" "$(printf '%s' "$out" | tail -c 300 | tr '\n' ' ')"
    fi
    summarize_arm_d_run "$BASE"
    i=$((i + 1))
done

# ---------------------------------------------------------------------------
# Arm T — relay-hook timeout enforcement: a hook sleeping past its configured
# per-hook `timeout` is killed and the session proceeds (the contract
# hook-design.md's `timeout: 10` relay hooks rely on)
# ---------------------------------------------------------------------------
echo
echo "===== Arm T: relay-position hook sleeps ${ARM_T_SLEEP}s past its ${ARM_T_TIMEOUT}s timeout"

# Same start/sleep/end logging hook as Arm S, sleeping far past the timeout
# configured below. Killed-early shows as: `start` logged, `end` never.
write_sleeping_hook "$PROBE_ROOT/log-timed.sh" timed "$ARM_T_SLEEP"
cat >"$SETTINGS_FILE" <<EOF
{"hooks": {
  "SessionStart": [{"hooks": [{"type": "command", "command": "$PROBE_ROOT/log-timed.sh", "timeout": $ARM_T_TIMEOUT}]}],
  "Stop": [{"hooks": [{"type": "command", "command": "$PROBE_ROOT/log-timed.sh", "timeout": $ARM_T_TIMEOUT}]}]
}}
EOF

analyze_arm_t_run() { # <base-line-count> <run-label> <claude-exit-ts>
    python3 - "$LOG" "$1" "$2" "$3" "$ARM_T_TIMEOUT" <<'EOF'
import sys

log_path, base_s, label, exit_ts, timeout_s = (
    sys.argv[1], int(sys.argv[2]), sys.argv[3], float(sys.argv[4]), float(sys.argv[5]),
)
entries = {}
with open(log_path) as fh:
    for i, line in enumerate(fh, 1):
        if i <= base_s:
            continue
        parts = line.rstrip("\n").split("|")
        if len(parts) != 4 or parts[1] != "timed":
            continue
        try:
            ts = float(parts[0])
        except ValueError:
            continue
        entries[(parts[2], parts[3])] = ts

for event in ("SessionStart", "Stop"):
    st = entries.get((event, "start"))
    en = entries.get((event, "end"))
    if st is None:
        print(f"  {event}: no firing logged")
        print(f"STAT-T {label} {event} absent")
        continue
    if en is None:
        print(f"  {event}: start {st:.6f}, END NEVER LOGGED — hook killed before "
              f"its sleep finished (timeout {timeout_s:.0f}s honored)")
        print(f"STAT-T {label} {event} killed=1 completed=0")
    else:
        print(f"  {event}: start {st:.6f} -> end {en:.6f} "
              f"(ran {(en - st) * 1000:.0f} ms — OUTLIVED the {timeout_s:.0f}s timeout)")
        print(f"STAT-T {label} {event} killed=0 completed=1")
    if event == "Stop":
        print(f"  Stop hook start -> claude exit: {exit_ts - st:.1f}s "
              f"(timeout {timeout_s:.0f}s, sleep far longer)")
EOF
}

i=1
while [ "$i" -le "$ARM_T_RUNS" ]; do
    BASE="$(loglines)"
    RUN_START="$(date +%s)"
    out="$( cd "$PROJ" && HOME="$SANDBOX_HOME" timeout 180 env "${SCRUB_ENV[@]}" "${FORCED_ENV[@]}" \
        "$CLAUDE_BIN" -p --setting-sources= --settings "$SETTINGS_FILE" \
        "Reply with exactly: OK" </dev/null 2>&1 )"
    rc=$?
    END_TS="$(date +%s.%N)"
    wall=$(( $(date +%s) - RUN_START ))
    if printf '%s' "$out" | grep -q 'OK'; then rok=yes; else rok=no; fi
    printf '  [T%d] claude exit=%s reply-contains-OK=%s (wall %ss)\n' "$i" "$rc" "$rok" "$wall"
    printf '  [T%d] reply head: %s\n' "$i" "$(printf '%s' "$out" | head -c 60 | tr '\n' ' ')"
    if [ "$rc" -ne 0 ]; then
        printf '  [T%d] output tail: %s\n' "$i" "$(printf '%s' "$out" | tail -c 300 | tr '\n' ' ')"
    fi
    analyze_arm_t_run "$BASE" "T$i" "$END_TS"
    i=$((i + 1))
done

# ---------------------------------------------------------------------------
# Evidence summary
# ---------------------------------------------------------------------------
printf '\n===== claude %s — raw firing log (ts|tag|event|phase for arm S; ts|payload elided for arm D)\n' "$CLAUDE_VERSION"
awk -F'|' 'NF==4 {print $1"|"$2"|"$3"|"$4} NF==2 {print $1"|payload"}' "$LOG" 2>/dev/null
printf '\n(probe root %s removed on exit)\n' "$PROBE_ROOT"

# Last line: the version-straddle bracket closes here — a version change since
# begin aborts the run as failed (exit 1) so its evidence cannot be pinned.
probe_version_guard_end
