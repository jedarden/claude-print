# Claude Code Internals

## Session System

### Session Identity

Every Claude Code process creates a live session record at:
```
~/.claude/sessions/<pid>.json
```

Contents (observed from v2.1.168):
```json
{
  "pid": 360946,
  "sessionId": "37f84004-275c-46fd-8947-54348867302a",
  "cwd": "/home/coding",
  "startedAt": 1780834744503,
  "procStart": "117958",
  "version": "2.1.168",
  "peerProtocol": 1,
  "kind": "interactive",
  "entrypoint": "cli",
  "status": "idle",
  "updatedAt": 1780836672456
}
```

`kind` is `interactive` for TUI sessions, `print` for `-p` runs. `entrypoint` is `cli` for both (the billing field is set separately in request headers). `status` transitions: `working` while a turn is in progress, `idle` when waiting for input.

The session file is deleted when the process exits. Enumerating `~/.claude/sessions/` gives all currently-running Claude Code processes.

### Transcript Storage

Each session writes its full conversation to:
```
~/.claude/projects/<cwd-slug>/<session-id>.jsonl
```

The `<cwd-slug>` is the working directory path with `/` replaced by `-` (e.g., `/home/coding/claude-print` → `-home-coding-claude-print`).

The JSONL file is **append-only** — every event is a single JSON line. The file is flushed incrementally during a session; at Stop-hook fire time there is a race window (2–5 ms) where the final assistant event may not yet be written.

To derive `transcript_path` from a session record:
```python
import os, re

def transcript_path(session_id, cwd):
    slug = cwd.replace('/', '-')
    return os.path.expanduser(f'~/.claude/projects/{slug}/{session_id}.jsonl')
```

### Session Flags

Relevant CLI flags for session management:

| Flag | Effect |
|------|--------|
| `--session-id <uuid>` | Assigns a specific UUID as this session's ID; the session writes to the corresponding JSONL path |
| `-r/--resume <id>` | Resumes a prior session: reads existing JSONL as conversation history, continues appending |
| `-c/--continue` | Resume the most recent session in the current working directory |
| `--fork-session` | Used with `--resume`/`--continue`; generates a new session ID instead of reusing the original — branches the conversation |
| `-n/--name <name>` | Display name shown in TUI and `/resume` picker |
| `--no-session-persistence` | Disables JSONL writing entirely; nothing is persisted |

**Resuming a subprocess session into the parent:**
```bash
# Start a subprocess session with a known ID
claude --session-id a1b2c3d4-... --dangerously-skip-permissions --print "do work"

# Later, resume that session in interactive mode — full history is available
claude --resume a1b2c3d4-...
```

This is the cleanest way to "add an independent session to the main session": resume the subprocess session in the parent terminal. The history is already in the JSONL; `--resume` replays it as context.

### `--fork-session` Branching

`--resume <id> --fork-session` creates a new session ID and writes to a new JSONL file, but reads the prior session's JSONL as its initial conversation history. The prior session is unchanged. This is useful for branching from a completed subprocess session without modifying it.

## JSONL Transcript Format

### Event Types

| Type | When written | Notes |
|------|-------------|-------|
| `file-history-snapshot` | Session start | File tracking for undo |
| `user` | Each user turn | Includes `entrypoint`, `cwd`, `sessionId`, `version`, `gitBranch` |
| `assistant` | Each API response chunk | One event per streaming chunk; all chunks for one turn share identical `message.usage` |
| `system` | Tool results, local commands | `subtype: "local_command"` for slash commands; `subtype: "tool_result"` etc. |
| `last-prompt` | After each turn | Records `lastPrompt`, `leafUuid`, `sessionId` |
| `attachment` | File/image attachments | |
| `result` | `--print` mode only | Final result object (see below) |
| `summary` | After compaction | Compressed context summary |

### `user` Event Fields

