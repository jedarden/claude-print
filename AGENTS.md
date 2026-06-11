# AGENTS.md — claude-print

## Repo purpose

`claude-print` is a drop-in replacement for `claude -p` that drives the Claude Code
interactive TUI via PTY, preserving subscription billing after the June 15, 2026
`cc_entrypoint` split. It spawns `claude` in a pseudo-terminal, auto-dismisses the
trust dialog, injects the user's prompt, waits for the Stop hook, reads the
transcript, and emits clean output — all without `--print` or `--output-format`.

## Build commands

```bash
# Debug build
cargo build

# Musl release (static binary for deployment)
cargo build --target x86_64-unknown-linux-musl --release

# Tests  (intercepted by ~/.local/bin/cargo — submits to iad-ci when repo is clean)
cargo test

# Unit tests only (no binary compilation required)
cargo test --lib

# Integration tests (requires compiled binary)
cargo test --test '*'

# Smoke check (verifies PTY, hooks, and environment prerequisites)
./target/debug/claude-print --check
```

The `cargo` wrapper at `~/.local/bin/cargo` auto-submits to the `rust-verify`
WorkflowTemplate on `iad-ci` when there are no uncommitted changes and the repo has
a remote. It falls back to a cgroup-limited local run otherwise.

## Test structure

| Location | What it tests |
|----------|---------------|
| `src/*.rs` inline (`#[cfg(test)]`) | Unit tests — pure logic, no I/O |
| `tests/integration.rs` | High-level integration; uses `mock_claude` |
| `tests/integration/` | Sub-module helpers for integration tests |
| `tests/cli.rs` | CLI argument parsing and flag validation |
| `tests/emitter.rs` | Output formatting (text / json / stream-json) |
| `tests/startup.rs` | Trust-dialog detection and prompt injection |
| `tests/terminal.rs` | Terminal probe parsing |
| `tests/transcript.rs` | JSONL transcript parsing |
| `tests/hooks.rs` | Stop hook FIFO install / read |
| `tests/stop_poller.rs` | Stop payload polling logic |
| `tests/pty_integration.rs` | PTY spawn + round-trip (requires PTY capability) |
| `tests/version_compat.rs` | `--version` output parsing |
| `tests/fixtures/` | Shared fixture helpers |

### mock_claude

`test-fixtures/mock-claude/` is a workspace member compiled as a separate binary.
It impersonates `claude` for integration tests and is controlled via environment
variables (see its own `README` / source). No real credentials are needed.

To rebuild mock_claude explicitly:
```bash
cargo build -p mock-claude
```

## Module map

| File | Role |
|------|------|
| `src/lib.rs` | Crate root — re-exports all public modules for use in integration tests |
| `src/main.rs` | Entry point — parses CLI, resolves claude binary version, calls run |
| `src/cli.rs` | Clap argument definitions (`Cli`, `OutputFormat`) |
| `src/config.rs` | Loads `~/.claude/claude-print.toml` (model default, etc.) |
| `src/pty.rs` | Forks child, opens PTY pair, forwards SIGWINCH/SIGINT to child |
| `src/startup.rs` | Reads PTY output until trust dialog; auto-dismisses, injects prompt |
| `src/event_loop.rs` | Single-threaded `poll(2)` loop over PTY master + self-pipe + stop FIFO |
| `src/hook.rs` | Installs Stop hook via `CLAUDE_CONFIG_DIR` temp dir; owns the FIFO |
| `src/poller.rs` | Parses Stop hook payload from the FIFO bytes |
| `src/transcript.rs` | Reads `.jsonl` transcript; extracts last assistant message + token usage |
| `src/emitter.rs` | Formats and writes output (`text`, `json`, `stream-json`) |
| `src/terminal.rs` | Absorbs and discards terminal probe sequences (DA1/DA2/DSR/xtversion) |
| `src/error.rs` | `Error` enum and `Result` alias |
| `src/check.rs` | `--check` mode: verifies PTY, FIFO, hooks, and `cc_entrypoint` env |

## Key invariants

These must hold across all changes:

1. **Do not set `CLAUDE_CONFIG_DIR`** — transcripts must land in
   `~/.claude/projects/` (the real config dir). The temp dir is only used for the
   Stop hook settings injection, and it must not redirect the config dir.

2. **Clean up the temp dir on all exit paths** — no `claude-print-<pid>-*`
   directories may be left in `$TMPDIR`. The `TempDir` handle in `HookInstaller`
   must remain owned until after the child exits.

3. **Forward SIGINT to the child process** — pressing Ctrl-C must reach `claude`,
   not just terminate `claude-print`.

4. **Never pass `--print` or `--output-format` to the child** — those flags
   activate the API billing path. The entire point is to stay on the PTY/TUI path.

5. **`cc_entrypoint=cli` is the correctness invariant** — verify that
   `CLAUDE_CC_ENTRYPOINT` (or equivalent) is `cli` via `--check` before each
   release. AS-4 in the plan documents the acceptance criterion.

## Bead workflow

Beads use the `bf` prefix. Config is at `.beads/config.yaml`.

```bash
# List open beads
br list

# Claim a bead
br claim <id>

# Close a bead (requires a commit first)
br close <id>
```

See `CLAUDE.md` (root workspace) for full `br` CLI docs and FrankenSQLite recovery
procedures.
