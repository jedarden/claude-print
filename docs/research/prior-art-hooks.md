# Prior Art: Hook Handling and Log Placement

Research conducted 2026-06-07 by examining smithersai/claude-p (Zig), hristo2612/jinn (TypeScript), and jedarden/NEEDLE plugins/claude-interactive (Python).

## Findings

### Universal: No CLAUDE_CONFIG_DIR

None of the three repos set `CLAUDE_CONFIG_DIR`. All allow the inner `claude` process to inherit the real `~/.claude` as its config directory. Transcripts land in `~/.claude/projects/<cwd-slug>/<session-id>.jsonl` directly.

### smithersai/claude-p

- Always injects `--settings <inline-json>` with only `SessionStart` + `Stop` hooks pointing to a temp relay script
- Also forwards `--setting-sources` as-is from the user's own invocation (passthrough)
- Explicitly rejects `--settings` from the user (can't override the relay)
- Child inherits full parent env + adds `CLAUDE_P_FIFO` and `TERM=xterm-256color`
- Transcript path learned from `SessionStart` hook payload (`transcript_path` field); Stop payload used as fallback
- Token counts extracted from JSONL `assistant` events; accumulates `input_tokens`, `output_tokens`, `cache_read_input_tokens`, `cache_creation_input_tokens`

### hristo2612/jinn

- Writes per-session `settings.json` to `~/.jinn/tmp/settings/<jinn-id>.json`, passed via `--settings`
- Settings file contains: SessionStart, Stop, StopFailure, PreToolUse, PostToolUse hooks → all route to a local HTTP relay
- Strips several env vars from child env: `CLAUDECODE`, all `CLAUDE_CODE_*`, `ANTHROPIC_API_KEY`, `ANTHROPIC_AUTH_TOKEN`
- Adds: `CLAUDE_CODE_DISABLE_ALTERNATE_SCREEN=1`, `CLAUDE_CODE_RESUME_TOKEN_THRESHOLD=999999999`
- Transcript path from Stop/StopFailure hook payload
- Token dedup by `message.id` (not usage fingerprint): each API call's streaming chunks share the same `message.id`; seen_ids set prevents double-counting

### jedarden/NEEDLE — claude-interactive

- No `--settings` or hook injection; user's `~/.claude/settings.json` hooks run fully inherited
- Completion detection: 30 s idle timeout after the `●` (U+25CF) bullet byte appears in PTY output
- No transcript reading; response text extracted by `pyte` screen-scraping
- Token counts always hardcoded to 0

## Implications for claude-print

1. **`--settings` for relay hook, no `CLAUDE_CONFIG_DIR`** — validated by both smithersai and jinn. Transcripts land in `~/.claude/projects/` without any forwarding step.

2. **User hooks inherit by default** — smithersai forwards `--setting-sources` passthrough; jinn's additional hooks merge with user hooks. Neither provides a disable mechanism beyond what the user controls themselves. claude-print adds `--no-inherit-hooks` (passes `--setting-sources=` to suppress user sources) as an explicit opt-out.

3. **Token dedup: prefer `message.id`** — jinn's approach is cleaner than usage-fingerprint dedup. `message.id` is unique per API call; streaming chunks share it. Usage-fingerprint fallback covers older Claude Code versions.

4. **Env var scrubbing** — jinn strips `CLAUDE_CODE_*` vars from the child env to prevent identity leakage when `claude-print` itself is running inside a Claude Code session. Specifically `CLAUDE_CODE_SESSION_ID` and `CLAUDE_CODE_SESSION_KIND` should be unset in the child.
