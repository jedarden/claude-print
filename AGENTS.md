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
| `tests/sigint_forwarding_e2e.rs` | Single-session SIGINT forwarding through `PtySpawner::relay` (HR-8): mock child receives the forwarded signal AS SIGINT (trap marker + default-disposition kill), relay returns 130, child reaped, SIGINT/SIGWINCH dispositions restored (bead claudepr-1472789b) |
| `tests/version_compat.rs` | `--version` output parsing |
| `tests/watchdog.rs` | Watchdog timeout for silent children (no output + no Stop hook) |
| `tests/binary_e2e.rs` | Binary-level end-to-end via the *compiled* binary + mock-claude (exit codes, stdout/stderr contract, child-argv forwarding: hook-inheritance modes across CLI flag and `inherit_hooks` config, `--dangerously-skip-permissions`) |
| `tests/stream_json_incremental.rs` | Incremental stream-json forwarding through the real binary (events emitted mid-session, not post-burst) |
| `tests/stream_json_cleanup.rs` | Stream-json reader thread cleanup on all exit paths (verifies plan invariant INV-8) |
| `tests/transcript_race_e2e.rs` | AS-6 end-to-end test: Stop-before-JSONL-flush race (bead bf-3isy) |
| `tests/stop_duplicate_firings_e2e.rs` | Degraded-run regression: duplicate/spurious extra Stop firings (`MOCK_EXTRA_STOPS`) still yield exactly one clean result (bead claudepr-8dcf53ce) |
| `tests/stop_sparse_payloads_e2e.rs` | Sparse Stop payload regression: absent optional fields derive, fall back to `last_assistant_message`, or produce a bounded setup error — across text/json/stream-json (`MOCK_OMIT_*`, `MOCK_WRITE_DERIVED_JSONL`, `MOCK_UNKNOWN_FIELDS`; bead claudepr-f3ed858a) |
| `tests/stop_delayed_payload_e2e.rs` | FIFO keeper-lifetime regression (key invariant 7): a Stop payload withheld ~1.5 s (`MOCK_DELAY_STOP`) is still received exactly once through the live event loop — no premature exit, no lost write, normal cleanup — plus a poller-level delayed hook-shaped write pin (bead claudepr-a847d4de) |
| `tests/transcript_flush_window.rs` | Flush-window regression: final assistant line absent/truncated on first read, present on retry; decoy `last_assistant_message` suppressed; bounded retries; text/json/stream-json all carry the complete final message |
| `tests/pool_socket_e2e.rs` | `--pool-socket` client matrix end-to-end through the compiled CLI (bead claudepr-c7824b71): text/json/stream-json over an acquired worker, stateless fallback for absent and stale sockets, three sequential clients with teardown/replace and zero cross-caller leakage, and three malformed-daemon acquire shapes (close mid-exchange, wrong-shape response, assignment without fd) failing safely within the caller timeout |
| `tests/pool_adversarial_e2e.rs` | Pool concurrency proofs (ADR-005 umbrella claudepr-a03e32d7): concurrent clients each drive a distinct worker with proven session↔worker binding (INV-9, INV-11), and three same-cwd stream-json clients under pool concurrency forward only their own session's events — the end-to-end proof the transcript-guessing defect is dead (claudepr-a927ec0c) |
| `tests/pool_failure_e2e.rs` | Pool failure paths end-to-end against REAL daemons/clients (bead claudepr-c470b8aa): daemon SIGKILLed mid-handoff (protocol failure, not fallback) and SIGSTOPped silent (budget expiry, no leak, recovery), daemon crash mid-drive (client still finishes inside `--timeout`), SIGKILLed client's worker orphaned but never reassigned (INV-9, INV-13), manager restart recovering on the same socket path with ownership-checked cleanup, and stateless-fallback output parity vs no-flag baselines across absent/stale/unavailable sockets in all three formats |
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
| `src/lib.rs` | Crate root — re-exports public modules for integration tests |
| `src/main.rs` | Entry point: CLI parse, claude binary resolution, calls `session::Session::run()` |
| `src/cli.rs` | Clap argument definitions (`Cli`, `OutputFormat`) |
| `src/config.rs` | Loads `$XDG_CONFIG_HOME/claude-print/config.toml` if set, otherwise `~/.config/claude-print/config.toml` (model default, inherit_hooks, max_turns, timeout_secs) |
| `src/session.rs` | Session orchestrator: installs hooks, spawns PTY child, runs event loop, reads transcript. `Session::run()` is the top-level entry point for a single prompt→response cycle; `Session::run_pooled()` drives an already-acquired pool worker through the same event loop, watchdog deadlines, Stop-FIFO handoff, and emitters. |
| `src/pool.rs` | ADR-005 warm PTY pool: `PoolManager`/`PoolServer` (the `serve` daemon — worker spawn, bounded warmup, SCM_RIGHTS fd handoff, SIGTERM→SIGKILL group teardown, ownership-checked socket cleanup) and the `--pool-socket` client (`AcquiredWorker` release-on-drop, acquire classification into fallback vs hard protocol failure, stateless-fallback contract). `MAX_POOL_SIZE` 256; acquire budget `min(60s, --timeout)`. |
| `src/prompt.rs` | Prompt input validation: NUL byte rejection, file size/type checks for `--input-file` (Security T-2, EC-4) |
| `src/verbose.rs` | `--verbose` timing traces: emits `[claude-print <ms>ms] <message>` to stderr across session lifecycle |
| `src/pty.rs` | Forks child, opens PTY pair, calls `login_tty`, unsets `CLAUDE_CODE_SESSION_ID` in child, forwards SIGWINCH/SIGINT |
| `src/startup.rs` | State machine: reads PTY output until trust dialog or idle; auto-dismisses (sends CR), injects prompt via bracketed paste; hard timeout after 45s with <200 bytes |
| `src/event_loop.rs` | Single-threaded `poll(2)` loop (50ms timeout for timer ticks) over PTY master + self-pipe + stop FIFO; calls callback on each chunk |
| `src/hook.rs` | Installs Stop hook via temp dir settings.json; creates FIFO; cleans up on drop |
| `src/poller.rs` | Opens FIFO non-blocking (read + keeper write ends), parses Stop hook payload, derives transcript path from session_id + cwd |
| `src/transcript.rs` | Reads `.jsonl` transcript; extracts last assistant message + token usage |
| `src/emitter.rs` | Formats and writes output (`text`, `json`, `stream-json`); owns the incremental stream-json reader thread (`StreamJsonHandle`) |
| `src/terminal.rs` | Absorbs and discards terminal probe sequences (DA1/DA2/DSR/xtversion) from Ink TUI |
| `src/watchdog.rs` | Watchdog: monitors four deadlines (PTY first-output, stream-json first-output, overall session, Stop-hook) in a background thread; signals timeout via the event-loop self-pipe |
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

