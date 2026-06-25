# Watchdog Timeout Implementation (Bead bf-2f5)

## Overview
This document describes the comprehensive watchdog timeout mechanism implemented in claude-print to prevent indefinite hangs when the child process wedges.

## Implementation Location
- **Module**: `src/watchdog.rs`
- **Integration**: `src/session.rs` (lines 200-332)
- **CLI**: `src/cli.rs` (timeout configuration parameters)

## Timeout Types

### 1. PTY First-Output Timeout (`DEFAULT_PTY_TIMEOUT_SECS: 90s`)
- **Purpose**: Detects if child produces no PTY output within deadline
- **Detection**: Watchdog thread checks `pty_output_received` flag
- **Trigger**: Event loop marks flag when first PTY chunk arrives
- **Error**: `TimeoutType::PtyFirstOutput`

### 2. Stream-JSON First-Output Timeout (`DEFAULT_STREAM_JSON_TIMEOUT_SECS: 90s`)
- **Purpose**: Detects if child emits no stream-json events within deadline
- **Detection**: Background thread monitors `<temp_dir>/transcript.jsonl` for valid JSON
- **Trigger**: Sets `stream_json_output_received` when first JSON line detected
- **Error**: `TimeoutType::StreamJsonFirstOutput`

### 3. Overall Timeout (`DEFAULT_OVERALL_TIMEOUT_SECS: 3600s`)
- **Purpose**: Maximum session duration
- **Detection**: Watchdog thread compares elapsed time against deadline
- **Trigger**: Configured via `--timeout` CLI flag
- **Error**: `TimeoutType::OverallTimeout`

### 4. Stop Hook Timeout (`DEFAULT_STOP_HOOK_TIMEOUT_SECS: 120s`)
- **Purpose**: Detects if Stop hook doesn't fire after prompt injection
- **Detection**: Watchdog thread measures time since `mark_prompt_injected()`
- **Trigger**: Startup sequence calls `mark_prompt_injected()` when prompt sent
- **Error**: `TimeoutType::StopHookTimeout`

## Timeout Handling Flow

### 1. Timeout Thread Spawns
```rust
// session.rs:220-225
let watchdog = Watchdog::new(watchdog_config, spawner.child_pid, ...);
let _timeout_thread = watchdog.spawn_timeout_thread();
```

### 2. Event Loop Signals Progress
```rust
// session.rs:260-261 - PTY output
watchdog_state_clone.mark_pty_output();

// session.rs:303-305 - Prompt injection
if current_phase.is_prompt_injected() {
    watchdog_state_clone.mark_prompt_injected();
}
```

### 3. Timeout Fires
```rust
// watchdog.rs:288-299 - PTY timeout example
if elapsed >= Duration::from_secs(config.pty_first_output_timeout_secs) {
    let _ = nix::sys::signal::kill(child_pid, nix::sys::signal::Signal::SIGTERM);
    timeout_fired.store(true, Ordering::SeqCst);
    // Signal event loop via self-pipe
    unsafe {
        let byte: [u8; 1] = [1];
        let _ = libc::write(fd, byte.as_ptr() as *const libc::c_void, 1);
    }
    return;
}
```

### 4. Event Loop Exits and Checks Timeout
```rust
// session.rs:322-332
if watchdog_state.has_timeout_fired() {
    let timeout_type = watchdog_state.get_timeout_type().unwrap();
    eprintln!("claude-print: {}", timeout_type.description());
    eprintln!("claude-print: sending SIGTERM to child pid {}", spawner.child_pid);
    kill_child(spawner.child_pid);
    return Err(Error::Timeout(timeout_msg.to_string()));
}
```

### 5. Child Cleanup (SIGTERM → SIGKILL)
```rust
// session.rs:399-419
fn kill_child(pid: nix::unistd::Pid) {
    let _ = nix::sys::signal::kill(pid, nix::sys::signal::Signal::SIGTERM);
    
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        match nix::sys::wait::waitpid(pid, Some(WaitPidFlag::WNOHANG)) {
            Ok(WaitStatus::StillAlive) => {
                if Instant::now() >= deadline {
                    let _ = nix::sys::signal::kill(pid, nix::sys::signal::Signal::SIGKILL);
                    return;
                }
                thread::sleep(Duration::from_millis(50));
            }
            _ => return,
        }
    }
}
```

### 6. Main Returns Error
```rust
// main.rs:202-212
Err(Error::Timeout(_msg)) => {
    let _ = emit_error(..., &ClaudePrintError::Timeout, ...);
    exit_with_cleanup(ClaudePrintError::Timeout.exit_code()); // 124
}
```

### 7. Cleanup Happens
```rust
// session.rs:55-75
pub fn cleanup_temp_dir() {
    if let Some(path) = TEMP_DIR_PATH.get() {
        let _ = std::fs::remove_file(&path.join("stop.fifo"));
        let _ = std::fs::remove_dir_all(path);
    }
}
```

## Exit Codes
- **Timeout**: 124 (GNU timeout convention)
- **Interrupted**: 130 (128 + SIGINT)
- **Setup errors**: 2
- **Assistant errors**: 1

## CLI Configuration
```bash
--timeout <seconds>              # Overall timeout (default: 3600)
--first-output-timeout <seconds>  # PTY first-output (default: 90)
--stream-json-timeout <seconds>   # Stream-json first-output (default: 90)
--stop-hook-timeout <seconds>    # Stop hook watchdog (default: 120)
```

## Integration Tests
See `tests/watchdog.rs`:
- `watchdog_silent_child_times_out_with_cleanup`: Verifies 2-second timeout fires cleanly
- `watchdog_one_second_timeout_fires_cleanly`: Verifies 1-second timeout fires quickly

## Self-Pipe Signaling
The watchdog uses a self-pipe to wake the event loop when a timeout fires:
- Watchdog writes byte `[1]` to self-pipe write end
- Event loop wakes from `poll()` with POLLIN on self-pipe read end
- Event loop exits normally
- Session checks `watchdog_state.has_timeout_fired()` and returns timeout error

## Stream-JSON Monitoring
A background thread monitors the transcript file:
```rust
fn spawn_stream_json_monitor_in_dir(temp_dir: PathBuf, ...) {
    thread::spawn(move || {
        let transcript_path = temp_dir.join("transcript.jsonl");
        loop {
            if output_received.load(Ordering::SeqCst) { return; }
            // Check file growth and parse JSON lines
            // Set flag when first valid JSON found
            thread::sleep(Duration::from_millis(100));
        }
    })
}
```

## Summary
The watchdog prevents indefinite hangs by:
1. Monitoring four independent timeout conditions
2. Sending SIGTERM → SIGKILL to child process
3. Writing clear diagnostics to stderr
4. Tearing down temp resources via CleanupGuard
5. Exiting non-zero (124) so caller can retry cleanly
