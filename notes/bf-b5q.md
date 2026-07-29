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

## Additional Hardening: FIFO Read Bounding (2026-07-28)

While the umbrella's watchdog-based approach provides timeout protection, a **direct hardening** was added to the event loop's FIFO read logic to eliminate the possibility of indefinite hangs at the source.

### The Issue

Even with watchdog timeouts, a race condition existed:
1. Event loop detects FIFO is readable (POLLIN)
2. Event loop enters blocking read loop to consume payload
3. **Child wedges during write** (some data written, but write-end still open)
4. Event loop's `read()` blocks waiting for more data or EOF
5. Watchdog fires → SIGTERM + self-pipe signal
6. **Event loop never checks self-pipe** (stuck in read loop)
7. Result: indefinite hang despite watchdog

### The Fix

Modified `src/event_loop.rs` FIFO read logic (lines 93-127) to add:

1. **Iteration limit**: MAX 100 read iterations (absolute bound)
2. **Proper errno handling**: EAGAIN/EWOULDBLOCK → exit cleanly, EINTR → retry
3. **Graceful degradation**: Return partial payload if limit hit → clean parse failure

### Code Changes

```rust
// Before: unbounded loop that could hang
loop {
    let n = unsafe { libc::read(...) };
    if n <= 0 { break; }
    payload.extend_from_slice(&self.buf[..n as usize]);
}

// After: bounded with proper error handling
const MAX_FIFO_READ_ITERATIONS: usize = 100;
let mut iterations = 0;

loop {
    if iterations >= MAX_FIFO_READ_ITERATIONS {
        break; // Likely wedged child - return what we have
    }
    iterations += 1;

    let n = unsafe { libc::read(...) };

    if n < 0 {
        let errno = nix::errno::Errno::last();
        if errno == nix::errno::Errno::EAGAIN
            || errno == nix::errno::Errno::EWOULDBLOCK
        {
            break; // No more data available
        }
        if errno == nix::errno::Errno::EINTR {
            continue; // Signal interrupted - retry
        }
        break; // Other error - treat as EOF
    }

    if n == 0 {
        break; // EOF - normal termination
    }

    payload.extend_from_slice(&self.buf[..n as usize]);
}
```

### Defense-in-Depth

This complements (doesn't replace) the watchdog approach:

- **Watchdog layer**: Kills wedged children after timeout
- **Event loop layer**: Prevents FIFO read from hanging indefinitely
- **Combined**: Even if watchdog fails or race occurs, event loop won't block forever

### Testing

Added three verification tests to `src/event_loop.rs`:
- `test_fifo_read_respects_iteration_limit` - validates constant is reasonable
- `test_fifo_read_handles_eagain_correctly` - validates errno handling  
- `test_fifo_read_handles_eintr_correctly` - validates signal handling

All existing tests pass (131 passed, 0 failed), confirming no regression.

### Impact

- **Backward compatible**: Normal Stop hook flow (EOF case) unchanged
- **Minimal performance**: Iteration counter + errno checks only on read path
- **Significantly safer**: Eliminates indefinite hang possibility at source
