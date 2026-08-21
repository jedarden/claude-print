# claude-print Plan

## Overview

Single Rust binary that is a drop-in replacement for `claude -p`. It drives the Claude Code interactive TUI via PTY, extracts the response via the Stop hook and JSONL transcript, and emits `claude -p`-compatible output — all while billing against the subscription (`cc_entrypoint=cli`) rather than the Agent SDK credit pool.

## Background

Starting June 15, 2026, Anthropic separates `claude -p` (headless) into a separate monthly credit pool. Only the interactive TUI (`cc_entrypoint=cli`) continues drawing from the unlimited subscription. `claude-print` wraps the TUI in a PTY so callers get `claude -p` wire-compatible output while billing against the subscription.

The billing classification is determined by `isatty(stdout)` inside the `claude` binary at startup:
- PTY slave as stdout → `isatty()` returns true → TUI mode → `cc_entrypoint=cli` → subscription
- Pipe as stdout → `isatty()` returns false → print mode → `cc_entrypoint=sdk-cli` → credit pool

## Glossary

| Term | Definition |
|------|-----------|
| PTY | Pseudoterminal: a master/slave fd pair where `isatty()` returns true on the slave. Allows a parent process to control a child process's terminal I/O through the kernel line discipline. |
| cc_entrypoint | Anthropic billing header field. `cli` = subscription pool; `sdk-cli` = Agent SDK credit pool. Determined at Claude Code startup by `isatty(stdout)`. |
| Stop hook | A Claude Code hook event fired when the AI completes a turn. Payload includes `session_id`, `transcript_path`, and `last_assistant_message`. Used as the IPC signal between the inner `claude` process and `claude-print`. (Note: in `claude -p`-style single-turn sessions, Stop fires once at session end. With `--max-turns > 1` and tool use, Stop behavior is unverified — add to OQ-1 resolution checklist. The Stop Poller assumes single-fire per session; if multi-fire is observed, the poller must be updated to match on the JSONL `Result` event before acting.) |
| FIFO | POSIX named pipe (`mkfifo`). The Stop hook writes to it; the parent poll loop reads from it. Per-run, per-pid — prevents cross-invocation contamination. |
| Bracketed paste | Terminal feature that wraps pasted text in `ESC[200~` … `ESC[201~` markers. Prevents embedded newlines from triggering premature Enter in Ink's REPL. |
| Ink | The React/Yoga-based TUI framework used by Claude Code. Sends DEC terminal probes (DA1, DA2, DSR, XTVERSION, window-size) at startup and hangs indefinitely if unanswered. |
| login_tty | glibc function: `setsid()` + `ioctl(TIOCSCTTY)` + `dup2(slave, 0/1/2)` + `close(slave)`. Makes the PTY slave the controlling terminal for the child process. |
| JSONL transcript | Newline-delimited JSON at `~/.claude/projects/<cwd-slug>/<session-id>.jsonl`. Claude Code appends one event per line as the session progresses. The `<cwd-slug>` is derived by stripping the leading `/` and replacing remaining `/` with `-`. (Note: paths containing hyphens in directory names produce ambiguous slugs; `session_id` resolves the file within the directory.) |
| usage-fingerprint | Tuple of `(input_tokens, output_tokens, cache_creation_input_tokens, cache_read_input_tokens)` used to deduplicate streaming JSONL events from the same API call when `message.id` is absent. |
| stream-json | Output format where each transcript event line is forwarded to stdout as Claude Code writes it, providing real-time streaming compatible with `claude -p --output-format stream-json`. |
| mock_claude | Compiled Rust binary (`test-fixtures/mock-claude/`) simulating Claude Code's PTY and JSONL behavior. Controlled via env vars — not a shell script. |
| NEEDLE | LLM fleet runner that dispatches AI agents to code workspaces. `claude-print.yaml` configures NEEDLE to use `claude-print` instead of `claude -p`. |

## Non-Goals

The following are explicitly out of scope with rationale:

| Non-Goal | Rationale |
|----------|-----------|
| Windows support | PTY (`openpty`, `login_tty`) is POSIX-only. The target platform is x86_64 Linux (musl). Adding Windows requires ConPTY — a fundamentally different approach not needed for the server/NEEDLE use case. |
| macOS / ARM Linux | Initial target is `x86_64-unknown-linux-musl`. Can be added in a future release if needed. |
| Response caching | Caching belongs at a higher layer (e.g., the NEEDLE dispatcher). Adding it here would complicate billing accounting and break the stateless design. |
| Multi-turn interactive sessions | `claude-print` handles one prompt → one response per invocation, mirroring `claude -p` semantics. Session management is the caller's responsibility. |
| GUI or web interface | Output format is stdin/stdout. No web server, no gRPC, no REST. |
| Rate-limit retry | Rate limits surface as exit 1. Retry logic belongs in the caller or NEEDLE. |
| Streaming response reassembly | `stream-json` forwards raw JSONL lines as-is. No custom streaming protocol or chunk reassembly. |
| Model-name validation | `--model` is forwarded verbatim to `claude`. If the model name is invalid, `claude` rejects it. |

## Hard Requirements

These MUST hold. Any design that violates them is invalid.

1. **MUST produce a single statically-linked binary** — no shared library dependencies, no Python, no Node, no scripts at runtime.
2. **MUST set `cc_entrypoint=cli`** — every invocation MUST bill against the subscription pool. This is the core correctness invariant.
3. **MUST be a drop-in replacement for `claude -p`** — positional prompt, stdin, `--input-file`, `--output-format text/json/stream-json`, `--model`, `--max-turns`, and all five exit codes MUST be compatible.
4. **MUST NOT redirect `CLAUDE_CONFIG_DIR`** — transcripts MUST land in `~/.claude/projects/` exactly as `claude -p` writes them.
5. **MUST NOT break user hooks in default mode** — all hooks in `~/.claude/settings.json` MUST fire alongside the relay hook.
6. **MUST survive Claude Code version updates** — unknown JSONL fields, event types, and escape sequences MUST be silently tolerated without a binary rebuild.
7. **MUST clean up temp dir on all exit paths** — no leftover `claude-print-*` directories in `$TMPDIR` after normal exit, timeout, SIGINT, or panic.
8. **MUST forward SIGINT to child** — Ctrl-C MUST reach the inner `claude` process.

## What It Is Not

- Not a general-purpose PTY wrapper (not `script(1)` or `tmux`).
- Not a Claude Code plugin — it runs `claude` as a subprocess.
- Not a billing bypass — it uses the interactive TUI as designed; it does not spoof headers.
- Not a session manager — no state persists between invocations.
- Not aware of multi-turn conversation history — each invocation is independent.
- Not a streaming proxy — `stream-json` forwards raw JSONL, not a custom protocol.

## Scope Lock

Any feature not listed in the Components section is out of scope for v1.0. To add a feature it MUST (1) solve a documented problem that `claude -p` compatibility cannot address, (2) not require changes to the PTY event loop's core state machine, and (3) not add a runtime dependency. Features violating the musl static binary requirement are permanently out of scope.

## Normative Language

This document uses RFC-2119 conventions: **MUST** = required, **MUST NOT** = prohibited, **SHOULD** = recommended, **MAY** = optional.

## Delivery

**Single statically-linked binary.** No Python, no runtime dependencies, no pip packages.

```
claude-print          # the binary (musl static)
mock_claude           # test fixture binary (musl static, installed by install.sh)
claude-print.yaml     # NEEDLE agent config
install.sh            # installs all of the above to ~/.local/bin/ and ~/.needle/agents/
```

Built with:
```bash
cargo build --release --target x86_64-unknown-linux-musl   # fully static, no libc dep
```

Distribution: GitHub Release artifact via `claude-print-ci` Argo WorkflowTemplate (same pattern as NEEDLE, SIGIL, ARMOR).

## Acceptance Scenarios

Named scenarios that define correct system behavior. Pass/fail criteria are testable without credentials unless noted.

### AS-1: Shell Script Caller (Happy Path)
**Action:** `echo "What is 2+2?" | claude-print`
**Pass:** exit 0; stdout contains a non-empty text response; `~/.claude/projects/` gains a new JSONL file.
**Fail:** any non-zero exit, empty stdout, or stdout contains JSON syntax.

### AS-2: JSON Consumer
**Action:** `claude-print --output-format json "What is the capital of France?"`
**Pass:** exit 0; stdout is a single valid JSON object with `type=result`, `is_error=false`, `result` non-empty, `usage.input_tokens > 0`, `claude_version` present.
**Fail:** invalid JSON, missing required field, `is_error=true`.

### AS-3: NEEDLE Worker
**Action:** NEEDLE dispatches a bead with `claude-print.yaml` agent.
**Pass:** exit 0; JSON output contains a valid UUID `session_id`; transcript appears in `~/.claude/projects/<workspace-slug>/`; `--no-inherit-hooks` suppresses user hooks.
**Fail:** NEEDLE cannot parse output; `session_id` absent; exit non-zero.

### AS-4: Billing Classification
**Action:** A daily systemd user timer runs one minimal claude-print invocation, resolves the resulting transcript by returned `session_id`, and passes that exact JSONL to `scripts/check-billing.sh`. Run the original newest-transcript check manually before each release as a second layer.
**Pass:** The exact canary transcript contains a line with `"entrypoint": "cli"`; the canary exits 0 and atomically writes a fresh `PASS` result file.
**Fail:** `entrypoint` is `"sdk-cli"` or absent.
*(Credential-required; automated daily on production hosts and run manually before each release.)*

### AS-5: Error Surface — `claude` Not Found
**Action:** `PATH= claude-print "hello"` (or `--claude-binary /nonexistent`).
**Pass:** exit 2; stderr contains a human-readable error naming the missing binary; `--output-format json` output has `is_error=true`, `subtype=internal_error`.
**Fail:** exit 0 or process hangs.

### AS-6: Degraded Path — Transcript Race
**Action:** Integration test with `mock_claude MOCK_DELAY_JSONL=150`.
**Pass:** retry loop fires (visible in `--verbose`); response extracted correctly; exit 0.
**Fail:** exit non-zero or empty response.

## Success Metrics

**Functionality:** AS-1 through AS-6 all pass on every commit; credential-backed AS-4 passes daily on production hosts and manually before every release; all mock integration scenarios (at minimum, the scenarios listed in the integration test table) exit with expected codes.

**Performance:** `claude-print` overhead (invocation to prompt injection) < 5 s on a cold start; transcript reader produces output within 2 s of Stop hook firing; binary size < 10 MB.

**Adoption:** NEEDLE workers using `claude-print.yaml` produce zero billing-classification failures; `claude --version` changes do not require a claude-print rebuild within 30 days of a Claude Code release.

## Architecture

```
caller
  │  prompt (stdin, arg, or --input-file)
  ▼
claude-print (single Rust binary)
  ├── CLI parser       flags forwarded to claude subprocess (clap)
  ├── Hook installer   per-run temp dir: settings.json + hook.sh + stop.fifo
  ├── PTY spawner      nix::pty::openpty() + fork() + login_tty()
  ├── Event loop       poll() on master_fd; dispatches to:
  │     ├── Terminal emu   responds to DA1/DA2/DSR/XTVERSION/window-size probes
  │     ├── Startup seq    phase 1: trust dismiss  phase 2: bracketed-paste inject
  │     └── FIFO poller    blocks on stop.fifo until Stop hook fires
  ├── Transcript rdr   JSONL parse → final text + token counts (retry loop)
  ├── Emitter          text / json / stream-json to stdout
  └── Cleanup          FIFO, temp dir, master_fd, waitpid
```

## Module Layout

```
claude-print/
├── Cargo.toml                        # workspace root; declares `test-fixtures/mock-claude` as a workspace member so `cargo build` compiles `mock_claude`
├── Cargo.lock
├── build.rs                          # build script for version info
├── install.sh
├── claude-print.yaml                 # NEEDLE agent config
├── claude-print-ci-workflowtemplate.yml    # Argo WorkflowTemplate for CI/CD
├── claude-print-ci-sensor.yml        # Event sensor for CI workflow triggers
├── claude-print-eventsource-stanza.yml     # EventSource stanza for workflow events
├── src/
│   ├── main.rs                       # entry point: parse args, orchestrate
│   ├── cli.rs                        # clap CLI struct + validation
│   ├── config.rs                     # ~/.config/claude-print/config.toml loader
│   ├── hook.rs                       # HookInstaller: temp dir, settings.json, hook.sh, mkfifo
│   ├── pty.rs                        # PTY spawner: openpty, fork, login_tty, winsize
│   ├── event_loop.rs                 # poll() loop: dispatch to terminal/startup/fifo
│   ├── terminal.rs                   # TerminalEmu: probe scanner, response table, dedup bitmask
│   ├── startup.rs                    # StartupSeq: trust dismiss, bracketed paste injection
│   ├── transcript.rs                 # JSONL parser, usage dedup, text extraction, retry loop
│   ├── emitter.rs                    # Output formatter: text/json/stream-json
│   ├── error.rs                      # ClaudePrintError enum, exit code mapping
│   ├── check.rs                      # --check subcommand: installation self-test
│   ├── lib.rs                        # library exports for testing
│   ├── poller.rs                     # stop.fifo poller: IPC read from Stop hook
│   ├── prompt.rs                     # prompt input validation (NUL byte check, file size limits)
│   ├── pool.rs                       # optional warm PTY pool and Unix-socket protocol (ADR-005)
│   ├── session.rs                    # Session: main orchestration flow (prompt → response)
│   ├── util.rs                       # shared HOME resolution and path helpers
│   ├── verbose.rs                    # --verbose timing traces to stderr
│   └── watchdog.rs                  # Watchdog: no-output/max-turn/stop-hook/stream-json timeout monitoring
├── tests/
│   ├── billing_canary.rs             # billing-canary script integration tests
│   ├── cli.rs
│   ├── config_error_helpers.rs       # shared helpers for config-error integration tests
│   ├── config_startup_errors.rs       # CLI startup behavior for invalid config files
│   ├── terminal.rs
│   ├── transcript.rs
│   ├── emitter.rs
│   ├── home_unset.rs                 # strict HOME-resolution contract tests
│   ├── startup.rs
│   ├── version_compat.rs
│   ├── hooks.rs                      # hook inheritance tests
│   ├── integration.rs                # integration test module entry point
│   ├── nested_session.rs             # inherited Claude session-variable regression tests
│   ├── pty_integration.rs            # PTY-specific integration tests
│   ├── stdin_limit.rs                # stdin prompt-size limit tests
│   ├── stop_poller.rs                # stop.fifo polling tests
│   ├── watchdog.rs                   # watchdog timeout tests
│   ├── binary_e2e.rs                 # binary-level E2E: AS-1/AS-2/AS-5 + stream-json + no-prompt exit 4 (compiled claude-print + mock-claude subprocess)
│   ├── stream_json_cleanup.rs        # stream-json reader cleanup on every exit path (INV-8)
│   ├── stream_json_incremental.rs    # incremental (real-time) stream-json E2E test (bf-5xw); #[ignore]'d pending bf-3isy + bf-5vm
│   ├── transcript_race_e2e.rs        # AS-6 Stop-before-JSONL-flush race E2E; separate test binary to isolate MOCK_* env (bf-3isy)
│   ├── integration/
│   │   └── scenarios.rs              # 20+ mock PTY integration tests (declared via `mod scenarios;` in integration.rs; no mod.rs)
│   └── fixtures/
│       └── transcript_v2.1.168.jsonl
├── scripts/
│   ├── check-billing.sh              # AS-4 billing conformance verification
│   ├── billing-canary.sh              # Daily AS-4 canary (exact transcript by session id)
│   ├── install-billing-canary.sh      # Installs and enables the systemd user timer
│   ├── claude-print-billing-canary.service
│   ├── claude-print-billing-canary.timer
│   ├── test_exact_claude_print_scenario.sh
│   ├── test_sessionstart_hook.sh
│   ├── test_startup_wedge.sh
│   ├── verify_fix.sh
│   └── verify-startup-wedge-fix.sh
├── test-fixtures/
│   └── mock-claude/
│       ├── Cargo.toml
│       └── src/
│           └── main.rs
└── notes/                            # per-bead NEEDLE worker scratch notes (notes/bf-*.md); tracked for history, not product docs
```

