# Changelog

All notable changes to `claude-print` are documented in this file. Versions
are tagged `vX.Y.Z` and published as GitHub Releases by the `claude-print-ci`
Argo workflow, which runs the full gate set (fmt, clippy, test, cargo audit,
static musl build) against the tagged commit before publishing.

## [0.2.2] - 2026-09-20

First tagged release since v0.2.0. The 0.2.1 version number was consumed by a
Cargo.toml bump that was never tagged; both versions are published by this
release. Headline change: the ADR-005 warm PTY pool.

### Added

- **Warm PTY pool (ADR-005).** `claude-print serve` runs a pool daemon that
  keeps pre-warmed `claude` PTY workers ready (`--pool-size` 1–256,
  `--socket`, `--verbose`). The new `--pool-socket <path>` client flag
  acquires a warm worker instead of spawning a fresh `claude`, with automatic
  stateless fallback whenever the pool cannot serve (crashed daemon, full
  pool, acquire timeout, stale socket). Pooled sessions bill identically to
  stateless ones — the worker is still `claude` under a PTY, so
  `cc_entrypoint=cli` holds (INV-15). Startup overhead measured 27.2% lower
  pooled vs stateless against the mock backend
  (`docs/notes/startup-overhead-benchmark.md`; no model-latency claim).
- **Session-identity transcript binding.** A UserPromptSubmit identity-relay
  hook binds the `stream-json` reader to this session's exact transcript at
  prompt-submission time, so concurrent same-cwd invocations never forward a
  sibling session's events. Residual limit: an identity-less `claude` without
  UserPromptSubmit hook support still binds only an unambiguous single new
  transcript and otherwise waits for the Stop payload.
- **Config file.** `--config` with XDG path resolution, documented schema and
  precedence, comprehensive value validation, and structured errors on
  stderr; startup parse errors propagate to a clean failure.
- `--check` now verifies the `claude` binary and cleans orphaned temp
  directories; `--verbose` emits timing traces to stderr.
- **AS-4 billing canary automation** (`scripts/billing-canary.sh` plus
  systemd timer/service units), including a pooled leg via
  `CLAUDE_PRINT_POOL=1` and a NixOS-compatible host path.
- **CI.** Release-mode tag parameter on the `claude-print-ci` workflow,
  static musl builds for both binaries with a <10 MiB size gate, a
  `last-claude-version.txt` release artifact, and `mock_claude` shipped as a
  release asset.
- Prompt handling: null-byte rejection, `--input-file` path resolution with
  size cap, and a Stop-before-prompt backstop (exit 2 / `is_error`).
- Mock fixture: writes transcript JSONL at its reported path and supports
  `MOCK_DELAY_JSONL` / `MOCK_IS_ERROR` for race testing.

### Fixed

- `--timeout` is no longer forwarded to the child `claude` process.
- Stream-json reader tails the session identity payload's transcript, not the
  newest file in the directory — the fix behind same-cwd sibling
  contamination.
- Trust dialog entry is selected by text before confirming, not by position.
- `CLAUDE_CODE_CHILD_SESSION` is scrubbed before exec so the child persists a
  transcript (fixes `session_id: null` results); `CLAUDECODE` is unset for
  nested sessions.
- Watchdog: the detached watchdog gets its own pipe duplicate, and a poisoned
  mutex degrades instead of panicking.
- Pool daemon shutdown reaps every worker and removes only the socket node it
  created at bind time; truncated client frames are dropped instead of
  spinning on EOF.
- Prompt size is capped at 10 MB on stdin to prevent OOM; ANSI escapes are
  stripped from the `last_assistant_message` fallback.
- FIFO hardening: the hook script escapes the FIFO path against shell
  injection, and the FIFO read can no longer hang indefinitely.
- HOME resolution is centralized and strict; inaccessible chroot paths are
  rejected. `cwd_to_slug` validates paths and matches the claude 2.1.263
  fold behavior.
- `--setting-sources=` forwarding is conditional on `--no-inherit-hooks`;
  `--output-format` is honored in the binary-not-found path; a transcript
  reporting an assistant error exits 1 with `is_error`.

## [0.2.1] - 2026-09-20

Never tagged; its single change shipped to the public in v0.2.2.

### Fixed

- Stop forwarding `--timeout` to the child `claude` process.
