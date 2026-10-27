# Claude Code Runtime Contract Probes — measured behavior

**Measured against:** `claude` 2.1.311 (`2.1.311 (Claude Code)`), 2026-10-27, on the
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
in §Stop firing contract below). Evidence tables below cite the 2.1.311
re-measurement — the same version the **Measured against:** stamp and the
active fixtures pin — so the doc body upholds the one-pin invariant the gate
enforces on the fixtures. Since claudepr-893bfc4e (2026-09-30) that upholding
is mechanical, not by-hand: the detection step parses the stamp, every
evidence-table row, and every `Evidence (…)` preamble out of this markdown
and fails (exit 2) on any cited version that is neither the active pin nor
explicitly marked historical on the same line — so the fixtures-move/
prose-lags shape of an incomplete re-pin (the reverted claudepr-2e8c3884)
fails CI instead of surviving to review. The two 2.1.270 measurements the
2.1.281/2.1.282 re-pins had left unrepeated — the sleeping-hook
concurrency probe and the
degraded-path extra Stop — were re-measured against that same pinned 2.1.282
on 2026-09-25 (claudepr-d9553d38, `scripts/probe-stop-edge-contracts.sh`,
which is now part of the probe set): concurrent cross-source execution
reproduced in **12/12** event pairs and the extra-Stop hazard **did not
reproduce** (15/15 permission-denied runs across three invocations fired
exactly one Stop). The relay-hook timeout-enforcement contract (Arm T of the
same script, added 2026-09-26, claudepr-352cf1df) was likewise measured
against that same pinned 2.1.282 — §Hook-timeout enforcement below — so every
contract this document states is backed by a measurement of the pinned
version. The move to 2.1.289 (claudepr-9e054b3d, 2026-10-04) re-ran the full
four-probe set under the version guard against the pinned
`~/.local/share/claude/versions/2.1.289` binary. Merge, source suppression,
Stop counts, the max-turns cutoff, cross-source concurrency, degraded-path
counts, and hook-timeout enforcement matched the prior pin. A sandboxed,
authenticated claude-print stream-json PTY capture in the same pinned session
produced 40 records; stdout was byte-identical to the transcript, with no
`result`, CR, or blank lines. Its assistant tool-use record carried
`wireToolInputs`. The prior 2.1.288 measurement is retained as historical
fixture evidence, as are the 2.1.287 and earlier captures. The
only
2.1.270 numbers still quoted below — the superseded TUI timings kept for
per-version contrast — carry an explicit historical attribution where they
appear; inside the evidence tables that attribution is the word `historical`
on the same line as the superseded number, which is exactly what the
detection step's prose check accepts for a non-pin citation. Per-version
fixtures:
`tests/fixtures/claude_contracts_v2.1.281.json`,
`tests/fixtures/claude_contracts_v2.1.282.json`,
`tests/fixtures/claude_contracts_v2.1.283.json`,
`tests/fixtures/claude_contracts_v2.1.284.json`,
historical `tests/fixtures/claude_contracts_v2.1.285.json`,
`tests/fixtures/claude_contracts_v2.1.286.json`,
`tests/fixtures/claude_contracts_v2.1.287.json`, and
`tests/fixtures/claude_contracts_v2.1.288.json` (prior same-host runs,
retained), `tests/fixtures/claude_contracts_v2.1.289.json` (retained),
`tests/fixtures/claude_contracts_v2.1.290.json` (the prior pin), and
`tests/fixtures/claude_contracts_v2.1.292.json` (the prior pin) and
`tests/fixtures/claude_contracts_v2.1.294.json` (the prior pin), and
`tests/fixtures/claude_contracts_v2.1.296.json` (retained) and
historical `tests/fixtures/claude_contracts_v2.1.299.json` and
`tests/fixtures/claude_contracts_v2.1.300.json` and
`tests/fixtures/claude_contracts_v2.1.301.json` (retained) and
historical `tests/fixtures/claude_contracts_v2.1.302.json` and
`tests/fixtures/claude_contracts_v2.1.303.json` (prior pins; the 2.1.303 pin
was measured 2026-10-19 by claudepr-9bdddad9 and landed 2026-10-22 as history
when claudepr-f664c5cd re-pinned past it),
and `tests/fixtures/claude_contracts_v2.1.307.json` and
`tests/fixtures/claude_contracts_v2.1.309.json` (prior pins; the 2.1.309 pin
was measured 2026-10-24 by claudepr-ab96e84a and landed as history when
claudepr-5bd6732c re-pinned past it; the 2.1.308 measurement of 2026-10-23
never landed a pin — the host auto-updated again before its re-pin commit,
and the split children of claudepr-2f73a845 re-measured on 2.1.309) and
`tests/fixtures/claude_contracts_v2.1.311.json` (the active pin, measured
2026-10-27 by claudepr-88e4ceed).

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
beside it as measurement history. On 2026-10-02 (claudepr-cb9dcf25), one
authenticated sandboxed PTY session through claude-print itself re-verified
the PTY transcript shape against 2.1.288: stdout contained 34 records, no
`result` record, compact JSON, no CR/blank lines, and the production
stream-json replay came out **byte-identical** to the transcript file. The
capture carried the newer `atis-latch`, `file-history-snapshot`, and
attachment records; the reader stayed type-agnostic. The regenerated
`stream_json_golden_v2.1.288` triple came out of the same capture path; the
`v2.1.288` and `v2.1.282` families are retained as history. On 2026-10-04
(claudepr-9e054b3d), a same-session 2.1.289 capture completed with 40 records
and remained byte-identical through the production stream-json replay. Its
assistant tool-use envelope carried `wireToolInputs`; no `thinking` block
occurred in this capture, so the golden keeps that block only as a retained
type-agnostic forwarding case from earlier live captures. The regenerated
`stream_json_golden_v2.1.289` family was derived from the sanitized captured
records, then replayed through the real reader and `emit_error` path.