## State Machine

Two orthogonal state machines run inside the event loop.

### StartupSeq States

```
WAITING
  │  trust keywords found in PTY line
  │  OR (bytes_received ≥ 200 AND PTY quiet ≥ 0.4 s)
  ▼
TRUST_DISMISSED   ← CR sent
  │  PTY quiet ≥ 1.0 s (window restarts on every PTY chunk)
  ▼
PROMPT_INJECTED   ← bracketed paste sent; FIFO read-end opened
  │  FIFO becomes readable (Stop hook fired)
  ▼
DONE

From any state:
  wall-clock timeout     → SIGTERM child → exit 124
  child exits unexpectedly → exit 2
  SIGINT                 → SIGINT child (per HR-8) → exit 130
  Stop fires before PROMPT_INJECTED → error: emit is_error=true, exit 2 (see EC-7: a response to an unsent prompt indicates a session identity leak; EC-11 prevents this in normal operation)
```

Guard conditions:
- `WAITING → TRUST_DISMISSED`: **either** trust keywords OR the idle/byte threshold. Not both required. One-shot: once the WAITING → TRUST_DISMISSED transition occurs for any reason (keyword or idle), the idle fallback is deactivated.
- `TRUST_DISMISSED → PROMPT_INJECTED`: the quiet window starts at the CR write and restarts on every subsequent PTY output chunk, so buffered redraw output delays rather than races prompt injection.
- FIFO read end opened at the `TRUST_DISMISSED → PROMPT_INJECTED` transition, **before** the bracketed paste is written (EC-3).

### FIFO Poller States

```
UNOPENED
  │  opened O_NONBLOCK at TRUST_DISMISSED → PROMPT_INJECTED transition
  ▼
OPEN_WAITING
  │  FIFO becomes readable (Stop hook wrote payload)
  ▼
PAYLOAD_READ → DONE
```

**FIFO open mechanics:** Opening O_RDONLY|O_NONBLOCK on a named FIFO returns ENXIO if no writer holds the write end. To prevent this, `claude-print` opens a "keeper" write-end fd O_WRONLY|O_NONBLOCK on the same FIFO and holds it open until Stop fires. This guarantees the read-end open succeeds (write end is always held). When Stop fires and the payload is read, the keeper write-end fd is closed. The `hook.sh` write (`cat > '<fifo>'`) opens a second write end and writes the payload — both write-end opens are valid simultaneously. On all other exit paths (SIGINT, timeout, child-exit-before-Stop), the keeper write-end fd MUST be explicitly closed before `waitpid` — this causes any pending `cat > '<fifo>'` in `hook.sh` to receive EPIPE/ENXIO and exit, preventing a hang in claude's hook runner.

## Concurrency Model

`claude-print` is **single-threaded** except for `stream-json` mode.

### Default and `json` mode

All work runs on the main thread: `fork()`, `poll()` event loop, transcript reading, output. No shared mutable state. No locks.

### `stream-json` mode

A reader thread is spawned at `PROMPT_INJECTED`:

```
Main thread                          Reader thread
─────────────────────────────────    ──────────────────────────────────
poll() loop (master_fd, stop_fifo)   tail transcript from prompt_injected_at
  │                                    byte offset — captured as file.seek(End)
  │                                    on the transcript file at the moment the
  │                                    bracketed paste is written. The reader
  │                                    thread reads from this byte offset forward,
  │                                    so pre-injection events (SessionStart,
  │                                    system messages) are not forwarded to stdout.
  │                                    If the transcript file does not exist at
  │                                    prompt injection time (claude has not yet
  │                                    written the first event), the reader thread
  │                                    MUST retry the file open in a loop with 50ms
  │                                    sleeps until the file appears or a 5-second
  │                                    timeout expires. If the 5-second timeout
  │                                    expires, the reader thread MUST send the
  │                                    drain signal on the mpsc channel (same as
  │                                    normal Stop) before returning, so the main
  │                                    thread's `Receiver::recv()` returns promptly.
  │                                    The main thread then emits an error result
  │                                    (`is_error: true`, `subtype: 'internal_error'`,
  │                                    `error_message: 'transcript file did not appear
  │                                    within 5s'`) and exits 2. This is the same race
  │                                    condition handled by the normal transcript
  │                                    reader's retry loop, applied here to the
  │                                    file-open step rather than the content-read
  │                                    step.
  │                                    write each new line → stdout
Stop fires                           via mpsc::channel unbounded sender
  │
mpsc drain_signal sent              drain remaining lines, thread exits
  │
join reader thread
  │
emit exit code
```

Synchronization: one-shot `std::sync::mpsc::channel`. Reader owns the transcript file handle (no sharing). Reader thread MUST be joined before `main()` returns on all exit paths — including timeout and SIGINT paths (the SIGINT handler sets a flag that breaks the poll loop, which then joins the thread before calling `process::exit`).

**Non-Stop exit paths (SIGINT, timeout):** The reader thread MUST also exit on these paths. Mechanism: the reader thread holds the mpsc `Receiver`; the main thread holds the `Sender`. On SIGINT or timeout, the main thread **drops the Sender** (without sending a value). The receiver's `recv()` or `try_recv()` then returns `Err(RecvError)`, which the reader thread treats as a shutdown signal — it exits its tail loop and returns. This means join() returns promptly on all exit paths. The reader thread drain logic: on `Ok(())` from recv = drain_signal; on `Err` = immediate exit without draining.

The reader thread handle is stored as `Option<JoinHandle<()>>`, initialized to `None`. The `Option` is set to `Some(handle)` only at the `PROMPT_INJECTED` transition when the thread is spawned. On any exit path — including early exits before `PROMPT_INJECTED` — the join is conditional: `if let Some(h) = reader_handle { h.join().ok(); }`

## Cross-Cutting Concerns

### Error Propagation

`error.rs` defines `ClaudePrintError` with an exit code per variant. All errors route through the Emitter, so `--output-format json` callers always receive a structured error object, never bare stderr.

```rust
pub enum ClaudePrintError {
    Setup(String),           // exit 2
    Timeout,                 // exit 124
    Interrupted,             // exit 130
    AssistantError(String),  // exit 1
}
```

Variant-to-JSON mapping:

| Variant | JSON subtype | Exit code |
|---------|-------------|-----------|
| Setup(_) | "internal_error" | 2 |
| Timeout | "timeout" | 124 |
| Interrupted | "interrupted" | 130 |
| AssistantError(_) | "assistant_error" | 1 |

### `--verbose` Trace Points

Written to stderr, timestamped `[claude-print <ms>ms] <message>`. Never to stdout. Trace points (in order): temp dir created, PTY opened, child forked (pid), phase transitions, FIFO opened, prompt injected, Stop received (session_id), retry count, cleanup reason.

### Signal Handling

| Signal | Handler | Action |
|--------|---------|--------|
| SIGINT | installed before fork | SIGINT child (forwarding the signal as required by HR-8); set `interrupted` flag; poll loop breaks; join reader thread (if any); emit exit 130 |
| SIGTERM | installed before fork — mirrors SIGINT handler | SIGTERM child (per HR-8 mirror); sets `interrupted` flag; writes to self-pipe; poll loop breaks; join reader thread; exit 130 (same as SIGINT via `Interrupted` variant); allowing normal cleanup and TempDir drop before exit. SIGTERM is handled the same as SIGINT — not a dirty kill. This guarantees INV-1 and INV-2 hold on SIGTERM. |
| SIGPIPE | ignored | stdout pipe may close early in stream-json mode |

**Signal handler safety:** The `interrupted` flag MUST be `std::sync::atomic::AtomicBool` with `store(true, Ordering::SeqCst)`. Calling `kill(2)` from a signal handler is async-signal-safe on Linux. The `AtomicBool::store` is also safe from signal handlers. To wake a blocked `poll()` call, use a self-pipe: before `fork()`, create a `pipe(2)` pair; add the read-end to the pollfd array; the SIGINT/SIGTERM handler writes one byte to the write-end. The `poll()` loop checks the self-pipe read-end and the `AtomicBool` on each wake.

### Temp Dir Cleanup

`tempfile::TempDir` is stored in `main()` scope (not nested in a struct). Drop on any exit path — including panics — calls `remove_dir_all`. The SIGINT handler does not directly clean up; it breaks the poll loop which returns control to `main()` where `TempDir` drops normally.

### Log Boundary

`claude-print` writes NO files to `~/.claude/`. All artifacts there are written by the inner `claude` process. `claude-print` only reads `~/.claude/projects/<slug>/<session-id>.jsonl` after Stop fires.

## Hook Inheritance and Log Placement

### Default: Inherit User Hooks

By default `claude-print` does **not** redirect `CLAUDE_CONFIG_DIR`. The inner `claude` process:
- Writes its transcript to `~/.claude/projects/<cwd-slug>/<session-id>.jsonl` directly — the same place `claude -p` writes it
- Writes its session entry to `~/.claude/sessions/<pid>.json` (ccdash sees it as a normal session)
- Appends to `~/.claude/history.jsonl`
- Fires all hooks in `~/.claude/settings.json` (SessionStart, Stop, PreToolUse, trail-boss, ccdash, etc.)

`claude-print` adds its own Stop hook by passing `--settings <temp>/settings.json` with the per-run relay hook. Claude Code merges `--settings` with the user's settings file — all existing hooks continue to fire alongside the relay hook (merge behavior per OQ-1, unverified; see Hook Installer §2 schema note and PO-1 for fallback if merge fails).

This matches exactly what `claude -p` does. Transcripts, token counts, and usage stats land in `~/.claude/` with no special handling.

### `--no-inherit-hooks` (Isolation Mode)

When `--no-inherit-hooks` is passed:
- `--setting-sources=` is forwarded to claude (empty value = load no standard settings sources)
- Only `--settings <temp>/settings.json` is loaded, which contains solely the Stop relay hook
- User's `~/.claude/settings.json` hooks do not fire (ccdash, trail-boss, etc.)
- `CLAUDE_CONFIG_DIR` is **not** set even in isolation mode — transcripts still land in `~/.claude/projects/`

Use this when running as a NEEDLE worker to prevent hook noise, or when the user's hooks have side effects (e.g., trail-boss POSTs to a collector that doesn't expect headless sessions).

### Configuration File

`$XDG_CONFIG_HOME/claude-print/config.toml` if `$XDG_CONFIG_HOME` is set, otherwise `~/.config/claude-print/config.toml`. Created with defaults on first run.

```toml
[defaults]
inherit_hooks = true      # do not pass --setting-sources; let claude use its default source loading
model = "claude-sonnet-4-6"
max_turns = 30
timeout_secs = 3600
```

CLI flags override config file values. `inherit_hooks = true` — Setting to `false` is equivalent to passing `--no-inherit-hooks` on the command line: `--setting-sources=` (per OQ-2, unverified) is forwarded to the inner `claude` process, suppressing user hook inheritance. CLI `--no-inherit-hooks` takes precedence over the config file value.

### Where Logs and Token Counts Land

In both modes:

| Artifact | Location | Same as `claude -p`? |
|----------|----------|----------------------|
| Transcript JSONL | `~/.claude/projects/<cwd-slug>/<session-id>.jsonl` | Yes |
| Session registry | `~/.claude/sessions/<pid>.json` | Yes |
| History entry | `~/.claude/history.jsonl` | Yes |
| Stats cache | `~/.claude/stats-cache.json` (rebuilt on next interactive start) | Yes |
| Token counts | Inside the transcript JSONL `message.usage` fields | Yes |

The temp dir holds only the relay infrastructure (hook script + FIFO). It is not part of the log path.

## Crate Dependencies

| Crate | Purpose | Rationale |
|-------|---------|-----------|
| `clap` (derive) | CLI argument parsing | Derive macros generate type-safe flag structs with no boilerplate; dominates Rust CLI tooling; well-maintained. `argh` considered but lacks completions/subcommands for future extensibility. |
| `nix` | `openpty`, `fork`, `login_tty`, `setsid`, `ioctl`, `poll`, `mkfifo`, `signal` | Safe Rust wrappers over the exact POSIX syscalls needed. Using the `libc` crate directly would require more `unsafe` blocks with no benefit. |
| `serde` + `serde_json` | JSONL parsing with schema-tolerant deserialization | Standard choice; `#[serde(default)]` + `#[serde(other)]` give schema tolerance with no extra code. |
| `uuid` | Reserved for future use (e.g., pre-assigning a session ID before spawning claude). Not required in v1.0 — the session_id is derived from the Stop payload or transcript filename. May be removed if unused after implementation. | Listed in Cargo.toml but not yet called; session_id is derived at runtime from Stop payload or transcript basename, not generated. |
| `tempfile` | Per-run temp directory with guaranteed cleanup | `TempDir` drop cleans up even on panic — manual `mktemp` + cleanup would require careful unwinding. |

No async runtime: the PTY event loop is a tight `poll()` on 2–3 fds; `tokio` would add binary size, compile time, and conceptual overhead for no throughput benefit. `stream-json` uses a single reader thread — no runtime needed.

No `regex` crate: probe matching uses a byte-by-byte state machine because probe bytes can straddle chunk boundaries; regex on a raw chunk would miss split sequences.

## Components

### 1. CLI Interface

Drop-in for `claude -p`:

