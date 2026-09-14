# Claude Code Runtime Contract Probes — measured behavior

**Measured against:** `claude` 2.1.270 (`2.1.270 (Claude Code)`), 2026-09-13, on the
claude-print dev host. Probe harness: `scripts/probe-claude-contracts.sh` (merge /
suppression / single-turn Stop), `scripts/probe-stop-toolallowed.sh` (multi-round
Stop contract, print mode), and `scripts/probe-tui-second-turn.sh` (TUI
once-per-turn contract). Re-run them after any Claude Code update; results are
version-pinned — the maintenance workflow (drift detection, re-run procedure,
re-pin checklist, follow-up rule) is defined in §Maintenance at the bottom of
this file, with `scripts/check-claude-version-bump.sh` as its detection step.
Resolves plan PO-1, PO-2, OQ-1, OQ-2 and the Stop-poller
assumption (plan §7 Stop Poller and the glossary Stop-hook note).

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
`hooks.Stop[ { hooks: [ {type: "command", ...} ] } ]` form — accepted and fired
by 2.1.270, which also live-verifies the Hook Installer §2 schema note.

## PO-1 / OQ-1 — `--settings` merge: CONFIRMED; firing order: NOT CONTRACTUAL

Evidence (P2: project source + `--settings`; P6: user source + `--settings`;
each a real completed `claude -p` turn):

| Run | Event | Firings in order |
|---|---|---|
| P2 | SessionStart | project `…287.350` → relay `…287.351` |
| P2 | Stop | project `…298.869` → relay `…298.872` |
| P6 | SessionStart | user `…325.235` → relay `…325.237` |
| P6 | Stop | user `…327.772` → relay `…327.774` |

- `--settings <file>` hooks fire **alongside** standard-source hooks — merge,
  not replace. Every loaded source fired on every event. PO-1's
  in-process-merge fallback is not needed.
- **Order across sources is not a contract.** In the four probe pairs above
  the standard-source hook started first (2–3 ms ahead), which initially read
  as "relay fires last" — but a dedicated timestamp probe (project hook
  sleeping 300 ms and logging start/end; relay hook sleeping 0 ms) showed the
  relay **starting before** the project hook in 1 of 4 runs, with the two
  hooks running **concurrently** (project `start 307.920 / end 308.225`, relay
  `start 307.914`). The typical pattern is sequential standard-source-first;
  the guaranteed property is only that **all loaded sources fire**.
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
  The PO-2 fallback spelling is not supported by 2.1.270; do not adopt it.
  (Side observation: `--allowedTools` consumes a variadic value list — pass it
  as `--allowedTools=Bash` or it will swallow a following positional prompt.)

## Stop firing contract — single-turn baseline

Every completed single-turn `claude -p` run (P1, P2, P3, P4, P6: no tool use)
fired **exactly one Stop per loaded source** (e.g. P6: user Stop + relay Stop,
2 ms apart; P3: relay Stop only). No run produced zero Stops; no run produced
more than one Stop per source.

## Stop firing contract — multi-round tool use, TUI turns, cutoffs

The first-round probes (T1/T2 in `probe-claude-contracts.sh`) ran without a
tool allowlist, so the model's Bash calls were permission-denied and their
Stop counts belong to degraded runs — one of the two degraded runs produced an
**extra** Stop firing (2 for one prompt). Those counts were discarded and the
contract was re-measured with tools actually permitted
(`scripts/probe-stop-toolallowed.sh`, `scripts/probe-tui-second-turn.sh`):

| Scenario | Stop firings | Evidence |
|---|---|---|
| Single-turn `claude -p`, no tools | 1 per loaded source | P1–P6 (above) |
| `claude -p`, two sequential permitted Bash rounds (`--allowedTools=Bash`) | **1**, at the turn's true end | one firing, `last_assistant_message: "DONE"`, `stop_hook_active: false`, single session id |
| TUI, turn 1 (plain reply) | **1** | firing + reply both observed (22.6 s) |
| TUI, turn 2 in the same session (plain reply) | **1** | firing + reply both observed (15.1 s); TUI status line showed `(running stop hook)` |
| `claude -p` cut off by `--max-turns 2` mid-task | **0** | exit code 1, zero firings across the run |

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
  firings for one prompt). Mechanism unexplained; the single-fire poller
  degrades gracefully (first payload → transcript retry/fallback path; the
  watchdog still bounds the session). Re-measure if permission-denied paths
  ever become a supported mode.

