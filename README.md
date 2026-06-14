# claude-print

Drop-in replacement for `claude -p` (print/headless mode) that drives the Claude Code interactive TUI via PTY — preserving subscription billing after the June 15, 2026 Agent SDK credit split.

## Why this exists

Starting June 15, 2026, Anthropic separates `claude -p` (headless) into a separate Agent SDK credit pool ($100–$200/month on Max plans). Only the interactive TUI (`cc_entrypoint=cli`) continues drawing from the unlimited subscription.

The billing path is determined by an `isatty` check inside the `claude` binary: when stdout is a TTY, the session is tagged `cc_entrypoint=cli` and billed against the subscription. When stdout is a pipe (as with `claude -p`), it becomes `cc_entrypoint=sdk-cli` and draws from the credit pool instead.

`claude-print` allocates a PTY, drives the interactive TUI over it, auto-dismisses the trust dialog, injects the user prompt via bracketed paste, waits for the Stop hook via a FIFO, reads the JSONL transcript, and emits clean stdout output — giving callers `claude -p` wire-compatible output while billing against the subscription.

## Prerequisites

- **Claude Code** must be installed and authenticated. See [claude.ai/code](https://claude.ai/code).
- An active **Claude subscription** (Pro or Max plan) is required. The whole point is to bill against subscription, not credits.
- Linux only. PTY support requires POSIX — no Windows ConPTY.

## Install

```bash
sh install.sh
```

`install.sh` downloads a pre-built static musl binary from GitHub Releases (`jedarden/claude-print`), runs `--check` to verify the setup, and copies `claude-print.yaml` to `~/.needle/agents/` if NEEDLE is present.

Set `SKIP_MOCK_CLAUDE=1` to skip the `mock_claude` test fixture download.

### Build from source

```bash
git clone https://github.com/jedarden/claude-print
cd claude-print
cargo build --release
# binary at target/release/claude-print

# fully static binary (recommended for deployment):
cargo build --target x86_64-unknown-linux-musl --release
```

Architectures: `x86_64` and `aarch64`.

## Self-check

After install, verify the PTY, Stop hook, and billing entrypoint:

```bash
claude-print --check
```

This confirms `cc_entrypoint=cli` appears in the session JSONL. `install.sh` runs this automatically, but it's worth running manually after upgrades.

## Usage

```
claude-print [OPTIONS] [PROMPT]
```

Reads the prompt from a positional argument, `--input-file`, or stdin (when not a TTY). These are mutually exclusive.

### Examples

```bash
# Positional prompt
claude-print "Summarize this in one sentence"

# Stdin pipe
echo "what is the capital of France?" | claude-print

# File input
claude-print --input-file prompt.txt

# Specify a model
claude-print --model claude-opus-4-8 "Write a haiku about Rust"

# JSON output
claude-print --output-format json "what is 2+2?" | jq .text

# Stream-JSON — real-time JSONL event replay
claude-print --output-format stream-json "Write a story"

# Agentic task with tool use
claude-print --max-turns 5 "List files in current dir and summarize"

# Short timeout for quick questions
claude-print --timeout 30 "quick question"
```

## Flags

| Flag | Short | Default | Description |
|------|-------|---------|-------------|
| `[PROMPT]` | | | Prompt string (mutually exclusive with `--input-file` and stdin) |
| `--input-file <FILE>` | `-f` | | Read prompt from file |
| `--model <MODEL>` | `-m` | `claude-sonnet-4-6` | Model to use |
| `--max-turns <N>` | | `30` | Maximum agentic turns |
| `--output-format <FORMAT>` | `-o` | `text` | Output format: `text`, `json`, `stream-json` |
| `--allowedTools <LIST>` | | | Comma-separated list of allowed tools |
| `--disallowedTools <LIST>` | | | Comma-separated list of disallowed tools |
| `--dangerously-skip-permissions` | | | Skip permission prompts (dangerous) |
| `--timeout <SECS>` | | `3600` | Wall-clock timeout in seconds |
| `--claude-binary <PATH>` | | PATH lookup | Path to claude binary |
| `--no-inherit-hooks` | | | Disable user hook inheritance |
| `--verbose` | | | Write timing traces to stderr |
| `--check` | | | Run installation self-test and exit |
| `--version` | `-V` | | Print version and exit |
| `--help` | `-h` | | Print help |

## Output formats

- `text` (default): plain text response, printed to stdout.
- `json`: one-line JSON object with `text`, `session_id`, `model`, and `usage` fields.
- `stream-json`: JSONL replay of the raw transcript events in real time, one event per line.

## Exit codes

| Code | Meaning |
|------|---------|
| `0` | Success |
| `1` | Assistant error (`is_error: true` in transcript) |
| `2` | Internal error (PTY spawn, hook setup, parse failure) |
| `3` | Timeout exceeded |
| `4` | Bad input (missing prompt, unreadable file) |
| `130` | Interrupted (SIGINT) |

## How it works

1. **PTY fork** — spawns `claude` under a PTY so `isatty` returns true and the session is tagged `cc_entrypoint=cli`.
2. **Trust dialog dismiss** — watches for the one-time "do you trust this project?" prompt and sends the confirmation keypress automatically.
3. **Bracketed paste injection** — sends the prompt wrapped in bracketed-paste escape sequences (`\x1b[200~` / `\x1b[201~`), which Claude Code's TUI accepts as user input without triggering shell interpretation.
4. **Stop hook FIFO** — installs a temporary Claude Code Stop hook that writes a payload to a FIFO when the response is complete; the process blocks on the FIFO read.
5. **Transcript read** — reads the JSONL session transcript Claude writes to `~/.claude/projects/`, extracts the assistant turn, and emits it in the requested format.

## NEEDLE integration

If you use NEEDLE for LLM fleet dispatch, `install.sh` automatically copies `claude-print.yaml` to `~/.needle/agents/`. This registers `claude-print` as the adapter for Anthropic subscription models (sonnet/opus/haiku) so NEEDLE workers bill against the subscription rather than the Agent SDK credit pool. See `claude-print.yaml` in the repo root for the full adapter config, including `--no-inherit-hooks` isolation mode and the `use_or_lose` cost type.

## Limitations

- **Linux only** — PTY allocation is POSIX. No Windows ConPTY support.
- **Claude Code must be authenticated** — `claude-print` delegates entirely to the `claude` binary; it cannot authenticate on its own.
- **One prompt per invocation** — there is no multi-turn session mode; each call starts a fresh session.
- **Startup latency ~2–5s** — the PTY handshake and Claude Code startup add overhead versus a direct HTTP call.

## Structure

- `docs/notes/` — design decisions, constraints, integration details
- `docs/plan/plan.md` — complete implementation plan
