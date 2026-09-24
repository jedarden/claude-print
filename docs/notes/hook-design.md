# Hook Design

Two relay hooks are the IPC mechanism between Claude Code and `claude-print`. When the AI completes a turn, Claude Code fires the **Stop** hook with a JSON payload containing session metadata; `claude-print` reads this from a FIFO to know when the response is ready. When the prompt is submitted, Claude Code fires the **UserPromptSubmit** hook; `claude-print` relays that payload into a per-drive identity file so the stream-json reader can bind to this session's transcript before any assistant event exists (see [UserPromptSubmit Identity Relay](#userpromptsubmit-identity-relay)).

## Temp Directory Structure

Each `claude-print` invocation creates a per-run temp directory:

```
<TMPDIR>/claude-print-<pid>-<rand>/
├── settings.json         # Relay hook configuration (Stop + UserPromptSubmit)
├── hook.sh               # Stop relay script executed by Claude Code
├── identity.sh           # UserPromptSubmit relay script executed by Claude Code
├── stop.fifo             # Named pipe carrying the Stop payload
└── session-identity.json # Written at runtime by identity.sh (not created by the installer)
```

The temp dir is created with mode `0700` (owner-only) to prevent local users from reading the Stop payload (session ID, prompt text). Both modes are pinned verbatim, not just requested: the dir is created `0700` up front (the umask can only tighten it) and then `chmod`'d again, and `stop.fifo` is created `0600` and `chmod`'d again, so a restrictive umask can never land the FIFO below `0600` and break the hook's write (`HookInstaller::new`; pinned including under a hostile umask 000 by `src/hook.rs::artifact_modes_hold_under_hostile_umask` and `::relay_artifact_modes_survive_usage`).

The owning pid embedded in the directory name (`claude-print-<pid>-<rand>`) is load-bearing: the startup orphan sweep uses it to prove a stale directory's owner is dead before reclaiming it (see [Cleanup](#cleanup)). `session-identity.json` sits beside `stop.fifo` by contract, not coincidence — pool clients reconstruct its path from the Stop FIFO path alone (`stop_fifo().with_file_name(SESSION_IDENTITY_FILE)`), so the sibling layout is part of the daemon/client contract (`src/hook.rs::identity_path_is_stop_fifo_sibling`).

## Relay Hook

The two relay scripts share one body — `cat > '<target>' 2>/dev/null || true` — written by the same generator (`write_cat_script`), executable `0750`. The Stop relay writes the Stop payload to the FIFO:

```sh
#!/bin/sh
cat > '<fifo-path>' 2>/dev/null || true
```

The identity relay (`identity.sh`) is the same script with `<identity-path>` in place of `<fifo-path>`.

Key points:
- The target path is embedded as a shell single-quoted string — no variable expansion at execution time; a single quote in the temp-dir path is escaped `'\''`, so metacharacters in the path can never become shell syntax (bf-5sj7, pinned by `src/hook.rs::hook_sh_escaping_handles_shell_metacharacters` and `::identity_sh_is_executable_and_targets_identity_file`)
- If the write fails, the hook exits cleanly (`|| true`); Claude Code does not wait beyond the 10s timeout
- The hooks are executed by Claude Code via `--settings <temp>/settings.json`

## settings.json

The per-run settings file contains the two relay hooks:

```json
{
  "hooks": {
    "Stop": [{
      "hooks": [{"type": "command", "command": "<temp>/hook.sh", "timeout": 10}]
    }],
    "UserPromptSubmit": [{
      "hooks": [{"type": "command", "command": "<temp>/identity.sh", "timeout": 10}]
    }]
  }
}
```

Claude Code merges this with any user hooks from `~/.claude/settings.json`, so both user hooks and the relay hooks fire. **Measured on claude 2.1.270 (2026-09-13, `docs/notes/claude-contract-probes.md`):** merge confirmed for user- and project-source hooks alongside the relay hook — every loaded source fires on each hook event. Cross-source firing *order* is not contractual: standard-source hooks typically start first (1–3 ms ahead), but concurrent firing — and a run where the relay started before the project hook — was also observed. Nothing in claude-print may depend on the relay firing last; the payload may arrive while user Stop hooks are still running, which is safe because the two consumers are independent. The merge semantics apply per event — the UserPromptSubmit relay merges with any user UserPromptSubmit hooks exactly as the Stop relay does (PO-1).

## FIFO Protocol

The FIFO is a POSIX named pipe created with `mkfifo(path, mode=0600)`:

