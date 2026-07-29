# bf-4j7: AGENTS.md Review and Update

## Task
Review and update AGENTS.md to ensure session.rs is properly documented in the module map after its addition in bf-2mm.

## Review Results

### Module Map
✅ **session.rs is already present** (line 77) with accurate description:
- "Session orchestrator: installs hooks, spawns PTY child, runs event loop, reads transcript. Session::run() is the top-level entry point for a single prompt→response cycle."

### Build Commands
✅ All build commands are accurate:
- `cargo build` - Debug build
- `cargo build --target x86_64-unknown-linux-musl --release` - Musl release
- `cargo test` - Tests (intercepted by ~/.local/bin/cargo)
- `cargo test --lib` - Unit tests only
- `cargo test --test '*'` - Integration tests
- `./target/debug/claude-print --check` - Smoke check

### Test Structure
✅ Test structure table is comprehensive and accurate, covering all test files in tests/.

### Key Invariants
✅ Verified all 7 key invariants against actual code:
1. CLAUDE_CONFIG_DIR not set - ✅ No references in code
2. Temp dir cleanup on all exit paths - ✅ Proper cleanup via TEMP_DIR_PATH and CleanupGuard
3. SIGINT forwarding to child - ✅ Implemented in pty.rs:132
4. Never pass --print or --output-format - ✅ No references in code
5. cc_entrypoint=cli invariant - ✅ Verified in check.rs
6. Unset CLAUDE_CODE_SESSION_ID in child - ✅ Implemented in pty.rs:93
7. Keep both FIFO ends alive - ✅ open_fifo_nonblock returns (read_fd, keeper_write_fd)

### Bead Workflow
❌ **Found issue**: Bead workflow examples use deprecated `br` alias instead of canonical `bf`
- Fixed: Updated all examples to use `bf list`, `bf claim`, `bf close`
- Updated reference to use "full `bf` CLI docs" instead of "full `br` CLI docs"

### Test Results
✅ All 131 unit tests pass
✅ All integration tests pass (binary_e2e: 12, cli: 23, emitter: 23, startup: 17, transcript: 10, hooks: 4, stop_poller: 4, pty_integration: 8, terminal: 4, version_compat: 4, watchdog: 2)

## Changes Made
- Updated AGENTS.md bead workflow section to use `bf` instead of deprecated `br` alias
- Updated reference text to match

## Conclusion
AGENTS.md was already comprehensive and accurate, with session.rs properly documented in the module map. The only issue found was the use of the deprecated `br` alias in the bead workflow examples, which has been corrected.
