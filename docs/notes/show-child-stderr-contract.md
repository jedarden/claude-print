# `--show-child-stderr` Contract (child PTY output capture)

| | |
|---|---|
| **Contract version** | v1 |
| **Pinned by** | `tests/show_child_stderr_contract.rs` (end to end), `src/session.rs` `#[cfg(test)]` (ring-buffer mechanics) |
| **Implementation** | `src/cli.rs` (the flag), `src/main.rs` (`LaunchOptions` wiring and the pooled dispatch), `src/session.rs` (`ChildCapture`, the two event-loop feeds, the six dump sites) |
| **Provenance** | bead claudepr-8b4aa030 (2026-09-27) |

The `claude -p` compatibility contract
(`docs/notes/claude-p-compat-contract.md` §1) summarizes `--show-child-stderr`
as an accepted claude-print-local flag, default off, that never reaches the
child — and leaves it at that. This note is the normative definition behind
that one line: what exactly is captured, the three exit windows that surface
it, the exact stderr block it renders, how the three output modes and the
pool paths treat it, and what the flag can never do. The test named above
holds this document against the implementation in both directions — the same
two-sided discipline as the sibling contracts — so the flag's behavior cannot
move without the document, the suite, and the implementation moving together.

Throughout, **the child** means the `claude` process claude-print spawns under
a PTY (stateless) or drives on a prewarmed pool worker (pooled), and **a dump**
means the one bounded stderr block described in §4.

## 1. Purpose and scope

claude-print swallows the child's terminal output by design: the response is
read from the transcript, and the TUI's own rendering (dialogs, repaints,
control sequences) is an implementation detail no caller should see. That is
the right default until a session wedges — an MCP server that never connects,
an init hook that blocks, a model call that hangs — at which point the
child's own last words are the only diagnosis available, and by then they are
gone. `--show-child-stderr` is the escape hatch: it keeps a bounded tail of
the child's output through the session and prints it when the run ends in one
of the failure windows of §3.

- **Default off, and off is free.** When the flag is absent the capture
  buffer is never allocated — the event-loop feed is a no-op and the hot
  path is unchanged. Enable it when diagnosing, not in production configs.
- **Not a tee.** A successful (or otherwise non-window) run prints nothing
  of the child's output even with the flag set. The capture exists solely
  for the §3 exit windows; there is no mode in which the child's stream
  streams anywhere.
- **"child stderr" is a name, not a stream selection.** The child runs on a
  PTY, and a PTY has one output side: the flag surfaces the child's combined
  stdout+stderr as rendered into that terminal — exactly the bytes
  claude-print's event loop already reads — not the child's fd 2 alone.

## 2. What is captured

- **Every non-empty PTY chunk** the session event loop delivers, on both the
  stateless and the pooled path — the same chunks the terminal emulator and
  the startup scanner consume, captured before any interpretation. This
  includes the PTY's **echo of claude-print's own writes**: the
  trust-dialog dismissal keys and the injected prompt travel through the
  line discipline and come back as output, so a dump shows the child-side
  view of the session, not only what the child volunteered.
- **The last 64 KiB, no more.** The capture is a ring buffer
  (`ChildCapture::CAP` = 64 × 1024 bytes): once full, the oldest bytes drop
  so a chatty-but-wedged child cannot grow memory unbounded. The tail is
  what diagnoses a stall — the wedge's cause sits at the end of the output.
- **Raw bytes.** Nothing is decoded, escaped, or validated as UTF-8; the
  captured tail is written to stderr byte-for-byte, control sequences
  included. What the child put on its terminal is what you get.

## 3. When it is emitted — the three windows

A dump happens on exactly three exit windows, each pinned per path. They are
mutually exclusive — **at most one dump per invocation** — because each is a
distinct way the event loop can end:

