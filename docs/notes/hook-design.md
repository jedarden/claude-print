# Hook Design

The Stop hook is the IPC mechanism between Claude Code and `claude-print`. When the AI completes a turn, Claude Code fires the Stop hook with a JSON payload containing session metadata; `claude-print` reads this from a FIFO to know when the response is ready.

## Temp Directory Structure

Each `claude-print` invocation creates a per-run temp directory:

```
<TMPDIR>/claude-print-<pid>-<rand>/
├── settings.json    # Relay Stop hook configuration
├── hook.sh          # Hook script executed by Claude Code
└── stop.fifo        # Named pipe for IPC
```

The temp dir is created with mode `0700` (owner-only) to prevent local users from reading the Stop payload (session ID, prompt text).

## Relay Hook

The relay hook is a minimal Claude Code Stop hook that writes the Stop payload to the FIFO:

```sh
#!/bin/sh
cat > '<fifo-path>' 2>/dev/null || true
```

Key points:
- The FIFO path is embedded as a shell single-quoted string — no variable expansion at execution time
- If FIFO write fails, the hook exits cleanly (`|| true`); Claude Code does not wait beyond the 10s timeout
- The hook is executed by Claude Code via `--settings <temp>/settings.json`

## settings.json

The per-run settings file contains only the Stop relay hook:

```json
{
  "hooks": {
    "Stop": [{
      "hooks": [{"type": "command", "command": "<temp>/hook.sh", "timeout": 10}]
    }]
  }
}
```

Claude Code merges this with any user hooks from `~/.claude/settings.json`, so both user hooks and the relay hook fire. **Measured on claude 2.1.270 (2026-09-13, `docs/notes/claude-contract-probes.md`):** merge confirmed for user- and project-source hooks alongside the relay hook — every loaded source fires on each hook event. Cross-source firing *order* is not contractual: standard-source hooks typically start first (1–3 ms ahead), but concurrent firing — and a run where the relay started before the project hook — was also observed. Nothing in claude-print may depend on the relay firing last; the payload may arrive while user Stop hooks are still running, which is safe because the two consumers are independent.

## FIFO Protocol

The FIFO is a POSIX named pipe created with `mkfifo(path, mode=0600)`:

- **Writer**: Claude Code executes `hook.sh`, which opens the FIFO for writing and writes the JSON payload
- **Reader**: `claude-print` opens the FIFO for reading and blocks until data is available

### Stop Hook Payload

All fields are optional for forward compatibility:

```json
{
  "hook_event_name": "Stop",
  "session_id": "abc123",
  "transcript_path": "/home/user/.claude/projects/home-user-myproject/abc123.jsonl",
  "last_assistant_message": "Response text...",
  "cwd": "/home/user/myproject"
}
```

### Transcript Path Derivation

If `transcript_path` is absent from the payload, it is derived from `session_id` and `cwd`:

```
<HOME>/.claude/projects/<slug>/<session_id>.jsonl
```

Where `<slug>` is the `cwd` with leading `/` stripped and remaining `/` replaced with `-`:

```
/home/user/myproject → home-user-myproject
/tmp → tmp
```

### Stop Firing Frequency

**Measured on claude 2.1.270** (full evidence: `docs/notes/claude-contract-probes.md`): Claude Code fires Stop **once per completed turn** — a multi-round tool-using turn produces exactly one Stop at its end, and each new user prompt in a TUI session produces its own Stop. A run cut off by `--max-turns` fires no Stop at all (it exits with an error; the `--stop-hook-timeout` watchdog owns that case). Since claude-print sends exactly one prompt per session, the first Stop payload it reads from the FIFO is the terminal signal.

