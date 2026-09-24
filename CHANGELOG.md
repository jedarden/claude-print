# Changelog

All notable changes to `claude-print` are documented in this file. Versions
are tagged `vX.Y.Z` and published as GitHub Releases by the `claude-print-ci`
Argo workflow, which runs the full gate set (fmt, clippy, test, cargo audit,
static musl build) against the tagged commit before publishing.

## [Unreleased]

### Added

- **`inherit_hooks` semantics documented and pinned end-to-end.** The README
  gains a "Hook inheritance" section defining what the config key controls
  (the standard settings sources — user/project/local — so your hooks fire or
  don't), what it never controls (the relay `--settings` hooks, which stay
  active in both modes because Stop detection depends on them), the
  `--no-inherit-hooks` → `defaults.inherit_hooks` → `true` precedence chain,
  and the one-directional CLI flag. `mock_claude` gains
  `MOCK_USER_HOOK_MARKER` — a user-settings hook with a filesystem side
  effect that fires only when the child argv carries no `--setting-sources`
  spelling — and three new `tests/binary_e2e.rs` cases pin the config-file
  route and CLI precedence: `inherit_hooks = false` isolates the child,
  `inherit_hooks = true` inherits, and `--no-inherit-hooks` overrides a
  config `true`.

- **Release artifact integrity verification in `install.sh`.** The CI release
  build now publishes a `sha256sums.txt` manifest alongside the binaries, and
  `install.sh` verifies every artifact against it before installing or
  executing anything. A missing manifest, an asset with no checksum entry, or
  a digest mismatch aborts the install with nothing placed. The
  `mock_claude` fixture keeps its documented skip behavior when the manifest
  does not list it; a listed fixture that fails to download or mismatches is
  fatal. Covered by `tests/install_sh.rs` (valid, missing-manifest,
  missing-entry, tampered-binary, tampered-fixture, fixture-skip).

- **Installer rollback (`claude-print.prev`) documented and pinned.** The
  rollback copy `install.sh` has always preserved now has a stated contract
  and workflow: `docs/notes/installer-rollback.md` is the authoritative
  semantics (created only on upgrade, always holding the *immediately*
  previous binary — no chain; mode 755 preserved; verify-before-backup
  ordering so a failed install disturbs nothing; only the main binary is
  covered, never `mock_claude` or the NEEDLE yaml), and the README gains an
  "Upgrades and rollback" section with the one-step `mv` workflow. Four new
  `tests/install_sh.rs` cases pin the semantics hermetically against fake
  releases whose binary bodies differ per generation: backup-on-upgrade
  (verbatim content, mode, stdout note, mock scope), single-generation
  replacement across two upgrades, no copy on a fresh install, and the
  failed-install no-touch ordering.

### Changed

- **Startup-overhead benchmark evidence made machine-independent and
  guarded.** `scripts/bench_startup_overhead.py` no longer needs
  `--bin-dir`: it derives the build directory from `cargo metadata` (the
  resolution AGENTS.md mandates in "Where the build output lands") plus a
  new `--profile debug|release` flag, records only the derivation
  (`harness.bin_dir_source`), and redacts an explicit `--bin-dir` from the
  recorded argv. The committed artifact
  (`docs/notes/startup-overhead-benchmark.json`) moves to schema
  `claude-print/startup-overhead-benchmark/2` — same measurements, harness
  block stripped of the stale `/build/target-workers/release` absolute
  paths that no other host could resolve and that no longer match even
  this host's redirect — and the note's reproduce command derives the
  path instead of hardcoding it. `tests/benchmark_reproducibility.rs`
  fails the suite if a machine-specific bin-dir path ever returns to the
  artifact or the note.

- **Forgejo/GitHub roles and the canonical release publication path stated
  explicitly.** The README's Repository & contributions and Install sections
  now pin down the division of labor: GitHub is read-only for source and refs
  (a one-way Forgejo → GitHub mirror), while release artifacts live only on
  GitHub Releases — Forgejo hosts no release assets. The canonical
  publication path is named end to end: the `vX.Y.Z` tag is pushed to Forgejo
  first, the `claude-print-ci` Argo Workflow builds the tagged commit from
  Forgejo, and the workflow publishes the artifacts to GitHub Releases, from
  which nothing flows back. Installation guidance now holds when the mirror
  is unavailable: `CLAUDE_PRINT_RELEASE_URL` redirects `install.sh` to any
  host serving the same assets under the same checksum verification, and
  build-from-source from Forgejo never touches GitHub. The repo-root
  `claude-print-ci-workflowtemplate.yml` clone source is synced to the
  deployed template (Forgejo, per declarative-config `01783ba9`), removing
  the last GitHub-clone instruction in the repository.

- **README identifies Forgejo as the canonical repository.** Clone, install,
  and release references now state that
  `git.ardenone.com/jedarden/claude-print` (Forgejo) is the source of truth
  and the destination for pushes, that the GitHub repo is a read-only push
  mirror, and that GitHub Releases remains the supported channel for
  downloading release artifacts.

### Fixed

- **Billing-entrypoint contract defined and aligned.** The three names are now
  distinguished in one place (`docs/notes/billing-context.md`): `cc_entrypoint`
  is the wire-level billing header (never an environment variable, never
  directly observable); `CLAUDE_CODE_ENTRYPOINT=cli` is the authoritative
  environment input, forced into the child by `FORCED_ENV` (`src/pty.rs`);
  the transcript JSONL's `entrypoint` field is the JSON evidence, asserted by
  `scripts/check-billing.sh`. `CLAUDE_CC_ENTRYPOINT` — the phantom variable
  AGENTS.md invariant 5 told operators to verify — is gone from every
  operational surface, and a guard test
  (`tests/billing_entrypoint_contract.rs`) keeps it out. Alignments:
  `--check` gained a credential-free billing row that re-runs the binary's
  own child-env construction over an inherited `sdk-cli` and asserts a single
  forced `cli` (and no longer claims, in any doc, to read the session JSONL —
  that is check-billing.sh's job); `check-billing.sh` now extracts the
  `entrypoint` evidence from the first event carrying it as a *top-level*
  field, so the substring appearing nested in quoted message text no longer
  false-fails the manual newest-transcript release gate; README, AGENTS.md,
  `scripts/billing-canary.md`, and the plan's `--check` descriptions state the
  two halves accurately.

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
