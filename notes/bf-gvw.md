# Phase 4: Terminal Emulator — bf-gvw

## Status: Complete

All 9 terminal unit tests pass.

## Implementation

`src/terminal.rs` implements `TerminalEmu` — a stateful probe scanner that:

- Accumulates partial CSI sequences across `feed()` calls using a `partial: Vec<u8>` buffer
- Detects and responds to the 5 DEC probes Ink sends at startup:
  - **DA1** (`ESC[c` / `ESC[0c`) → `ESC[?6c`
  - **DA2** (`ESC[>c` / `ESC[>0c`) → `ESC[>0;0;0c`
  - **DSR** (`ESC[6n`) → `ESC[1;1R`
  - **XTVERSION** (`ESC[>q` / `ESC[>0q`) → `ESC P>|claude-print ESC \`
  - **WinSize** (`ESC[18t`) → `ESC[8;<rows>;<cols>t`
- Uses a dedup bitmask (`answered: u8`) to answer each probe type at most once per session
- Passes through unknown probes silently with no response and no panic
- Limits partial buffer to `MAX_PROBE_LEN = 32` bytes to prevent unbounded growth

## Tests Verified

```
test da1_responds_with_csi_6c ... ok
test da2_responds_with_secondary_attrs ... ok
test dsr_responds_with_cursor_pos ... ok
test xtversion_responds_with_dcs_string ... ok
test window_size_responds_with_configured_dimensions ... ok
test multiple_probes_in_one_chunk_answered_in_order ... ok
test probe_dedup_da1_answered_only_once ... ok
test unknown_probe_ignored_no_response_no_panic ... ok
test split_chunk_probe_answered_on_second_read ... ok
```
