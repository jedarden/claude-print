# Verification of spawn_stream_json_reader

## Function Signatures

### Primary public function (src/emitter.rs:98-100)
```rust
pub fn spawn_stream_json_reader(transcript_path: PathBuf, start_offset: u64) -> StreamJsonHandle
```

### Testable variant with writer injection (src/emitter.rs:103-116)
```rust
pub fn spawn_stream_json_reader_to(
    transcript_path: PathBuf,
    start_offset: u64,
    writer: Box<dyn Write + Send + 'static>,
) -> StreamJsonHandle
```

Both return a `StreamJsonHandle` containing:
- `drain_tx: mpsc::SyncSender<()>` - channel to signal "drain then exit"
- `join_handle: thread::JoinHandle<()>` - thread handle for joining/waiting

## Retry Logic Verification (src/emitter.rs:127-149)

**Confirmed: 50ms retries up to 5 seconds are correctly implemented.**

The retry logic in `stream_json_reader_loop` works as follows:

1. **5-second deadline**: `let deadline = std::time::Instant::now() + Duration::from_secs(5);`
2. **Retry loop** with 50ms sleep: `thread::sleep(Duration::from_millis(50));`
3. **On `File::open` failure**:
   - First checks for drain signal via `drain_rx.try_recv()` → exit if received
   - Then checks if 5-second deadline expired → exit if timed out
   - Otherwise sleeps 50ms and retries
4. **On success**: `break f` exits the retry loop with the opened `File`

The logic correctly handles:
- File appearing mid-retry (opens and continues)
- Drain signal during retry (exits immediately)
- Timeout after 5 seconds (exits, file never appeared)

## mpsc Channel Drain Verification (src/emitter.rs:157-194)

**Confirmed: The function correctly drains to the mpsc channel.**

The channel usage:
- **Creation**: `mpsc::sync_channel(1)` - bounded sync channel with capacity 1
- **Normal mode**: Continuously reads lines and writes to writer
- **Drain signal**: Sending `()` via `drain_tx` sets `draining = true`, causing the loop to finish the current line then exit
- **Immediate exit**: Dropping `StreamJsonHandle` without sending causes `Disconnected` error → immediate return

## Summary

All acceptance criteria verified:
- ✅ Function signature documented (takes `PathBuf` transcript path and `u64` start_offset, returns `StreamJsonHandle`)
- ✅ 50ms/5s retry logic confirmed in reader loop
- ✅ mpsc channel drain logic confirmed correct
- ✅ No code changes required