| Flag | Description |
|------|-------------|
| `prompt` (positional) | Prompt string; mutually exclusive with `--input-file` and stdin |
| `--input-file FILE` | Read prompt from file |
| `--model MODEL` | Forwarded to claude (default: `claude-sonnet-4-6`) |
| `--max-turns N` | Forwarded to claude (default: 30) |
| `--output-format FORMAT` | `text` (default), `json`, `stream-json` |
| `--allowedTools LIST` | Comma-separated, forwarded |
| `--disallowedTools LIST` | Forwarded |
| `--dangerously-skip-permissions` | Forwarded |
| `--timeout SECS` | Wall-clock timeout (default: 3600) |
| `--first-output-timeout SECS` | PTY first-output timeout in seconds (default: 90). If child produces no PTY output within this deadline, watchdog terminates with timeout error. |
| `--stream-json-timeout SECS` | Stream-json first-output timeout in seconds (default: 90). If child produces no stream-json events within this deadline, watchdog terminates with timeout error. |
| `--stop-hook-timeout SECS` | Stop hook watchdog timeout in seconds (default: 120). If Stop hook doesn't fire within this deadline after prompt injection, watchdog assumes child is hung and terminates. |
| `--claude-binary PATH` | Override claude binary path (default: resolves `claude` from PATH) |
| `--no-inherit-hooks` | Disable user hook inheritance; passes `--setting-sources=` to claude (unverified per OQ-2) |
| `--version` | Print `claude-print <version> (wrapping claude <version>)` and exit. The claude version is obtained by running the binary at `--claude-binary` (or the PATH-resolved `claude` if not specified). If claude is not found, print `claude-print <version> (wrapping claude: not found)` and exit 0. |
| `--verbose` | Write timing traces to stderr |
| `--check` | Run installation self-test: verify openpty, mkfifo, optional PTY round-trip with mock_claude. Exits 0 on all checks passed, 2 on any failure. |

Stdin accepted as prompt when not a TTY and no positional/`--input-file` given.

**Model precedence:** CLI `--model` flag > `config.toml defaults.model` > compiled-in default (`claude-sonnet-4-6`). The NEEDLE `claude-print.yaml` `model:` field is passed by NEEDLE as the `{model}` template variable, which is forwarded via `--model` — so NEEDLE YAML's model is equivalent to passing `--model` on the command line.

Exit codes:
- `0` — success
- `1` — assistant error (`is_error: true` in transcript)
- `2` — internal error (PTY spawn, hook setup, parse failure)
- `4` — input error (no prompt source: stdin is a TTY with no positional/`--input-file`, or `--input-file`/stdin unreadable, or stdin empty)
- `124` — timeout exceeded
- `130` — interrupted (SIGINT)

Code 4 is a claude-print input-validation code outside the `claude -p` wire-compatibility set (the five codes 0/1/2/124/130 referenced in HR-3). It is emitted from the prompt-resolution path in `main()` before any PTY work: `eprintln!` to stderr + `exit_with_cleanup(4)`, with no structured JSON result object.

### 2. Hook Installer

Creates `$TMPDIR/claude-print-<pid>-<rand>/` via `tempfile::Builder`, created with mode 0700 (via `tempfile::Builder::new().mode(0o700)`) — world-readable temp dirs would allow other local users to read the Stop hook payload (T-1). The temp dir path is validated at creation time: if the path returned by `tempfile` contains a single-quote character, abort with exit 2 (see T-4). In practice this cannot happen with standard `tempfile` crate output, but the check is required by the security threat model.

```
<temp>/
├── settings.json    ← per-run Stop relay hook (merged with user settings via --settings)
├── hook.sh          ← executed by Claude Code on Stop
└── stop.fifo        ← POSIX named pipe for hook→parent IPC
```

**`settings.json`** — contains only the per-run Stop relay hook:
```json
{
  "hooks": {
    "Stop": [{
      "hooks": [{"type": "command", "command": "<temp>/hook.sh", "timeout": 10}]
    }]
  }
}
```

Passed to claude via `--settings <temp>/settings.json`. Claude Code merges this with all other loaded settings sources. The user's `~/.claude/settings.json` Stop hooks (if any) also fire, plus this relay hook.

*Schema note: This double-nested `hooks.Stop[{hooks:[...]}]` structure matches the Claude Code settings format observed in v2.x. Add schema verification to OQ-1's resolution checklist: confirm the settings JSON schema by inspecting a real `~/.claude/settings.json` from the target Claude Code version. If the schema changes, this template must be updated.*

**Hook merge ordering:** Claude Code runs merged hooks sequentially in the order they appear in the merged settings. The relay hook's `"timeout": 10` applies only to the relay hook itself — it does not affect the user's hooks. The user's Stop hooks likely run first (settings.json is merged before --settings), but **this ordering is unverified (per OQ-1)**.

**`hook.sh`** (executed by Claude Code on Stop):
```sh
#!/bin/sh
cat > '<temp>/stop.fifo' 2>/dev/null || true
```

Receives the Stop JSON payload on stdin and writes it to the FIFO. Claude Code does not wait for the hook to complete beyond the 10 s timeout.

**`stop.fifo`** — POSIX named pipe created with `nix::unistd::mkfifo()`.

**In `--no-inherit-hooks` mode**, also forward `--setting-sources=` to claude (empty = no standard sources loaded) *(per OQ-2, unverified; see PO-2 for fallback)*. Only `--settings <temp>/settings.json` is active. This prevents the user's SessionStart/Stop/PreToolUse hooks from firing.

`tempfile::TempDir` handles cleanup on any drop path.

### 3. PTY Spawner

```rust
use nix::pty::{openpty, OpenptyResult};
use nix::unistd::{fork, ForkResult, login_tty};

let OpenptyResult { master, slave } = openpty(None, None)?;

// Set window size on master before fork
set_winsize(master, rows, cols);

match unsafe { fork()? } {
    ForkResult::Child => {
        drop(master);
        login_tty(slave)?;   // setsid + TIOCSCTTY + dup2(slave, 0/1/2)
        // Reset inherited signal handlers to default before exec
        nix::sys::signal::signal(Signal::SIGINT, SigHandler::SigDfl)?;
        nix::sys::signal::signal(Signal::SIGTERM, SigHandler::SigDfl)?;
        execvp("claude", &args)?;
        unreachable!()
    }
    ForkResult::Parent { child } => {
        drop(slave);
        // After the prompt is read from stdin and the fork is complete, the parent
        // closes STDIN_FILENO (nix::unistd::close(0)) to release the caller's pipe.
        // The child's fd 0 is already replaced by login_tty's dup2(slave, 0) regardless.
        run_event_loop(master, child, ...)
    }
}
```

Signal handlers MUST be reset to SIG_DFL in the child before `execvp` — the child inherits the parent's SIGINT/SIGTERM handlers from `fork()`, which would interfere with `claude`'s own signal handling.

`login_tty(slave)` is glibc's `login_tty(3)`: `setsid()` → `TIOCSCTTY` → `dup2(slave, 0/1/2)` → `close(slave)`.

Window size probe order: (1) `TIOCGWINSZ` on `STDOUT_FILENO`, (2) `TIOCGWINSZ` on `STDIN_FILENO`, (3) open `/dev/tty` and `TIOCGWINSZ`, (4) fallback `220 × 50`. In headless/NEEDLE mode, steps 1–3 all fail and the fallback is always used — this is the expected behavior.

Cleanup on any exit path: `SIGTERM` → 2 s → `SIGKILL` → `waitpid`. (Note: the 2-second grace period means actual process exit may be up to 2s after the specified `--timeout`. Callers should account for this when setting their own outer timeout budget. The grace period exists to allow `claude` to save any in-progress state before being killed.)

### 4. Event Loop

Single `poll()` call on `master_fd` and `self_pipe_read` (2 fds always present). At PROMPT_INJECTED, `stop_fifo` read-end is added as a third fd. Deadline tracking is separate:

```
master_fd   POLLIN → read PTY output, dispatch to TerminalEmu + StartupSeq
stop_fifo   POLLIN → Stop hook fired; read payload, begin transcript extraction (added at PROMPT_INJECTED)
[timeout]   —      → tracked via Instant; sets poll() timeout_ms, not a physical fd
```

**Timer mechanism:** There is no separate timer fd. Timeouts (startup 45s, wall-clock `--timeout`) are tracked via `Instant::now()` captured at the relevant phase transition. On each `poll()` call, the timeout argument is set to the minimum remaining ms across all active timers. `poll()` returns at or before the soonest deadline. The initial poll set is 2 fds (`master_fd`, `self_pipe_read`); the FIFO fd is pushed at `PROMPT_INJECTED`. The 'timer' entry in the architecture diagram is a logical representation of deadline tracking, not a physical fd.

**Dynamic fd registration:** The event loop initially polls only `master_fd` (1 fd). At the `TRUST_DISMISSED → PROMPT_INJECTED` transition, the FIFO read-end fd is added to the poll() set. Subsequent poll() iterations include both fds. The simplest implementation: represent the pollfd array as a `Vec<pollfd>` and push the FIFO fd at transition time.

**TerminalEmu** runs on every chunk of PTY output, scanning for escape sequences and queueing responses. Responses written to `master_fd` on the next writable poll.

**StartupSeq** tracks phase (Waiting / TrustDismiss / PromptInjected) and transitions based on heuristics (see §5).

**FifoPoller** opens `stop.fifo` for reading in a non-blocking O_NONBLOCK open; polls for data via the same `poll()` call.

### 5. Terminal Emulator (Ink probe responder)

Ink sends DEC terminal queries at startup and hangs if unanswered. The emulator scans raw bytes for known probe patterns:

| Probe bytes | Response bytes | Notes |
|-------------|---------------|-------|
| `ESC [ c` or `ESC [ 0 c` | `ESC [ ? 6 c` | DA1 |
| `ESC [ > c` or `ESC [ > 0 c` | `ESC [ > 0 ; 0 ; 0 c` | DA2 |
| `ESC [ 6 n` | `ESC [ 1 ; 1 R` | DSR cursor position |
| `ESC [ > q` or `ESC [ > 0 q` | `\x1bP>|claude-print\x1b\\` | XTVERSION (DCS string) — ST = String Terminator = 0x1B 0x5C; the final two bytes are ESC + backslash, not backtick. Both the bare form and the parameterized form (`0` parameter) produce the same response. |
| `ESC [ 1 8 t` | `ESC [ 8 ; <rows> ; <cols> t` | Window size |

**Version-resilience rule:** Unknown escape sequences (`ESC [ ... <letter>` not in the table above) are silently discarded — never treated as an error. If Ink adds new probe types in future versions, they are ignored and the session proceeds via the startup sequencer timeout.

Each probe type is acknowledged at most once per session (dedup bitmask).

### 6. Startup Sequencer

**Phase 1 — Trust/welcome dismiss:**

The trust dialog asks the user to confirm before allowing tool use. Detection uses keyword scanning, not exact string match, to survive UI text changes across Claude Code versions:

- If any output line contains two or more of: `trust`, `Allow`, `continue`, `folder`, `permission`, `proceed` → send `\r` immediately
- Fallback: after 0.4 s with no new PTY bytes and ≥ 200 bytes received total → send `\r` (covers any welcome/confirmation prompt)
  - **Reduced quiet window (2026-08-20):** This was 0.8 s. The 0.4 s value is an uninterrupted-silence requirement, not a wall-clock deadline: every PTY chunk restarts it. A settled TUI advances 0.4 s sooner, while a TUI that is still rendering continues to defer the fallback.
- Hard timeout: if the process has been in WAITING state for 45 s and fewer than 200 bytes have been received → exit 2 (binary not found or hung, or partial-output hang)

The idle/byte fallback is a one-shot: once any trigger (keyword or idle) fires and transitions to TRUST_DISMISSED, the fallback timer is deactivated and cannot re-fire.

**Phase 2 — Prompt injection:**

- After Phase 1 CR, wait until the PTY is quiet for 1.0 s (REPL re-renders)
  - **Reduced quiet window (2026-08-20):** This was 2.0 s. The window restarts on every PTY chunk received via `feed()`, so prompt injection still requires a full second of silence after the final redraw rather than merely one second since the CR.
- (If the PTY never goes idle for 1.0 s — e.g., claude streams continuous progress output — the wall-clock `--timeout` is the only exit path. This is expected behavior; the phase has no dedicated sub-timeout. `--verbose` logs a warning if TRUST_DISMISSED persists > 10 s.)
- Send via bracketed paste: `\x1b[200~<prompt>\x1b[201~\r`
- Bracketed paste treats embedded `\n` as literals (no premature Enter)
- Prompts > 32 KB: write to `$TMPDIR/claude-print-.../prompt.txt`; send `/read <path>\r` (`/read` is a built-in slash command, not an MCP tool. Prompt file written as UTF-8 with no BOM. After sending `/read <path>\r`, the startup sequencer re-enters the idle-wait loop (the same 1.0s quiet window, restarted by PTY output). Claude Code reads the file contents and begins processing — no system acknowledgment is emitted before the response. The response extraction path is identical to inline injection: Stop hook fires after the response, transcript JSONL is read normally. See EC-5 for sandboxing note.)

The reduced quiet windows leave the race guards intact: EC-7 still rejects a Stop event before `PROMPT_INJECTED`; EC-8 retains its 200-byte gate and 45 s hard timeout; and INV-3 still opens the FIFO before any prompt injection. Deterministic startup tests cover both quiet-window boundaries and verify that new PTY output restarts each window.

### 7. Stop Poller

**Assumption:** Stop fires once per session, not once per turn. This matches observed `claude -p` behavior for single-turn sessions. Verify for multi-turn `--max-turns > 1` sessions during OQ-1 verification.

Reads from `stop.fifo` (non-blocking open; polled via the main `poll()` loop). On data available:

1. Read one line → parse JSON with lenient schema (all fields `Option<T>`)
2. Extract `session_id` and `transcript_path` (either direct or derived from `session_id` + `cwd`). If both `transcript_path` and `cwd` are absent from the Stop payload: skip path derivation entirely; proceed directly to the retry loop using `last_assistant_message` as the only fallback. If `last_assistant_message` is also absent: emit `is_error=true`, exit 1.
3. Signal the event loop to exit
4. Send `/exit\r` to the PTY child. (Bracketed paste is not used here: at this point the REPL has returned to idle after completing the response, so a plain CR-terminated command is accepted. `/exit` is a Claude Code built-in slash command that initiates graceful shutdown.) After sending `/exit\r`, wait up to 5s for the child to exit, detected by polling `master_fd` with a 5-second deadline: when `EIO` is returned, the child process has exited. `waitpid(WNOHANG)` MAY be used as a supplementary check on each poll iteration. No SIGCHLD handler is required for this path. If the child has not exited after 5s, proceed directly to SIGTERM → 2s → SIGKILL cleanup.

If Stop never fires within `--timeout` seconds: emit timeout result, SIGTERM child, exit 124.

### 8. Transcript Reader

On Stop receipt:

```
1. Open transcript_path (derived if not in payload)
   Path derivation algorithm (observed from Claude Code v2.x): strip the leading `/` from
   `cwd`, replace all remaining `/` characters with `-`.
   Example: `/home/coding/myproject` → `home-coding-myproject`.
   This algorithm can produce ambiguous slugs for paths where directory names contain hyphens
   (e.g., `/home/user/a-b` and `/home/user-a/b` both produce `home-user-a-b`). In practice,
   `session_id` uniquely identifies the JSONL file within the directory, so slug ambiguity only
   causes a problem if the slug-derived *directory* is wrong. If path derivation fails (directory
   not found), fall back to `last_assistant_message`.
   Add a unit test in `tests/transcript.rs` asserting this mapping for 3–4 representative
   cwd values (e.g. `/home/coding/myproject`, `/root/foo/bar`, `/home/user/a-b` [note: same
   slug as `/home/user-a/b` — ambiguity documented above], `/tmp/x`).
2. Scan for unique API turns (usage-fingerprint dedup)
3. Collect final turn's text blocks
4. Sum token counts across all unique turns
5. Retry loop if final_text is empty (race window): 40 × 50 ms
6. Fallback to last_assistant_message from Stop payload if retries exhausted
7. If both empty: is_error=true, exit 1
```

