#!/usr/bin/env python3
"""probe-tui-terminal-probes.py — capture the DEC probe bytes the real Claude
Code TUI writes at startup, and record them as a version-pinned fixture.

Companion to docs/notes/terminal-probes.md and tests/terminal.rs: the fixture
this script writes (tests/fixtures/terminal_probes_v<version>.json) pins the
probe traffic claude-print's TerminalEmu must answer, exactly like
tests/fixtures/claude_contracts_v2.1.282.json pins the hook contracts. Re-run
it after any Claude Code update; if the TUI starts emitting a probe the doc's
table does not list, the fixture test fails loudly and forces a re-measure.

Isolation (same contract as scripts/probe-tui-stop.py): HOME is redirected
into a throwaway sandbox whose .claude.json pre-seeds trust for the probe cwd
only; CLAUDECODE* session markers are scrubbed and the claude-print child
environment contract (src/pty.rs) is forced. The host's real ~/.claude is
never read or written; auth travels by inherited environment only.

By default the driver never answers the probes — it only listens, which
captures the hang shape: Ink stalls after the first one or two queries and
the rest of the probe set never hits the wire. Pass `--answer` to reply via
the built-in port of src/terminal.rs's responder, exactly as claude-print
does; that is the production shape and the one to pin as the fixture.

Usage:
  probe-tui-terminal-probes.py <claude_bin> <output-fixture.json> [--answer] [--untrusted]
"""

import fcntl
import json
import os
import pty
import select
import signal
import struct
import subprocess
import sys
import tempfile
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

# The TUI probes land in the first render burst, well inside this window.
CAPTURE_SECS = 6.0
# Stop early once the burst has gone quiet this long.
QUIET_SECS = 2.5
# Fixture size cap: the welcome-banner render is a few KB; anything past this
# is trailing repaint noise no probe lives in.
MAX_CAPTURE_BYTES = 24 * 1024
# claude-print's stty fallback dimensions (docs/notes/terminal-probes.md).
ROWS, COLS = 50, 220

# Probe table from docs/notes/terminal-probes.md: params → (name, response).
PROBE_TABLE = {
    b"c": ("DA1", b"\x1b[?6c"),
    b"0c": ("DA1", b"\x1b[?6c"),
    b">c": ("DA2", b"\x1b[>0;0;0c"),
    b">0c": ("DA2", b"\x1b[>0;0;0c"),
    b"6n": ("DSR", b"\x1b[1;1R"),
    b">q": ("XTVERSION", b"\x1bP>|claude-print\x1b\\"),
    b">0q": ("XTVERSION", b"\x1bP>|claude-print\x1b\\"),
    b"18t": ("WinSize", b"\x1b[8;%d;%dt" % (ROWS, COLS)),
}


def esc(data: bytes) -> str:
    """Escape bytes to a printable ASCII form that round-trips through JSON."""
    out = []
    for b in data:
        if 0x20 <= b < 0x7F and b not in (0x5C, 0x22):
            out.append(chr(b))
        else:
            out.append("\\x%02x" % b)
    return "".join(out)


def unesc(text: str) -> bytes:
    """Inverse of esc() — used by tests via serde, and here for self-checks."""
    out = bytearray()
    i = 0
    while i < len(text):
        if text[i] == "\\" and i + 1 < len(text) and text[i + 1] == "x":
            out.append(int(text[i + 2 : i + 4], 16))
            i += 4
        else:
            out.append(ord(text[i]))
            i += 1
    return bytes(out)


def scan_csi(data: bytes):
    """Scan raw bytes for complete CSI sequences (ESC [ params final), the way
    src/terminal.rs does, and classify each against the probe table. Returns a
    list of {offset, params, complete, name} — unknown finals included, so the
    fixture also pins which real TUI sequences must produce silence."""
    found = []
    i = 0
    n = len(data)
    while i < n - 1:
        if data[i] != 0x1B or data[i + 1] != 0x5B:  # ESC [
            i += 1
            continue
        start = i
        j = i + 2
        while j < n and 0x20 <= data[j] <= 0x3F:
            j += 1
        if j >= n:
            break  # truncated tail — no complete sequence past here
        final = data[j]
        if 0x40 <= final <= 0x7E:
            params = bytes(data[i + 2 : j])
            # Table keys are the full params+final form ("c", ">0q", "18t").
            name = PROBE_TABLE.get(params + bytes([final]), (None, None))[0]
            found.append(
                {
                    "offset": start,
                    "params": esc(params) if params else "",
                    "final": chr(final),
                    "probe": name,
                }
            )
            i = j + 1
        else:
            i = j
    return found


