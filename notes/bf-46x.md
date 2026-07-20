# bf-46x — Binary-level E2E tests: AS-1/AS-2/AS-5 via mock_claude

## Task
Write binary-level E2E tests that invoke the *compiled* `claude-print` binary as a
subprocess, using `mock-claude` (built alongside by `build.rs`) as the claude backend.
Every test pins `--claude-binary` to the mock-claude path, so the suite is hermetic and
needs no real Anthropic credentials. Tests live in `tests/binary_e2e.rs`.

## Acceptance criteria
- `cargo test --test binary_e2e` passes with all scenarios
- No `#[ignore]`d tests
- Covers: AS-1 (text mode), AS-2 (json mode), AS-5 (missing binary, text + json),
  stream-json, no-prompt → exit 4, `--version`

## Outcome on dispatch
The test file `tests/binary_e2e.rs` was **already committed** by a prior run as part of
`7320094` ("docs(bf-1x2h): record re-dispatch"). It is clean in the working tree
(neither modified nor untracked) and already covers every required scenario. This
dispatch's job was to verify it actually passes and close the bead.

## Work performed (this dispatch)
- **Read the contract:** `test-fixtures/mock-claude/src/main.rs` (env-var controls:
  `MOCK_RESPONSE`, `MOCK_EXIT_CODE`, `MOCK_DELAY_STOP`, `MOCK_SILENT`, …) and
  `src/main.rs` to confirm the externally observable behavior the tests assert against
  (exit codes 0/2/4, text vs json vs stream-json output shapes, `--version` string from
  `cli::version_string`, the AS-5 missing-binary guard at `which::which`).
- **Confirmed no `#[ignore]`:** `grep -n ignore tests/binary_e2e.rs` finds none — all 7
  tests run unconditionally.
- **Pre-build disk check:** Root FS had 24G free — above the ~20G Rust-build safety
  threshold from `~/CLAUDE.md`, so no `target/` clearing was needed; incremental build.
- **Ran the suite:** `cargo test --test binary_e2e` → `7 passed; 0 failed; 0 ignored`.

## Result
All acceptance criteria pass.

| Scenario | Test | Result |
|----------|------|--------|
| AS-1 text mode | `as1_text_mode_exit0_nonempty_not_json` | ✅ exit 0, non-empty, not JSON |
| AS-2 json mode | `as2_json_mode_exit0_valid_result_object` | ✅ `result`/`subtype`/`is_error`/`claude_version`/`usage` |
| AS-5 missing (text) | `as5_missing_binary_text_mode_exit2_stderr_message` | ✅ exit 2, stderr names `/nonexistent` |
| AS-5 missing (json) | `as5_missing_binary_json_mode_is_error_true` | ✅ exit 2, `is_error=true` on stdout |
| stream-json | `stream_json_mode_exit0_each_line_valid_json` | ✅ every stdout line is JSON |
| no prompt | `no_prompt_exit4` | ✅ exit 4, stderr mentions "no prompt" |
| `--version` | `version_flag_exit0_names_claude_print_and_wrapping` | ✅ stdout has `claude-print` + `wrapping` |

```
running 7 tests
test as2_json_mode_exit0_valid_result_object ... ok
test as1_text_mode_exit0_nonempty_not_json ... ok
test as5_missing_binary_json_mode_is_error_true ... ok
test as5_missing_binary_text_mode_exit2_stderr_message ... ok
test no_prompt_exit4 ... ok
test version_flag_exit0_names_claude_print_and_wrapping ... ok
test stream_json_mode_exit0_each_line_valid_json ... ok

test result: ok. 7 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```

## Notes
- `tests/binary_e2e.rs` was already committed (in `7320094`) and is unchanged, so this
  dispatch produced no source changes. This note file is the commit artifact for the bead.
- `build.rs` guarantees `mock-claude` is built for any `cargo test`/`clippy` run, so the
  `workspace_bin("mock-claude")` resolution in the test helper is reliable in CI.
