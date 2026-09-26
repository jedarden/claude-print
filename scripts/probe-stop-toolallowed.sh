#!/usr/bin/env bash
# probe-stop-toolallowed.sh — authoritative Stop-count probes with tool use
# actually permitted. The first-round probes (T1/T2 in probe-claude-contracts.sh)
# ran without an allowlist, so the model's Bash calls were permission-blocked
# and the measured Stop counts belong to degraded runs. This probe grants
# Bash via --allowedTools and records per-firing payload fields.
#
# Arm P (print): one prompt, two sequential Bash rounds, per-firing detail.
# Arm T (TUI):   two prompts (multi-round, then plain reply) driven under a
#                PTY with per-firing detail.
#
# Version sensitivity: results are pinned to the claude binary on PATH at run
# time, and since 2026-09-26 every run is version-guarded
# (scripts/probe-version-guard.sh, sourced below): the binary is resolved and
# pinned once, and the run aborts as failed unless its version held to the
# end — a mid-run auto-update can no longer straddle a measurement
# (docs/notes/claude-contract-probes.md §Version guard).

set -u

PROBE_ROOT="$(mktemp -d /tmp/ccprobe-allow-XXXXXX)"
PROJ="$PROBE_ROOT/proj"
LOG="$PROBE_ROOT/firings.log"
SANDBOX_HOME="$PROBE_ROOT/home"

mkdir -p "$PROJ/.claude" "$SANDBOX_HOME/.claude"

cleanup() { rm -rf "$PROBE_ROOT"; }
trap cleanup EXIT

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

HOOK="$PROJ/log-project.sh"
cat >"$HOOK" <<EOF
#!/bin/sh
payload=\$(cat 2>/dev/null | head -c 4000)
(
  flock 9
  printf '%s|%s\n' "\$(date +%s.%N)" "\$payload" >> '$LOG'
) 9>>'$LOG'
EOF
chmod +x "$HOOK"

cat >"$PROJ/.claude/settings.json" <<EOF
{"hooks": {"Stop": [{"hooks": [{"type": "command", "command": "$HOOK"}]}]}}
EOF

cat >"$PROJ/toolwork.sh" <<'EOF'
#!/bin/sh
echo "step-one-done"
EOF
chmod +x "$PROJ/toolwork.sh"

# Version-straddle guard: begin pins CLAUDE_BIN (resolved once) and stamps the
# start version; probe_version_guard_end, as the script's last line, aborts
# the run as failed if that binary's version moved mid-run.
source "$(dirname "$0")/probe-version-guard.sh"
probe_version_guard_begin

SCRUB=(-u CLAUDECODE -u CLAUDE_CODE_SESSION_ID -u CLAUDE_CODE_CHILD_SESSION -u CLAUDE_CODE_SKIP_PROMPT_HISTORY)
FORCE=(CLAUDE_CODE_ENTRYPOINT=cli CLAUDE_CODE_FORCE_SESSION_PERSISTENCE=1 CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC=1)

summarize() {
    python3 - "$1" "$LOG" <<'EOF'
import json, sys, os
baseline = int(sys.argv[1])
log_path = sys.argv[2]
rows = []
with open(log_path) as fh:
    for i, line in enumerate(fh, 1):
        if i <= baseline:
            continue
        ts, _, payload = line.partition("|")
        try:
            p = json.loads(payload)
        except json.JSONDecodeError:
            rows.append((ts, "?", "", "", "UNPARSEABLE"))
            continue
        rows.append((
            ts,
            p.get("hook_event_name") or "?",
            (p.get("session_id") or "")[:8],
            str(p.get("stop_hook_active")),
            (p.get("last_assistant_message") or "").replace("\n", " ")[:70],
        ))
for ts, ev, sid, sha, lam in rows:
    print(f"  ts={ts} event={ev} session={sid} stop_hook_active={sha}")
    if lam:
        print(f"      last_msg: {lam}")
print(f"  firings this arm: {len(rows)}")
EOF
}

# ---------------------------------------------------------------- Arm P (print)
echo "===== Arm P: print mode, Bash allowed, two sequential tool rounds"
BASE=0
( cd "$PROJ" && HOME="$SANDBOX_HOME" timeout 240 env "${SCRUB[@]}" "${FORCE[@]}" \
    "$CLAUDE_BIN" -p --setting-sources=project --max-turns 6 \
    --allowedTools=Bash \
    "Use the Bash tool to run $PROJ/toolwork.sh. Only after you see its output, run 'echo step-two-done' with the Bash tool — do not run the two commands in parallel, the second must start only after the first finished. Then reply DONE." \
    </dev/null >/dev/null 2>&1 )
echo "  claude exit=$?"
summarize "$BASE"
BASE=$(wc -l <"$LOG" 2>/dev/null || echo 0)

# ---------------------------------------------------------------- Arm T (TUI)
echo "===== Arm T: TUI mode, Bash allowed, two prompts (multi-round then plain)"
python3 - "$CLAUDE_BIN" "$PROJ" "$SANDBOX_HOME" "$LOG" "$BASE" <<'EOF'
import fcntl, json, os, pty, re, select, signal, struct, sys, termios, time

