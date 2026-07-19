# bf-3mfy — Document watchdog component and timeout flags in plan.md

## Status: COMPLETE — scope already landed under bf-4q2 (commit ce0a13b)

## Summary

bf-3mfy asks to (1) add a Components subsection for `src/watchdog.rs` and
(2) add three CLI flag rows (`--first-output-timeout`, `--stream-json-timeout`,
`--stop-hook-timeout`) to `docs/plan/plan.md`. This is a strict subset of the
plan-refresh done by **bf-4q2** (commit `ce0a13b`, "docs(bf-4q2): update
plan.md - stale Status table, module layout, and undocumented watchdog/flags"),
whose item #3 was literally:

> Undocumented component: src/watchdog.rs ... and the CLI flags
> --first-output-timeout, --stream-json-timeout, --stop-hook-timeout (src/cli.rs)
> appear nowhere in the plan. Add a short component subsection + rows in the CLI table.

Both deliverables are therefore already present in the committed `plan.md`. This
bead required no further edits to `plan.md`; this note records the verification
pass against the acceptance criteria.

## Acceptance criteria — verification

| Criterion | Where in `docs/plan/plan.md` | Status |
|-----------|------------------------------|--------|
| watchdog.rs functionality described — no-output timeout detection | `### 10. Watchdog` table rows "PTY first-output timeout" (L764) and "Stream-json first-output timeout" (L765) | ✅ |
| watchdog.rs functionality described — max-turn / overall timeout enforcement | `### 10. Watchdog` table row "Overall timeout" 3600 s, "session exceeded overall max-turn deadline (applies throughout entire session)" (L766) | ✅ |
| watchdog.rs functionality described — stream-json first-output monitoring | `### 10. Watchdog` table row "Stream-json first-output timeout" (L765); also the stream-json monitor thread (`spawn_stream_json_monitor_in_dir`) over `<temp_dir>/transcript.jsonl` in `src/watchdog.rs` | ✅ |
| `--first-output-timeout` in CLI table | L446 — "PTY first-output timeout in seconds (default: 90)..." | ✅ |
| `--stream-json-timeout` in CLI table | L447 — "Stream-json first-output timeout in seconds (default: 90)..." | ✅ |
| `--stop-hook-timeout` in CLI table | L448 — "Stop hook watchdog timeout in seconds (default: 120)..." | ✅ |

All three watchdog aspects from the task description (no-output detection,
max-turn/overall enforcement, stream-json first-output monitoring) are described
in `### 10. Watchdog`, and all three flags carry descriptions in the CLI table.

## Cross-check vs. source of truth (`src/watchdog.rs`)

Defaults in `plan.md` match `src/watchdog.rs` constants:
- `DEFAULT_PTY_TIMEOUT_SECS = 90` ↔ `--first-output-timeout` default 90 ✅
- `DEFAULT_STREAM_JSON_TIMEOUT_SECS = 90` ↔ `--stream-json-timeout` default 90 ✅
- `DEFAULT_OVERALL_TIMEOUT_SECS = 3600` ↔ Overall timeout 3600 s ✅
- `DEFAULT_STOP_HOOK_TIMEOUT_SECS = 120` ↔ `--stop-hook-timeout` default 120 ✅

`TimeoutType` enum variants (PtyFirstOutput / StreamJsonFirstOutput /
OverallTimeout / StopHookTimeout) map 1:1 to the four table rows and their
`subtype()` strings (`pty_first_output_timeout`,
`stream_json_first_output_timeout`, `overall_timeout`, `stop_hook_timeout`).

## Action taken

No file changes to `plan.md` — content already committed via `ce0a13b`.
This note is the commit artifact required to close bf-3mfy.
