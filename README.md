# claude-print

Drop-in replacement for `claude -p` (print/headless mode) that drives the Claude Code interactive TUI via PTY — preserving subscription billing after the June 15, 2026 Agent SDK credit split.

## Why this exists

Starting June 15, 2026, Anthropic separates `claude -p` (headless) into a separate Agent SDK credit pool ($100–$200/month on Max plans). Only the interactive TUI (`cc_entrypoint=cli`) continues drawing from the unlimited subscription. `claude-print` wraps the interactive TUI in a PTY so callers get `claude -p` wire-compatible output while billing against the subscription.

## Structure

- `docs/notes/` — design decisions, constraints, integration details
- `docs/plan/plan.md` — complete implementation plan
