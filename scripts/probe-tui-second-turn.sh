#!/usr/bin/env bash
# probe-tui-second-turn.sh — decisive probe for the second-turn Stop question.
#
# Two earlier TUI runs both showed zero Stop firings for a second injected
# prompt. Unresolved: did the second turn actually run (injection failure?)
# or did it run WITHOUT firing Stop (contract divergence)? This probe watches
# BOTH the firing log and the TUI screen text, so the outcome is
# distinguishable:
#   reply rendered + Stop fired   -> once-per-turn confirmed
#   reply rendered + no Stop      -> Stop does NOT fire on later turns
#   no reply                      -> injection/TUI-state problem, not a
#                                    Stop-contract finding
#
# Same isolation model as the other probes: sandbox HOME, scrubbed env,
# project-source Stop hook, payloads reduced to safe fields.
#
# Version sensitivity: results are pinned to the claude binary on PATH at run
# time, and since 2026-09-26 every run is version-guarded
# (scripts/probe-version-guard.sh, sourced below): the binary is resolved and
# pinned once, and the run aborts as failed unless its version held to the
# end — a mid-run auto-update can no longer straddle a measurement
# (docs/notes/claude-contract-probes.md §Version guard).

set -u

PROBE_ROOT="$(mktemp -d /tmp/ccprobe-tui2-XXXXXX)"
PROJ="$PROBE_ROOT/proj"
LOG="$PROBE_ROOT/firings.log"
SANDBOX_HOME="$PROBE_ROOT/home"

mkdir -p "$PROJ/.claude" "$SANDBOX_HOME/.claude"
cleanup() { rm -rf "$PROBE_ROOT"; }
trap cleanup EXIT

python3 - "$SANDBOX_HOME" "$PROJ" <<'EOF'
import json, os, sys
home, proj = sys.argv[1], sys.argv[2]
data = {"hasCompletedOnboarding": True, "theme": "dark",
        "projects": {proj: {"hasTrustDialogAccepted": True,
                             "hasCompletedProjectOnboarding": True}}}
with open(os.path.join(home, ".claude.json"), "w") as fh:
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

# Version-straddle guard: begin pins CLAUDE_BIN (resolved once) and stamps the
# start version; probe_version_guard_end, as the script's last line, aborts
# the run as failed if that binary's version moved mid-run.
source "$(dirname "$0")/probe-version-guard.sh"
probe_version_guard_begin

python3 - "$CLAUDE_BIN" "$PROJ" "$SANDBOX_HOME" "$LOG" <<'EOF'
import fcntl, json, os, pty, re, select, signal, struct, sys, termios, time

claude_bin, proj, sandbox_home, log_path = sys.argv[1:5]

SCRUBBED = ["CLAUDE_CODE_SESSION_ID", "CLAUDECODE", "CLAUDE_CODE_CHILD_SESSION",
            "CLAUDE_CODE_SKIP_PROMPT_HISTORY"]
FORCED = {"CLAUDE_CODE_ENTRYPOINT": "cli", "CLAUDE_CODE_FORCE_SESSION_PERSISTENCE": "1"}
PASTE_ON, PASTE_OFF = "\x1b[200~", "\x1b[201~"
PROMPT_1 = ("Reply with exactly: FIRST-OK")
PROMPT_2 = ("Reply with exactly: SECOND-OK")
QUIET_SECS = 5.0
STARTUP_TIMEOUT = 120.0
TURN_TIMEOUT = 150.0


def strip_ansi(data: bytes) -> str:
    return re.sub(r"\x1b\[[0-9;?]*[a-zA-Z]|\x1b\][^\x07]*\x07|\x1b[=>]|\r",
                  "", data.decode("utf-8", "replace"))


def stop_count() -> int:
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
        os.execvpe(claude_bin, [claude_bin], env)
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


def wait_turn(fd, baseline_stops, want_text, deadline):
    """Wait until the firing log gains a Stop AND the screen shows want_text;
    return (stops_seen, screen_text, which_condition)."""
    text = ""
    while time.monotonic() < deadline:
        ready, _, _ = select.select([fd], [], [], 1.0)
        if ready:
            try:
                chunk = os.read(fd, 65536)
                text += strip_ansi(chunk)
            except OSError:
                pass
        if len(text) > 300_000:
            text = text[-150_000:]
        stops = stop_count()
        if stops >= baseline_stops + 1 and want_text in text:
            return stops, text, "stop+text"
        time.sleep(0.3)
    return stop_count(), text, "timeout"


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
result = {}
try:
    startup = drain_until_quiet(fd, time.monotonic() + STARTUP_TIMEOUT)
    if "trust" in startup.lower():
        print("TRUST DIALOG APPEARED — pre-seed failed")
        print(startup[-1000:])
        sys.exit(2)

    # Turn 1 — plain reply.
    base0 = stop_count()
    inject(fd, PROMPT_1)
    t0 = time.monotonic()
    stops1, text1, how1 = wait_turn(fd, base0, "FIRST-OK", time.monotonic() + TURN_TIMEOUT)
    result["turn1"] = {"stop_firings": stops1 - base0, "reply_rendered": "FIRST-OK" in text1,
                       "how": how1, "seconds": round(time.monotonic() - t0, 1)}

    # Turn 2 — plain reply, same session. The decisive measurement.
    base1 = stop_count()
    inject(fd, PROMPT_2)
    t0 = time.monotonic()
    stops2, text2, how2 = wait_turn(fd, base1, "SECOND-OK", time.monotonic() + TURN_TIMEOUT)
    result["turn2"] = {"stop_firings": stops2 - base1, "reply_rendered": "SECOND-OK" in text2,
                       "how": how2, "seconds": round(time.monotonic() - t0, 1)}
    result["verdict"] = (
        "once-per-turn confirmed" if result["turn2"]["reply_rendered"] and result["turn2"]["stop_firings"] >= 1
        else "reply rendered but NO Stop fired on second turn" if result["turn2"]["reply_rendered"]
        else "second turn never ran (injection/TUI-state problem)"
    )
except BaseException as exc:
    result["error"] = repr(exc)
finally:
    try:
        terminate(pid, fd)
    except Exception:
        pass

# Keep a bounded screen tail from turn 2 for diagnosis (TUI rendering only —
# the model's reply to a fixed sentinel prompt, no user data).
tail = text2[-1200:] if "text2" in dir() else ""
result["turn2_screen_tail"] = tail
print(json.dumps(result, indent=2))
EOF
echo
echo "===== firing log (ts|event only)"
cut -d'|' -f1 "$LOG" 2>/dev/null | paste -d' ' - <(grep -o '"hook_event_name":"[A-Za-z]*"' "$LOG" 2>/dev/null) || true

# Last line: the version-straddle bracket closes here — a version change since
# begin aborts the run as failed (exit 1) so its evidence cannot be pinned.
probe_version_guard_end
