# PTY Mechanics

## What a PTY Is

A PTY (pseudoterminal) is a pair of file descriptors that behave like a physical serial terminal from the perspective of a program reading/writing to them. One end is the **master** (held by the controlling process); the other is the **slave** (handed to the subprocess as its stdin/stdout/stderr). The kernel's line discipline sits between them and handles character echo, line buffering, signal delivery (Ctrl-C → SIGINT), and special character processing.

Physical terminal:
```
keyboard → /dev/ttyS0 → shell
shell → /dev/ttyS0 → screen
```

PTY:
```
parent (master_fd) ←→ [kernel line discipline] ←→ slave_fd (child sees as /dev/pts/N)
```

Any program that calls `isatty(STDOUT_FILENO)` on the slave fd gets `true`. This is the core of how `claude-print` preserves `cc_entrypoint=cli` — Claude Code calls `isatty()` at startup to decide whether to enter TUI mode.

## System Calls

### `os.openpty()` / POSIX `posix_openpt()` + `grantpt()` + `unlockpt()`

```python
master_fd, slave_fd = os.openpty()
```

Returns two open file descriptors. On Linux this calls `openpty(3)` from glibc, which internally calls `posix_openpt(O_RDWR | O_NOCTTY)` to allocate a new PTY from the kernel's PTY multiplexer (`/dev/ptmx`), then `grantpt()` + `unlockpt()` to make the slave accessible, then `ptsname()` to get the slave path (e.g. `/dev/pts/7`), then `open()` on that path.

Both descriptors must be closed after `fork()` — master in child, slave in parent.

### `os.fork()` + `os.login_tty()`

```python
pid = os.fork()
if pid == 0:  # child
    os.close(master_fd)
    os.login_tty(slave_fd)   # setsid() + TIOCSCTTY + dup2(slave, 0/1/2) + close(slave)
    os.execvp('claude', args)
else:          # parent
    os.close(slave_fd)
    # event loop on master_fd
```

`os.login_tty(slave_fd)` is a glibc function that does three things in sequence:
1. `setsid()` — detach child from parent's process group, become session leader
2. `ioctl(slave_fd, TIOCSCTTY, 0)` — claim the PTY slave as the controlling terminal for this session
3. `dup2(slave_fd, 0); dup2(slave_fd, 1); dup2(slave_fd, 2)` — replace stdin/stdout/stderr
4. `close(slave_fd)` — close the now-duplicated original fd

After this, everything Claude Code writes to stdout goes through the PTY line discipline to `master_fd` in the parent. Everything the parent writes to `master_fd` appears as stdin to Claude Code.

### Window Size (`TIOCGWINSZ` / `TIOCSWINSZ`)

Ink queries the terminal size to lay out its UI. The PTY slave's window size is a kernel struct, not derived from the terminal itself. Must be set explicitly:

```python
import fcntl, struct, termios

# Read from real terminal (if parent has one)
try:
    ws = fcntl.ioctl(sys.stdout.fileno(), termios.TIOCGWINSZ, b'\x00' * 8)
    rows, cols, xpix, ypix = struct.unpack('HHHH', ws)
except OSError:
    rows, cols, xpix, ypix = 50, 220, 0, 0

# Apply to slave via master
fcntl.ioctl(master_fd, termios.TIOCSWINSZ,
            struct.pack('HHHH', rows, cols, xpix, ypix))
```

If not set, the kernel default is 0×0, which causes Ink to render nothing or crash. Set before `execvp` for best results.

Constants on Linux x86-64: `TIOCGWINSZ = 0x5413`, `TIOCSWINSZ = 0x5414`.

### Non-blocking I/O on `master_fd`

The master fd is blocking by default. The read loop must use `select()` or `poll()` to avoid deadlocking when the child hasn't produced output:

```python
import select, os

def read_master(master_fd, timeout=0.1):
    r, _, _ = select.select([master_fd], [], [], timeout)
    if r:
        try:
            return os.read(master_fd, 4096)
        except OSError:  # EIO = child closed slave
            return b''
    return None
```

`EIO` from `os.read(master_fd)` means the child closed all references to the slave (typically on exit). This is the reliable EOF signal.

### Writing Prompts: Bracketed Paste Mode

The shell (and Ink) enables **bracketed paste mode** (`\x1b[?2004h`) to distinguish pasted text from typed characters. In this mode, embedded newlines in pasted text do not trigger command execution. To send a multi-line prompt without each `\n` being treated as Enter:

```python
BRACKETED_PASTE_START = b'\x1b[200~'
BRACKETED_PASTE_END   = b'\x1b[201~'

def send_prompt(master_fd, text):
    payload = BRACKETED_PASTE_START + text.encode() + BRACKETED_PASTE_END + b'\r'
    os.write(master_fd, payload)
```

