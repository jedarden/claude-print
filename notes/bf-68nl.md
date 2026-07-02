# Bead bf-68nl: Stream-JSON Reader Join Verification

**Date:** 2026-07-02
**Status:** ✅ Already Implemented

## Task
Add stream-json reader join on all exit paths (success, timeout, interrupted, error).

## Verification Results

### Implementation Found
The stream-json reader join logic was already implemented in commit `e0cf57abf11beeb49de7ed953d9be71e6c43008b` (Pre-release commit for v0.2.0).

### Exit Path Verification

All exit paths properly join the stream-json reader thread:

| Exit Path | Line Numbers | Cleanup Mode | Status |
|-----------|--------------|--------------|--------|
| **Timeout** | 404-407 | Drop drain_tx (immediate exit) | ✅ |
| **Success (FifoPayload)** | 442-446 | Send drain signal | ✅ |
| **Error (no transcript)** | 424-428 | Send drain signal | ✅ |
| **Child Exit** | 460-463 | Drop drain_tx (immediate exit) | ✅ |
| **Interrupted** | 469-472 | Drop drain_tx (immediate exit) | ✅ |

### Code Examples

**Success Path (drain mode):**
```rust
// INV-8: On success, send drain signal and join stream-json reader
if let Some(handle) = stream_json_handle {
    // Send drain signal: drain remaining lines then exit
    let _ = handle.drain_tx.send(());
    let _ = handle.join_handle.join();
}
```

**Timeout Path (immediate exit mode):**
```rust
// INV-8: Join stream-json reader on timeout path (drop sender, exit immediately)
if let Some(handle) = stream_json_handle {
    drop(handle.drain_tx); // Drop without sending -> exit immediately
    let _ = handle.join_handle.join();
}
```

### Test Results
- **Lib tests:** 90/90 passed ✅
- **Integration tests:** 28/28 passed ✅
- **Emitter tests:** 13/13 passed ✅
- **Total:** 131/131 tests passed ✅

## Conclusion
The stream-json reader join implementation is complete and correct. All exit paths properly join the background thread before returning, ensuring all output is drained and no threads are leaked.
