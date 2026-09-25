# The stream-json Output Contract

| | |
|---|---|
| **Pinned by** | `tests/stream_json_contract.rs` against the `tests/fixtures/stream_json_golden_v2.1.270.{input,expected,errors}.jsonl` triple |
| **Implementation** | `src/emitter.rs` (the reader thread: `spawn_stream_json_reader_bound_to`, `tail_loop`, `StreamJsonHandle`), spawned and drained by `src/session.rs`; error objects by `emit_error` |
| **Summarized by** | `docs/notes/output-format-contracts.md` §`stream-json` mode and the README — this document is the normative deep-dive; where they differ, this document wins |
| **Provenance** | bead claudepr-6b587922 (2026-09-25); binding design bead claudepr-a927ec0c; first-output credit bead claudepr-33fdf4ed |

This note is the normative definition of what `--output-format stream-json`
writes to stdout: the framing of each line, which records are forwarded and
in what order, what "live" delivery guarantees, how the stream completes,
how the reader binds to *this* session's transcript under concurrency, and
what an error looks like on the wire. Every guarantee below is pinned
byte-for-byte by the golden fixtures — the reader's stdout is compared
against a committed file, so any re-serialization, reorder, drop, duplicate,
or synthesized byte surfaces as a diff in review, not as silent drift.

The golden fixtures are version-pinned like the transcript captures
(`tests/fixtures/transcript_v2.1.*.jsonl`): the `v2.1.270` in the filename
names the Claude Code capture family whose record shapes the fixtures model,
and the `claude_version` stamped into the golden error objects is
`2.1.270 (Claude Code)`. Re-pins follow the same maintenance workflow as the
other version-stamped fixtures (`docs/notes/claude-contract-probes.md`
§Maintenance — regenerate through the capture path, never hand-edit).

## 1. Scope

In `stream-json` mode, claude-print's stdout is a **live JSONL replay of
this session's transcript records**, written as claude appends them to
`~/.claude/projects/<cwd-slug>/<session_id>.jsonl`. The reader thread is
spawned at the `PROMPT_INJECTED` transition and tails that file; it neither
produces the events nor interprets them — it is a byte pipe from the
transcript file to stdout, with exactly the framing transformations of §2
and nothing else.

The contract covers six behaviors, each with its pinning test:

| § | Guarantee | Pinning test in `tests/stream_json_contract.rs` |
|---|-----------|--------------------------------------------------|
| 2 | Wire format | `golden_final_record_without_trailing_newline_is_still_terminated`, `golden_non_json_lines_are_forwarded_verbatim` |
| 3 | Event schema | `golden_v2_1_233_capture_is_forwarded_verbatim_including_result_record`, `golden_non_json_lines_are_forwarded_verbatim` |
| 4 | Ordering | `golden_full_replay_is_byte_identical_and_retarget_does_not_duplicate` |
| 5 | Completion behavior | `golden_full_replay_is_byte_identical_and_retarget_does_not_duplicate` |
| 6 | Incremental delivery | `golden_incremental_arrival_replays_expected_bytes_in_order` |
| 7 | Transcript identity binding | `golden_identity_binding_with_snapshot_offset_skips_pre_injection_lines` |
| 8 | Error behavior | `golden_synthesized_error_result_bytes` |

Exit codes are mode-independent and defined with the full error table in
`docs/notes/output-format-contracts.md`; this document does not redefine
them.

## 2. Wire format

stdout carries **one transcript record per line, LF-terminated**. A record
is forwarded **verbatim**: no re-serialization, no reformatting, no field
filtering. Compact input stays compact; spaced input keeps its spaces;
unicode text passes through byte-for-byte. The framing adjustments the
reader makes are exactly three:

1. **Line termination** — the line's terminating LF (if present in the
   file) is not copied as-is; every forwarded line is written with exactly
   one trailing LF. A final record with **no** trailing newline in the file
   still yields a newline-terminated line on stdout — the stream's last
   line is never left dangling.
2. **CR trimming** — a trailing CR (a CRLF-terminated record) is stripped
   before the write. No other byte is touched, and no CR ever reaches
   stdout.
3. **Blank-line dropping** — a line that is empty after terminator
   stripping (a blank line in the transcript) is dropped entirely, producing
   no stdout bytes. A line containing only spaces is *not* blank and is
   forwarded verbatim.

The reader treats the transcript as UTF-8 text lines (which claude's
transcripts are by construction); whether a line parses as JSON is not its
concern — see §3.

## 3. Event schema

**The reader does not define one.** It forwards every non-blank line without
parsing, validating, filtering by record type, or inspecting fields.
Consequences, each pinned:

- **Every record type flows.** `summary`, `user`, `assistant`, `system`,
  `result` — the reader has no type-aware behavior on this path. A
  `result` record, when the transcript holds one (the print/SDK-shaped
  captures, e.g. `tests/fixtures/transcript_v2.1.233.jsonl`), is forwarded
  like any other line, verbatim.