- **Writer**: Claude Code executes `hook.sh`, which opens the FIFO for writing and writes the JSON payload
- **Reader**: `claude-print` opens the FIFO for reading and blocks until data is available

### Stop Hook Payload

All fields are optional for forward compatibility:

```json
{
  "hook_event_name": "Stop",
  "session_id": "abc123",
  "transcript_path": "/home/user/.claude/projects/-home-user-myproject/abc123.jsonl",
  "last_assistant_message": "Response text...",
  "cwd": "/home/user/myproject"
}
```

### Transcript Path Derivation

If `transcript_path` is absent from the payload, it is derived from `session_id` and `cwd`:

```
<HOME>/.claude/projects/<slug>/<session_id>.jsonl
```

Where `<slug>` folds **every** non-alphanumeric byte of the `cwd` to `-` —
including the leading `/` — the scheme claude 2.1.263 actually uses for
`~/.claude/projects/` (verified live; the earlier strip-leading-slash scheme
produced slugs claude never creates — bead claudepr-26e7a0b6):

```
/home/user/myproject → -home-user-myproject
/tmp → -tmp
```

### Sparse Stop Payloads

Because every field is optional, a payload may arrive missing `transcript_path`,
`session_id`, `cwd`, or all of them at once. The contract (pinned by
`tests/stop_sparse_payloads_e2e.rs` across text/json/stream-json, with the
empty-string variants pinned at the unit level in `src/poller.rs` and the shared
degraded-result shape in `src/transcript.rs::TranscriptResult::from_fallback`):

1. **Derive when possible.** `transcript_path` absent (or empty — an empty
   string names nothing and selects this branch, it is never treated as a
   relative explicit path) but `session_id` + `cwd` both present and non-empty →
   derive the path per the algorithm above. HOME is consulted only on this
   branch.
2. **Derivation impossible → `last_assistant_message` fallback.** When
   `session_id` or `cwd` is absent/empty, resolution yields no path; if the
   payload still carries a non-empty `last_assistant_message`, the run is a
   **degraded success**: that text is the response (ANSI-stripped per EC-9,
   `used_fallback=true`, `num_turns` 0, zero usage, `session_id` from the
   payload — `null` when the payload had none). Same shape as the file-level
   fallback in `read_transcript` — a turn that produced an answer is not
   discarded because its metadata was sparse.
3. **Nothing to fall back to → bounded setup error.** No derivable path and no
   `last_assistant_message` → exit 2, `error:` message on stderr in text mode,
   one `internal_error` result object on stdout in json and stream-json (after
   inject). Bounded means: no panic, no hang, FIFO/temp cleanup still runs.
4. **Unknown extra fields are ignored** — payloads carrying fields
   claude-print has never heard of are processed normally
   (`#[serde(default)]`, no `deny_unknown_fields`).

Boundaries of the fallback:

- **HOME failures stay hard.** If derivation is *attempted* and `HOME` is
  unset/invalid, the strict `get_home` error propagates (exit 2) even when
  `last_assistant_message` is present — the strict HOME contract
  (`src/util.rs`, `tests/home_unset.rs`) is not relaxed by the sparse-payload
  fallback. The fallback covers payloads that *cannot name* a transcript, not
  environments that cannot resolve HOME.
- **Malformed derivation inputs stay hard.** A `cwd` containing a null byte
  fails derivation with the `cwd_to_slug` Config error (exit 2) — a broken
  payload is a bounded setup error, not a silent degradation.
- The transcript-read retry loop and its `last_assistant_message` fallback
  (below) are unchanged: they govern a *resolved* path whose file is missing or
  empty, not a payload that cannot name a path.

### Stop Firing Frequency

**Measured on claude 2.1.270** (full evidence: `docs/notes/claude-contract-probes.md`): Claude Code fires Stop **once per completed turn** — a multi-round tool-using turn produces exactly one Stop at its end, and each new user prompt in a TUI session produces its own Stop. A run cut off by `--max-turns` fires no Stop at all (it exits with an error; the `--stop-hook-timeout` watchdog owns that case). Since claude-print sends exactly one prompt per session, the first Stop payload it reads from the FIFO is the terminal signal.

