# Verification: Store StreamJsonHandle and Set Spawned Flag

## Task (bf-5uv2)
Verify that the returned StreamJsonHandle is stored in stream_json_handle and the stream_json_spawned_clone flag is set.

## Verification Results

### 1. stream_json_handle = Some(...) assignment ✅
**Location:** src/session.rs:370-373
```rust
stream_json_handle = Some(emitter::spawn_stream_json_reader(
    transcript_path.clone(),
    start_offset,
));
```

### 2. stream_json_spawned_clone.store(true, ...) call ✅
**Location:** src/session.rs:374
```rust
stream_json_spawned_clone.store(true, std::sync::atomic::Ordering::SeqCst);
```

### 3. Ordering::SeqCst is used ✅
The atomic store at line 374 uses `std::sync::atomic::Ordering::SeqCst`, which provides the strongest memory ordering guarantee and ensures visibility across all threads.

### 4. Correct variable type ✅
**Declaration at src/session.rs:304:**
```rust
let mut stream_json_handle: Option<emitter::StreamJsonHandle> = None;
```

**Struct field at src/session.rs:45:**
```rust
pub stream_json_handle: Option<emitter::StreamJsonHandle>,
```

**Type definition at src/emitter.rs:90:**
```rust
pub struct StreamJsonHandle { ... }
```

### 5. Spawned flag visibility ✅
The `Ordering::SeqCst` memory ordering ensures that:
- The store operation is sequentially consistent
- All threads see the write in the same order
- The flag becomes visible to other parts of the code immediately after the store

## Context
This code runs in the event loop callback when the phase transitions to `PromptInjected` (line 360-376). The spawned flag signals to the watchdog that the stream-json reader has been started, which is important for monitoring stream-json output timeouts.
