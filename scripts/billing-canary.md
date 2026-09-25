# Billing-classification canary

The AS-4 canary detects a Claude Code update that changes claude-print sessions
from subscription billing (`entrypoint: cli`) to the Agent SDK credit pool
(`entrypoint: sdk-cli`). It runs one minimal, one-turn Haiku request each day.

`billing-canary.sh` reads the `session_id` from that invocation's JSON result,
locates the matching transcript, and passes that exact file to
`check-billing.sh`. For older adapters that emit `session_id: null`, it runs in
a dedicated working directory and selects the one transcript created there by
the current run. This matters on codinghome and lab because concurrent NEEDLE
sessions can otherwise make an unrelated transcript appear newest.

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

The canary deploys as a systemd **user** timer — no root, everything under the
deploying user's home. Run the installer as the authenticated user that
normally runs claude-print, from a checkout (it installs the four files that
sit next to it in `scripts/`):

```bash
cd /path/to/claude-print
./scripts/install-billing-canary.sh
systemctl --user start claude-print-billing-canary.service   # force the first run now
```

The checkout is only needed at install time: the timer runs the installed
copies, never the repo.

The cross-service operator sequence — install, status/log inspection,
restart/disable, removal, and the contract-drift watch that installs the
same way — is
[`docs/notes/scheduled-services-runbook.md`](../docs/notes/scheduled-services-runbook.md).

### Prerequisites

