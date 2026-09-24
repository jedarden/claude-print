# Output-Format Contracts

| | |
|---|---|
| **Contract version** | v1 |
| **Pinned by** | `tests/output_format_contracts.rs` against `tests/fixtures/output_format_examples_v1.json` |
| **Implementation** | `src/emitter.rs` (`emit_success`, `emit_error`, the stream-json reader thread), values shaped by `src/transcript.rs` and `src/error.rs`, wired by `src/main.rs` |
| **Provenance** | bead claudepr-1a89e5b4 (2026-09-24) |

This note is the normative definition of what `--output-format` `text`,
`json`, and `stream-json` write to stdout and stderr: the fields, the
framing, the ordering, and the error shapes. The README summarizes; where the two differ,
this document wins. Every example below is pinned byte-for-byte by the
fixture, and the test fails if the emitter's output, the fixture, or this
document drift apart — changing the contract means changing all three in one
commit.

## Ground rules (all modes)

1. **stdout carries the response payload only.** Everything human —
   `--verbose` timing traces, warnings, `--show-child-stderr` PTY dumps, and
   any error a mode renders as text — goes to stderr. A caller piping stdout
   never receives diagnostics mixed into the payload.
2. **Exit codes are mode-independent** (table below). The mode changes the
   *shape* of the error report, never the process status.
3. **Every JSON document claude-print writes is one line**, terminated by
   exactly one LF, with keys serialized in lexicographic order (serde_json's
   default map ordering; the crate does not enable `preserve_order`). Parsers
   should not depend on key order — the fixtures pin the exact bytes anyway,
   so any reordering or field addition surfaces as a conscious contract
   change rather than silent drift.
4. **Validation failures that fire before the emitter engages** (no prompt
   found, unreadable/oversize `--input-file` or stdin, a NUL byte in the
   prompt) are always plain stderr lines prefixed `claude-print: `, in every
   mode, with no JSON shape. Every failure after that point goes through the
   emitter and follows the mode contracts below.

### Exit codes and subtypes

| Exit | `subtype` | `ClaudePrintError` variant | When |
|------|-----------|----------------------------|------|
| 0 | `success` | — | Turn completed; the Stop hook fired and the transcript was read |
| 1 | `assistant_error` | `AssistantError` | The turn finished but the transcript reported `is_error: true` (rate limit, tool failure) |
| 2 | `internal_error` | `Setup` or `Config` | claude-print-side failure: missing binary, PTY/hook setup, parse failure, config error, child exited before Stop |
| 124 | `timeout` | `Timeout` | A watchdog or wall-clock deadline expired |
| 130 | `interrupted` | `Interrupted` | SIGINT/SIGTERM forwarded |
| 4 | *(none — `claude-print: …` stderr line)* | — | Input validation (rule 4 above) |

## `text` mode (default)

### Success

Exactly the response text, byte-for-byte, followed by one LF:

```text
The capital of France is Paris.
```

- The text is the concatenation of the final assistant turn's `text` blocks
  from the session transcript, verbatim — interior blank lines and paragraphs
  are preserved, and nothing is added, wrapped, or truncated.
- The one transformation: results built from the Stop payload's
  `last_assistant_message` fallback (`used_fallback`) are ANSI-stripped
  before emission (EC-9), so PTY render escapes can never reach stdout.
  Transcript-sourced text is emitted verbatim without passing through the
  stripper.

### Errors

One stderr line — the prefix `error: ` plus the message plus LF; stdout stays
empty:

```text
error: operation timed out
```

## `json` mode

### Success

Exactly one line on stdout: a single JSON `result` object terminated by one
LF. Fields, in lexicographic (serialization) order:

| Field | Type | Value |
|-------|------|-------|
| `claude_version` | string | First line of the child's `--version` output, e.g. `2.1.270 (Claude Code)`; `"unknown"` on error paths where resolution never ran |
| `cost_usd` | number | Always `0` — subscription-billed, no metered cost is measured |
| `duration_ms` | number | Wall-clock milliseconds for the whole invocation, measured from process start in `main` |
| `is_error` | boolean | `false` on the success path (a transcript with `is_error: true` is converted to an error result before emission — the field is read from the transcript, never hardcoded) |
| `num_turns` | number | Assistant turns counted in the transcript (deduplicated by `message.id`) |
| `result` | string | The response text (same content `text` mode prints; there is no `text` or `model` field) |
| `session_id` | string \| null | The session's id from the transcript records; `null` when none could be recovered |
| `subtype` | string | `"success"` |
| `type` | string | `"result"` |
| `usage` | object | Four integer counters, each the **sum over assistant turns** of the per-record usage: `cache_creation_input_tokens`, `cache_read_input_tokens`, `input_tokens`, `output_tokens` (absent per-record fields count as 0) |