```json
{
  "parentUuid": "...",          // UUID of the event this follows in the message tree
  "isSidechain": false,
  "promptId": "...",            // groups all events for a single user turn
  "type": "user",
  "message": {
    "role": "user",
    "content": "<prompt text>"  // may be string or array of content blocks
  },
  "uuid": "...",
  "timestamp": "2026-06-07T...",
  "userType": "external",
  "entrypoint": "cli",          // "cli" for TUI, "sdk-cli" for -p after June 15
  "cwd": "/home/coding/...",
  "sessionId": "...",
  "version": "2.1.168",
  "gitBranch": "main"
}
```

Tool results appear as `user` events with `message.content` being an array of `{"type": "tool_result", "tool_use_id": "...", "content": [...]}` blocks.

### `assistant` Event — Streaming Chunks

A single API call (one LLM turn) produces **multiple consecutive `assistant` events** — one per streaming chunk. All chunks for the same API call carry **identical `message.usage`** objects.

This means: to identify unique API turns, detect when `message.usage` changes between consecutive `assistant` events.

```json
{
  "parentUuid": "...",
  "isSidechain": false,
  "type": "assistant",
  "message": {
    "role": "assistant",
    "content": [
      {"type": "thinking", "thinking": "..."},
      {"type": "text", "text": "..."},
      {"type": "tool_use", "id": "toolu_...", "name": "Bash", "input": {"command": "..."}}
    ],
    "model": "claude-sonnet-4-6",
    "usage": {
      "input_tokens": 6178,
      "output_tokens": 295,
      "cache_creation_input_tokens": 825,
      "cache_read_input_tokens": 26442,
      "server_tool_use": {"web_search_requests": 0, "web_fetch_requests": 0},
      "service_tier": "standard",
      "cache_creation": {
        "ephemeral_5m_input_tokens": 0,
        "ephemeral_1h_input_tokens": 825
      },
      "inference_geo": "",
      "iterations": [
        {
          "input_tokens": 6178,
          "output_tokens": 295,
          "cache_read_input_tokens": 26442,
          "cache_creation_input_tokens": 825,
          "cache_creation": {"ephemeral_5m_input_tokens": 0, "ephemeral_1h_input_tokens": 825},
          "type": "message"
        }
      ],
      "speed": "standard"
    }
  },
  "uuid": "...",
  "timestamp": "..."
}
```

Content block types within a single turn:
- `"type": "thinking"` — extended thinking scratchpad (not part of final response)
- `"type": "text"` — assistant prose (the human-visible response)
- `"type": "tool_use"` — a tool call (name + input object)

One turn often splits across several chunks: e.g., chunk 1 has a `thinking` block, chunk 2 has the first `text` block, chunk 3 has a `tool_use` block.

### `last-prompt` Event

```json
{
  "type": "last-prompt",
  "lastPrompt": "full text of the last user prompt...",
  "leafUuid": "...",
  "sessionId": "..."
}
```

Written after every assistant turn (points to the most recent user message). Useful for finding session boundaries without scanning all events.

### `result` Event (`--print` mode)

In `--print` mode, the final event in the JSONL is a `result` object:
```json
{
  "type": "result",
  "subtype": "success",
  "is_error": false,
  "result": "final response text",
  "session_id": "...",
  "num_turns": 3,
  "duration_ms": 12400,
  "cost_usd": 0,
  "usage": {
    "input_tokens": 1240,
    "output_tokens": 380,
    "cache_creation_input_tokens": 0,
    "cache_read_input_tokens": 900
  }
}
```

**This event is absent from interactive-mode transcripts.** In interactive mode, token totals must be computed by aggregating across unique API turns in the `assistant` events.

## Token Counting

### Problem: Streaming Duplicates

Every streaming chunk event for the same API call carries the same `usage` object. Naively summing `output_tokens` across all `assistant` events over-counts by the number of chunks per turn.

### Correct Approach: Dedup by Usage Fingerprint

Two consecutive `assistant` events belong to the same API call if and only if their `message.usage` objects are identical (same `input_tokens`, `output_tokens`, `cache_creation_input_tokens`, `cache_read_input_tokens`). Detect turn boundaries when any of these values changes:

