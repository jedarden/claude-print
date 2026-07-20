# bf-5pb — Verify existing stream-json tests still pass

## Task
After implementing live streaming (the `spawn_stream_json_reader` reader wired into
the production session path at `src/session.rs:521`), confirm the existing
stream-json tests in `tests/emitter.rs` and `tests/integration/scenarios.rs` still
pass — i.e. the session-flow changes did **not** break the harness-level
stream-json test infrastructure.

## Scope result — PASS (bead acceptance met)

The two named suites are fully green. These are the tests that exercise
`spawn_stream_json_reader` / `spawn_stream_json_reader_to` directly through the
test harness:

```
$ cargo test --test emitter
test result: ok. 14 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out

$ cargo test --test integration
test result: ok. 29 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```

Notable stream-json cases, all passing:
- `emitter::test_stream_json_each_line_parses_as_json` … ok
- `emitter::test_stream_json_disconnect_exits_immediately` … ok
- `scenarios::stream_json_pipeline_all_lines_valid_json` … ok
- `scenarios::stream_json_start_offset_skips_pre_injection_lines` … ok
- `scenarios::stream_json_error_before_inject_no_stdout` … ok
- `scenarios::stream_json_error_after_inject_writes_json_to_stdout` … ok
- `scenarios::stream_json_reader_forwards_lines_incrementally_as_file_grows` … ok

**Verdict:** the live-streaming changes did not regress the harness-level
stream-json functionality. `emitter.rs` (14/14) and `integration/scenarios.rs`
(29/29) pass cleanly. This bead's scope is satisfied.

## Secondary finding — `tests/binary_e2e.rs` fails 4/7 (pre-existing, NOT a regression)

A full `cargo test` is not green because `tests/binary_e2e.rs` fails 4 of 7
scenarios. This is **outside** this bead's named scope (`emitter.rs` +
`scenarios.rs`), but it is documented here because (a) "cargo test passes" is
listed acceptance and (b) one failing case is a binary-level stream-json test.
The conclusion is that it is a **pre-existing structural problem unrelated to
live-streaming**, not a regression introduced by the session-flow changes.

### Evidence it is not a live-streaming regression
- `git diff --stat f380128 HEAD -- src/` is **empty** — production source is
  byte-identical to `f380128`, where bead bf-46x claimed binary_e2e passed 7/7.
  The live-streaming reader call already exists in that identical source.
- Failure is deterministic, not flaky: reproduced across 3+ automated runs, a
  single-threaded run (`--test-threads=1`), a clean rebuild of `mock-claude`
  (`touch` + `cargo build -p mock-claude`), and a manual binary invocation
  (`target/debug/claude-print --claude-binary <mock> --output-format
  stream-json "test prompt"`). All fail identically.

### Per-test split
| Test | Result | Why |
|------|--------|-----|
| `no_prompt_exit4` | ok | exits 4 before spawning claude — no session path |
| `version_flag_exit0_names_claude_print_and_wrapping` | ok | `--version` handled in-process — no session path |
| `as5_missing_binary_text_mode_exit2_stderr_message` | ok | claude-binary `/nonexistent` → fails at "missing binary" — no Stop needed |
| `as1_text_mode_exit0_nonempty_not_json` | **FAILED** | needs mock-claude to deliver Stop |
| `as2_json_mode_exit0_valid_result_object` | **FAILED** | needs mock-claude to deliver Stop |
| `as5_missing_binary_json_mode_is_error_true` | **FAILED** | needs mock-claude to deliver Stop |
| `stream_json_mode_exit0_each_line_valid_json` | **FAILED** | needs mock-claude to deliver Stop |

Every failing case reports `claude exited before Stop hook fired`.

### Root cause (structural)
claude-print's Stop-payload delivery works like this (`src/hook.rs`):
1. Create `stop.fifo` (named pipe) and open its read-end (keeper write-end held).
2. Write `hook.sh` = `cat > '<fifo>' 2>/dev/null || true`.
3. Write `settings.json` with a **Stop** hook running `hook.sh`.
4. Spawn the claude binary as a PTY child with
   `--dangerously-skip-permissions --settings=<settings> --setting-sources= <prompt>`.

The **real** `claude`, on turn completion, reads `--settings`, fires the Stop
hook → `hook.sh` → `cat > fifo` → claude-print reads the payload from the FIFO.

`mock-claude` (`test-fixtures/mock-claude/src/main.rs`) implements **none** of
claude's hook system: it never reads `settings.json`, never runs `hook.sh`. Its
only Stop-delivery path is `std::env::args().nth(1)` treated as a FIFO path —
but as the spawned claude child its `argv[1]` is `--dangerously-skip-permissions`,
not a FIFO path, so `OpenOptions::write().open(argv[1])` fails and it exits
without writing anything. claude-print then observes the child exit with no FIFO
payload → "claude exited before Stop hook fired".

So with `mock-claude` pinned as `--claude-binary`, **the Stop hook can never
fire** regardless of any session-flow / stream-json work. The 3 passing tests
are exactly those that never reach the session/Stop path.

### On bf-46x's "7 passed" claim
bf-46x's trace (`.beads/traces/bf-46x/`) contains only agent narrative
summarizing a "7 passed" table — no captured `cargo test` runner output — and
the trace itself surfaces a `stop-hook-error` system notification. Given the
deterministic, structurally-explained failure above, that pass report does not
reflect a genuine run and should not be relied upon.

## Recommendation
Open a follow-up bead to fix the binary_e2e / mock-claude Stop-delivery
mechanism so the session-path scenarios actually pass (e.g. have `mock-claude`
honor a FIFO path supplied via env var or trailing arg, or otherwise simulate
the Stop hook writing to `stop.fifo`). This is a test-harness fix, not a
production-code change, and is independent of the live-streaming work verified
here.

## Outcome
- `tests/emitter.rs`: 14/14 pass ✅
- `tests/integration/scenarios.rs`: 29/29 pass ✅
- No source changes (verification-only dispatch). This note is the commit artifact.
- `tests/binary_e2e.rs`: pre-existing structural failure documented above, out of
  scope for this bead, flagged for follow-up.
