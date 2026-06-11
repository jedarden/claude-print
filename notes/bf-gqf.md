# Bead bf-gqf: PTY spawn and signal forwarding - Already Complete

## Task

Implement src/pty.rs: PTY spawn and signal forwarding

## Findings

The PTY implementation in `src/pty.rs` was already complete at the time of this bead's execution. All required functionality is implemented:

### PtySpawner::spawn (lines 56-97)
- Opens PTY pair via `nix::pty::openpty`
- Forks process using `nix::unistd::fork`
- In child: calls `login_tty` to make slave the controlling terminal, then `execvp` the command
- In parent: returns `PtySpawner` with master fd and child pid

### Signal forwarding in relay() (lines 101-249)
- **SIGWINCH handler** (lines 14-17): Sets atomic flag when window size changes
- **SIGINT handler** (lines 19-22): Sets atomic flag when Ctrl-C pressed
- **SIGINT forwarding** (lines 122-127): When flag set, calls `kill(child_pid, SIGINT)`
- **SIGWINCH propagation** (lines 130-136): Reads winsize from stdin, applies to PTY via TIOCSWINSZ ioctl
- **I/O relay**: Poll loop copies data between stdin→PTY master and PTY master→stdout
- **Exit code handling**: Waits for child and returns exit code or signal+128

### Key invariants verified
✅ Invariant #3: SIGINT is forwarded to child process (not just terminates claude-print)
✅ Signal handlers are async-signal-safe (only touch AtomicBool)
✅ Window size propagated from controlling terminal to PTY
✅ Proper cleanup with waitpid

### Tests verified
```bash
cargo test --lib pty
```
All 7 tests passed:
- `spawn_bin_true_exits_zero` - verifies fork/exec works
- `master_fd_carries_child_stdout` - verifies PTY I/O
- `relay_echo_exits_zero_and_produces_output` - verifies full relay loop
- `relay_surfaces_nonzero_exit_code` - verifies exit code propagation

## Conclusion

No implementation work was required. The PTY spawn and signal forwarding functionality is fully implemented and tested per AGENTS.md specification.
