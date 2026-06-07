# claude-print Plan

## Overview

Single Rust binary that is a drop-in replacement for `claude -p`. It drives the Claude Code interactive TUI via PTY, extracts the response via the Stop hook and JSONL transcript, and emits `claude -p`-compatible output — all while billing against the subscription (`cc_entrypoint=cli`) rather than the Agent SDK credit pool.

## Background

Starting June 15, 2026, Anthropic separates `claude -p` (headless) into a separate monthly credit pool. Only the interactive TUI (`cc_entrypoint=cli`) continues drawing from the unlimited subscription. `claude-print` wraps the TUI in a PTY so callers get `claude -p` wire-compatible output while billing against the subscription.

The billing classification is determined by `isatty(stdout)` inside the `claude` binary at startup:
- PTY slave as stdout → `isatty()` returns true → TUI mode → `cc_entrypoint=cli` → subscription
- Pipe as stdout → `isatty()` returns false → print mode → `cc_entrypoint=sdk-cli` → credit pool

## Delivery

**Single statically-linked binary.** No Python, no runtime dependencies, no pip packages.

```
claude-print          # the binary
install.sh            # copies binary to ~/.local/bin/, installs NEEDLE agent config
```

Built with:
```bash
cargo build --release --target x86_64-unknown-linux-musl   # fully static, no libc dep
```

Distribution: GitHub Release artifact via `claude-print-ci` Argo WorkflowTemplate (same pattern as NEEDLE, SIGIL, ARMOR).

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

## Hook Inheritance and Log Placement

### Default: Inherit User Hooks

By default `claude-print` does **not** redirect `CLAUDE_CONFIG_DIR`. The inner `claude` process:
- Writes its transcript to `~/.claude/projects/<cwd-slug>/<session-id>.jsonl` directly — the same place `claude -p` writes it
- Writes its session entry to `~/.claude/sessions/<pid>.json` (ccdash sees it as a normal session)
- Appends to `~/.claude/history.jsonl`
- Fires all hooks in `~/.claude/settings.json` (SessionStart, Stop, PreToolUse, trail-boss, ccdash, etc.)

`claude-print` adds its own Stop hook by passing `--settings <temp>/settings.json` with the per-run relay hook. Claude Code merges `--settings` with the user's settings file — all existing hooks continue to fire alongside the relay hook.

This matches exactly what `claude -p` does. Transcripts, token counts, and usage stats land in `~/.claude/` with no special handling.

### `--no-inherit-hooks` (Isolation Mode)

When `--no-inherit-hooks` is passed:
- `--setting-sources=` is forwarded to claude (empty value = load no standard settings sources)
- Only `--settings <temp>/settings.json` is loaded, which contains solely the Stop relay hook
- User's `~/.claude/settings.json` hooks do not fire (ccdash, trail-boss, etc.)
- `CLAUDE_CONFIG_DIR` is **not** set even in isolation mode — transcripts still land in `~/.claude/projects/`

Use this when running as a NEEDLE worker to prevent hook noise, or when the user's hooks have side effects (e.g., trail-boss POSTs to a collector that doesn't expect headless sessions).

### Configuration File

`~/.config/claude-print/config.toml` (created with defaults on first run):

```toml
[defaults]
inherit_hooks = true      # pass --setting-sources=user,project,local (default)
model = "claude-sonnet-4-6"
max_turns = 30
timeout_secs = 3600
```

CLI flags override config file values. `--no-inherit-hooks` flag is equivalent to `inherit_hooks = false` in config.

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

| Crate | Purpose |
|-------|---------|
| `clap` (derive) | CLI argument parsing |
| `nix` | `openpty`, `fork`, `login_tty`, `setsid`, `ioctl`, `poll`, `mkfifo`, `signal` |
| `serde` + `serde_json` | JSONL parsing with schema-tolerant deserialization |
| `uuid` | Generate session IDs (for `--session-id` pre-assignment) |
| `tempfile` | Per-run temp directory with guaranteed cleanup |

No async runtime. The PTY event loop uses `nix::poll::poll()` synchronously. `stream-json` output uses a separate thread tailing the transcript file.

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
| `--claude-binary PATH` | Override claude binary path (default: resolves `claude` from PATH) |
| `--no-inherit-hooks` | Disable user hook inheritance; passes `--setting-sources=` to claude |
| `--version` | Print `claude-print <version> (wrapping claude <version>)` and exit |
| `--verbose` | Write timing traces to stderr |

