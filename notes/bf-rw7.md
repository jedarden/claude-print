## Bead bf-rw7: Phase 2 — Hook Installer + PTY Spawner

Phase 2 was fully implemented in prior commits on this branch. This note records the
completed state for bead bf-rw7.

### Deliverables (all in prior commits)

**hook.rs** — `HookInstaller` (commit `1ff7715`):
- Creates a temp dir via `tempfile::TempDir`
- Writes `settings.json` with a `Stop` hook entry pointing to `hook.sh`
- Writes `hook.sh` that echoes `stop` to the FIFO and exits
- Creates a named pipe (mkfifo equivalent) via `nix::unistd::mkfifo`

**pty.rs** — `PtySpawner` (commits `4a38e8f`, `dcbdb8c`, `a1e74b5`):
- `openpty` via `nix::pty::openpty`
- `fork` via `nix::unistd::fork`
- Child: `login_tty`, `execvp` with the target binary
- Parent: `TIOCSWINSZ` window-size propagation, I/O relay, signal forwarding, `waitpid`

**CLI** — `--no-inherit-hooks` flag (commit `17c35f4`):
- Added to `src/cli.rs` via clap derive

**mock-claude fixture** — `test-fixtures/mock-claude/` (commit `17c35f4`):
- Workspace member binary that checks `isatty(STDIN_FILENO)`, writes `stop\n` to its
  FIFO argument, and exits 0 if stdin is a TTY (exits 1 otherwise)

**Integration test** — `tests/pty_integration.rs` (commit `17c35f4`):
- `test_pty_spawns_tty` exercises `HookInstaller` + `PtySpawner` end-to-end

### Acceptance verified

```
cargo test test_pty_spawns_tty
...
test test_pty_spawns_tty ... ok
```

All 14 unit tests and 1 integration test pass.
