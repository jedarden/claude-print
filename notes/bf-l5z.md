## Bead bf-l5z: Wire --no-inherit-hooks, mock-claude, test_pty_spawns_tty

All three deliverables were committed in 17c35f4 by a prior run of this bead
that did not reach the close step. This note records the completed state.

### What was done

1. `--no-inherit-hooks` flag added to `src/cli.rs` (line 72–73) via clap derive.
2. `test-fixtures/mock-claude/` — workspace member binary that writes `stop\n`
   to its FIFO argument and exits 0 if `isatty(STDIN_FILENO)` is true.
3. `tests/pty_integration.rs` — `test_pty_spawns_tty` uses `HookInstaller` +
   `PtySpawner` to spawn mock-claude under a PTY and asserts exit code 0.

### Acceptance verified

```
cargo test test_pty_spawns_tty
...
test test_pty_spawns_tty ... ok
```