Stdin accepted as prompt when not a TTY and no positional/`--input-file` given.

Exit codes:
- `0` — success
- `1` — assistant error (`is_error: true` in transcript)
- `2` — internal error (PTY spawn, hook setup, parse failure)
- `124` — timeout exceeded
- `130` — interrupted (SIGINT)

### 2. Hook Installer

Creates `$TMPDIR/claude-print-<pid>-<rand>/` via `tempfile::Builder`:

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

**`hook.sh`** (executed by Claude Code on Stop):
```sh
#!/bin/sh
cat > <temp>/stop.fifo
```

Receives the Stop JSON payload on stdin and writes it to the FIFO. Claude Code does not wait for the hook to complete beyond the 10 s timeout.

**`stop.fifo`** — POSIX named pipe created with `nix::unistd::mkfifo()`.

**In `--no-inherit-hooks` mode**, also forward `--setting-sources=` to claude (empty = no standard sources loaded). Only `--settings <temp>/settings.json` is active. This prevents the user's SessionStart/Stop/PreToolUse hooks from firing.

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
        execvp("claude", &args)?;
        unreachable!()
    }
    ForkResult::Parent { child } => {
        drop(slave);
        run_event_loop(master, child, ...)
    }
}
```

`login_tty(slave)` is glibc's `login_tty(3)`: `setsid()` → `TIOCSCTTY` → `dup2(slave, 0/1/2)` → `close(slave)`.

Window size read from `/dev/tty` via `TIOCGWINSZ`; falls back to 220 × 50.

Cleanup on any exit path: `SIGTERM` → 2 s → `SIGKILL` → `waitpid`.

### 4. Event Loop

Single `poll()` call on three fds:

```
master_fd   POLLIN → read PTY output, dispatch to TerminalEmu + StartupSeq
stop_fifo   POLLIN → Stop hook fired; read payload, begin transcript extraction
timer       —      → check wall-clock timeout
```

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
| `ESC [ > q` | `ESC P > \| claude-print ESC \` | XTVERSION (DCS string) |
| `ESC [ 1 8 t` | `ESC [ 8 ; <rows> ; <cols> t` | Window size |

**Version-resilience rule:** Unknown escape sequences (`ESC [ ... <letter>` not in the table above) are silently discarded — never treated as an error. If Ink adds new probe types in future versions, they are ignored and the session proceeds via the startup sequencer timeout.

Each probe type is acknowledged at most once per session (dedup bitmask).

### 6. Startup Sequencer

**Phase 1 — Trust/welcome dismiss:**

The trust dialog asks the user to confirm before allowing tool use. Detection uses keyword scanning, not exact string match, to survive UI text changes across Claude Code versions:

- If any output line contains two or more of: `trust`, `Allow`, `continue`, `folder`, `permission`, `proceed` → send `\r` immediately
- Fallback: after 0.8 s with no new PTY bytes and ≥ 200 bytes received total → send `\r` (covers any welcome/confirmation prompt)
- Hard timeout 45 s with zero bytes → exit 2 (binary not found or hung)

**Phase 2 — Prompt injection:**

- After Phase 1 CR, wait until PTY is idle for 2.0 s (REPL re-renders)
- Send via bracketed paste: `\x1b[200~<prompt>\x1b[201~\r`
- Bracketed paste treats embedded `\n` as literals (no premature Enter)
- Prompts > 32 KB: write to `$TMPDIR/claude-print-.../prompt.txt`; send `/read <path>\r`

### 7. Stop Poller

Reads from `stop.fifo` (non-blocking open; polled via the main `poll()` loop). On data available:

1. Read one line → parse JSON with lenient schema (all fields `Option<T>`)
2. Extract `session_id` and `transcript_path` (either direct or derived from `session_id` + `cwd`)
3. Signal the event loop to exit
4. Send `\x1b[201~\r/exit\r` to PTY child to trigger graceful shutdown

If Stop never fires within `--timeout` seconds: emit timeout result, SIGTERM child, exit 124.

### 8. Transcript Reader

On Stop receipt:

```
1. Open transcript_path (derived if not in payload)
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

**`stream-json`**: Spawns a reader thread that tails the transcript JSONL from `prompt_injected_at` timestamp, forwarding each new raw event line to stdout as it is written by Claude Code. After Stop fires, drains remaining lines. Output is raw JSONL (one JSON object per line), compatible with `claude -p --output-format stream-json`.

`claude_version` field (new, not in `claude -p` wire format): included in all output formats for version-change debugging. Callers that parse strictly by field name are unaffected by the extra field.

Error result:
```json
{"type": "result", "subtype": "timeout|interrupted|internal_error|assistant_error",
 "is_error": true, "error_message": "..."}
```

### 10. NEEDLE Agent Config

`claude-print.yaml` → `~/.needle/agents/`:
```yaml
name: claude-print
description: Claude Code interactive mode — subscription billing (cc_entrypoint=cli)
agent_cli: claude-print
version_command: "claude-print --version"
input_method:
  method: stdin
invoke_template: "cd {workspace} && claude-print --model {model} --max-turns 30 --dangerously-skip-permissions"
timeout_secs: 3600
provider: anthropic
model: claude-sonnet-4-6
output_transform: needle-transform-claude
cost:
  type: use_or_lose
```

### 11. Install Script

`install.sh`:
1. Detect arch (`uname -m`) and select binary from release assets
2. Verify `claude` is on `$PATH`
3. Install binary to `~/.local/bin/claude-print` (mode 755)
4. Install `claude-print.yaml` to `~/.needle/agents/` (mode 644, skipped if NEEDLE not installed)
5. Run `claude-print --version` to confirm
6. Print detected `claude` version for version-compat record

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

## Error Handling

| Condition | Detection | Action | Exit |
|-----------|-----------|--------|------|
| `claude` binary not found | PATH lookup fails at startup | emit error | 2 |
| PTY open fails | `openpty()` returns Err | emit error | 2 |
| Hook installer fails | temp dir / mkfifo / write error | emit error | 2 |
| No PTY output within 45 s | startup timer | kill child, emit error | 2 |
| Child exits before Stop | `waitpid` returns | emit error with child exit code | 2 |
| Wall-clock timeout | poll timer | SIGTERM child, emit timeout | 124 |
| Stop hook never fires | FIFO timeout | SIGTERM child, emit timeout | 124 |
| SIGINT | signal handler | SIGTERM child, emit interrupt result | 130 |
| Transcript empty + fallback empty | retry exhausted | emit error | 1 |
| `is_error: true` in transcript | result event or error block | emit error result | 1 |
| Rate limit / API error | error content in transcript | emit error result | 1 |

## Implementation Phases

- [ ] **Phase 1: Crate scaffold** — `Cargo.toml` with pinned deps, `src/main.rs` with CLI parsing (`clap`), `--version` output including detected `claude --version`
- [ ] **Phase 2: Hook installer + PTY spawner** — temp dir, `settings.json` relay hook, `hook.sh`, `mkfifo`, then `nix` fork/exec, window-size probe, `login_tty`, SIGTERM/SIGKILL cleanup, `waitpid`; `--no-inherit-hooks` path forwards `--setting-sources=` to child
- [ ] **Phase 3: Event loop** — `poll()` on master_fd + FIFO fd + timeout; read buffer; EIO detection
- [ ] **Phase 4: Terminal emulator** — probe scanner, response table, dedup bitmask; unknown-probe passthrough
- [ ] **Phase 5: Startup sequencer** — keyword-based trust dismiss, idle-gap timing, bracketed paste injection, large-prompt file relay
- [ ] **Phase 6: Hook installer** — `tempfile::TempDir`, write `settings.json` and `hook.sh`, `mkfifo`, FIFO polling
- [ ] **Phase 7: Transcript reader** — JSONL parse with lenient serde, usage dedup, text extraction, retry loop, Stop-payload fallback, path derivation
- [ ] **Phase 8: Emitter** — text/json/stream-json formats, `claude_version` field, error result objects, exit code mapping
- [ ] **Phase 9: NEEDLE integration** — `claude-print.yaml`, `install.sh`, `claude-print-ci` WorkflowTemplate in declarative-config
- [ ] **Phase 10: Tests** — unit + mock PTY + version-resilience (see Testing section)
- [ ] **Phase 11: CI** — `claude-print-ci` Argo WorkflowTemplate: fmt + clippy + test + release binary

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
- **Arbitrary unknown welcome text (no keywords) → fallback: CR after 0.8 s idle**
- No output for 45 s → error returned
- 199 bytes received then idle 0.8 s → no CR yet (minimum 200 bytes enforced)
- 200 bytes received then idle 0.8 s → CR sent

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

Integration test scenarios:

| Scenario | Mock config | Assertion |
|----------|------------|-----------|
| Happy path | defaults | exit 0, correct response text, non-zero token counts |
| Trust dialog (standard wording) | `TRUST_DIALOG=1` | exit 0 |
| **Trust dialog (alternate wording)** | `TRUST_DIALOG=1 TRUST_WORDING=alternate` | exit 0 (resilience) |
| No startup output | emit nothing | exit 2 after timeout |
| Child exits before Stop | `EXIT_BEFORE_STOP=1` | exit 2 |
| Stop hook never fires | `DELAY_STOP=99999` | exit 124 |
| Transcript race | `DELAY_JSONL=100` | retry loop fires, exit 0 |
| Missing `transcript_path` | `OMIT_TRANSCRIPT_PATH=1` | path derived, exit 0 |
| Missing `last_assistant_message` | `OMIT_LAST_MESSAGE=1` | retry-only path, exit 0 |
| **Both omitted + delayed JSONL** | `OMIT_LAST_MESSAGE=1 DELAY_JSONL=200` | retries suffice, exit 0 |
| Error in transcript | `IS_ERROR=1` | exit 1, `is_error: true` in output |
| SIGINT | `DELAY_STOP=5000` + send SIGINT at 1 s | exit 130, child killed |
| Multi-turn | `TURNS=3` | last turn text returned, 3 turns in token sum |
| Large prompt (>32KB) | 33000-byte prompt | file relay used, exit 0 |
| **Unknown probe emitted** | `UNKNOWN_PROBE=1` | probe ignored, session completes |
| **Unknown event type in JSONL** | `UNKNOWN_EVENT_TYPE=1` | parse succeeds, text extracted |
| **Unknown usage fields** | `UNKNOWN_USAGE_FIELDS=1` | ignored, token counts correct |
| `--no-inherit-hooks` | `--no-inherit-hooks` flag set | `--setting-sources=` in child argv, exit 0 |
| Output format json | defaults | output parses as valid JSON |
| Output format stream-json | defaults | each output line parses as valid JSON |

### Hook Inheritance Tests (`tests/hooks.rs`)

These tests verify that `--settings` relay hook merges correctly and that `--no-inherit-hooks` suppresses user hooks.

**Settings merge (default mode):**
- Verify `--settings <temp>/settings.json` is always passed to mock_claude
- Verify the relay hook fires (Stop payload arrives on FIFO)
- With `mock_claude` simulating additional hooks in user settings: both user hook + relay hook fire
- `--settings` flag is present in the child process argv (visible via `/proc/<pid>/cmdline`)

**`--no-inherit-hooks` flag:**
- `--setting-sources=` is present in child argv when flag is set
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

A dedicated test module that verifies the binary survives schema changes across Claude Code versions. These tests are run in CI on every push and also on a weekly schedule.

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

## Open Questions

- **`--settings` merge behavior**: Needs end-to-end verification that `--settings` truly merges with `~/.claude/settings.json` hooks rather than replacing them. If Claude Code's merge fails, the fallback is to read `~/.claude/settings.json` in-process, merge hooks manually, write a combined settings file to the temp dir, and pass only the combined file via `--settings`.
- **`--setting-sources=` empty value**: Verify Claude Code accepts an empty string for `--setting-sources` (to suppress all standard sources). If it doesn't, alternatives are `--setting-sources=none` (if supported) or `--no-session-persistence` + other flags to achieve the same effect.
- **Multiline prompt > 32 KB**: Does the `/read <path>` slash command accept absolute paths? Does it interact with `--allowedTools`? Needs end-to-end verification.
- **`FIFO` open race**: The parent must open the FIFO read end before the Stop hook fires. Mitigation: open read end before prompt injection. Verify timing in the mock integration test.
- **musl vs glibc**: `openpty` and `login_tty` are glibc extensions; musl may not provide `login_tty`. May need to inline it as `setsid()` + `ioctl(TIOCSCTTY)` + `dup2(slave, 0/1/2)`.
- **`CLAUDE_CODE_SESSION_ID` / `CLAUDE_CODE_SESSION_KIND` inheritance**: The inner claude process inherits the parent's full env. If the parent is itself inside a Claude Code session, these vars may be set and could confuse the child's session identity. Evaluate whether they need to be unset in the child env before `execvp`.
