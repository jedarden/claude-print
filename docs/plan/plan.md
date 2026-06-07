# claude-print Plan

## Overview

Drop-in replacement for `claude -p` that drives the Claude Code interactive TUI via PTY, emitting wire-compatible output while billing against the subscription (`cc_entrypoint=cli`) rather than the Agent SDK credit pool.

## Background

Anthropic's June 15, 2026 billing split:
- `claude -p` / Agent SDK → separate monthly credit pool ($100/mo Max 5x, $200/mo Max 20x)
- Interactive TUI (`claude`) → continues consuming unlimited subscription

The key mechanism: the interactive TUI sends `cc_entrypoint=cli` in the billing header. Any wrapper that gives `claude` a real PTY inherits that classification. Screen-scraping or hook-based completion detection extracts the response and emits it in `claude -p` wire format.

## Prior Art

### `jedarden/NEEDLE` — `plugins/claude-interactive`
Python script (~300 lines). PTY via `pty.openpty()` + `os.fork()`. Completion detection via 30s idle timeout after seeing `●` bullet. Response extraction via `pyte` virtual terminal screen parse. Token counts always zero. Single commit (2026-05-16). **Source for initial implementation.**

### `smithersai/claude-p`
Zig binary, zmux PTY library. Uses `SessionStart` + `Stop` hooks injected via `--settings` for authoritative completion detection. Reads token counts from JSONL transcript. Wire-compatible with `claude -p`. ~50–200ms overhead. **Source for Stop hook pattern.**

### `hristo2612/jinn`
Node.js, node-pty. Hook relay via HTTP loopback with shared secret. Intercepts SSE stream via `ANTHROPIC_BASE_URL` proxy. More moving parts; better for persistent session keep-alive use cases.

## Architecture

```
caller
  │  prompt (stdin or arg)
  ▼
claude-print
  ├── PTY spawner   forkpty() → claude --dangerously-skip-permissions
  ├── Terminal emu  responds to DA1/DA2/DSR/XTVERSION probes (Ink requirement)
  ├── Startup seq   wait for burst → CR (trust dismiss) → bracketed-paste prompt
  ├── Stop hook     per-run ~/.config/claude-print/<pid>/settings.json overlay
  │                 hook writes {session_id, transcript_path} to named pipe
  ├── Transcript    reads ~/.claude/projects/**/<session>.jsonl for token counts
  └── Emitter       emits stream-json / json / text to stdout (claude -p compat)
```

## Components

### 1. PTY Spawner
- `pty.openpty()` + `os.fork()` + `os.execvp('claude', [...])`
- Forwards `--model`, `--max-turns`, `--allowedTools`, `--dangerously-skip-permissions`
- Sets PTY window size from `/dev/tty` or defaults (220×50)