**Token aggregation (usage dedup):**

Multiple consecutive `assistant` events sharing the same API call carry identical `message.usage` objects (streaming chunks). Use two complementary dedup strategies, with `message.id` as the primary key:

```rust
let mut seen_ids: HashSet<String> = HashSet::new();
let mut prev_usage_key: Option<UsageKey> = None;
let mut turns: Vec<Usage> = vec![];

for event in parse_events(path) {
    if let Event::Assistant { message } = event {
        // Primary dedup: message.id (each API call has a unique id)
        let is_new_turn = if let Some(id) = &message.id {
            seen_ids.insert(id.clone())   // returns true if newly inserted
        } else {
            // Fallback for versions that omit message.id: usage-fingerprint dedup
            let key = UsageKey::from(&message.usage);
            let new = Some(&key) != prev_usage_key.as_ref();
            prev_usage_key = Some(key);
            new
        };

        if is_new_turn {
            turns.push(message.usage.clone());
        }
        // accumulate text blocks from current chunk regardless
    }
}
```

`message.id` is present in observed transcripts. Usage-fingerprint fallback handles older Claude Code versions that may not include it.

**Known limitation of fingerprint fallback:** Two consecutive turns with identical `(input_tokens, output_tokens, cache_creation_input_tokens, cache_read_input_tokens)` are incorrectly collapsed into one turn. This is a known false-negative. `message.id` is the required path in production — fingerprint fallback is only for Claude Code versions that omit `message.id`, which is not observed in any current version. If fingerprint dedup is triggered and produces wrong results, the indication is a lower-than-expected `num_turns` count in the JSON output.

**Schema tolerance (`serde` config for all JSONL structs):**

```rust
#[derive(Deserialize, Default)]
#[serde(default)]          // missing fields → Default::default()
pub struct Usage {
    pub input_tokens:                Option<u64>,
    pub output_tokens:               Option<u64>,
    pub cache_creation_input_tokens: Option<u64>,
    pub cache_read_input_tokens:     Option<u64>,
    // Unknown fields are silently ignored (no deny_unknown_fields)
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum Event {
    Assistant { message: AssistantMessage },
    User { message: UserMessage },
    Result(ResultEvent),
    #[serde(other)]         // any unknown type → skip, no error
    Unknown,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum ContentBlock {
    Text { text: String },
    ToolUse { name: String },
    Thinking { thinking: String },
    #[serde(other)]
    Unknown,
}
```

### 9. Emitter

**`text`** (default): `{response_text}\n`

**`json`**:
```json
{
  "type": "result",
  "subtype": "success",
  "is_error": false,
  "result": "<response text>",
  "session_id": "<uuid>",
  "num_turns": 3,
  "duration_ms": 4200,
  "cost_usd": 0,
  "claude_version": "2.1.168",
  "usage": {
    "input_tokens": 6224,
    "output_tokens": 43079,
    "cache_creation_input_tokens": 107205,
    "cache_read_input_tokens": 4066110
  }
}
```

`duration_ms`: wall-clock milliseconds from `std::time::Instant::now()` captured at `main()` entry to the moment the emitter writes its final output. This includes all overhead AND model latency — it is the total time a caller waited for a response.

**`stream-json`**: Current implementation is post-session replay — after Stop fires and the child exits, the emitter (via `src/main.rs` `replay_stream_json`) reads the complete transcript from the beginning and forwards all JSONL events to stdout. This provides `claude -p`-compatible output but does not stream events in real-time. Live tailing (real-time forwarding as Claude Code writes events) is tracked as an open work item (bead bf-5vm). When live tailing lands, a reader thread will tail the transcript from the byte offset captured at prompt injection time, forwarding each new line immediately. Until then, the replay implementation forwards all raw JSONL lines (no dedup) — this matches `claude -p --output-format stream-json` behavior, which also emits one line per chunk. The dedup logic in §8 Transcript Reader applies only to the `json` and `text` output formats where a single aggregated response is needed. Callers of `stream-json` MUST handle duplicate streaming chunks (same `message.id`, identical `usage`) as they would with `claude -p`. On normal completion, the final `{"type":"result", "is_error": false, ...}` line in the output is Claude Code's own Result event forwarded verbatim; claude-print does NOT synthesize an additional result line on success. `claude_version` is NOT injected into the forwarded Result event. On error (no Claude Code result), claude-print synthesizes the final result line and injects `claude_version`.

`session_id` in output: taken directly from the Stop payload if present. If absent from the payload, derive from the transcript file basename (filename without `.jsonl`). If neither is available (no transcript), emit `null`.

**Known limitation:** `cost_usd` is always `0`. Claude Code does not expose per-session cost data via the transcript JSONL. Callers should not use this field for billing purposes. It is included for wire compatibility with `claude -p --output-format json` which also emits `0` for this field.

`claude_version` field (new, not in `claude -p` wire format): included in `json` output and in the final error result line of `stream-json` output. It does not appear in `text` output (no JSON envelope in text mode). Callers that parse strictly by field name are unaffected by the extra field.

`claude_version` runtime value: run `claude --version` (or the binary at `--claude-binary`) **once at process startup, before `fork()`**. Parse the output with the same permissive regex used by `--version` flag handling. Cache the result and pass it to the emitter. On parse failure, use `"unknown"`.

Error result:
```json
{"type": "result", "subtype": "timeout|interrupted|internal_error|assistant_error",
 "is_error": true, "error_message": "...", "claude_version": "..."}
```

**Error output by format:**
- `text` mode: on error, nothing is written to stdout; the error message is written to stderr. Exit code is the signal to callers.
- `json` mode: the error JSON object is written to stdout (as specified above). Nothing to stderr unless `--verbose`.
- `stream-json` mode: if an error occurs after prompt injection, a final JSON error line is emitted to stdout (`{"type": "result", "is_error": true, "subtype": "...", "error_message": "...", "claude_version": "..."}`); if an error occurs before prompt injection, same as `text` mode (nothing to stdout, stderr message).

### 10. Watchdog

The watchdog prevents indefinite hangs when the child `claude` process wedges. Without watchdog, a hung child would cause `claude-print` to poll `stop.fifo` forever, never exiting.

**Timeout types:**

| Timeout Type | Default | Purpose | Error Subtype |
|--------------|---------|---------|---------------|
| PTY first-output timeout | 90 s | Child produces no PTY output within deadline (process may be hung at startup) | `pty_first_output_timeout` |
| Stream-json first-output timeout | 90 s | Child produces no stream-json events within deadline (process may be hung during session initialization) | `stream_json_first_output_timeout` |
| Overall timeout | 3600 s | Session exceeded overall max-turn deadline (max-turn timeout applies throughout entire session) | `overall_timeout` |
| Stop hook timeout | 120 s | Stop hook didn't fire within deadline after prompt injection (child may have hung during tool use or model inference) | `stop_hook_timeout` |

**Implementation:** `src/watchdog.rs` runs timeout checks on separate deadlines tracked via `Instant::now()`. Each timeout is independent — if any deadline expires, the watchdog triggers cleanup: SIGTERM child → 2 s grace → SIGKILL → waitpid → emit timeout result and exit.

The watchdog operates through a multi-phase timeout thread that monitors four distinct conditions:

1. **PTY first-output detection**: If the child produces no PTY output within the deadline, the watchdog assumes the process is hung at startup and terminates it.

2. **Stream-json first-output monitoring**: A background monitor thread watches `<temp_dir>/transcript.jsonl` for valid JSON lines. This monitor only runs in `stream-json` output mode (guarded by `stream_json_mode` flag). In text/json modes, this timeout is disabled because the transcript is written directly to `~/.claude/projects/` rather than the temp directory. The monitor wakes every 100ms to check if the file exists and contains valid JSON.

3. **Overall session timeout**: Enforced throughout the entire session duration, regardless of current phase. This prevents indefinite polling of `stop.fifo` if the child wedges during any stage of execution.

4. **Stop hook timeout**: After prompt injection, if the Stop hook doesn't fire within the deadline, the watchdog assumes the child is hung during tool use or model inference and terminates it.

**State tracking**: The watchdog uses atomic flags for thread-safe state coordination:
- `pty_output_received`: Set when PTY output is detected
- `stream_json_output_received`: Set when stream-json events are detected  
- `prompt_injected_at`: Captures the timestamp when prompt injection occurs
- `timeout_fired` and `timeout_type`: Indicate which timeout triggered

**Integration with event loop:** The watchdog is consulted on each `poll()` iteration. When a timeout fires, the event loop breaks, cleanup runs, and a timeout result is emitted with the appropriate `subtype` field. The watchdog signals the event loop via a self-pipe mechanism — the timeout thread writes a byte to a pipe fd that's included in the `poll()` set, waking the main loop immediately when a timeout occurs.

### 11. NEEDLE Agent Config

`claude-print.yaml` → `~/.needle/agents/`:
```yaml
name: claude-print
description: Claude Code interactive mode — subscription billing (cc_entrypoint=cli)
agent_cli: claude-print
version_command: "claude-print --version"
input_method:
  method: stdin
invoke_template: "cd {workspace} && claude-print --model {model} --max-turns 30 --output-format json --dangerously-skip-permissions --no-inherit-hooks"
timeout_secs: 3600
provider: anthropic
# Note: --max-turns 30 and --no-inherit-hooks are hardcoded in the template above.
# --max-turns 30 takes precedence over config.toml's max_turns setting for NEEDLE-dispatched
# jobs. To change the turn limit for NEEDLE workers, edit the invoke_template directly.
# NEEDLE workers run in isolation mode by default (--no-inherit-hooks is included in the
# template). To enable user hook inheritance for NEEDLE jobs, remove --no-inherit-hooks
# from the invoke_template.
model: claude-sonnet-4-6
output_transform: needle-transform-claude
cost:
  type: use_or_lose
```

`needle-transform-claude` is the built-in NEEDLE output transform for Claude Code's `--output-format json` output. It extracts the `result` field (the assistant's response text) from the JSON object and passes it to the NEEDLE worker as the agent's response. This transform is already defined in NEEDLE's built-in transform registry — no new implementation is required in Phase 9.

With `input_method: stdin`, NEEDLE pipes the bead prompt text to `claude-print`'s stdin. Since `claude-print` is invoked non-interactively (its stdin is a pipe, not a TTY), the CLI reads stdin as the prompt source (see §1: "Stdin accepted as prompt when not a TTY and no positional/`--input-file` given").

### 12. Install Script

`install.sh`:
1. Detect arch (`uname -m`) and select binary from release assets
2. Verify `claude` is on `$PATH`
3. If `~/.local/bin/claude-print` already exists, move it to `~/.local/bin/claude-print.prev` (enables one-step rollback)
4. Install binary to `~/.local/bin/claude-print` (mode 755)
5. Install `mock_claude` to `~/.local/bin/mock_claude` (mode 755) — unless `SKIP_MOCK_CLAUDE=1` (`mock_claude` installation can be skipped by setting `SKIP_MOCK_CLAUDE=1` in the install environment — e.g., for users who prefer not to add test fixtures to their PATH)
6. Install `claude-print.yaml` to `~/.needle/agents/` (mode 644, skipped if NEEDLE not installed)
7. Run `claude-print --check` to verify installation (full PTY round-trip self-test using mock_claude; skips PTY round-trip if `SKIP_MOCK_CLAUDE=1` was set in step 5)
8. Print `claude-print --version` for confirmation

## Data Models

### Stop Hook Payload (received from Claude Code — all fields optional)
```json
{
  "hook_event_name": "Stop",
  "session_id": "abc123",
  "transcript_path": "/home/coding/.claude/projects/.../abc123.jsonl",
  "last_assistant_message": "...",
  "cwd": "/home/coding/..."
}
```

`transcript_path` absent → derive from `session_id` + `cwd`.
`last_assistant_message` absent → retry loop only (no string fallback).

### JSONL Transcript — Full Usage Object (as observed v2.1.168)
```json
{
  "input_tokens": 6178,
  "output_tokens": 295,
  "cache_creation_input_tokens": 825,
  "cache_read_input_tokens": 26442,
  "server_tool_use": {"web_search_requests": 0, "web_fetch_requests": 0},
  "service_tier": "standard",
  "cache_creation": {"ephemeral_5m_input_tokens": 0, "ephemeral_1h_input_tokens": 825},
  "inference_geo": "",
  "iterations": [{"input_tokens": 6178, "output_tokens": 295, ...}],
  "speed": "standard"
}
```

Only `input_tokens`, `output_tokens`, `cache_creation_input_tokens`, `cache_read_input_tokens` are aggregated. All other fields ignored.

### Emitted Result (--output-format json)
```json
{
  "type": "result",
  "subtype": "success",
  "is_error": false,
  "result": "response text",
  "session_id": "abc123",
  "num_turns": 1,
  "duration_ms": 4200,
  "cost_usd": 0,
  "claude_version": "2.1.168",
  "usage": {
    "input_tokens": 1240,
    "output_tokens": 380,
    "cache_creation_input_tokens": 0,
    "cache_read_input_tokens": 900
  }
}
```

`duration_ms`: wall-clock milliseconds from `std::time::Instant::now()` captured at `main()` entry to the moment the emitter writes its final output. This includes all overhead AND model latency — it is the total time a caller waited for a response.

## Error Handling

| Condition | Detection | Action | Exit |
|-----------|-----------|--------|------|
| No prompt source, or unreadable `--input-file`/stdin | prompt resolution in `main()` (TTY stdin with no positional/`--input-file`; `--input-file` or stdin read fails; stdin empty) | stderr message (no JSON result) | 4 |
| `claude` binary not found | PATH lookup fails at startup | emit error | 2 |
| PTY open fails | `openpty()` returns Err | emit error | 2 |
| Hook installer fails | temp dir / mkfifo / write error | emit error | 2 |
| WAITING state persists for 45 s and bytes_received < 200 | startup timer | kill child, emit error | 2 |
| Child exits before Stop | `waitpid` returns | emit error with child exit code | 2 |
| Wall-clock timeout | poll timer | SIGTERM child, emit timeout | 124 |
| Stop hook never fires | FIFO timeout | SIGTERM child, emit timeout | 124 |
| SIGINT | signal handler | SIGINT child (per HR-8); set interrupted flag, emit interrupt result | 130 |
| SIGTERM received | signal handler | SIGTERM child, emit interrupt result | 130 |
| Stop payload has no `transcript_path` and no `cwd` | payload parse | skip to `last_assistant_message` fallback; if also absent, emit error | 1 |
| Transcript empty + fallback empty | retry exhausted | emit error | 1 |
| `is_error: true` in transcript | result event or error block | emit error result | 1 |
| Rate limit / API error | error content in transcript | emit error result | 1 |