Known hazard on degraded runs: when the model's tool calls are permission-**denied** (no allowlist, headless), one measured run produced an extra Stop firing. This is not reachable in the NEEDLE fleet (`claude-print.yaml` passes `--dangerously-skip-permissions`), and the single-fire poller degrades gracefully there — it acts on the first payload, the transcript retry/fallback path absorbs the rest, and the watchdog still bounds the session. The measured counts are pinned in `tests/fixtures/claude_contracts_v2.1.270.json`; the merge and suppression contracts are re-measured live by `cargo test --test claude_contracts -- --ignored`, and the Stop-count probes are re-runnable via `scripts/probe-stop-toolallowed.sh` after any Claude Code update. The degraded-run tolerance itself is pinned by regression tests: `tests/stop_poller.rs` delivers duplicate/spurious payloads coalesced through the FIFO and asserts first-payload-wins under a single fire, and `tests/stop_duplicate_firings_e2e.rs` drives full sessions via mock-claude's `MOCK_EXTRA_STOPS` knob (duplicate re-fire + spurious phantom turn) asserting exactly one result, a clean exit, and no double emission.

## UserPromptSubmit Identity Relay

The Stop payload is the first *authoritative* statement of which transcript is ours, but it arrives after the turn — far too late for live stream-json forwarding. The stream-json reader is spawned at `PROMPT_INJECTED`, where the `session_id` (and thus the exact filename `<session_id>.jsonl`) is still unknown: claude assigns it, and it only surfaces in the Stop payload. Under same-cwd concurrency the old fallback — pick the newest-growing `.jsonl` — forwarded a **sibling session's transcript wholesale** (the claudepr-a927ec0c defect). Identity has to come from a hook that fires at prompt-submission time.

`UserPromptSubmit` fires the instant the prompt is submitted, **before any assistant transcript event exists**. The relay (`identity.sh`) cats its stdin payload into `session-identity.json` beside `stop.fifo`, giving the reader a per-drive binding that cannot be a sibling's:

```json
{
  "hook_event_name": "UserPromptSubmit",
  "session_id": "abc123",
  "transcript_path": "/home/user/.claude/projects/-home-user-myproject/abc123.jsonl",
  "cwd": "/home/user/myproject"
}
```

- **Same envelope as Stop**, minus `last_assistant_message` (no assistant turn exists at submission). Every field is optional with the same forward-compat rules (`parse_stop_payload` parses both); a sparse payload without either `transcript_path` or `session_id` is unusable for binding and the reader simply keeps polling.
- **Truncating write.** `cat >` truncates: a one-prompt session fires the event once, and a rewritten file always describes the *current* session.
- **Sibling layout is contract.** `session-identity.json` lives in the same `HookInstaller` dir as `stop.fifo`. Stateless runs pass `installer.identity_path`; pool clients reconstruct it as `stop_fifo().with_file_name(SESSION_IDENTITY_FILE)` because the assignment frame carries only the Stop FIFO path — the daemon/client contract on the wire (see `docs/notes/pool-socket-protocol.md`).
- **Ordering invariant.** Identity lands before any transcript content: real claude fires the hook at submission, before the first assistant event; `mock-claude` writes the identity file just before its Stop payload and writes the transcript JSONL only after (`MOCK_DELAY_JSONL` stretches that window further), so every scenario preserves the invariant. The identity write is skipped when no prompt was ever submitted (`MOCK_STOP_BEFORE_INJECT`) and in legacy direct-spawn mode — both shapes the reader must survive unbound (below).

Pinned by `src/hook.rs::settings_json_has_user_prompt_submit_identity_hook`, `::identity_path_is_stop_fifo_sibling`, `::identity_sh_is_executable_and_targets_identity_file`, and `src/emitter.rs::resolve_identity_binding_prefers_explicit_path_then_session_id`.

## Per-Session Transcript Binding (stream-json)

The reader spawned at `PROMPT_INJECTED` (`spawn_stream_json_reader_bound`) **forwards nothing until positively bound** — an unbound reader is silent by design, because under same-cwd concurrency any guess can be a sibling's file. It polls a binding ladder every 50 ms until one rung resolves (or the reader is told to exit):

