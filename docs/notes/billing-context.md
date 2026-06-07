# Billing Context

## The June 15, 2026 Split

Anthropic's billing header field `cc_entrypoint` determines which pool a request draws from:

- `cc_entrypoint=cli` → interactive TUI → unlimited subscription
- `cc_entrypoint=sdk-cli` → `claude -p` / Agent SDK → monthly credit pool

Credit pool sizes: Pro $20/mo, Max 5x $100/mo, Max 20x $200/mo. No rollover.

`claude -p` is currently misclassified as `sdk-cli` even for subscription users (GitHub issue #59105 — acknowledged by Anthropic, not fixed). The June 15 change formalizes this split rather than fixing the classification.

## Why PTY Preserves `cli` Billing

Running `claude` under a real PTY (via `forkpty`) produces `cc_entrypoint=cli` because:
1. `claude` detects it has a real TTY on stdout
2. It enters interactive/TUI mode
3. The billing header is set at startup based on the entrypoint mode

Any wrapper that provides a PTY inherits the `cli` classification. Screen-scraping and hook-based approaches extract the response without changing the billing header.

## NEEDLE Integration

The NEEDLE agent config (`claude-print.yaml`) replaces `claude-anthropic-sonnet.yaml` for workers that should bill against the subscription. Install by running `install.sh`, which copies the YAML to `~/.needle/agents/`.
