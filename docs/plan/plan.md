# claude-print Plan

## Overview

Drop-in replacement for `claude -p` that drives the Claude Code interactive TUI via PTY, emitting wire-compatible output while billing against the subscription (`cc_entrypoint=cli`) rather than the Agent SDK credit pool.

## Background

Starting June 15, 2026, Anthropic separates `claude -p` (headless) into a separate monthly credit pool. Only the interactive TUI continues drawing from the unlimited subscription. The billing classification is determined by the `cc_entrypoint` field in the request header:

- `cc_entrypoint=cli` → interactive TUI → subscription
- `cc_entrypoint=sdk-cli` → `claude -p` / Agent SDK → credit pool

Running `claude` under a real PTY causes it to enter interactive mode, which sets `cc_entrypoint=cli`. `claude-print` wraps this in a PTY, extracts the response via the Stop hook and JSONL transcript, and emits output in `claude -p` wire format. Callers see no difference; billing goes to the subscription.

## Architecture

```
caller
  │  prompt (stdin, arg, or --input-file)
  ▼
claude-print
  ├── CLI parser       flags forwarded to claude subprocess
  ├── Hook installer   writes per-run settings overlay + hook script + FIFO
  ├── PTY spawner      forkpty() → claude [forwarded flags]
  ├── Terminal emu     responds to DA1/DA2/DSR/XTVERSION/window-size probes
  ├── Startup seq      phase 1: trust dismiss  phase 2: bracketed-paste prompt
  ├── Stop poller      blocks on FIFO until Stop hook fires
  ├── Transcript rdr   JSONL parse → final text + token counts (retry loop)
  ├── Emitter          text / json / stream-json to stdout
  └── Cleanup          FIFO, settings overlay, PTY master fd, child wait
```

## Components

### 1. CLI Interface

Accepts a strict subset of `claude -p` flags so it is a drop-in replacement:

| Flag | Description |
|------|-------------|
| `prompt` (positional) | Prompt string; mutually exclusive with `--input-file` and stdin |
| `--input-file FILE` | Read prompt from file (required for multiline >32KB) |
| `--model MODEL` | Model identifier forwarded to claude (default: `claude-sonnet-4-6`) |
| `--max-turns N` | Max assistant turns forwarded to claude (default: 30) |
| `--output-format FORMAT` | `text` (default), `json`, `stream-json` |
| `--allowedTools LIST` | Comma-separated tools forwarded to claude |
| `--disallowedTools LIST` | Forwarded to claude |
| `--dangerously-skip-permissions` | Forwarded to claude (required for autonomous use) |
| `--timeout SECS` | Hard wall-clock timeout in seconds (default: 3600) |
| `--verbose` | Write timing traces to stderr |

Stdin is accepted as prompt when it is not a TTY and no positional or `--input-file` is given.

Exit codes:
- `0` — success, assistant responded normally
- `1` — assistant error (`is_error: true` in transcript)
- `2` — internal error (PTY spawn failure, hook setup failure, parse error)
- `124` — timeout exceeded
- `130` — interrupted (SIGINT)

### 2. Hook Installer

Creates a per-run temp directory (`$TMPDIR/claude-print-<pid>-<rand>/`) containing:

**`settings.json`** — passed to claude via `--settings`:
```json
{
  "hooks": {
    "Stop": [{
      "hooks": [{"type": "command", "command": "/tmp/claude-print-.../hook.sh", "timeout": 10}]
    }]
  }
}
```

**`hook.sh`** — shell script executed by Claude Code on Stop:
```bash
#!/bin/sh
# Reads Stop hook JSON from stdin, writes payload line to FIFO
cat >> /tmp/claude-print-.../stop.fifo
```

**`stop.fifo`** — named pipe created with `mkfifo`; parent blocks reading it.

Cleanup on any exit path: remove `settings.json`, `hook.sh`, `stop.fifo`, temp dir.

The user's `~/.claude/settings.json` is never modified.

### 3. PTY Spawner

```
master_fd, slave_fd = pty.openpty()
pid = os.fork()
# child: setsid(), TIOCSCTTY, dup2(slave, 0/1/2), execvp('claude', args)
# parent: close(slave_fd), enter event loop on master_fd
```