## Edge Case Catalog

| # | Edge Case | Resolution |
|---|-----------|-----------|
| EC-1 | Two `claude-print` instances on the same `cwd` concurrently | Each has its own `session_id` and JSONL file. FIFO paths are per-pid — no cross-contamination. |
| EC-2 | `~/.claude/projects/` does not exist | The inner `claude` creates it (standard behavior). If still absent after Stop, path derivation returns an error; fallback to `last_assistant_message`. |
| EC-3 | FIFO write blocks (Stop fires before read-end is open) | Read-end opened O_NONBLOCK at `TRUST_DISMISSED → PROMPT_INJECTED` transition, before prompt is injected. Stop cannot fire before prompt is sent. |
| EC-4 | Prompt contains null bytes | Rejected at CLI validation time with exit 2. `claude -p` itself does not support null bytes. |
| EC-5 | Prompt > 32 KB | Written to `$TMPDIR/<session>/prompt.txt`; `/read <path>\r` sent instead. File cleaned up with temp dir. Requires PO-6 to hold. See Startup Sequencer §6 for the full /read relay specification including encoding and response flow. |
| EC-6 | `claude --version` output format changes | Version parsing uses a permissive regex. If parsing fails, `claude_version: "unknown"` in output; `--version` still exits 0. |
| EC-7 | Stop hook fires before trust dismiss (no dialog shown) | EC-11 unsets `CLAUDE_CODE_SESSION_ID`/`CLAUDE_CODE_SESSION_KIND` before `execvp`, which should prevent this in normal operation. If Stop fires before prompt injection despite EC-11, treat it as an error: emit `is_error=true` and exit 2, rather than silently accepting an empty-prompt response. |
| EC-8 | WAITING state persists for 45 s with fewer than 200 bytes received (covers both zero-byte case and partial-output hang — detects binary-not-found, hung startup, or process emitting <200 bytes then stalling) | Hard timeout: SIGTERM → 2 s → SIGKILL → waitpid → exit 2. |
| EC-9 | `last_assistant_message` contains ANSI escape sequences | Strip ANSI before emitting in `text` and `json` formats (simple regex on the fallback string only). In `stream-json` mode, if the `last_assistant_message` fallback is used (retry loop exhausted), ANSI sequences MUST also be stripped before the synthesized fallback result event is emitted. |
| EC-10 | Truncated final JSONL line | Malformed line skipped by lenient parser. If no complete assistant events remain, retry loop fires. |
| EC-11 | `CLAUDE_CODE_SESSION_ID` / `CLAUDE_CODE_SESSION_KIND` inherited from parent | Unset both in child env before `execvp` to prevent session identity confusion. (See Open Questions #6.) |
| EC-12 | Stdin is a TTY (interactive call with no prompt) | Require a prompt source. If stdin is a TTY and no positional/`--input-file` given, exit 4 with a usage error on stderr. Do NOT drop into an interactive session. (Exit 4, matching the `src/main.rs` prompt-resolution path — the same code covers an unreadable `--input-file`, an unreadable stdin, and an empty stdin. EC-12 previously said "exit 2"; corrected to exit 4 to match the implementation. Source behavior is unchanged.) |

## Anti-Patterns

Approaches considered and rejected. Document why so they are not re-proposed.

| Anti-Pattern | Why Rejected |
|-------------|-------------|
| Use `CLAUDE_CONFIG_DIR` to sandbox all claude I/O | Over-engineering: requires credential symlinking, settings duplication, and transcript forwarding. `--settings` merge achieves the relay hook without redirecting any I/O. |
| Parse Ink probes with regex on raw chunks | Probe bytes can straddle chunk boundaries. A regex on a single chunk misses split sequences. Use a byte-by-byte state machine. |
| Use `tokio` async runtime for the event loop | Tight `poll()` on 2–3 fds; no throughput benefit. Adds compile time, binary size, and complexity. |
| Open FIFO read-end after prompt injection | Creates a race: Stop hook may write before the read-end is open, causing hook's `cat > fifo` to block until timeout. |
| Use `last_assistant_message` from Stop payload as primary text | May be truncated or differently formatted than transcript content blocks. JSONL transcript is canonical; Stop payload is fallback only. |
| Scrape PTY screen buffer with `pyte` as primary path | Screen holds only what fits in terminal height. Long responses truncated. JSONL is complete. `pyte` is last-resort only. |
| One global relay `settings.json` in `~/.claude/` | Multiple concurrent invocations would race on the same file. Per-run temp dir + per-invocation file avoids all concurrency issues. |
| `shell=true` for `hook.sh` | Shell injection risk if temp dir path contains special characters. `hook.sh` is exec'd directly by Claude Code, not through a shell. |

## Invariants

Named invariants that MUST hold on all exit paths. Each is testable.

| # | Invariant | Test |
|---|-----------|------|
| INV-1 | Temp dir cleaned up on every exit path | After each integration test assert `$TMPDIR/claude-print-*` is absent |
| INV-2 | Child process always waited on before `main()` returns | Zombie check in cleanup integration test |
| INV-3 | FIFO read-end opened before prompt injection | `--verbose` trace: `"fifo opened"` timestamp precedes `"prompt injected"` |
| INV-4 | `master_fd` closed before `waitpid` | `lsof` in integration test: no master fd open after child exits |
| INV-5 | No write-opens to `~/.claude/` by the `claude-print` process itself | `strace -e openat` shows no writes; verified in hook inheritance tests |
| INV-6 | `cc_entrypoint=cli` in every generated transcript | AS-4 daily canary plus the manual pre-release gate |
| INV-7 | Exit code matches the Error Handling table | Each error condition tested with mock_claude; exit code asserted |
| INV-8 | Reader thread (stream-json) joined before process exit | Join coverage in stream-json integration test |

## Proof Obligations

Assumptions that must hold for the design to work. Each has a named recovery if false.

| # | Assumption | If False | Recovery |
|---|-----------|---------|---------|
| PO-1 | `--settings <file>` merges hooks rather than replacing | User hooks silently stop firing | Read `~/.claude/settings.json`, merge hook arrays in-process, write combined file to temp dir, pass combined via `--settings` |
| PO-2 | `--setting-sources=` (empty) suppresses all standard sources | `--no-inherit-hooks` still loads user hooks | Try `--setting-sources=none`; if unsupported, enumerate only relay hook source explicitly |
| PO-3 | `login_tty` compiles under `x86_64-unknown-linux-musl` | Phase 2 fails to build | Inline as `setsid()` + `ioctl(slave, TIOCSCTTY, 0)` + `dup2(slave, 0/1/2)` + `close(slave)` — all four syscalls musl always provides |
| PO-4 | Ink probes are DA1/DA2/DSR/XTVERSION/window-size only | Session hangs on unrecognized probe | Unknown probes ignored; session falls through to idle timeout for trust dismiss. Add new probes to table as discovered. |
| PO-5 | Stop hook fires after final JSONL flush | Transcript empty on first attempt | 40×50 ms retry loop (2 s budget). If Stop fires >2 s ahead of JSONL flush, increase retry budget or fall back to `last_assistant_message`. |
| PO-6 | `/read <path>` accepts absolute paths for prompts >32 KB | Large prompt relay fails | Truncate at 32 KB with appended notice `[prompt truncated at 32KB]`. |

## Implementation Phases

### Status

| Item | State |
|------|-------|
| Phases 1–11 module implementation | **COMPLETE** — all module-level deliverables committed |
| `main()` session orchestration | **COMPLETE** — src/main.rs and src/session.rs orchestration shipped as v0.2.0 |
| Binary-level E2E tests (AS-1, AS-2, AS-5) | **COMPLETE** — tests passing (claudepr-6ab03b22) |
| AS-4 billing classification | **AUTOMATED DAILY** on production hosts; manual pre-release verification remains required |
| CI release binary | **PENDING** — fresh tagged CI release tracked separately |

Phase ordering is sequential. Each phase MUST NOT begin until the prior phase's completion criterion is met.

**Phase 1: Crate Scaffold (~150 LOC)**
*Entry:* None.
- [x] `Cargo.toml` workspace with pinned deps, `src/main.rs`, `cli.rs` (clap), `error.rs`, `config.rs`
- [x] `--version` prints `claude-print <VERSION> (wrapping claude <claude-version>)` where <VERSION> is from Cargo.toml and <claude-version> is resolved at runtime
- [x] Add `claude-print-ci.yaml` stub to `jedarden/declarative-config` (verify step only; `build-musl` and `github-release` steps added in Phase 11)

*Complete when:* `cargo build --target x86_64-unknown-linux-musl` succeeds; `claude-print --version` prints expected format; `cargo test --lib` passes; `claude-print-ci.yaml` stub exists in declarative-config and ArgoCD syncs it to `argo-workflows-ns-iad-ci`.

**Phase 2: Hook Installer + PTY Spawner (~200 LOC)**
*Entry:* Phase 1 complete. **PO-3 verified** (attempt `login_tty` under musl; if absent, inline implementation ready before starting). **PO-1 verified** (confirm `--settings` merges hooks rather than replacing; if false, see PO-1 recovery before writing the hook installer). PO-1 can be verified with a simple test: run `claude --settings /tmp/test_settings.json echo test` where test_settings.json contains a dummy hook, alongside a user hook in ~/.claude/settings.json, and confirm both fire. **OQ-5 (login_tty availability in musl) verified or PO-3 inline fallback ready; OQ-6 (CLAUDE_CODE_SESSION_ID inheritance) resolved.**
- [x] `hook.rs`: temp dir (`tempfile::TempDir`), write `settings.json` and `hook.sh`, `mkfifo`
- [x] `pty.rs`: `openpty`, `fork`, window-size probe, `login_tty`, `execvp`, SIGTERM/SIGKILL/`waitpid`
- [x] `--no-inherit-hooks` forwards `--setting-sources=` to child (unverified per OQ-2)
- [x] Build `mock_claude` fixture binary (`test-fixtures/mock-claude/`) as part of the workspace — required for PTY integration tests starting this phase

*Complete when:* Integration test `test_pty_spawns_tty` passes (child observes `isatty(stdout)=true`); temp dir absent after test; `--setting-sources=` in child argv when `--no-inherit-hooks` set.

**Phase 3: Event Loop (~150 LOC)**
*Entry:* Phase 2 complete.
- [x] `event_loop.rs`: `poll()` on `master_fd + self_pipe_read` (initial 2-fd set); `Vec<pollfd>` for dynamic stop_fifo registration at PROMPT_INJECTED; read buffer; EIO detection (child exit)

*Complete when:* `test_event_loop_reads_pty_output` passes; `test_event_loop_detects_child_exit` (EIO → exit 2) passes.

**Phase 4: Terminal Emulator (~100 LOC)**
*Entry:* Phase 3 complete. PO-4 noted (unknown Ink probes are ignored by design — no pre-phase verification required beyond confirming the design choice is implemented correctly).
- [x] `terminal.rs`: probe scanner, response table, dedup bitmask, unknown-probe passthrough

*Complete when:* All terminal unit tests pass (all 5 probes answered, unknown probe ignored, split-chunk probe handled, dedup works).

**Phase 5: Startup Sequencer (~120 LOC)**
*Entry:* Phase 4 complete. **OQ-3b must be resolved** (verify `/read` accepts absolute paths; if false, commit to PO-6 truncation fallback before implementing the large-prompt relay).
- [x] `startup.rs`: keyword trust dismiss, idle-gap timing, bracketed paste injection, large-prompt file relay

*Complete when:* All startup unit tests pass; integration test `test_trust_dialog_standard_wording` and `test_trust_dialog_alternate_wording` pass.

**Phase 6: Stop Poller (~80 LOC)**
*Entry:* Phase 5 complete. **OQ-2 must be resolved** (verify `--setting-sources=` suppresses standard sources; see PO-2 for fallback). **OQ-4 (FIFO open race) validated by test.**
- [x] Open FIFO read-end O_NONBLOCK, integrate into `poll()` loop, parse Stop payload, derive transcript path, signal event loop exit

*Complete when:* Integration test `test_stop_hook_fires` passes; `test_missing_transcript_path_derived` passes.

**Phase 7: Transcript Reader (~180 LOC)**
*Entry:* Phase 6 complete. **PO-5 acknowledged**: retry loop (40×50ms) is the mitigation for Stop-before-JSONL races. Verify retry timing is sufficient by running `test_transcript_race` with `MOCK_DELAY_JSONL=100` and confirming exit 0.
- [x] `transcript.rs`: JSONL parse with lenient serde, `message.id` dedup + fingerprint fallback, text extraction, retry loop, Stop-payload fallback, path derivation

*Complete when:* All transcript unit tests pass; `test_streaming_dedup_40_retries` passes; AS-6 (race scenario) passes.

**Phase 8: Emitter (~120 LOC)**
*Entry:* Phase 7 complete.
- [x] `emitter.rs`: text/json/stream-json, `claude_version`, error result objects, exit code mapping; stream-json reader thread + mpsc channel

*Complete when:* All emitter unit tests pass; AS-1 (text), AS-2 (json), stream-json output parses as valid JSONL.

**Phase 9: NEEDLE Integration (~50 LOC + config)**
*Entry:* Phase 8 complete.
- [x] `claude-print.yaml`, `install.sh`, `claude-print-ci` WorkflowTemplate in declarative-config
- [x] Implement `--check` doctor subcommand (openpty probe, mkfifo probe, optional mock_claude PTY round-trip)

*Complete when:* `install.sh` is written and syntactically valid (`bash -n install.sh` passes); manually copying the locally-built binary to `~/.local/bin/claude-print` and running `claude-print --check` succeeds. Full install.sh end-to-end test (downloading from GitHub Release) is reserved for Phase 11. NEEDLE dispatches a test bead using `claude-print.yaml`; AS-3 passes; README flags table matches `claude-print --help` output (verified manually).

**Phase 10: Tests (~500 LOC)**
*Entry:* Phase 8 complete (can run in parallel with Phase 9).
- [x] Phase 10 **completes** the test suite by adding any tests not already written as part of Phases 2–9's completion criteria. Each phase's completion criterion already specifies and runs its own targeted integration tests — Phase 10 adds the remaining cross-phase and corner-case tests: the version-resilience suite, hook inheritance suite, all MEDIUM/LOW mock scenarios not covered by earlier phases, and the conformance harness.

*Complete when:* `cargo test` passes with zero failures.

**Phase 11: CI (~YAML only)**
*Entry:* Phase 10 complete.
- [x] `claude-print-ci` Argo WorkflowTemplate: fmt + clippy + test + musl release binary + artifact upload
  *(The repo-root reference and `jedarden/declarative-config` copy are identical and the live WorkflowTemplate is Synced. The existing v0.2.0 assets are static, but a fresh tagged CI submission fails at `cargo fmt --check` on that immutable tag; a new rustfmt-clean release tag is required to verify end-to-end CI provenance.)*
- [x] CI also builds `mock_claude` binary (musl) and uploads it as a release artifact alongside `claude-print`

- [x] Confirm `cargo audit` runs on every push (either via `rust-verify` or as an explicit CI step)
- [x] Run install.sh end-to-end download test: download release artifact from GitHub Release URL and verify install.sh exits 0 and `claude-print --check` passes
  *(Verified against the v0.2.0 GitHub release on a clean prefix; both assets download and `claude-print --check` exits 0.)*

*Complete when:* CI run on main branch produces release binary; `last-claude-version.txt` artifact present; binary passes `claude-print --check` (credential-free) via `install.sh`; install.sh end-to-end download test (deferred from Phase 9) passes; full AS-1 is verified manually before each release tag is pushed.

## Testing

### Unit Tests (`src/` inline + `tests/`)

**Terminal probe responder** (`tests/terminal.rs`):
- DA1 bytes in → `ESC[?6c` response bytes out
- DA2 bytes in → `ESC[>0;0;0c` out
- DSR bytes in → `ESC[1;1R` out
- XTVERSION bytes in → correct DCS string out
- Window-size query → `ESC[8;50;220t` with actual configured dimensions
- Multiple probes in one chunk → all answered in order
- Probe dedup: send DA1 twice → response emitted only once
- **Unknown escape sequence (`ESC[99t`) → ignored, no response, no panic**
- **Partial probe at chunk boundary (probe split across two reads) → matched and answered on second read**

**JSONL parser** (`tests/transcript.rs`):
- Single assistant turn, single text block → correct text
- Multi-block content: text + tool_use + thinking + text → text blocks concatenated, others skipped
- Multi-turn: 3 unique usage keys → 3 unique turns, last turn's text returned
- Streaming duplicate dedup: 5 consecutive events with identical usage → counted as 1 turn
- Token aggregation: 45 unique turns → correct sum across all 4 token fields
- Missing `cache_creation_input_tokens` in usage → defaults to 0, no panic
- `input_tokens: null` in usage → treated as 0
- **Unknown event type (`"type": "new-future-event"`) → silently skipped, parse continues**
- **Unknown content block type (`"type": "image"`) → silently skipped, text blocks still extracted**
- **Unknown fields in `usage` object → silently ignored, known fields still parsed**
- Malformed JSONL line (truncated JSON) → line skipped, subsequent lines parsed
- Empty file → returns empty text, zero token counts (no panic)

**Stop hook parser** (`tests/hook.rs`):
- Full payload → all fields extracted
- Missing `transcript_path` → fallback path derived from `session_id` + `cwd`
- Missing `last_assistant_message` → `None` (retry-only fallback)
- **Unknown top-level fields in payload → silently ignored**
- Malformed JSON → `Err`, triggers exit 2

**Emitter** (`tests/emitter.rs`):
- `text`: correct string, trailing newline, no extra whitespace
- `json`: valid JSON, all required fields present, `claude_version` included
- `json`: `usage` fields are integers not strings
- `stream-json`: each line parses as independent JSON object
- Error result: `is_error: true`, correct `subtype` string, non-zero exit
- Zero token counts when fallback path taken: `usage` present with all-zero values

**Startup sequencer** (`tests/startup.rs`):
- Trust keywords `trust` + `Allow` in same line → CR sent immediately
- Trust keywords in different lines of same chunk → CR sent
- **Alternative wording `continue` + `folder` → CR sent** (keyword union logic)
- **Arbitrary unknown welcome text (no keywords) → fallback: CR after 0.4 s quiet**
- WAITING state persists for 45 s with fewer than 200 bytes received → error returned (covers zero-byte case and partial-output hang; if ≥ 200 bytes arrive before 45s, the idle fallback fires after 0.4s of quiet)
- 199 bytes received then quiet for 0.4 s → no CR yet (minimum 200 bytes enforced)
- 200 bytes received then quiet for 0.4 s → CR sent
- Additional PTY output before either quiet window expires → restart that window; do not dismiss or inject while the TUI is still rendering

**CLI** (`tests/cli.rs`):
- Positional prompt → forwarded correctly
- `--input-file` overrides stdin
- Stdin used when not a TTY and no other prompt source
- Conflicting prompt sources → error with clear message
- `--timeout 0` → error (must be positive)
- `--output-format invalid` → error listing valid values
- `--claude-binary /custom/path` → spawns that binary, not PATH lookup
- **`--version` output parses as `"claude-print X.Y.Z (wrapping claude A.B.C)"`**

### Mock PTY Integration Tests (`tests/integration/`)

All integration tests invoke `claude-print --claude-binary <path-to-mock_claude>`. The path is resolved in `tests/integration/mod.rs` using `env!("CARGO_MANIFEST_DIR")` plus the known `target/debug/mock_claude` output path from the `test-fixtures/mock-claude` workspace member. Mock behavior is set via env vars passed to the `mock_claude` process.

A `mock_claude` binary (compiled as a test fixture, not a shell script) simulates Claude Code's startup behavior. Built in a separate Cargo workspace member `test-fixtures/mock-claude/` so it compiles to a native binary with controlled behavior. Controlled via env vars:

| Env var | Effect |
|---------|--------|
| `MOCK_TRUST_DIALOG=1` | Emit trust dialog text before REPL |
| `MOCK_TRUST_WORDING=alternate` | Use different trust wording (`Continue` instead of `Allow`) |
| `MOCK_OMIT_TRANSCRIPT_PATH=1` | Omit `transcript_path` from Stop payload |
| `MOCK_OMIT_LAST_MESSAGE=1` | Omit `last_assistant_message` from Stop payload |
| `MOCK_DELAY_JSONL=<ms>` | Write final JSONL event after N ms delay (race simulation) |
| `MOCK_UNKNOWN_PROBE=1` | Emit unknown ESC sequence before DA1 |
| `MOCK_UNKNOWN_EVENT_TYPE=1` | Write unknown event type to transcript JSONL |
| `MOCK_UNKNOWN_USAGE_FIELDS=1` | Add extra fields to usage object |
| `MOCK_RESPONSE=<text>` | Response text to write into transcript |
| `MOCK_TURNS=<n>` | Number of assistant turns to simulate |
| `MOCK_EXIT_BEFORE_STOP=1` | Exit without firing Stop hook |
| `MOCK_DELAY_STOP=<ms>` | Fire Stop after delay |
| `MOCK_IS_ERROR=1` | Write `is_error: true` to transcript result event |
| `MOCK_STOP_BEFORE_INJECT=1` | Fire Stop hook immediately, before trust dismiss |
| `MOCK_SILENT=1` | Emit no startup output; never fire Stop hook; block indefinitely (used to test timeout paths). |

*All env vars listed above are exercised by at least one scenario in the integration test table. `MOCK_DELAY_STOP` is used in the SIGINT and "Stop hook never fires" scenarios.*

Integration test scenarios:

| Scenario | Mock config | Assertion |
|----------|------------|-----------|
| Happy path | defaults | exit 0, correct response text, non-zero token counts |
| Trust dialog (standard wording) | `MOCK_TRUST_DIALOG=1` | exit 0 |
| **Trust dialog (alternate wording)** | `MOCK_TRUST_DIALOG=1 MOCK_TRUST_WORDING=alternate` | exit 0 (resilience) |
| No startup output | `MOCK_SILENT=1` | exit 2 after timeout |
| Child exits before Stop | `MOCK_EXIT_BEFORE_STOP=1` | exit 2 |
| Stop hook never fires | `MOCK_DELAY_STOP=99999` | exit 124 |
| Transcript race | `MOCK_DELAY_JSONL=100` | retry loop fires, exit 0 |
| Missing `transcript_path` | `MOCK_OMIT_TRANSCRIPT_PATH=1` | path derived, exit 0 |
| Missing `last_assistant_message` | `MOCK_OMIT_LAST_MESSAGE=1` | retry-only path, exit 0 |
| **Both omitted + delayed JSONL** | `MOCK_OMIT_LAST_MESSAGE=1 MOCK_DELAY_JSONL=200` | retries suffice, exit 0 |
| Error in transcript | `MOCK_IS_ERROR=1` | exit 1, `is_error: true` in output |
| SIGINT | `MOCK_DELAY_STOP=5000` + send SIGINT at 1 s | exit 130, child killed |
| Multi-turn | `MOCK_TURNS=3` | last turn text returned, 3 turns in token sum |
| Large prompt (>32KB) | (no mock env var needed; test harness sends a 33 000-byte string as stdin; mock_claude reads stdin verbatim and reflects it in the transcript JSONL) | file relay used, exit 0 |
| **Unknown probe emitted** | `MOCK_UNKNOWN_PROBE=1` | probe ignored, session completes |
| **Unknown event type in JSONL** | `MOCK_UNKNOWN_EVENT_TYPE=1` | parse succeeds, text extracted |
| **Unknown usage fields** | `MOCK_UNKNOWN_USAGE_FIELDS=1` | ignored, token counts correct |
| Custom response text | `MOCK_RESPONSE=hello` | response field in json output equals 'hello' |
| `--no-inherit-hooks` | `--no-inherit-hooks` flag set | appropriate `--setting-sources` arg in child argv (either `=` or `=none` per OQ-2 resolution), exit 0 |
| Output format json | defaults | output parses as valid JSON |
| Output format stream-json | defaults | each output line parses as valid JSON |
| Stop fires before PROMPT_INJECTED | `MOCK_STOP_BEFORE_INJECT=1` | exit 2, `is_error: true` in output (EC-7 path) |

### Hook Inheritance Tests (`tests/hooks.rs`)

These tests verify that `--settings` relay hook merges correctly and that `--no-inherit-hooks` suppresses user hooks.

**Settings merge (default mode):**
- Verify `--settings <temp>/settings.json` is always passed to mock_claude
- Verify the relay hook fires (Stop payload arrives on FIFO)
- With `mock_claude` simulating additional hooks in user settings: both user hook + relay hook fire
- `--settings` flag is present in the child process argv (visible via `/proc/<pid>/cmdline`)

**`--no-inherit-hooks` flag:**
- The appropriate `--setting-sources` argument is present in child argv when flag is set — either `--setting-sources=` (empty value, per OQ-2 primary) or `--setting-sources=none` (per PO-2 fallback). The test MUST be parameterized over both valid forms and accept whichever is generated by the current implementation. The specific form used MUST match what was verified in OQ-2 resolution.
- `--setting-sources` is absent from child argv when flag is not set
- Mock that tracks whether a "user hook" fires: with `--no-inherit-hooks`, user hook does not fire; without, it does

**Temp dir lifecycle:**
- After a successful run, `$TMPDIR` contains no leftover `claude-print-*` directories
- After a panicked/early-exit run (simulated), TempDir drop cleans up
- `hook.sh` and `stop.fifo` paths are within the temp dir (not in user-visible locations)

**Hook script correctness:**
- `hook.sh` writes exactly the stdin payload to the FIFO (no modification, no extra newline)
- `hook.sh` exits 0 even if FIFO write fails (fire-and-forget)

**`--verbose` trace:**
- With `--verbose`, stderr includes: temp dir path, `--settings` path, `--no-inherit-hooks` status

### Version-Resilience Test Suite (`tests/version_compat.rs`)

A dedicated test module that verifies the binary survives schema changes across Claude Code versions. These tests run in CI on every push as part of the standard `claude-print-ci` WorkflowTemplate.

**Schema migration tests** (property-based, using `serde_json::Value` to construct arbitrary payloads):
- Stop payload with 50 unknown extra fields → parsed without error
- Usage object with 20 new numeric fields → all ignored, 4 known fields correct
- Content block with new required field → `#[serde(other)]` catches it as Unknown
- JSONL with events in a new order (e.g., `summary` before `user`) → no assumption on ordering

**`claude --version` compatibility tracker:**
```rust
fn test_claude_version_recorded() {
    let output = Command::new("claude").arg("--version").output().unwrap();
    let version_str = String::from_utf8_lossy(&output.stdout);
    // Verify output is parseable (not checking the specific version)
    assert!(version_str.contains("Claude Code"), "unexpected claude --version format: {}", version_str);
    // Write to test artifact for CI diff tracking
    std::fs::write("target/last-claude-version.txt", version_str.as_bytes()).ok();
}
```

CI stores `last-claude-version.txt` as a build artifact. On the next run, if the version changed, a warning is printed and the full integration suite re-runs.

**Startup heuristic stability test:**
- Generate 20 different trust dialog phrasings (varied keyword combinations)
- For each: verify `should_dismiss(line)` returns true
- Generate 10 non-dialog lines (ANSI art, progress bars, empty lines)
- For each: verify `should_dismiss(line)` returns false

**Token count regression test:**
- Fixture: `tests/fixtures/transcript_v2.1.168.jsonl` — a real captured transcript
- Assert: token sum matches hardcoded expected values
- When a new Claude version produces transcripts with a different schema, add a new fixture and assert on the new values. Both old and new fixtures must pass simultaneously (the parser handles both)

### Conformance Harness

The `test_output_format_wire_compat` test verifies `claude-print` JSON output is structurally identical to `claude -p --output-format json`. It runs against `mock_claude` (no credentials needed):

1. Run `claude-print --output-format json <prompt>` with `mock_claude`
2. Assert all fields present in the `claude -p` wire format are present
3. Assert `is_error=false`, `type=result`, `usage` object has all four token fields as integers
4. The extra `claude_version` field MUST NOT cause a parse failure in a strict JSON parser (tested with `serde_json` `deny_unknown_fields` on a `claude -p`-shaped struct)

For billing conformance (AS-4, credential-required), the daily canary passes its exact session transcript to `scripts/check-billing.sh` and records a machine-readable PASS/FAIL result. With no path argument, `check-billing.sh` still inspects the most recent JSONL; run that manual form before every release.

### Definition of Done

A phase or PR is done when ALL of the following hold:
- `cargo fmt --check` passes
- `cargo clippy -- -D warnings` passes
- `cargo test` passes with zero failures (all mocked tests, no credentials needed)
- No `unsafe` blocks added without a comment explaining why
- No new `unwrap()` calls in non-test code
- Integration tests cover the new phase's completion criterion
- INV-1 (temp dir cleanup) verified for any new exit path

All-gates policy: every commit that reaches the CI step MUST pass all gates simultaneously. No "fix tests separately" commits.

### End-to-End Tests (credential-required, excluded from CI, run manually)

```bash
# Basic
echo "Say hello" | claude-print
claude-print --output-format json "What is 2+2?"
claude-print --output-format stream-json "List 5 animals"

# Tool use
claude-print --allowedTools Bash --dangerously-skip-permissions "Run: echo hello"

# Billing verification
# After running: check transcript entrypoint field
python3 -c "
import json, glob
for path in sorted(glob.glob('/home/coding/.claude/projects/**/*.jsonl', recursive=True))[-1:]:
    for line in open(path):
        obj = json.loads(line)
        if ep := obj.get('entrypoint'):
            print('entrypoint:', ep)
            break
"
# Expected: entrypoint: cli  (not sdk-cli)

# NEEDLE integration
needle run --agent claude-print --workspace /home/coding/some-project
```

## Security

### Threat Model

| # | Threat | Attacker | Surface | Impact | Mitigation |
|---|--------|---------|---------|--------|-----------|
| T-1 | FIFO hijack | Local user on same machine | `$TMPDIR` world-readable by default | Attacker reads the Stop payload (session_id, prompt text) | Create temp dir with mode 0700 via `tempfile::Builder::new().mode(0o700)`. |
| T-2 | Prompt injection via `--input-file` | Any caller | `--input-file` path argument | Read arbitrary file contents as the prompt | `--input-file` is resolved to an absolute path and size-checked before use. Null bytes rejected. |
| T-3 | Environment variable leakage | None (ambient) | Inherited env of parent process | `CLAUDE_CODE_SESSION_ID` / `CLAUDE_CODE_SESSION_KIND` confuse child session identity | Unset both before `execvp` (EC-11). |
| T-4 | Temp dir path with shell metacharacters | Filesystem | hook.sh path interpolation | Command injection if `hook.sh` uses shell expansion | `hook.sh` uses `cat > <literal-path>` with the FIFO path embedded at write time — no variable expansion at hook execution time. The FIFO path is written as a shell single-quoted string: `cat > '<path>'`. Single quotes prevent all shell interpretation. If the path contains a single quote character (extremely unlikely in `$TMPDIR` output from `tempfile`), reject it at temp-dir creation time. |
| T-5 | PTY escape sequence injection from response | Malicious assistant response | ANSI sequences in prompt/response | Terminal control of caller's terminal | `claude-print` does not forward raw PTY output to its stdout. Output is extracted from JSONL as plain text. |
| T-6 | PATH hijack | Local attacker with PATH control | PATH lookup of `claude` binary | Malicious binary intercepts all sessions; billing classification undetectable | Users can set `claude-binary` to an absolute path in `config.toml` as hardening. Out of scope for v1.0 signature verification. |

### Untrusted Input Policy

- **Prompts** (positional, stdin, `--input-file`): content is forwarded verbatim to claude via bracketed paste. Null bytes rejected. Size capped at 32KB before file relay.
- **Stop hook payload**: parsed with lenient serde (`Option<T>` for all fields). Malformed JSON → exit 2. Path values from payload are validated before use as filesystem paths.
- **JSONL transcript**: parsed with lenient serde. Malformed lines skipped. No eval or dynamic dispatch on transcript content.

### Supply Chain

- All dependencies pinned in `Cargo.lock`.
- `cargo audit` run in CI on every push.
- The `claude` binary being spawned is resolved from PATH (or `--claude-binary`). `claude-print` does not verify the binary's signature — this is out of scope for v1.0.

## Performance

### Budgets

| Metric | Target | How Measured |
|--------|--------|-------------|
| Startup overhead (invocation → prompt injection) | < 5 s | `--verbose` trace timestamps |
| Transcript-to-output latency after Stop | < 2 s | Retry loop bound: 40 × 50 ms |
| Binary size (musl static) | < 10 MB | `ls -lh target/x86_64-unknown-linux-musl/release/claude-print` |
| Memory (RSS at steady state) | < 50 MB | `/proc/<pid>/status VmRSS` during integration test |
| PTY read-to-write round-trip (probe response) | < 1 ms | Not CI-gated; verified by Ink not hanging |

### Benchmark Contract

Overhead is measured as wall-clock time from process start to the bracketed paste write timestamp (logged at PROMPT_INJECTED transition in `--verbose` mode). This excludes model latency, which is outside `claude-print`'s control.

### CI-Gated Benchmarks

Binary size is checked in CI: after the musl release build, `ls -lh` the binary and fail if > 10 MB. No runtime performance benchmarks in CI (they require credentials or complex mock setup). Performance is validated manually against the budgets above before each release.

### Scalability Limits

`claude-print` is designed for at most ~20 concurrent invocations on the same machine (matching NEEDLE fleet size). Each instance holds one PTY fd pair and one temp dir. No per-instance memory scaling concerns. Maximum transcript size: bounded by disk; the reader loads one line at a time, not the whole file.

## Operations

### Migration Plan

Users currently calling `claude -p` in scripts, Makefiles, or NEEDLE configs:
1. Install `claude-print` via `install.sh`
2. Replace `claude -p` with `claude-print` (all other flags identical)
3. Replace `claude -p --output-format json` with `claude-print --output-format json` (output is a superset: adds `claude_version` field; strict parsers unaffected if using field-name access)
4. NEEDLE: swap agent YAML from `claude-anthropic-sonnet.yaml` to `claude-print.yaml`

No data migration required. Transcripts from before the switch remain in `~/.claude/projects/` and are unaffected.

### Backward Compatibility Stance

`claude-print` follows **semver** for its own output format:
- **Patch** (X.Y.Z): bug fixes; output format unchanged.
- **Minor** (X.Y.0): new optional output fields (additive); new flags. Existing callers unaffected.
- **Major** (X.0.0): breaking output format change or flag removal. Requires caller update.

The `claude_version` field is additive (minor) and will not be removed in a major release — it is needed for version-regression debugging.

### Rollout / Rollback Criteria

- **Promote to stable:** AS-1 through AS-6 pass; the daily AS-4 canary is healthy and AS-4 is verified manually; no open P0 bugs.
- **Roll back:** If AS-4 fails (entrypoint is `sdk-cli`), immediately pull the release from the CI artifact store and revert the install. The previous binary is always preserved as `claude-print.prev` by `install.sh`.

### Monitoring and Alerting

`claude-print` emits no metrics itself. A daily systemd user timer on ex44 and lab runs one cheap Haiku session, checks its exact transcript with `scripts/check-billing.sh`, and writes an atomic result to `~/.local/state/claude-print/billing-canary/last-result`. Every run also emits a `CLAUDE_PRINT_BILLING_CANARY status=PASS|FAIL` journal line and exits non-zero on failure. Operators or a heartbeat monitor alert when the result is `FAIL`, missing, or older than 48 hours.

The manual `scripts/check-billing.sh` release gate remains required as a belt-and-suspenders second layer. Reviewing NEEDLE transcripts for unexpected `entrypoint: sdk-cli` remains useful corroboration.

### Doctor Command (`--check`)

`claude-print --check` runs a self-test with no credentials needed:
1. Verify `claude` binary found on PATH (or `--claude-binary`)
2. Verify `openpty()` succeeds and returns two valid fds
3. Verify `mkfifo` works in `$TMPDIR`
4. Spawn `mock_claude` (installed alongside the main binary by `install.sh`) and verify a basic PTY round-trip — `mock_claude` is resolved from the same directory as `claude-print` itself, not hardcoded to `~/.local/bin/`. If `claude-print` is at `~/.local/bin/claude-print`, `mock_claude` is expected at `~/.local/bin/mock_claude`. If `mock_claude` is not found at the expected path (e.g., because `SKIP_MOCK_CLAUDE=1` was used during install), step 4 emits a warning `mock_claude not found — skipping PTY round-trip test` and proceeds. The `--check` exits 0 with steps 1–3 verified.
5. Scan `$TMPDIR` for leftover `claude-print-*` directories older than 1 hour and report them as warnings (does not fail the check). Example message: `WARNING: found orphaned temp dir /tmp/claude-print-12345-abc (1.2h old) — run rm -rf to clean up`.
6. Print `OK` or a specific failure message per step

`install.sh` runs `--check` after installation. `--check` exits 0 on success, 2 on failure.

## Risk Register

| # | Risk | Likelihood | Impact | Mitigation |
|---|------|-----------|--------|-----------|
| R-1 | Claude Code update changes `isatty()` detection logic; `cc_entrypoint` silently becomes `sdk-cli` | Low | Critical (billing regression, all sessions misclassified) | Daily credential-backed AS-4 canary with PASS/FAIL state and journal output; manual AS-4 check before every release; `--verbose` shows PTY slave assigned; `--check` verifies PTY opens |
| R-2 | `--settings` merge behavior changes in a Claude Code update; user hooks stop firing | Medium | Medium (user hooks silently broken) | PO-1 verified before Phase 2; version-compat tests track `claude --version`; CI alert on version change |
| R-3 | Ink adds a new mandatory terminal probe; session hangs indefinitely | Low | High (complete outage for new Claude Code versions) | Unknown probes are ignored; session falls through to idle timeout; `MOCK_UNKNOWN_PROBE` integration test verifies resilience |
| R-4 | `login_tty` absent in musl-libc | Low | High (binary fails to build) | Inline implementation (PO-3 recovery) is 4 syscalls; verified before Phase 2 |
| R-5 | FIFO race: Stop hook fires before read-end open | Low | Medium (payload lost; exit 2) | FIFO opened before prompt injection (EC-3, INV-3); integration test `test_fast_stop_hook` validates timing |
| R-6 | JSONL schema changes break transcript parsing | Medium | High (empty response, exit 1 for all sessions) | `#[serde(default)]` + `#[serde(other)]` on all structs; property-based schema tests; version-compat fixture suite |
| R-7 | Temp dir cleanup fails on panic; disk fills over time | Low | Low (disk leak, recoverable with `rm -rf /tmp/claude-print-*`) | `tempfile::TempDir` drop on panic; INV-1 integration test; `--check` can scan for orphaned dirs |

## ADRs

### ADR-001: No `CLAUDE_CONFIG_DIR` Redirect

**Decision:** Do not set `CLAUDE_CONFIG_DIR` in the child environment.

**Context:** An early design redirected all claude I/O to a per-run sandbox directory using `CLAUDE_CONFIG_DIR`, then forwarded transcripts to `~/.claude/`. This was replaced.

**Rationale:** The `--settings` overlay achieves the only goal that required redirection (injecting the relay hook). Redirecting `CLAUDE_CONFIG_DIR` requires symlinking credentials, duplicating settings, and forwarding transcripts — all complexity with no benefit. Transcripts land in `~/.claude/projects/` natively, which is exactly what we want.

**Consequences:** Transcripts always land in `~/.claude/projects/`. User hooks always fire (unless `--no-inherit-hooks`). No transcript forwarding logic needed.

### ADR-002: Synchronous `poll()` Over Async Runtime

**Decision:** Use `nix::poll::poll()` synchronously; no `tokio` or `async-std`.

**Context:** The event loop monitors at most 3 file descriptors: `master_fd` (always), `self_pipe_read` (always), and `stop_fifo` (added at PROMPT_INJECTED). A reader thread handles stream-json output.

**Rationale:** Async runtimes add binary size (~2 MB), compile time, and conceptual complexity. The workload is I/O-bound on 2–3 fds with no parallelism benefit. A single `poll()` call + one reader thread is the simplest correct model.

**Consequences:** `stream-json` mode uses `std::sync::mpsc`. All new I/O (if added in future versions) must be registered with the `poll()` call or pushed to a thread.

### ADR-003: `message.id` Primary Dedup with Fingerprint Fallback

**Decision:** Deduplicate streaming JSONL events by `message.id` (primary) with usage-fingerprint fallback.

**Context:** Claude Code writes multiple `assistant` events per API call when streaming. They share identical `message.usage` but have a unique `message.id`. Token counts must be summed once per API call, not once per event.

**Rationale:** `message.id` is stable across Claude Code versions and is the authoritative dedup key. The fingerprint fallback handles older versions that may omit `message.id`. Using fingerprint alone risks false dedup if two consecutive API calls have identical usage (unlikely but possible). Using `message.id` alone risks double-counting on older versions.

**Consequences:** Both `seen_ids: HashSet<String>` and `prev_usage_key: Option<UsageKey>` are maintained. Memory cost is O(unique API calls) per session — negligible.

### ADR-004: NEEDLE Workers Must Use Configured Agent — No Silent Escalation

**Decision:** A NEEDLE worker dispatching beads from this workspace MUST use its configured agent adapter (e.g., `claude-code-glm-47`). No strand may silently escalate to a different model (e.g., `claude-sonnet`) based on bead complexity or adapter availability.

**Context:** During Phase 6–11 completion, the `claude-print-bravo` worker's configured adapter (`claude-code-glm-47`) was not found in the dispatcher at runtime. NEEDLE's `resolve_adapter()` silently fell back to the `claude-sonnet` built-in. `claude-sonnet` uses `unbuffer -p claude` which allocates a PTY for stdout; `needle-transform-claude` then receives escape sequences instead of `stream-json`, causing `transform.failed` with `exit -1`. The bead was dispatched at sequence 97882, timed out after exactly 600 s (exit 124), failed to release (`bead.release.failed`), and was lost when the mend cycle rebuilt the DB. This blocked the main() wiring bead indefinitely.

**Rationale:** `claude-print` exists specifically because the PTY-vs-pipe distinction determines billing classification. A worker running in this workspace that silently switches to a PTY-based agent inverts the very invariant the project enforces. Beyond billing, the transform failure silently destroys progress: the bead times out, can't release, and disappears from the DB. Silent degradation is worse than loud failure.

**Consequences:**
- NEEDLE `resolve_adapter()` must fail loudly if the configured adapter is not found (NEEDLE beads bf-14w, bf-2wi track this fix).
- All implementation beads in this workspace carry `--label atomic` to suppress the mitosis strand's forced-split behavior, which can also destroy beads when combined with release failures.
- When launching workers for this workspace, always verify the agent adapter file is present before dispatch: `ls ~/claude-config/agents/claude-code-glm-47/` or equivalent.

### ADR-005: 2026-07-20 — Opt-In Warm PTY Pool for NEEDLE-Fleet Latency (v2, Deferred)

**Decision:** Add a new, entirely opt-in `claude-print serve --pool-size N` daemon mode that pre-forks and pre-warms `claude` PTY processes (through trust-dismiss and idle-settle, but never past prompt injection) and hands them out over a local Unix domain socket to ordinary `claude-print` invocations that pass `--pool-socket <path>` (or find a default socket). If no pool is reachable, `claude-print` behaves exactly as it does today — this is additive, not a replacement for the default stateless path. Scoped as v2 / future work: this ADR records the decision and rationale; implementation is tracked via beads, not built in this pass.

**Context:** `claude-print`'s real production usage is not one-off ad-hoc calls — it is the NEEDLE fleet's `claude-print-sonnet.yaml` adapter (`~/.needle/agents/claude-print-sonnet.yaml`), which dispatches a fresh `claude-print` invocation per bead, across up to ~20 concurrent workers (the documented Scalability Limits ceiling), all day. At the time of this decision, every invocation paid the full cost from zero: `fork()`, `openpty()`, `execvp(claude)`, Claude Code's own startup (including MCP server init), the trust-dialog wait, and the then-2.0s post-CR idle settle — none of it was amortized across invocations.

Empirical measurement (2026-07-20, this audit, wrapping locally-installed `claude-print` 0.2.0 / `claude` 2.1.215): a maximally trivial round trip —
```
echo "Reply with exactly one word: pong" | claude-print --output-format json --timeout 45
```
— took **10.2–13.2s wall-clock** across two runs, for a one-word answer. The Performance Budgets table targets "<5s" for *startup overhead alone* (invocation → prompt injection), a sub-metric that excludes model latency — but even granting that the remainder is Claude Code/model latency outside `claude-print`'s control, the fixed per-invocation cost is being re-paid by every bead the fleet dispatches, with zero reuse. At fleet scale this is the single largest aggregate latency/throughput lever available for this artifact — larger than any output-format or reliability tweak, because it is multiplicative across every dispatch rather than a one-time fix.

**Alternatives Considered:**
1. **Do nothing / accept the cost.** Rejected — the cost is paid on every one of potentially hundreds of daily NEEDLE dispatches; it is the dominant lever for fleet throughput, not a minor rough edge.
2. **True multi-turn reuse — keep one `claude` REPL alive and `/clear` + resubmit for each new bead, instead of spawning fresh processes.** Rejected — this would violate the explicit Non-Goal "Multi-turn interactive sessions," and more concretely risks context/prompt bleed between unrelated beads if a `/clear` is incomplete or races with tool output — a correctness/isolation regression far worse than the latency it would save. Process-per-session isolation is a feature, not overhead to be optimized away.
3. **Shrink the fixed idle-wait constants (0.8s trust-dismiss fallback, 2.0s post-CR settle) instead of pooling.** Not rejected as an independent improvement, but rejected as the *primary* fix: most of the observed 10–13s is `claude`'s own startup/MCP-init and model inference, not these two constants. This follow-up subsequently reduced them to 0.4s/1.0s quiet windows that restart on PTY output, preserving the redraw race guards while saving up to 1.4s in settled fast-start cases; it does not address the dominant cost that motivated pooling.
4. **Embed pool logic in every `claude-print` invocation (self-electing leader/coordinator) instead of a separate long-lived manager process.** Rejected — adds locking/coordination complexity (concurrent invocations racing to become "the pool") for no benefit over one long-lived `serve` process, and cuts against the simplicity bias already established by ADR-002 (synchronous `poll()` over an async runtime).

**Consequences:**
- This is a genuinely new component (a daemon subcommand + a small local IPC surface — JSON request/response over a Unix socket, reusing the existing bracketed-paste/Stop-FIFO machinery unchanged inside the pool manager), not a tweak to the existing event loop's state machine — hence it is scoped as v2, opt-in, and deferred rather than folded into a v0.2.x patch.
- The default `claude-print` invocation path is completely unchanged when no pool is configured — HR-1 (single static binary, no runtime deps) and the Scope Lock's "no runtime dependency" clause hold for the default path; the pool manager is the same musl-static binary running in a different subcommand, not a new dependency.
- Every pooled invocation still gets a unique `claude` process and session per prompt — no cross-request identity leakage, no change to the "one prompt per invocation" contract callers see.
- New invariants will be needed before implementation (tracked as follow-up, not yet written as formal INV-N entries): e.g. "a pool member is never handed a second prompt without first being torn down and replaced," and "pool manager crash/restart never leaves a `claude-print` client blocked past `--timeout`."
- Implementation is intentionally NOT started in this pass — this ADR exists to record the decision and scope it correctly for whoever picks it up next, so it isn't re-litigated from scratch.

## Open Questions

Unresolved questions are mapped to the phase they block. Each MUST be resolved before that phase begins.

| # | Question | Blocks | Resolution / Fallback |
|---|---------|--------|----------------------|
| OQ-1 | Does `--settings <file>` merge hooks with `~/.claude/settings.json` or replace them? | Phase 2 | Verify by running `claude` with `--settings` containing a test hook alongside a real user hook and checking both fire. If merge fails: PO-1 fallback (merge in-process). Also verify hook firing order: confirm user hooks run before or after the relay hook. If relay fires first, confirm this does not cause a read race with user Stop hooks that post-process the JSONL (e.g., ccdash). |
| OQ-2 | Does `--setting-sources=` (empty string) suppress all standard sources? | Phase 6 | Verify by running `claude --setting-sources= --settings <relay-only-file>` and checking user hooks do not fire. If not accepted: try `--setting-sources=none`; if neither works, enumerate relay source explicitly. |
| OQ-3a | Is `/read` a built-in slash command (always available) vs. a tool invocation (requires allowedTools)? | — | **Resolved.** Confirmed built-in slash command; does not require `Read` in `--allowedTools`. |
| OQ-3b | Does `/read` accept absolute paths for prompts >32 KB? | Phase 5 | End-to-end test with a 33 KB prompt file at an absolute path. If not: PO-6 fallback (truncate at 32 KB). |
| OQ-4 | FIFO open race: will O_NONBLOCK open-before-inject reliably prevent timing issues? | Phase 6 | Validated by `test_fast_stop_hook` integration test (MOCK_DELAY_STOP=0). If race occurs in practice, add a pre-prompt-inject `poll()` to confirm FIFO open. |
| OQ-5 | Is `login_tty` available in `x86_64-unknown-linux-musl`? | Phase 2 | Attempt compilation before Phase 2 begins. If absent: inline 4-syscall implementation (PO-3 recovery). **Resolve before writing Phase 2 code.** |
| OQ-6 | Do `CLAUDE_CODE_SESSION_ID` / `CLAUDE_CODE_SESSION_KIND` from a parent session confuse the child? | Phase 2 | Unset both in child env before `execvp` as a precaution. Test by running `claude-print` from inside an active `claude` session and verifying the child gets its own session identity. |

## CI/CD

### Overview

`claude-print` ships as a static musl binary. All CI/CD runs on Argo Workflows in the `iad-ci` cluster. GitHub Actions are disabled — never re-enable them.

WorkflowTemplate location: `jedarden/declarative-config → k8s/iad-ci/argo-workflows/claude-print-ci.yaml`

ArgoCD app `argo-workflows-ns-iad-ci` auto-syncs on push to `declarative-config`.

### WorkflowTemplate: `claude-print-ci`

Two trigger paths:
1. **PR / branch push** — verify only (fmt + clippy + test); no release.
2. **Release tag** (`v*`) — verify, then build musl binary, then create GitHub release.

Template structure (conceptual — final YAML lives in declarative-config):

```
entrypoint: main
arguments:
  parameters:
    - name: repo          # git.ardenone.com/jedarden/claude-print
    - name: revision      # branch name or tag name
    - name: tag           # set by caller; empty on branch push

steps:
  - [verify]              # rust-verify WorkflowTemplate ref (fmt + clippy + test)
  - [build-musl]          # only if tag is non-empty
  - [github-release]      # only if tag is non-empty
```

### Step: verify

Delegates to the existing `rust-verify` WorkflowTemplate (fmt + clippy + test). No duplication. If `rust-verify` is not yet parameterized for arbitrary repos, add a `repo` parameter — do not inline the verify steps. Note: if `rust-verify` does not already include `cargo audit`, add it as an explicit step in `claude-print-ci` between `verify` and `build-musl`. The Phase 11 checklist MUST include `cargo audit` verification either way.

### Step: build-musl

```yaml
container:
  image: ghcr.io/jedarden/rust-musl-builder:latest   # or equivalent
  command: [sh, -c, "git clone {{inputs.parameters.repo}} /workspace &&
    git -C /workspace checkout {{inputs.parameters.revision}} &&
    cd /workspace &&
    cargo build --release --target x86_64-unknown-linux-musl &&
    mv /workspace/target/x86_64-unknown-linux-musl/release/claude-print /workspace/claude-print-x86_64-linux &&
    mv /workspace/target/x86_64-unknown-linux-musl/release/mock_claude /workspace/mock_claude-x86_64-linux"]
  env:
    - name: CARGO_TERM_COLOR
      value: never
outputs:
  artifacts:
    - name: binary
      path: /workspace/claude-print-x86_64-linux
    - name: mock-binary
      path: /workspace/mock_claude-x86_64-linux
```

The `cargo build` step also builds `mock_claude` from the `test-fixtures/mock-claude/` workspace member (it is declared as a workspace member in the root `Cargo.toml`, so a single `cargo build --release` compiles both). After the build, both binaries are renamed for upload: `claude-print` → `claude-print-x86_64-linux`, `mock_claude` → `mock_claude-x86_64-linux`.

Both binaries MUST be statically linked and self-contained. Verify with `file <binary>` — must say "statically linked".

### Step: github-release

Uses `gh release create` with the artifacts from build-musl:

```sh
gh release create "${TAG}" \
  --repo jedarden/claude-print \
  --title "${TAG}" \
  --notes "Release ${TAG}" \
  claude-print-x86_64-linux \
  mock_claude-x86_64-linux
```

Asset naming convention: `claude-print-x86_64-linux` and `mock_claude-x86_64-linux` (no version in filenames — the release tag provides the version). This matches `install.sh`'s `${ARCH}-linux` target naming.

### Release Tag Convention

Tags follow semver: `v<MAJOR>.<MINOR>.<PATCH>`. Tags are pushed manually (`git tag vX.Y.Z && git push origin vX.Y.Z`). The workflow is submitted manually or via Argo Events webhook on tag push (out of scope for v1.0; manual workflow submission is sufficient for initial releases).

### Submitting CI Manually

```bash
kubectl --kubeconfig=/home/coding/.kube/iad-ci.kubeconfig create -f - <<EOF
apiVersion: argoproj.io/v1alpha1
kind: Workflow
metadata:
  generateName: claude-print-ci-manual-
  namespace: argo-workflows
spec:
  workflowTemplateRef:
    name: claude-print-ci
  arguments:
    parameters:
      - name: repo
        value: "git.ardenone.com/jedarden/claude-print"
      - name: revision
        value: main
      - name: tag
        value: ""     # empty = verify only; set to "vX.Y.Z" for release
EOF
```

### Implementation Placement

- **Phase 1**: Add `claude-print-ci.yaml` stub to declarative-config (verify step only; no release). Create `jedarden/claude-print` repo on GitHub if not already done.
- **Phase 11** (CI): Add build-musl and github-release steps to the template, matching the phase completion criterion in the Implementation Phases section.
- CI also builds `mock_claude` as a musl binary and uploads it as a release artifact alongside `claude-print`.

## Documentation

### README.md

The repository README targets two audiences: (a) a human who wants to install and use `claude-print`, and (b) an AI agent that needs to invoke it programmatically.

**Required sections** (in order):

1. **One-line description** — "Drop-in replacement for `claude -p` that drives the interactive TUI via PTY, preserving subscription billing after the June 15, 2026 Agent SDK split."

2. **Installation** — `curl`-based one-liner pulling the latest GitHub release asset:
   ```sh
   curl -fsSL https://github.com/jedarden/claude-print/releases/latest/download/claude-print-x86_64-linux \
     -o ~/.local/bin/claude-print && chmod +x ~/.local/bin/claude-print
   ```
   And the `install.sh` variant (from the repo) for NEEDLE agent YAML setup.

3. **Requirements** — `claude` (Claude Code) must be on PATH; Linux x86-64 only; `TMPDIR` must support `mkfifo`.

4. **Quick start** — Three examples:
   ```sh
   # Simple prompt
   echo "What is 2+2?" | claude-print

   # Structured JSON output
   echo "Summarize this" | claude-print --output-format json

   # Streaming (NEEDLE-style)
   echo "Write a Rust function to..." | claude-print --output-format stream-json --max-turns 10
   ```

5. **Output formats** — Brief prose description of `text`, `json`, `stream-json` with a sample of each.

6. **All flags** — Reference the CLI table from §1 of this plan verbatim or as a derived table; keep in sync with `claude-print --help` output.

7. **Exit codes** — Table: 0 = success, 1 = assistant error, 2 = internal error, 124 = timeout, 130 = interrupted.

8. **NEEDLE Integration** — One paragraph explaining the YAML agent config + install step. Link to `~/.needle/agents/claude-print.yaml` or include its contents as a code block.

9. **Self-test** — `claude-print --check` and what each check does.

10. **Troubleshooting** — Two most common failure modes:
    - "PTY open failed" → likely in a container without `/dev/ptmx`; run on a real host.
    - "Session never completes" → check `--timeout`; `--verbose` shows state transitions.

**README must NOT contain**: implementation internals, PTY mechanics, JSONL schema, or billing internals — those live in `docs/`.

### AGENTS.md

`AGENTS.md` lives at the repo root. Its purpose is to give AI agents invoking `claude-print` everything they need in one file, without requiring the agent to read the full plan.

**Required sections** (in order):

1. **Purpose** — One paragraph: what `claude-print` does, why it exists, and why an agent should prefer it over `claude -p`.

2. **Invocation** — The canonical single-turn invocation:
   ```sh
   echo "<prompt>" | claude-print \
     --model claude-sonnet-4-6 \
     --max-turns 30 \
     --output-format stream-json \
     --dangerously-skip-permissions \
     --no-inherit-hooks
   ```
   And the equivalent NEEDLE template form for agents running in NEEDLE context.

3. **Input** — Prompt is read from stdin. Max ~32 KB before `/read` fallback kicks in (OQ-3b). Must be plain UTF-8 text; no shell escaping needed when piped.

4. **Output** — For each `--output-format`:
   - `text`: the assistant's response, verbatim, on stdout. Nothing else.
   - `json`: a JSON object on stdout; list every field (see Emitter §9 and Data Models for the full field list).
   - `stream-json`: A sequence of JSONL lines forwarded verbatim from the Claude Code transcript. On success, the final line is Claude Code's own `{"type":"result", "is_error": false, ...}` event (forwarded as-is; no `claude_version` field). On error, the final line is a synthesized result event: `{"type":"result", "is_error": true, "subtype": "...", "error_message": "...", "claude_version": "..."}`. List the result line fields.

5. **Exit codes** — Same table as README, plus: "On exit ≠ 0, check stderr for a human-readable error message."

6. **Do not** — A short bulleted list of anti-patterns:
   - Do not pass `--dangerously-skip-permissions` in interactive (human-supervised) contexts.
   - Do not read or parse mid-session JSONL files directly — wait for `claude-print` to exit.
   - Do not retry on exit 130 (interrupted) — investigate the cause.
   - Do not set `CLAUDE_CODE_SESSION_ID` in the environment before invoking `claude-print`.

7. **Self-test** — `claude-print --check` exits 0 if the environment can run it.

8. **Version compatibility** — `claude-print` embeds `claude --version` at startup; pass `--verbose` to see it. The `claude_version` field is present in `json` output and in the synthesized error result line of `stream-json` output. In the `stream-json` success path, the final result line is forwarded verbatim from Claude Code and does not contain `claude_version`.

### Docs Organization

`docs/notes/` hosts short decision notes:
- `billing-context.md` — why PTY preserves subscription billing (**already exists**)
- `hook-design.md` — relay hook mechanics, FIFO protocol, keeper fd pattern
- `terminal-probes.md` — Ink startup probe table and response bytes

`docs/research/` hosts external reference material:
- `claude-code-internals.md` — Claude Code TUI behavior observations (**already exists**)
- `pty-mechanics.md` — PTY system call reference (**already exists**)

`docs/plan/plan.md` — the implementation plan (**this file**).

### Implementation Placement

- **Phase 1**: Stub README.md with description, requirements, and placeholder sections.
- **Phase 9** (NEEDLE Integration): Complete README.md (all sections) + write AGENTS.md.
- **Phase 9 acceptance criterion**: `claude-print --help` output matches the README flags table exactly. Any divergence is a CI failure (checked manually before release).
