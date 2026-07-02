# Terminal Probes

Claude Code's TUI (built on Ink, a React/Yoga-based framework) sends DEC terminal queries at startup and hangs indefinitely if unanswered. The terminal emulator in `claude-print` scans PTY output for these probes and responds automatically.

## Probe Table

| Probe Bytes | Response Bytes | Name | Notes |
|-------------|---------------|------|-------|
| `ESC [ c` or `ESC [ 0 c` | `ESC [ ? 6 c` | DA1 (Device Attributes) | Primary terminal type query |
| `ESC [ > c` or `ESC [ > 0 c` | `ESC [ > 0 ; 0 ; 0 c` | DA2 (Device Attributes 2) | Secondary terminal type query |
| `ESC [ 6 n` | `ESC [ 1 ; 1 R` | DSR (Device Status Report) | Cursor position report |
| `ESC [ > q` or `ESC [ > 0 q` | `ESC P >\| claude-print ESC \` | XTVERSION (Terminal Identification) | DCS string with ST terminator |
| `ESC [ 1 8 t` | `ESC [ 8 ; <rows> ; <cols> t` | Window Size | Responds with configured dimensions |

### Response Details

- **DA1**: `ESC [ ? 6 c` — Indicates "VT102" compatibility level
- **DA2**: `ESC [ > 0 ; 0 ; 0 c` — Format: `> <version> ; <options> ; <rom-version>c`
- **DSR**: `ESC [ 1 ; 1 R` — Cursor at row 1, column 1
- **XTVERSION**: `ESC P >\| claude-print ESC \` — DCS string with identifier and ST (String Terminator = ESC + backslash)
  - Note: The final two bytes are ESC (`0x1B`) + backslash (`0x5C`), not a backtick
- **Window Size**: `ESC [ 8 ; <rows> ; <cols> t` — Configured dimensions (default 220×50 from stty fallback)

## Implementation

The probe responder (`src/terminal.rs`) uses a byte-by-byte state machine to handle probes that may be split across chunk boundaries:

### State Machine

```
Empty → ESC received
Partial → accumulating CSI sequence
Complete → CSI sequence complete
Invalid → not a recognized probe
```

### CSI Format

A CSI (Control Sequence Introducer) sequence has the structure:

```
ESC [ <params> <final-byte>
```

- `<params>`: intermediate/parameter bytes in range `0x20-0x3F`
- `<final-byte>`: terminator in range `0x40-0x7E`

### Matching Logic

Probe identification:

```rust
// params = everything after ESC [ up to the final byte
if params == b"c" || params == b"0c" → DA1
else if params == b">c" || params == b">0c" → DA2
else if params == b"6n" → DSR
else if params == b">q" || params == b">0q" → XTVersion
else if params == b"18t" → WinSize
else → Unknown probe (silently ignored)
```

## Deduplication

Each probe type is answered at most once per session using a bitmask:

- Bit 0: DA1
- Bit 1: DA2
- Bit 2: DSR
- Bit 3: XTVersion
- Bit 4: WinSize

If the same probe is received again, no response is emitted.

## Unknown Sequences

Unknown escape sequences are **silently ignored** — they are never treated as an error. This ensures version-resilience: if Ink adds new probe types in future versions, `claude-print` will not hang; it simply won't respond to the unrecognized probes.

The startup sequencer has a fallback timeout (0.8 s idle after ≥ 200 bytes received) to cover cases where the terminal doesn't respond to all probes or emits unexpected output.

## Version Resilience

The probe responder is designed to survive Claude Code version changes:

1. **Unknown probes ignored**: New probe types won't crash the binary
2. **Split-chunk handling**: Probe bytes straddling chunk boundaries are correctly assembled
3. **Length cap**: Sequences exceeding `MAX_PROBE_LEN` (32 bytes) are discarded as invalid
4. **Lenient matching**: Both bare (`c`) and parameterized (`0c`) forms are recognized where applicable

## Window Size Fallback

Window size is probed in order:

1. `TIOCGWINSZ` on `STDOUT_FILENO`
2. `TIOCGWINSZ` on `STDIN_FILENO`
3. Open `/dev/tty` and `TIOCGWINSZ`
4. Fallback: `220 × 50`

In headless/NEEDLE mode, steps 1–3 fail and the fallback is always used.

## Integration with Event Loop

The terminal emulator runs on every chunk of PTY output in the event loop:

```
master_fd POLLIN → read chunk → feed to TerminalEmu → response bytes queued
next writable poll → write response bytes to master_fd
```

This ensures low-latency probe responses — Ink receives answers before its own timeouts.
