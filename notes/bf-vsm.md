# bf-vsm: Phase 5 — Startup Sequencer

## Status: Complete

Phase 5 (Startup Sequencer) was implemented across four child beads. This bead closes
the phase as a whole after verifying all completion criteria are met.

## Implementation (src/startup.rs)

All four components required by Phase 5 are present and tested:

**1. Keyword trust dismiss** (bf-38q)
- `StartupSeq::scan_line()` counts occurrences of trust-dialog keywords (threshold: ≥ 2)
- Keywords: `trust`, `Allow`, `continue`, `folder`, `permission`, `proceed`
- Case-sensitive matching to avoid false positives (e.g. `allow` ≠ `Allow`)
- `feed()` scans PTY output line-by-line, accumulating partial lines across chunks

**2. Idle-gap timing** (bf-54m)
- `poll_timers()` handles three deadline-driven transitions:
  - Hard timeout: WAITING + < 200 bytes after 45 s → `HardTimeout`
  - Idle fallback: WAITING + ≥ 200 bytes + 0.8 s idle → dismiss CR
  - Post-dismiss idle gap: TRUST_DISMISSED + no output for `idle_gap_ms` → injection
- `last_output_at` resets on every `feed()` call, so TUI redraws after dismiss
  push the injection window forward until the terminal goes silent

**3. Bracketed paste injection** (bf-9u4)
- `make_prompt_payload()` wraps prompt in `\x1b[200~` … `\x1b[201~\r`
- Fires from `poll_timers()` in TrustDismissed after idle gap expires
- Phase transitions: Waiting → TrustDismissed → PromptInjected (one-shot)

**4. Large-prompt file relay** (bf-1cx)
- Threshold: `INLINE_PROMPT_MAX = 32 KB`
- Below threshold: inline bracketed paste with prompt bytes
- Above threshold: `make_file_relay_payload()` writes prompt to `NamedTempFile`,
  injects `$(< /path/to/file)` via bracketed paste; relay_file held alive in struct
  until session ends

## Completion Criteria (all met)

- **Startup unit tests**: 20 tests in `src/startup.rs` — all pass
- **test_trust_dialog_* integration tests**: 11 tests in `tests/startup.rs` — all pass
- Total test suite: 55 tests, 0 failures