6. **Unset `CLAUDE_CODE_SESSION_ID` in child** — the child must not inherit the
   parent's session ID or it will write events into the parent's transcript and may
   skip Stop hook dispatch. Only `CLAUDE_CODE_SESSION_ID` is unset;
   `CLAUDECODE=1` and `CLAUDE_CODE_ENTRYPOINT=cli` must be preserved.

7. **Keep both FIFO ends alive for the full event loop** — `open_fifo_nonblock()`
   returns `(read_fd, keeper_write_fd)`. Both must be stored until after the event
   loop exits. Dropping `read_fd` closes the fd the event loop is polling; dropping
   `keeper` causes `ENXIO` when the hook writes to the FIFO.

## Key implementation notes

- **Event loop ticks on empty slices** — the event loop uses `poll(50ms)` (not
  blocking) and emits an empty-slice tick to the callback on timeout. The callback
  must guard `startup.feed()` and `terminal.feed()` from empty slices — feeding
  empty data resets the idle timer in `StartupSeq`.

- **Watchdog timeout thread is detached, not joined** — `session.rs` spawns the
  `watchdog`'s timeout thread and drops the `JoinHandle` (bound to `_timeout_thread`)
  instead of joining it. The watchdog enforces four deadlines — PTY first-output
  (default 90s), stream-json first-output (default 90s), overall session (default
  3600s), and Stop-hook (default 120s) — and on expiry signals the event loop via
  the self-pipe write fd. Joining would block the main thread for the full deadline
  on early exit; the watchdog thread exits on its own once the child is killed.

- **Stream-json reader cleanup is RAII** — `emitter::StreamJsonHandle::Drop`
  disconnects the drain channel and joins the reader thread, so every return path in
  `Session::run_inner` (success, timeout, signal, child-exit, and `?` propagations)
  joins the reader before returning (plan invariant INV-8). Only the normal Stop path
  calls `signal_drain()` first; error paths drop the handle without signaling.