Known hazard on degraded runs: when the model's tool calls are permission-**denied** (no allowlist, headless), one measured run produced an extra Stop firing. This is not reachable in the NEEDLE fleet (`claude-print.yaml` passes `--dangerously-skip-permissions`), and the single-fire poller degrades gracefully there — it acts on the first payload, the transcript retry/fallback path absorbs the rest, and the watchdog still bounds the session. The measured counts are pinned in `tests/fixtures/claude_contracts_v2.1.270.json`; the merge and suppression contracts are re-measured live by `cargo test --test claude_contracts -- --ignored`, and the Stop-count probes are re-runnable via `scripts/probe-stop-toolallowed.sh` after any Claude Code update. The degraded-run tolerance itself is pinned by regression tests: `tests/stop_poller.rs` delivers duplicate/spurious payloads coalesced through the FIFO and asserts first-payload-wins under a single fire, and `tests/stop_duplicate_firings_e2e.rs` drives full sessions via mock-claude's `MOCK_EXTRA_STOPS` knob (duplicate re-fire + spurious phantom turn) asserting exactly one result, a clean exit, and no double emission.

## Keeper FD Pattern

Linux FIFO semantics: `open(O_WRONLY|O_NONBLOCK)` returns `ENXIO` if no reader is present. To prevent the hook's `cat > fifo` from blocking or failing, `claude-print` uses a **keeper write-end fd**:

1. Open FIFO read-end `O_RDONLY|O_NONBLOCK` — always succeeds immediately
2. Open keeper write-end `O_WRONLY|O_NONBLOCK` — succeeds because read-end is now open
3. Hold keeper write-end open until Stop fires
4. When Stop fires, read the payload, then close the keeper write-end
5. Claude Code's `cat > fifo` opens its own write-end (simultaneous write-ends are valid)

### Cleanup on Non-Stop Exit Paths

On exit paths where Stop never fires (SIGINT, timeout, child exit), the keeper write-end **must be explicitly closed** before `waitpid`:

- Closing the keeper causes any pending `cat > fifo` in `hook.sh` to receive `EPIPE`/`ENXIO` and exit
- Without this, the hook runner would hang indefinitely waiting for a reader that will never come

## FIFO Poller States

```
UNOPENED
  │  opened O_NONBLOCK at TRUST_DISMISSED → PROMPT_INJECTED transition
  ▼
OPEN_WAITING
  │  FIFO becomes readable (Stop hook wrote payload)
  ▼
PAYLOAD_READ → DONE
```

The FIFO read-end is opened **before** the bracketed paste is injected (at the `TRUST_DISMISSED → PROMPT_INJECTED` transition), so Stop cannot fire before the reader is ready.

## Hook Inheritance

### Default Mode (inherit hooks)

By default, `claude-print` does not redirect `CLAUDE_CONFIG_DIR`. The inner `claude` process:

- Writes transcripts to `~/.claude/projects/<cwd-slug>/<session-id>.jsonl`
- Writes session entry to `~/.claude/sessions/<pid>.json`
- Appends to `~/.claude/history.jsonl`
- Fires all hooks in `~/.claude/settings.json` alongside the relay hook

### Isolation Mode (--no-inherit-hooks)

When `--no-inherit-hooks` is passed:

- `--setting-sources=` (empty) is forwarded to claude — suppresses loading of standard settings sources (**measured** on claude 2.1.270: the empty spelling is accepted, suppresses every standard source, and does not suppress the `--settings` file; the alternative spelling `=none` is rejected outright — exit 1 before session start)
- Only `--settings <temp>/settings.json` is active — contains solely the relay hook
- User hooks (SessionStart, Stop, PreToolUse, ccdash, trail-boss, etc.) do not fire

Use this mode for NEEDLE workers to prevent hook noise, or when user hooks have side effects.

## Cleanup

The temp directory is cleaned up on all exit paths via `tempfile::TempDir` drop:

- Normal exit: Stop fires, payload read, cleanup completes
- Timeout: keeper write-end closed, child SIGTERM'd, cleanup runs
- SIGINT: interrupted flag set, poll loop breaks, cleanup runs
- Panic: `TempDir` destructor runs during unwind

Cleanup is idempotent — can be called multiple times safely. The FIFO is removed before the directory to avoid permission issues.
