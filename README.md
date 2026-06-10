# claude-print

Drop-in replacement for `claude -p` (print/headless mode) that drives the Claude Code interactive TUI via PTY — preserving subscription billing after the June 15, 2026 Agent SDK credit split.

## Why this exists

Starting June 15, 2026, Anthropic separates `claude -p` (headless) into a separate Agent SDK credit pool ($100–$200/month on Max plans). Only the interactive TUI (`cc_entrypoint=cli`) continues drawing from the unlimited subscription. `claude-print` wraps the interactive TUI in a PTY so callers get `claude -p` wire-compatible output while billing against the subscription.

## Install

```sh
sh install.sh
```

Or set `SKIP_MOCK_CLAUDE=1` to skip the `mock_claude` test fixture.

## Usage

```
claude-print [OPTIONS] [PROMPT]
```

Reads prompt from positional argument, `--input-file`, or stdin (when not a TTY).

## Flags

| Flag | Short | Default | Description |
|------|-------|---------|-------------|
| `[PROMPT]` | | | Prompt string (mutually exclusive with `--input-file` and stdin) |
| `--input-file <FILE>` | `-f` | | Read prompt from file |
| `--model <MODEL>` | `-m` | `claude-sonnet-4-6` | Model to use |
| `--max-turns <N>` | | `30` | Maximum number of turns |
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

## Exit codes

| Code | Meaning |
|------|---------|
| `0` | Success |
| `1` | Assistant error (`is_error: true` in transcript) |
| `2` | Internal error (PTY spawn, hook setup, parse failure) |
| `124` | Timeout exceeded |
| `130` | Interrupted (SIGINT) |

## NEEDLE integration

Copy `claude-print.yaml` to `~/.needle/agents/` (handled automatically by `install.sh`).

## Structure

- `docs/notes/` — design decisions, constraints, integration details
- `docs/plan/plan.md` — complete implementation plan
