# Verify transcript_path and start_offset availability (bf-5t5n)

## Summary

Verified that `transcript_path` and `start_offset` variables are correctly available and used at the PROMPT_INJECTED transition point in the event loop.

## Verification Details

### Location: src/session.rs

1. **transcript_path declaration** (line 303):
   ```rust
   let transcript_path = temp_dir_path.join("transcript.jsonl");
   ```
   - Correctly points to `<temp_dir>/transcript.jsonl`

2. **start_offset calculation** (lines 366-368):
   ```rust
   let start_offset = std::fs::metadata(&transcript_path)
       .map(|m| m.len())
       .unwrap_or(0);
   ```
   - Uses `std::fs::metadata` to get file metadata
   - Extracts file length in bytes via `m.len()`
   - `unwrap_or(0)` handles missing file case: defaults to 0 if file doesn't exist yet

3. **Spawn call** (lines 370-373):
   ```rust
   stream_json_handle = Some(emitter::spawn_stream_json_reader(
       transcript_path.clone(),
       start_offset,
   ));
   ```
   - Both variables are in scope
   - `transcript_path` is cloned (owned value moved into spawned thread)
   - `start_offset` is copied (u64 is Copy)

## Conclusion

All acceptance criteria met. The variables are correctly available and contain correct values at the PROMPT_INJECTED transition point.
