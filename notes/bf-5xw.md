# bf-5xw — Integration test for incremental stream-json output

**Bead:** bf-5xw — *Write integration test for incremental stream-json output*
**Result:** VERIFIED + STRENGTHENED — the deliverable test landed in `eaa350c`
(`tests/stream_json_incremental.rs`, on origin) but was never closed. This
re-dispatch verified the test is sound and closed a documentation defect the
original commit introduced.

## State at re-dispatch

- Bead was `in_progress`; test already committed in `eaa350c` and present on
  both local `main` and `origin/main` (file identical to commit — no working-
  tree diff). No new end-to-end test was needed.
- The end-to-end test is intentionally `#[ignore]`'d as a forward-looking
  contract gated on two sibling beads, both still **open**:
  - **bf-3isy** — `mock_claude` must actually write the transcript JSONL at the
    `transcript_path` it reports (it doesn't today) and add `MOCK_DELAY_JSONL`/
    `MOCK_TURNS`. Confirmed: `test-fixtures/mock-claude/src/main.rs` handles
    `MOCK_SILENT`, `MOCK_DELAY_STOP`, `MOCK_TRUST_DIALOG`, etc. — **not**
    `MOCK_DELAY_JSONL` or `MOCK_TURNS`.
  - **bf-5vm** — the live reader must tail the REAL transcript path.
- Blocker dependency **bf-8le** is `closed`.

## Defect found and fixed: dangling test reference

The module docs of `stream_json_incremental.rs` (line 37) and the `eaa350c`
commit message both claim the live-tail primitive is pinned by a non-ignored
test:

> `stream_json_reader_forwards_lines_incrementally_as_file_grows` (in
> `tests/integration/scenarios.rs`)

That test **did not exist** — it was referenced but never written (grep across
`tests/`/`src/` found only the doc comment, no `#[test] fn`). This made the
"non-ignored safety net" claim false.

**Fix:** added the test to `tests/integration/scenarios.rs`. It deterministically
proves `spawn_stream_json_reader_to` forwards lines INCREMENTALLY as the file
grows — independent of `mock_claude`:

1. Write only line 1 to the transcript, spawn the reader (no drain signaled).
2. Sleep 100ms (>> the reader's 5ms EOF poll), assert line 1 already reached the
   writer while the file is still "open" — i.e. live forwarding, not replay.
3. Append line 2, sleep, assert line 2 also forwarded.
4. `signal_drain` + drop, assert both lines present, in order.

Added `use std::time::Duration;` to the file (was not previously imported).

## Verification

```
cargo test --test integration                                  # 29 passed; 0 failed; 0 ignored
cargo test --test integration stream_json                      # 5 passed (incl. new primitive)
cargo test --test stream_json_incremental                      # 1 ignored, exit 0  (default — not run)
cargo test --test stream_json_incremental -- --ignored         # FAILED as expected
```

The `--ignored` run fails at the **assistant-event assertion** (line 205), not
the "no output" assertion the original commit described — because the binary
now emits an `internal_error` result line (`claude exited before Stop hook
fired`) even when the session fails. Either failure point correctly proves the
test cannot pass until bf-3isy/bf-5vm land; the harness is sound. (A
`dead_code` warning on the `claude-print` lib is pre-existing — out of scope;
this change touches only test files.)

## Acceptance criteria

- Integration test using `mock_claude` with `MOCK_DELAY_JSONL` ✓
  (`stream_json_incremental.rs`, ignored pending bf-3isy).
- Multi-turn output scenario ✓ (`MOCK_TURNS=3`).
- Assertion that stream-json lines appear on stdout BEFORE session completion ✓
  (core `first_instant < exit_instant` assertion).
- "Test passes after bead 1 and 2 are implemented" ✓ — it is the contract for
  bf-3isy + bf-5vm; remove the `#[ignore]` once both close.
