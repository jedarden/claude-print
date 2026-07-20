# bf-8le — Reader thread cleanup on all session exit paths

**Bead:** bf-8le — *Implement reader thread cleanup on all session exit paths*
**Invariant:** INV-8 — *Reader thread (stream-json) joined before process exit*
**Result:** VERIFIED — implementation is complete and correct in the committed
baseline. No source changes were required; this is a verification pass.

## Why cleanup matters here

`main()` terminates every session via `process::exit()`, which bypasses Rust
destructors. An unjoined stream-json reader thread would be killed mid-write,
truncating its stdout output. `StreamJsonHandle::Drop`
(`src/emitter.rs:142`) is the single chokepoint that prevents this: it takes
the `drain_tx` sender (disconnecting the mpsc channel so the reader stops
blocking on `try_recv`), then takes and joins the `JoinHandle`. Because every
exit path drops the handle — explicitly or by letting the local fall out of
scope — the join is guaranteed before `run_inner` returns.

## Acceptance criteria — all satisfied

### 1. Drain signal sent on Stop transition ✓

Normal completion path (`src/session.rs:616-619`):

```rust
if let Some(handle) = stream_json_handle.as_ref() {
    handle.signal_drain();
}
drop(stream_json_handle);
```

`signal_drain()` delivers a `()` over the bounded sync channel; the reader
observes `Ok(())`, flushes its remaining transcript lines to stdout, and only
then sees the disconnect inside the join. The reader is then joined via Drop.

### 2. Sender dropped on SIGINT / timeout paths ✓

- **Watchdog timeout** (`src/session.rs:539-565`): the watchdog thread SIGTERMs
  the child, sets `timeout_fired`, and writes one byte to the self-pipe
  (`src/watchdog.rs:296-301`, `314-319`, `331-336`, `354-359`). The event loop
  sees `POLLIN` on the self-pipe and returns `ExitReason::Interrupted`
  (`src/event_loop.rs:89-91`). Post-loop, `watchdog_state.has_timeout_fired()`
  (`src/session.rs:539`) distinguishes a real signal from a watchdog timeout and
  routes to the timeout arm, which does `drop(stream_json_handle)` at line 562 —
  **no drain signal**, so the reader exits immediately.
- **SIGINT/SIGTERM** (`src/session.rs:643-654`, `ExitReason::Interrupted` with
  no timeout fired): `drop(stream_json_handle)` at line 652 — again no drain,
  immediate exit.
- **Child-exited-without-Stop** (`src/session.rs:629-642`): `drop()` at line 638.

In every one of these, `Drop` takes `drain_tx` (the "drop the sender" step),
disconnecting the channel; the reader treats `Disconnected` as exit-immediately
and the join returns promptly.

### 3. Handle joined on ALL exit paths — INV-8 ✓

Audited every return / `?` exit point in `run_inner` declared after the handle
is created (`src/session.rs:441`). Each is covered:

| # | Exit path | Line | Join mechanism |
|---|-----------|------|----------------|
| 1 | `event_loop.run()?` (poll failure) | 536 | local drop → Drop joins |
| 2 | Watchdog timeout | 562 | explicit `drop()` |
| 3 | `parse_stop_payload()?` | 576 | local drop → Drop joins |
| 4 | `read_transcript()?` | 583 | local drop → Drop joins |
| 5 | `Error::AssistantError` return | 594 | local drop → Drop joins |
| 6 | No-transcript-path return | 599 | local drop → Drop joins |
| 7 | Normal Stop transition | 616-619 | `signal_drain()` + `drop()` |
| 8 | `ExitReason::ChildExited` | 638 | explicit `drop()` |
| 9 | `ExitReason::Interrupted` | 652 | explicit `drop()` |

Panic path: `Session::run` wraps `run_inner` in `catch_unwind`
(`src/session.rs:250`). A panic unwinds the stack, dropping the local
`stream_json_handle` → Drop joins before the `Err(Error::Internal(...))` is
returned. No path orphans the reader.

### 4. No orphaned reader threads after any exit scenario ✓

Single chokepoint guarantee: there is exactly **one** way to join the reader
(`StreamJsonHandle::Drop`), and the handle is reachable on every exit path as
shown above. The reader loop itself (`stream_json_reader_loop`,
`src/emitter.rs:183`) exits on either `Ok(())` (drain, then break), or
`Disconnected` (immediate return) — so it cannot block indefinitely once the
sender is taken or dropped.

## Test coverage (INV-8: "Join coverage in stream-json integration test")

- `tests/integration/scenarios.rs::stream_json_pipeline_all_lines_valid_json`
  — spawns the reader via `spawn_stream_json_reader_to`, calls `signal_drain()`,
  then `drop(handle)` with the comment `// disconnect + join (INV-8)`. Asserts
  all forwarded lines drain and parse as JSON.
- `tests/emitter.rs::test_stream_json_each_line_parses_as_json` — drain + drop
  + join; verifies all lines forwarded.
- `tests/emitter.rs::test_stream_json_disconnect_exits_immediately` — drops the
  handle **without** a drain signal; asserts the thread exits (does not hang),
  mirroring the SIGINT / timeout / error paths.

## Verification run

```
cargo test        # 226 tests across all binaries — 0 failed, 1 ignored
cargo test --lib  # 91 unit tests — 0 failed
```

(1 ignored is a pre-existing `#[ignore]` integration test unrelated to the
reader; not introduced or affected here.)