```json
{"claude_version":"2.1.270 (Claude Code)","cost_usd":0,"duration_ms":45213,"is_error":false,"num_turns":3,"result":"The capital of France is Paris.","session_id":"3f2b8c64-9d1e-4a7b-b2c3-1e5f9a8d6c04","subtype":"success","type":"result","usage":{"cache_creation_input_tokens":650,"cache_read_input_tokens":21000,"input_tokens":5200,"output_tokens":42}}
```

With no session id recovered (and zeroed usage — e.g. the fallback result
built from a sparse Stop payload):

```json
{"claude_version":"2.1.270 (Claude Code)","cost_usd":0,"duration_ms":0,"is_error":false,"num_turns":1,"result":"ok","session_id":null,"subtype":"success","type":"result","usage":{"cache_creation_input_tokens":0,"cache_read_input_tokens":0,"input_tokens":0,"output_tokens":0}}
```

### Errors

Same one-line framing, five fields (`claude_version`, `error_message`,
`is_error` always `true`, `subtype`, `type` `"result"` — no `result`, `usage`,
`session_id`, `num_turns`, `duration_ms`, or `cost_usd`):

```json
{"claude_version":"2.1.270 (Claude Code)","error_message":"operation timed out","is_error":true,"subtype":"timeout","type":"result"}
```

```json
{"claude_version":"2.1.270 (Claude Code)","error_message":"Credit balance too low","is_error":true,"subtype":"assistant_error","type":"result"}
```

**Stream selection:** the error object goes to **stdout** — the caller's pipe
carries the structured failure — *except* config errors, which go to
**stderr** in every mode. A config failure fires before a session exists, so
stdout, which carries the response payload, is left clean for it:

```json
{"claude_version":"2.1.276 (Claude Code)","error_message":"invalid config: malformed.toml: TOML parse error at line 1, column 3\n  |\n1 | [[\n  |   ^\ninvalid key\n","is_error":true,"subtype":"internal_error","type":"result"}
```

(The message is whatever the config layer reported; embedded newlines are
JSON-escaped, so the object is still one line. `Setup` and `Config` share the
`internal_error` subtype and exit 2.)

## `stream-json` mode

### Success

stdout is a live JSONL replay of **this session's transcript records**,
written as claude appends them:

- **Framing** — one transcript record per line, LF-terminated. A line is
  forwarded **verbatim**: no re-serialization, no reformatting, no field
  filtering. Compact input stays compact; spaced input keeps its spaces.
  Blank lines are dropped; a trailing CR (CRLF-terminated records) is
  trimmed.
- **Ordering** — transcript-file order, exactly. Records are never reordered,
  deduplicated, or synthesized on this path.
- **Start offset** — replay begins at the transcript's byte size captured
  when the prompt was injected, so pre-injection records (session start,
  system messages of an ongoing session) are skipped.
- **Binding** — the reader resolves *this* session's transcript via the
  per-drive identity file the `UserPromptSubmit` relay hook writes, falling
  back to a sole-new-transcript scan, with the Stop payload as the
  authoritative retarget; under same-cwd concurrency an unresolvable binding
  forwards nothing live rather than guessing (details and rationale in
  `src/emitter.rs` on `spawn_stream_json_reader_bound_to`, and the README's
  concurrency note).
- **Completion** — on the normal Stop transition the reader drains the
  remaining records, is joined, and the process exits 0. `emit_success` is a
  **no-op** in this mode: the replay is the whole success output, and —
  because a PTY-driven transcript contains no `type: "result"` record —
  **there is no synthesized trailing result object.** Callers detect
  completion by process exit, not by watching for a result event (see
  "Divergences" below).

Input (what the transcript file holds; the blank line and the CR are
deliberate) →

