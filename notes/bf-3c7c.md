# bf-3c7c — Live stream-json reader tails the real transcript path (re-dispatch verify)

## Outcome
Re-dispatched for verification (failure-count had reached 2). The fix was already
complete and committed on `main`; this pass re-ran every acceptance criterion and
confirms it. No source changes were needed — the implementation is correct and all
tests pass.

## The bug (recap)
The stream-json reader was spawned against a placeholder path
(`<temp>/transcript.jsonl`) that neither real claude nor mock_claude ever writes —
the transcript lands at `~/.claude/projects/<cwd-slug>/<session_id>.jsonl`. So the
reader's open-retry loop timed out after 5s and forwarded NOTHING, leaving the
feature inert despite the wiring shipping. The hard part: the `session_id` (and
thus the exact filename) only surfaces in the Stop payload, which arrives AFTER
`PROMPT_INJECTED`, where the reader spawns — so it cannot be handed a final path
at spawn time.

## Prior commit that did the core work
- `93e2f90 fix(bf-3c7c): live stream-json reader tails the real transcript path`
  — At `PROMPT_INJECTED`, derives the projects dir from cwd
  (`poller::projects_dir_for_cwd` → `$HOME/.claude/projects/<cwd-slug>/`), snapshots
  every `.jsonl`'s size there (`emitter::snapshot_jsonl_sizes`), and spawns a
  **discover** reader (`spawn_stream_json_reader_discover`) that scans the dir for
  the newest `.jsonl` new or grown since the snapshot, reusing the existing
  50ms/5s open-retry loop until it appears. The snapshot lets an ongoing session's
  pre-injection bytes be skipped (the kept `start_offset`). This preserves live
  pre-Stop tailing — the entire point of bf-5vm — rather than forfeiting it by
  re-spawning only after Stop.

  Files touched: `src/poller.rs` (`projects_dir_for_cwd`), `src/emitter.rs`
  (`snapshot_jsonl_sizes`, `spawn_stream_json_reader_discover[_to]`,
  `TranscriptSource::Discover`, `discover_with_retry`, `discover_session_jsonl`),
  `src/session.rs` (snapshot + spawn at `PROMPT_INJECTED`; drop the bogus
  `<temp>/transcript.jsonl` path; warn-and-skip if the projects dir can't be
  derived), and `tests/integration/scenarios.rs` (new acceptance test).

## Acceptance-criteria verification (all PASS)
1. **Integration test proving the reader tails the path mock_claude writes,
    forwarding lines as the file grows** — PASS.
   `stream_json_reader_discovers_and_tails_projects_dir_jsonl`
   (`tests/integration/scenarios.rs`) extends the existing non-ignored incremental
   reader test to a projects-dir-style layout matching mock_claude's real write
   location (`~/.claude/projects/mock-cwd/<id>.jsonl`). It pins:
     - a NEW session file created AFTER spawn (the `PROMPT_INJECTED` condition,
       `session_id` unknown) is discovered at runtime and tailed line-by-line,
     - lines are forwarded INCREMENTALLY as the file grows (both the first event
       and an appended second event),
     - a STALE pre-existing session (present in the injection snapshot, not grown)
       is excluded — the snapshot's `start_offset` semantics.
   Ran explicitly: 2 passed (incremental + discover), 0 failed.
2. **No regression: the 120 lib tests still pass** — PASS.
   `cargo test --lib` → `test result: ok. 120 passed; 0 failed`.
3. **Full integration target** — PASS.
   `cargo test --test integration` → `test result: ok. 30 passed; 0 failed`.

## Notes
- No source changes this pass; only this verification note. The committed
  implementation already satisfies every acceptance criterion.
- The end-to-end gate `tests/stream_json_incremental.rs` remains intentionally
  `#[ignore]`'d — it is the cross-bead contract blocked on bf-3isy (mock_claude
  writes the transcript JSONL) and bf-5vm, and is out of scope for this child.
  bf-3c7c's own acceptance is pinned by the non-ignored
  `stream_json_reader_discovers_and_tails_projects_dir_jsonl` primitive above.