- **Child cleanup uses `kill_child(pid)`** — `kill_child(pid)` sends SIGTERM, waits
  up to 2s, then SIGKILL. Use this for all child cleanup paths, not bare `waitpid`.

## Pool operations (ADR-005)

The warm PTY pool is opt-in and additive: the ordinary invocation path runs no
pool code unless `--pool-socket` is set. Product-level docs (full flag
descriptions, failure-behavior table, measured startup overhead) live in
README.md §"Warm PTY pool (ADR-005)"; the normative invariant text is plan.md
§Invariants (INV-9 through INV-15). This section is the operator quick
reference.

```bash
# Daemon: keep N workers warm (1–256; 0 or >256 exits 2 before any spawn)
claude-print serve --pool-size 2 --socket /tmp/claude-print-pool.sock --verbose

# Client: ordinary invocation, optionally acquiring a prewarmed worker
claude-print --pool-socket /tmp/claude-print-pool.sock "prompt"
```

Operating limits:

| Limit | Value |
|-------|-------|
| `--pool-size` | 1–256 (`MAX_POOL_SIZE` in `src/pool.rs`; each worker is a full `claude` PTY process) |
| Acquire budget (client) | `min(60s, --timeout)` — bounds the whole connect + request + response + fd transfer; every protocol stage inside it is deadline-bounded |
| Worker warmup | 120 s per worker, enforced on the event-loop timer tick; a timed-out or failed warmup is destroyed and respawned, never handed out |
| Release exchange | 10 s, best-effort — a dead daemon has nothing left to release |
| Socket permissions | 0600 regardless of umask (narrowed umask across the bind, then `set_permissions`) |
| Shutdown | SIGINT/SIGTERM → exit 0; per worker: close PTY master → SIGTERM group → 2 s grace (waitpid-observed) → SIGKILL group → reap |

Failure behavior (the client's ADR-005 contract, `AcquireFailure` in
`src/pool.rs`):

- **Fallback to the stateless session** (exactly one `--verbose` diagnostic,
  identical output/exit code to a no-flag run): socket absent, stale
  (nothing listening), connect refused/timeout, permission denied; and
  well-formed daemon refusals (`pool_full`, `shutting_down`,
  `internal_error`, `acquire_timeout`).
- **Hard error, exit 2, never fallback:** the daemon answered garbage, a
  malformed frame, an incomplete `worker_assigned`, or the fd transfer
  failed — or stayed silent past the acquire budget. The daemon was
  reachable; falling back would mask the breakage behind full-price
  stateless sessions.
- **Mid-drive daemon death:** the client's session is daemon-independent
  once assigned and finishes inside its `--timeout`; the release attempt
  against the dead daemon is bounded and non-fatal.
