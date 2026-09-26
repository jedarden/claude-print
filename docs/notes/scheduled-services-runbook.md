# Scheduled Services Runbook (billing canary + contract-drift watch)

Two systemd **user** timers carry claude-print's daily operational checks on a
production host (codinghome, lab): the credential-backed billing canary
(`claude-print-billing-canary`) and the credential-free contract-drift watch
(`claude-print-contract-drift-watch`). Both installers, their units, and their
state-file shapes are pinned hermetically by
`tests/install_billing_canary.rs`, `tests/billing_canary.rs`,
`tests/install_contract_drift_watch.rs`, and `tests/contract_drift_watch.rs` —
but those tests document the machinery, not the operator sequence. This note
is that sequence: prerequisites, installation, status and log inspection,
restart/disable, removal, and failure behavior, with the exact unit names and
paths each step touches. No root anywhere — everything lives under the
deploying user's home.

Deeper context, not repeated here: the canary's internals (pooled leg,
per-`reason=` recovery table) are [`scripts/billing-canary.md`][canary-doc];
the drift watch's semantics and the re-pin procedure it alerts for are
`docs/notes/claude-contract-probes.md` §Maintenance.

[canary-doc]: ../../scripts/billing-canary.md

## The two services at a glance

| | Billing canary | Contract-drift watch |
|---|---|---|
| Timer unit | `claude-print-billing-canary.timer` | `claude-print-contract-drift-watch.timer` |
| Service unit (oneshot) | `claude-print-billing-canary.service` | `claude-print-contract-drift-watch.service` |
| What it runs | `~/.local/libexec/claude-print/billing-canary.sh` | `~/.local/libexec/claude-print/contract-drift-watch.sh` |
| What it proves | Sessions bill `entrypoint: cli` (AS-4), via one one-turn Haiku request | The repo's pinned contract evidence covers the *installed* Claude Code |
| Cadence | `OnCalendar=daily`, `RandomizedDelaySec=6h` | `OnCalendar=daily`, `RandomizedDelaySec=30m` |
| Both timers | `AccuracySec=15m`, `Persistent=true` (a missed window fires once when the user manager returns), `WantedBy=timers.target` | |
| Service hard bounds | `TimeoutStartSec=10min`, `UMask=0077` | `TimeoutStartSec=5min`, `UMask=0077` |
| Result file | `${XDG_STATE_HOME:-~/.local/state}/claude-print/billing-canary/last-result` | `${XDG_STATE_HOME:-~/.local/state}/claude-print/contract-drift-watch/last-result` |
| Success | exit 0, line starts `PASS` | exit 0, line starts `PASS verdict=current` |
| Alert states | exit 1, `FAIL reason=…` | exit 1 `DRIFT verdict=re-run-due`; exit 2 `INDETERMINATE` |
| Credentials | required (the Haiku turn) | none (the detector runs `claude --version` only) |
| Installer | `scripts/install-billing-canary.sh` | `scripts/install-contract-drift-watch.sh` |

## Prerequisites

Run everything as the authenticated user that normally runs claude-print.

**Hard at install time** (preflight aborts with exit 1 *before anything is
written* — no partial install to clean up):

| Service | Requirement | Why hard |
|---|---|---|
| both | `systemctl` on PATH | there is no systemd user manager to install into |
| canary | `claude-print` on PATH | the thing whose billing the canary exercises |
| drift watch | `bead` on PATH | the filed bead *is* the drift alert channel — a watch that cannot file would silently reintroduce the blind window the timer exists to close |

**Warnings, never blockers:**

- `claude` missing at drift-watch install time — the watch still installs and
  degrades to a loud INDETERMINATE (exit 2, red unit, state line) on every
  run until `claude` is installed; the remedy the installer prints is
  `curl -fsSL https://claude.ai/install.sh | bash`.