Use `\r` (CR) as the final character — not `\n`. Ink's REPL expects CR to submit.

## DEC Terminal Probes (Ink Startup Sequence)

Ink sends terminal capability queries at startup and **hangs indefinitely** if they are not answered. The parent process must respond to each probe read from `master_fd`.

All responses are written back to `master_fd` (i.e., as if typed on a keyboard — they travel from parent → line discipline → Claude Code's stdin).

| Query | Bytes received | Response to send | Purpose |
|-------|----------------|------------------|---------|
| DA1 | `ESC [ c` or `ESC [ 0 c` | `ESC [ ? 6 c` | Device attributes (VT102) |
| DA2 | `ESC [ > c` or `ESC [ > 0 c` | `ESC [ > 0 ; 0 ; 0 c` | Secondary DA |
| DSR | `ESC [ 6 n` | `ESC [ 1 ; 1 R` | Cursor position report |
| XTVERSION | `ESC [ > q` | `ESC P > \| <name> ESC \` | xterm version (DCS string) |
| Window size | `ESC [ 1 8 t` | `ESC [ 8 ; <rows> ; <cols> t` | Terminal size in chars |

Concretely in Python:

```python
PROBE_RESPONSES = {
    b'\x1b[c':    b'\x1b[?6c',
    b'\x1b[0c':   b'\x1b[?6c',
    b'\x1b[>c':   b'\x1b[>0;0;0c',
    b'\x1b[>0c':  b'\x1b[>0;0;0c',
    b'\x1b[6n':   b'\x1b[1;1R',
    b'\x1b[>q':   b'\x1bP>|claude-print\x1b\\',
    b'\x1b[18t':  b'\x1b[8;50;220t',
}
```

Probes appear in raw PTY output interleaved with UI text. They must be detected by scanning received bytes — they can appear mid-chunk. Dedup each probe type (respond once per session).

## Line Discipline and Terminal Modes

The PTY line discipline operates in **canonical mode** by default (line buffering, character echo). Ink switches the slave to **raw mode** using `tcsetattr(TIOCSETA)` at startup. In raw mode:
- Each byte is delivered immediately (no line buffering)
- Characters are NOT echoed back
- No special-character processing (Ctrl-C does not become SIGINT through the discipline)

As the master fd owner, `claude-print` sees the raw byte stream after Ink enables raw mode. This means CR is not translated to LF, escape sequences are passed through as-is, and the bracketed paste bytes arrive unmodified at Claude Code's stdin.

## Cleanup

On any exit path (normal, SIGINT, timeout):

```python
import signal, os

def cleanup(pid, master_fd):
    try:
        os.kill(pid, signal.SIGTERM)
        import time; time.sleep(2)
        try:
            os.waitpid(pid, os.WNOHANG)
        except ChildProcessError:
            pass
        os.kill(pid, signal.SIGKILL)
    except ProcessLookupError:
        pass  # already exited
    try:
        os.close(master_fd)
    except OSError:
        pass
    os.waitpid(pid, 0)  # reap zombie
```

Failure to `waitpid` leaves a zombie. Failure to close `master_fd` keeps the slave's reference count > 0, preventing EIO on any remaining reads.

## Signal Propagation

SIGINT sent to the parent process is NOT automatically forwarded to the PTY child (they are in different process groups). Two options:

1. **Signal handler on parent**: `signal.signal(signal.SIGINT, lambda s,f: os.kill(child_pid, signal.SIGINT))`
2. **Ctty approach**: The child is the session leader of the PTY's session; `SIGINT` sent to the foreground process group of that session (via `tcgetsid` + `killpg`) reaches it.

For `claude-print`, option 1 is simpler and sufficient.

## pyte: Virtual Terminal Emulator (Fallback Scraping)

`pyte` is a pure-Python ANSI/VT100 terminal emulator. Feed it raw PTY bytes and query the resulting screen grid:

```python
import pyte

screen = pyte.Screen(220, 50)
stream = pyte.ByteStream(screen)

# Feed bytes as they arrive from master_fd
stream.feed(raw_bytes)

# Read a screen line
line = ''.join(screen.buffer[row][col].data for col in range(screen.columns)).rstrip()
```

`pyte` handles cursor movement, SGR color sequences, erase operations, and scroll regions — the same sequences Ink uses to build its TUI. This is used as a **last-resort fallback** if the transcript JSONL is empty and the Stop hook payload lacks `last_assistant_message`.

Limitation: `pyte` screen state reflects the latest render, not the full scrollback. Long assistant responses that scroll past the screen height are truncated. The primary path (JSONL transcript) avoids this limitation entirely.