PTY window size set from `/dev/tty` via `TIOCGWINSZ`; falls back to 220 columns × 50 rows.

Forwards to claude subprocess:
- `--settings <temp-dir>/settings.json`
- `--model`, `--max-turns`, `--allowedTools`, `--disallowedTools`
- `--dangerously-skip-permissions` (if passed)

Cleanup: SIGTERM → 2s wait → SIGKILL; `os.waitpid`; close `master_fd`.

### 4. Terminal Emulator (Ink probe responder)

Ink queries the terminal at startup and hangs indefinitely if unanswered. Responses sent back through `master_fd`:

| Probe | Response |
|-------|----------|
| DA1 `ESC[c` or `ESC[0c` | `ESC[?6c` |
| DA2 `ESC[>c` or `ESC[>0c` | `ESC[>0;0;0c` |
| DSR `ESC[6n` | `ESC[1;1R` |
| XTVERSION `ESC[>q` | `ESC P>|claude-print ESC \` |
| Window size `ESC[18t` | `ESC[8;50;220t` |

Probes are matched against raw PTY output bytes. Each probe type is acknowledged once per session (dedup set). Responses queued to a mutex-guarded buffer and flushed on the next write pass to avoid re-entrance on the read thread.

### 5. Startup Sequencer

**Phase 1 — Trust dismiss:**
- Accumulate PTY output bytes
- If `trust` and `folder` appear together in any output line: send `\r` immediately
- Otherwise: after 0.8 s idle gap (no PTY bytes) and at least 200 bytes received, send `\r`
- Hard timeout 45 s: if no output at all, emit error and exit 2

**Phase 2 — Prompt injection:**
- After Phase 1 CR, wait for 2.0 s idle gap (REPL re-renders)
- Send prompt via bracketed paste: `ESC[200~<prompt>ESC[201~\r`
- Bracketed paste ensures embedded newlines are treated as literals, not as Enter presses
- For prompts > 32 KB or containing binary sequences, write to a temp file and send `/read <path>\r` then `/file <path>\r` as fallback

### 6. Stop Poller

After prompt injection, the parent blocks reading `stop.fifo` (blocking `open()` on reader side, with 0.25 s `select` poll to check wall-clock timeout). When the Stop hook fires:
- Claude Code runs `hook.sh` which cats stdin (the Stop JSON payload) into the FIFO
- Parent reads the line, parses JSON, extracts `session_id` and `transcript_path`
- Breaks from the PTY read loop
- Sends `/exit\r` to the PTY child to trigger graceful shutdown

Timeout: if Stop never fires within `--timeout` seconds, emit error (exit 124) and terminate child.

### 7. Transcript Reader

On Stop hook receipt:

1. Extract `transcript_path` from payload
2. Open JSONL file, iterate lines:
   - Collect all `{"type":"assistant"}` events
   - For the last assistant event: concatenate `content[].text` blocks (skip `tool_use`, `thinking` blocks)
   - Accumulate `usage` fields across all assistant events: `input_tokens`, `output_tokens`, `cache_creation_input_tokens`, `cache_read_input_tokens`
3. **Race condition handling**: Stop fires before Claude flushes the final JSONL line. Retry loop: 40 attempts × 50 ms sleep = 2 s max. Each attempt re-reads the file from the last known offset.
4. **Fallback**: if after 40 retries `final_text` is still empty, extract `last_assistant_message` from the Stop payload directly (present in Claude Code ≥ 2.1). Emit with zero token counts.
5. If both transcript and fallback are empty: emit `is_error: true`, exit 1.

### 8. Emitter

**`--output-format text`** (default):
```
<response text>\n
```

**`--output-format json`**:
```json
{
  "type": "result",
  "subtype": "success",
  "is_error": false,
  "result": "<response text>",
  "session_id": "<uuid>",
  "num_turns": 1,
  "duration_ms": 4200,
  "cost_usd": 0,
  "usage": {
    "input_tokens": 1240,
    "output_tokens": 380,
    "cache_creation_input_tokens": 0,
    "cache_read_input_tokens": 900
  }
}
```

**`--output-format stream-json`**:
Tails the JSONL transcript via `pread` (non-destructive) from prompt-sent time, emitting each new line as it appears. Matches the granularity of `claude -p --output-format stream-json`. Stops after Stop hook fires and transcript is drained.

On error, all formats emit an error result object and exit non-zero:
```json
{"type": "result", "subtype": "<code>", "is_error": true, "error_message": "..."}
```

### 9. NEEDLE Agent Config

`claude-print.yaml` installed to `~/.needle/agents/`:
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

### 10. Install Script

`install.sh` actions:
1. Verify `claude` is on `$PATH` (`claude --version`)
2. Verify Python ≥ 3.9 and install `pyte` if absent (`pip3 install --quiet pyte`)
3. Install `claude-print` to `~/.local/bin/` (mode 755)
4. Install `claude-print.yaml` to `~/.needle/agents/` (mode 644)
5. Print summary and verify: `claude-print --version`

## Data Models

### Stop Hook Payload (received from Claude Code)
```json
{
  "hook_event_name": "Stop",
  "session_id": "abc123",
  "transcript_path": "/home/coding/.claude/projects/.../abc123.jsonl",
  "last_assistant_message": "..."
}
```

### JSONL Transcript — Assistant Event
```json
{
  "type": "assistant",
  "message": {
    "content": [
      {"type": "text", "text": "response text"},
      {"type": "tool_use", "name": "Bash", "input": {...}}
    ],
    "usage": {
      "input_tokens": 1240,
      "output_tokens": 380,
      "cache_creation_input_tokens": 0,
      "cache_read_input_tokens": 900
    }
  }
}
```

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
| `claude` not on PATH | `execvp` fails | emit error | 2 |
| Hook installer fails (permissions, disk) | `mkfifo`/`open` error | emit error | 2 |
| No PTY output within 45 s | startup timer | emit error, kill child | 2 |
| SIGINT received | signal handler | emit interrupt result, kill child | 130 |
| Wall-clock timeout | poll timer | emit timeout result, kill child | 124 |
| Stop hook never fires | timeout on FIFO read | emit timeout result, kill child | 124 |
| Transcript empty after retries + fallback empty | retry exhausted | emit error result | 1 |
| Claude returns error event | `is_error` in transcript | emit error result | 1 |
| Rate limit (429) | error event in transcript | emit error result, exit 1; caller retries | 1 |
| Child exits before Stop | `os.waitpid` returns | emit error result | 2 |

## Implementation Phases

- [ ] **Phase 1: PTY core** — spawner, terminal probe responder, startup sequencer (two-phase CR + bracketed paste), wall-clock timeout, SIGINT handler, cleanup
- [ ] **Phase 2: Stop hook** — per-run temp dir, `settings.json` overlay, `hook.sh`, named FIFO, Stop poller replacing all idle-timeout logic
- [ ] **Phase 3: Transcript reader** — JSONL parse, assistant text extraction, token aggregation, retry loop, Stop-payload fallback
- [ ] **Phase 4: Emitter** — text/json/stream-json output formats, error result objects, exit code mapping
- [ ] **Phase 5: CLI** — full argument parser, stdin/file/positional prompt modes, `--version`, `--verbose` trace output
- [ ] **Phase 6: NEEDLE integration** — `claude-print.yaml`, `install.sh`, manual end-to-end test with `needle run --agent claude-print`
- [ ] **Phase 7: Tests** — see Testing section
- [ ] **Phase 8: CI** — Argo Workflows pipeline, lint, unit tests, package release

## Testing

### Unit Tests (`tests/`)

**Terminal probe responder** (`test_terminal.py`):
- DA1 input → correct response bytes
- DA2 input → correct response bytes
- DSR input → correct response bytes
- XTVERSION input → correct DCS string
- Window size query → correct escape sequence
- Multiple probes in one chunk — all answered
- Probe dedup — each type answered only once

**JSONL transcript parser** (`test_transcript.py`):
- Single assistant turn, single text block → correct extraction
- Multi-block content: text + tool_use + text → text blocks concatenated, tool_use skipped
- Multi-turn: multiple assistant events → last turn's text returned
- Token aggregation across multiple assistant events → summed correctly
- Empty `content` array → `final_text` is empty string, no crash
- Malformed JSONL lines → skipped gracefully, valid lines parsed
- `type: "result"` with `is_error: true` → `is_error` flag set
- Cache token fields absent → default to 0

**Stop hook parser** (`test_hook.py`):
- Valid payload → `session_id`, `transcript_path`, `last_assistant_message` extracted
- Missing `transcript_path` → fallback to `last_assistant_message`
- Missing both → error
- Malformed JSON payload → error

**Emitter** (`test_emitter.py`):
- `text` format: plain text, trailing newline
- `json` format: all fields present, valid JSON, correct types
- `stream-json` format: one JSON object per line, lines parseable individually
- Error result: `is_error: true`, non-zero `subtype`
- Token counts zero when fallback path taken

**CLI argument parser** (`test_cli.py`):
- Positional prompt accepted
- `--input-file` accepted, stdin rejected when file given
- Stdin accepted when no TTY and no positional/file
- Mutually exclusive prompt sources → error
- Unknown flags passed through to claude arg list
- `--timeout` validated as positive integer
- `--output-format` validated against allowed values

### Mock PTY Integration Tests (`tests/mock_claude/`)

A mock `claude` shell script simulates the real binary's startup/response sequence without requiring an Anthropic API call. Tests run in CI without credentials.

**`mock_claude.sh`** phases:
1. Emit DA1 and XTVERSION probes (tests that wrapper responds)
2. Emit trust dialog text (tests that wrapper sends CR)
3. Emit REPL prompt `❯ ` (tests that wrapper sends the real prompt)
4. Emit `●` bullet + response text (tests response detection)
5. Fire the Stop hook via the settings overlay
6. Exit 0

Mock scenarios:

| Scenario | Mock behavior | Expected outcome |
|----------|---------------|-----------------|
| Happy path | Normal startup + response | exit 0, correct text emitted |
| Trust dialog present | Emit trust text before REPL | CR sent, session continues |
| No startup output | Emit nothing | exit 2 after 45 s |
| Stop hook never fires | Skip hook execution | exit 124 after timeout |
| Transcript race | Hook fires before JSONL written | retry loop recovers |
| Empty response | Hook fires, transcript has no text | Stop payload fallback used |
| Error event in transcript | `is_error: true` in result event | exit 1 |
| SIGINT during session | Send SIGINT to claude-print | exit 130, child killed |
| Child exits early | Mock exits before hook | exit 2 |
| Very long prompt (>32KB) | Accept bracketed paste | session completes normally |
| Multi-turn (`--max-turns 2`) | Emit two assistant turns | last turn text returned |

### End-to-End Tests (slow, credential-required, excluded from CI)

Manual verification checklist:
- `echo "Say hi" | claude-print` → non-empty response, exit 0
- `claude-print --output-format json "What is 2+2"` → valid JSON with `result` and `usage`
- `claude-print --output-format stream-json "List 5 animals"` → multiple JSONL lines during response
- `claude-print --max-turns 2 "Start counting from 1"` → response terminates after turn 2
- `claude-print --allowedTools Bash "Run: echo hello"` → tool use executes, output in response
- Verify `cc_entrypoint=cli` in `~/.claude/projects/**/*.jsonl` billing header
- Run as NEEDLE worker: `needle run --agent claude-print --workspace .` on a workspace with 1 open bead

## Open Questions

- **Language**: Python (fast iteration, `pty` and `pyte` in stdlib/pip) or Rust (native binary, no runtime dependency)? Python chosen for Phase 1; can port to Rust later without changing the interface.
- **`--settings` overlay scope**: Per-run temp file avoids any cross-session interference. Project-level `.claude/settings.json` is an alternative but would affect all sessions in that directory.
- **`pyte` for fallback extraction**: Needed only if both transcript and `last_assistant_message` in Stop payload are empty (older Claude Code versions lacking the field). Keep as last resort fallback.
- **Multiline prompt >32KB**: Bracketed paste buffer limit varies by terminal. If exceeded, `/read <file>` or temp file approach needed. Measure in practice.
- **Opus support**: `claude-print-opus.yaml` agent config for use cases requiring Opus. Identical wrapper, different `--model` default.