- Lingering disabled (checked when `loginctl` exists and `Linger` is not
  `yes`) — see [Linger](#linger).

**Runtime requirements** (not preflighted; their absence shows up as the
failure modes in the [failure table](#failure-behavior)):

- canary: `claude-print` **and** `claude` resolvable on the service unit's
  PATH, `~/.claude/projects` existing (claude has run at least once), and API
  auth for the one-turn request.
- drift watch: the contract repo checkout pinned by the unit's
  `Environment=CLAUDE_PRINT_CONTRACT_REPO=%h/claude-print` (the shared
  checkout whose pins the detector reads and whose `.beads/` workspace the
  follow-up lands in), plus `claude` and `bead` on the unit's PATH.

**The PATH trap.** The installers preflight the *shell's* PATH, but the
services run with the unit's own pinned PATH — systemd user services do not
inherit the login shell's environment:

```text
%h/.local/bin:%h/.cargo/bin:/run/current-system/sw/bin:/usr/local/bin:/usr/bin:/bin
```

`install.sh` puts `claude-print` in `~/.local/bin`, which the unit includes —
a binary living anywhere else installs cleanly and then fails on every timer
run. A binary outside those entries needs a drop-in (which survives
reinstalls — the installer rewrites only the unit files themselves):

```bash
systemctl --user edit claude-print-billing-canary.service
# [Service]
# Environment=PATH=/opt/claude/bin:%h/.local/bin:/usr/local/bin:/usr/bin:/bin
```

The drift watch has one more deliberate asymmetry: only the *watcher script*
is installed. The detector it runs
(`scripts/check-claude-version-bump.sh`) is always executed from the pinned
checkout, never from an installed copy, so the detection logic cannot go
stale the way an installed copy would.

## Linger

Both timers live in the systemd **user** manager, and without lingering that
manager exists only while you are logged in — a daily timer on a headless
host would never fire while you are away. Enabling linger keeps the user
manager (and its timers) running across logout, which is the state these
checks assume on a production host.

- Check: `loginctl show-user "$USER" -p Linger` — `yes` means the timers
  stay armed after logout.
- Remedy: `loginctl enable-linger "$USER"`, with `sudo`/polkit where the
  host demands it.
- Both installers probe this (when `loginctl` exists and `Linger` is not
  `yes`) and print the remedy as a warning — never a blocker, since the
  timers still fire while you are logged in.

The no-linger failure shape is the install-time one under
[Installation](#installation): with linger off and you logged out,
`systemctl --user enable --now` exits non-zero with no user bus and no
`[INFO] Installed and enabled` line — enable linger and re-run the
installer.

## Installation

From a checkout of the repo (the install source is the four/three files that
sit next to the installer in `scripts/`; the checkout is needed only at
install time — the timer runs the installed copies, never the repo):

```bash
cd ~/claude-print
./scripts/install-billing-canary.sh
./scripts/install-contract-drift-watch.sh

# Force the first run now instead of waiting for the next elapse:
systemctl --user start claude-print-billing-canary.service
systemctl --user start claude-print-contract-drift-watch.service
```

What each installer does, in order:

1. Preflight the hard prerequisites above; abort before any directory is
   created if one is missing.
2. Probe linger (when `loginctl` exists) and print the `enable-linger`
   remedy if disabled — a warning; the install proceeds either way.
3. Copy the scripts to `~/.local/libexec/claude-print/` and the units to the
   systemd user directory, byte-identical to the repo, with the modes below.
   The canary installs four files (its script, `check-billing.sh`, and the
   two units); the drift watch three (its script and the two units).
4. `systemctl --user daemon-reload`.
5. `systemctl --user enable --now <timer>` — the *timer* is armed from now
   on; the service first runs at the next elapse (or immediately via the
   `start` above).
6. Print the result-file path, the `journalctl` command, and the resulting
   timer table.

Re-running an installer is always safe and is the refresh procedure: it is
idempotent and restores any installed copy or mode that drifted from the
repo (byte-identical re-copy), so re-run it after pulling changes that touch
either script or the units. A failing `enable --now` — typically no user
bus, because linger is off and you logged out — exits non-zero without the
`[INFO] Installed and enabled` line, so an operator cannot believe a timer
is armed when it is not. The files are already on disk at that point; fix
the user manager and simply re-run.

### Installed paths

| Path | Mode | Written by |
|---|---|---|
| `~/.local/libexec/claude-print/billing-canary.sh` | 0755 (dir 0700) | canary installer |
| `~/.local/libexec/claude-print/check-billing.sh` | 0755 | canary installer |
| `~/.local/libexec/claude-print/contract-drift-watch.sh` | 0755 | drift installer |
| `${XDG_CONFIG_HOME:-~/.config}/systemd/user/claude-print-billing-canary.service` | 0644 (dir 0755) | canary installer |
| `${XDG_CONFIG_HOME:-~/.config}/systemd/user/claude-print-billing-canary.timer` | 0644 | canary installer |
| `${XDG_CONFIG_HOME:-~/.config}/systemd/user/claude-print-contract-drift-watch.service` | 0644 | drift installer |
| `${XDG_CONFIG_HOME:-~/.config}/systemd/user/claude-print-contract-drift-watch.timer` | 0644 | drift installer |
| `…/claude-print/billing-canary/last-result` (state) | 0600 (dir 0700) | every canary run, atomically replaced |
| `…/claude-print/contract-drift-watch/last-result` (state) | 0600 (dir 0700) | every watch run, atomically replaced |

The state directories (`${XDG_STATE_HOME:-~/.local/state}/claude-print/…`)
are created by the services themselves on first run, not by the installers.
Both scripts resolve their siblings by their own location, so the installed
copies are self-contained; the service `ExecStart`s the libexec copy —
editing the repo after installing changes nothing until the installer is
re-run.

## Status and log inspection

```bash
# Both timers: armed, next elapse, last run
systemctl --user list-timers 'claude-print-*' --no-pager

# Per service: unit state (failed after an alert run) and recent journal
systemctl --user status claude-print-billing-canary.service
journalctl --user -u claude-print-billing-canary.service -n 50 --no-pager
cat "${XDG_STATE_HOME:-$HOME/.local/state}/claude-print/billing-canary/last-result"

systemctl --user status claude-print-contract-drift-watch.service
journalctl --user -u claude-print-contract-drift-watch.service -n 50 --no-pager
cat "${XDG_STATE_HOME:-$HOME/.local/state}/claude-print/contract-drift-watch/last-result"
```

A healthy canary leaves a `last-result` beginning
`PASS … mode=stateless entrypoint=cli` and the journal ends with
`CLAUDE_PRINT_BILLING_CANARY status=PASS …`. A healthy watch leaves
`PASS timestamp=… verdict=current` and the journal ends with
`CLAUDE_PRINT_CONTRACT_DRIFT_WATCH status=PASS …`. Every run — success or
failure — writes exactly one such journal summary line and atomically
replaces the result file, so both are machine-searchable alert inputs.

External heartbeat rules: alert when a `last-result` starts with `FAIL`
(canary) or `DRIFT`/`INDETERMINATE` (watch), is missing, or is stale. The
canary's documented ceiling is 48 h (daily + the 6 h randomization); the
watch's 30-minute randomization fits comfortably under the same ceiling, so
one staleness threshold serves both. A failed oneshot also leaves its
service in the systemd failed state — `systemctl --user --failed` lists it.

## Run now, disable, re-enable

```bash
# Run once immediately, outside the schedule (still under the unit's pinned
# environment — this is the right way to test the installed service):
systemctl --user start claude-print-billing-canary.service
systemctl --user start claude-print-contract-drift-watch.service

# Follow a run live:
journalctl --user -u claude-print-billing-canary.service -f

# Stop scheduling; keep everything installed. The service can still be run
# manually with `start` as above.
systemctl --user disable --now claude-print-billing-canary.timer
systemctl --user disable --now claude-print-contract-drift-watch.timer

# Re-enable later:
systemctl --user enable --now claude-print-billing-canary.timer
systemctl --user enable --now claude-print-contract-drift-watch.timer
```

Two clarifications operators regularly need:

- **A failed unit is reporting, not a lock.** After an alert exit the
  oneshot service sits in the failed state, but the timer fires again at the
  next elapse regardless, and the next successful run clears the marker.
  `systemctl --user reset-failed <service>` clears it manually (purely
  cosmetic — it changes no scheduling and runs nothing).
- **Unit or script changes never reach the host by editing alone.** The
  timer runs the installed copies; after changing `scripts/` or the units in
  the repo, re-run the installer (it re-copies, `daemon-reload`s, and
  re-enables in one step). PATH drop-ins are the one exception — they live
  in `<unit>.d/` and survive reinstalls.

The canary's pooled leg (`CLAUDE_PRINT_POOL=1`) is a manual verification of
the pool billing path, not something the timer ever runs — see
[`scripts/billing-canary.md`][canary-doc] for it. The timer always exercises
the stateless leg real sessions take.

## Removal

There is no uninstaller; removal is the inverse of the install, per service
(this example removes the billing canary — substitute the
`claude-print-contract-drift-watch` names for the watch):

```bash
systemctl --user disable --now claude-print-billing-canary.timer
rm "${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user/claude-print-billing-canary.service" \
   "${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user/claude-print-billing-canary.timer"
rm -f "$HOME/.local/libexec/claude-print/billing-canary.sh" \
      "$HOME/.local/libexec/claude-print/check-billing.sh"
systemctl --user daemon-reload
systemctl --user reset-failed claude-print-billing-canary.service 2>/dev/null || true
```

Then, deliberately:

- **State history** (`${XDG_STATE_HOME:-~/.local/state}/claude-print/<name>/`)
  is not touched above — removing it discards the result-line audit trail
  (and the canary's `pool-daemon.log`). Remove it only if you are retiring
  the host's history with the service.
- **Drop-ins** (`…user/<unit>.service.d/`) survive both reinstalls and the
  unit-file removal above; remove the directory explicitly if one exists.
- The shared `~/.local/libexec/claude-print/` directory may be removed once
  both services' scripts are gone and nothing else you installed lives
  there.

Disabling alone (previous section) is the right move for anything that
might be re-enabled — removal is for retiring a service from a host.

## Failure behavior

Shared mechanics first: every failure still writes its `last-result` line
atomically and emits one `CLAUDE_PRINT_BILLING_CANARY` /
`CLAUDE_PRINT_CONTRACT_DRIFT_WATCH status=…` journal summary, then exits
non-zero, leaving the oneshot failed (the alert). Start triage with the
result file and the journal:

```bash
cat "${XDG_STATE_HOME:-$HOME/.local/state}/claude-print/<name>/last-result"
journalctl --user -u <service> -n 50 --no-pager
```

### Billing canary

`FAIL timestamp=… mode=… reason=<why> …`. The reasons, with recovery:

| `reason=` | Meaning | Recovery |
|---|---|---|
| `billing_classification` | The event the canary exists to catch: the transcript's `entrypoint` is not `cli` — sessions are drawing from the Agent SDK credit pool | Treat as a real billing regression: keep the transcript (the line's `session_id` locates it under `~/.claude/projects`), check for a Claude Code update, run the manual gate `./scripts/check-billing.sh` |
| `claude_print_not_found` | `claude-print` is not on the service unit's PATH (the installer only checked the shell's) | Reinstall via `install.sh` (its `~/.local/bin` target is on the unit PATH) or add a PATH drop-in; retry with `systemctl --user start` |
| `billing_check_not_executable` | installed `check-billing.sh` missing or not executable | Re-run `./scripts/install-billing-canary.sh` — it restores copies and modes |
| `invocation_failed` | the session itself failed: expired credentials, `claude` missing/broken on the unit PATH, timeout | The journal carries the first 20 stderr lines; re-authenticate `claude`, confirm it resolves on the unit PATH, retry |
| `transcripts_directory_missing` | `~/.claude/projects` does not exist | claude has never run on the host — run one session, then retry |
| `canary_transcript_missing` / `canary_transcript_ambiguous` | no transcript matched the returned session id, or the fallback matched other than exactly one | Usually a Claude Code format change breaking session-id/workdir discovery — follow `docs/notes/claude-contract-probes.md` §Maintenance |
| `pool_daemon_exited` / `pool_daemon_warmup_timeout` | pooled-leg-only (manual runs): the `serve` daemon died during warmup / never became ready | Read `…/billing-canary/pool-daemon.log`; classic cause is an installed `claude-print` predating the pool — reinstall or point `CLAUDE_PRINT_BIN` at a fresh build |

The full table with the pooled-leg detail is in
[`scripts/billing-canary.md`][canary-doc].

### Contract-drift watch

Exit 1 (`DRIFT`) is an **expected red** — it is the alert working, and it
stays red until the re-pin lands:

| Exit / state line | Meaning | Operator action |
|---|---|---|
| 1, `DRIFT verdict=re-run-due pinned=<v> live=<v> follow-up=<…>` | the installed Claude Code differs from the repo's pinned evidence | Follow `docs/notes/claude-contract-probes.md` §Maintenance (detect, re-run, re-pin, file follow-ups); the CI gate stays red until the re-pin lands. The follow-up bead is in the pinned checkout's workspace (`cd ~/claude-print && bead list`) |
| … `follow-up=filed <id>` | the drift bead was created | it is the hand-off — pick it up like any bead |
| … `follow-up=existing <id>` / `existing-closed <id>` | a bead for this live version already exists (idempotent daily repeats) | nothing new to file; the standing bead is the tracking |
| … `follow-up=not-filed …` / `failed …` | `bead` missing on the unit PATH, or `bead create` failed (its stderr is in the journal) | file the drift follow-up manually — the red unit and state line still alert |
| 2, `INDETERMINATE verdict=cannot-determine` | version could not be determined — nothing is filed and the unit fails loudly | Check `claude` resolves on the unit PATH and `claude --version` prints a parseable version; a "divergent active pins" failure means the repo checkout's active pins disagree with each other — complete the half-landed re-pin in the checkout |
| 2, `INDETERMINATE reason=detector_missing repo=<path>` | the pinned contract repo checkout is absent at `CLAUDE_PRINT_CONTRACT_REPO` | restore the checkout at `~/claude-print`, or correct the unit's `Environment=` pin and `daemon-reload` |

Note that a `DRIFT` exit files its bead only once per installed version
(`--unique-ref claude-contract-drift:live-<version>`); the watch itself is
otherwise read-only apart from its state dir.

### Installer failures (both)

Recap of the shapes pinned by the two installer suites: a missing hard
prerequisite (`systemctl`, `claude-print`, or `bead`) aborts with exit 1
before anything is written — nothing to clean up; a failing `enable --now`
fails loudly after the files are placed (fix the user manager, re-run); a
missing `claude` or disabled linger only warns.

## Hermetic coverage

| Claim | Pinned by |
|-------|-----------|
| Canary installer: paths, modes, byte-identical copies, unit contents, `daemon-reload` before `enable --now`, idempotence/drift-restore, clean prerequisite aborts, loud failed enable, linger warning only when `Linger=no` | `tests/install_billing_canary.rs` |
| Canary script: flag contract, failure shapes, result-line format | `tests/billing_canary.rs` |
| Drift-watch installer: the same contract set, plus `bead` as a hard prerequisite and the missing-`claude` warning | `tests/install_contract_drift_watch.rs` |
| Drift-watch script: exit codes, state-line shape, idempotent bead filing, unit/doc wiring fragments | `tests/contract_drift_watch.rs` |
