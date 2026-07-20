# bf-b5q — Harden headless claude-print: never hang on a wedged child

**Bead:** bf-b5q — *Umbrella: investigate + harden so a wedged/slow/missing child
can never hang the wrapper.*
**Result:** VERIFIED — every blocking dependency is closed and the hardening is
proven end-to-end. This is a verification/closure pass on the umbrella; no source
changes were required for the umbrella itself.

## The fatal gap this umbrella closed

On 2026-06-21 a marathon run wedged 33+ min on iteration 1: the child `claude`
hung at startup (S state, ~17s CPU over 33 min, never emitted stream-json), so
its Stop hook never fired, and `claude-print` sat in `do_sys_poll` reading
`stop.fifo` **with no timeout**. The root architectural gap: there was no
watchdog — a missing/wedged Stop hook could block the wrapper forever. This
umbrella tracked the five fixes that, together, guarantee that can no longer
happen.

## All five blocking sub-beads — closed

| Sub-bead | Title | Status |
|----------|-------|--------|
| bf-2u1 | Investigate: child hangs at startup so Stop hook never fires | closed |
| bf-2f5 | Add watchdog: no-output + max-turn timeout that kills child, exits non-zero | closed |
| bf-uj0 | Make child startup non-blocking in headless mode (pre-trust cwd, bound MCP init) | closed |
| bf-2w7 | Always tear down temp dir + stop.fifo on every exit path; sweep orphans on startup | done |
| bf-3eq | Regression test: a child that never outputs / never fires Stop must time out | closed |

Together they cover the full hardening arc: root-cause investigation (bf-2u1), a
watchdog that kills the child and exits non-zero on no-output/overall/Stop-hook
timeouts (bf-2f5), non-blocking startup so the child can't wedge at trust/MCP
init (bf-uj0), guaranteed temp-dir/FIFO teardown plus startup orphan sweep (bf-2w7),
and the regression test that pins the contract (bf-3eq).

## End-to-end verification — the umbrella's acceptance criterion

The umbrella's whole point is "a wedged child times out instead of hanging." That
is exercised directly by bf-3eq's regression test, which drives the **real**
`Session::run` flow with a wedged mock child:

- `tests/watchdog.rs::watchdog_silent_child_times_out_with_cleanup` — spawns
  `mock-claude` with `MOCK_SILENT=1` (blocks forever, writes nothing, never fires
  Stop), then asserts `Session::run` returns `Error::Timeout` mentioning
  PTY/output **within the 2s first-output deadline**, and that no
  `claude-print-*` temp dir is left orphaned.
- `tests/watchdog.rs::watchdog_one_second_timeout_fires_cleanly` — same path with
  an aggressive 1s budget, confirming the watchdog fires (rather than the wrapper
  hanging on `stop.fifo`) even under tight deadlines.

Because these tests call `Session::run` directly (not an isolated harness), they
prove the watchdog is wired into the production session flow. The pre-existing
bug — an indefinite `do_sys_poll` on `stop.fifo` — is now bounded by the watchdog
on every iteration.

## Verification run

```
cargo build                                    # lib + bin — clean (no warnings/errors)
cargo test --test watchdog                     # 2 passed; 0 failed  (bf-3eq regression)
cargo test --lib watchdog                      # 6 passed; 0 failed  (watchdog unit)
cargo test --lib session::                     # 17 passed; 0 failed (incl. bf-uj0 pretrust + watchdog wiring)
```

The watchdog SIGTERMs the wedged child, sets `timeout_fired`, writes one byte to
the self-pipe, and the event loop returns `ExitReason::Interrupted` →
`Error::Timeout`; the stream-json handle is then dropped (joined) on the timeout
exit path. No iteration can block forever.

## Note on uncommitted sibling-bead WIP

The working tree contains uncommitted changes that belong to **other, still-open
beads** — not this umbrella — and are intentionally left untouched:

- `tests/stream_json_incremental.rs` + parts of
  `tests/integration/scenarios.rs`, `test-fixtures/mock-claude/src/main.rs`,
  `src/session.rs`, `src/watchdog.rs` are work toward **bf-5xw** (in_progress) and
  its blockers **bf-3isy** / **bf-5vm** (open). The new test is explicitly
  `#[ignore]`'d pending those beads.
- The small `src/watchdog.rs` tweaks (`#[cfg(test)]` on the test-only
  `fire_timeout`, a `Default` impl, `map_while(Result::ok)`) are incidental
  clippy/dead-code cleanups that ride along with that WIP and compile cleanly,
  but are out of scope for this umbrella.

This umbrella's own deliverables (the five sub-beads above) are all committed in
the baseline; the regression test that defines its acceptance is green.
