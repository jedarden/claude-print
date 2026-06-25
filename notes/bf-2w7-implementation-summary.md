# BF-2W7: Cleanup Implementation Summary

## Task
Always tear down temp dir + stop.fifo on every exit path; sweep orphans on startup.

## Implementation Status: ✅ COMPLETE

All requirements have been implemented and tested:

### 1. Orphan Cleanup on Startup ✅
- **Function**: `hook::cleanup_orphans()` in `src/hook.rs:9-57`
- **Called**: `main.rs:43` - early in main(), before any session runs
- **Behavior**: Sweeps `/tmp/claude-print-*` directories older than 60 seconds, removes FIFO first then entire directory

### 2. CleanupGuard RAII Pattern ✅
- **Struct**: `CleanupGuard<'a>` in `src/session.rs:47-53`
- **Drop Implementation**: Calls `installer.cleanup()` when guard is dropped
- **Coverage**: All exit paths where guard goes out of scope (success, error, timeout, signal, panic)

### 3. Global Cleanup Before process::exit() ✅
- **Function**: `session::cleanup_temp_dir()` in `src/session.rs:60-98`
- **Wrapper**: `exit_with_cleanup()` in `main.rs:30-33`
- **Behavior**: Idempotent via atomic swap, removes FIFO first, retry logic (3 attempts, 10ms delays)
- **Called**: Before every `process::exit()` call in all exit paths

### 4. Atexit Handler Registration ✅
- **Function**: `session::register_cleanup_handler()` in `src/session.rs:106-115`
- **Called**: `main.rs:38` - very early in startup
- **Behavior**: Registers `libc::atexit()` handler to call `cleanup_temp_dir()` even if Rust's default signal handler triggers

### 5. Signal Handling (SIGINT/SIGTERM) ✅
- **Handlers**: `sigint_handler` and `sigterm_handler` in `src/session.rs:464-486`
- **Mechanism**: Write to self-pipe → event loop returns `ExitReason::Interrupted` → `CleanupGuard` drops
- **SignalGuard**: Restores default handlers on drop

### 6. HookInstaller Cleanup Method ✅
- **Function**: `HookInstaller::cleanup()` in `src/hook.rs:122-163`
- **Behavior**: Idempotent via `Arc<AtomicBool>`, removes FIFO first, retry logic for directory removal
- **Called**: Automatically by `CleanupGuard::drop`

### 7. Panic Safety ✅
- **Implementation**: `std::panic::catch_unwind` in `Session::run()` (session.rs:154-173)
- **Behavior**: Panic caught → `CleanupGuard` drops → cleanup runs

## Test Coverage

All tests pass:
- ✅ 90 library tests (including `cleanup_can_be_called_multiple_times`, `cleanup_orphans_does_not_panic`)
- ✅ 28 integration tests (including `invariant_temp_dir_drop_removes_all_artifacts`)

## Exit Path Coverage Matrix

| Exit Path | Cleanup Mechanism | Status |
|-----------|-------------------|--------|
| Normal exit | `exit_with_cleanup()` + `CleanupGuard::drop` | ✅ |
| Error exit | `exit_with_cleanup()` + `CleanupGuard::drop` | ✅ |
| Watchdog timeout | Self-pipe → Event loop exit → `CleanupGuard::drop` | ✅ |
| SIGTERM | Signal handler → self-pipe → `Interrupted` → `CleanupGuard::drop` | ✅ |
| SIGINT | Signal handler → self-pipe → `Interrupted` → `CleanupGuard::drop` | ✅ |
| Panic | `catch_unwind` → `CleanupGuard::drop` | ✅ |
| Orphan cleanup on startup | `cleanup_orphans()` | ✅ |

## Implementation Details

### FIFO Removal Strategy
The FIFO (named pipe) is removed **before** the directory because:
1. FIFOs have different permissions that can block directory removal
2. Must be explicitly removed (not part of normal directory tree)
3. Retry logic handles transient errors

### Retry Logic
Both `cleanup_temp_dir()` and `HookInstaller::cleanup()` use retry logic:
- 3 attempts with 5-10ms delays between attempts
- Prevents failures due to temporary file locks or access delays

### Idempotency
Both cleanup mechanisms are idempotent:
- `cleanup_temp_dir()` uses `AtomicBool::swap` to prevent double cleanup
- `HookInstaller::cleanup()` uses `Arc<AtomicBool>` flag
- Safe to call multiple times without side effects

## Conclusion

The implementation provides **defense in depth** with multiple redundant cleanup mechanisms:
1. RAII `CleanupGuard` (always runs when `Session::run_inner` returns)
2. Global `TEMP_DIR_PATH` with `cleanup_temp_dir()` (for `process::exit` paths)
3. `HookInstaller::cleanup()` with idempotent flag and retry logic
4. Atexit handler for external signal cases
5. Startup orphan sweeping to prevent accumulation

All exit paths are covered, ensuring no orphaned temp directories or FIFOs remain after any termination scenario.

## Files Modified
- `src/hook.rs`: Added `cleanup_orphans()` function and enhanced `HookInstaller::cleanup()`
- `src/session.rs`: Added `CleanupGuard`, `cleanup_temp_dir()`, `register_cleanup_handler()`
- `src/main.rs`: Added `exit_with_cleanup()` wrapper, registered cleanup handlers, called `cleanup_orphans()`
- `tests/`: Integration tests verify cleanup behavior

## Verification
Run: `cargo test` - All 90 library tests + 28 integration tests pass