- **No synthesis of a `result` record.** A PTY-driven session (what
  claude-print actually drives) writes no `type: "result"` record to its
  transcript, and the reader does not add one — stream-json output ends
  with the last transcript record (Divergence 2 in
  `docs/notes/output-format-contracts.md`). Completion is signaled by
  process exit, not by a result event (§5).
- **Split assistant records sharing a `message.id` are both forwarded.**
  The `json`-mode deduplication by `message.id` (for `num_turns`) does not
  exist on the stream path — the replay is the file's bytes, and two
  records with the same `message.id` but different `uuid`s are two lines.
- **`thinking` and every other content-block type** pass through as bytes.
- **A line that is not valid JSON is forwarded verbatim.** Validity
  checking, schema validation, and per-type handling are the consumer's
  job. (The golden input carries no such line; the non-JSON pin appends one
  to a copy of the golden bytes and requires it forwarded unchanged.)

## 4. Ordering

Records reach stdout in **transcript-file order, exactly**. The reader never
reorders, deduplicates, filters, or synthesizes records on this path. A full
replay of the golden input must equal the golden expected bytes byte-for-
byte — any reorder, drop, or duplicate is a contract violation by
definition.

The only way order can deviate from single-file order is a **rebind**
(§5/§7): if the reader swaps from one transcript file to another mid-run,
the output is the lines already forwarded from the first file, then the
post-snapshot-offset lines of the second. That is the documented residue of
the misbinding fallback and cannot occur on the identity-bound path (§7).

## 5. Completion behavior

The stream ends on **process exit**, and the reader is joined on *every*
exit path (plan invariant INV-8 — `main()` always terminates via
`process::exit()`, so an unjoined reader would be killed mid-write and
truncate its output; `StreamJsonHandle`'s `Drop` disconnects the channels
and joins the thread, making every drop — including `?` propagation — safe).

- **Normal Stop transition** — the session, having received the Stop hook
  payload, calls `handle.retarget(transcript_path)` with the payload's
  resolved path, **then** `handle.signal_drain()`, then drops the handle.
  This ordering is load-bearing: at every idle tick the reader honors a
  pending retarget *before* a pending drain, so the drained tail always
  comes from the transcript the Stop payload names. Draining means: forward
  everything appended since the last read **until the tail catches up with
  the file's end**, then exit. The reader is joined, the process exits 0.
- **Stop-retarget idempotence** — a retarget naming the path the reader is
  already bound to (the normal identity-bound run) is consumed and dropped:
  no seek, no offset reset, **no duplicate forwarding**. The golden test
  replays the full transcript, retargets to the same path, drains, and
  requires the output to remain exactly the golden bytes.
- **Retarget to a different path** — swap the tail to that file at its
  injection-snapshot offset (§7), then drain it. Lines already forwarded
  from the previous file cannot be retracted; that residue is the
  documented bound of the fallback (§7 rung 3).
- **Every non-Stop exit path** (watchdog timeout, SIGINT/SIGTERM,
  child-exit-without-Stop, any `?` early return): the handle is dropped
  **without** a drain signal. `Drop` disconnects the channels; the reader
  treats `Disconnected` as exit-immediately and the join returns promptly.
  No drain is performed — whatever was forwarded so far stays on stdout, no
  remainder is chased, and the error result object of §8 is appended.
- **`emit_success` is a no-op in this mode** — the replay is the whole
  success output. There is no trailing summary object and no synthesized
  result (§3).

If the reader is told to exit before any binding resolved, it forwards
nothing and never opens a file. The one exception is the backstop of §7
rung 3: a Stop-payload retarget pending during the bind poll outranks the
drain, binds immediately at the snapshot offset, and the drained tail is
forwarded from that file — which is how an identity-less ambiguous run
(§7) still produces complete, correct output.

## 6. Incremental-delivery guarantees

Forwarding is **live**, not end-of-turn bulk output:

- **Per-line forwarding** — each non-blank line is written to stdout the
  moment it is read from the tail, one line at a time. stdout is
  line-buffered, so a forwarded line reaches the caller's pipe on its write;
  nothing is held for end-of-turn.
- **Append order, prefix property** — because lines are forwarded in file
  order as they arrive (§4), the accumulated output is at every instant a
  **prefix of the full replay**: a consumer that reads the pipe sees a
  growing prefix of exactly the bytes a full replay would produce. The
  golden incremental test grows the transcript line by line (blank line and
  CRLF line included) while the reader runs and requires (a) the final
  accumulation to equal the golden expected bytes and (b) lines to have
  arrived *before* the drain was signaled.
- **Latency shape** — the tail polls at a 5ms idle cadence; the binding
  poll runs at 50ms (§7). A line appended to the bound transcript is
  forwarded within one poll–read–write of its append; there is no batch,
  debounce, or coalescing.