(filled in by the claudepr-6ef2541c probe run — see plan.md §Stop Poller for
the concluded contract)

## Reproducing

```bash
bash scripts/probe-claude-contracts.sh        # merge/suppression/single-turn Stop
bash scripts/probe-stop-toolallowed.sh        # multi-round Stop contract (print mode)
bash scripts/probe-tui-second-turn.sh         # TUI once-per-turn contract
```

Each run is self-contained (sandboxed HOME, scrubbed `CLAUDECODE*` env, forced
`CLAUDE_CODE_ENTRYPOINT=cli` + `CLAUDE_CODE_FORCE_SESSION_PERSISTENCE=1`,
mirroring `src/pty.rs`'s child-environment contract) and prints its version
stamp. Runtime ≈ 1–6 minutes each (model turns via the configured provider).
`tests/claude_contracts.rs` pins the fixture and re-verifies the cheap
contracts live under `cargo test --test claude_contracts -- --ignored`.

Two probe-authoring notes for whoever reruns these: `--allowedTools` takes a
variadic value list, so pass `--allowedTools=Bash` (equals form) or it will
swallow a following positional prompt; and a hook log of the form
`timestamp|payload` must be counted with a payload-substring filter, not a
field-count check (a filter written for the 4-field `ts|tag|event|payload`
layout silently counts zero on the 2-field layout, which invalidated one TUI
driver run before this was caught).

## Maintenance: re-running after a Claude Code update

The evidence above is pinned to one claude version and carries to a new one
only by re-measurement. The maintenance step has four parts: detect, re-run,
re-pin, and (only if a contract moved) file follow-ups. This section is the
definition of that step — the doc-level instruction "re-run them after any
Claude Code update" is otherwise unowned and unscheduled.

**Detect.** `bash scripts/check-claude-version-bump.sh` compares the live
`claude --version` against the **Measured against:** stamp at the top of this
file: exit 0 = current, exit 1 = drift (re-run due), exit 2 = cannot
determine. It runs `claude --version` only — no sandbox, no model turns — so
it is safe to run on a schedule or from CI, where exit 1 is the R-2 "CI alert
on version change" signal. Versions move in two ways; either should trigger
the check:

- the dev host auto-updates the native install
  (`~/.local/share/claude/versions/`, repointing `~/.local/bin/claude`) —
  a manual upgrade lands the same way;
- CI records the version it saw into `target/last-claude-version.txt` via
  `test_claude_version_recorded` (`tests/version_compat.rs`), and the release
  WorkflowTemplate uploads it as a release asset — diffing consecutive
  artifacts is the fleet-visible drift signal (claudepr-777d3056).

**Re-run.** No drift → nothing to do. The cheap live tests re-verify the
merge and suppression contracts against the installed binary in ~35 s
(`cargo test --test claude_contracts -- --ignored`) and are the fastest way
to confirm "no drift within a version" (last verified 2026-09-14 against
2.1.270: 2/2 passed). On drift, run all three scripts from §Reproducing —
each is self-contained (sandboxed mktemp `HOME`, scrubbed `CLAUDECODE*` env,
trust pre-seeded for the probe cwd only, auth by inherited environment) and
stamps the version it measured. Budget 1–6 min each. Note that
`probe-tui-second-turn.sh` can legitimately fail to produce a second
completed reply (an incomplete turn firing no Stop *is* the contract) — an
incomplete second turn is a re-run, not a contract finding; only
"reply rendered + no Stop" would be.

**Re-pin** (all contracts unchanged): re-stamp **Measured against:** at the
top of this file; copy `tests/fixtures/claude_contracts_v<old>.json` to
`claude_contracts_v<new>.json`, updating `claude_version`/`measured_at`; and
repoint `FIXTURE` in `tests/claude_contracts.rs` together with its header
comment and any version-citing assertion messages. Commit doc + fixture +
test in one change so the always-on suite and this document keep claiming the
same version.

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