```jsonl
{"type":"user","message":{"role":"user","content":"hi"},"sessionId":"s-270","uuid":"u-1"}

{"type":"assistant","message":{"id":"m-1","content":[{"type":"text","text":"hello"}]},"sessionId":"s-270","uuid":"a-1"}
{ "type": "assistant", "message": { "id": "m-2", "content": [{ "type": "text", "text": "spaced JSON stays byte-for-byte" }] }, "sessionId": "s-270", "uuid": "a-2" }
{"type":"assistant","message":{"id":"m-3","content":[{"type":"text","text":"trailing CR trimmed"}]},"sessionId":"s-270","uuid":"a-3"}
```

stdout (exactly; blank line dropped, CR trimmed, spacing preserved):

```jsonl
{"type":"user","message":{"role":"user","content":"hi"},"sessionId":"s-270","uuid":"u-1"}
{"type":"assistant","message":{"id":"m-1","content":[{"type":"text","text":"hello"}]},"sessionId":"s-270","uuid":"a-1"}
{ "type": "assistant", "message": { "id": "m-2", "content": [{ "type": "text", "text": "spaced JSON stays byte-for-byte" }] }, "sessionId": "s-270", "uuid": "a-2" }
{"type":"assistant","message":{"id":"m-3","content":[{"type":"text","text":"trailing CR trimmed"}]},"sessionId":"s-270","uuid":"a-3"}
```

### Errors

The emitter exposes one switch, `stream_json_after_inject`:

- **After the prompt was injected** (the CLI's error arms): the JSON error
  object is **appended to stdout as the final line**, after whatever the
  reader already forwarded — the pipe ends with a structured failure. Config
  errors still go to stderr (same rule as `json` mode).

  ```json
  {"claude_version":"2.1.270 (Claude Code)","error_message":"Credit balance too low","is_error":true,"subtype":"assistant_error","type":"result"}
  ```

- **Before injection** (emitter-level branch): the text-mode shape — one
  `error: …` stderr line, stdout untouched:

  ```text
  error: operation timed out
  ```

  Note the CLI currently passes `after_inject = true` on *every* error arm —
  including pre-session failures like a missing binary or an unset `HOME`
  (AS-5: JSON callers must never be left with empty stdout) — so the
  before-inject branch describes the library surface, not an observable CLI
  state today.

## Divergences from `claude -p`

claude-print is a drop-in replacement, with three documented differences:

1. **`cost_usd` is always `0`** — sessions bill against the subscription, and
   no metered cost is measured. There is no `model` field either.
2. **`stream-json` output ends with the last transcript record, not a
   `result` event** — a PTY-driven session writes no `type: "result"`
   record, and claude-print does not synthesize one on success. Completion is
   signaled by process exit (and the error object on failure paths).
3. **Pre-emitter validation failures are unstructured** — the exit-4/oversize
   cases in ground rule 4 print `claude-print: …` stderr lines in every mode,
   where `claude -p` may shape them per mode.

## How the contract is pinned

`tests/output_format_contracts.rs` loads
`tests/fixtures/output_format_examples_v1.json` and checks three layers:

1. **Emitter alignment** — every fixture case is replayed through
   `emit_success` / `emit_error` / the stream-json reader, and the captured
   stdout/stderr must equal the fixture's expected bytes exactly. Field
   sets, key order, single-line framing, and the stdout/stderr routing
   matrix (including config-to-stderr and the after-inject switch) are all
   covered by this byte comparison.
2. **Error-table alignment** — the fixture's subtype table (variant →
   `subtype()`, `exit_code()`, `message()`) is asserted against
   `ClaudePrintError`'s accessors, so the doc's exit-code table cannot drift
   from `src/error.rs`.
3. **Doc alignment** — every fixture case marked `documented: true` must
   appear verbatim in this document (trailing newline aside; a case whose
   payload is empty — the stream-json success no-op — has no bytes to search
   for and is pinned by the byte layer alone), so an example edited here
   without updating the fixture — or vice versa — fails the test.

A contract change therefore updates implementation, fixture, and this
document together, and bumps `contract_version` for anything a caller could
observe. The stream-json *transcript* record shapes themselves (what claude
writes into the file being replayed) are Claude Code's contract, not
claude-print's — those are pinned separately by the transcript fixtures and
`docs/notes/claude-contract-probes.md`.