On 2026-10-06 (claudepr-c02f9cfc), all four guarded probes ran against the
concrete 2.1.290 binary. Merge, source suppression, single-turn and
multi-round Stop counts, the TUI second-turn Stop, the `--max-turns` cutoff,
cross-source concurrency, the denied-tool Stop count, and hook-timeout
enforcement matched the prior active evidence. Arm D now installs a
command-specific Bash deny rule and verifies an errored Bash tool result in
each transcript; 2.1.290 otherwise permitted the unlisted probe command, so
the old implicit-denial setup did not establish the degraded path. The new
Arm S sample was concurrent in 12/12 event pairs, with relay-first in 3/12;
Arm D completed 5/5 denied runs with one Stop each; Arm T killed all 8
overrun firings and all 4 sessions completed at the 5 s timeout. A same-session
sandboxed claude-print stream-json capture contained 40 records, had no
`result`, CR, or blank records, and replayed byte-identically to the
transcript. Its tool-use record carried `wireToolInputs`; the regenerated
2.1.290 golden family was sanitized from that capture and replayed through the
real reader and `emit_error` path.

On 2026-10-07 (claudepr-1558810f), all four guarded probes ran against the
concrete 2.1.292 binary. Merge, source suppression, single-turn and
multi-round Stop counts, the TUI second-turn Stop, the `--max-turns` cutoff,
cross-source concurrency, the denied-tool Stop count, and hook-timeout
enforcement matched the prior active evidence. Arm S was concurrent in 12/12
event pairs, with relay-first in 5/12; Arm D completed 5/5 denied runs with
one Stop each; Arm T killed all 8 overrun firings and all 4 sessions completed
at the 5 s timeout. A same-session sandboxed claude-print stream-json capture
contained 39 records, had no `result`, CR, or blank records, and replayed
byte-identically to the transcript. Its assistant tool-use record carried
`wireToolInputs`; the regenerated 2.1.292 golden family was sanitized from
that capture and replayed through the real reader and `emit_error` path.

On 2026-10-08 (claudepr-49259380), all four guarded probes ran against the
concrete 2.1.294 binary. Merge, source suppression, single-turn and
multi-round Stop counts, the TUI second-turn Stop, the `--max-turns` cutoff,
cross-source concurrency, denied-path counts, and hook-timeout enforcement
matched the prior pin. Arm S was concurrent in 12/12 event pairs, with
relay-first in 5/12; Arm D completed 5/5 denied runs with one Stop each; Arm T
killed all 8 overrun firings while all 4 sessions completed at the 5 s
timeout. The dedicated TUI probe observed one Stop per turn at 1.5 s / 1.5 s.
A same-session authenticated claude-print stream-json capture contained 41
records, had no `result`, CR, or blank records, and replayed byte-identically
to the transcript. Its tool-use record carried `wireToolInputs`, and its
tool-result record carried `permissionDecision`; the sanitized 2.1.294 golden
family was replayed through the real reader and `emit_error` path.

On 2026-10-09 (claudepr-0548e0e4), all four guarded probes ran against the
concrete 2.1.296 binary and each ended `version-guard: verdict=single-version
start=2.1.296 end=2.1.296`. Merge, source suppression, single-turn and
multi-round Stop counts, the TUI second-turn Stop, the `--max-turns` cutoff,
cross-source concurrency, denied-path counts, and hook-timeout enforcement
matched the prior pin. Arm S was concurrent in 12/12 event pairs, with
relay-first in 4/12; Arm D completed 5/5 denied runs with one Stop each; Arm T
killed all 8 overrun firings while all 4 sessions completed with replies.
The timeout-to-exit interval averaged 5.2 s across a 5.0–5.3 s range. The
dedicated TUI probe observed one Stop per turn at 1.5 s / 1.2 s. Arm P was
rerun with `--dangerously-skip-permissions` after the allowed-tools-only
attempt was blocked by the auto-mode classifier; the rerun completed both
sequential Bash rounds and fired one Stop. A same-session authenticated
claude-print stream-json capture contained 41 records, had no `result`, CR,
or blank records, and stdout was byte-identical to the transcript. Its
assistant tool-use record carried `wireToolInputs`, its tool-result record
carried `permissionDecision`, and a thinking block appeared. The new sanitized
2.1.296 golden family was generated from representative records in that
capture, replayed through the real reader, and paired with error bytes from
the real `emit_error` path.

