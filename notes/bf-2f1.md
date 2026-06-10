# Phase 8: Emitter — Completion Notes (bf-2f1)

## Status

Phase 8 implementation was already present in commit `bfb50da` (Add Phase 8: Emitter).
Two bead runs verified correctness; all 13 tests confirmed passing on each run.

## Verification

All 13 emitter unit tests pass:

- `test_text_correct_string_trailing_newline` — text format emits `{response}\n`
- `test_text_no_extra_whitespace` — no leading/trailing whitespace beyond newline
- `test_json_valid_with_required_fields` — all required fields present in JSON output
- `test_json_claude_version_included` — `claude_version` field emitted
- `test_json_usage_fields_are_integers` — usage token counts are integers not strings
- `test_error_result_is_error_true_and_subtype` — error JSON has correct structure
- `test_error_exit_code_nonzero` — all error variants produce non-zero exit codes
- `test_error_subtypes` — subtype strings match plan spec
- `test_error_exit_codes` — exit codes: Setup→2, Timeout→124, Interrupted→130, AssistantError→1
- `test_text_error_goes_to_stderr_not_stdout` — text-mode errors go only to stderr
- `test_zero_token_counts_when_fallback` — fallback path produces all-zero usage object
- `test_stream_json_each_line_parses_as_json` — forwarded JSONL lines are valid JSON
- `test_stream_json_disconnect_exits_immediately` — reader thread exits cleanly on disconnect

## Implementation Summary

`src/emitter.rs` (~185 LOC) provides:

- `emit_success()` — routes to `text`/`json`/`stream-json` format output
- `emit_error()` — structured error output by format; text-mode errors to stderr only
- `StreamJsonHandle` — holds mpsc drain channel + thread join handle
- `spawn_stream_json_reader()` / `spawn_stream_json_reader_to()` — testable reader thread
- `stream_json_reader_loop()` — tails transcript JSONL from start_offset, forwards lines to stdout;
  retries file open if transcript not yet present; exits cleanly on drain signal or channel disconnect