| Window | Guard | Reason string in the dump header |
|---|---|---|
| A watchdog deadline fired — the PTY first-output, stream-json first-output, overall `--timeout`, or stop-hook deadline | none: **unconditional**, even when the prompt was already injected — a mid-session stall is precisely what the stop-hook deadline catches | the deadline's own description (the four `TimeoutType::description()` strings, e.g. `Stop hook did not fire within deadline after prompt injection …`) |
| The child exited without a Stop payload, before the prompt was injected | `!prompt_injected` | `child exited before prompt was injected` |
| SIGINT/SIGTERM ended the run before the prompt was injected | `!prompt_injected` | `interrupted before prompt was injected` |

The closed complement — exits that never dump, even with the flag set:

- **Any successful run** (Stop received, transcript read) and any error
  raised *after* a normal Stop (transcript parse failures, `is_error`
  results): the session worked; its output is not diagnostic material.
- **The EC-7 identity-leak exit** (a Stop payload arrived before any prompt
  was injected): the payload is untrustworthy and the run fails fast — its
  output is not surfaced.
- **The trust-dialog refusal exit** (`TrustDialogUnresolved`): that path has
  its own actionable diagnostic naming `--pretrust-cwd`; the raw output adds
  nothing.

And one boundary that applies inside the windows themselves: **an empty
capture dumps nothing.** A child that stalled without ever writing a byte
(the silent-binary wedge) produces the deadline diagnostics but no block —
there is literally nothing to show, and the `dump` renderer no-ops rather
than printing an empty fence.

## 4. Where it is emitted — stderr, in every mode

The dump goes to **claude-print's own stderr**, always, in all three output
formats. Stdout never carries dump bytes in any mode. The block is:

```
claude-print: ----- child PTY output (<reason>, <N> bytes) -----
<the captured tail, raw bytes>
claude-print: ----- end child output -----
```

- `<reason>` is the §3 reason string; `<N>` is the kept tail's byte count
  (≤ 65536). An LF is appended between the tail and the end marker when the
  tail does not already end with one — and never doubled when it does.
- **Ordering.** The dump lands after the run's own `claude-print:`-prefixed
  diagnostics and before the mode-shaped error report main emits:
  - stateless timeout: `claude-print: <deadline description>` →
    `claude-print: sending SIGTERM to child pid <pid>` → the dump →
    `error: operation timed out` (text) or the structured error object;
  - pooled timeout: the same minus the `sending SIGTERM` line — the worker
    is the daemon's, no direct signal is sent or claimed;
  - pre-injection child death: the dump → `error: claude exited before Stop
    hook fired` (exit 2);
  - pre-injection interrupt: the dump → the `interrupted` error shape
    (exit 130).
