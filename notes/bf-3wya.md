# Bead bf-3wya: Transcript Byte Offset Capture

## Task
Capture transcript byte offset at bracketed-paste write for stream-json reader.

## Implementation
The implementation was already complete in `src/session.rs` at lines 363-375:

```rust
// Check if phase changed to PromptInjected and notify watchdog
let current_phase = startup.phase();
if last_phase != *current_phase && current_phase.is_prompt_injected() {
    watchdog_state_clone.mark_prompt_injected();

    // Spawn stream-json reader at PROMPT_INJECTED for stream-json output
    if matches!(output_format, crate::cli::OutputFormat::StreamJson) {
        // Calculate byte offset: current transcript file size, or 0 if not exists
        let start_offset = std::fs::metadata(&transcript_path)
            .map(|m| m.len())
            .unwrap_or(0);

        stream_json_handle = Some(emitter::spawn_stream_json_reader(
            transcript_path.clone(),
            start_offset,
        ));
        stream_json_spawned_clone.store(true, std::sync::atomic::Ordering::SeqCst);
    }
}
```

## Acceptance Criteria Met
- ✅ Identifies exact point where bracketed-paste write completes (phase change to `PromptInjected`)
- ✅ Captures file size using `std::fs::metadata(&transcript_path).map(|m| m.len()).unwrap_or(0)`
- ✅ Stores offset in `start_offset` variable for use by reader thread
- ✅ Handles missing transcript case with `.unwrap_or(0)`

## Changes
- Fixed warning: removed unnecessary `mut` from `stream_json_spawned` variable (line 305)
- All tests pass (90 passed)
