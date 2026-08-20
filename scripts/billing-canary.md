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
PASS timestamp=2026-08-20T06:12:00Z entrypoint=cli session_id=... transcript=...
FAIL timestamp=2026-08-21T06:10:00Z reason=billing_classification entrypoint=sdk-cli session_id=... check_exit=1
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