```python
def extract_turns(jsonl_path):
    turns = []
    prev_usage_key = None

    with open(jsonl_path) as f:
        for line in f:
            obj = json.loads(line)
            if obj.get('type') != 'assistant':
                continue
            usage = obj.get('message', {}).get('usage', {})
            key = (
                usage.get('input_tokens'),
                usage.get('output_tokens'),
                usage.get('cache_creation_input_tokens'),
                usage.get('cache_read_input_tokens'),
            )
            if key != prev_usage_key:
                turns.append(usage)
                prev_usage_key = key

    return turns

def sum_tokens(turns):
    return {
        'input_tokens':                 sum(t.get('input_tokens', 0)                 for t in turns),
        'output_tokens':                sum(t.get('output_tokens', 0)                for t in turns),
        'cache_creation_input_tokens':  sum(t.get('cache_creation_input_tokens', 0) for t in turns),
        'cache_read_input_tokens':      sum(t.get('cache_read_input_tokens', 0)      for t in turns),
    }
```

Observed behavior on a 176-line transcript: 45 unique API turns (not 65 assistant events).

### `iterations` Array

Each `usage` object also has an `iterations` array with one entry per API sub-call within the turn (used for extended thinking or multi-step internal reasoning). For standard turns, `len(iterations) == 1`. Sum `iterations[i].output_tokens` if you need granular per-sub-call data.

### Extracting the Final Response Text

The final assistant message's text is the concatenation of all `"type": "text"` blocks from the last unique API turn:

```python
def extract_final_text(jsonl_path):
    last_text_blocks = []
    prev_usage_key = None

    with open(jsonl_path) as f:
        lines = f.readlines()

    for line in lines:
        obj = json.loads(line)
        if obj.get('type') != 'assistant':
            continue
        msg = obj.get('message', {})
        usage = msg.get('usage', {})
        key = (usage.get('input_tokens'), usage.get('output_tokens'),
               usage.get('cache_creation_input_tokens'), usage.get('cache_read_input_tokens'))
        if key != prev_usage_key:
            last_text_blocks = []
            prev_usage_key = key
        for block in msg.get('content', []):
            if block.get('type') == 'text':
                last_text_blocks.append(block['text'])

    return ''.join(last_text_blocks)
```

Skip `thinking` and `tool_use` blocks — they are not part of the human-visible response.

### Race Condition: Stop Hook Fires Before JSONL Flush

The Stop hook fires approximately 2–5 ms before Claude Code flushes the final `assistant` event to the JSONL. If the transcript is read immediately on Stop:
- The final API turn may be missing from the JSONL
- Or the last chunk may be partially written (truncated JSON line)

**Retry strategy:**
```python
import time

def read_with_retry(jsonl_path, max_retries=40, interval=0.05):
    for attempt in range(max_retries):
        text = extract_final_text(jsonl_path)
        if text:
            return text
        time.sleep(interval)
    return None  # use Stop hook payload fallback
```

40 × 50 ms = 2 s maximum wait. Observed: text available within 1–3 retries (50–150 ms after Stop fires).

## Hook System

### Available Hook Events

From `~/.claude/settings.json` (observed on v2.1.168):

| Hook event | When it fires | Stdin payload |
|------------|---------------|---------------|
| `SessionStart` | Claude Code process starts | `{session_id, cwd, ...}` |
| `SessionEnd` | Process exits | `{session_id, ...}` |
| `Stop` | Assistant finishes a turn, waiting for next input | `{session_id, transcript_path, last_assistant_message, ...}` |
| `UserPromptSubmit` | User submits a new message | `{session_id, prompt, ...}` |
| `PreToolUse` | Before each tool call | `{session_id, tool_name, tool_input, ...}` |
| `PermissionRequest` | Before granting a permission | `{session_id, permission, ...}` |

### Stop Hook Payload

```json
{
  "hook_event_name": "Stop",
  "session_id": "37f84004-275c-46fd-8947-54348867302a",
  "transcript_path": "/home/coding/.claude/projects/-home-coding-claude-print/37f84004-....jsonl",
  "last_assistant_message": "The final text of the last assistant turn",
  "cwd": "/home/coding/claude-print"
}
```

`last_assistant_message` is the extracted text of the final turn — available directly without reading the JSONL. Useful as a fallback when the JSONL isn't flushed yet and the retry loop is exhausted.