- **First-output credit (watchdog Phase 2)** — the reader sets the
  watchdog's first-output flag the moment it forwards its first transcript
  line, so the stream-json first-output deadline
  (`--stream-json-timeout`, default 90s) measures time to first *forwarded*
  output, not an unconditional session cap. If live tailing was never
  enabled (the projects dir could not be resolved), the session credits the
  flag itself — an unobservable stream must not re-arm an unconditional
  kill. Phase 2 is armed only in stream-json mode; `text` and `json` cannot
  satisfy it and never arm it.

## 7. Transcript identity binding

At `PROMPT_INJECTED` the exact transcript filename is still unknown — the
`session_id` (which names `<session_id>.jsonl`) is assigned by claude and
surfaces only in the Stop payload, after injection. The reader therefore
**binds**, polling a three-rung ladder every 50ms until one resolves (or it
is told to exit):

1. **Identity file** — the per-drive `session-identity.json` (sibling of
   `stop.fifo`) that the `UserPromptSubmit` relay hook writes the instant
   the prompt is submitted — before any assistant event exists. Its
   `transcript_path` is preferred; a bare `session_id` joins the projects
   dir as `<projects_dir>/<session_id>.jsonl`. This is the normal path:
   binding lands within milliseconds of submission and live forwarding is
   unaffected. An absent, mid-write, unparseable, or sparse identity file
   simply polls again.
2. **Unambiguous new transcript** — the identity-less fallback (a claude
   without UserPromptSubmit hook support): bind the **single** `.jsonl`
   created after the injection snapshot, and only once it has remained the
   sole new candidate for a 250ms grace. Identity outranks this rung at
   every tick, so a payload landing mid-grace wins. Two or more new
   transcripts are unresolvable — the refusal is the fix (the old
   newest-mtime guess forwarded a sibling wholesale).
3. **Stop-payload retarget** — the authoritative backstop when neither rung
   resolves during the run: the Stop payload names the transcript, the
   session retargets the reader to it before the drain (§5), and the
   output arrives whole — correct and uncontaminated, but not streamed.

**Until a binding exists the reader forwards nothing** — under same-cwd
concurrency a guessable file can be a sibling session's, and forwarding
nothing live is strictly safer than forwarding the wrong session.

**Start offsets come from the injection snapshot.** Before spawning the
reader, the session snapshots every `.jsonl` in the projects dir to its
byte size (`snapshot_jsonl_sizes`). A newly bound file present in that
snapshot starts at its **snapshot byte offset** — pre-injection bytes (an
ongoing session's `SessionStart`/system records) are skipped; a file absent
from it is tailed from offset 0. The offset is a *byte* count, not a line
count. The golden binding test stages the input with a snapshot offset
after its first three records and requires exactly the post-offset expected
lines, unduplicated.

**A late identity still wins over a fallback bind.** A Bind-source tail
keeps polling the identity file at the 50ms cadence after binding; an
identity landing after the fallback matured (a hook slower than the grace)
rebinds the tail onto the right transcript at its snapshot offset. A Stop
retarget is never overridden by identity — it arrives with the retarget of
§5, and the session stops polling identity once Stop resolves the
transcript.

## 8. Error behavior

Errors synthesize a `result` object on the wire — the only object the
stream-json path ever *writes* rather than forwards. It has five fields
(lexicographic serialization order): `claude_version`, `error_message`,
`is_error` (always `true`), `subtype`, and `type: "result"`. Both golden
error lines — the session error and the config error — are pinned
byte-for-byte in `stream_json_golden_v2.1.270.errors.jsonl`.

- **Session errors after injection** (watchdog timeout, interrupt,
  assistant error, child exit, internal failure): the object is **appended
  to stdout as the final line**, after whatever the reader already
  forwarded — the pipe ends with a structured failure, never a silent
  truncation. stderr stays empty.
- **Config errors**: the object goes to **stderr**, and stdout — which
  carries the response payload — is left untouched (a config failure fires
  before a session exists).
- **Exit codes** are the mode-independent table of
  `docs/notes/output-format-contracts.md` (`timeout` 124, `interrupted`
  130, `internal_error` 2, `assistant_error` 1); the subtype and message
  inside the object come from the same error value, and `claude_version`
  is the resolved child version string (`<x.y.z> (Claude Code)`, `"unknown"`
  on paths where resolution never ran).
- **Before injection** the emitter's library surface falls back to the
  text-mode shape (one `error: …` stderr line, stdout untouched); the CLI
  passes `after_inject = true` on every error arm so JSON callers are never
  left with empty stdout (AS-5), so this branch describes the library
  surface rather than an observable CLI state.

The full stream-selection matrix and the `json`-mode counterpart of these
objects are specified in `docs/notes/output-format-contracts.md`; the
fixtures there pin the field set, these goldens pin the exact bytes on the
stream-json path.
