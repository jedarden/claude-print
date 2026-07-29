# BF-4J7: AGENTS.md Verification

## Task
Review and update AGENTS.md to reflect the final module map including session.rs.

## Verification Results

### 1. Module Map Status
✅ **session.rs is already included** in the module map (line 79) with accurate description:
```
Session orchestrator: installs hooks, spawns PTY child, runs event loop, reads transcript.
Session::run() is the top-level entry point for a single prompt→response cycle.
```

### 2. Build/Test Commands
✅ All commands verified working:
- `cargo build` - compiles successfully
- `cargo test` - all 131 unit tests + integration tests pass
- `./target/debug/claude-print --check` - check mode works

### 3. Key Invariants Section
✅ All 7 invariants documented and accurate:
1. Do not set CLAUDE_CONFIG_DIR
2. Clean up temp dir on all exit paths
3. Forward SIGINT to child
4. Never pass --print or --output-format
5. cc_entrypoint=cli correctness invariant
6. Unset CLAUDE_CODE_SESSION_ID in child
7. Keep both FIFO ends alive

### 4. Implementation Notes
✅ session.rs patterns well-documented:
- Watchdog timeout thread detached (line 133-139)
- Stream-json reader RAII cleanup (line 141-145)
- Child cleanup with kill_child() (line 147-149)

### 5. Bead Workflow
✅ Accurate and matches current workspace practice

## Conclusion
AGENTS.md was already complete and up-to-date. No changes needed. All tests pass, documentation matches implementation.

## Test Results
- 131 unit tests passed
- Integration tests passed
- No TODO/FIXME markers in session.rs
- All invariants (INV-3, INV-8) properly enforced in code