- **Client killed without destructors:** its worker stays `InUse` forever —
  never reassigned, never destroyed early; reclaimed only at daemon
  shutdown (a daemon-side lease is the deliberate non-fix; see
  `AcquiredWorker`'s doc comment).

Stale-socket handling: a daemon *replaces* whatever sits at its socket path
at bind; at shutdown it removes the node only when it still resolves to the
(dev, ino) it captured at bind time — so a replaced daemon never unlinks the
winner's socket, and a leftover stale node is always safe to `rm` by hand.

Rollback (concrete): (1) remove `--pool-socket` from the invoking config —
for NEEDLE, the `invoke` template in `~/.needle/agents/claude-print.yaml`
(the shipped template does not set it); (2) `kill -TERM` the `serve` process
(clean teardown, exit 0, socket removed). Order does not matter: clients
left pointing at a dead/stale socket fall back statelessly (INV-10). A
binary-level rollback uses the `claude-print.prev` copy `install.sh`
preserves.

Pool invariants (full table with tests in plan.md §Invariants):

- **INV-9** — one request per member: at most one prompt per pooled worker;
  release destroys and replaces, never re-handshakes a used worker.
- **INV-10** — stateless fallback compatibility; reachable-but-broken is a
  hard exit 2, never a silent fallback.
- **INV-11** — no cross-request transcript/session contamination, including
  same-cwd stream-json concurrency (per-drive identity binding).
- **INV-12** — bounded client return: nothing pool-related blocks a client
  past `min(60s, --timeout)`, and mid-drive daemon death never does either.
- **INV-13** — orphan containment: killed client ⇒ worker held `InUse` until
  daemon shutdown.
- **INV-14** — cleanup: no leaked PTY master fds, no survivors, no zombies;
  socket removed only when still ours.
- **INV-15** — pooled sessions bill `cc_entrypoint=cli` (AS-4 on the pool
  path); verify with `CLAUDE_PRINT_POOL=1 ./scripts/billing-canary.sh`.

The startup-overhead benchmark (`scripts/bench_startup_overhead.py`, results
in `docs/notes/startup-overhead-benchmark.{md,json}`) measures process start
→ prompt injection under the `mock-claude` fixture only. It isolates
`claude-print`'s own overhead; it establishes **no model-latency savings**.

## Bead workflow

Beads use the **bead-rs `bead` CLI** — canonical across this environment since
2026-08-14. The backend is declared in `.needle.yaml` (`bead_cli: backend:
bead-rs`); the live store is SQLite at `.beads/beads.db` with a git-tracked
durable checkpoint under `.beads/checkpoint/`; bead IDs use the `claudepr`
prefix (workspace identity in `.beads/config.json`).

> **Never run `bf` (bead-forge) against this workspace — `bf` is retired and
> not installed on this box.** Running the wrong CLI does not fail cleanly:
> `bf` reports a generic SQLite "no such column" error rather than "wrong
> tool," and applying the *other* tool's recovery recipe to that error
> silently reinitializes the store with the wrong schema and destroys the
> live data. The on-disk tell is unambiguous here: `.beads/config.json` +
> `.beads/checkpoint/` = bead-rs; a `.beads/config.yaml` + flat
> `.beads/issues.jsonl` would mean bf (this repo has neither). If a bead
> command fails with an unfamiliar schema/column error, stop and re-check the
> backend declaration before attempting any repair.

```bash
# List beads (all / ready frontier only)
bead list
bead list --ready

# Show one bead
bead show <id>            # claudepr-… IDs only — historical bf-* IDs no longer resolve

# Record progress / verification evidence on a bead
bead update <id> --notes "..."

# Atomically claim from the ready frontier (or claim for a named worker)
bead claim
bead claim --assignee <worker>

# Close — non-empty --reason is required (status can't be closed via update).
# Commit the work first; NEEDLE re-verifies close evidence against committed state.
bead close <id> --reason "..."
```

Checkpoint sync and recovery:

```bash
# Database -> checkpoint (idempotent; bead 0.2.x also auto-publishes the
# checkpoint after every successful mutation)
bead sync flush-only

# If beads.db is missing/corrupt/wrong-schema (fresh clone, or someone ran the
# wrong CLI): diagnose read-only first, then rebuild losslessly from the
# git-tracked checkpoint — never by deleting beads.db and re-importing with a
# bf-shaped command (see the warning above).
bead doctor                    # read-only; --repair adds non-destructive fixes only
bead init                      # rebuild schema, keeps committed workspace identity
bead sync import-only --input .beads/checkpoint/forensic.jsonl \
  --restore-into-empty --actor <you>
```

Historical note: code comments, parts of `docs/plan/plan.md`, and everything
under `notes/` still reference `bf-*` bead IDs from the bead-forge era. Those
IDs are provenance, not pointers — they predate the 2026-08-14 bead-rs
migration and do not resolve in this store (`bead show bf-3isy` → "Issue not
found"). Do not rewrite them, and don't try to look them up.

See the **"Beads (bead-rs CLI)"** section of the root workspace `CLAUDE.md`
for the full `bead` CLI reference and gotchas (`bead reopen` clears the
assignee; `bead release` refuses assigned-but-open beads; `--if-revision N`
for optimistic concurrency on worker-contested beads).

## Notes

`notes/` holds per-bead NEEDLE worker scratch notes — one file per bead,
named after the bead's ID (`notes/claudepr-*.md`; the existing `bf-*.md`
files are historical artifacts of the bead-forge era, kept for traceability).
These are worker journals, **not** product documentation; they are deliberately
tracked (kept simple — history is appended, never rewritten). Routine status
and verification evidence belongs on the bead itself (`bead update --notes`,
`bead close --reason`) — never create a notes file just to have a commit
artifact. Product design lives in `docs/plan/plan.md`.