class TerminalEmu:
    """Faithful port of src/terminal.rs's responder state machine, used by
    --answer to reply to the TUI exactly as claude-print does, so the capture
    shows the startup traffic claude-print actually sees."""

    MAX_PROBE_LEN = 32

    def __init__(self, rows: int, cols: int):
        self.rows, self.cols = rows, cols
        self.partial = bytearray()
        self.answered = set()

    def _state(self):
        buf = self.partial
        if not buf or buf[0] != 0x1B:
            return "invalid"
        if len(buf) == 1:
            return "incomplete"
        if buf[1] != 0x5B:  # '['
            return "invalid"
        if len(buf) == 2:
            return "incomplete"
        last = buf[-1]
        if 0x40 <= last <= 0x7E:
            return "complete"
        if 0x20 <= last <= 0x3F:
            return "incomplete"
        return "invalid"

    def feed(self, chunk: bytes) -> bytes:
        out = bytearray()
        for byte in chunk:
            if not self.partial:
                if byte == 0x1B:
                    self.partial.append(byte)
                continue
            self.partial.append(byte)
            if len(self.partial) > self.MAX_PROBE_LEN:
                self.partial.clear()
                continue
            state = self._state()
            if state == "complete":
                resp = self._respond()
                if resp:
                    out += resp
                self.partial.clear()
            elif state == "invalid":
                last = self.partial[-1]
                self.partial.clear()
                if last == 0x1B:
                    self.partial.append(0x1B)
        return bytes(out)

    def _respond(self):
        params = bytes(self.partial[2:])
        kind = {
            b"c": "DA1", b"0c": "DA1",
            b">c": "DA2", b">0c": "DA2",
            b"6n": "DSR",
            b">q": "XTVERSION", b">0q": "XTVERSION",
            b"18t": "WinSize",
        }.get(params)
        if kind is None or kind in self.answered:
            return None
        self.answered.add(kind)
        return {
            "DA1": b"\x1b[?6c",
            "DA2": b"\x1b[>0;0;0c",
            "DSR": b"\x1b[1;1R",
            "XTVERSION": b"\x1bP>|claude-print\x1b\\",
            "WinSize": b"\x1b[8;%d;%dt" % (self.rows, self.cols),
        }[kind]


def preseed_trust(sandbox_home: str, proj: str) -> None:
    path = os.path.join(sandbox_home, ".claude.json")
    try:
        with open(path) as fh:
            data = json.load(fh)
    except (FileNotFoundError, json.JSONDecodeError):
        data = {}
    data.setdefault("hasCompletedOnboarding", True)
    data.setdefault("theme", "dark")
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