### Hook Configuration

Hooks are configured in `~/.claude/settings.json` (user-global), `.claude/settings.json` (project), or `.claude/settings.local.json` (local override). The `--settings <path>` flag specifies an additional settings file. Settings are merged; all matching hooks fire.

Per-run settings overlay (the `claude-print` approach):
```json
{
  "hooks": {
    "Stop": [{
      "hooks": [{"type": "command", "command": "/tmp/claude-print-PID/hook.sh", "timeout": 10}]
    }]
  }
}
```

The hook script receives the JSON payload on stdin. Exit code is ignored by Claude Code (hooks are fire-and-forget). Timeout (seconds) aborts the hook process if it runs too long.

### Existing Hooks on This Server

The following hooks are active in `~/.claude/settings.json` and will fire for all claude sessions including subprocess ones:
- `PermissionRequest` → `trail-boss/trailboss-emit.sh`
- `PreToolUse` → `~/.ccdash/hooks/pre-tool-use.sh`
- `SessionEnd` → `~/.ccdash/hooks/session-end.sh` + `trailboss-emit.sh`
- `SessionStart` → `~/.ccdash/hooks/session-start.sh` + `trailboss-emit.sh`
- `Stop` → `~/.ccdash/hooks/stop.sh`
- `UserPromptSubmit` → (ccdash hook)

`trailboss-emit.sh` silently exits 0 if `$TMUX_PANE` is not set — subprocess sessions are unaffected. `ccdash` hooks update the session registry, which is correct behavior.

## Retrieving Output from an Independent Session

### Method 1: Stop Hook + JSONL (Primary)

The subprocess session fires the Stop hook when done. `claude-print` pre-installs an additional per-run hook via `--settings` overlay. The hook writes the Stop payload to a named FIFO. The parent reads the FIFO, gets `transcript_path` and `last_assistant_message`, then reads the JSONL for full text and token counts.

This is the most reliable method. Latency: Stop hook fires within 50–200 ms of the final token being generated.

### Method 2: Session-ID Pre-assignment (`--session-id`)

Assign a known UUID to the subprocess session at spawn time:
```python
import uuid
child_session_id = str(uuid.uuid4())
transcript_path = f'~/.claude/projects/{cwd_slug}/{child_session_id}.jsonl'

args = ['claude', '--session-id', child_session_id, '--dangerously-skip-permissions', ...]
```

The parent knows the JSONL path before the session starts. Can poll the file directly without waiting for a Stop hook payload. Combine with the Stop FIFO for reliable completion signaling.

### Method 3: Resume (`--resume`) — Adding to the Main Session

After a subprocess session completes, its full history (user prompts + assistant responses) is in its JSONL. The main session (or any subsequent session) can incorporate it:

```bash
# Branch from the subprocess session's history
claude --resume <child-session-id> --fork-session
```

This creates a new session that has the subprocess session's entire conversation as its history. The user (or next automated prompt) continues from that point.

Alternatively: the calling session can read the subprocess session's final response and inject it as context in the next user turn. This avoids merging session histories but achieves the same goal.

### Method 4: Structured Output Re-injection

`claude-print` emits a structured result object (`--output-format json`). The caller (e.g., NEEDLE) treats this as the final response. The caller's own session (the NEEDLE worker session) receives the result as a tool output. The subprocess session's token usage is reported in the structured result and can be forwarded to any accounting system.

This is how `claude-print` integrates with NEEDLE: NEEDLE's session sees the result as if it were a tool call output; the actual LLM work happened in the subprocess session billed separately.

## Billing Classification

The `entrypoint` field is set in `user` events in the JSONL. Observed values: `"cli"` for interactive TUI sessions. The billing classification (`cc_entrypoint` header sent to the API) is determined by the process mode at startup — if `claude` has a real TTY (checked via `isatty()`), it enters TUI mode and uses `cli`. If stdout is a pipe, it uses `sdk-cli`.

Running under `claude-print`'s PTY: `isatty(slave_fd)` returns `true` → TUI mode → `cli` billing.
Running as `claude -p`: `isatty(stdout)` returns `false` → print mode → `sdk-cli` billing.
