# Verification: Reader Thread Cleanup on All Session Exit Paths (bf-8le)

## Task Requirements
Ensure the stream-json reader thread is properly cleaned up on ALL exit paths:
- Send drain signal on Stop transition
- Drop sender on SIGINT/timeout paths
- Join the handle in all cases (INV-8)

## Exit Path Analysis

### 1. Normal Completion (Stop → transition)
**Location:** `src/session.rs:748-751`
```rust
if let Some(handle) = stream_json_handle.as_ref() {
    handle.signal_drain();
}
drop(stream_json_handle);
```
✅ **CORRECT:** Sends drain signal, then drops (which triggers join)

### 2. Watchdog Timeout
**Location:** `src/session.rs:631-658`
```rust
// 13. Check if watchdog timeout fired.
if watchdog_state.has_timeout_fired() {
    // ... diagnostic output ...
    kill_child(spawner.child_pid);

    // INV-8: Timeout path — drop the reader WITHOUT a drain signal
    drop(stream_json_handle);

    return Err(Error::Timeout(timeout_msg.to_string()));
}
```
✅ **CORRECT:** Drops handle without drain (sender dropped, then joined)

### 3. SIGINT/SIGTERM Interrupt
**Location:** `src/session.rs:775-786`
```rust
ExitReason::Interrupted => {
    kill_child(spawner.child_pid);
    // ... child capture dump ...
    // SIGINT/SIGTERM path: the sender is dropped (not signaled)
    drop(stream_json_handle);
    Err(Error::Interrupted("interrupted by signal".to_string()))
}
```
✅ **CORRECT:** Drops handle without drain (sender dropped, then joined)

### 4. Child Exited Without Stop
**Location:** `src/session.rs:761-773`
```rust
ExitReason::ChildExited => {
    let _ = waitpid(spawner.child_pid, None);
    // ... child capture dump ...
    // Drop joins the reader without draining (INV-8, exit-immediately).
    drop(stream_json_handle);
    Err(Error::Internal(anyhow::anyhow!(
        "Child exited without sending Stop payload"
    )))
}
```
✅ **CORRECT:** Drops handle without drain (sender dropped, then joined)

### 5. Stop Before Prompt Injected (EC-7 defense)
**Location:** `src/session.rs:688-693`
```rust
if !prompt_injected {
    kill_child(spawner.child_pid);
    drop(stream_json_handle);
    return Err(Error::Internal(anyhow::anyhow!(
        "Stop hook fired before prompt was injected (EC-7: response to an unsent prompt — possible session identity leak)"
    )));
}
```
✅ **CORRECT:** Drops handle without drain (sender dropped, then joined)

## Drop Implementation Verification

**Location:** `src/emitter.rs:161-179`
```rust
impl Drop for StreamJsonHandle {
    fn drop(&mut self) {
        // 1. Disconnect the channel FIRST
        self.drain_tx.take();
        
        // 2. Join the thread (INV-8)
        if let Some(handle) = self.join_handle.take() {
            let _ = handle.join();
        }
    }
}
```
✅ **CORRECT:**
1. Sender is dropped (`drain_tx.take()`) - disconnects channel
2. Thread is joined (`handle.join()`) - ensures reader exits

## Test Verification

All cleanup tests pass:
- `test_stream_json_handle_drop_joins_thread_after_drain` ✅
- `test_stream_json_handle_drop_joins_thread_without_drain` ✅
- `test_stream_json_handle_drop_joins_thread_mid_read` ✅
- `test_stream_json_handle_multiple_drop_safe` ✅
- `test_stream_json_discover_reader_drop_joins_thread` ✅
- `test_stream_json_handle_cleanup_order` ✅
- `test_stream_json_signal_drain_then_drop` ✅

## Conclusion

**IMPLEMENTATION STATUS: ✅ COMPLETE**

All session exit paths properly clean up the reader thread:
1. Normal Stop: drain signal → drop → join
2. Timeout/SIGINT/ChildExit/EarlyExit: drop → join

The `Drop` implementation ensures:
- Sender is always dropped (disconnects channel)
- Thread is always joined (INV-8 satisfied)

No orphaned reader threads are possible after any exit scenario.
