# Bead bf-5k9t: Verify spawn_stream_json_reader call

## Task Verification

Verified that the `spawn_stream_json_reader` call is properly implemented with all required components.

## Location
File: `/home/coding/claude-print/src/session.rs`
Lines: 360-375

## Implementation Details

### PROMPT_INJECTED Detection Block (lines 360-375)
```rust
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

## Acceptance Criteria Status

✅ **output_format check**: `matches!(output_format, crate::cli::OutputFormat::StreamJson)` (line 364)
✅ **spawn_stream_json_reader call exists**: Lines 370-373
✅ **Parameters passed correctly**: `transcript_path.clone()` (line 371) and `start_offset` (line 372)
✅ **Inside PROMPT_INJECTED detection block**: Lines 360-375
✅ **Code compiles**: Verified with `cargo check`

## Notes

The implementation correctly:
1. Detects the phase transition to PromptInjected
2. Checks if output format is StreamJson before spawning
3. Calculates the start_offset from the current transcript file size
4. Spawns the stream-json reader with the correct parameters
5. Sets the stream_json_spawned flag for coordination

## Conclusion

All acceptance criteria met. No changes required - implementation was already correct.
