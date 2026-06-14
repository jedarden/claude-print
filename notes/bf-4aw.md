# bf-4aw: Wire main.rs - Prompt Resolution, Session Dispatch, Emit

## Summary

This bead verified that the main.rs implementation (from commit d942572) correctly wires the full execution path.

## What Was Verified

### 1. Prompt Resolution (lines 57-92)
- ✓ `--input-file <path>`: reads file bytes, exits 4 on error
- ✓ Positional `<prompt>`: UTF-8 encoded bytes
- ✓ stdin: reads when !is_terminal(), exits 4 if empty
- ✓ No prompt: exits 4 with human-readable message

### 2. Build claude_args (lines 94-110)
- ✓ model flag: `--model <name>` when specified
- ✓ max_turns flag: `--max-turns <n>` when non-default (30)
- ✓ no_inherit_hooks: `--setting-sources=` when specified

### 3. Session Dispatch (line 115)
- ✓ Calls `session::Session::run()` with binary, args, prompt, timeout

### 4. Result Matching (lines 122-206)
- ✓ `Ok(session_result)`: emits success, exits 0
- ✓ `Err(Error::Interrupted)`: emits error, exits 130
- ✓ `Err(Error::Timeout)`: emits error, exits 3
- ✓ `Err(Error::Internal)` with "Child exited without sending Stop payload": emits "claude exited before Stop hook fired", exits 2
- ✓ Other errors: emit with message, exits 2

### 5. Stream-JSON Output (lines 127-141, 209-224)
- ✓ `replay_stream_json()` reads transcript line-by-line
- ✓ Emits to stdout for stream-json format
- ✓ Other formats use `emitter::emit_success()`

### 6. AS-5: Binary Not Found Check (lines 48-55)
- ✓ Checks binary existence with `which::which()`
- ✓ Exits 2 with human-readable error: `"'<binary>' not found in PATH"`

## Test Results

```
$ cargo test --lib
running 81 tests
test result: ok. 81 passed; 0 failed; 0 ignored
```

No dead_code warnings for output format arms (text/json/stream-json).

### AS-5 Verification

```bash
$ echo 'hello' | ./target/debug/claude-print --claude-binary /nonexistent
claude-print: '/nonexistent' not found in PATH
Exit code: 2

$ PATH= ./target/debug/claude-print 'hello'
claude-print: 'claude' not found in PATH
Exit code: 2
```

Both tests pass with human-readable error messages naming the missing binary.

## Implementation Notes

- The `session::Session::run()` method internally adds `--dangerously-skip-permissions` and `--settings=<hook_path>` to claude_args, so main.rs only needs to pass model/max-turns flags.
- Duration tracking happens in main.rs with `Instant::now()` before calling session::run().
- The `SessionResult` struct includes `transcript_path` for stream-json replay.
- Input errors (file read, stdin read) are handled at prompt resolution time with exit code 4.