1. **Identity** — `session-identity.json` parses to a usable payload; bind to its `transcript_path` (preferred — the exact path claude reports) or `<projects_dir>/<session_id>.jsonl`. This is the normal path; with working hooks identity lands within milliseconds of submission.
2. **Unambiguous new candidate** (identity-less fallback, for a claude without UserPromptSubmit support) — exactly one `.jsonl` **created after** the injection snapshot, and only once it has stayed sole for `IDENTITY_GRACE` (250 ms). Identity outranks it at every tick, so an identity payload landing mid-grace wins; a sibling's second new file makes the scan ambiguous and resets the wait. "Created after the snapshot" is what makes a single candidate unambiguous: claude-print drives fresh sessions, so this session's transcript is always a new file, and a sibling that started *before* us only ever grows a pre-existing one.
3. **Stop-payload retarget** (authoritative backstop) — when neither rung resolves, the session's normal Stop transition delivers the resolved transcript path via `StreamJsonHandle::retarget` **before** `signal_drain`. An identity-less ambiguous run therefore forwards nothing live and its output arrives whole — correct and uncontaminated — at the drain.

The bind offset follows the injection snapshot: a file present at injection is tailed from its injection-time size (skip the pre-injection bytes of an ongoing session's file); a file created after injection is tailed from 0. **Retarget semantics:** already bound to the same path → no-op (no offset reset, no duplicate tail); bound to a different file or still unbound → rebind at the snapshot offset, so the drained tail carries this session's final events, result event included. The ordering is load-bearing: the reader honors a pending retarget before a pending drain at every idle tick, so a retarget sent first is always applied to the drained tail. Lines already forwarded from a wrongly-fallback-bound file cannot be retracted — that residue is the documented bound of the identity-less fallback (a near-simultaneous same-cwd sibling can still mis-attribute the live prefix; identity-bound runs cannot).

The reader is joined on **every** exit path (INV-8): `Drop` disconnects the drain/retarget channels and joins the thread, so dropping the handle — normally, on error, or while still unbound — never orphans it. Pinned by `src/emitter.rs::bound_reader_forwards_nothing_while_identity_unresolved` and `::bound_reader_binds_identity_exact_path_not_newest_mtime`; the ladder's race shapes by `tests/integration/scenarios.rs::stream_json_reader_identity_binding_wins_over_newest_sibling`, `::stream_json_reader_binds_when_identity_arrives_late`, `::stream_json_reader_late_identity_rebinds_after_fallback_bind_matured`, `::stream_json_reader_refuses_ambiguous_candidates_until_retarget`, `::stream_json_reader_retarget_same_path_does_not_duplicate`, `::stream_json_reader_retarget_binds_from_snapshot_offset`; the join/drain lifecycle by `tests/stream_json_cleanup.rs` (including `test_stream_json_bound_reader_drop_joins_thread_while_unbound`).

Both session arms use the same ladder. The stateless arm binds via `installer.identity_path` with the projects dir derived from this process's cwd. The pool arm binds via the **worker's** identity file — `worker.stop_fifo().with_file_name(SESSION_IDENTITY_FILE)` — with the projects dir derived from the **worker's** cwd (`projects_dir_for(worker.worker_cwd())`): the worker's claude runs in the daemon's cwd and writes its transcript under that slug, so deriving from the client's cwd would watch the wrong directory. The pool daemon installs the same two relay hooks per worker (`HookInstaller` in `create_worker`, with `--setting-sources=` empty so no user hooks fire), so the identity file appears beside the worker's Stop FIFO the moment the worker is driven. See `docs/notes/pool-socket-protocol.md` for the wire-side contract.

## Keeper FD Pattern

Linux FIFO semantics: `open(O_WRONLY|O_NONBLOCK)` returns `ENXIO` if no reader is present. To prevent the hook's `cat > fifo` from blocking or failing, `claude-print` uses a **keeper write-end fd**:

1. Open FIFO read-end `O_RDONLY|O_NONBLOCK` — always succeeds immediately
2. Open keeper write-end `O_WRONLY|O_NONBLOCK` — succeeds because read-end is now open
3. Hold keeper write-end open until Stop fires
4. When Stop fires, read the payload, then close the keeper write-end
5. Claude Code's `cat > fifo` opens its own write-end (simultaneous write-ends are valid)

### Cleanup on Non-Stop Exit Paths

On exit paths where Stop never fires (SIGINT, timeout, child exit), the keeper write-end **must be explicitly closed** before `waitpid`:

- Closing the keeper causes any pending `cat > fifo` in `hook.sh` to receive `EPIPE`/`ENXIO` and exit
- Without this, the hook runner would hang indefinitely waiting for a reader that will never come

## FIFO Poller States

```
UNOPENED
  │  opened O_NONBLOCK at TRUST_DISMISSED → PROMPT_INJECTED transition
  ▼
OPEN_WAITING
  │  FIFO becomes readable (Stop hook wrote payload)
  ▼
PAYLOAD_READ → DONE
```

The FIFO read-end is opened **before** the bracketed paste is injected (at the `TRUST_DISMISSED → PROMPT_INJECTED` transition), so Stop cannot fire before the reader is ready.

## Hook Inheritance

### Default Mode (inherit hooks)

By default, `claude-print` does not redirect `CLAUDE_CONFIG_DIR`. The inner `claude` process:

- Writes transcripts to `~/.claude/projects/<cwd-slug>/<session-id>.jsonl`
- Writes session entry to `~/.claude/sessions/<pid>.json`
- Appends to `~/.claude/history.jsonl`
- Fires all hooks in `~/.claude/settings.json` alongside the relay hooks

### Isolation Mode (--no-inherit-hooks)

When `--no-inherit-hooks` is passed:

- `--setting-sources=` (empty) is forwarded to claude — suppresses loading of standard settings sources (**measured** on claude 2.1.270: the empty spelling is accepted, suppresses every standard source, and does not suppress the `--settings` file; the alternative spelling `=none` is rejected outright — exit 1 before session start)
- Only `--settings <temp>/settings.json` is active — contains solely the relay hooks
- User hooks (SessionStart, Stop, PreToolUse, ccdash, trail-boss, etc.) do not fire

Use this mode for NEEDLE workers to prevent hook noise, or when user hooks have side effects.

**Pool workers always run isolated.** The pool daemon spawns each worker with `--setting-sources=` (empty) and only the per-worker `--settings` file — the isolation-mode behavior, independent of the client's `--no-inherit-hooks` flag (which cannot apply to an already-running worker). See `docs/notes/pool-socket-protocol.md`.

## Cleanup

The temp directory is cleaned up on all exit paths via `tempfile::TempDir` drop:

- Normal exit: Stop fires, payload read, cleanup completes
- Timeout: keeper write-end closed, child SIGTERM'd, cleanup runs
- SIGINT: interrupted flag set, poll loop breaks, cleanup runs
- Panic: `TempDir` destructor runs during unwind

`HookInstaller::cleanup` is **idempotent** — an atomic swap flag makes it safe to call explicitly and again from `Drop` (including during unwind). It removes `stop.fifo` first (up to three attempts — the FIFO may carry different permissions than the dir), then `remove_dir_all` of the whole dir, also retried; `session-identity.json` and the two relay scripts go with the directory. Pinned by `src/hook.rs::cleanup_can_be_called_multiple_times`, `::cleanup_explicitly_removes_fifo`, `::temp_dir_cleaned_up_on_drop`.

### Orphan sweep (crashed runs)

`Drop` cannot run after a `SIGKILL` or a host crash, so every invocation begins with `hook::cleanup_orphans()` at the top of `main()`: a sweep of `$TMPDIR` for `claude-print-*` directories whose mtime is older than **60 s**, removed **only when the PID embedded in the directory name is provably no longer running** (`kill(pid, 0)`; `EPERM` counts as alive — a process owned by another user is one we cannot positively identify as dead). A directory whose name carries no parseable PID is left alone. The liveness check is what protects EC-1's no-cross-contamination guarantee: a concurrent long-running claude-print session's `stop.fifo`/`session-identity.json` IPC is never deleted out from under it, no matter its age. Pinned by `src/hook.rs::cleanup_preserves_temp_dir_with_live_owner_pid`, `::cleanup_removes_temp_dir_with_dead_owner_pid`, `::cleanup_leaves_young_dirs_alone`, `::owner_pid_parses_from_dir_name`.

### Pool-mode ownership

On the `--pool-socket` path the **client owns no hook artifacts**. The daemon creates one `HookInstaller` dir per worker at `create_worker` (named with the *daemon's* pid), and `destroy_worker` — reached on release, warmup failure, or daemon shutdown — closes the PTY master, SIGTERMs the worker's process group, reaps it, and drops the installer, removing that worker's `stop.fifo` and `session-identity.json` with it. A client that dies mid-session leaves its artifacts to the daemon's destroy path, and the daemon's own crash leaves them to the orphan sweep above. Sequential callers therefore observe fresh, distinct hook artifacts per invocation — pinned (distinct Stop FIFO, pid, PTY, and no crossing prompt/env/session between two invocations) by `src/pool.rs::two_sequential_pooled_invocations_observe_zero_cross_caller_leakage` and `tests/pool_socket_e2e.rs::sequential_clients_get_fresh_replaced_workers_with_no_cross_caller_leakage`.