- **Mode routing of everything else is unchanged.** The dump is one more
  stderr citizen in the routing rule the compat contract §3 states ("stdout
  carries the payload only, and everything human … goes to stderr"); the
  error report that follows it routes per
  `docs/notes/output-format-contracts.md` — `error: …` on stderr in text
  mode, the one-line `result` object with `subtype: "timeout"` on **stdout**
  in json mode and in stream-json mode after prompt injection. The practical
  consequence for a JSON caller: stdout parses exactly as before, and the
  diagnosis sits beside the process's stderr, not interleaved into the
  payload stream.
- **Independent of `--verbose`.** The dump does not require `--verbose` and
  verbose traces do not require the flag; both write stderr and neither
  gates the other.

## 5. Pooled and fallback runs

- **The flag applies on pooled runs.** Of the session-side launch options it
  is one of the two (`--verbose` is the other) carried into the pooled
  driver `Session::run_pooled`; it is deliberately absent from main's
  `not applied:` inapplicability diagnostic, which exists for
  per-invocation child-*launch* flags (`--model`, `--mcp-config`,
  `--pretrust-cwd`, …) that cannot reach an already-running worker. This
  flag has no launch-side meaning to miss — it configures the client's own
  capture.
- **The capture window is the client's invocation.** The pooled capture is
  created inside the pooled driver, after acquisition. The daemon's warmup —
  trust dismissal, the worker's init burst — happened before the client
  existed, and the daemon itself has no capture (`serve` has no such flag),
  so warmup bytes are never in a client's dump. A pooled dump shows the
  drive-time PTY stream: typically the echo of the injected prompt and
  whatever the worker emitted during the invocation.
- **The watchdog window is identical.** The pooled driver runs the same
  four deadlines with the same descriptions, and its timeout arm dumps the
  same block (§4's ordering row notes the one cosmetic difference: no
  `sending SIGTERM` line). The pre-injection child-death and interrupt
  windows also dump, guarded by the same `!prompt_injected` predicate.
- **Fallback runs are stateless runs.** When pool acquisition falls back —
  socket absent, stale, pool exhausted — exactly one verbose diagnostic
  says so, and the ordinary stateless session runs with this flag's
  semantics unchanged (§3, §4): the fallback's own fresh child is captured,
  SIGTERM line and all.

## 6. Scope limits

- **claude-print-local.** The flag never reaches the child's argv (the
  compat contract's `never` class) and has no effect on the child's
  environment. The child cannot detect it.
- **Inert wherever no session runs.** `--help`, `--version`, `--check`
  (with or without `--clean`), and `serve` accept the parser surface but no
  capture exists on those paths — there is no session to observe.
- **Bounded and once.** One dump per invocation at most (§3), the block is
  the ≤ 64 KiB tail plus its two marker lines, and nothing is re-dumped,
  retried, or written anywhere else — no file, no log, no transcript.

## 7. How the contract is pinned

`tests/show_child_stderr_contract.rs` holds each section against the
implementation:

1. **Source pins (§1–§3, §6)** — the flag is a real `Cli` field wired into
   `LaunchOptions`; both event-loop closures feed the capture on every
   non-empty chunk; there are exactly six dump sites — the three windows ×
   the stateless and pooled drivers; the timeout dump is unconditional
   while the other two sit behind the `!prompt_injected` gate; the disabled
   capture never allocates; the `dump_to` header/end-marker template, the
   two pre-injection reason literals, and the 64 KiB cap are quoted against
   this document; `run_pooled` takes the flag and main's pooled
   inapplicability list does not name it; the README row and the compat
   contract's §1/§3 mentions stay linked to this note.
2. **Default off (§1, §4)** — a stateless run held past the stop-hook
   deadline without the flag exits 124 with the deadline and SIGTERM
   diagnostics and no dump block.
3. **Enabled × the three modes (§4)** — the same stalled run with the flag:
   text mode keeps stdout empty and puts the deadline reason, the captured
   dialog bytes, and the end marker on stderr; json mode's stdout is
   exactly the one-line `timeout` error object with the block on stderr;
   stream-json mode forwards the synthesized error result on stdout with
   the block on stderr.
4. **Empty capture (§3)** — a silent child past the first-output deadline
   prints the deadline diagnostics but no block, even enabled.
5. **Pre-injection child death (§3, §4)** — a child that renders its trust
   dialog and then exits before injection dumps the startup bytes under the
   `child exited before prompt was injected` reason, in an A/B against the
   identical flag-off run; both exit 2 with `claude exited before Stop hook
   fired`.
6. **Pooled (§5)** — a daemon-driven worker held past the stop-hook
   deadline dumps the drive-time stream (the injected prompt's echo, never
   warmup bytes such as the trust dialog) with the deadline reason, no
   `sending SIGTERM` line, and the verbose `driving prewarmed worker` trace
   proving the pooled path.
7. **Fallback (§5)** — an absent socket produces the one fallback
   diagnostic and then the full stateless dump, SIGTERM line included.

The ring-buffer mechanics themselves (feed no-ops, accumulation, eviction,
and the `dump_to` renderings into a buffer) are unit-pinned in
`src/session.rs`'s `#[cfg(test)]` module.
