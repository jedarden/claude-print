# Pool Startup-Overhead Benchmark — measured evidence for ADR-005

**Measured against:** `claude-print` 0.2.2 (wrapping `claude` 2.1.278) and
`mock-claude-version-1.0.0`, release-profile builds of commit `03062de`,
2026-09-19, on the claude-print dev host (`codinghome`, i5-13500,
6.18.46). Harness: `scripts/bench_startup_overhead.py`. Raw
machine-readable samples and summary statistics:
`docs/notes/startup-overhead-benchmark.json` (schema
`claude-print/startup-overhead-benchmark/2`, committed alongside this note).
Schema 2 differs from the schema-1 recording of this same run only in the
`harness` block: the original recorded the bin dir as this host's
machine-specific absolute path (`/build/target-workers/release` — the cargo
redirect in effect here at measurement time, since superseded by the
per-repo `/build/claude-print` redirect), which no other box could resolve
and which no longer even matches this one. The re-recorded block states the
derivation instead; every measurement, sample, and environment field is
byte-identical (claudepr-70a60152).

**Scope — read this first.** This measurement covers **startup/prompt-injection
overhead only**: wall-clock from client process start to the arrival of the
`prompt injected` verbose trace (the plan's Benchmark Contract, plan §
Performance). It **does not establish model-latency savings**. The backend is
`mock-claude` — a test fixture answering from a canned response with none of
real Claude Code's JS-runtime startup, MCP-server init, or inference cost —
so these numbers isolate the per-invocation overhead `claude-print` itself
adds and the share of it the warm pool removes. They do not estimate
production wall-clock savings in absolute terms, and nothing here re-opens
the ADR-005 decision or the pool's design.

## Command line

```
cargo build --release
python3 scripts/bench_startup_overhead.py \
  --profile release --samples 10 --warmup 3 --mode both \
  --output docs/notes/startup-overhead-benchmark.json
```

No `--bin-dir` is needed: the harness locates the build output through
`cargo metadata --no-deps --format-version 1` and appends `--profile`
(`release` above; the default is `debug`), which resolves to
`/build/claude-print/release` on fleet hosts with the shared `cargo`
wrapper and `target/release` on a stock checkout — the resolution AGENTS.md
mandates in "Where the build output lands", so the same command line
reproduces on either. `cargo build --release` produces both `claude-print`
and `mock-claude` there. (`--bin-dir DIR` still overrides for ad-hoc
layouts.) The harness re-records its own argv and the bin-dir *derivation*
(`harness.bin_dir_source`) into the JSON artifact, redacting any explicit
`--bin-dir` value, so the committed results file is self-describing
without carrying machine-specific paths. A deterministic sanity harness
exists for CI-adjacent use (no subprocesses, no binaries needed):
`python3 scripts/bench_startup_overhead.py --self-check`.

## Method

The plan's Benchmark Contract defines overhead as process start → the
bracketed-paste write, logged at the PROMPT_INJECTED transition in
`--verbose` mode. The harness timestamps that stderr line from *outside* the
client — one wall clock for both paths — because the pooled session's
internal tracer re-anchors at `run_pooled` entry (after pool acquisition,
`src/session.rs`), so its internal `<ms>` values would exclude acquisition
cost and make the comparison dishonest. Per-sample *internal* trace values
are still captured as diagnostic sub-metrics (`internal_trace_ms`), which is
where the phase decomposition below comes from. Everything else follows the
plan contract unchanged: same box, sequential samples (one client at a
time), identical environment for both paths.

**Controlled environment (both paths identical):** mock-claude backend via
`--claude-binary` (hermetic — no credentials, no network, no real `claude`
install); throwaway temp `HOME` and `XDG_CONFIG_HOME` with an **empty**
`config.toml`, so no host config (model defaults, hook inheritance,
timeouts) steers a run; the ADR-005 canonical trivial prompt
(`Reply with exactly one word: pong`); `--output-format json`; per-invocation
`--timeout 60`. The same env is shared by the pool daemon and its clients.

**Pool configuration:** `claude-print serve --pool-size 1` over a temp-dir
Unix socket; clients pass `--pool-socket`. The daemon's initial warmup (start
→ first `settled and ready` log) was **1455.6 ms** — the one-off cost ADR-005
amortizes across invocations.

