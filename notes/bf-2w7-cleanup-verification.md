# bf-2w7: Cleanup Implementation Verification

## Requirements
Ensure temp dir and stop.fifo are removed on ALL exit paths:
- Normal exit
- Error exit
- Watchdog timeout
- SIGTERM/SIGINT
- On startup, sweep stale claude-print-* temp dirs

## Implementation Verification

### 1. Startup Orphan Cleanup ✓
**Location:** `main.rs:39`
```rust
hook::cleanup_orphans();
```
- Sweeps all `/tmp/claude-print-*` directories older than 10 minutes
- Removes FIFO first, then entire directory
- Runs on every invocation, not just session runs

### 2. Normal Exit Cleanup ✓
**Location:** `main.rs:30-33`
```rust
fn exit_with_cleanup(code: i32) -> ! {
    session::cleanup_temp_dir();
    process::exit(code);
}
```
- All success paths call `exit_with_cleanup(0)`
- Explicitly removes FIFO and temp dir with retry logic
- Bypasses Rust destructors (which don't run after process::exit)

### 3. Error Exit Cleanup ✓
**Locations:** All error paths in `main.rs`
- Line 66: binary not found → `exit_with_cleanup(2)`
- Line 80: input file read error → `exit_with_cleanup(4)`
- Line 92: stdin read error → `exit_with_cleanup(4)`
- Line 102: no prompt provided → `exit_with_cleanup(4)`
- Line 174: transcript replay failed → `exit_with_cleanup(2)`
- Line 186: emit success failed → `exit_with_cleanup(2)`
- Line 200: interrupted → `exit_with_cleanup(130)`
- Line 211: timeout → `exit_with_cleanup(124)`
- Line 227/238: internal error → `exit_with_cleanup(2)`

### 4. Watchdog Timeout Cleanup ✓
**Flow:** Watchdog timeout → SIGTERM → self-pipe write → event loop exit → CleanupGuard drop
**Locations:** `watchdog.rs:288-298`, `session.rs:157`, `session.rs:43-49`

Watchdog thread:
```rust
let _ = nix::sys::signal::kill(child_pid, nix::sys::signal::Signal::SIGTERM);
timeout_fired.store(true, Ordering::SeqCst);
// Signal the event loop via self-pipe
if let Some(fd) = self_pipe_write_fd {
    let byte: [u8; 1] = [1];
    unsafe { let _ = libc::write(fd as i32, byte.as_ptr() as *const libc::c_void, 1); }
}
```

Event loop exits when self-pipe has data → CleanupGuard drops → `HookInstaller::cleanup()` runs

### 5. Signal (SIGTERM/SIGINT) Cleanup ✓
**Flow:** Signal → handler writes to self-pipe → event loop returns Interrupted → CleanupGuard drop
**Locations:** `session.rs:424-446`, `session.rs:370-373`

Signal handlers:
```rust
extern "C" fn sigint_handler(_: libc::c_int) {
    unsafe {
        if let Some(fd) = fd_option {
            let byte: [u8; 1] = [1];
            let _ = nix::unistd::write(fd, &byte);
        }
    }
}
```

Event loop returns `ExitReason::Interrupted` → kill_child() → CleanupGuard drops

### 6. Panic Safety ✓
**Location:** `session.rs:114-124`
```rust
let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
    Self::run_inner(...)
}));
match result {
    Ok(inner_result) => inner_result,
    Err(_) => {
        // Panic occurred - cleanup already handled by CleanupGuard
        Err(Error::Internal(anyhow::anyhow!("Session panicked")))
    }
}
```

### 7. RAII CleanupGuard ✓
**Location:** `session.rs:38-49`
```rust
struct CleanupGuard<'a>(&'a HookInstaller);

impl<'a> Drop for CleanupGuard<'a> {
    fn drop(&mut self) {
        self.0.cleanup();
    }
}
```
- Created at `session.rs:157`
- Ensures cleanup even if explicit cleanup is forgotten
- Runs on normal return, error return, timeout, signal handling, panic

### 8. HookInstaller::cleanup() ✓
**Location:** `hook.rs:116-146`
```rust
pub fn cleanup(&self) {
    // Idempotent via atomic swap
    if self.cleanup_performed.swap(true, Ordering::SeqCst) {
        return;
    }
    
    // Remove FIFO first (different permissions)
    let _ = std::fs::remove_file(&self.fifo_path);
    
    // Remove entire temp directory with retry
    for attempt in 0..3 {
        let result = std::fs::remove_dir_all(dir_path);
        if result.is_ok() { break; }
        if attempt < 2 {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }
}
```

### 9. Global TEMP_DIR_PATH ✓
**Location:** `session.rs:23`, `session.rs:55-75`
```rust
static TEMP_DIR_PATH: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

pub fn cleanup_temp_dir() {
    if let Some(path) = TEMP_DIR_PATH.get() {
        let fifo_path = path.join("stop.fifo");
        let _ = std::fs::remove_file(&fifo_path);
        // Retry logic for directory removal
        for attempt in 0..3 {
            let result = std::fs::remove_dir_all(path);
            if result.is_ok() { break; }
            if attempt < 2 {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        }
    }
}
```

## Test Coverage
All 90 library tests pass, including:
- `watchdog_silent_child_times_out_with_cleanup` - verifies no orphaned temp dirs after timeout
- `watchdog_one_second_timeout_fires_cleanly` - verifies cleanup with very short timeout
- `cleanup_can_be_called_multiple_times` - verifies idempotent cleanup
- `cleanup_orphans_does_not_panic` - verifies startup orphan sweep

## Exit Path Matrix

| Exit Path | Cleanup Mechanism | Status |
|-----------|-------------------|--------|
| Normal exit | exit_with_cleanup() + CleanupGuard::drop | ✓ |
| Error exit | exit_with_cleanup() + CleanupGuard::drop | ✓ |
| Watchdog timeout | Self-pipe → Event loop exit → CleanupGuard::drop | ✓ |
| SIGTERM | Signal handler → self-pipe → Interrupted → CleanupGuard::drop | ✓ |
| SIGINT | Signal handler → self-pipe → Interrupted → CleanupGuard::drop | ✓ |
| Panic | catch_unwind → CleanupGuard::drop | ✓ |
| Orphan cleanup on startup | cleanup_orphans() | ✓ |

## Conclusion
All exit paths are covered by multiple redundant cleanup mechanisms:
1. RAII CleanupGuard (always runs when Session::run_inner returns)
2. Global TEMP_DIR_PATH with cleanup_temp_dir() (for process::exit paths)
3. HookInstaller::cleanup() with idempotent flag and retry logic
4. Startup orphan sweeping to prevent accumulation

The implementation is complete and tested.
