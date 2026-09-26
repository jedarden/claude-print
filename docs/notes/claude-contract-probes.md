# Claude Code Runtime Contract Probes — measured behavior

**Measured against:** `claude` 2.1.282 (`2.1.282 (Claude Code)`), 2026-09-24, on the
claude-print dev host. Probe harness: `scripts/probe-claude-contracts.sh` (merge /
suppression / single-turn Stop), `scripts/probe-stop-toolallowed.sh` (multi-round
Stop contract, print mode), `scripts/probe-tui-second-turn.sh` (TUI
once-per-turn contract), and `scripts/probe-stop-edge-contracts.sh` (the edge
measurements — sleeping-hook cross-source concurrency and degraded-path
permission-denied Stop counts, added 2026-09-25; relay-hook timeout
enforcement, added 2026-09-26).
Re-run them after any Claude Code update; results are
version-pinned — the maintenance workflow (drift detection, re-run procedure,
re-pin checklist, follow-up rule) is defined in §Maintenance at the bottom of
this file and made executable by `scripts/contract-maintenance-gate.sh`
(detection step: `scripts/check-claude-version-bump.sh`), which CI runs on
every push as a mandatory gate — drift fails the build until the re-pin lands
(see §Maintenance → Wiring).
Resolves plan PO-1, PO-2, OQ-1, OQ-2 and the Stop-poller
assumption (plan §7 Stop Poller and the glossary Stop-hook note).

**Re-measurement history.** First measured against 2.1.270 on 2026-09-13
(claudepr-6ef2541c). On 2026-09-24 (claudepr-3094ab2e — the re-pin that also
made drift a mandatory CI gate) the suite ran **twice**: fully against
2.1.281, then the host auto-updated to 2.1.282 mid-re-pin and the whole
suite was re-run against 2.1.282 the same evening — two consecutive versions,
identical contracts, and a live demonstration of the drift cadence the gate
exists for. The runs: merge (P2/P6), suppression (P3), `=none` rejection
(P5) and the once-per-source single-turn Stop all reproduced exactly on both;
the `--max-turns` cutoff again fired no Stop (T3, exit 1); the permitted-tool
arms re-measured the multi-round and TUI Stop contracts
(`probe-stop-toolallowed.sh`, `probe-tui-second-turn.sh` — outcomes recorded
in §Stop firing contract below). Evidence tables below cite the 2.1.282
re-measurement — the same version the **Measured against:** stamp and the
active fixtures pin — so the doc body upholds the one-pin invariant the gate
enforces on the fixtures. The two 2.1.270 measurements the 2.1.281/2.1.282
re-pins had left unrepeated — the sleeping-hook concurrency probe and the
degraded-path extra Stop — were re-measured against that same pinned 2.1.282
on 2026-09-25 (claudepr-d9553d38, `scripts/probe-stop-edge-contracts.sh`,
which is now part of the probe set): concurrent cross-source execution
reproduced in **12/12** event pairs and the extra-Stop hazard **did not
reproduce** (15/15 permission-denied runs across three invocations fired
exactly one Stop). The relay-hook timeout-enforcement contract (Arm T of the
same script, added 2026-09-26, claudepr-352cf1df) was likewise measured
against that same pinned 2.1.282 — §Hook-timeout enforcement below — so every
contract this document states is backed by a measurement of the pinned
version. The only
2.1.270 numbers still quoted below — the superseded TUI timings kept for
per-version contrast — carry an explicit historical attribution where they
appear. Per-version fixtures:
`tests/fixtures/claude_contracts_v2.1.281.json` (the earlier same-day run)
and `tests/fixtures/claude_contracts_v2.1.282.json` (the pinned one).

That incident also exposed a design gap that outlived it: the straddle was
recoverable only because it was *noticed* — an unnoticed one would have
pinned mixed-version evidence that neither the gate nor the one-pin invariant
can detect after the fact. Since 2026-09-26 (claudepr-9fe76ef4) the probes
are guarded against straddling at all: each run pins the binary it resolved
at start and aborts as failed unless that binary's version holds to the end
of the run (§Version guard below).

