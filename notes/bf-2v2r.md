# bf-2v2r — Make --verbose traces observable end-to-end (AS-6)

## Outcome

**The `--verbose` implementation already worked end-to-end at HEAD.** The task
hypothesized (from 3× failed verifications of parent bf-1bg4) that the trace line
was not being OBSERVED on stderr in a live mock-claude run. Empirical
investigation showed the opposite: the trace fires correctly today.

## Verification (empirical, built binary vs mock-claude)

```
$ ./target/debug/claude-print --verbose --claude-binary ./target/debug/mock-claude 'hi' 2>/tmp/v
[claude-print 0ms] temp dir created at /tmp/claude-print-...-XXXX
[claude-print 0ms] pty opened
[claude-print 0ms] child forked pid=...
[claude-print 0ms] fifo opened
[claude-print 1ms] phase transition: waiting -> trust-dismissed
[claude-print 2004ms] phase transition: trust-dismissed -> prompt-injected
[claude-print 2004ms] prompt injected
[claude-print 2005ms] stop received session_id=mock-session-abc123
[claude-print 2005ms] transcript read on attempt 1
```

- WITH `--verbose`: **9** `[claude-print <ms>ms]` lines on stderr.  ✓
- WITHOUT `--verbose`: **0** trace lines.  ✓
- The two stderrs differ (verbose adds traces).  ✓

The wiring is intact and was verified line-by-line:
`cli.rs:113` (`verbose: bool`) → `main.rs:211` (`verbose: cli.verbose`) →
`session.rs:91` (`LaunchOptions.verbose`) → `session.rs:323`
(`Tracer::new(launch.verbose, start_time)`), with trace points at
`session.rs:327/423/424/488/583/643/705` and `transcript.rs`. The reason the
3 earlier verifications of bf-1bg4 failed is no longer reproducible — the
landed commits (`a84f77e` wire verbose, `ecaf068` gate mock Stop on prompt
receipt) together made the trace observable. **No change to the --verbose
path was needed.**

## The one thing that kept `cargo test --test binary_e2e` red

`as6_verbose_…` was **already green**. The sole remaining red test was an
**unrelated, pre-existing** failure:

`as5_missing_binary_json_mode_is_error_true` — the binary-not-found
short-circuit (`main.rs`) unconditionally printed a text message to stderr and
exited 2, **ignoring `--output-format json`**, so JSON callers got empty stdout.
This path has been text-only since bf-2f5 (2026-06-25) and never honored JSON.

### Fix (minimal, scoped to `src/main.rs` only)

Route the binary-not-found branch through `emit_error` for JSON / stream-json
(producing the same structured `result` object every other error arm emits —
`type:"result"`, `subtype:"internal_error"`, `is_error:true`,
`error_message:"'<path>' not found in PATH"`), while keeping **text-mode stderr
byte-identical** to before:

- text: `claude-print: '<path>' not found in PATH` (unchanged)
- json/stream-json: structured object on stdout

This is orthogonal to `--verbose` — the flag is never inspected — so it cannot
regress the AS-6 contract, and non-verbose text-mode stderr is unchanged.

## Acceptance criteria

- [x] `--verbose` vs mock-claude emits ≥1 `[claude-print <ms>ms]` line; without
      it, none. (9 vs 0)
- [x] `cargo test --test binary_e2e` green — **12 passed; 0 failed**.
- [x] Non-verbose stderr unchanged (text-mode missing-binary path byte-identical).