### 2. Terminal Emulator (Ink probe responder)
- DA1 (`ESC[c`) → `ESC[?6c`
- DA2 (`ESC[>0c`) → `ESC[>0;0;0c`
- DSR (`ESC[6n`) → `ESC[1;1R`
- XTVERSION (`ESC[>q`) → `ESC P>|claude-print ESC \`
- Window size (`ESC[18t`) → `ESC[8;50;220t`
- Without these, Ink hangs indefinitely at startup

### 3. Startup Sequencer
- Phase 1: accumulate startup bytes; after 0.8s idle gap (or 45s timeout), send CR to dismiss any trust dialog
- Phase 2: after 2.0s gap post-CR, send prompt via bracketed paste (`ESC[200~...ESC[201~` + CR)
- Detects `trust` + `folder` in PTY output and sends CR immediately

### 4. Stop Hook (completion signal)
- Before spawning: write per-run settings overlay to `~/.config/claude-print/<pid>/settings.json`
  ```json
  {"hooks": {"Stop": [{"hooks": [{"type": "command", "command": "/path/to/hook.sh"}]}]}}
  ```
- Invoke claude with `--settings ~/.config/claude-print/<pid>/settings.json`
- Hook script reads stdin JSON, writes `{session_id, transcript_path, timestamp}` to named FIFO
- Parent polls FIFO; on receipt, breaks from PTY read loop
- Cleanup: remove settings overlay + FIFO on exit (defer)
- **This replaces the fragile idle-timeout approach in the NEEDLE plugin**

### 5. Transcript Reader
- On Stop hook receipt, extract `transcript_path` from hook payload
- Read JSONL, find last `type: "assistant"` event, extract `content[].text`
- Aggregate `usage` blocks across all assistant events for real token counts
- Retry loop (40 × 50ms) for Stop→JSONL flush race
- Fallback: extract `last_assistant_message` from Stop payload directly

### 6. Emitter
- `--output-format text` (default): print response text to stdout
- `--output-format json`: emit single JSON object with `result`, `session_id`, `usage`, `duration_ms`, `is_error`
- `--output-format stream-json`: tail transcript JSONL and emit lines as they appear (per-message streaming)
- All formats match `claude -p` wire format

## Data Models

### Stop Hook Payload (from Claude Code)
```json
{
  "session_id": "abc123",
  "transcript_path": "/home/coding/.claude/projects/.../abc123.jsonl",
  "last_assistant_message": "...",
  "hook_event_name": "Stop"
}
```

### Emitted JSON (--output-format json)
```json
{
  "type": "result",
  "subtype": "success",
  "is_error": false,
  "result": "assistant response text",
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

### NEEDLE Agent Config (`claude-print.yaml`)
```yaml
name: claude-print
description: Claude Code interactive mode — subscription billing (cc_entrypoint=cli)
agent_cli: claude-print
input_method:
  method: stdin
invoke_template: "cd {workspace} && claude-print --model {model} --max-turns 30 --dangerously-skip-permissions"
timeout_secs: 3600
provider: anthropic
model: claude-sonnet-4-6
output_transform: needle-transform-claude
```

## Implementation Phases

- [ ] Phase 1: Core PTY wrapper — spawner, terminal probe responder, startup sequencer, idle-timeout completion (port from NEEDLE plugin, working baseline)
- [ ] Phase 2: Stop hook completion — per-run settings overlay, named FIFO, hook script, poll loop (replaces idle timeout)
- [ ] Phase 3: Transcript reader — JSONL parse, token extraction, retry loop, fallback to Stop payload
- [ ] Phase 4: Emitter — text/json/stream-json output formats, wire-compat with `claude -p`
- [ ] Phase 5: NEEDLE integration — `claude-print.yaml` agent config, `install.sh`, test with NEEDLE worker
- [ ] Phase 6: Tests and CI — unit tests for transcript parsing, mock PTY scenarios, Argo Workflows CI

## Open Questions

- **Language**: Python (port from NEEDLE plugin, fast iteration) or Rust (native NEEDLE integration)? Python has `pyte` and `pty` ready; Rust would need a PTY crate.
- **`--settings` overlay vs project-level hooks**: Per-run `--settings` file (smithersai approach) avoids mutating `~/.claude/settings.json` and is self-contained. Project-level `.claude/settings.json` per workspace is an alternative but affects all sessions in that workspace.
- **pyte for response extraction**: Still needed for fallback when Stop payload `last_assistant_message` is absent (older Claude Code versions). Keep or drop?
- **Multiline prompts**: NEEDLE sends prompt via stdin pipe. Bracketed paste handles embedded newlines correctly; verify with very long prompts (>32KB).
- **Rate limit 429 handling**: Claude emits an error event in the transcript; `is_error: true` and exit 1. No explicit retry — callers handle retry.
- **Dedicated GitHub repo vs NEEDLE plugin**: Standalone repo (`jedarden/claude-print`) allows independent versioning, pre-built releases, and use outside NEEDLE. NEEDLE plugin stays as thin wrapper pointing at the binary.
