# bf-30e — Add stream-json reader thread spawn to session flow

## Status: COMPLETE (umbrella bead — sub-beads already closed)

## Summary

The stream-json reader thread spawn is wired into the session flow at the
`PROMPT_INJECTED` transition. The implementation was landed across the
closed sub-beads (bf-549b, bf-5k9t, bf-5t5n, bf-3wya, bf-3p8h) and the
join-on-all-exit-paths dependency (bf-68nl). This note records the
verification pass that closed the umbrella.

## Acceptance criteria — verification

| Criterion | Where | Status |
|-----------|-------|--------|
| Reader thread spawned at PROMPT_INJECTED transition | `src/session.rs` (~L360-375) — guarded by `output_format == StreamJson` AND a `last_phase != PromptInjected` phase-change guard, so it fires exactly once | ✅ |
| Byte offset captured from transcript file length at bracketed-paste write | `src/session.rs` — `std::fs::metadata(&transcript_path).map(\|m\| m.len()).unwrap_or(0)` is read in the same event-loop tick as the transition, *between* `poll_timers()` producing the bracketed-paste `Write` action and the actual `libc::write()` to the PTY master. The offset therefore reflects the pre-injection transcript length. | ✅ |
| Retry logic (50 ms / 5 s) for transcript-not-yet-created | `src/emitter.rs` `stream_json_reader_loop` (~L127-149) — opens the file in a loop with `thread::sleep(50ms)`, 5 s deadline; on drain signal or disconnect it exits, on timeout it returns (main thread then emits error) | ✅ |
| Reader thread drains to mpsc channel | `src/emitter.rs` — `mpsc::sync_channel(1)`; success path sends `()` (drain-then-exit), exit paths drop the sender (exit immediately). Joined on every exit path in `src/session.rs`. | ✅ |

Join coverage (INV-8): success (`FifoPayload`), no-transcript-path error,
timeout, child-exit-before-Stop, and interrupted paths all drain/drop +
`join_handle.join()` before returning.

## Tests run (all green)

- `cargo test --lib` — 90 passed (incl. `session`, `pty`, `event_loop` unit tests)
- `cargo test --test emitter stream_json` — 2 passed
  (`test_stream_json_each_line_parses_as_json`,
  `test_stream_json_disconnect_exits_immediately`)
- `cargo test --test integration stream_json` — 4 passed, including
  `stream_json_start_offset_skips_pre_injection_lines` (validates the
  captured byte offset skips pre-injection lines — directly exercises
  this bead's offset contract via `spawn_stream_json_reader_to`)
- `cargo test --test pty_integration` / `--test watchdog` — pass

## Known out-of-scope gap (tracked separately)

The reader is spawned against `transcript_path = <temp_dir>/transcript.jsonl`.
The real Claude Code transcript lives at
`~/.claude/projects/<cwd-slug>/<session-id>.jsonl`, whose exact filename
requires the `session_id` — not known until the Stop payload. Resolving
the real path for *live* tailing is the explicitly separate open bead:

- **bf-5vm** (P1, open) — "Wire live stream-json streaming into session
  flow (currently buffered replay)"
- **bf-3isy** (P2, open) — "mock_claude never writes a transcript JSONL"

bf-30e is the *basic invocation* (spawn plumbing, offset capture, retry,
mpsc drain, joins); the path-resolution work is bf-5vm's domain.

## Changes in this commit

- `src/session.rs`, `src/event_loop.rs`, `src/pty.rs`: test portability —
  resolve `bash`/`true` via PATH (`which::which`) instead of hardcoded
  `/bin/bash` / `/bin/true`, which are absent on non-FHS systems (NixOS).
  Needed for the DoD `cargo test` gate to run on such hosts.
