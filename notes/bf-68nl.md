# Verification of stream-json reader join on all exit paths

## Task (bf-68nl)
Ensure the stream-json reader thread is properly joined on all exit paths (success, timeout, interrupted, error).

## Verification Status: COMPLETE ✓

The stream-json reader join implementation is already present in `src/session.rs` on all exit paths:

### Exit paths verified

1. **Success path (FifoPayload)** - Lines 442-446
   - Sends drain signal: `handle.drain_tx.send(())`
   - Joins thread: `handle.join_handle.join()`

2. **Timeout path** - Lines 404-407
   - Drops handle without sending: `drop(handle.drain_tx)`
   - Joins thread: `handle.join_handle.join()`

3. **Interrupted path** - Lines 469-472
   - Drops handle without sending: `drop(handle.drain_tx)`
   - Joins thread: `handle.join_handle.join()`

4. **Child exit path** - Lines 460-463
   - Drops handle without sending: `drop(handle.drain_tx)`
   - Joins thread: `handle.join_handle.join()`

5. **Early error path (no transcript path)** - Lines 425-428
   - Sends drain signal: `handle.drain_tx.send(())`
   - Joins thread: `handle.join_handle.join()`

### Acceptance criteria met
- ✓ Drain signal and join on success path (FifoPayload)
- ✓ Drop-and-join on timeout path
- ✓ Drop-and-join on interrupted path
- ✓ Drop-and-join on child exit path
- ✓ All paths join the thread before returning
- ✓ Code compiles and tests pass (90 passed)

## Implementation details

The design distinguishes between two types of exit paths:

1. **Graceful exits (success, early error)**: Send drain signal `()` via channel to allow the reader thread to finish processing remaining output before exiting.

2. **Immediate exits (timeout, interrupted, child exit)**: Drop the sender handle without sending, which causes the channel to close immediately and the reader thread exits without waiting for more data.

In both cases, `join_handle.join()` is called to wait for the thread to finish before proceeding.