The maintenance gate covers both active version-pinned fixture families:
`tests/fixtures/claude_contracts_v*.json` (the active reference is the
fixture selected by `tests/claude_contracts.rs`) and
`tests/fixtures/stream_json_golden_v*` (the active
`.{input,expected,errors}.jsonl` triple selected by
`tests/stream_json_contract.rs`). Historical captures may remain beside the
active files, but every active reference must be re-measured and re-pinned
with the same Claude version — and since claudepr-b590e46d (2026-09-25) the
active pins must agree with each other and with the **Measured against:**
stamp as one measurement of one version; the detector rejects divergence
outright (exit 2, no claude consulted), and the always-on
`active_fixture_families_share_one_pinned_version` test in
`tests/contract_maintenance.rs` enforces the same invariant in every
`cargo test` run while explicitly exempting unreferenced historical files.
The stream-json golden family — born pinned to 2.1.270 while the runtime pin
had moved to 2.1.282, leaving the gate intentionally red (claudepr-e65ab413)
— was re-measured and re-pinned to 2.1.282 on 2026-09-25: two sandboxed PTY
sessions through claude-print itself re-verified the PTY transcript shape
(no `result` record; compact JSON; no CR/blank lines), the 2.1.282
user/assistant/`mode` record envelopes (the assistant envelope now carries
`session_id` *and* `sessionId`, `model`, `stop_reason`, and an expanded
`usage`; 2.1.282 also writes `mode`, `permission-mode`, `attachment`,
`last-prompt`, `ai-title`, `cost-state` records — all forwarded, the reader
is type-agnostic), and — observed live for the first time on 2.1.282 —
`thinking` blocks (`type`/`thinking`/`signature`) and split assistant
records sharing a `message.id` with distinct `uuid`s. The regenerated
`stream_json_golden_v2.1.282` triple was produced through the capture path
(expected = a replay of the staged input by the real reader thread, errors =
the real `emit_error`), never hand-edited; the `v2.1.270` family is retained
beside it as measurement history.

**Isolation:** every probe ran with `HOME` redirected into a throwaway
`mktemp` sandbox (fresh `.claude.json`, trust pre-seeded for the probe cwd
only). The host's real `~/.claude/settings.json`, `~/.claude.json`, and
credentials were never read, copied, or modified. Auth traveled by inherited
environment only. Probe hooks wrote only inside the sandbox; evidence below is
timestamps, source tags, and event names — no payload contents.

**Probe wiring:** two hook sources installed the same pair of hooks
(`SessionStart` + `Stop`), each appending `timestamp|tag|event` to one shared
log: a **project source** (`.claude/settings.json` in the probe cwd) and a
**`--settings` file** (the "relay" — what claude-print passes). Where a user
source was exercised, the sandbox `~/.claude/settings.json` carried the same
wiring. The settings schema used was claude-print's double-nested
`hooks.Stop[ { hooks: [ {type: "command", ...} ] } ]` form — accepted and
fired by every measured version (2.1.270, 2.1.281, 2.1.282), which also
live-verifies the Hook Installer §2 schema note.

## PO-1 / OQ-1 — `--settings` merge: CONFIRMED; firing order: NOT CONTRACTUAL

Evidence (2.1.282; P2: project source + `--settings`; P6: user source +
`--settings`; each a real completed `claude -p` turn):

| Run | Event | Firings in order |
|---|---|---|
| P2 | SessionStart | relay `…292.005941` → project `…292.008763` (~2.8 ms) |
| P2 | Stop | project first → relay (~3 ms) |
| P6 | SessionStart | user + relay, both fired (pair order not logged) |
| P6 | Stop | user `…320.628` → relay `…320.632` (~4 ms) |

- `--settings <file>` hooks fire **alongside** standard-source hooks — merge,
  not replace. Every loaded source fired on every event. PO-1's
  in-process-merge fallback is not needed.
- **Order across sources is not a contract.** On 2.1.282 the flip is visible
  in the ordinary probe pairs above, with no experiment at all: P2's
  SessionStart pair fired **relay-first** (~2.8 ms) while P2's Stop pair in
  the same run fired project-first (~3 ms), and P6's Stop pair fired
  user-first (~4 ms). The same-day 2.1.281 run of the same pairs fired
  standard-source-first throughout (2–3 ms; P2 SessionStart project
  `…839.154` → relay `…839.157`, P6 user Stop `…870.244` → relay
  `…870.246`) — which order materializes is run-dependent, not a property
  of the source or the version. The dedicated sleeping-hook probe (project
  hook sleeping 300 ms and logging start/end; relay hook sleeping 0 ms —
  `scripts/probe-stop-edge-contracts.sh` Arm S) supplies the direct
  concurrency proof a timestamp pair alone cannot. Re-measured against the
  pinned 2.1.282 on 2026-09-25 (claudepr-d9553d38; 6 runs × 2 event pairs):
  the two hooks ran **concurrently in 12 of 12 pairs** — the relay always
  started while the project hook's sleep was still running (project wall
  time 305–310 ms in every firing, so the sleep demonstrably executed), and
  the relay **started before** the project hook in **6 of 12 pairs** (relay
  2.3–6.0 ms ahead: 3 SessionStart pairs, 3 Stop pairs; two earlier
  same-day invocations of the same probe against the same binary measured
  the same shape — concurrent 12/12 each, flips 4/12 and 5/12, so the flip
  rate itself is unstable). That reproduces the original 2.1.270 finding
  (concurrent firing observed; start-order flip in 1 of 4 runs) on a later
  version. The guaranteed
  property is only that **all loaded sources fire**.
