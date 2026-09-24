# Billing Context

## The billing-entrypoint contract

Three names circulate around this invariant; they are not interchangeable.

| Name | What it is | Who controls it | How it is verified |
|---|---|---|---|
| `cc_entrypoint` | Anthropic's **wire-level billing header field** on API requests. `cli` draws from the unlimited subscription; `sdk-cli` from the Agent SDK credit pool. Not an environment variable — no transcript and no env ever carries the literal name. | Claude Code, chosen at startup from the process mode (`isatty` on the TTY → TUI → `cli`) | Never directly. Observed only through the JSONL evidence below. |
| `CLAUDE_CODE_ENTRYPOINT` | The **authoritative environment input**. Claude Code's own env var; inherited as `sdk-cli` it steers a child onto the SDK path. | claude-print *forces* it to `cli` in the child environment (`FORCED_ENV` in `src/pty.rs`), overriding whatever the parent inherited — alongside the PTY that makes `isatty` true. | `claude-print --check` (credential-free): re-runs the binary's own child-env construction over an inherited `sdk-cli` and asserts `cli` comes out. Behavior pinned by `tests/nested_session.rs` and `tests/billing_entrypoint_contract.rs`. |
| `entrypoint` (JSONL field) | The **JSON evidence**. Claude Code records the classification it chose as a top-level `entrypoint` field on transcript events. | Claude Code (transcript writer) | `scripts/check-billing.sh` asserts `cli` — against the canary's exact transcript daily, and against the newest transcript manually before each release. Credential-backed. |

`CLAUDE_CC_ENTRYPOINT` is not part of this contract. It is a phantom name that
appeared once in `AGENTS.md` and matches no variable Claude Code reads, sets,
or documents; a test in `tests/billing_entrypoint_contract.rs` keeps it out of
operational surfaces.

So the causal chain reads: **FORCED_ENV forces `CLAUDE_CODE_ENTRYPOINT=cli`
(env input) → PTY makes `isatty` true → Claude Code picks TUI mode and sends
`cc_entrypoint=cli` on the wire → the transcript's `entrypoint` field records
it (JSON evidence)**. The `--check` self-test covers the first link without
credentials; AS-4 (`check-billing.sh` + canary) covers the last, which is the
only link whose failure is a real billing regression.

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