**Sample count and warmup policy:** 3 discarded warmup samples per mode,
then 10 recorded samples per mode. Warm mode additionally waits for the
daemon to log `settled and ready` for every worker up front **and again
after every sample** — each drive destroys its worker and the threshold
strictly increases per drive, so every recorded warm sample acquires a fully
settled replacement (steady-state warm path; a sample taken while the pool
is empty either falls back stateless — caught by the validity gate — or
queues behind the settling worker, inflating the number).

**Validity gates (abort, never trim):** a recorded run is valid only if the
client exited 0, a `prompt injected` trace arrived, a pooled sample shows
`driving prewarmed worker` (a quiet stateless fallback would pollute the
warm numbers), a stateless sample shows *no* such trace, and stdout parses
as a result object. Any violation aborts the whole benchmark with the full
log — a failed run is a bug signal, not an outlier to wait out.

**Variance / outlier handling:** no sample is discarded, trimmed, or winsorized;
the summary reports n, mean, stdev, min, p50, p90, p95, max, and cv_pct so
dispersion stays visible in the artifact itself. Every run made while
developing this harness on this box (debug and release, n=2 smoke runs and
the n=10 recorded run) landed per-mode cv at ≤2.6%; a future run whose
cv_pct is materially higher should be treated as a noisy-box measurement to
re-run on an idle machine, not as evidence of a code change.

## Results (release profile, 2026-09-19)

| Path | n | mean | p50 | p95 | max | cv |
|---|---|---|---|---|---|---|
| Cold / stateless | 10 | **1443.6 ms** | 1444.0 ms | 1451.6 ms | 1452.2 ms | 0.43% |
| Warm / pool | 10 | **1050.6 ms** | 1044.8 ms | 1083.9 ms | 1100.9 ms | 1.83% |

Mean cold − warm = **393.0 ms (27.2%)** under mock-claude.

### Phase decomposition (per-sample internal traces)

| Phase (internal ms) | Cold | Warm |
|---|---|---|
| spawn → child forked / pool acquired + fifo opened | 0–4 | ≈0 (acquisition incl.) |
| waiting → trust-dismissed | **401–408** | absent (worker pretrusted) |
| trust-dismissed → prompt injected | **1002–1007** | **1002–1007** |
| external clock − internal trace (spawn + arg/config resolution) | ≈30–40 | ≈40–50 |

Both paths are dominated by `claude-print`'s own fixed quiet windows — the
0.4 s trust-dismiss fallback and the 1.0 s post-CR idle-settle window
(ADR-005 "Alternatives Considered" #3) — because mock-claude produces almost
no PTY output, so each window runs to its full bound. The warm pool removes
the trust-dismiss window per drive (the worker was pretrusted by the
daemon); the drive still re-runs the settle window before pasting, by
design: the conservative redraw-race guard re-verifies PTY quiet before
injecting, even on a prewarmed worker. The daemon's 1455.6 ms initial warmup
is the mirror-image cost it amortizes away.

## Reproducibility and scope of the evidence

- **No credentials, no secret values, no network.** The backend is the
  in-repo `mock-claude` fixture; HOME/XDG_CONFIG_HOME are throwaway temp
  dirs; nothing outside the benchmark's own temp output is read or written.
  The committed JSON contains host/kernel/CPU/versions and timings only.
- **Deterministic harness check:** `--self-check` pins the percentile
  math, the trace-line parser, and the argv redaction without spawning
  anything.
- **Reproducibility guard:** `tests/benchmark_reproducibility.rs` runs in
  every `cargo test` and fails if the committed artifact (or this note)
  ever again hardcodes a machine-specific bin-dir path — the artifact must
  record the derivation (schema 2), not the path.
- **To reproduce:** build (`cargo build --release`), run the command line
  above on an idle machine, and compare against the committed JSON. The
  absolute numbers are box-specific (they price two sleep-bounded quiet
  windows plus process mechanics); the **structure** — cold paying
  trust-dismiss + settle, warm paying settle only, daemon warmup paid once —
  is the portable finding, and it is what ADR-005's rationale rests on.
- **What this evidence cannot say:** how much wall-clock a real NEEDLE
  dispatch would save. That depends on real Claude Code's startup and
  inference, which the mock deliberately removes. Measuring it would require
  a credentialed run and is out of scope here by the plan's own
  no-credentials CI rule.
