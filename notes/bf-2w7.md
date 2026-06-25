# Bead bf-2w7: Cleanup Implementation

## Summary

Implemented comprehensive temp directory and named pipe cleanup on all exit paths.

## Changes Made

### 1. Enhanced HookInstaller cleanup (src/hook.rs)

- Added `cleanup_performed: Arc<AtomicBool>` flag to track cleanup state
- Made `cleanup()` method idempotent - can be called multiple times safely
- Enhanced cleanup to explicitly remove both FIFO and temp directory
- Added `Drop` implementation to ensure cleanup happens on all exit paths

### 2. Exit Path Coverage

The implementation now handles cleanup on all exit paths:

1. **Normal exit**: CleanupGuard drops → HookInstaller drops → cleanup()
2. **Error exit**: Same as normal (CleanupGuard runs on drop)
3. **Watchdog timeout**: Watchdog sets flag and returns Error → main calls exit_with_cleanup()
4. **SIGINT/SIGTERM**: Signal handlers write to self-pipe → EventLoop returns Interrupted → exit_with_cleanup()
5. **Panic**: Drop implementations run during stack unwinding
6. **Abort**: No destructors run, but cleanup_orphans() cleans up on next run

### 3. Startup Orphan Cleanup (src/hook.rs)

- `cleanup_orphans()` is called at start of main() (main.rs:39)
- Sweeps temp dirs matching pattern `claude-print-*` older than 1 hour
- Prevents accumulation of stale temp dirs from crashed runs

## Testing

All 90 tests pass, including:
- `cleanup_can_be_called_multiple_times` - verifies idempotent cleanup
- `temp_dir_cleaned_up_on_drop` - verifies automatic cleanup
- `cleanup_orphans_does_not_panic` - verifies startup cleanup

## Verification

- `cargo check` - compiles without errors
- `cargo test` - all 90 tests pass
- Cleanup is now robust against double-cleanup, panics, and early exit paths
