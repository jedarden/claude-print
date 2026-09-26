# AGENTS.md — claude-print

## Repo purpose

`claude-print` is a drop-in replacement for `claude -p` that drives the Claude Code
interactive TUI via PTY, preserving subscription billing after the June 15, 2026
`cc_entrypoint` split. It spawns `claude` in a pseudo-terminal, auto-dismisses the
trust dialog, injects the user's prompt, waits for the Stop hook, reads the
transcript, and emits clean output — all without `--print` or `--output-format`.

## Build commands

```bash
# Debug build
cargo build

# Musl release (static binary for deployment)
cargo build --target x86_64-unknown-linux-musl --release

# Tests  (intercepted by ~/.local/bin/cargo — submits to iad-ci when repo is clean)
cargo test

# Unit tests only (no binary compilation required)
cargo test --lib

# All test targets — unit + integration (requires compiled binaries)
cargo test --tests

# Smoke check (verifies PTY, FIFO, and billing env prerequisites; credential-free)
cargo run --bin claude-print -- --check
```

**Never use `cargo test --test '*'`** — use `cargo test --tests`. Quoted, the
wildcard does resolve on stock Cargo (glob target selection), but it cannot
survive the fleet path: `~/.local/bin/cargo-remote` flattens the invocation
into one string (`TEST_ARGS="${*:2}"` drops the quoting), and the
`rust-verify` verify pod re-expands that string unquoted
(`cargo test $TEST_ARGS`), so the literal `*` glob-expands to the clone's
top-level files — `error: unexpected argument 'Cargo.toml' found` (verified
2026-09-24, claudepr-07a82368). A clean checkout is precisely the case that
offloads to iad-ci, so the wildcard fails exactly where a fleet agent
following this doc would run it. `cargo test --tests` selects every test
target (unit + integration, no doctests) with nothing to quote, identically
under stock and wrapped Cargo; a single target remains
`cargo test --test <name>` — a named selector carries no metacharacters
either way.

### Where the build output lands

`./target/…` is correct on a stock Cargo checkout and wrong on fleet hosts
(codinghome, lab): there the `cargo` wrapper at `~/.local/bin/cargo` exports
`CARGO_TARGET_DIR=/build/claude-print` for every invocation, so build output
lands under `/build/claude-print/…` and `./target/` is never created. The
wrapper also refuses a `--target-dir` outside `/build/claude-print/` (one
shared target dir per repo; needle-d6b685b4). The same command therefore has
two possible output locations:

| Artifact | Stock checkout | Fleet hosts (wrapper) |
|----------|----------------|----------------------|
| Debug binary | `target/debug/claude-print` | `/build/claude-print/debug/claude-print` |
| Host release binary | `target/release/claude-print` | `/build/claude-print/release/claude-print` |
| Musl release binary | `target/x86_64-unknown-linux-musl/release/claude-print` | `/build/claude-print/x86_64-unknown-linux-musl/release/claude-print` |
| mock-claude fixture | `target/debug/mock-claude` | `/build/claude-print/debug/mock-claude` |

Don't hardcode either column. Resolve the target directory through cargo
itself — `cargo metadata` runs through the same wrapper, so it reports
`/build/claude-print` on fleet hosts and the checkout's `target` dir on a
stock one:

```bash
TARGET="$(cargo metadata --no-deps --format-version 1 | jq -r .target_directory)"
"$TARGET/debug/claude-print" --check
```

`cargo run --bin claude-print -- --check` sidesteps path resolution entirely
and works under both layouts. Tests need no path pinning either: the
integration and e2e suites locate `mock-claude` and the crate binary through
cargo's own locators, never a written-out path — `current_exe()` (the test
binary sits in `target/<profile>/deps/`, the bins beside it in
`target/<profile>/`; e.g. `tests/pty_integration.rs` and
`tests/binary_e2e.rs`) or the compile-time `CARGO_BIN_EXE_claude-print`
(e.g. `tests/home_unset.rs` and `tests/stdin_limit.rs`) — so they pass
wherever cargo puts the build.

This section is drift-pinned by `tests/docs_build_layout.rs`
(claudepr-9c5f098f): the artifact table is re-derived from Cargo.toml's
`[[bin]]` set and the CI WorkflowTemplate's toolchain, the locator examples
are verified against the sources they name, no test source may hardcode a
written-out build-output path, and both locators are proven live against
the artifacts cargo just built. The locator discipline's code side is
enforced standing by `tests/target_path_guard.rs` (claudepr-7a9e7130),
the sibling of `tests/home_env_guard.rs`: it sweeps `tests/`, `scripts/`,
and `build.rs` for written-out target paths (`target/debug`,
`target/release`, the musl triple CI builds, `/build/` redirects) outside
an explicit allowlist of documentation and assertion quotes, failing the
build on any new hardcode — without it, a test that pins
`target/debug/…` passes on a stock checkout and fails only on fleet
hosts, so nothing that runs on a fresh checkout could ever catch it.

The `cargo` wrapper at `~/.local/bin/cargo` auto-submits to the `rust-verify`
WorkflowTemplate on `iad-ci` when there are no uncommitted changes and the repo has
a remote. It falls back to a cgroup-limited local run otherwise.

## Test structure

