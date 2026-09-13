#!/usr/bin/env python3
"""probe-tui-stop.py — drive the real Claude Code TUI under a PTY and count
Stop-hook firings per injected prompt.

This is the T2 arm of scripts/probe-claude-contracts.sh (see that file for
isolation guarantees): claude-print drives the interactive TUI, so the
Stop-per-turn question must be answered in TUI mode, not print mode.

The driver mirrors claude-print's child-environment contract (src/pty.rs):
scrubs CLAUDECODE / CLAUDE_CODE_SESSION_ID / CLAUDE_CODE_CHILD_SESSION /
CLAUDE_CODE_SKIP_PROMPT_HISTORY, forces CLAUDE_CODE_ENTRYPOINT=cli and
CLAUDE_CODE_FORCE_SESSION_PERSISTENCE=1, and runs with HOME redirected into a
throwaway sandbox so the host's real hooks, transcripts, and session state are
never touched. Trust for the probe cwd is pre-seeded in the sandbox's
.claude.json copy so no dialog handling is needed.

Completion detection does not parse TUI rendering: the project Stop hook
appends to the probe log, and the driver watches the log.

Usage:
  probe-tui-stop.py <claude_bin> <proj_dir> <sandbox_home> <hook_marker_path> <log> [extra claude args...]
"""

import fcntl
import json
import os
import pty
import re
import select
import signal
import struct
import subprocess
import sys
import termios
import time

SCRUBBED = [
    "CLAUDE_CODE_SESSION_ID",
    "CLAUDECODE",
    "CLAUDE_CODE_CHILD_SESSION",
    "CLAUDE_CODE_SKIP_PROMPT_HISTORY",
]
FORCED = {
    "CLAUDE_CODE_ENTRYPOINT": "cli",
    "CLAUDE_CODE_FORCE_SESSION_PERSISTENCE": "1",
}

PASTE_ON, PASTE_OFF = "\x1b[200~", "\x1b[201~"

PROMPT_1 = (
    "Use the Bash tool to run 'echo tui-r1'. Only after you see its output, "
    "run 'echo tui-r2' with the Bash tool. Do not run the two commands in "
    "parallel — the second must start only after the first finished. Then "
    "reply TUIDONE."
)
PROMPT_2 = "Reply with exactly: TUI-SECOND-OK"

QUIET_SECS = 6.0        # TUI-output silence that counts as "settled"
STARTUP_TIMEOUT = 120.0
TURN_TIMEOUT = 240.0
# After the first Stop firing, keep watching for this long for ANOTHER firing
# (a per-turn Stop would land here); every new firing restarts the window.
POST_STOP_WATCH = 30.0


def strip_ansi(data: bytes) -> str:
    text = data.decode("utf-8", "replace")
    return re.sub(r"\x1b\[[0-9;?]*[a-zA-Z]|\x1b\][^\x07]*\x07|\x1b[=>]|\r", "", text)


def stop_count(log_path: str) -> int:
    """Count Stop-event log rows. Log format: ts|tag|event|payload — match on
    the exact event field, never a substring scan of the payload."""
    try:
        with open(log_path) as fh:
            return sum(1 for line in fh if len(line.split("|")) > 2 and line.split("|")[2] == "Stop")
    except FileNotFoundError:
        return 0


def preseed_trust(sandbox_home: str, proj: str) -> None:
    path = os.path.join(sandbox_home, ".claude.json")
    try:
        with open(path) as fh:
            data = json.load(fh)
    except (FileNotFoundError, json.JSONDecodeError):
        data = {}
    projects = data.setdefault("projects", {})
    entry = projects.get(proj) or {}
    entry.update(
        {
            "hasTrustDialogAccepted": True,
            "hasCompletedProjectOnboarding": True,
        }
    )
    projects[proj] = entry
    with open(path, "w") as fh:
        json.dump(data, fh)


def spawn(claude_bin: str, proj: str, sandbox_home: str, extra_args):
    env = dict(os.environ)
    for key in SCRUBBED:
        env.pop(key, None)
    env.update(FORCED)
    env["HOME"] = sandbox_home
    env.setdefault("TERM", "xterm-256color")

    pid, fd = pty.fork()
    if pid == 0:  # child
        os.chdir(proj)
        os.execvpe(claude_bin, [claude_bin, *extra_args], env)

    # 40x120 window so the TUI renders its standard layout.
    fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", 40, 120, 0, 0))
    return pid, fd


def drain_until_quiet(fd, deadline: float) -> str:
    """Read the PTY until QUIET_SECS of silence or deadline; return text."""
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


def inject(fd, prompt: str) -> None:
    os.write(fd, (PASTE_ON + prompt + PASTE_OFF).encode())
    time.sleep(0.3)
    os.write(fd, b"\r")


def wait_for_stops(fd, baseline: int, want: int, deadline: float):
    """Wait until `want` additional Stop rows exist past baseline, then keep
    watching for POST_STOP_WATCH seconds for any FURTHER firing — a per-turn
    Stop lands there. Each new firing restarts the watch window, so the count
    only settles once a full window passes with nothing new."""
    while time.monotonic() < deadline and stop_count(LOG) < baseline + want:
        time.sleep(0.5)
    seen = stop_count(LOG)
    window_end = time.monotonic() + POST_STOP_WATCH
    while time.monotonic() < window_end:
        time.sleep(0.5)
        now = stop_count(LOG)
        if now > seen:
            seen = now
            window_end = time.monotonic() + POST_STOP_WATCH
    return seen


def terminate(pid: int, fd: int) -> None:
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


def main() -> int:
    global LOG
    claude_bin, proj, sandbox_home, hook_marker, log_path = sys.argv[1:6]
    extra_args = sys.argv[6:]
    LOG = log_path

    preseed_trust(sandbox_home, proj)
    baseline = stop_count(LOG)

    pid, fd = spawn(claude_bin, proj, sandbox_home, extra_args)
    result = {"prompts": []}
    try:
        startup = drain_until_quiet(fd, time.monotonic() + STARTUP_TIMEOUT)
        if "trust" in startup.lower():
            print("TRUST DIALOG APPEARED — pre-seed failed; TUI text tail follows:")
            print(startup[-1500:])
            return 2

        # Prompt 1: multi-round tool use.
        inject(fd, PROMPT_1)
        t0 = time.monotonic()
        after1 = wait_for_stops(fd, baseline, 1, time.monotonic() + TURN_TIMEOUT)
        result["prompts"].append(
            {
                "prompt": 1,
                "kind": "multi-round tool use",
                "stop_firings": after1 - baseline,
                "seconds": round(time.monotonic() - t0, 1),
            }
        )

        # Prompt 2: plain reply, same session — per-turn evidence.
        baseline2 = stop_count(LOG)
        inject(fd, PROMPT_2)
        t0 = time.monotonic()
        after2 = wait_for_stops(fd, baseline2, 1, time.monotonic() + TURN_TIMEOUT)
        result["prompts"].append(
            {
                "prompt": 2,
                "kind": "plain reply",
                "stop_firings": after2 - baseline2,
                "seconds": round(time.monotonic() - t0, 1),
            }
        )
        result["total_stop_firings"] = stop_count(LOG) - baseline
    except BaseException as exc:  # keep the summary flowing on timeout
        result["error"] = repr(exc)
    finally:
        terminate(pid, fd)

    print(json.dumps(result, indent=2))
    with open(os.path.join(os.path.dirname(log_path), "tui-probe-result.json"), "w") as fh:
        json.dump(result, fh, indent=2)
    return 0


if __name__ == "__main__":
    sys.exit(main())
