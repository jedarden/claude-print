# bf-fdf3 — README/plan docs accuracy (2026-07-21)

**Result: all three bugs already fixed and on `origin/main` (commit `1a531ce`).
Re-verified every acceptance criterion against the live source this session —
all four match exactly. No doc edits were needed.**

The prior session committed the docs-only fixes for this bead but did not close
it; this session confirms correctness and closes it.

## Verification against source

| Criterion (acceptance) | Source of truth | Current doc state | Verdict |
|---|---|---|---|
| json format lists the real emitter fields (result, not text; drop model; add type/subtype/is_error/num_turns/duration_ms/cost_usd/claude_version) | `src/emitter.rs` L49–65 emits `type, subtype, is_error, result, session_id, num_turns, duration_ms, cost_usd, claude_version, usage` (no `model`/`text`) | README.md L115 lists exactly those fields; explicitly notes "`result` holds the response text (there is no `text` or `model` field)"; example at L77 uses `jq .result` | ✅ |
| Exit Codes table gains a `4` row for missing-prompt / unreadable `--input-file` | `src/main.rs` L85/97/103/111 all call `exit_with_cleanup(4)` | README.md L125: `` `4` \| Input error (no prompt provided, or `--input-file`/stdin unreadable) `` | ✅ |
| plan.md §1 Exit codes list + Error Handling table include code 4; EC-12 reconciled to exit 4 | `src/main.rs` prompt-resolution path (above) | plan.md L466 (§1 list), L881 (Error Handling table row → exit 4), L911 (EC-12 corrected: "exit 4 … previously said 'exit 2'") | ✅ |
| Architectures corrected to x86_64-only (matches plan Non-Goals + CI capability) | plan.md L39 Non-Goals excludes "macOS / ARM Linux"; CI builds `$(uname -m)` of the x86_64 runner only | README.md L41: "Architectures: `x86_64` only … aarch64 / ARM Linux is out of scope for v1.0 … an `install.sh` aarch64 branch would 404" | ✅ |

## Stale-reference sweep (none found)

- README: only `aarch64` mention is the corrected L41; only `jq` example is `jq .result` (L77).
  No `jq .text`, no wrong exit codes, no `x86_64 and aarch64` pairing remains.
- plan.md: the remaining `exit 2` references are all legitimate exit-2 conditions (binary
  not found, PTY/hook failures, child-exit-before-Stop, WAITING 45 s timeout, etc.) — none
  describe the no-prompt case. EC-12 is the only stdin/no-prompt row and it now says exit 4.

## Source behavior unchanged

Docs-only. No `src/` edits in this bead or in `1a531ce`. `src/main.rs` continues to emit
exit 4 for every prompt-resolution failure; EC-12's prose was brought into agreement with
that behavior rather than the reverse.

## Commit

`1a531ce` (on `origin/main`) contains the README.md + docs/plan/plan.md edits. This notes
file is the single additional commit for this session, staged by explicit path — the
unrelated dirty `.beads/`-tree changes were not swept in.