On 2026-10-15 (claudepr-05a74d6c), all four guarded probes ran against the
concrete 2.1.300 binary and ended `version-guard: verdict=single-version
start=2.1.300 end=2.1.300` (historical). Merge, source suppression, one Stop
per completed turn, the `--max-turns` cutoff, cross-source concurrency,
denied-path Stop counts, and hook-timeout enforcement matched the prior pin.
On 2026-10-16 (claudepr-7b323468), all four guarded probes ran against the
concrete 2.1.301 binary and ended `version-guard: verdict=single-version
start=2.1.301 end=2.1.301` (historical). Every contract matched the prior
pin again. Arm S was concurrent in 12/12 event pairs, with relay-first in
3/12; Arm D completed 5/5 denied runs with one Stop each and an errored Bash
result; Arm T killed all 8 overrun firings while all 4 sessions completed
with replies at 5.1 s after Stop-hook start. The dedicated TUI probe observed
one Stop per turn at 13.2 s / 12.6 s, and probe-claude-contracts T2 turn 1
(multi-round tool use, two loaded sources) fired two Stops — one per loaded
source. The allowed-tool TUI arm of probe-stop-toolallowed did not complete
its turns inside the driver window this time (205.1 s each, zero firings, no
rendered reply — an incomplete-turn re-run shape, not a
rendered-reply-without-Stop finding; its Arm P print-mode contract passed:
one Stop, DONE, exit 0). A same-session authenticated claude-print
stream-json capture contained 30 records, had no `result`, CR, or blank
records, and stdout was byte-identical to the transcript. The `date` and
`remote_session_change` attachment types and a `turn_duration` record were
forwarded; the sampled assistant record carried `requestRef`, and no
tool-use or thinking record appeared in this capture.
On 2026-10-17 (claudepr-fa11b4f7), all four guarded probes ran against the
concrete 2.1.302 binary and ended `version-guard: verdict=single-version
start=2.1.302 end=2.1.302` (historical). Every contract matched the prior pin
again. Arm S
was concurrent in 12/12 event pairs, with relay-first in 6/12; Arm D
completed 5/5 denied runs with one Stop each and an errored Bash result; Arm
T killed all 8 overrun firings while all 4 sessions completed with replies at
5.1 s after Stop-hook start. The dedicated TUI probe observed one Stop per
turn at 4.6 s / 3.6 s, and probe-claude-contracts T2 turn 1 (multi-round tool
use, two loaded sources) fired two Stops — one per loaded source. The
allowed-tool TUI arm of probe-stop-toolallowed completed both turns this time
(34.0 s multi-round, 27.0 s plain reply, one Stop each) rather than timing
out as it did on the prior pin; its Arm P print-mode contract passed: one
Stop, DONE, exit 0. A same-session authenticated claude-print stream-json
capture contained 39 records, had no `result`, CR, or blank records, and
stdout was byte-identical to the transcript. The `date` and
`remote_session_change` attachment types and a `turn_duration` record were
forwarded again; the sampled assistant record carried `requestRef`, and no
tool-use or thinking record appeared in this capture. The new sanitized
2.1.302 golden family uses
representatives from this capture and retains the older thinking/split case
with `wireToolInputs`.
Its expected bytes and error objects were checked through the production
reader and emitter.
On 2026-10-19 (claudepr-9bdddad9), all four guarded probes ran against the
concrete 2.1.303 binary and ended `version-guard: verdict=single-version
start=2.1.303 end=2.1.303` (historical). Every contract matched the prior pin
again. Arm S
was concurrent in 12/12 event pairs, with relay-first in 5/12; Arm D
completed 5/5 denied runs with one Stop each and an errored Bash result; Arm
T killed all 8 overrun firings while all 4 sessions completed with replies at
5.1 s after Stop-hook start. The dedicated TUI probe observed one Stop per
turn at 4.8 s / 6.0 s, and probe-claude-contracts T2 turn 1 (multi-round tool
use, two loaded sources) fired two Stops — one per loaded source; its turn 2
did not complete inside the driver window (270.2 s, zero firings — the
incomplete-turn re-run shape, not a rendered-reply-without-Stop finding).
The allowed-tool TUI arm of probe-stop-toolallowed likewise produced zero
firings inside its driver window (as on the 2.1.301 pin, unlike 2.1.302,
which completed both turns); a re-run reproduced the shape, and its Arm P
print-mode contract passed twice: one Stop, DONE, exit 0 both times. The
per-turn TUI evidence for this pin is the dedicated probe.
probe-claude-contracts T1 (multi-round, print mode, exit 0) fired exactly one
own Stop (the probe's per-arm print is the shared log's cumulative project
count, 4, of which P1/P2/P6 own one each), and the T3 cutoff exited 1 with
zero own firings. A same-session authenticated claude-print stream-json
capture contained 29 records, had no `result`, CR, or blank records, and
stdout was byte-identical to the transcript. The `date` and
`remote_session_change` attachment types and `turn_duration` and
`stop_hook_summary` system records were forwarded again; an `auto_mode`
attachment was newly observed; the sampled assistant record carried
`requestRef`, and no tool-use or thinking record appeared in this capture —
and no `ai-title` record either, so the ai-title case is retained from the
2.1.302 live evidence as a historical forwarding case. The new sanitized
2.1.303 golden family uses representatives from this capture, retains that
ai-title case and the older thinking/split case with `wireToolInputs`, and
keeps the blank/spaced-JSON/CRLF framing cases.
Its expected bytes and error objects were produced through the production
reader and emitter.
On 2026-10-22 (claudepr-f664c5cd — the pin that also landed the 2.1.303
measurement above as history, its own re-pin having been superseded before it
committed), all four guarded probes ran against the
concrete 2.1.307 binary and ended `version-guard: verdict=single-version
start=2.1.307 end=2.1.307`. Every contract matched the prior pin again. Arm S
was concurrent in 12/12 event pairs, with relay-first in 3/12. Arm D needed
three invocations: 10 of 15 denied-scenario runs verified an errored Bash
result, every completed run fired exactly one Stop, and the five unverified
runs were model-declined attempts ("no Bash tool is available in this
session" — consistent with the `deferred_tools_delta` attachment this version
newly emits); the shape costs the probe scenario coverage, not a Stop-count
claim. Arm T killed all 8 overrun firings while all 4 sessions completed with
replies at 5.1–5.2 s after Stop-hook start. The dedicated TUI probe observed
one Stop per turn at 9.4 s / 2.8 s, and probe-claude-contracts T2 turn 1
(multi-round tool use, two loaded sources) fired two Stops — one per loaded
source; its turn 2 did not complete inside the driver window (zero firings —
the incomplete-turn re-run shape, not a rendered-reply-without-Stop finding).
The allowed-tool TUI arm of probe-stop-toolallowed likewise produced zero
firings inside its driver window; its Arm P print-mode contract passed: one
Stop, DONE, exit 0. The per-turn TUI evidence for this pin is the dedicated
probe. probe-claude-contracts T1 (multi-round, print mode, exit 0) fired
exactly one own Stop, and the T3 cutoff exited 1 with zero own firings. A
same-session authenticated claude-print stream-json capture contained 32
records, had no `result`, CR, or blank records, and stdout was
byte-identical to the transcript. A `deferred_tools_delta` attachment was
newly observed, and `ai-title` records occurred live again (twice, as on
2.1.302; the 2.1.303 capture had none); `auto_mode`, `date`, and
`remote_session_change` attachments and `turn_duration` and
`stop_hook_summary` system records were forwarded again; the sampled
assistant record carried `requestRef`, and no tool-use or thinking record
appeared in this capture. The new sanitized
2.1.307 golden family uses representatives from this capture, retains the
older thinking/split case with `wireToolInputs`, and
keeps the blank/spaced-JSON/CRLF framing cases.
Its expected bytes and error objects were produced through the production
reader and emitter.
On 2026-10-24 (claudepr-ab96e84a, the re-pin child of claudepr-2f73a845's
auto-split), all four guarded probes ran against the concrete 2.1.309 binary
and ended `version-guard: verdict=single-version start=2.1.309 end=2.1.309`.
Every contract matched the prior pin again. Arm S was concurrent in 24/24
event pairs across two invocations, with relay-first in 6/24 (5/12 then
1/12). Arm D needed two invocations: 5 of 10 denied-scenario runs verified an
errored Bash result, every completed run fired exactly one Stop, and the five
unverified runs were model-declined attempts ("Bash tool isn't available in
this session" — the same declined-attempt shape as the prior pin); the shape
costs the probe scenario coverage, not a Stop-count claim. Arm T killed all
16 overrun firings (8 per invocation) while all 8 sessions completed with
replies at 5.1 s after Stop-hook start. The dedicated TUI probe observed one
Stop per turn at 3.6 s / 10.3 s, and probe-claude-contracts T2 (multi-round
tool use, two loaded sources) completed both its turns inside the driver
window, firing two Stops per turn — one per loaded source. The allowed-tool
TUI arm of probe-stop-toolallowed completed both its prompts at one Stop each
(52.5 s / 27.0 s, both replies rendered; the first reply was a model-declined
attempt — a coverage note, not a Stop-count deviation); its Arm P print-mode
contract passed: one Stop, DONE, exit 0. probe-claude-contracts T1
(multi-round, print mode, exit 0) fired exactly one own Stop, and the T3
cutoff exited 1 with zero own firings. A same-session authenticated
claude-print stream-json capture contained 34 records, had no `result`, CR,
or blank records, and stdout was byte-identical to the transcript. The
envelope set kept the standing attachments (`ai-title` twice again;
`deferred_tools_delta` standing) and newly observed a live
`assistant:thinking` record (the 2.1.307 capture had none), the
`api-request-shape`/`api-request-blob`/`api-request` request-observability
trio, `attachment:prompt_snapshot` carrying tool schema objects (formerly
bare tool names), `agent_listing_delta` with `builtInPluginTypes`,
`auto_mode` with `finishExposedWork`, and `skill_listing` with `digests`;
`session_context.context` arrived empty (the capture cwd was not a git repo,
so a `gitStatus` key-move is indistinguishable from a cwd-conditional
omission); no tool-use record appeared. The new sanitized
2.1.309 golden family uses representatives from this capture (a live thinking
record as the representative), retains the
older thinking/split case with `wireToolInputs`, and
keeps the blank/spaced-JSON/CRLF framing cases.
Its expected bytes and error objects were produced through the production
reader and emitter. The 2.1.308 measurement of 2026-10-23 (same parent bead,
probes against the then-pinned 2.1.308 binary) had found the identical
contracts — concurrent 24/24 with relay-first 4/24, Arm D 8 of 10 verified,
Arm T 16/16 at 5.1 s, TUI 10.1 s / 6.7 s — but its re-pin commit never
landed: the host auto-updated to 2.1.309 first, and this 2.1.309
re-measurement supersedes it.

On 2026-10-27 (claudepr-88e4ceed, the scheduled drift-watch bead), all four
guarded probes ran against the concrete 2.1.311 binary and ended
`version-guard: verdict=single-version start=2.1.311 end=2.1.311`. Every
contract matched the prior pin again. Arm S was concurrent in 12/12 event
pairs, with relay-first in 1/12. Arm D completed 5/5 explicit-deny runs with
an errored Bash result, every run exited 0 and fired exactly one Stop. Arm T
killed all 8 overrun firings while all 4 sessions completed with replies at
5.1 s after Stop-hook start. The dedicated TUI probe observed one Stop per
turn at 2.2 s / 2.1 s, and probe-claude-contracts T2 (multi-round tool use,
two loaded sources) completed both turns with two Stops per turn — one per
loaded source. The allowed-tool TUI arm of probe-stop-toolallowed completed
both prompts at one Stop each (31.5 s / 26.5 s, both replies rendered); its
Arm P print-mode contract passed: one Stop, DONE, exit 0.
probe-claude-contracts T1 (multi-round, print mode, exit 0) fired exactly
one own Stop, and the T3 cutoff exited 1 with zero own firings. One
same-session authenticated claude-print stream-json capture (37 records)
had no `result`, CR, or blank records, and stdout was byte-identical to its
transcript. The envelope set matched the prior pin; the live capture carried
the `api-request-shape`/`api-request-blob`/`api-request`
request-observability trio, `attachment:prompt_snapshot` carrying tool
schema objects, `agent_listing_delta` with `builtInPluginTypes`, `auto_mode`
with `finishExposedWork`, and `skill_listing` with `digests`; `ai-title`
records occurred live again (twice in the capture) and
`deferred_tools_delta` stayed in the standing attachment set;
`session_context.context` arrived empty (the capture cwd was not a git
repo); no tool-use or thinking record appeared. The new sanitized 2.1.311 golden family
uses representatives from this capture, retains the older thinking/split case with
`wireToolInputs`, and keeps the blank/spaced-JSON/CRLF framing cases. Its
expected bytes and error objects were produced through the production reader
and emitter.

**Isolation:** every probe ran with `HOME` redirected into a throwaway
`mktemp` sandbox (fresh `.claude.json`, trust pre-seeded for the probe cwd
only). The host's real `~/.claude/settings.json` and `~/.claude.json` were
never read, copied, or modified. `~/.claude/.credentials.json`, when present,
was copied file-to-file into the throwaway sandbox; credential contents were
never printed. Probe hooks wrote only inside the sandbox; evidence below is
timestamps, source tags, and event names — no payload contents.

**Probe wiring:** two hook sources installed the same pair of hooks
(`SessionStart` + `Stop`), each appending `timestamp|tag|event` to one shared
log: a **project source** (`.claude/settings.json` in the probe cwd) and a
**`--settings` file** (the "relay" — what claude-print passes). Where a user
source was exercised, the sandbox `~/.claude/settings.json` carried the same
wiring. The settings schema used was claude-print's double-nested
`hooks.Stop[ { hooks: [ {type: "command", ...} ] } ]` form — accepted and
fired by every measured version (2.1.270, 2.1.281, 2.1.282, 2.1.285, 2.1.286, 2.1.287, 2.1.288, 2.1.289, 2.1.290, 2.1.292, 2.1.294, 2.1.296, 2.1.300, 2.1.301, 2.1.302, 2.1.303, 2.1.307, 2.1.308, 2.1.309, 2.1.311), which also
live-verifies the Hook Installer §2 schema note.

## PO-1 / OQ-1 — `--settings` merge: CONFIRMED; firing order: NOT CONTRACTUAL

Evidence (2.1.311; P2: project source + `--settings`; P6: user source +
`--settings`; each a real completed `claude -p` turn):

| Run | Event | Firings in order |
|---|---|---|
| P2 | SessionStart | project → relay |
| P2 | Stop | relay → project |
| P6 | SessionStart | user → relay-settings (project also loaded; its position is not printed by the probe's `user|relay-settings` log filter) |
| P6 | Stop | user → relay-settings (project also loaded; position not printed) |

- `--settings <file>` hooks fire **alongside** standard-source hooks — merge,
  not replace. Every loaded source fired on every event. PO-1's
  in-process-merge fallback is not needed.
- **Order across sources is not a contract.** On 2.1.311 the ordinary pairs
  showed every loaded source firing in each event (P2's SessionStart and Stop
  pairs 2–3 ms apart; P6 one Stop per source), with start order varying
  between the runs and events. The dedicated
  sleeping-hook arm observed concurrent starts and
  start-order flips directly. The 2.1.282 run of the same pairs had P2's SessionStart
  pair relay-first, and the same-day 2.1.281 run fired standard-source-first
  throughout; the two prior pins (both historical) split the same way — one
  relay-first on P6's SessionStart, the next project-first —
  which order materializes is run-dependent, not a property of
  the source or the version. The dedicated sleeping-hook probe (project
  hook sleeping 300 ms and logging start/end; relay hook sleeping 0 ms —
  `scripts/probe-stop-edge-contracts.sh` Arm S) supplies the direct
  concurrency proof a timestamp pair alone cannot. Re-measured against the
  pinned 2.1.311 on 2026-10-27 (claudepr-88e4ceed; 6 runs × 2 event pairs
  across two invocations): the two hooks ran **concurrently in 12 of 12
  pairs** — the relay always started while the project hook's sleep was
  still running (project wall time exceeded 300 ms across the firings, so
  the sleep demonstrably executed), and
  the relay **started before** the project hook in **1 of 12 pairs** (1 of 6
  in the first invocation, 0 of 6 in the second). The prior pin 2.1.309
  (claudepr-ab96e84a, 2026-10-24, historical), the 2.1.308 run of 2026-10-23
  (claudepr-2f73a845, historical — its re-pin never landed), the prior pin
  2.1.307 (claudepr-f664c5cd, 2026-10-22, historical),
  the one before it (claudepr-9bdddad9, 2026-10-19, historical) and the
  pinned 2.1.282
  run of 2026-09-25 (claudepr-d9553d38) with its two extra same-day
  invocations had measured the same shape — concurrent 24/24 and 12 of 12 each, flips
  5/12, 1/12, 4/24, 3/12, 6/12, 4/12 and 5/12 — and an earlier drift-window invocation had measured
  12 of 12 concurrent with 7 of 12 flips, so the flip rate itself is unstable.
  That reproduces the original 2.1.270 finding
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
version (2.1.270 through 2.1.311); do not adopt it.
  (Side observation: `--allowedTools` consumes a variadic value list — pass it
  as `--allowedTools=Bash` or it will swallow a following positional prompt.)

## Hook-timeout enforcement — an overrun relay hook is killed; the session proceeds

`docs/notes/hook-design.md` §Relay Hook configures both relay hooks with
`"timeout": 10` and states Claude Code "does not wait beyond the 10s timeout"
— a claim the merge/suppression/Stop pins above did not cover. Measured
directly against the pinned 2.1.311 on 2026-10-27 (claudepr-88e4ceed; Arm T
of `scripts/probe-stop-edge-contracts.sh`, added 2026-09-26,
claudepr-352cf1df, re-run natively here in the same pinned session as the
rest of the re-pin — the contract's first measurement, against the pinned
2.1.282, had needed the versions-dir PATH shim because the host had already
drifted to 2.1.283 — §Reproducing): the relay-position hook (a `--settings`
file, run with `--setting-sources=` so it is the only loaded hook —
claude-print's isolation-mode shape) was configured with a per-hook
`"timeout": 5` while its script slept 30 s, wired on SessionStart and Stop,
across 8 real single-turn runs (4 per invocation):

| Signal | Result (2.1.311, 2026-10-27) |
|---|---|
| Hook killed before its sleep finished — `end` line never logged after `start` | **8/8** event firings (4 SessionStart + 4 Stop) |
| Session proceeds — claude exit code | **4/4** runs exit 0 |
| Session proceeds — reply rendered | 4/4 runs (`reply-contains-OK` 4/4 per invocation) |
| claude process exit − Stop hook start | **5.1 s** in each of 4 runs (4/4) — the configured 5 s timeout, not the 30 s sleep |

The prior 2.1.309 run (2026-10-24, claudepr-ab96e84a, historical), the
2.1.308 run of 2026-10-23 (claudepr-2f73a845, historical), the
2.1.307 run (2026-10-22, claudepr-f664c5cd, historical), the
2.1.302 run (2026-10-17, claudepr-fa11b4f7, historical) and the
2.1.301 run (2026-10-16, claudepr-7b323468, historical) had each produced the
identical shape — 8 of 8 firings killed, 4 of 4 sessions exit 0 with replies
rendered, and the process exiting at 5.1 s after the Stop hook start in every
run. The earlier 2.1.282 shim measurement (two invocations, 8 runs) had also
produced the identical shape — 16 of 16 firings killed, 8 of 8 sessions exit
0 with replies rendered, `reply-contains-OK` 4 of 4 per invocation, and the
process exiting at the configured 5.0 s after the Stop hook start in every
run — so the enforcement has held unchanged across every version move
measured.

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
fired **exactly one Stop per loaded source** (e.g. 2.1.311 P6: user, project,
and relay Stop hooks each fired once; P3: relay Stop only). No run produced
zero Stops; no run produced more than one Stop per source — re-verified
unchanged on 2.1.281, 2.1.282, 2.1.287, 2.1.288, 2.1.289, 2.1.290, 2.1.292,
2.1.296, 2.1.300, 2.1.301, 2.1.302, 2.1.303, 2.1.307, 2.1.308, 2.1.309, and 2.1.311, whose fixtures pin the count at 1 per loaded source.

## Stop firing contract — multi-round tool use, TUI turns, cutoffs

The first-round probes (T1/T2 in `probe-claude-contracts.sh`) ran without an
explicit tool allowlist. On older versions, Bash calls in that shape were
permission-denied; 2.1.270 produced one **extra** Stop firing (2 for one
prompt; one of the two degraded runs in that historical sample, kept as the
hazard note below). On the prior pin, the missing
allowlist alone did not deny Bash, so those counts do not establish the
degraded-path contract. The completed-turn contract was measured with tools
actually permitted
(`scripts/probe-stop-toolallowed.sh`, `scripts/probe-tui-second-turn.sh`);
counts and timings below are the 2.1.311 run — the active pin. Arm D now
installs a command-specific Bash deny rule and verifies an errored Bash result
in each transcript. Its 2.1.311 re-measurement needed two invocations —
7 of 10 denied-scenario runs verified an errored Bash result, every
run (verified or not) exited 0 and fired exactly one Stop, and the
three unverified runs ended with the model declining to attempt the call
("Bash tool isn't available in this session" — the same declined-attempt
shape as the prior pin, consistent with its deferred-tools mechanism); that
shape costs the probe scenario coverage, not
a Stop-count claim: no completed run fired ≠1 Stop. See the hazard note
below the table. Measured scenarios:

| Scenario | Stop firings | Evidence |
|---|---|---|
| Single-turn `claude -p`, no tools | 1 per loaded source | P1–P6 (above) |
| `claude -p`, two sequential permitted Bash rounds (`--allowedTools=Bash`) | **1**, at the turn's true end | one firing, `last_assistant_message: "DONE"`, `stop_hook_active: false`, single session id |
| `claude -p`, tool calls permission-denied (degraded, explicit Bash deny) | **1** | Arm D (2.1.311, 2026-10-27): 5/5 denied-scenario runs verified an errored Bash result and every run fired exactly one Stop (`stop_hook_active: false`); the historical 2.1.270 hazard — one extra Stop in 1 of 2 runs — did not reproduce (the 2.1.309 run, historical, verified 5 of 10 across two invocations; the 2.1.307 run, historical, verified 10 of 15 across three invocations; the 2.1.308 run, historical, verified 8 of 10 across two) |
| TUI, turn 1 (plain reply) | **1** | firing + reply both observed (2.2 s) |
| TUI, turn 2 in the same session (plain reply) | **1** | firing + reply both observed (2.1 s); TUI status line showed `(running Stop hook)` |
| `claude -p` cut off by `--max-turns 2` mid-task | **0** | exit code 1, zero firings across the run |

*(Timings in the table above are the 2.1.311 measurement. Full 2.1.311
evidence: Arm P — `--allowedTools=Bash` completed both sequential Bash commands, final message `DONE`, exit 0, and one Stop fired,
`stop_hook_active: false`, single session id; T1 — completed (exit 0) with
exactly one firing of its own (the probe's per-arm print is the shared log's
cumulative project count); TUI turns — 1 Stop each, 2.2 s / 2.1 s
(probe-tui-second-turn, verdict "once-per-turn confirmed", both replies
rendered); cutoff
(T3) — 0 own firings, exit 1. The same-day 2.1.281 run (measured hours before
the host auto-updated mid-re-pin) recorded the same contracts (Arm T clean
at 48.0 s / 60.0 s; probe-tui-second-turn 13.0 s / 9.1 s), and the original
re-run that straddle forced, against 2.1.282, matched them (Arm P clean;
TUI turns — 1 Stop each, 62.5 s / 32.5 s (probe-1 T2, per loaded source),
60.5 s (Arm T multi-round); probe-tui-second-turn
41.6 s / 3.3 s), and the original 2.1.270 run (claudepr-6ef2541c) had
measured 22.6 s / 15.1 s for the plain TUI turns — per-version numbers
live in the fixtures. The 2.1.308 run of 2026-10-23 (claudepr-2f73a845,
historical — measured before its re-pin could land) had TUI turns — 1 Stop
each, 10.1 s / 6.7 s (probe-tui-second-turn), 48.5 s (allowed-tool TUI arm,
its second prompt), and the original 2.1.307 run (claudepr-f664c5cd,
2026-10-22, historical) had measured 9.4 s / 2.8 s for its plain TUI turns —
per-version numbers live in the fixtures. The prior 2.1.309 pin
(claudepr-ab96e84a, 2026-10-24, historical) had TUI turns — 1 Stop each,
3.6 s / 10.3 s (probe-tui-second-turn, verdict "once-per-turn confirmed",
both replies rendered), 52.5 s / 27.0 s (its allowed-tool TUI arm, both
prompts completed at one Stop each, the first reply a model-declined
attempt — a coverage note), and the original 2.1.270 degraded-run hazard
stayed unreproduced there as well — per-version numbers live in the
fixtures. The 2.1.311 allowed-tool TUI arm completed both its prompts at
one Stop each (31.5 s / 26.5 s, both replies rendered; the first reply was
a model-declined attempt — "Bash tool isn't available in this session" —
the same coverage shape, not a Stop-count deviation), like the prior pin
2.1.309 (historical) and unlike the zero-firing driver windows of 2.1.307
and 2.1.308 (both historical); the tool-using TUI turn is additionally
measured by probe-claude-contracts T2 — two Stops for two loaded sources on
each completed turn — and the dedicated
probe-tui-second-turn.sh run completed both plain turns and is the
per-turn TUI evidence for this re-pin.)*

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
  **Re-measured against the pinned 2.1.311 on 2026-10-27** (claudepr-88e4ceed,
  `scripts/probe-stop-edge-contracts.sh` Arm D): across two invocations,
  10 denied-scenario runs — 7 transcripts contained an errored Bash result,
  all 10 exited 0, and `stop_hook_active: false` on every
  firing — and **every run fired exactly one Stop**; the hazard did not
  reproduce. The three runs without an errored Bash result were model-declined
  attempts ("Bash tool isn't available in this session"), the same
  declined-attempt shape as the prior pin, consistent with its deferred-tools
  mechanism — a coverage note, not a
  Stop-count deviation. The prior pin 2.1.309 (claudepr-ab96e84a, 2026-10-24,
  historical: 5 of 10 verified across two invocations), the one before it
  2.1.307 (claudepr-f664c5cd, 2026-10-22,
  historical: 10 of 15 verified across three invocations), the 2.1.308 run
  (claudepr-2f73a845, 2026-10-23, historical: 8 of 10 verified across two
  invocations), the one before those
  (claudepr-9bdddad9, 2026-10-19,
  historical: five of five verified) and the one before that (claudepr-fa11b4f7,
  2026-10-17, historical)
  had measured the same shape, and the pinned 2.1.282 run of 2026-09-25
  (claudepr-d9553d38) had measured it on 15 of 15 completed denied runs
  across three invocations. It stays on record as
  historical: the mechanism was never explained, the reproducing sample was
  two runs, and the single-fire poller's tolerance
  (first payload → transcript retry/fallback path; the watchdog still bounds
  the session) is kept as cheap insurance. Re-measure (Arm D) if
  permission-denied paths ever become a supported mode.

(first filled in by the claudepr-6ef2541c probe run against 2.1.270; the
counts and timings above are re-cited from the 2.1.311 re-measurement — see
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
into every `cargo test` run. Since claudepr-893bfc4e the detector also
enforces the one-pin invariant on this document's prose: every version
cited on the **Measured against:** line, in a markdown table row, or in an
`Evidence (…)` preamble must be the active pin or carry the word
`historical` on the same line (the explicit attribution superseded numbers
keep) — anything else is mixed-version evidence and exits 2 before `claude`
is consulted, so the fixtures-move/prose-lags shape of an incomplete re-pin
(the reverted claudepr-2e8c3884) fails the gate mechanically instead of
surviving to review; `tests/contract_maintenance.rs`'s
`doc_evidence_prose_upholds_the_one_pin_invariant` pins the prose check
into every `cargo test` run the same way. A version bump therefore stays
red until the runtime evidence and the stream-json goldens are re-measured
and their active references are re-pinned together.
Versions move in two ways; either should trigger the check:

- the dev host auto-updates the native install
  (`~/.local/share/claude/versions/`, repointing `~/.local/bin/claude`) —
  a manual upgrade lands the same way;
- CI records the version it saw into `last-claude-version.txt` via
  `test_claude_version_recorded` (`tests/version_compat.rs`), and the release
  WorkflowTemplate uploads it as a release asset — diffing consecutive
  artifacts is the fleet-visible drift signal (claudepr-777d3056). The test
  and the gate resolve the artifact dir the same way —
  `$CLAUDE_PRINT_VERSION_ARTIFACT_DIR` when set, else the `cargo metadata`
  `target_directory` (the checkout's `target/` under stock cargo, the fleet
  wrapper's redirect under the wrapper), else the stock `target/` — so the
  two writers always land on one file however the host lays out build output.
  The gate refreshes the same file on every run, in the same full-line format
  `test_claude_version_recorded` writes, so the release asset stays real and
  consistently formatted even when cargo did not run first.

**Wiring.** The `claude-print-ci` WorkflowTemplate (this repo's
`claude-print-ci-workflowtemplate.yml`; the live copy is applied through
`declarative-config`) runs the gate on every push, in verify-only and release
mode alike: it installs the claude binary first (native installer —
`claude --version` needs no auth) so detection compares the *installed*
version instead of recording `unknown`, then invokes
`scripts/contract-maintenance-gate.sh --file-follow-up`. On drift the gate
re-anchors the last-claude-version artifact (same resolution as above), files or updates a GitHub
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
wiring (the push trigger that submits the template — the Sensor's
push/`refs/heads/main` filters and its `workflowTemplateRef` hand-off, fed
by the vendored EventSource stanza's push subscription — template fragments
including the fatal wrapper, the execution pin of that wrapper
(claudepr-3f4aefad): the invocation must sit on an *uncommented* line
inside the `if ! … exit 1 … fi` — a commented-out gate, an `exit 0`
failure branch, or a `|| true` neuter fails the suite, as does a gate
demoted below any quality gate or the verify-only green exit, a dropped
claude install, or a lost `set -ex` — gate exit-code contract against a
stubbed claude, and the doc/plan mentions) so the automation cannot
silently detach from this page again. When that enforcement goes red on a
version bump, the way through is the procedure below — the §Re-run and
§Re-pin steps (or §File follow-ups when a contract moved) — never
commenting the gate out of the template.

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
comment and any version-citing assertion messages, and update this
document's evidence citations — every version cited in the **Measured
against:** stamp, an evidence-table row, or an `Evidence (…)` preamble must
name the new pin or carry the `historical` marker, or the detector's prose
one-pin check (claudepr-893bfc4e) fails the re-pin commit itself. Commit
doc + fixture + same version. Re-measure the stream-json capture path —
**in the same pinned
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
