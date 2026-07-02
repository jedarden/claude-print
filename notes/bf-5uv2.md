# Verification: StreamJsonHandle Storage and Spawned Flag Setting

## Task Confirmation

Verified that `StreamJsonHandle` is stored and the spawned flag is set correctly in `src/session.rs`.

## Acceptance Criteria Verified

### 1. ✓ stream_json_handle = Some(...) assignment exists
**Location:** `src/session.rs:370-373`
```rust
stream_json_handle = Some(emitter::spawn_stream_json_reader(
    transcript_path.clone(),
    start_offset,
));
```

### 2. ✓ stream_json_spawned_clone.store(true, ...) call is present
**Location:** `src/session.rs:374`
```rust
stream_json_spawned_clone.store(true, std::sync::atomic::Ordering::SeqCst);
```

### 3. ✓ Ordering::SeqCst is used for the atomic store
The store operation uses `std::sync::atomic::Ordering::SeqCst`, providing the strongest memory ordering guarantees.

### 4. ✓ stream_json_handle is the correct variable type
**Variable declaration:** `src/session.rs:304`
```rust
let mut stream_json_handle: Option<emitter::StreamJsonHandle> = None;
```

**Struct field definition:** `src/session.rs:45`
```rust
pub stream_json_handle: Option<emitter::StreamJsonHandle>,
```

### 5. ✓ Spawned flag visibility to other parts of the code
The flag is declared as:
```rust
let stream_json_spawned = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
```

Since it's wrapped in `Arc<AtomicBool>` and stored with `Ordering::SeqCst`, the spawned flag is properly synchronized and will be visible across threads.

## Context

The handle and spawned flag are set when the `PROMPT_INJECTED` phase is reached in the event loop (`src/session.rs:360-376`). This ensures the stream-json reader is spawned at the correct time during the session lifecycle.
