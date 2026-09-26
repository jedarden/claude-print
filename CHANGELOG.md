# Changelog

All notable changes to `claude-print` are documented in this file. Versions
are tagged `vX.Y.Z` and published as GitHub Releases by the `claude-print-ci`
Argo workflow, which runs the full gate set (fmt, clippy, test, cargo audit,
static musl build) against the tagged commit before publishing.

## [Unreleased]

### Added

- **Hardcoded-target-path guard.** `tests/target_path_guard.rs` (bead
  claudepr-7a9e7130) enforces AGENTS.md §"Where the build output lands"'s
  locator discipline standing across every surface that resolves build
  output — all of `tests/` and `scripts/` (any file type, fixtures
  included) plus `build.rs` at the root (absent today, covered from the
  day one appears). A written-out `target/debug`, `target/release`,
  musl-triple, or `/build/` redirect path fails the build outside a
  two-tier allowlist: whole-file exemptions for guards whose needles
  assert on the literals, and exact-line content-anchored exemptions for
  documentation and assertion quotes, each with its rationale in the same
  commit as the exception. Allowlist hygiene fails on stale, pointless,
  or redundant entries, and always-on negative meta-tests prove planted
  hardcodes are reported in all three scanned locations, commented
  mentions are not, and dropping a live exemption re-flags its line. A
  test that pins `target/debug/…` passes on a stock checkout and fails
  only on fleet hosts — this guard is what makes that drift fail CI.
- **Test-target classification guard.** AGENTS.md §"Execution requirements"
  now carries the exhaustive classification as an explicit table (every
  `tests/*.rs` target in exactly one row: compiled-binaries, repo-scripts,
  real-claude, library-level, plus the `config_error_helpers` helper
  carve-out), and `tests/docs_test_classification.rs` enforces it in CI:
  unclassified new targets, stale rows, and duplicate classifications fail;
  each group's dependency claim is verified against the target's
  compilation unit (target + `mod`-included helpers — `CARGO_BIN_EXE_*` /
  `current_exe()` for binary rows, spawning without a binary locator for
  script rows, a `"claude"` probe for real-claude rows, no
  `std::process::Command` for library-level rows outside the `#[ignore]`d
  live-probe carve-out); the Cargo.toml autodiscovery assumptions behind
  the enumeration are pinned; and the Ignored table plus the §"Test
  structure" table are cross-checked against the tree (bead
  claudepr-26cf624a). The old prose-enumerated group lists and the
  inaccurate "no tests of its own" claim about the standalone
  `config_error_helpers` target are gone — the helper's `cfg(test)` unit
  tests run in every target that includes it.

- **NEEDLE adapter installation contract defined and pinned.**
  `docs/notes/installer-needle-adapter.md` is now the authoritative statement
  of `install.sh`'s conditional NEEDLE leg (previously promised only in the
  README's one-liner): detection (`needle` on `PATH` or an existing
  `~/.needle/agents`), source (the checkout's `claude-print.yaml` beside the
  script — never a release artifact, hence the one installed file without a
  checksum entry), destination (`~/.needle/agents/claude-print.yaml`, dir
  created even when the source is absent), permissions (`install -m 644`,
  forcing 0644 over the repo copy's 0664 and over drifted destination modes),
  in-place overwrite with no backup (hand edits revert on the next install
  run — the pool `--pool-socket` opt-in is the affected workflow), the
  silent no-NEEDLE skip (`~/.needle` is never created), the missing-source
  skip note for the `curl install.sh | sh` shape, and the ordering that any
  earlier install failure places no adapter. `tests/install_sh.rs` pins all
  of it hermetically with the child `PATH` fully controlled (host tool dirs
  resolved at test time, so `command -v needle` never depends on the host —
  the fleet's coding boxes carry a real `needle`), plus a builder that
  panics if a real `needle` sneaks into the pinned `PATH` so the no-NEEDLE
  cases cannot pass vacuously.

- **installer-needle-adapter note pinned to the installer and its tests.**
  The note is now inside the pinning scope itself (bead claudepr-36893892):
  `tests/install_sh.rs` walks a table of (Semantics clause, `install.sh`
  fragment, enforcing test) triples — each documented clause must survive
  verbatim in `docs/notes/installer-needle-adapter.md`, the fragment
  implementing it must survive verbatim in `install.sh`'s source, and the
  note's Hermetic-coverage table must still name the test that enforces it
  — and the two position claims (the leg between the `mock_claude` leg and
  the `--check` smoke; `mkdir -p` inside the detection branch, before the
  source check) are checked against `install.sh`'s own source order.
  Rewording the note, changing the installer's mechanics, or renaming a
  pinning test now fails the build instead of letting the note and the
  installer drift apart.

- **Billing-canary installation and operations documented.** The "Install on
  each host" section of `scripts/billing-canary.md` is now the complete
  operator workflow for `install-billing-canary.sh`: prerequisites (including
  the unit-PATH trap — the installer checks the shell's PATH, the service
  runs the unit's pinned one), the installed-path/mode table, the installer's
  steps in order, the timer's schedule and catch-up semantics, the linger
  requirement (check, admin remedy, warning-not-block), post-install
  verification commands, how `CLAUDE_PRINT_POOL` relates to the installed
  timer (stateless by design; the pooled leg runs manually from the libexec
  copy, with `CLAUDE_PRINT_BIN` for fresh builds), and a `reason=`-keyed
  failure-recovery table covering every `FAIL` shape the canary can write
  plus the installer's own failure modes. Stale `ex44` host references in
  the README, the plan, and the canary doc corrected to `codinghome`
  (ex44 was decommissioned 2026-08-30).

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

- **README HOME provisioning recipes exercised and pinned.** The
  "HOME in containers and chroots" recipes are no longer documentation
  only (bead claudepr-ab982704): `tests/home_provisioning_recipes.rs`
  extracts the section's fenced blocks (the `text` unset-HOME error, the
  Dockerfile `ENV HOME=/home/claude`, the Kubernetes `env:` stanza) and
  the Prerequisites not-writable quote from README.md at runtime and
  matches them against the compiled binary's actual output, so the quoted
  lines cannot fork from `get_home()`'s messages. Each recipe is executed
  as its reader would — a provisioned HOME takes `--version` (the
  documented health-check form) and a full mock-claude prompt run to
  success with no probe residue, `--help` renders without HOME (the one
  documented exception), and the documented failure shapes are driven for
  real: missing mount, read-only permissions, and HOME-as-file on the
  host, plus a genuine `chroot(2)` jail running the whole matrix with the
  literal documented paths (`/home/service` provisioned, chmod-0555, a
  read-only tmpfs mount, and the never-provisioned `/home/claude`).

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
