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

The startup sequencer has a fallback timeout (0.4 s idle after ≥ 200 bytes received — `IDLE_THRESHOLD_BYTES` / `IDLE_TIMEOUT_MS` in `src/startup.rs`; reduced from the original 0.8 s on 2026-08-15 by the adaptive-backoff change) to cover cases where the terminal doesn't respond to all probes or emits unexpected output.

## Version-Pinned Capture

What the real TUI actually sends is pinned per Claude Code version, the same way `docs/notes/claude-contract-probes.md` pins the hook contracts:

- **Fixture:** `tests/fixtures/terminal_probes_v<version>.json` — a startup capture of the real `claude` TUI (chunks preserved at read boundaries, plus every CSI sequence found in it, recognized or not). Two are pinned: `terminal_probes_v2.1.282.json` (the current pin, 2026-09-24) and `terminal_probes_v2.1.270.json` (2026-09-14), deliberately **retained as the compatibility baseline** — `tests/terminal.rs::retained_prior_capture_probe_inventory_matches_the_pinned_shape` asserts the recognized probe inventory is identical across the two, so a future re-pin that changes the real-traffic shape cannot land as a mere version bump.
- **Capture harness:** `scripts/probe-tui-terminal-probes.py <claude_bin> <out.json> --answer`. `--answer` makes the driver reply via a port of the `src/terminal.rs` responder, which is the production shape (unanswered, Ink stalls after its first queries and the rest never hit the wire — useful for observing the hang, not for pinning the contract).
- **`tests/terminal.rs`** feeds the recorded chunks (and the same bytes one byte at a time) through the real `TerminalEmu` and asserts the answers are exactly the deduplicated documented responses.

Measured against `claude` 2.1.282 (2026-09-24, claudepr-8600fc27; sandboxed HOME, trust pre-seeded, `TERM=xterm-256color`, winsize 220×50): the startup render burst sends **XTVERSION** (`ESC[>0q`) once and **DA1** (`ESC[c`) **twice** — the retry is real-traffic proof the dedup bitmask must suppress — and no DA2/DSR/window-size probe at all. **Comparison against the retained 2.1.270 capture (2026-09-14): the recognized probe inventory is identical — same kinds, same spellings, same capture order (XTVERSION `>0q`, then DA1 `c`, DA1 `c`)** — confirmed across three captures (2.1.270, and two independent 2.1.282 runs the same evening, which differ from each other only in non-probe cursor/SGR render noise). What 2.1.282 adds are two query kinds the table does not list — `ESC[16t` (XTWINOPS cell-size-**in-pixels** query: the sibling of the answered `18t` chars-size probe, deliberately unanswered) and `ESC[?1016$p` (synchronized-output pixel-mode query) — plus the already-documented silent traffic (`ESC[?u`, `ESC[?2026$p`, SGR colors, mode sets). That is the version-resilience path (§Unknown Sequences) firing for real: new queries arrive, the responder stays silent, nothing hangs. The other three probe kinds remain part of the responder contract (other versions / terminal shapes) and are pinned by the table-driven tests.

Re-run the capture script after any Claude Code update; if a probe the table does not list appears in the new capture, treat it like a moved contract in `claude-contract-probes.md` §Maintenance: re-measure, update table + fixture + tests in one change, and file a follow-up bead if any downstream design depends on it.

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