- OQ-1's read-race clause, resolved as "cannot be ordered away": the relay may
  deliver the payload while user Stop hooks are still starting or running, so
  claude-print *can* observe the Stop before a slow user hook finishes. That
  is acceptable here: the two consumers are independent (claude-print reads
  the payload + transcript and tears down; user hooks post-process
  separately), the single-prompt session shape means no later turn depends on
  hook output, and the pre-existing teardown bounds (graceful child exit
  window, then SIGTERM/SIGKILL; `--stop-hook-timeout` watchdog) are unchanged.
  Do not add logic that assumes the relay fires last.

## OQ-2 / PO-2 — `--setting-sources` suppression: CONFIRMED (primary spelling works)

- P3: `claude -p --setting-sources= --settings <relay>` — **zero** firings from
  the project source (SessionStart and Stop both suppressed; cumulative counts
  unchanged), while the `--settings` relay hooks **still fired** (SessionStart
  and Stop each +1). The empty spelling is accepted, suppresses all standard
  sources, and does **not** suppress the `--settings` file — exactly the
  semantics `--no-inherit-hooks` mode requires. PO-2's fallbacks are moot.
- P4: `--setting-sources=user` selects the user source (sandbox user
  SessionStart fired once; project source absent).
- P5: `--setting-sources=none` — **rejected**, exit 1, before any session
  starts and with zero hook firings: `Error processing --setting-sources:
  Invalid setting source: none. Valid options are: user, project, local`.
  The PO-2 fallback spelling is rejected identically by every measured
  version (2.1.270 through 2.1.282); do not adopt it.
  (Side observation: `--allowedTools` consumes a variadic value list — pass it
  as `--allowedTools=Bash` or it will swallow a following positional prompt.)

## Hook-timeout enforcement — an overrun relay hook is killed; the session proceeds

`docs/notes/hook-design.md` §Relay Hook configures both relay hooks with
`"timeout": 10` and states Claude Code "does not wait beyond the 10s timeout"
— a claim the merge/suppression/Stop pins above did not cover. Measured
directly against the pinned 2.1.282 (Arm T of
`scripts/probe-stop-edge-contracts.sh`, added 2026-09-26, claudepr-352cf1df;
the host binary had auto-updated to 2.1.283, so the persisted
`~/.local/share/claude/versions/2.1.282` was measured on a PATH shim —
§Reproducing): the relay-position hook (a `--settings` file, run with
`--setting-sources=` so it is the only loaded hook — claude-print's
isolation-mode shape) was configured with a per-hook `"timeout": 5` while
its script slept 30 s, wired on SessionStart and Stop, across two
invocations of 4 real single-turn runs each (a full three-arm run of the
script, then an Arm-T-only run with `ARM_S_RUNS=0 ARM_D_RUNS=0`):

| Signal | Result (2.1.282, 2026-09-26) |
|---|---|
| Hook killed before its sleep finished — `end` line never logged after `start` | **16/16** event firings (per invocation: 4 SessionStart + 4 Stop) |
| Session proceeds — claude exit code | **8/8** runs exit 0 |
| Session proceeds — reply rendered | 8/8 runs (`reply-contains-OK` 4/4 per invocation) |
| claude process exit − Stop hook start | **5.0 s** every run (8/8) — the configured 5 s timeout, not the 30 s sleep |

Conclusions:

- The per-hook `timeout` field is **enforced by kill**, not advisory: a hook
  that outlives it never completes (in every firing the hook's post-sleep log
  write never happened).
- The session is **not blocked beyond the timeout**: `-p` runs exit 0 with
  the reply rendered, and the claude process exits ≈ the configured timeout
  after the Stop hook starts. Claude Code waits *up to* the timeout, then
  kills and moves on — exactly the clause the relay design relies on when it
  bounds `hook.sh`/`identity.sh` at 10 s. Enforcement was measured at 5 s; it
  is enforcement *of the field* `src/hook.rs` sets to 10 on both relay hooks,
  so the relay value is the same mechanism at a different setting.