claude_bin, proj, sandbox_home, log_path, base_s = sys.argv[1:6]
BASE = int(base_s)

SCRUBBED = ["CLAUDE_CODE_SESSION_ID", "CLAUDECODE", "CLAUDE_CODE_CHILD_SESSION",
            "CLAUDE_CODE_SKIP_PROMPT_HISTORY"]
FORCED = {"CLAUDE_CODE_ENTRYPOINT": "cli", "CLAUDE_CODE_FORCE_SESSION_PERSISTENCE": "1"}
PASTE_ON, PASTE_OFF = "\x1b[200~", "\x1b[201~"
PROMPT_1 = ("Use the Bash tool to run {twsh}. Only after you see its output, run "
            "'echo step-two-done' with the Bash tool. Do not run the two commands "
            "in parallel — the second must start only after the first finished. "
            "Then reply DONE.").format(twsh=os.path.join(proj, "toolwork.sh"))
PROMPT_2 = "Reply with exactly: TUI-SECOND-OK"
QUIET_SECS = 6.0
STARTUP_TIMEOUT = 120.0
TURN_TIMEOUT = 180.0
POST_STOP_WATCH = 25.0


def strip_ansi(data: bytes) -> str:
    return re.sub(r"\x1b\[[0-9;?]*[a-zA-Z]|\x1b\][^\x07]*\x07|\x1b[=>]|\r",
                  "", data.decode("utf-8", "replace"))


def stop_count() -> int:
    # Hook log layout is `timestamp|payload` — match the payload substring
    # (a field-count check written for a 4-field layout counts zero here).
    try:
        with open(log_path) as fh:
            return sum(1 for line in fh if '"hook_event_name":"Stop"' in line)
    except FileNotFoundError:
        return 0


def spawn():
    env = dict(os.environ)
    for key in SCRUBBED:
        env.pop(key, None)
    env.update(FORCED)
    env["HOME"] = sandbox_home
    env.setdefault("TERM", "xterm-256color")
    pid, fd = pty.fork()
    if pid == 0:
        os.chdir(proj)
        os.execvpe(claude_bin,
                   [claude_bin, "--max-turns=6", "--allowedTools", "Bash"], env)
    fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", 40, 120, 0, 0))
    return pid, fd


def drain_until_quiet(fd, deadline):
    seen = ""
    while True:
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            return seen
        ready, _, _ = select.select([fd], [], [], min(QUIET_SECS, remaining))
        if not ready:
            return seen
        try:
            chunk = os.read(fd, 65536)
        except OSError:
            return seen
        if not chunk:
            return seen
        seen += strip_ansi(chunk)
        if len(seen) > 200_000:
            seen = seen[-100_000:]


def inject(fd, prompt):
    os.write(fd, (PASTE_ON + prompt + PASTE_OFF).encode())
    time.sleep(0.3)
    os.write(fd, b"\r")


def wait_for_stops(fd, baseline, want, deadline):
    while time.monotonic() < deadline and stop_count() < baseline + want:
        time.sleep(0.5)
    seen = stop_count()
    window_end = time.monotonic() + POST_STOP_WATCH
    while time.monotonic() < window_end:
        time.sleep(0.5)
        now = stop_count()
        if now > seen:
            seen = now
            window_end = time.monotonic() + POST_STOP_WATCH
    return seen


def terminate(pid, fd):
    try:
        os.write(fd, b"\x03")
        time.sleep(1.5)
        os.write(fd, b"\x03")
        time.sleep(1.5)
    except OSError:
        pass
    try:
        os.kill(pid, signal.SIGKILL)
    except ProcessLookupError:
        pass
    os.close(fd)
    os.waitpid(pid, 0)


pid, fd = spawn()
result = {"prompts": []}
try:
    startup = drain_until_quiet(fd, time.monotonic() + STARTUP_TIMEOUT)
    if "trust" in startup.lower():
        print("TRUST DIALOG APPEARED — pre-seed failed")
        print(startup[-1200:])
        sys.exit(2)

    inject(fd, PROMPT_1)
    t0 = time.monotonic()
    after1 = wait_for_stops(fd, BASE, 1, time.monotonic() + TURN_TIMEOUT)
    result["prompts"].append({"prompt": 1, "kind": "multi-round tool use (Bash allowed)",
                              "stop_firings": after1 - BASE,
                              "seconds": round(time.monotonic() - t0, 1)})

    baseline2 = stop_count()
    inject(fd, PROMPT_2)
    t0 = time.monotonic()
    after2 = wait_for_stops(fd, baseline2, 1, time.monotonic() + TURN_TIMEOUT)
    result["prompts"].append({"prompt": 2, "kind": "plain reply",
                              "stop_firings": after2 - baseline2,
                              "seconds": round(time.monotonic() - t0, 1)})
    result["total_stop_firings"] = stop_count() - BASE
except BaseException as exc:
    result["error"] = repr(exc)
finally:
    terminate(pid, fd)

print(json.dumps(result, indent=2))
EOF

echo
echo "===== per-firing payload summary (whole run)"
summarize "$BASE"

# Last line: the version-straddle bracket closes here — a version change since
# begin aborts the run as failed (exit 1) so its evidence cannot be pinned.
probe_version_guard_end
