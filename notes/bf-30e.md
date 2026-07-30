# bf-30e: Stream-JSON Reader Thread Spawn - VERIFICATION COMPLETE

## Status: ✅ ALREADY IMPLEMENTED

The stream-json reader thread spawn at PROMPT_INJECTED transition is **already fully implemented** in `src/session.rs:580-611`.

## Acceptance Criteria - ALL MET ✅

### ✅ 1. Reader thread spawned at PROMPT_INJECTED transition
**Code:** `src/session.rs:580-611`
- Triggered when phase transitions to `PromptInjected`
- Only spawns for `OutputFormat::StreamJson`

### ✅ 2. Byte offset captured from transcript file length at bracketed-paste write
**Code:** `src/session.rs:599` via `emitter::snapshot_jsonl_sizes(&projects_dir)`
- Captures all existing .jsonl file sizes at injection moment
- Used as baseline to skip pre-injection events

### ✅ 3. Retry logic implemented (50ms intervals, 5s timeout)
**Code:** `src/emitter.rs:396-419` in `discover_with_retry()`
- Retries every 50ms via `thread::sleep(Duration::from_millis(50))`
- 5-second deadline via `Instant::now() + Duration::from_secs(5)`
- Handles transcript-not-yet-created case

### ✅ 4. Reader thread drains to mpsc channel
**Code:** `src/emitter.rs:274-286` in `spawn_reader()`
- Creates `mpsc::sync_channel(1)` for drain coordination
- `StreamJsonHandle` holds `drain_tx` and `join_handle`
- Reader loop receives on `drain_rx` for graceful shutdown

## Test Results - ALL PASS ✅

```
running 6 tests
test scenarios::stream_json_error_before_inject_no_stdout ... ok
test scenarios::stream_json_error_after_inject_writes_json_to_stdout ... ok  
test scenarios::stream_json_pipeline_all_lines_valid_json ... ok
test scenarios::stream_json_reader_forwards_lines_incrementally_as_file_grows ... ok
test scenarios::stream_json_start_offset_skips_pre_injection_lines ... ok
test scenarios::stream_json_reader_discovers_and_tails_projects_dir_jsonl ... ok

test result: ok. 6 passed; 0 failed; 0 ignored
```

## Implementation Notes

The implementation uses `spawn_stream_json_reader_discover()` instead of the basic `spawn_stream_json_reader()` because:

1. At PROMPT_INJECTED, the exact `<session_id>.jsonl` filename is **unknown**
2. The session_id only arrives later in the **Stop payload**
3. The discovery pattern polls for the newest/grown .jsonl file

This is the **correct architectural solution** per bead bf-3c7c and the plan documentation.

## Conclusion

No implementation work was needed. The feature is complete and operational.