| Location | What it tests |
|----------|---------------|
| `src/*.rs` inline (`#[cfg(test)]`) | Unit tests — pure logic, no I/O |
| `tests/integration.rs` | High-level integration; uses `mock_claude` |
| `tests/integration/` | Sub-module helpers for integration tests |
| `tests/cli.rs` | CLI argument parsing and flag validation |
| `tests/config_parse_errors.rs` | Malformed config → exit 2 (not 0) + structured JSON error on stderr in json/stream-json modes, human-readable stderr in text mode; no silent fallback to defaults (bead claudepr-ea80e6b2) |
| `tests/config_startup_errors.rs` | End-to-end config failures during CLI startup in all three output modes, via the shared `config_error_helpers` module |
| `tests/config_error_helpers.rs` | Shared helper module for the config-error suites (`ConfigFixture`, `run_with_config*`, structured-error assertions); also compiled as a standalone integration target, where its `cfg(test)` unit tests of the fixture helpers themselves run — a helper module, not a suite, and the one `helper`-group carve-out in the §"Execution requirements" classification table |
| `tests/config_contract.rs` | Config-file contract pin (bead claudepr-227efdb1): loads `tests/fixtures/config_contract_examples_v1.json` and replays it through the real implementation so `docs/notes/config-file-contract.md` (and the README's Configuration section) cannot drift — loader alignment (fixture TOML through `Config::load_or_default` from a temp cwd: successes resolve through the real CLI-over-config tiering, failures byte-match the user-facing message), path alignment (every `default_path` rule, incl. the empty-but-set and non-UTF-8 `XDG_CONFIG_HOME` edges and the strict `HOME` failure), table alignment (the four `[defaults]` keys against the resolvers' built-ins and clap's actual `default_value`s — the mechanism behind the documented `max_turns`/`timeout_secs` limitation), emitter alignment (documented text/json/stream-json error lines byte-for-byte through `emit_error`), doc alignment (every `documented: true` example verbatim in the doc, `also_in_readme` ones in the README), and README Configuration-section alignment (bead claudepr-746dd1c4): the `## Configuration` slice is extracted and its key table (columns, key set, types, built-in defaults, CLI counterparts), shipped-defaults TOML block, and File-location precedence list are asserted against the fixture, so a contract change that forgets the README fails the suite. Env/cwd-mutating replays serialize on one process lock |
| `tests/config_entry_point_scope.rs` | Binary-level pin of the per-entry-point config-load scope (bead claudepr-58e1a4b0): `--version`, `--check`, and `serve` never read the config file (`docs/notes/config-file-contract.md` §"Scope", `docs/config-error-analysis.md`). Each entry point runs with a poisoned config — a directory at the path (unreadable tier) or `[[` garbage TOML (parse tier) — at the discovered path via both discovery rules (`$XDG_CONFIG_HOME` and `$HOME/.config`) and via `--config`, and its stdout, stderr, and exit status must be byte-identical to a run with no config file present (baselines anchored as each entry point's healthy output so two identical failures cannot pass); a lethality control runs every poison through the prompt path and pins the exit-2 `invalid config` error, proving the identity cannot hold vacuously; and a live `serve` daemon with BOTH channels poisoned at once still binds, warms a worker, and takes the full clean-stop contract — covering a config load placed inside `run_serve` past `validate_pool_size`, which the fast-fail matrix shape cannot reach. Hermetic: compiled `claude-print` + mock-claude |
| `tests/emitter.rs` | Output formatting (text / json / stream-json) |
| `tests/startup.rs` | Trust-dialog detection and prompt injection |
| `tests/terminal.rs` | Terminal probe parsing |
| `tests/transcript.rs` | JSONL transcript parsing |
| `tests/tui_transcript.rs` | TUI-shaped transcript parsing (claudepr-26e7a0b6): `sessionId` spelled on every ordinary record and no print-mode `type: "result"` event — the two reasons `session_id` came back null for every real PTY run |
| `tests/docs_slug_consistency.rs` | Documentation-drift guard for the transcript-slug algorithm (bead claudepr-3243f25c): `tests/fixtures/slug_vectors_v2.1.263.json` pins `cwd_to_slug` against live-verified vectors, every `<path> → <slug>` example in the markdown docs is re-derived with the implementation, and stale prose patterns are linted |
| `tests/docs_pool_contract.rs` | Documentation-contract test for the published serve / `--pool-socket` API (bead claudepr-e4e94485): every `claude-print ...` example line in README §"Warm PTY pool (ADR-005)" and AGENTS.md §"Pool operations" parses through the real `Cli` parser with the documented flag values, the documented default socket path / pool-size default and cap / acquire budget equal `DEFAULT_SOCKET_PATH`, clap's `default_value`, `MAX_POOL_SIZE` + `validate_pool_size`, and `DEFAULT_ACQUIRE_TIMEOUT_SECS`, the serve usage line and flag table list exactly the long flags the subcommand accepts (cross-checked against the rendered `serve --help`), the `0600` permission claim is re-derived through a real `bind_socket`, and the fallback-vs-protocol-failure split matches `is_stateless_fallback` plus `AcquireFailure`'s Display |
| `tests/docs_test_classification.rs` | Documentation-drift guard for the exhaustive test-target classification (bead claudepr-26cf624a): re-derives the AGENTS.md §"Execution requirements" classification table from the `tests/` tree and fails CI on any unclassified, stale, or duplicated row; verifies each execution group's dependency claim against the target's compilation unit (target + `mod`-included helpers — compiled-binary rows reference `CARGO_BIN_EXE_*`/`current_exe()`, repo-script rows spawn with no binary locator, real-claude rows probe the installed `claude`, library-level rows contain no `std::process::Command` outside the `#[ignore]`d live-probe carve-out); pins the Cargo.toml autodiscovery assumptions behind the enumeration; and cross-checks the Ignored table against the `#[ignore]`d tests in the tree plus the §"Test structure" table against the target list. Library-level; spawns nothing |
| `tests/docs_build_layout.rs` | Drift-pin for AGENTS.md's own build/test documentation (bead claudepr-9c5f098f), the surface the README and docs/notes pins left unguarded: §"Where the build output lands" — the stock-vs-fleet artifact table is re-derived from Cargo.toml's `[[bin]]` set and the CI WorkflowTemplate's `rustup target add` toolchain (the musl row's triple comes from CI, never from the doc), the fleet column must be one consistent absolute redirect base carrying the same cargo-relative suffixes as the stock column (the wrapper's per-repo redirect value itself is a fleet-environment fact, deliberately unpinned), the documented `cargo metadata` target-dir snippet must survive verbatim and address the same artifact as the table's debug row, the musl build command must match the CI toolchain, and the `current_exe()`/`CARGO_BIN_EXE_claude-print` locator prose is verified against the example suites it names; §"mock_claude"'s member claims and the bin-naming example commands are re-derived too (claudepr-15471b3f) — the membership sentence's directory against the root Cargo.toml `[workspace]` members list, the `cargo build -p <name>` rebuild command against the member's real `[package]` name, the fixture row's bin against the member's own `[[bin]]` declaration, and every `--bin <name>` example in the section against the `[[bin]]` set (the commands' success under either layout is the environment half, deliberately unpinned); no test source may hardcode a written-out build-output path (`target/debug`, `target/release`, a musl release path, or a `/build/` literal); both locators are proven live against the artifacts cargo just built (profile-dir derivation from the test binary's own location, `CARGO_BIN_EXE_claude-print` and `CARGO_BIN_EXE_mock-claude` agreeing with it, every `[[bin]]` present and executable beside the test binaries); and §"Test structure" hygiene — every `tests/<name>.rs`/`src/<name>.rs` mention in the table resolves, and the `tests/fixtures/` inventory row is exhaustive against the directory with brace forms expanded; and the guard's own failure behavior is pinned by always-on negative meta-tests (claudepr-4d967120) — each guarded input mutated in memory (corrupted stock cell, mutated Cargo.toml bin set, drifted CI musl triple, mis-attributed locator example, stale or undocumented fixture-row entry, unlisted member directory or mismatched `-p` selector, renamed member package or `[[bin]]`, unbuilt example `--bin` name) must fail the owning check naming the drift, proving the guard non-vacuous on every run. Library-level: reads AGENTS.md, the root and fixture-member Cargo.tomls, the WorkflowTemplate, and the tests/ tree; resolves built artifacts through cargo's locators; spawns nothing |
| `tests/docs_build_commands.rs` | Drift-pin for AGENTS.md's §"Build commands" test-invocation contract (bead claudepr-a34a6399), the section the artifact-table pin in `tests/docs_build_layout.rs` deliberately leaves unguarded: the bolded wildcard prohibition sentence, the `cargo test --tests` alternative it prescribes in the same breath, the named single-target selector guidance, and the exact all-targets fence line must all stay in the section, and the section's command fence may never itself prescribe a wildcard `--test` selector — the fence is the copy-paste surface. A selector grammar (`--test NAME` and the `=`-joined form, surrounding quotes stripped, any glob metacharacter) then scans AGENTS.md, README.md, `docs/`, and `scripts/` for the hazardous form, failing the build if it re-enters any operational doc or script: the form resolves under stock Cargo and dies only on the fleet's cargo-remote TEST_ARGS flatten (verified 2026-09-24, claudepr-07a82368), so no behavioral test on a stock checkout could catch its return. The one sanctioned occurrence is the AGENTS.md prohibition sentence itself, anchored by its exact bolded lead so the exemption cannot hide a prescription — a negative meta-test re-flags the line the moment the lead leaves it — and further negative meta-tests strip each pinned fragment and plant every hazardous spelling in memory, proving the guard non-vacuous. Library-level: reads AGENTS.md, README.md, `docs/`, and `scripts/`; spawns nothing |
| `tests/benchmark_reproducibility.rs` | Reproducibility guard for the committed startup-overhead evidence (bead claudepr-70a60152): the schema-1 recording carried machine-specific paths (`/build/target-workers/release`) no other box could resolve; schema 2 derives the bin dir from `cargo metadata`, records only the derivation (`harness.bin_dir_source`), and redacts an explicit `--bin-dir` from the recorded argv — this test pins that shape (against `docs/notes/startup-overhead-benchmark.{json,md}` and the script) so the drift cannot silently return. Library-level; spawns nothing |
| `tests/hooks.rs` | Stop hook FIFO install / read |
| `tests/stop_poller.rs` | Stop payload polling logic |
| `tests/pty_integration.rs` | PTY spawn + round-trip (requires PTY capability) |
| `tests/nested_session.rs` | `CLAUDECODE`/session-marker scrub regression (claudepr-26e7a0b6): the child env is built in the *parent* (`build_child_env`/`scrub_env`, `SCRUBBED_ENV`/`FORCED_ENV` in `src/pty.rs`), so a claude-print run inside another Claude session still creates a fresh top-level session instead of a subagent-style transcript with null `session_id`. Also pins that the removed `unsetenv`-between-fork-and-exec mechanism (not async-signal-safe; pool mode is multithreaded) stays dead |
| `tests/home_unset.rs` | Strict `HOME` contract end-to-end (`src/util.rs::get_home`): unset/empty/missing/non-directory/non-writable HOME yield path-specific setup errors and never an implicit `/root` fallback — across config path resolution, transcript path derivation, the live projects dir, direct session startup, and binary error output. Env-mutating cases serialize on one lock; binary cases override the child env only (`docs/test-coverage-home-unset.md`) |
| `tests/home_env_guard.rs` | Standing call-site guard for the HOME policy (bead claudepr-bdfac6f7): no direct HOME environment read outside `src/util.rs` (`var("HOME")`, `var_os("HOME")`, and the `home_dir(` bypass family), exactly one raw read inside it, and the `docs/notes/home-handling-strategy.md` call-site table pinned against the actual `get_home()` production call sites via a per-module reference snapshot — doc and code move together or this fails, replacing the one-time audit beads the contract kept outliving. Library-level: reads `src/` and the strategy doc, spawns nothing |
| `tests/target_path_guard.rs` | Standing guard for the no-written-out-build-output-path discipline (bead claudepr-7a9e7130), the code-side sibling of `tests/home_env_guard.rs` and `tests/docs_build_layout.rs` (whose own lint covers `tests/**/*.rs` only): scans every file under `tests/` and `scripts/` (any extension, fixtures included) plus `build.rs` at the root (absent today, covered from the day one appears) for written-out target paths — `target/debug`, `target/release`, the musl triple derived from the CI WorkflowTemplate, and `/build/` redirects however spelled (the legacy `target-workers` base included) — outside a two-tier allowlist (whole-file: guards whose needles assert on the literals; exact-line, content-anchored: documentation and assertion quotes, each with its rationale, so any edit to an exempted line forces a deliberate allowlist update in the same commit); allowlist hygiene fails on stale, pointless, or redundant entries so the list cannot rot into noise; and always-on negative meta-tests prove planted hardcodes are reported in all three locations with file, line, and needle, commented mentions are not, and dropping a live exemption re-flags its line. Library-level; spawns nothing |
| `tests/home_provisioning_recipes.rs` | End-to-end exercise of the README §"HOME in containers and chroots" provisioning recipes against the compiled binary (bead claudepr-ab982704), complementing `tests/home_unset.rs` (which pins the error contract, not the recipes): the section's fenced blocks (`text` error line, `dockerfile` ENV, `yaml` env stanza) and the Prerequisites not-writable quote are extracted from README.md at runtime and matched against the binary's actual stderr so the quoted lines cannot fork from `get_home()`'s messages; each recipe is then executed as its reader would — a provisioned HOME takes `--version` (the documented health-check form) and a full mock-claude prompt run to success with no probe residue, `--help` renders without HOME (the one documented exception), and the documented failure shapes are driven for real on the host (missing mount → `not accessible`, chmod-0555 → the README-quoted `not writable` line rebuilt from its own ellipsis template, HOME-as-file → `is not a directory`) plus in a genuine `chroot(2)` jail (user+mount namespaces, the `tests/home_unset.rs` harness pattern, `/bin/sh` probe so NixOS hosts run it too) with the literal documented paths: provisioned `/home/service` succeeds, read-only mode and a read-only tmpfs mount fail naming `/home/service`, and the never-provisioned `/home/claude` fails as a missing mount. Child-env overrides only; no process-env mutation |
| `tests/sigint_forwarding_e2e.rs` | Single-session SIGINT forwarding through `PtySpawner::relay` (HR-8): mock child receives the forwarded signal AS SIGINT (trap marker + default-disposition kill), relay returns 130, child reaped, SIGINT/SIGWINCH dispositions restored (bead claudepr-1472789b) |
| `tests/sigwinch_forwarding_e2e.rs` | Single-session SIGWINCH forwarding regression (bead claudepr-4f8c962e): grafts a 24×80 PTY onto the test's own stdin, spawns a trapping `sh` child through `PtySpawner::spawn`, and delivers one resize-shaped SIGWINCH to the test process only after both the child's trap and relay's handler are provably armed — proving through the real `relay` that `TIOCSWINSZ` lands the new geometry on the child's terminal and fires its trap, with dispositions restored and the child reaped. Hermetic (report files only); no compiled binary, no `claude` |
| `tests/claude_config_dir_contract.rs` | Key-invariant #1 regression contract — never set `CLAUDE_CONFIG_DIR`, so transcripts land in the real `$HOME/.claude/projects` (bead claudepr-bfe97ce4, plan HR-4/ADR-001): a real child spawned through the public `PtySpawner` receives no `CLAUDE_CONFIG_DIR` even when one is inherited (outer cleanrooms export it to relocate Claude Code's whole config dir, and the leak would redirect the child's transcript root while claude-print keeps watching the HOME-rooted tree), the scrub is wired into `SCRUBBED_ENV` in `src/pty.rs` (the one env builder the exec path uses), and a binary end-to-end run against mock-claude with an inherited decoy value and a throwaway `HOME` shows no `CLAUDE_CONFIG_DIR` in the child env dump (`MOCK_RECORD_ENV`) and lands the transcript under `<HOME>/.claude/projects` — never the decoy — with the session reading back successfully. mock-claude honors `CLAUDE_CONFIG_DIR` when present, so a scrub regression fails instead of passing vacuously |
| `tests/claude_config_dir_docs_contract.rs` | Documentation-contract test for the Claude-state / `CLAUDE_CONFIG_DIR` sections (bead claudepr-e46458c4): pins the wording of `docs/notes/config-file-contract.md` §"Claude Code state and `CLAUDE_CONFIG_DIR`" and the README's `### Claude Code state (CLAUDE_CONFIG_DIR)` summary against the implementation — every contract-bearing sentence present in its scoped, whitespace-normalized section; the two documents agreeing identifier-for-identifier (`SCRUBBED_ENV`, `derive_transcript_path`, `projects_dir_for_cwd`, `scripts/check-billing.sh`, the behavioral twin `tests/claude_config_dir_contract.rs`); every cited file existing; the env-list claims checked against the `SCRUBBED_ENV`/`FORCED_ENV`/`src/hook.rs` source wiring; the HOME-rooting claim replayed behaviorally under a throwaway `HOME` with a decoy inherited `CLAUDE_CONFIG_DIR` (exact `$HOME/.claude/projects/<cwd-slug>/<session-id>.jsonl` shape, neither reader touching the decoy); and the no-redirection claim checked against clap's parser definitions (only `--config` and `--mcp-config` contain "config"), the closed-world `[defaults]` schema, and `FORCED_ENV`. Library-level and hermetic; the env-mutating replay serializes on one lock |
| `tests/version_compat.rs` | `--version` output parsing (print/TUI transcript shape fixtures). `test_claude_version_recorded` shells out to the installed `claude --version` — silent skip when `claude` is absent — and writes the `target/last-claude-version.txt` CI artifact that `scripts/check-claude-version-bump.sh` diffs against |
| `tests/contract_maintenance.rs` | Contract-probe maintenance wiring guard (beads claudepr-e8fc5744 + claudepr-3094ab2e + claudepr-b590e46d): the always-on active-version consistency check (doc stamp + `claude_contracts_v*` + `stream_json_golden_v*` must name one Claude version; unreferenced fixture files are exempt history), gate script + detector parse (including divergent-pin rejection without any `claude`), the gate's exit-code/evidence/version-file/follow-up contract against a stubbed `claude` (hermetic, re-pin-proof), real-environment self-consistency (CI's exercise of the detection path), and template/docs wiring fragments — including the fatal drift wrapper that makes a version change fail CI until the re-pin lands (`scripts/contract-maintenance-gate.sh`, `claude-print-ci-workflowtemplate.yml`, §Maintenance, plan R-2) |
| `tests/contract_drift_watch.rs` | Scheduled contract-drift watch (bead claudepr-e6e54313): `scripts/contract-drift-watch.sh` under `bash` with `claude`/`bead` stubbed on a single-entry PATH of real-coreutils symlinks (the `contract_maintenance` pattern) — exit codes mirror the detector (0 current / 1 drift / 2 indeterminate), the PASS/DRIFT/INDETERMINATE state line lands atomically in the billing-canary shape via `CLAUDE_PRINT_DRIFT_STATE_DIR`, drift files exactly one bead (`--unique-ref claude-contract-drift:live-<version>`, `--label contract-drift`, pin/live in the title, the `claude-contract-drift live=` marker in the body, cwd = the contract repo so the follow-up lands in its workspace), `EXISTING`/`EXISTING_CLOSED` idempotent hits are recorded not duplicated, a missing or failing `bead` degrades to a recorded not-filed/failed while the drift exit stands, indeterminate never files, `CLAUDE_PRINT_CONTRACT_REPO` resolves the repo out-of-tree (the libexec mode the service unit drives, with the no-env case failing closed as detector-missing), and the units/doc wiring fragments (`OnCalendar=daily`, `Persistent=true`, ExecStart + repo env, §Maintenance → Scheduled watch) stay attached |
| `tests/install_contract_drift_watch.rs` | Hermetic pin of `scripts/install-contract-drift-watch.sh`, the §Maintenance → Scheduled watch installer (bead claudepr-e6e54313), on the `install_billing_canary` pattern: redirected `HOME`/`XDG_CONFIG_HOME` and a one-fake-bin-dir PATH (no real systemd reachable, so absent fakes are genuinely absent) verify the watcher and both units land byte-identical at the documented paths with 0755/0644 modes (dirs 0700/0755), the installed service still ExecStarts the libexec copy and pins `CLAUDE_PRINT_CONTRACT_REPO=%h/claude-print`, `daemon-reload` precedes `enable --now`, a second run is idempotent and restores drifted copies/modes, missing `systemctl` or `bead` aborts with exit 1 before anything is written (no partial install — `bead` is hard because the filed bead *is* the drift alert channel), a missing `claude` only warns (the watch degrades to a loud INDETERMINATE rather than losing its alert), a failed enable never reports success, and the linger warning fires only when `Linger=no` |
| `tests/flag_compat.rs` | Child-argv compatibility with the *installed* `claude`: every flag claude-print forwards is still accepted by the child's argument parser — the check that was missing when `--timeout` was forwarded to a binary without that option and broke every invocation against claude 2.1.263 — with a deliberately-unknown-flag inverse so the probe cannot silently stop detecting. Credential-free: an unknown *option* is rejected before any model request; silently skips when `claude` is not on PATH |
| `tests/claude_contracts.rs` | Measured Claude Code runtime contracts (bead claudepr-6ef2541c, re-pinned to 2.1.282 by claudepr-3094ab2e), pinned in `tests/fixtures/claude_contracts_v2.1.282.json`: `--settings` merges (not replaces), `--setting-sources=` suppresses standard sources while the settings file still loads, Stop fires once per turn. Always-on tests keep claude-print's child argv and relay-settings schema aligned with the *measured* spelling; two `#[ignore]`'d tests re-measure against the real claude (API auth required — they skip silently without it; sandboxed HOME). Maintenance workflow: `docs/notes/claude-contract-probes.md` |
| `tests/install_sh.rs` | `install.sh` end-to-end against a fake release directory (assets + `sha256sums.txt`) served via `CLAUDE_PRINT_RELEASE_URL=file://…`, temp `HOME`, fake `claude` on `PATH` so the post-install `--check` smoke passes. Three surfaces: release-artifact integrity verification (missing manifest, unlisted asset, and digest mismatch each abort with nothing placed); the `claude-print.prev` rollback-copy semantics of `docs/notes/installer-rollback.md` — backup on upgrade (verbatim content, mode 755, no `mock_claude` copy), single-generation replacement across consecutive upgrades, no copy on a fresh install, and verify-before-backup ordering (a failed install disturbs neither the live binary nor an existing copy); the mid-install failure window — with verification passed and the backup `mv` done, a failing final placement (an `install` shim on `PATH` matching only the main binary's destination) exits nonzero with the tool's error on stderr, prints no success line (`Installed …`/`--check`/`Installation complete`), places nothing further, and leaves the live path vacant while `.prev` holds the previous binary verbatim at 755, still runnable, with the documented rollback `mv` restoring a working binary; the post-install `--check` failure (rollback-note triage row three) — a release whose binary passes verification and placement but fails the smoke exits nonzero with the documented error line plus the check's own output on stderr, prints no success output after the failed leg (no `--version` line, no `Installation complete.`), and leaves the new binary live with `.prev` holding the previous generation and earlier legs' placements standing, the documented rollback `mv` restoring a binary that passes the post-rollback `--check`/`--version` gate; and the documented `SKIP_MOCK_CLAUDE=1` opt-out — a release that ships and lists the fixture installs only the binary: the fixture leg never starts (no download, no verify line, nothing placed) while the binary is still verified, installed verbatim, and mode 755; and the optional-fixture skip decided by the manifest alone — a shipped-but-unlisted fixture is neither downloaded nor placed, the same omitted checksum entry that is fatal for the main binary. The README's `## Install` supply-chain paragraph is pinned to these tests (the layer-6 pattern of tests/config_contract.rs): every documented clause — verification against the published `sha256sums.txt` before install or execution, the fail-closed triad (missing manifest / unlisted asset / digest mismatch, nothing placed), the optional-fixture skip, the `SKIP_MOCK_CLAUDE=1` opt-out — is matched verbatim in the section and mapped to the named adversarial test enforcing it, with the manifest asset name and the opt-out variable cross-checked against install.sh's own source; and the conditional NEEDLE-adapter leg of `docs/notes/installer-needle-adapter.md` — both detection arms (`needle` on `PATH`, existing `~/.needle/agents`), the checkout-beside-the-script source (never a release artifact), destination and forced 0644 mode, in-place overwrite of a hand-edited copy with no backup, the silent no-NEEDLE skip (not even `~/.needle` created), the missing-source skip note, and the ordering that an earlier failure places no adapter — pinned with the child `PATH` fully controlled (host tool dirs resolved at test time; a real `needle` inside a pinned dir aborts the suite, so detection never depends on the host), and the note itself held in a three-way pin (`needle_adapter_note_rows_match_the_installer_and_name_live_pinning_tests`, `needle_adapter_note_ordering_claims_match_the_installer_s_control_flow`): every Semantics clause matched verbatim in the note, mapped to the `install.sh` fragment implementing it and to the enforcing test named in the note's Hermetic-coverage section, with the position claims checked against `install.sh`'s source order. Hermetic — no network, no real download (asset names pinned to x86_64, the only architecture CI publishes) |
| `tests/install_sh_arch.rs` | Platform matrix for `install.sh`'s `uname` → release-asset mapping (claudepr-b583cd6a): `uname -s`/`-m` stubbed via PATH so outcomes never depend on the host — the supported Linux/x86_64 row fetches and verifies the `x86_64-linux` assets, and every unsupported row exits 1 with the actionable supported-platform message (platform named, matrix stated, way forward given) before any download starts and with nothing placed; refusal cases serve a fully valid x86_64 release so a reintroduced arch→asset mapping would fail as a download error, which the asserts distinguish from the up-front refusal. Hermetic (`file://` release dir, fake `claude`) |
| `tests/install_sh_release_source.rs` | Contract pin for `install.sh`'s *default* release source (claudepr-5e546c72) — the path every other installer suite leaves unexercised, since they all serve their fake releases through the `CLAUDE_PRINT_RELEASE_URL` override. A recording `curl` shim first on the child `PATH` logs every URL install.sh requests and serves a fake release's bytes by basename (a missing name exits 22, curl's `-f` 404 code); with the override *removed* from the child env, the default run must fetch exactly `sha256sums.txt` first and then `claude-print-x86_64-linux`/`mock_claude-x86_64-linux` from `https://github.com/jedarden/claude-print/releases/latest/download` and install both artifacts end to end — no fetched URL may carry a tag segment and install.sh may name no `releases/download/<tag>` URL at all (the default floats to the latest release; the version discipline lives in the publisher, whose `VERSION=$(grep -m1 '^version' Cargo.toml)` and `gh release create "v${VERSION}"` wiring is cross-pinned); the `REPO` slug is parsed out of install.sh and must equal every `--repo` flag in the WorkflowTemplate, its Forgejo clone URL, and the README's distribution-channel naming; and the README Install section's default-source sentences (GitHub-only host, the base-URL redirect, "a mirror can redistribute the artifacts but cannot bypass verification", the asset names) are pinned against the installer's declared default line and this suite. The declared-default-line pin complements the behavioral one: the run proves the script resolves the default, the source pin proves it declares it in the documented shape. Hermetic — no network (the shim answers every fetch locally), temp `HOME`, fake `claude` (asset names pinned to x86_64, the only architecture CI publishes) |
| `tests/platform_matrix_docs.rs` | Doc-consistency pin binding the README "Supported platforms" matrix to the artifacts it describes (claudepr-d0b97e47): derives the published asset names from the `claude-print-ci` WorkflowTemplate (its `rustup target add` set is exactly `x86_64-unknown-linux-musl` and its build target is uname-derived, so a successful release can only carry `claude-print-x86_64-linux`/`mock_claude-x86_64-linux`) and asserts the README matrix table and its CI claims, the Prerequisites cross-link, `install.sh`'s single `Linux-x86_64 → x86_64-linux` mapping, refusal matrix line, gate-before-download ordering, and the installer suites' fake-release constants against them — CI widening the release, an asset rename, a regained installer mapping, or deletion of the section all fail the build. Library-level: reads README/WorkflowTemplate/install.sh/installer test sources, spawns nothing; per-row installer *behavior* stays in `tests/install_sh_arch.rs` |
| `tests/release_runbook_docs.rs` | Doc-consistency pin binding `docs/notes/release-runbook.md`, the Forgejo→GitHub release runbook, to the `claude-print-ci` WorkflowTemplate it documents (claudepr-8d43bae4) — the territory `tests/platform_matrix_docs.rs` deliberately leaves uncovered: publication *order* (tag pushed to Forgejo before `gh release create` references it — the mirror prunes GitHub-only refs and GitHub demotes a release whose tag was deleted; draft/publish idempotency before the build; manifest generation before the upload), manifest coverage (the `sha256sum` line's inputs equal the four upload args minus `sha256sums.txt` itself, bare filenames, with install.sh's `./`-tolerant parser cross-referenced), and mode/version wiring (the `tag` parameter's verify-only/release split, version read from the cloned tree's `Cargo.toml`, `--check`-then-`--version` acceptance pair, `last-claude-version.txt` travel). Re-derives the asset names from the toolchain set independently and asserts the runbook's six sections, README cross-links, and coverage table — reordering publication, attaching an upload with no manifest entry, renaming an asset, or deleting a section all fail the build. Library-level: reads the runbook, WorkflowTemplate, README, and install.sh; spawns nothing |
| `tests/billing_canary.rs` | Pins `scripts/billing-canary.sh`'s flag contract through a fake `claude-print` (bash fixtures, hermetic): the pooled leg adds exactly `--pool-socket <path>`, neither leg ever carries a print/API-path flag, and the die-during-warmup / never-ready daemon shapes plus the stateless fallback produce the scripted outcomes |
| `tests/install_billing_canary.rs` | Hermetic pin of `scripts/install-billing-canary.sh`, the documented "Install on each host" workflow (bead claudepr-4ba48681): redirected `HOME`/`XDG_CONFIG_HOME` and a child PATH of one fake-bin dir (no real systemd reachable, so absent fakes are genuinely absent) verify the two scripts and two units land byte-identical at the documented paths with 0755/0644 modes (dirs 0700/0755), the service still ExecStarts the libexec copy the installer places, `daemon-reload` precedes `enable --now`, a second run is idempotent and restores drifted copies/modes, missing `systemctl`/`claude-print` aborts with exit 1 before anything is written (no partial install), a failed enable never reports success, and the linger warning fires only when `Linger=no` |
| `tests/billing_entrypoint_contract.rs` | The billing-entrypoint contract, both halves (bead claudepr-6274dcc6): the child env forces exactly one `CLAUDE_CODE_ENTRYPOINT=cli` over an inherited `sdk-cli`, the phantom `CLAUDE_CC_ENTRYPOINT` stays out of operational surfaces, `--check` prints the billing row, and `scripts/check-billing.sh`'s JSON-evidence half is pinned hermetically through transcript fixtures (sandboxed `HOME` + `CLAUDE_PRINT_TRANSCRIPTS_DIR`): top-level evidence found past nested decoys, exact-path mode grades only the file it is given even when a newer transcript contradicts it, default mode selects the newest `.jsonl` under the discovery tree (recursive, extension-filtered, both mtime orderings, plus the real `$HOME/.claude/projects` default), partial/empty/non-JSON transcripts fail closed, and every exit code is pinned — 0 pass, 1 billing-or-input failure, 2 usage |
| `tests/stdin_limit.rs` | stdin prompt-size limit (T-2): stdin enforces the same 10 MB `PROMPT_MAX_BYTES` ceiling as `--input-file`; oversize, empty, and NUL-byte input are rejected before the child is spawned (reaching the session error against an inline mock proves the prompt passed validation) |
| `tests/watchdog.rs` | Watchdog timeout for silent children (no output + no Stop hook) |
| `tests/binary_e2e.rs` | Binary-level end-to-end via the *compiled* binary + mock-claude (exit codes, stdout/stderr contract, child-argv forwarding: hook-inheritance modes across CLI flag and `inherit_hooks` config, `--dangerously-skip-permissions`) |
| `tests/help_version_e2e.rs` | Binary-level `--help`/`--version` contract (claudepr-be05847d): invokes the compiled `claude-print` with `mock-claude` as the backend (same `current_exe()`-relative bin resolution as `binary_e2e`) — `--help` exits 0 with the full help text on stdout, empty stderr, carrying the about line, exact usage line, `serve`, and the drop-in-compat flags; `--version`/`-V` exit 0 with a single exact stdout line and empty stderr, degrading to `not found` when the claude binary is absent; and an exec-sentinel backend proves the early-exit flags spawn no child. Hermetic, credential-free |
| `tests/stream_json_incremental.rs` | Incremental stream-json forwarding through the real binary (events emitted mid-session, not post-burst) |
| `tests/stream_json_cleanup.rs` | Stream-json reader thread cleanup on all exit paths (verifies plan invariant INV-8) |
| `tests/stream_json_contract.rs` | Golden replay of the stream-json output contract (contract: `docs/notes/stream-json-contract.md`; fixtures version-pinned like the transcript captures — the `v2.1.282` in the filename is the pinned Claude version, re-measured live 2026-09-25 by claudepr-b590e46d with the `v2.1.270` family retained as history): a full replay of `stream_json_golden_v2.1.282.input.jsonl` (PTY-shaped, one instance of every byte-level case: a 2.1.282 `mode` record, compact and spaced JSON, unicode, a `thinking` block, split assistant records sharing a `message.id`, a blank line, a CRLF line) through the real reader must be byte-identical to `stream_json_golden_v2.1.282.expected.jsonl`; identity binding with a snapshot offset forwards only post-injection lines without duplicating them; incrementally arriving lines replay in order; the pinned `transcript_v2.1.233` capture forwards verbatim including its `result` record; a final record without a trailing newline is still LF-terminated; and the two synthesized `result` error objects byte-match `stream_json_golden_v2.1.282.errors.jsonl`. The reader thread is driven in-process — no binary spawned |
| `tests/transcript_race_e2e.rs` | AS-6 end-to-end test: Stop-before-JSONL-flush race (bead bf-3isy) |
| `tests/stop_duplicate_firings_e2e.rs` | Degraded-run regression: duplicate/spurious extra Stop firings (`MOCK_EXTRA_STOPS`) still yield exactly one clean result (bead claudepr-8dcf53ce) |
| `tests/stop_sparse_payloads_e2e.rs` | Sparse Stop payload regression: absent optional fields derive, fall back to `last_assistant_message`, or produce a bounded setup error — across text/json/stream-json (`MOCK_OMIT_*`, `MOCK_WRITE_DERIVED_JSONL`, `MOCK_UNKNOWN_FIELDS`; bead claudepr-f3ed858a) |
| `tests/stop_delayed_payload_e2e.rs` | FIFO keeper-lifetime regression (key invariant 7): a Stop payload withheld ~1.5 s (`MOCK_DELAY_STOP`) is still received exactly once through the live event loop — no premature exit, no lost write, normal cleanup — plus a poller-level delayed hook-shaped write pin (bead claudepr-a847d4de) |
| `tests/transcript_flush_window.rs` | Flush-window regression: final assistant line absent/truncated on first read, present on retry; decoy `last_assistant_message` suppressed; bounded retries; text/json/stream-json all carry the complete final message |
| `tests/pool_protocol_compat.rs` | Pool-socket wire-contract pins, in process (spec `docs/notes/pool-socket-protocol.md`; its Test map points every documented claim here or at the e2e suites): serde wire frames against literal JSON (tag/code registries, defaults, unknown-field tolerance, the legacy-assignment parse that detects old daemons, malformed bodies), the real `PoolClient` against hand-rolled fake daemons on real Unix sockets (happy path with a genuine SCM_RIGHTS fd transfer, every documented refusal code and its fallback classification, every catalogued malformed shape, budget enforcement, the frames the client itself emits), and a real `PoolServer` driven by a raw wire client (refusal frames; malformed requests answered by close-never-reply with the daemon staying healthy). No workers, no subprocesses |
| `tests/pool_socket_e2e.rs` | `--pool-socket` client matrix end-to-end through the compiled CLI (bead claudepr-c7824b71): text/json/stream-json over an acquired worker, stateless fallback for absent and stale sockets, three sequential clients with teardown/replace and zero cross-caller leakage, and three malformed-daemon acquire shapes (close mid-exchange, wrong-shape response, assignment without fd) failing safely within the caller timeout |
| `tests/pool_adversarial_e2e.rs` | Pool concurrency proofs (ADR-005 umbrella claudepr-a03e32d7): concurrent clients each drive a distinct worker with proven session↔worker binding (INV-9, INV-11), and three same-cwd stream-json clients under pool concurrency forward only their own session's events — the end-to-end proof the transcript-guessing defect is dead (claudepr-a927ec0c) |
| `tests/pool_failure_e2e.rs` | Pool failure paths end-to-end against REAL daemons/clients (bead claudepr-c470b8aa): daemon SIGKILLed mid-handoff (protocol failure, not fallback) and SIGSTOPped silent (budget expiry, no leak, recovery), daemon crash mid-drive (client still finishes inside `--timeout`), SIGKILLed client's worker orphaned but never reassigned (INV-9, INV-13), manager restart recovering on the same socket path with ownership-checked cleanup, and stateless-fallback output parity vs no-flag baselines across absent/stale/unavailable sockets in all three formats |
| `tests/serve.rs` | `serve` subcommand end-to-end (bead claudepr-7f088327), same hermetic strategy as `binary_e2e` (`--claude-binary` pinned to mock-claude): serve enters the server path and never falls through to prompt validation; `--pool-size 0`/over-max/non-numeric/negative exit 2 before any spawn; unbindable socket paths fail fast naming the exact path; the socket node is 0600 under any umask; SIGINT/SIGTERM teardown is bounded, reaps every worker, and repeated/second signals neither wedge nor respawn; a foreign file at the socket path survives shutdown, while the daemon's own node is removed even when a foreign file preceded the bind; a non-serve invocation is behaviorally unchanged |
| `tests/output_format_contracts.rs` | Output-format contract pin (bead claudepr-1a89e5b4): loads `tests/fixtures/output_format_examples_v1.json` and checks three layers — every fixture case replayed through `emit_success`/`emit_error`/the stream-json reader with stdout/stderr byte-compared against the expected examples, the fixture's error table (variant → `subtype()`/`exit_code()`/`message()`) asserted against `ClaudePrintError`'s accessors, and every `documented: true` example required to appear verbatim in `docs/notes/output-format-contracts.md` — so implementation, fixture, and doc change together in one commit or the test fails |
| `tests/fixtures/` | Version-pinned hermetic fixtures: `claude_contracts_v2.1.282.json` (the pinned measured-hook-contracts fixture — `tests/claude_contracts.rs` loads this one; original measurement `claude_contracts_v2.1.270.json`, bead claudepr-6ef2541c, re-pinned to 2.1.282 by claudepr-3094ab2e with `claude_contracts_v2.1.281.json` as the earlier same-day run — per-version history retained per `docs/notes/claude-contract-probes.md`), `terminal_probes_v2.1.282.json` (DEC probe traffic, via `scripts/probe-tui-terminal-probes.py`; the prior `terminal_probes_v2.1.270.json` capture is retained as the compatibility baseline — `tests/terminal.rs` asserts the recognized probe inventory is identical across the two), `slug_vectors_v2.1.263.json` (live-verified `cwd_to_slug` vectors), `startup_trust_dialog_v2.1.263.txt` (trust-dialog shape), `transcript_v2.1.{168,233}.jsonl` (print-mode transcript shapes), `output_format_examples_v1.json` (claude-print's own output-format contract — version-stamped by contract version `v1`, not by the Claude version; pinned by `tests/output_format_contracts.rs`), `config_contract_examples_v1.json` (claude-print's own config-file contract, stamped by contract version `v1` — pinned by `tests/config_contract.rs`), and the `stream_json_golden_v2.1.282.{input,expected,errors}.jsonl` triple (golden stream-json replay, re-measured against live 2.1.282 on 2026-09-25 — claudepr-b590e46d — with the `stream_json_golden_v2.1.270.{input,expected,errors}.jsonl` triple retained as measurement history; pinned by `tests/stream_json_contract.rs`). Re-pin version-stamped fixtures through the probe scripts after a Claude Code update; never hand-edit one — and every active family plus the doc stamp must move to one version together, enforced always-on by `tests/contract_maintenance.rs::active_fixture_families_share_one_pinned_version` |

### Execution requirements

Which targets run under a plain `cargo test`, and what each additionally
needs. The classification table is exhaustive: every `tests/*.rs` target
appears in exactly one row, in exactly one execution group
(`config_error_helpers` is the helper carve-out — a shared `mod`-included
module, not an execution group; `tests/integration/` is a module directory
of `integration`, not a target). A new target that fits none of the groups
is documentation drift: classify it here in the commit that adds it.
`tests/docs_test_classification.rs` guards the table in CI (bead
claudepr-26cf624a): it re-derives the target set from `tests/`, fails on
any unclassified, stale, or duplicated row, and verifies each group's
dependency claim against the target's actual compilation unit (the target
file plus everything it pulls in via `mod …;`) — `CARGO_BIN_EXE_*` /
`current_exe()` for compiled binaries, spawning with no binary locator for
repo scripts, a `"claude"` subprocess probe for real-claude, and no
`std::process::Command` for library-level. It also cross-checks the
Ignored table below against the `#[ignore]`d tests actually present in the
tree, and the §"Test structure" table against the target list.

| Target | Group | Execution notes |
|--------|-------|-----------------|
| `integration` | compiled-binaries | + `integration/scenarios.rs`; high-level scenarios through `mock_claude` and the shared helpers |
| `pty_integration` | compiled-binaries | mock-claude under a real PTY; PTY capability required |
| `binary_e2e` | compiled-binaries | compiled `claude-print` + mock-claude |
| `help_version_e2e` | compiled-binaries | compiled `claude-print`; mock-claude and exec-sentinel backends |
| `serve` | compiled-binaries | compiled CLI through the `serve` subcommand, mock-claude backend |
| `pool_socket_e2e` | compiled-binaries | compiled CLI; client matrix across three output formats |
| `pool_adversarial_e2e` | compiled-binaries | compiled CLI; concurrent clients |
| `pool_failure_e2e` | compiled-binaries | compiled CLI + real daemons/clients |
| `stream_json_incremental` | compiled-binaries | compiled binary; mid-session event forwarding |
| `transcript_race_e2e` | compiled-binaries | mock-claude; one `#[ignore]`d timing race test (Ignored table) |
| `stop_duplicate_firings_e2e` | compiled-binaries | mock-claude (`MOCK_EXTRA_STOPS`) |
| `stop_sparse_payloads_e2e` | compiled-binaries | mock-claude (`MOCK_OMIT_*`) |
| `stop_delayed_payload_e2e` | compiled-binaries | mock-claude (`MOCK_DELAY_STOP`) |
| `sigint_forwarding_e2e` | compiled-binaries | mock-claude child under a real PTY |
| `watchdog` | compiled-binaries | mock child with silent output |
| `home_unset` | compiled-binaries | binary cases; the rest is lib-level |
| `home_provisioning_recipes` | compiled-binaries | every test drives `CARGO_BIN_EXE_claude-print`; the chroot matrix additionally needs `unshare`/`chroot`/`ldd` (`mount` + `sh` for the read-only-tmpfs leg) and skips with a reason where user namespaces are unavailable |
| `claude_config_dir_contract` | compiled-binaries | real child through the public `PtySpawner`, plus a binary e2e leg against mock-claude |
| `stdin_limit` | compiled-binaries | compiled binary; inline mock child |
| `config_parse_errors` | compiled-binaries | compiled binary through the shared helpers |
| `config_startup_errors` | compiled-binaries | compiled binary through the shared helpers |
| `config_entry_point_scope` | compiled-binaries | compiled CLI across `--version`/`--check`/`serve` fast-fail plus one live daemon; mock-claude backend |
| `billing_entrypoint_contract` | compiled-binaries | `CARGO_BIN_EXE_claude-print` for the `--check` half; the check-billing half is a `bash` subprocess (repo-scripts work, but the binary half owns the row) |
| `contract_maintenance` | repo-scripts | gate + detector under `bash` with `claude`/`gh`/`cargo` stubbed on a single-entry PATH of real-coreutils symlinks; the real-environment leg degrades to `unknown` when `claude` is absent rather than skipping |
| `contract_drift_watch` | repo-scripts | `scripts/contract-drift-watch.sh` (which itself runs the repo's detector) under `bash` with `claude`/`bead` stubbed on a single-entry PATH of real-coreutils symlinks; state redirected via `CLAUDE_PRINT_DRIFT_STATE_DIR` |
| `install_contract_drift_watch` | repo-scripts | `scripts/install-contract-drift-watch.sh` with fake `systemctl`/`bead`/`loginctl` plus real coreutils — panics if those are missing |
| `install_sh` | repo-scripts | `install.sh` under `sh` against a `file://` fake release with a fake `claude` |
| `install_sh_arch` | repo-scripts | same, with `uname` stubbed so outcomes never depend on the host |
| `install_sh_release_source` | repo-scripts | `install.sh` under `sh` with `CLAUDE_PRINT_RELEASE_URL` removed — the default path — against a recording `curl` shim that serves a fake release by basename, plus a fake `claude` |
| `billing_canary` | repo-scripts | `scripts/billing-canary.sh` under `bash` behind a fake `claude-print` |
| `install_billing_canary` | repo-scripts | `scripts/install-billing-canary.sh` with fake `systemctl`/`claude-print`/`loginctl` plus real coreutils — panics if those are missing |
| `sigwinch_forwarding_e2e` | repo-scripts | an `sh` trap child under a real PTY via the library's `PtySpawner` — PTY capability required, like `pty_integration` |
| `version_compat` | real-claude | only `test_claude_version_recorded` (`--version` probe, writes the CI version artifact); the rest is library-level |
| `flag_compat` | real-claude | argv-parse probe with a deliberately-unknown-flag inverse |
| `cli` | library-level | |
| `emitter` | library-level | |
| `startup` | library-level | one `#[ignore]`d slow test (Ignored table) |
| `terminal` | library-level | |
| `transcript` | library-level | |
| `tui_transcript` | library-level | |
| `hooks` | library-level | |
| `stop_poller` | library-level | |
| `transcript_flush_window` | library-level | |
| `stream_json_cleanup` | library-level | |
| `docs_slug_consistency` | library-level | |
| `docs_pool_contract` | library-level | |
| `platform_matrix_docs` | library-level | reads README, the CI WorkflowTemplate, install.sh, and the installer suites' sources; spawns nothing |
| `release_runbook_docs` | library-level | reads the release runbook, the CI WorkflowTemplate, README, and install.sh; spawns nothing |
| `nested_session` | library-level | |
| `output_format_contracts` | library-level | |
| `benchmark_reproducibility` | library-level | |
| `config_contract` | library-level | mutates process-global cwd/`HOME`/`XDG_CONFIG_HOME`, serialized on one lock |
| `claude_config_dir_docs_contract` | library-level | mutates process-global `HOME`/`CLAUDE_CONFIG_DIR`, serialized on one lock |
| `pool_protocol_compat` | library-level | real Unix sockets in-process, no workers |
| `stream_json_contract` | library-level | golden replay through the real stream-json reader thread |
| `claude_contracts` | library-level | always-on half only; the spawning half is the two `#[ignore]`d live probes (credentials-gated — next section) |
| `docs_test_classification` | library-level | this guard — reads AGENTS.md and the `tests/` tree; spawns nothing |
| `docs_build_layout` | library-level | AGENTS.md build-docs guard — reads AGENTS.md, Cargo.toml, the CI WorkflowTemplate, and the tests/ tree; resolves built artifacts through cargo's real locators; spawns nothing |
| `docs_build_commands` | library-level | AGENTS.md build-commands guard — reads AGENTS.md, README.md, docs/, and scripts/; spawns nothing |
| `home_env_guard` | library-level | scans `src/` and the HOME strategy doc; spawns nothing |
| `target_path_guard` | library-level | scans `tests/`, `scripts/`, `build.rs`, and the CI WorkflowTemplate (for the musl needle); spawns nothing |
| `config_error_helpers` | helper | shared module `mod`-included by `integration`, `config_parse_errors`, `config_startup_errors`; compiled standalone too, where its `cfg(test)` fixture unit tests run |

**Compiled binaries.** A default `cargo test` builds `claude-print` and
`mock-claude` first, so nothing extra is needed. `cargo test --lib` runs only
the `src/` inline unit tests — no `tests/` target at all. The rows above
spawn a compiled binary at runtime (and therefore mean nothing under
`--lib`, and need the binaries present under a selective `--test <name>`
run) — the guard pins that each row's compilation unit references
`CARGO_BIN_EXE_*` or `current_exe()`, the two locators the suites use (see
"Where the build output lands").

**Repo scripts, stubs, and host-tool children.** These spawn subprocesses
but need neither the compiled binaries nor the real `claude`: they drive the
repo's shell scripts or stub/fake children against redirected
`HOME`/`PATH`, so their only host requirements are a POSIX `sh`/`bash` and
coreutils — no network, no credentials, no skip conditions; they always
run. The guard pins the split: each row's unit spawns something
(`std::process::Command` or the library's `PtySpawner`) while referencing
neither binary locator.

**Real `claude` on PATH** (silent skip when absent — never a failure):
both rows are credential-free — an unknown *option* is rejected by the
child's argument parser before any model request, which is exactly what
they probe.

**Library-level — spawn no process.** The guard pins the claim literally:
no `std::process::Command` anywhere in the row's compilation unit. The one
sanctioned exception is a target whose spawning code sits entirely behind
`#[ignore]`d tests, with the carve-out declared in the row's notes —
`claude_contracts`'s live probes, which are also credentials-gated (next
section).

**Credentials (API auth).** Required only by the two `#[ignore]`'d
`claude_contracts` live re-measurements — which *check* for
`ANTHROPIC_AUTH_TOKEN` / `ANTHROPIC_API_KEY` / `CLAUDE_CODE_OAUTH_TOKEN` and
skip silently without one — and by the live probe/canary scripts in the next
section. Everything under `cargo test` runs without credentials.

**Ignored (`#[ignore]`)** — excluded from default runs; execute explicitly
with `cargo test -- --ignored`:

| Test | Why it is ignored |
|------|-------------------|
| `claude_contracts::live_settings_flag_merges_across_sources` | live probe: real claude + API auth, sandboxed HOME (PO-1/OQ-1 re-measurement) |
| `claude_contracts::live_empty_setting_sources_suppresses_but_settings_file_loads` | live probe: real claude + API auth, sandboxed HOME (OQ-2 re-measurement) |
| `startup::test_hard_timeout_fires_after_45s_with_few_bytes` | slow by design: sleeps 45 s (EC-8 hard-timeout pin) |
| `transcript_race_e2e::as6_transcript_race_delayed_jsonl_write` | timing-sensitive race window (AS-6): run deliberately, not incidentally |

### Probes, drift checks, and release verification (`scripts/` + `install.sh`)

The live probes and gates the plan and `docs/notes/` reference. These are the
maintenance surface for the version-pinned fixtures above — not part of
`cargo test`, and (except where noted) requiring the real `claude` with API
auth. All live-probe isolation follows the same contract: `HOME` redirected
into a throwaway sandbox, `CLAUDECODE*` session markers scrubbed,
`CLAUDE_CODE_ENTRYPOINT=cli` forced — the child-env contract of `src/pty.rs`.

| Script | What it does | Requires |
|--------|--------------|----------|
| `scripts/check-claude-version-bump.sh` | Claude-version drift detection: compares live `claude --version` against the **Measured against:** stamp in `docs/notes/claude-contract-probes.md` and the active `claude_contracts_v*` / `stream_json_golden_v*` fixture pins (unreferenced files are exempt history), and rejects active pins that disagree with each other before consulting `claude` at all. Exit 0 = evidence current, 1 = drift (re-run due), 2 = cannot determine (or divergent pins). The detection step of the probe maintenance workflow (plan R-2 CI alert) | `claude` on PATH; read-only, credential-free |
| `scripts/contract-drift-watch.sh` | Scheduled drift watch (bead claudepr-e6e54313): runs the credential-free detection step outside CI so a Claude auto-update between pushes still alerts — daily systemd user timer (`claude-print-contract-drift-watch.{service,timer}`, installed by `install-contract-drift-watch.sh`, repo pinned via `CLAUDE_PRINT_CONTRACT_REPO`). On drift files exactly one idempotent bead (`--unique-ref claude-contract-drift:live-<version>`) and writes PASS/DRIFT/INDETERMINATE to `~/.local/state/claude-print/contract-drift-watch/last-result`; indeterminate fails the unit loudly without filing. Pinned by `tests/contract_drift_watch.rs` + `tests/install_contract_drift_watch.rs` | `claude` + `bead` on PATH; read-only apart from the state dir and the filed bead |
| `scripts/probe-claude-contracts.sh` | Measures the hook contracts pinned as PO-1/PO-2/OQ-1/OQ-2 (`--settings` merge, `--setting-sources=` suppression) plus the Stop-per-turn baseline; feeds the `claude_contracts` fixture | real claude + auth (model turns) |
| `scripts/probe-stop-toolallowed.sh` | Authoritative Stop-count probes with tool use actually permitted (`--allowedTools Bash`); the earlier un-permitted probes measured degraded runs. Print + TUI arms with per-firing payload detail | real claude + auth (multi-round tool use) |
| `scripts/probe-tui-second-turn.sh` | Decisive TUI once-per-turn Stop probe: watches both the hook firing log and the TUI screen text so a failed prompt injection is distinguishable from a second turn that fires no Stop | real claude + auth; TUI/PTY |
| `scripts/probe-stop-edge-contracts.sh` | The two edge measurements the 2.1.281/2.1.282 re-pins had left at their 2.1.270 originals, re-measured 2026-09-25 (claudepr-d9553d38) and owned by this script since: Arm S — sleeping-hook cross-source concurrency (project hook sleeps 300 ms, relay 0 ms; 2.1.282: concurrent 12/12 event pairs, relay-start-first 6/12, proving order is not contractual); Arm D — degraded-path Stop counts on permission-denied tool calls (2.1.282: 15/15 completed denied runs across three invocations fired exactly one Stop; the 2.1.270 extra-Stop hazard did not reproduce). Part of the maintenance gate's `--run-probes` set | real claude + auth (11 model turns) |
| `scripts/probe-tui-stop.py` | PTY driver for `probe-tui-second-turn.sh` (standalone-capable): drives the real TUI under claude-print's child-env contract, counts Stop firings from the hook log | real claude + auth |
| `scripts/probe-tui-terminal-probes.py` | Captures the DEC probe bytes the TUI writes at startup and records them as `tests/fixtures/terminal_probes_v<version>.json` (companion to `docs/notes/terminal-probes.md`); `--answer` replies via the `src/terminal.rs` responder. No model turn | real claude; sandboxed HOME |
| `scripts/check-billing.sh` | AS-4 billing conformance: no argument inspects the newest real transcript (manual release gate); a path argument inspects exactly that file (used by the automated canary so concurrent sessions cannot false-positive). Path selection, discovery, and exit codes pinned hermetically by `tests/billing_entrypoint_contract.rs` | a transcript to inspect; parsing itself is credential-free |
| `scripts/billing-canary.sh` | Automated AS-4 canary: one-turn Haiku session through claude-print, transcript matched by session id; `CLAUDE_PRINT_POOL=1` proves pooled sessions also bill `cli` (INV-15). Installed as a systemd user timer by `install-billing-canary.sh` (+ `.service`/`.timer` units). Flag contract pinned by `tests/billing_canary.rs` | real claude + auth |
| `scripts/bench_startup_overhead.py` | Startup-overhead benchmark (process start → prompt injection) against `mock-claude` only; measures claude-print's own overhead, no model latency. Results in `docs/notes/startup-overhead-benchmark.{md,json}` | compiled `claude-print` + `mock-claude`; hermetic, no credentials |
| `scripts/test_startup_wedge.sh`, `scripts/test_sessionstart_hook.sh`, `scripts/test_exact_claude_print_scenario.sh` | Historical repros from the startup-wedge investigation (untrusted-dir hang, SessionStart-hook interference, exact relay scenario) | real claude + auth; diagnostic provenance, kept in `docs/plan/plan.md`'s tree |
| `scripts/verify_fix.sh`, `scripts/verify-startup-wedge-fix.sh` | Historical verifications of the `--setting-sources=` wedge fix (bf-2u1) | real claude + auth; diagnostic provenance |
| `install.sh` (repo root) | Release installer: verifies every artifact against the published `sha256sums.txt` before installing or executing anything; preserves a `claude-print.prev` rollback copy (semantics and rollback workflow: `docs/notes/installer-rollback.md`) | network + release URL at install time; its verification and rollback logic are tested hermetically by `tests/install_sh.rs` |

### mock_claude

`test-fixtures/mock-claude/` is a workspace member compiled as a separate binary.
It impersonates `claude` for integration tests and is controlled via environment
variables (see its own `README` / source). No real credentials are needed.

To rebuild mock_claude explicitly:
```bash
cargo build -p mock-claude
```

## Module map

| File | Role |
|------|------|
| `src/lib.rs` | Crate root — re-exports public modules for integration tests |
| `src/main.rs` | Entry point: CLI parse, claude binary resolution, calls `session::Session::run()` |
| `src/cli.rs` | Clap argument definitions (`Cli`, `OutputFormat`) |
| `src/config.rs` | Loads `$XDG_CONFIG_HOME/claude-print/config.toml` if set, otherwise `~/.config/claude-print/config.toml` (model default, inherit_hooks, max_turns, timeout_secs) |
| `src/session.rs` | Session orchestrator: installs hooks, spawns PTY child, runs event loop, reads transcript. `Session::run()` is the top-level entry point for a single prompt→response cycle; `Session::run_pooled()` drives an already-acquired pool worker through the same event loop, watchdog deadlines, Stop-FIFO handoff, and emitters. |
| `src/pool.rs` | ADR-005 warm PTY pool: `PoolManager`/`PoolServer` (the `serve` daemon — worker spawn, bounded warmup, SCM_RIGHTS fd handoff, SIGTERM→SIGKILL group teardown, ownership-checked socket cleanup) and the `--pool-socket` client (`AcquiredWorker` release-on-drop, acquire classification into fallback vs hard protocol failure, stateless-fallback contract). `MAX_POOL_SIZE` 256; acquire budget `min(60s, --timeout)`. |
| `src/prompt.rs` | Prompt input validation: NUL byte rejection, file size/type checks for `--input-file` (Security T-2, EC-4) |
| `src/verbose.rs` | `--verbose` timing traces: emits `[claude-print <ms>ms] <message>` to stderr across session lifecycle |
| `src/pty.rs` | Forks child, opens PTY pair, calls `login_tty`; builds the child env pre-fork via `scrub_env` (drops session markers) + `FORCED_ENV` (forces `CLAUDE_CODE_ENTRYPOINT=cli`), passes it to `execvpe`; forwards SIGWINCH/SIGINT |
| `src/startup.rs` | State machine: reads PTY output until trust dialog or idle; auto-dismisses (sends CR), injects prompt via bracketed paste; hard timeout after 45s with <200 bytes |
| `src/event_loop.rs` | Single-threaded `poll(2)` loop (50ms timeout for timer ticks) over PTY master + self-pipe + stop FIFO; calls callback on each chunk |
| `src/hook.rs` | Installs Stop hook via temp dir settings.json; creates FIFO; cleans up on drop |
| `src/poller.rs` | Opens FIFO non-blocking (read + keeper write ends), parses Stop hook payload, derives transcript path from session_id + cwd |
| `src/transcript.rs` | Reads `.jsonl` transcript; extracts last assistant message + token usage |
| `src/emitter.rs` | Formats and writes output (`text`, `json`, `stream-json`); owns the incremental stream-json reader thread (`StreamJsonHandle`) |
| `src/terminal.rs` | Absorbs and discards terminal probe sequences (DA1/DA2/DSR/xtversion) from Ink TUI |
| `src/watchdog.rs` | Watchdog: monitors four deadlines (PTY first-output, stream-json first-output, overall session, Stop-hook) in a background thread; signals timeout via the event-loop self-pipe |
| `src/error.rs` | `Error` enum and `Result` alias |
| `src/util.rs` | `get_home()` — the sole production read of `HOME` and the authoritative strict resolver (policy and call-site table: `docs/notes/home-handling-strategy.md`): unset/empty values and missing, inaccessible, non-directory, or non-writable paths fail with path-specific `Error::Config` setup errors — never a fallback to `/root`, the passwd database, or the cwd. Writability is proven by a short-lived create/write/remove probe (`.claude-print-home-check-*` temp file, removed before return), which mode-bit checks cannot replace (ACL denial, read-only mounts, full filesystems); the env is read via `var_os`, so valid non-UTF-8 Unix paths are preserved. Call sites: pre-dispatch in `main.rs` (every entry point, including early-exit ones like `--version`), `config.rs::Config::default_path()` (only when `XDG_CONFIG_HOME` is unavailable), `poller.rs` (`resolve_stop_info`/`derive_transcript_path`/`projects_dir_for_cwd`), and `session.rs::pretrust_cwd()`; contract pinned end-to-end by `tests/home_unset.rs` |
| `src/check.rs` | `--check` mode: verifies claude binary, openpty, mkfifo, optional mock_claude PTY round-trip, and the billing env-input force (`CLAUDE_CODE_ENTRYPOINT=cli`); warns on orphaned temp dirs |

## Key invariants

These must hold across all changes:

1. **Do not set `CLAUDE_CONFIG_DIR`** — transcripts must land in
   `~/.claude/projects/` (the real config dir). The temp dir is only used for the
   Stop hook settings injection, and it must not redirect the config dir.
   Enforced in the child-env builder: `CLAUDE_CONFIG_DIR` is in `SCRUBBED_ENV`
   (`src/pty.rs`), so claude-print never sets it **and** a value inherited from
   an outer wrapper (agent cleanrooms export it to relocate claude's whole
   config dir) is dropped before `execvpe` — the poller's transcript
   derivation and the stream-json live reader are HOME-rooted and cannot
   follow a redirect. Regression coverage: `tests/claude_config_dir_contract.rs`
   (child env, source wiring, and binary end-to-end transcript placement) plus
   the `scrub_env_*` unit tests in `src/pty.rs` (claudepr-bfe97ce4). mock-claude
   models real claude's redirect (it honors `CLAUDE_CONFIG_DIR` when present),
   so the binary leg relocates the transcript to the decoy on a scrub
   regression and fails instead of passing vacuously.

2. **Clean up the temp dir on all exit paths** — no `claude-print-<pid>-*`
   directories may be left in `$TMPDIR`. The `TempDir` handle in `HookInstaller`
   must remain owned until after the child exits.

3. **Forward SIGINT to the child process** — pressing Ctrl-C must reach `claude`,
   not just terminate `claude-print`.

4. **Never pass `--print` or `--output-format` to the child** — those flags
   activate the API billing path. The entire point is to stay on the PTY/TUI path.

5. **`cc_entrypoint=cli` is the correctness invariant** — the wire-level
   billing header is not an environment variable and is never directly
   observable. The env input claude-print controls is
   `CLAUDE_CODE_ENTRYPOINT`, forced to `cli` in the child (`FORCED_ENV`,
   `src/pty.rs`) regardless of inheritance; verify that half credential-free
   via `--check`. The JSON evidence is the transcript's `entrypoint` field;
   verify it with `scripts/check-billing.sh` (daily canary, plus the manual
   newest-transcript run before each release). AS-4 in the plan documents the
   acceptance criterion. `CLAUDE_CC_ENTRYPOINT` does not exist — no such
   variable is read or set anywhere.

6. **Scrub the parent's session markers from the child env** — an inherited
   session ID makes the child write events into the parent's transcript and
   may skip Stop hook dispatch; an inherited `CLAUDECODE=1` flips it into
   nested-session mode. `scrub_env` drops all four markers
   (`CLAUDE_CODE_SESSION_ID`, `CLAUDECODE`, `CLAUDE_CODE_CHILD_SESSION`,
   `CLAUDE_CODE_SKIP_PROMPT_HISTORY`) and then `FORCED_ENV` appends
   `CLAUDE_CODE_ENTRYPOINT=cli` and `CLAUDE_CODE_FORCE_SESSION_PERSISTENCE=1`,
   overriding any inherited values. The environment is built in the parent
   before `fork()` and passed to `execvpe` — never `setenv`/`unsetenv`
   post-fork (async-signal-safety).

7. **Keep both FIFO ends alive for the full event loop** — `open_fifo_nonblock()`
   returns `(read_fd, keeper_write_fd)`. Both must be stored until after the event
   loop exits. Dropping `read_fd` closes the fd the event loop is polling; dropping
   `keeper` causes `ENXIO` when the hook writes to the FIFO.

## Key implementation notes

- **Event loop ticks on empty slices** — the event loop uses `poll(50ms)` (not
  blocking) and emits an empty-slice tick to the callback on timeout. The callback
  must guard `startup.feed()` and `terminal.feed()` from empty slices — feeding
  empty data resets the idle timer in `StartupSeq`.

- **Watchdog timeout thread is detached, not joined** — `session.rs` spawns the
  `watchdog`'s timeout thread and drops the `JoinHandle` (bound to `_timeout_thread`)
  instead of joining it. The watchdog enforces four deadlines — PTY first-output
  (default 90s), stream-json first-output (default 90s), overall session (default
  3600s), and Stop-hook (default 120s) — and on expiry signals the event loop via
  the self-pipe write fd. Joining would block the main thread for the full deadline
  on early exit; the watchdog thread exits on its own once the child is killed.

- **Stream-json reader cleanup is RAII** — `emitter::StreamJsonHandle::Drop`
  disconnects the drain channel and joins the reader thread, so every return path in
  `Session::run_inner` (success, timeout, signal, child-exit, and `?` propagations)
  joins the reader before returning (plan invariant INV-8). Only the normal Stop path
  calls `signal_drain()` first; error paths drop the handle without signaling.

- **Child cleanup uses `kill_child(pid)`** — `kill_child(pid)` sends SIGTERM, waits
  up to 2s, then SIGKILL. Use this for all child cleanup paths, not bare `waitpid`.

## Pool operations (ADR-005)

The warm PTY pool is opt-in and additive: the ordinary invocation path runs no
pool code unless `--pool-socket` is set. Product-level docs (full flag
descriptions, failure-behavior table, measured startup overhead) live in
README.md §"Warm PTY pool (ADR-005)"; the normative invariant text is plan.md
§Invariants (INV-9 through INV-15). This section is the operator quick
reference.

```bash
# Daemon: keep N workers warm (1–256; 0 or >256 exits 2 before any spawn)
claude-print serve --pool-size 2 --socket /tmp/claude-print-pool.sock --verbose

# Client: ordinary invocation, optionally acquiring a prewarmed worker
claude-print --pool-socket /tmp/claude-print-pool.sock "prompt"
```

Operating limits:

| Limit | Value |
|-------|-------|
| `--pool-size` | 1–256 (`MAX_POOL_SIZE` in `src/pool.rs`; each worker is a full `claude` PTY process) |
| Acquire budget (client) | `min(60s, --timeout)` — bounds the whole connect + request + response + fd transfer; every protocol stage inside it is deadline-bounded |
| Worker warmup | 120 s per worker, enforced on the event-loop timer tick; a timed-out or failed warmup is destroyed and respawned, never handed out |
| Release exchange | 10 s, best-effort — a dead daemon has nothing left to release |
| Socket permissions | 0600 regardless of umask (narrowed umask across the bind, then `set_permissions`) |
| Shutdown | SIGINT/SIGTERM → exit 0; per worker: close PTY master → SIGTERM group → 2 s grace (waitpid-observed) → SIGKILL group → reap |

Failure behavior (the client's ADR-005 contract, `AcquireFailure` in
`src/pool.rs`):

- **Fallback to the stateless session** (exactly one `--verbose` diagnostic,
  identical output/exit code to a no-flag run): socket absent, stale
  (nothing listening), connect refused/timeout, permission denied; and
  well-formed daemon refusals (`pool_full`, `shutting_down`,
  `internal_error`, `acquire_timeout`).
- **Hard error, exit 2, never fallback:** the daemon answered garbage, a
  malformed frame, an incomplete `worker_assigned`, or the fd transfer
  failed — or stayed silent past the acquire budget. The daemon was
  reachable; falling back would mask the breakage behind full-price
  stateless sessions.
- **Mid-drive daemon death:** the client's session is daemon-independent
  once assigned and finishes inside its `--timeout`; the release attempt
  against the dead daemon is bounded and non-fatal.
- **Client killed without destructors:** its worker stays `InUse` forever —
  never reassigned, never destroyed early; reclaimed only at daemon
  shutdown (a daemon-side lease is the deliberate non-fix; see
  `AcquiredWorker`'s doc comment).

Stale-socket handling: a daemon *replaces* whatever sits at its socket path
at bind; at shutdown it removes the node only when it still resolves to the
(dev, ino) it captured at bind time — so a replaced daemon never unlinks the
winner's socket, and a leftover stale node is always safe to `rm` by hand.

Rollback (concrete): (1) remove `--pool-socket` from the invoking config —
for NEEDLE, the `invoke` template in `~/.needle/agents/claude-print.yaml`
(the shipped template does not set it); (2) `kill -TERM` the `serve` process
(clean teardown, exit 0, socket removed). Order does not matter: clients
left pointing at a dead/stale socket fall back statelessly (INV-10). A
binary-level rollback uses the `claude-print.prev` copy `install.sh`
preserves.

Pool invariants (full table with tests in plan.md §Invariants):

- **INV-9** — one request per member: at most one prompt per pooled worker;
  release destroys and replaces, never re-handshakes a used worker.
- **INV-10** — stateless fallback compatibility; reachable-but-broken is a
  hard exit 2, never a silent fallback.
- **INV-11** — no cross-request transcript/session contamination, including
  same-cwd stream-json concurrency (per-drive identity binding).
- **INV-12** — bounded client return: nothing pool-related blocks a client
  past `min(60s, --timeout)`, and mid-drive daemon death never does either.
- **INV-13** — orphan containment: killed client ⇒ worker held `InUse` until
  daemon shutdown.
- **INV-14** — cleanup: no leaked PTY master fds, no survivors, no zombies;
  socket removed only when still ours.
- **INV-15** — pooled sessions bill `cc_entrypoint=cli` (AS-4 on the pool
  path); verify with `CLAUDE_PRINT_POOL=1 ./scripts/billing-canary.sh`.

The startup-overhead benchmark (`scripts/bench_startup_overhead.py`, results
in `docs/notes/startup-overhead-benchmark.{md,json}`) measures process start
→ prompt injection under the `mock-claude` fixture only. It isolates
`claude-print`'s own overhead; it establishes **no model-latency savings**.

## Bead workflow

Beads use the **bead-rs `bead` CLI** — canonical across this environment since
2026-08-14. The backend is declared in `.needle.yaml` (`bead_cli: backend:
bead-rs`); the live store is SQLite at `.beads/beads.db` with a git-tracked
durable checkpoint under `.beads/checkpoint/`; bead IDs use the `claudepr`
prefix (workspace identity in `.beads/config.json`).

> **Never run `bf` (bead-forge) against this workspace — `bf` is retired and
> not installed on this box.** Running the wrong CLI does not fail cleanly:
> `bf` reports a generic SQLite "no such column" error rather than "wrong
> tool," and applying the *other* tool's recovery recipe to that error
> silently reinitializes the store with the wrong schema and destroys the
> live data. The on-disk tell is unambiguous here: `.beads/config.json` +
> `.beads/checkpoint/` = bead-rs; a `.beads/config.yaml` + flat
> `.beads/issues.jsonl` would mean bf (this repo has neither). If a bead
> command fails with an unfamiliar schema/column error, stop and re-check the
> backend declaration before attempting any repair.

```bash
# List beads (all / ready frontier only)
bead list
bead list --ready

# Show one bead
bead show <id>            # claudepr-… IDs only — historical bf-* IDs no longer resolve

# Record progress / verification evidence on a bead
bead update <id> --notes "..."

# Atomically claim from the ready frontier (or claim for a named worker)
bead claim
bead claim --assignee <worker>

# Close — non-empty --reason is required (status can't be closed via update).
# Commit the work first; NEEDLE re-verifies close evidence against committed state.
bead close <id> --reason "..."
```

Checkpoint sync and recovery:

```bash
# Database -> checkpoint (idempotent; bead 0.2.x also auto-publishes the
# checkpoint after every successful mutation)
bead sync flush-only

# If beads.db is missing/corrupt/wrong-schema (fresh clone, or someone ran the
# wrong CLI): diagnose read-only first, then rebuild losslessly from the
# git-tracked checkpoint — never by deleting beads.db and re-importing with a
# bf-shaped command (see the warning above).
bead doctor                    # read-only; --repair adds non-destructive fixes only
bead init                      # rebuild schema, keeps committed workspace identity
bead sync import-only --input .beads/checkpoint/forensic.jsonl \
  --restore-into-empty --actor <you>
```

Historical note: code comments, parts of `docs/plan/plan.md`, and everything
under `notes/` still reference `bf-*` bead IDs from the bead-forge era. Those
IDs are provenance, not pointers — they predate the 2026-08-14 bead-rs
migration and do not resolve in this store (`bead show bf-3isy` → "Issue not
found"). Do not rewrite them, and don't try to look them up.

See the **"Beads (bead-rs CLI)"** section of the root workspace `CLAUDE.md`
for the full `bead` CLI reference and gotchas (`bead reopen` clears the
assignee; `bead release` refuses assigned-but-open beads; `--if-revision N`
for optimistic concurrency on worker-contested beads).

## Notes

`notes/` holds per-bead NEEDLE worker scratch notes — one file per bead,
named after the bead's ID (`notes/claudepr-*.md`; the existing `bf-*.md`
files are historical artifacts of the bead-forge era, kept for traceability).
These are worker journals, **not** product documentation; they are deliberately
tracked (kept simple — history is appended, never rewritten). Routine status
and verification evidence belongs on the bead itself (`bead update --notes`,
`bead close --reason`) — never create a notes file just to have a commit
artifact. Product design lives in `docs/plan/plan.md`.