- claude-print's own relay hooks exit in milliseconds (`cat > target ||
  true`); this contract bounds the pathological case — a relay hook that
  hangs — at the configured timeout per hook event rather than an unbounded
  stall, alongside the `--stop-hook-timeout` watchdog that bounds the
  session as a whole.

## Stop firing contract — single-turn baseline

Every completed single-turn `claude -p` run (P1, P2, P3, P4, P6: no tool use)
fired **exactly one Stop per loaded source** (e.g. 2.1.282 P6: user Stop +
relay Stop, ~4 ms apart; P3: relay Stop only). No run produced zero Stops; no
run produced more than one Stop per source — re-verified unchanged on 2.1.281
and 2.1.282, whose fixtures pin the count at 1 per loaded source.

## Stop firing contract — multi-round tool use, TUI turns, cutoffs

The first-round probes (T1/T2 in `probe-claude-contracts.sh`) ran without a
tool allowlist, so the model's Bash calls were permission-denied and their
Stop counts belong to degraded runs — one of the two degraded runs produced an
**extra** Stop firing (2 for one prompt; a 2.1.270 measurement, kept as the
historical hazard note below). Those counts were discarded and the contract
was re-measured with tools actually permitted
(`scripts/probe-stop-toolallowed.sh`, `scripts/probe-tui-second-turn.sh`);
counts and timings below are the 2.1.282 run — the pinned version. The
degraded path itself — deliberately re-entered, not just discarded — was
re-measured against 2.1.282 on 2026-09-25 (`scripts/probe-stop-edge-contracts.sh`
Arm D: 15 completed permission-denied runs across three invocations, every one
firing exactly one Stop — see the hazard note below the table). Measured
scenarios:

| Scenario | Stop firings | Evidence |
|---|---|---|
| Single-turn `claude -p`, no tools | 1 per loaded source | P1–P6 (above) |
| `claude -p`, two sequential permitted Bash rounds (`--allowedTools=Bash`) | **1**, at the turn's true end | one firing, `last_assistant_message: "DONE"`, `stop_hook_active: false`, single session id |
| `claude -p`, tool calls permission-denied (degraded, no allowlist) | **1** | Arm D (2.1.282, 2026-09-25): 15/15 completed denied runs across three invocations fired exactly one Stop each (`stop_hook_active: false`); the 2.1.270 hazard — one extra Stop in 1 of 2 runs — did not reproduce |
| TUI, turn 1 (plain reply) | **1** | firing + reply both observed (41.6 s) |
| TUI, turn 2 in the same session (plain reply) | **1** | firing + reply both observed (3.3 s); TUI status line showed `(running stop hook)` |
| `claude -p` cut off by `--max-turns 2` mid-task | **0** | exit code 1, zero firings across the run |

*(Timings in the table above are the 2.1.282 measurement. Full 2.1.282
evidence: Arm P — exit 0, one firing, `last_assistant_message: "DONE"`,
`stop_hook_active: false`, single session id; T1 — completed (exit 0) with
exactly one firing of its own; TUI turns — 1 Stop each, 62.5 s / 32.5 s
(probe-1 T2, per loaded source), 60.5 s (Arm T multi-round), and 41.6 s /
3.3 s (probe-tui-second-turn, verdict "once-per-turn confirmed", both
replies rendered); cutoff (T3) — 0 firings, exit 1. The same-day 2.1.281
run measured the same contracts (Arm T clean at 48.0 s / 60.0 s;
probe-tui-second-turn 13.0 s / 9.1 s), and the original 2.1.270 run
(claudepr-6ef2541c) had measured 22.6 s / 15.1 s for the plain TUI turns —
per-version numbers live in the fixtures. Incomplete second turns — one per
evening, no reply and no Stop after a long wait — occurred once per run and
are re-runs, not findings, per §Re-run below.)*

Conclusions:

- Stop fires **once per completed turn**, not once per API round and not once
  per session: a multi-round tool-using turn ends with exactly one Stop, and a
  second prompt in the same TUI session gets its own Stop.
- For claude-print's one-prompt-per-session shape, the first (and only) Stop
  payload is the terminal signal — the Stop Poller's single-fire design is
  correct, and the plan's conditional fallback ("match on the JSONL `Result`
  event") is both unneeded and unusable: a PTY/TUI transcript contains no
  `type: "result"` event (documented in `src/transcript.rs`).
- A `--max-turns` cutoff fires **no** Stop; the child exits with an error.
  That case belongs to the `--stop-hook-timeout` watchdog, not the poller.
- Known hazard (not reachable in the NEEDLE fleet — `claude-print.yaml` passes
  `--dangerously-skip-permissions`): on headless runs where the model's tool
  calls are permission-denied, an extra Stop firing was observed once (2
  firings for one prompt — a 2.1.270 measurement, historical).
  **Re-measured against the pinned 2.1.282 on 2026-09-25** (claudepr-d9553d38,
  `scripts/probe-stop-edge-contracts.sh` Arm D): 15 completed
  permission-denied runs across three invocations of the probe —
  all exit 0, the model's own final message confirming its Bash calls were
  blocked, `stop_hook_active: false` on every firing — and **every run fired
  exactly one Stop**; the hazard did not reproduce. It stays on record as
  historical: the mechanism was never explained, the reproducing sample was
  two runs, and the single-fire poller's tolerance
  (first payload → transcript retry/fallback path; the watchdog still bounds
  the session) is kept as cheap insurance. Re-measure (Arm D) if
  permission-denied paths ever become a supported mode.

(first filled in by the claudepr-6ef2541c probe run against 2.1.270; the
counts and timings above are re-cited from the 2.1.282 re-measurement — see
plan.md §Stop Poller for the concluded contract)

## Reproducing

```bash
bash scripts/probe-claude-contracts.sh        # merge/suppression/single-turn Stop
bash scripts/probe-stop-toolallowed.sh        # multi-round Stop contract (print mode)
bash scripts/probe-tui-second-turn.sh         # TUI once-per-turn contract
bash scripts/probe-stop-edge-contracts.sh     # sleeping-hook concurrency + degraded-path Stop counts + hook-timeout enforcement
```

Each run is self-contained (sandboxed HOME, scrubbed `CLAUDECODE*` env, forced
`CLAUDE_CODE_ENTRYPOINT=cli` + `CLAUDE_CODE_FORCE_SESSION_PERSISTENCE=1`,
mirroring `src/pty.rs`'s child-environment contract) and prints its version
stamp. Since 2026-09-26 every run is also **version-guarded**
(§Version guard): the measured binary is resolved and pinned at start, and
the run exits non-zero if its version changed by the end — a straddled run's
evidence is invalid and must be re-measured, never cherry-picked from.
Runtime ≈ 1–6 minutes each (model turns via the configured provider;
the Arm T default adds 4 more turns to `probe-stop-edge-contracts.sh`).
`tests/claude_contracts.rs` pins the fixture and re-verifies the cheap
contracts live under `cargo test --test claude_contracts -- --ignored`.

During a drift window (the host auto-updater has repointed `~/.local/bin/
claude` past the **Measured against:** stamp, but the re-pin hasn't landed),
run the probes against the *pinned* version rather than polluting the pin
with the newer binary's numbers: old versions persist under
`~/.local/share/claude/versions/`, so put a symlink named `claude` on PATH
ahead of it —

```bash
mkdir -p /tmp/claude-pin-<v> && ln -sf ~/.local/share/claude/versions/<v> /tmp/claude-pin-<v>/claude
PATH="/tmp/claude-pin-<v>:$PATH" bash scripts/probe-stop-edge-contracts.sh
```

— and let the probe's own version stamp identify what was measured (the
Arm T evidence above was gathered this way, against 2.1.282 while the host
binary had drifted to 2.1.283).

Two probe-authoring notes for whoever reruns these: `--allowedTools` takes a
variadic value list, so pass `--allowedTools=Bash` (equals form) or it will
swallow a following positional prompt; and a hook log of the form
`timestamp|payload` must be counted with a payload-substring filter, not a
field-count check (a filter written for the 4-field `ts|tag|event|payload`
layout silently counts zero on the 2-field layout, which invalidated one TUI
driver run before this was caught).

## Version guard — a mid-run auto-update cannot straddle a measurement

The 2026-09-24 double run (§Re-measurement history) was benign only because
the straddle was noticed. The dangerous shape is the one that is not: an
auto-update landing mid-run mixes measurements from two Claude versions into
one probe session, and a re-pin built on it would stamp one version while
individual evidence numbers came from another — the **Measured against:**
stamp and the active fixtures would "agree" (satisfying the one-pin invariant
claudepr-b590e46d enforces) while being false. Detection after the fact
cannot catch that, so since 2026-09-26 (claudepr-9fe76ef4) every
`probe-*.sh` refuses to produce mixable evidence in the first place.
`scripts/probe-version-guard.sh` owns the mechanism, with two layered
defenses:

- **Pin.** `probe_version_guard_begin` resolves `CLAUDE_BIN` to its final
  symlink target once (`~/.local/bin/claude` is a symlink the auto-updater
  repoints; the versions it leaves behind persist under
  `~/.local/share/claude/versions/`). Every claude invocation in the run then
  execs that one concrete binary, so a mid-run repoint cannot redirect a
  measurement — the 2026-09-24 shape cannot occur at all.
- **Bracket.** `probe_version_guard_end` — the last line of every probe —
  re-runs `--version` on the pinned binary after the measurements and
  compares. This catches the remaining straddle shape, the resolved path
  changing content in place (an install swapping files under a stable path),
  and any mismatch aborts the run as failed: exit 1, the verdict line
  `version-guard: verdict=STRADDLED start=<a> end=<b>`, and an ERROR naming
  the evidence untrustworthy. A missing or unparsable version fails closed
  with exit 2 instead. A stable run ends by stamping the machine-readable
  line the re-pin records beside the fixture write (§Re-pin):

  ```text
  version-guard: verdict=single-version start=<v> end=<v> binary=<path>
  ```

  If the *host* launcher moved on while the run stayed pinned, a final
  `version-guard: note=host-claude-moved-on path-now=<v>` line says so: the
  run's evidence is still single-version (every measurement used the pinned
  binary), but the operator re-pins knowingly — that is the 2026-09-24 shape
  made visible instead of silent.

`scripts/contract-maintenance-gate.sh` brackets itself the same way
(claudepr-9fe76ef4): it captures the live version before detection,
re-captures before writing its status, and a mid-gate update voids the
verdict — `version-stability: straddled (start=… end=…)` in
contract-status.txt, exit 2 (INDETERMINATE, fail closed), with the re-run as
the next step. A probe that straddled under `--run-probes` records it itself
(non-zero `probe-exit` in `probes/<script>.txt`), and its evidence must be
discarded with the gate's.

**Pinning a whole re-pin session.** The per-run pin above protects each
script on its own; to hold one binary across an entire re-pin session — all
four probes plus the stream-json golden capture, so the doc stamp, both
fixture families, and every probe provably name one version — pin it
yourself before starting. Use the drift-window shim from §Reproducing when
the session includes the golden capture (claude-print resolves `claude`
through PATH, so only a PATH pin reaches it):

```bash
mkdir -p /tmp/claude-pin-<v> && ln -sf ~/.local/share/claude/versions/<v> /tmp/claude-pin-<v>/claude
export PATH="/tmp/claude-pin-<v>:$PATH"
```

For the four guarded probes alone, pointing their override at the concrete
binary is equivalent (the guard resolves it to the same pinned file):

```bash
export CLAUDE_BIN="$HOME/.local/share/claude/versions/<v>"
```

Every guarded script in the session then stamps that binary and its verdict
lines, and a host auto-update mid-session cannot reach any of them. The
guard's contract — pin resolution, the bracket's verdict lines and exit
codes, the launcher-repoint immunity, the gate's own bracket, and the
fixture's `version_guard` record — is pinned by `tests/probe_version_guard.rs`.

## Maintenance: re-running after a Claude Code update

The evidence above is pinned to one claude version and carries to a new one
only by re-measurement. The maintenance step has four parts: detect, re-run,
re-pin, and (only if a contract moved) file follow-ups. This section is the
definition of that step, and since 2026-09-24 it has an executable owner:
`scripts/contract-maintenance-gate.sh` performs all four parts (detect →
re-run → evidence → follow-up) and is invoked by CI on every push — see
**Wiring** below — while its detection step additionally runs on a daily
dev-host timer independent of pushes (**Scheduled watch** below). Before
that, the doc-level instruction "re-run them after
any Claude Code update" was unowned and unscheduled.

**Detect.** `bash scripts/check-claude-version-bump.sh` compares the live
`claude --version` against the **Measured against:** stamp at the top of this
file: exit 0 = current, exit 1 = drift (re-run due), exit 2 = cannot
determine. It runs `claude --version` only — no sandbox, no model turns — so
it is safe to run on a schedule or from CI, where exit 1 is the R-2 signal —
and, since claudepr-3094ab2e, a **build-failing gate** (see **Wiring**).
It also checks the active references for both version-pinned fixture classes,
`tests/fixtures/claude_contracts_v*.json` and
`tests/fixtures/stream_json_golden_v*`'s
`.{input,expected,errors}.jsonl` family; historical fixture files that are no
longer selected by a contract test do not hold the gate back. And it rejects
divergent active pins outright (claudepr-b590e46d): the doc stamp and both
active families must agree with each other before anything is compared
against the installed binary — a half-landed re-pin fails the gate (exit 2)
even where claude is absent, and `tests/contract_maintenance.rs`'s
`active_fixture_families_share_one_pinned_version` pins the same invariant
into every `cargo test` run. A version bump therefore stays red until the
runtime evidence and the stream-json goldens are re-measured and their
active references are re-pinned together.
Versions move in two ways; either should trigger the check:

- the dev host auto-updates the native install
  (`~/.local/share/claude/versions/`, repointing `~/.local/bin/claude`) —
  a manual upgrade lands the same way;
- CI records the version it saw into `target/last-claude-version.txt` via
  `test_claude_version_recorded` (`tests/version_compat.rs`), and the release
  WorkflowTemplate uploads it as a release asset — diffing consecutive
  artifacts is the fleet-visible drift signal (claudepr-777d3056). The gate
  refreshes the same file on every run, in the same full-line format
  `test_claude_version_recorded` writes, so the release asset stays real and
  consistently formatted even when cargo did not run first.

**Wiring.** The `claude-print-ci` WorkflowTemplate (this repo's
`claude-print-ci-workflowtemplate.yml`; the live copy is applied through
`declarative-config`) runs the gate on every push, in verify-only and release
mode alike: it installs the claude binary first (native installer —
`claude --version` needs no auth) so detection compares the *installed*
version instead of recording `unknown`, then invokes
`scripts/contract-maintenance-gate.sh --file-follow-up`. On drift the gate
re-anchors `target/last-claude-version.txt`, files or updates a GitHub
follow-up issue (idempotent per installed version — searched by a
`claude-contract-drift live=<version>` marker before create), and leaves the
evidence bundle under `target/contract-maintenance/` (`detection.txt`,
`live-contract-tests.txt`, `probes/*.txt`, `contract-status.txt`,
`next-steps.txt`; `live-contract-tests.txt` exists only when the tests
actually ran). The `contract-status.txt` `alert:` line — `none`,
`re-run-due`, or `indeterminate` — mirrors the gate's exit code, so a
release-notes stamp can explain a non-zero gate on its own; the release
path stamps `contract-status.txt` into the
release notes and uploads the refreshed version file as the release asset.
Drift exits the gate non-zero and, since claudepr-3094ab2e, that **fails the
build**: the gate runs as the workflow's FIRST quality gate (before fmt), and
its exit is fatal — exit 1 (drift) or 2 (cannot determine; fails closed)
both go red. A Claude version change therefore *requires* rerunning the
probes, recording updated evidence, and completing the re-pin (or, if a
contract moved, the follow-up beads plus the code/doc changes they demand)
before CI passes again; the re-pin commit itself is what turns the gate
green. This is clearable even though the full probes need model-turn auth CI
does not have, because the probes and the re-pin run host-side (authed dev
host) — CI only enforces that the pinned evidence covers the version it
sees. The follow-up issue survives the red (the gate files it before
exiting), so the hand-off channel is unchanged. Note that CI installs the
*latest* stable claude while the dev host auto-updates on its own schedule —
the two can drift independently, and each is a valid drift signal against
the same pinned stamp. The `tests/contract_maintenance.rs` suite pins this
wiring (template fragments including the fatal wrapper, gate exit-code
contract against a stubbed claude, and the doc/plan mentions) so the
automation cannot silently detach from this page again.

**Scheduled watch.** CI fires only on a push, and Claude Code auto-updates
on its own schedule — the 2026-09-24 re-pin was itself superseded hours
later by the host auto-updater (a 2.1.281 pin, 2.1.282 installed the same
day) with nothing pushed in between. Between pushes, then, the
**Measured against:** stamp and the active fixture pins can be stale with
nothing red. Since 2026-09-25 (claudepr-e6e54313) the detection step
therefore also runs on a schedule independent of repo activity:
`claude-print-contract-drift-watch.timer`, a systemd user timer on the dev
host (`OnCalendar=daily`, `Persistent=true`, so a missed window fires after
the next boot), runs `scripts/contract-drift-watch.sh` — the
credential-free detector and nothing heavier. On drift (detector exit 1)
the watcher files exactly one bead in this repo's bead workspace via
`bead create --unique-ref claude-contract-drift:live-<version>` (the CLI's
atomic idempotent create: daily repeats while the drift persists return
`EXISTING <id>` instead of duplicating), carrying the same
`claude-contract-drift live=<version>` marker as the gate's gh issue — the
hand-off lands in the same queue the re-pin work is dispatched from. Exit 2
(cannot determine) files nothing and fails the unit loudly. Install or
refresh it (after editing the watcher) with
`./scripts/install-contract-drift-watch.sh`: the watcher goes to
`~/.local/libexec/claude-print/`, the units to `~/.config/systemd/user/`,
and the service pins `CLAUDE_PRINT_CONTRACT_REPO=%h/claude-print` — the
shared checkout whose pins the detector reads and whose `.beads/` the
follow-up lands in (the detector itself always runs from that checkout, so
detection logic never goes stale; only the watcher is installed). Result
line: `~/.local/state/claude-print/contract-drift-watch/last-result`
(PASS/DRIFT/INDETERMINATE, the billing-canary state shape); logs:
`journalctl --user -u claude-print-contract-drift-watch.service`.
`tests/contract_drift_watch.rs` and `tests/install_contract_drift_watch.rs`
pin the watcher's exit/filing contract and the installer, so the schedule
cannot silently detach from this page either. The operator-facing sequence
for this timer and the billing canary — prerequisites, installation,
status/log inspection, restart/disable, removal, and failure behavior — is
`docs/notes/scheduled-services-runbook.md`.

**Re-run.** No drift → nothing to do. The cheap live tests re-verify the
merge and suppression contracts against the installed binary in ~35 s
(`cargo test --test claude_contracts -- --ignored`) and are the fastest way
to confirm "no drift within a version" (last verified 2026-09-25 against
2.1.282: 2/2 passed; previously 2026-09-24 against 2.1.282 and 2026-09-14
against 2.1.270: 2/2 passed each). On
drift, run all four scripts from §Reproducing —
each is self-contained (sandboxed mktemp `HOME`, scrubbed `CLAUDECODE*` env,
trust pre-seeded for the probe cwd only, auth by inherited environment) and
stamps the version it measured. Budget 1–6 min each (`probe-stop-edge-contracts.sh`
runs 15 model turns across its three arms — budget up to ~20 min). Note that
`probe-tui-second-turn.sh` can legitimately fail to produce a second
completed reply (an incomplete turn firing no Stop *is* the contract) — an
incomplete second turn is a re-run, not a contract finding; only
"reply rendered + no Stop" would be. Likewise an Arm D run that exits 1 with
zero Stops was cut off by `--max-turns` (the measured cutoff contract) — a
re-run, not a finding; only "completed run (exit 0) with ≠1 Stop per loaded
source" would be. And an Arm T run that exits non-zero or logs no firing for
an event (model-turn failure, `timeout` wrapper) is a re-run — the findings
are "hook `end` logged past its timeout" (timeout not enforced) or "session
blocked or failed after the kill" (enforcement is not clean). And every probe
run ends with its version-guard verdict (§Version guard): `single-version`,
or a run aborted as STRADDLED (exit 1) — a straddled run is itself a re-run;
discard its evidence entirely and do not cherry-pick from it.

**Re-pin** (all contracts unchanged): re-stamp **Measured against:** at the
top of this file; copy `tests/fixtures/claude_contracts_v<old>.json` to
`claude_contracts_v<new>.json`, updating `claude_version`/`measured_at`, and
record the guard verdict beside the write in the fixture's `version_guard`
object — `start`/`end` taken from the probes'
`version-guard: verdict=single-version` lines, all four probes agreeing with
`start == end == claude_version` (the shape is enforced always-on by
`tests/probe_version_guard.rs`; the 2.1.282 pin carries a `retroactive`
provenance note instead, having been measured under the 2026-09-24
full-re-run discipline before the guard existed); then
repoint `FIXTURE` in `tests/claude_contracts.rs` together with its header
comment and any version-citing assertion messages. Commit doc + fixture +
same version. Re-measure the stream-json capture path — **in the same pinned
session as the probes** (§Version guard, "pinning a whole re-pin session",
so the goldens provably come from the same single version) — and regenerate the
complete `tests/fixtures/stream_json_golden_v<new>.{input,expected,errors}.jsonl`
family, then update all three `include_str!` references, the header comment,
and the stamped `GOLDEN_CLAUDE_VERSION` in `tests/stream_json_contract.rs`.
Do not hand-edit golden bytes or merely rename their files. Commit the doc,
both active fixture families, and their test references in one change so the
always-on suites and this document keep claiming the same version — that
commit is also what turns the CI drift gate green again
(§Wiring above; it is how the 2.1.270 → 2.1.282 re-pin of 2026-09-24 was
landed, via a 2.1.281 pin the host's auto-updater superseded the same day).
Since claudepr-9fe76ef4 the gate additionally brackets itself against a
mid-run update (§Version guard): `version-stability: straddled` in
contract-status.txt means the verdict was voided and the gate exited 2
(fail closed) even where detection alone had said CURRENT — re-run it.

**File follow-ups** (a contract moved): one bead per moved contract,
naming the downstream design that depends on it, and update this document and
the fixture to the new measured truth in the same change, citing sanitized
evidence only (timestamps, counts, tags — never payloads):

| Moved contract | Design that depends on it |
|---|---|
| `--settings` merge (PO-1/OQ-1) | relay wiring in `hook-design.md`; plan hook sections |
| cross-source firing order becoming contractual (OQ-1) | `hook-design.md` read-race note (currently: not contractual, do not order away) |
| `--setting-sources=` suppression (PO-2/OQ-2) | `--no-inherit-hooks` mode (plan) |
| once-per-turn Stop | Stop Poller single-fire design (plan §Stop Poller) |
| `--max-turns` cutoff fires no Stop | watchdog ownership of cutoff cases (plan) |
| per-hook `timeout` no longer enforced (overrun hook not killed, or the session blocked past the kill) | relay `"timeout": 10` bound in `hook-design.md` §Relay Hook; `--stop-hook-timeout` watchdog interaction |