def capture(claude_bin: str, untrusted: bool = False, answer: bool = False):
    emu = TerminalEmu(ROWS, COLS) if answer else None
    sandbox = tempfile.mkdtemp(prefix="claude-print-probe-caps.")
    home = os.path.join(sandbox, "home")
    proj = os.path.join(sandbox, "proj")
    os.makedirs(home)
    os.makedirs(proj)
    if not untrusted:
        preseed_trust(home, proj)

    env = dict(os.environ)
    for key in SCRUBBED:
        env.pop(key, None)
    env.update(FORCED)
    env["HOME"] = home
    env.setdefault("TERM", "xterm-256color")

    pid, fd = pty.fork()
    if pid == 0:  # child
        os.chdir(proj)
        os.execvpe(claude_bin, [claude_bin], env)

    # claude-print's stty fallback dimensions, so the capture matches the
    # window the responder documents.
    fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", ROWS, COLS, 0, 0))

    chunks = []
    total = 0
    deadline = time.monotonic() + CAPTURE_SECS
    last_output = None
    try:
        while time.monotonic() < deadline and total < MAX_CAPTURE_BYTES:
            wait = deadline - time.monotonic()
            if last_output is not None:
                wait = min(wait, QUIET_SECS - (time.monotonic() - last_output))
            if wait <= 0:
                break
            ready, _, _ = select.select([fd], [], [], wait)
            if not ready:
                if last_output is not None:
                    break  # burst went quiet; probes are all in by now
                continue
            try:
                chunk = os.read(fd, 65536)
            except OSError:
                # EIO: the child exited and closed its side of the pty. Keep
                # whatever the capture already holds instead of tracebacking.
                break
            if not chunk:
                break
            chunks.append(chunk)
            total += len(chunk)
            last_output = time.monotonic()
            if emu is not None:
                resp = emu.feed(chunk)
                if resp:
                    os.write(fd, resp)
    finally:
        try:
            os.kill(pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        os.close(fd)
        os.waitpid(pid, 0)

    import shutil

    shutil.rmtree(sandbox, ignore_errors=True)
    return chunks, env.get("TERM")


def main() -> int:
    claude_bin, out_path = sys.argv[1], sys.argv[2]
    flags = set(sys.argv[3:])
    untrusted = "--untrusted" in flags
    answer = "--answer" in flags

    # Normalize to the bare version number ("2.1.282"), matching the
    # claude_version field style of tests/fixtures/claude_contracts_v2.1.282.json
    # and the v<version> filename stamp.
    version = subprocess.run(
        [claude_bin, "--version"], capture_output=True, text=True, timeout=30
    ).stdout.strip().split()[0]

    chunks, term = capture(claude_bin, untrusted, answer)
    raw = b"".join(chunks)
    sequences = scan_csi(raw)
    probes = [s for s in sequences if s["probe"]]

    fixture = {
        "claude_version": version,
        "measured_at": time.strftime("%Y-%m-%d"),
        "capture_script": "scripts/probe-tui-terminal-probes.py",
        "evidence": "docs/notes/terminal-probes.md",
        "environment": {
            "term": term,
            "rows": ROWS,
            "cols": COLS,
            "winsize_source": "driver TIOCSWINSZ set to claude-print's stty fallback",
        },
        "isolation": "HOME redirected into a throwaway sandbox; CLAUDECODE* scrubbed; auth by inherited environment. "
        + ("Driver answered probes via the src/terminal.rs responder port (--answer)." if answer else "Probes were listened to, never answered.")
        + (" Trust NOT pre-seeded — captures the untrusted-cwd dialog path claude-print drives." if untrusted else " Trust pre-seeded for the probe cwd."),
        "chunk_count": len(chunks),
        "capture_bytes": len(raw),
        "truncated": len(raw) >= MAX_CAPTURE_BYTES,
        # Read-boundary-preserving chunks, escaped. tests/terminal.rs feeds
        # these to TerminalEmu both as-recorded and byte-by-byte.
        "chunks": [esc(c) for c in chunks],
        # Every complete CSI sequence in the capture, known or not.
        "csi_sequences": sequences,
        # The subset the probe table recognizes, in capture order.
        "probe_inventory": [
            {"name": s["probe"], "params": s["params"], "offset": s["offset"]}
            for s in probes
        ],
    }
    with open(out_path, "w") as fh:
        json.dump(fixture, fh, indent=2)
        fh.write("\n")

    print(f"claude: {version}")
    print(f"captured {len(raw)} bytes in {len(chunks)} chunks from TERM={term}")
    print("CSI sequences seen:")
    for s in sequences:
        label = s["probe"] or "unknown"
        print(f"  offset {s['offset']:>6}  ESC[ {s['params']!r} {s['final']}  → {label}")
    print(f"probes recognized: {len(probes)}")
    if not probes:
        print("WARNING: no recognized probes in capture — investigate before pinning")
        return 1
    print(f"fixture written: {out_path}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
