# bf-5hnu — Update Emitter/stream-json section and fix --version text in plan.md

## Status: COMPLETE — both edits already landed under bf-4q2 (commit ce0a13b)

## Summary

bf-5hnu asks to (1) note in the "Components #9 Emitter / stream-json" section of
`docs/plan/plan.md` that the current implementation is post-session replay with
live tailing tracked as open work item **bf-5vm**, and (2) make the Phase 1
`--version` text version-agnostic instead of hardcoding `0.1.0`. Both are a
strict subset of the plan-refresh done by **bf-4q2** (commit `ce0a13b`,
"docs(bf-4q2): update plan.md - stale Status table, module layout, and
undocumented watchdog/flags"), whose items #4 and #5 were literally:

> 4. 'Components #9 Emitter' / stream-json: note current implementation is
>    post-session replay (src/main.rs replay_stream_json) with live tailing
>    tracked as an open work item (bead bf-5vm), so the plan reflects reality
>    until that lands.
> 5. Phase 1 --version text says 0.1.0 — make version-agnostic.

(Same lineage as the sibling bead `bf-3mfy`, already closed the same way.)

Both deliverables are therefore already present in the committed `plan.md`. This
bead required no further edits to `plan.md`; this note records the verification
pass against the acceptance criteria.

## Acceptance criteria — verification

| Criterion | Where in `docs/plan/plan.md` | Status |
|-----------|------------------------------|--------|
| stream-json section notes replay-only implementation | L735 — "`stream-json`**: Current implementation (v0.2.0) is post-session replay — after Stop fires and the child exits, the emitter reads the complete transcript from the beginning and forwards all JSONL events to stdout … does not stream events in real-time." | ✅ |
| stream-json section references bf-5vm | L735 — "Live tailing (real-time forwarding as Claude Code writes events) is tracked as an open work item (bead bf-5vm)." | ✅ |
| `--version` text no longer hardcodes `0.1.0` | L966 — "`--version` prints `claude-print <VERSION> (wrapping claude <claude-version>)` where <VERSION> is from Cargo.toml and <claude-version> is resolved at runtime" | ✅ |

Confirming `0.1.0` is gone entirely: `grep -n "0.1.0" docs/plan/plan.md` returns
no matches. (The only version literal near `--version` is the `X.Y.Z` placeholder
in the version-compat test assertion at L1102, which is intentionally generic.)

## What `ce0a13b` changed (the bf-4q2 commit that did this work)

```
-**`stream-json`**: Spawns a reader thread that tails the transcript JSONL from
-the byte offset captured at prompt injection time, forwarding each new raw event
-line to stdout as it is written by Claude Code. After Stop fires, drains
-remaining lines. ...
+**`stream-json`**: Current implementation (v0.2.0) is post-session replay —
-after Stop fires and the child exits, the emitter reads the complete transcript
-from the beginning ... Live tailing ... is tracked as an open work item
+(bead bf-5vm). ...

-- [x] `--version` prints `claude-print 0.1.0 (wrapping claude X.Y.Z)`
+- [x] `--version` prints `claude-print <VERSION> (wrapping claude <claude-version>)`
+where <VERSION> is from Cargo.toml and <claude-version> is resolved at runtime
```

## Cross-check vs. source of truth (out-of-scope note for bf-5vm)

The bf-5hnu/bf-5vm task bodies reference a function `src/main.rs replay_stream_json`.
No such function exists in the codebase (`grep -rn replay_stream_json src/` → no
matches). The actual stream-json emission path is `emitter::spawn_stream_json_reader`
(`src/emitter.rs:98`), wired into the production session flow at
`src/session.rs:363-374` (spawned at the `PROMPT_INJECTED` transition with the
injection-time byte offset). That reader continuously tails the transcript
(`src/emitter.rs:118-195`, 5 ms poll loop) and forwards each new line to stdout
in real time — i.e. the code already behaves as live tailing, not post-session
replay.

Reconciling the "post-session replay" wording in `plan.md` L735 with the actual
reader-thread implementation is **bf-5vm's** scope ("Wire live stream-json
streaming into session flow"), not bf-5hnu's. bf-5hnu's acceptance criteria are
satisfied by the existing L735 / L966 text as written, so no `plan.md` edit was
made here.

## Action taken

No file changes to `plan.md` — content already committed via `ce0a13b`.
This note is the commit artifact required to close bf-5hnu.
