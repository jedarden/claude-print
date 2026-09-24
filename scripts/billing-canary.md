# Billing-classification canary

The AS-4 canary detects a Claude Code update that changes claude-print sessions
from subscription billing (`entrypoint: cli`) to the Agent SDK credit pool
(`entrypoint: sdk-cli`). It runs one minimal, one-turn Haiku request each day.

`billing-canary.sh` reads the `session_id` from that invocation's JSON result,
locates the matching transcript, and passes that exact file to
`check-billing.sh`. For older adapters that emit `session_id: null`, it runs in
a dedicated working directory and selects the one transcript created there by
the current run. This matters on ex44 and lab because concurrent NEEDLE sessions
can otherwise make an unrelated transcript appear newest.

The canary covers the JSON-evidence half of the billing-entrypoint contract:
the transcript's `entrypoint` field, the only observable proxy for the
wire-level `cc_entrypoint` header. The other half — the environment input,
`CLAUDE_CODE_ENTRYPOINT=cli` forced into the child by `FORCED_ENV`
(`src/pty.rs`) — is credential-free and is verified by `claude-print --check`
and pinned by `tests/nested_session.rs` / `tests/billing_entrypoint_contract.rs`.
See `docs/notes/billing-context.md` for the full contract.

## Pooled leg (`CLAUDE_PRINT_POOL=1`)

By default the canary runs a stateless session (its own `claude` spawn) — that
is what the daily systemd timer exercises. Setting `CLAUDE_PRINT_POOL=1` runs
the same one-turn session against an ADR-005 warm pool instead: the script
starts a `claude-print serve` daemon (pool size 1) on a private socket in the
state directory, waits for the worker's `settled and ready` line, runs the
invocation with `--pool-socket`, and tears the daemon down on exit (including
failure paths). The transcript check is identical — this proves the pool
invariant that sessions driven through a prewarmed worker also bill as
`entrypoint: cli`:

```bash
CLAUDE_PRINT_POOL=1 ./scripts/billing-canary.sh
```

The pooled leg needs an installed `claude-print` that actually has the pool
(ADR-005 landed 2026-09; a pre-pool install fails at warmup with
`pool_daemon_exited`, its log ending in `no prompt provided` because the old
binary never dispatched `serve`). After rebuilding the workspace, either
reinstall the binary or point the canary at the fresh build:

```bash
CLAUDE_PRINT_POOL=1 CLAUDE_PRINT_BIN=/path/to/claude-print ./scripts/billing-canary.sh
```

The daemon's verbose log is kept at
`~/.local/state/claude-print/billing-canary/pool-daemon.log` for diagnosis.
Every result line and journal summary carries `mode=stateless` or `mode=pooled`.

The warmup wait is 0.1 s per try, 1200 tries ≈ 120 s by default;
`CLAUDE_PRINT_POOL_WARMUP_TRIES` shortens it (used by the hermetic failure-shape
tests in `tests/billing_canary.rs`). Warmup failures are recorded as
`reason=pool_daemon_exited` (the daemon died during warmup) or
`reason=pool_daemon_warmup_timeout` (never became ready). The pooled leg's flag
contract — `--pool-socket` added, and never a `-p`/`--print` API-path flag — is
pinned by `tests/billing_canary.rs` against a fake daemon, so the classification
property the canary verifies stays testable without credentials.

## Install on each host

Run as the authenticated user that normally runs claude-print:

```bash
cd /home/coding/claude-print
./scripts/install-billing-canary.sh
systemctl --user start claude-print-billing-canary.service
```

The installer copies both scripts to `~/.local/libexec/claude-print/` and
enables `claude-print-billing-canary.timer`. The timer runs once per day, with
up to six hours of random delay, and catches up after downtime because it is
persistent. The user's systemd manager must have lingering enabled on a server
where that user may log out (`loginctl show-user "$USER" -p Linger`).

## Result and logs

Every attempt atomically replaces:

```text
~/.local/state/claude-print/billing-canary/last-result
```

A healthy result begins with `PASS`; every billing or operational error begins
with `FAIL`. Examples:

```text
PASS timestamp=2026-08-20T06:12:00Z mode=stateless entrypoint=cli session_id=... transcript=...
PASS timestamp=2026-09-19T06:44:02Z mode=pooled entrypoint=cli session_id=... transcript=...
FAIL timestamp=2026-08-21T06:10:00Z mode=stateless reason=billing_classification entrypoint=sdk-cli session_id=... check_exit=1
```

The journal also receives one machine-searchable summary per run:

```text
CLAUDE_PRINT_BILLING_CANARY status=PASS ...
CLAUDE_PRINT_BILLING_CANARY status=FAIL ...
```

Useful operator checks:

```bash
cat ~/.local/state/claude-print/billing-canary/last-result
systemctl --user status claude-print-billing-canary.timer
journalctl --user -u claude-print-billing-canary.service
```

An external heartbeat should alert if `last-result` starts with `FAIL`, is
missing, or is older than 48 hours. A failed oneshot also leaves the systemd
service in the failed state.

## Manual release gate

The automated canary supplements, but does not replace, the credential-backed
pre-release check:

```bash
./scripts/check-billing.sh
```

With no argument, `check-billing.sh` retains its original behavior and checks
the newest transcript. It also accepts an explicit JSONL path for callers such
as the canary.
