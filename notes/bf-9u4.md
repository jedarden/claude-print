# bf-9u4: Bracketed Paste Injection

## Status: Already Implemented (verified)

Bracketed paste injection was completed as part of bf-54m (idle-gap timing). No additional code was required.

## Implementation

`startup.rs:188-199` — `StartupSeq::make_prompt_payload()` wraps the startup prompt in the correct escape sequences:

- `\x1b[200~` (ESC[200~) — bracketed paste open
- prompt bytes
- `\x1b[201~\r` (ESC[201~) — bracketed paste close + CR to submit

Injection fires from `poll_timers()` in the `TrustDismissed` phase after `idle_gap_ms` of uninterrupted PTY silence, satisfying the ordering requirement from bf-54m.

## Tests Verified

All 41 tests pass (30 unit + 11 integration):

- `startup::tests::make_prompt_payload_wraps_in_bracketed_paste` — output bytes contain `\x1b[200~` and `\x1b[201~\r`
- `startup::tests::idle_gap_fires_after_silence` — full payload verified (open, prompt, close+CR)
- `startup::tests::idle_gap_resets_on_new_output` — injection deferred until PTY goes silent
- `tests/startup.rs::test_trust_dialog_prompt_payload_uses_bracketed_paste` — integration-level check