| Requirement | Enforced | On failure |
|---|---|---|
| `systemctl` on PATH | installer preflight | exit 1 **before anything is written** — no partial install |
| `claude-print` on PATH | installer preflight | exit 1 before anything is written |
| lingering enabled (servers) | warned, never blocked | the timer only runs while the user manager is alive — see [Linger](#linger) |
| `claude-print` **and** `claude` on the service unit's PATH | not preflighted | first run fails with `reason=claude_print_not_found` / `reason=invocation_failed` |

The last row is the one trap: the installer checks the *shell's* PATH, but the
service runs with the unit's pinned PATH (below). `install.sh` puts
`claude-print` in `~/.local/bin`, which the unit includes — a binary living
anywhere else installs cleanly and then fails on every timer run.

### Installed paths

| Path | Mode | Written by |
|---|---|---|
| `~/.local/libexec/claude-print/billing-canary.sh` | 0755 (dir 0700) | installer |
| `~/.local/libexec/claude-print/check-billing.sh` | 0755 | installer |
| `${XDG_CONFIG_HOME:-~/.config}/systemd/user/claude-print-billing-canary.service` | 0644 (dir 0755) | installer |
| `${XDG_CONFIG_HOME:-~/.config}/systemd/user/claude-print-billing-canary.timer` | 0644 | installer |
| `${XDG_STATE_HOME:-~/.local/state}/claude-print/billing-canary/last-result` | 0600 (dir 0700) | every run, atomically replaced |
| `…/billing-canary/workdir/` | 0700 | every run (dedicated cwd for transcript discovery) |
| `…/billing-canary/pool.sock` · `…/pool-daemon.log` | — | pooled leg only; socket removed on exit, log kept |

The state directory is created by the canary itself, not the installer. Both
scripts resolve their sibling by their own location, so the installed pair is
self-contained: `check-billing.sh` is found next to `billing-canary.sh` in
libexec, never in the repo. The service `ExecStart`s the libexec copy
(`%h/.local/libexec/claude-print/billing-canary.sh`) — editing the repo after
installing changes nothing until the installer is re-run.

### What the installer does, in order

1. Preflight: abort unless `systemctl` and `claude-print` are on PATH — before
   any directory is created.
2. Linger probe (when `loginctl` exists): print the `enable-linger` remedy if
   `Linger` is not `yes`. A warning; the install proceeds either way.
3. Copy the two scripts to libexec and the two units to the systemd user
   directory, byte-identical to the repo, with the modes above.
4. `systemctl --user daemon-reload`
5. `systemctl --user enable --now claude-print-billing-canary.timer` — the
   *timer* is active from now on; the service first runs at the next elapse
   (or immediately via the `start` above).
6. Print the result/log paths and the resulting timer table.

Re-running the installer is always safe: it is idempotent and restores any
copy or mode that drifted from the repo. The whole flow — paths, modes,
`daemon-reload` before `enable --now`, idempotence, clean prerequisite
aborts, the loud failed-enable — is pinned hermetically by
`tests/install_billing_canary.rs` (redirected `HOME`/`XDG_CONFIG_HOME`, a
PATH of fakes, no real systemd); change any documented path or message here
and that test changes with it.

### Timer and service

| Setting | Value | Effect |
|---|---|---|
| `OnCalendar` | `daily` | one elapse per day, local midnight |
| `RandomizedDelaySec` | `6h` | the actual run lands anywhere in a 6-hour window |
| `AccuracySec` | `15m` | wakeup coalescing slack on top of the random delay |
| `Persistent` | `true` | a run missed while the machine or user manager was down fires once when it comes back |
| `WantedBy` | `timers.target` | armed with the user manager |
| `Type` / `TimeoutStartSec` | `oneshot` / `10min` | one run per activation, hard-bounded |
| `UMask` | `0077` | result and state files stay user-private |

The service pins its own PATH — systemd user services do not inherit the
login shell's environment, so this list, not your shell's PATH, decides
whether a run finds `claude-print` and `claude`:

```text
%h/.local/bin:%h/.cargo/bin:/run/current-system/sw/bin:/usr/local/bin:/usr/bin:/bin
```

A binary outside those entries needs a drop-in. Drop-ins live in
`*.service.d/` and survive reinstalls — the installer rewrites only the unit
files themselves:

```bash
systemctl --user edit claude-print-billing-canary.service
# [Service]
# Environment=PATH=/opt/claude/bin:%h/.local/bin:/usr/local/bin:/usr/bin:/bin
```

### Linger

On a server, the user manager — and with it every `--user` timer — stops when
its last session logs out, unless lingering is enabled for that user. Check:

```bash
loginctl show-user "$USER" -p Linger    # want: Linger=yes
```

Enabling it needs an administrator; the installer prints exactly this command
when it detects `Linger=no`:

```bash
sudo loginctl enable-linger "$USER"
```

Where `loginctl` does not exist the check is skipped silently. Without linger
the canary still works, but only while you are logged in — which on a
headless host means barely at all.

### Verifying the installation

```bash
systemctl --user list-timers claude-print-billing-canary.timer --no-pager  # armed; NEXT/LEFT
systemctl --user start claude-print-billing-canary.service                 # run once now
journalctl --user -u claude-print-billing-canary.service -n 30 --no-pager
cat "${XDG_STATE_HOME:-$HOME/.local/state}/claude-print/billing-canary/last-result"
```

A healthy first run ends the journal with
`CLAUDE_PRINT_BILLING_CANARY status=PASS …` and leaves a `last-result`
beginning `PASS … mode=stateless entrypoint=cli`. Result formats and the
steady-state heartbeat rules (alert on `FAIL`, missing, or older than 48 h —
the 48 h covers daily + the 6 h randomization) are in
[Result and logs](#result-and-logs).

### CLAUDE_PRINT_POOL on an installed host

The timer always runs the **stateless** leg: the unit exports no
`CLAUDE_PRINT_POOL`, and user services never see the login shell's
environment anyway. That is deliberate — the daily canary exercises the
default path real sessions take. The pooled leg (see
[Pooled leg](#pooled-leg-claude_print_pool1)) runs against the installed copy
exactly like against a checkout:

```bash
CLAUDE_PRINT_POOL=1 ~/.local/libexec/claude-print/billing-canary.sh
```

After rebuilding `claude-print` in a checkout, the installed binary is stale
until you reinstall it — either run `install.sh` again, or point the canary
at the fresh build without installing (`CLAUDE_PRINT_BIN` defaults to
`claude-print` from PATH, so a normally installed host needs nothing):

```bash
CLAUDE_PRINT_POOL=1 CLAUDE_PRINT_BIN=/path/to/claude-print \
  ~/.local/libexec/claude-print/billing-canary.sh
```

Putting the pooled leg on the timer itself is possible — the same drop-in
mechanism with `Environment=CLAUDE_PRINT_POOL=1` — but not recommended: it
stops exercising the stateless path daily. The remaining knobs
(`CLAUDE_PRINT_POOL_WARMUP_TRIES`, `CLAUDE_PRINT_BILLING_STATE_DIR`,
`CLAUDE_PRINT_TRANSCRIPTS_DIR`, `CLAUDE_PRINT_CHECK_BILLING`) exist for the
hermetic tests in `tests/billing_canary.rs`, not for operations.

### Failure recovery

Every failure atomically writes a `FAIL timestamp=… reason=<why> …` line to
`last-result`, emits one `CLAUDE_PRINT_BILLING_CANARY status=FAIL …` journal
line, and leaves the oneshot service in the failed state. That state is
reporting, not a lock — the timer fires again at the next elapse regardless,
and `systemctl --user reset-failed claude-print-billing-canary.service`
clears the marker. Start with:

```bash
cat "${XDG_STATE_HOME:-$HOME/.local/state}/claude-print/billing-canary/last-result"
journalctl --user -u claude-print-billing-canary.service -n 50 --no-pager
```

Daily (stateless) failures, by `reason=`:

| `reason=` | Meaning | Recovery |
|---|---|---|
| `billing_classification` | The event the canary exists to catch: the transcript's `entrypoint` is not `cli` — sessions are drawing from the Agent SDK credit pool | Treat as a real billing regression. Keep the transcript (the line's `session_id` locates it: `find ~/.claude/projects -name '<session_id>.jsonl'`), check for a Claude Code update, run the manual gate `./scripts/check-billing.sh`. Full contract: `docs/notes/billing-context.md` |
| `claude_print_not_found` | `claude-print` is not on the service unit's PATH (the installer only checked the shell's) | Reinstall via `install.sh` — its `~/.local/bin` target is on the unit PATH — or add a PATH drop-in; retry with `systemctl --user start claude-print-billing-canary.service` |
| `billing_check_not_executable` | the installed `check-billing.sh` is missing or not executable | Re-run `./scripts/install-billing-canary.sh`; it restores the copies and modes |
| `invocation_failed` | `claude-print` ran but the session failed: expired credentials, `claude` missing or broken on the unit PATH, timeout | The journal carries the first 20 stderr lines. Re-authenticate `claude`, confirm `claude` resolves on the unit PATH, retry |
| `transcripts_directory_missing` | `~/.claude/projects` does not exist (or `CLAUDE_PRINT_TRANSCRIPTS_DIR` points nowhere) | claude has never run on the host — run one session, then retry |
| `canary_transcript_missing` · `canary_transcript_ambiguous` | no transcript matched the returned session id, or the dedicated-workdir fallback matched ≠ 1 new transcript | Usually a Claude Code format change breaking session-id/workdir discovery — capture the result line and follow `docs/notes/claude-contract-probes.md` §Maintenance |

Pooled-leg-only failures (manual runs; the daemon log is kept at
`…/billing-canary/pool-daemon.log`):

| `reason=` | Meaning | Recovery |
|---|---|---|
| `pool_daemon_exited` | the `serve` daemon died during warmup | Read `pool-daemon.log` (first 20 lines also land in the journal). Classic cause: the installed `claude-print` predates the pool — reinstall, or pass `CLAUDE_PRINT_BIN` pointing at a fresh build |
| `pool_daemon_warmup_timeout` | the worker never reached `settled and ready` within ~120 s | Same log. If warmup is genuinely that slow it is a pool problem, not a canary problem — do not paper over it with `CLAUDE_PRINT_POOL_WARMUP_TRIES` |

Installer failures are loud and clean: a missing `systemctl` or
`claude-print` aborts before anything is written (nothing to clean up), and a
failing `enable --now` — typically no user bus, because linger is off and you
logged out — exits non-zero without the success line. The files are already
on disk at that point, so fix the user manager and simply re-run. After
pulling changes that touch either script or the units, re-run the installer
on each host to refresh the copies.

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
