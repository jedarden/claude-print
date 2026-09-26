//! Measured Claude Code runtime contracts (bead claudepr-6ef2541c).
//!
//! The plan pinned three runtime assumptions as unverified — PO-1/OQ-1
//! (`--settings` merge + firing order), PO-2/OQ-2 (`--setting-sources=`
//! suppression), and the Stop-poller's once-per-turn assumption. They were
//! measured against claude 2.1.282 on 2026-09-24 (re-pinned from the 2.1.270
//! measurement of 2026-09-13, via a 2.1.281 run the same day that the host's
//! auto-updater superseded; re-run procedure in
//! docs/notes/claude-contract-probes.md §Maintenance) with the live probe
//! scripts (`scripts/probe-claude-contracts.sh`,
//! `scripts/probe-stop-toolallowed.sh`, and — for the edge measurements: the
//! two the re-pins had left unrepeated, re-measured 2026-09-25, plus the
//! relay-hook timeout-enforcement arm added 2026-09-26 (claudepr-352cf1df) —
//! `scripts/probe-stop-edge-contracts.sh`); the observed values are pinned in
//! `tests/fixtures/claude_contracts_v2.1.282.json` and documented in
//! `docs/notes/claude-contract-probes.md`.
//!
//! The always-on tests below assert that what claude-print *does* (child argv,
//! relay settings schema) still matches the spelling/schema that was verified
//! live, so a silent change here cannot drift away from measured reality.
//!
//! The `#[ignore]`-gated tests re-measure the cheap contracts (merge,
//! suppression — observable on SessionStart, no model turn needed) against the
//! real claude binary and fail if Claude Code's behavior diverges from the
//! fixture. Run them explicitly:
//!
//! ```text
//! cargo test --test claude_contracts -- --ignored --nocapture
//! ```
//!
//! They skip (pass with a printed notice) when `claude` is not on PATH or no
//! API auth is available in the environment. Like the probe scripts, they
//! never touch the real `~/.claude`: the child runs with `HOME` redirected
//! into a throwaway sandbox whose `.claude.json` pre-seeds trust for the probe
//! cwd only, and `CLAUDECODE*` session markers are scrubbed exactly as
//! `src/pty.rs` does.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use claude_print::hook::HookInstaller;
use claude_print::session::{LaunchOptions, Session};
use serde::Deserialize;

const FIXTURE: &str = include_str!("fixtures/claude_contracts_v2.1.282.json");

/// The measured contracts, as recorded by the probe run.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
struct ContractFixture {
    claude_version: String,
    measured_at: String,
    contracts: Contracts,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
struct Contracts {
    /// PO-1/OQ-1: `--settings <file>` hooks fire alongside standard-source
    /// hooks (merge, not replace).
    settings_flag_merges_not_replaces: bool,
    /// PO-1/OQ-1: every loaded source's hooks fire on each hook event.
    all_loaded_sources_hooks_fire: bool,
    /// OQ-1: cross-source firing order is *not* contractual — the `--settings`
    /// relay hook typically starts after the standard-source hooks, but
    /// concurrent (and start-order-flipped) firing was measured, and
    /// re-confirmed on the pinned version by the dedicated sleeping-hook
    /// probe (`probe-stop-edge-contracts.sh` Arm S, 2026-09-25: concurrent
    /// execution in 12/12 event pairs, relay started first in 6/12).
    /// claude-print must not depend on order.
    cross_source_hook_order_guaranteed: bool,
    /// OQ-2: empty `--setting-sources=` suppresses every standard source.
    setting_sources_empty_suppresses_standard_sources: bool,
    /// OQ-2: the `--settings` file still loads when `--setting-sources=` is
    /// passed — suppression does not extend to it.
    settings_file_loads_despite_setting_sources_empty: bool,
    /// PO-2 fallback spelling: `--setting-sources=none` is rejected by the CLI.
    setting_sources_none_accepted: bool,
    /// Stop contract: a completed single-turn run fires exactly one Stop per
    /// loaded source.
    stop_firings_single_turn_completed_run_per_loaded_source: u32,
    /// Stop contract: a completed multi-round tool-using turn (tools actually
    /// permitted) fires exactly one Stop.
    stop_firings_multi_round_tool_use_completed_turn: u32,
    /// Stop contract: a run cut off by `--max-turns` fires no Stop at all.
    stop_firings_max_turns_cutoff: u32,
    /// Relay-hook timeout: a hook sleeping past its configured per-hook
    /// `timeout` (the field `src/hook.rs` sets to 10 on both relay hooks) is
    /// killed by Claude Code — it logs its start and its end line never
    /// appears (`probe-stop-edge-contracts.sh` Arm T).
    hook_timeout_kills_overrun_hook: bool,
    /// Relay-hook timeout: the session proceeds despite the killed hook —
    /// exit 0 with the reply rendered, the process exiting on the order of
    /// the configured timeout rather than the hook's sleep (measured 2.1.282:
    /// 8/8 runs exit 0, claude exiting 5.0 s after the Stop hook's start
    /// against a 5 s timeout / 30 s sleep). This is the
    /// "does not wait beyond the 10s timeout" clause of
    /// `docs/notes/hook-design.md` §Relay Hook.
    hook_timeout_overrun_session_proceeds: bool,
}

fn fixture() -> ContractFixture {
    serde_json::from_str(FIXTURE).expect("contract fixture must parse")
}

// ── Fixture sanity ───────────────────────────────────────────────────────────

#[test]
fn fixture_pins_measured_contracts() {
    let f = fixture();
    assert!(
        f.claude_version.starts_with("2."),
        "unexpected version form"
    );
    assert!(!f.measured_at.is_empty());
    // The measured values the codebase and docs now rely on. If a live
    // re-verification (the --ignored tests below) contradicts any of these,
    // the fixture, docs, and this assertion must be updated together.
    assert!(
        f.contracts.settings_flag_merges_not_replaces,
        "PO-1 measured: --settings merges"
    );
    assert!(
        f.contracts.all_loaded_sources_hooks_fire,
        "PO-1 measured: every loaded source fires"
    );
    assert!(
        !f.contracts.cross_source_hook_order_guaranteed,
        "OQ-1 measured: cross-source order is not contractual (concurrent firing observed)"
    );
    assert!(
        f.contracts
            .setting_sources_empty_suppresses_standard_sources,
        "OQ-2 measured: empty spelling suppresses"
    );
    assert!(
        f.contracts
            .settings_file_loads_despite_setting_sources_empty,
        "OQ-2 measured: --settings independent of the flag"
    );
    assert!(
        !f.contracts.setting_sources_none_accepted,
        "PO-2 fallback spelling rejected by claude 2.1.282"
    );
    assert_eq!(
        f.contracts
            .stop_firings_single_turn_completed_run_per_loaded_source,
        1
    );
    assert_eq!(
        f.contracts.stop_firings_multi_round_tool_use_completed_turn,
        1
    );
    assert_eq!(f.contracts.stop_firings_max_turns_cutoff, 0);
    assert!(
        f.contracts.hook_timeout_kills_overrun_hook,
        "relay-hook timeout measured: an overrun hook is killed (start logged, end never)"
    );
    assert!(
        f.contracts.hook_timeout_overrun_session_proceeds,
        "relay-hook timeout measured: the session proceeds past a killed hook"
    );
}

// ── Argv contract: claude-print must emit exactly the verified spellings ────

/// The `--settings` argv spelling passed to the child, with the relay file
/// path substituted for `<file>`.
fn expected_settings_arg(settings_path: &Path) -> String {
    format!("--settings={}", settings_path.to_string_lossy())
}

/// The `--setting-sources` spelling verified (P3) to suppress every standard
/// source while leaving the `--settings` file loaded.
const VERIFIED_SUPPRESS_SPELLING: &str = "--setting-sources=";

fn argv_strings(argv: &[std::ffi::CString]) -> Vec<String> {
    argv.iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect()
}

#[test]
fn child_argv_matches_verified_spelling_default_mode() {
    let installer = HookInstaller::new().unwrap();
    let argv = Session::build_child_argv(
        Path::new("/usr/bin/claude"),
        &installer,
        &LaunchOptions::default(),
        &[],
    )
    .unwrap();
    let args = argv_strings(&argv);
    let f = fixture();

    // PO-1 verified merge semantics hold only when the relay file is passed
    // via --settings — the spelling measured in P2/P6.
    assert_eq!(
        args.iter().filter(|a| a.starts_with("--settings=")).count(),
        1,
        "exactly one --settings flag expected: {args:?}"
    );
    assert!(
        args.iter()
            .any(|a| *a == expected_settings_arg(&installer.settings_path)),
        "relay settings must be passed via the verified --settings=<file> spelling: {args:?}"
    );
    // Default mode omits --setting-sources entirely (Hard Requirement 5: user
    // hooks keep firing alongside the relay — measured as merge in P2/P6).
    assert!(
        !args.iter().any(|a| a.starts_with("--setting-sources")),
        "default mode must not restrict setting sources: {args:?}"
    );
    // Fixture cross-check: these assertions are only meaningful while the
    // fixture records merge semantics.
    assert!(f.contracts.settings_flag_merges_not_replaces);
}

#[test]
fn child_argv_matches_verified_spelling_no_inherit_hooks() {
    let installer = HookInstaller::new().unwrap();
    let argv = Session::build_child_argv(
        Path::new("/usr/bin/claude"),
        &installer,
        &LaunchOptions {
            no_inherit_hooks: true,
            ..Default::default()
        },
        &[],
    )
    .unwrap();
    let args = argv_strings(&argv);
    let f = fixture();

    assert!(
        args.iter().any(|a| a == VERIFIED_SUPPRESS_SPELLING),
        "isolation mode must forward the verified empty spelling {VERIFIED_SUPPRESS_SPELLING:?}: {args:?}"
    );
    // The `=none` fallback is REJECTED by claude 2.1.282 (P5: exit 1 before
    // session start) — it must never be emitted.
    assert!(
        !args.iter().any(|a| a.starts_with("--setting-sources=none")),
        "=none is rejected by the measured claude version and must not be emitted: {args:?}"
    );
    assert!(
        f.contracts
            .setting_sources_empty_suppresses_standard_sources
    );
    assert!(!f.contracts.setting_sources_none_accepted);
}

// ── Relay settings schema: the structure verified live (P2/P6 firings) ──────

#[test]
fn relay_settings_schema_matches_live_verified_structure() {
    let installer = HookInstaller::new().unwrap();
    let content = std::fs::read_to_string(&installer.settings_path).unwrap();
    let val: serde_json::Value = serde_json::from_str(&content).unwrap();

    // Double-nested hooks.Stop[ { hooks: [ {type: "command", ...} ] } ] —
    // accepted and fired by claude 2.1.282 (Hook Installer §2 schema note).
    let stop = val
        .pointer("/hooks/Stop")
        .and_then(|v| v.as_array())
        .expect("hooks.Stop must be an array");
    assert_eq!(stop.len(), 1, "one Stop matcher group");
    let inner = stop[0]
        .pointer("/hooks")
        .and_then(|v| v.as_array())
        .expect("matcher group must carry a hooks array");
    assert_eq!(inner.len(), 1, "one relay hook");
    assert_eq!(inner[0]["type"], "command");
    assert_eq!(inner[0]["timeout"], 10);
    let cmd = inner[0]["command"]
        .as_str()
        .expect("command must be a string");
    assert!(
        cmd.ends_with("hook.sh"),
        "relay command must point at hook.sh: {cmd}"
    );
}

// ── Live re-verification (#[ignore]; runs the real claude) ──────────────────

/// Environment contract mirrored from `src/pty.rs`: probes launched from
/// inside an agent session inherit `CLAUDECODE*` markers, which make claude
/// start as a nested session. Scrub and force exactly what claude-print does.
fn prepared_command(claude_bin: &Path, sandbox_home: &Path, workdir: &Path) -> Command {
    let mut cmd = Command::new(claude_bin);
    cmd.current_dir(workdir)
        .env_remove("CLAUDECODE")
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .env_remove("CLAUDE_CODE_CHILD_SESSION")
        .env_remove("CLAUDE_CODE_SKIP_PROMPT_HISTORY")
        .env("CLAUDE_CODE_ENTRYPOINT", "cli")
        .env("CLAUDE_CODE_FORCE_SESSION_PERSISTENCE", "1")
        .env("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC", "1")
        .env("HOME", sandbox_home)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    cmd
}

/// Sandbox with trust pre-seeded for `proj`, one shared firing log, and a
/// hook script per tag that appends its tag to the log on every firing.
struct ProbeSandbox {
    _dir: tempfile::TempDir,
    home: PathBuf,
    proj: PathBuf,
    log: PathBuf,
}

impl ProbeSandbox {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("sandbox tempdir");
        let home = dir.path().join("home");
        let proj = dir.path().join("proj");
        std::fs::create_dir_all(home.join(".claude")).unwrap();
        std::fs::create_dir_all(proj.join(".claude")).unwrap();

        // Pre-seed trust + onboarding so the child never blocks on a dialog.
        let claude_json = json!({
            "hasCompletedOnboarding": true,
            "theme": "dark",
            "projects": {
                proj.to_string_lossy(): {
                    "hasTrustDialogAccepted": true,
                    "hasCompletedProjectOnboarding": true,
                }
            }
        });
        std::fs::write(
            home.join(".claude.json"),
            serde_json::to_string(&claude_json).unwrap(),
        )
        .unwrap();

        let log = dir.path().join("firings.log");
        std::fs::write(&log, b"").unwrap();

        ProbeSandbox {
            _dir: dir,
            home,
            proj,
            log,
        }
    }

    /// Wire a SessionStart hook tagged `tag` into the settings source at
    /// `settings_path`, with its script inside `dir_for_scripts`.
    fn install_session_start_hook(&self, settings_path: &Path, tag: &str, dir_for_scripts: &Path) {
        let hook = dir_for_scripts.join(format!("hook-{tag}.sh"));
        let log_path = self.log.clone();
        let script = format!(
            "#!/bin/sh\ncat >/dev/null 2>/dev/null\nprintf '%s\\n' '{tag}' >> '{}'\n",
            log_path.to_string_lossy().replace('\'', "'\\''")
        );
        std::fs::write(&hook, script).unwrap();
        make_executable(&hook);

        let settings = json!({
            "hooks": {
                "SessionStart": [{ "hooks": [{ "type": "command", "command": hook }] }]
            }
        });
        std::fs::write(settings_path, serde_json::to_string(&settings).unwrap()).unwrap();
    }

    fn firings(&self) -> Vec<String> {
        std::fs::read_to_string(&self.log)
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }
}

fn make_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(path).unwrap().permissions();
    perms.set_mode(0o750);
    std::fs::set_permissions(path, perms).unwrap();
}

/// True when the live tests can run here at all: claude on PATH and some API
/// auth visible in the environment (the probe scripts' precondition too).
fn live_preconditions() -> Option<PathBuf> {
    let claude = which::which("claude").ok()?;
    let authed = std::env::var_os("ANTHROPIC_AUTH_TOKEN").is_some()
        || std::env::var_os("ANTHROPIC_API_KEY").is_some()
        || std::env::var_os("CLAUDE_CODE_OAUTH_TOKEN").is_some();
    if !authed {
        return None;
    }
    Some(claude)
}

/// Run one `claude -p` probe turn; returns raw exit status.
fn run_probe_turn(claude_bin: &Path, sandbox: &ProbeSandbox, extra_args: &[&str]) -> Option<i32> {
    const TIMEOUT: Duration = Duration::from_secs(120);
    let mut cmd = prepared_command(claude_bin, &sandbox.home, &sandbox.proj);
    cmd.arg("-p").args(extra_args).arg("Reply with exactly: OK");
    let mut child = cmd.spawn().expect("spawn claude");
    let deadline = Instant::now() + TIMEOUT;
    loop {
        match child.try_wait().expect("try_wait") {
            Some(status) => return status.code(),
            None if Instant::now() > deadline => {
                let _ = child.kill();
                let _ = child.wait();
                eprintln!("live probe turn exceeded {TIMEOUT:?}; treating as skip");
                return None;
            }
            None => std::thread::sleep(Duration::from_millis(250)),
        }
    }
}

/// PO-1/OQ-1 live: `--settings` merges — both sources' hooks fire on the same
/// event. Re-measures fixture contracts `settings_flag_merges_not_replaces`
/// and `all_loaded_sources_hooks_fire`. Cross-source *order* is deliberately
/// not asserted: the probe run measured it as non-deterministic (typically
/// standard-source-first, but concurrent and start-order-flipped firing was
/// observed), so no ordering can be pinned here.
#[test]
#[ignore = "live probe: runs the real claude binary (sandboxed HOME); cargo test -- --ignored"]
fn live_settings_flag_merges_across_sources() {
    let Some(claude_bin) = live_preconditions() else {
        eprintln!("skip: claude or API auth unavailable");
        return;
    };
    let sandbox = ProbeSandbox::new();
    sandbox.install_session_start_hook(
        &sandbox.proj.join(".claude/settings.json"),
        "project",
        &sandbox.proj,
    );
    let relay_settings = sandbox._dir.path().join("relay-settings.json");
    sandbox.install_session_start_hook(&relay_settings, "relay", sandbox._dir.path());

    let exit = run_probe_turn(
        &claude_bin,
        &sandbox,
        &[
            "--setting-sources=project",
            &format!("--settings={}", relay_settings.to_string_lossy()),
        ],
    );
    let exit = exit.expect("claude run must finish inside the probe timeout");

    assert_eq!(exit, 0, "probe turn must exit 0");
    let firings = sandbox.firings();
    assert!(
        firings.iter().any(|t| t == "project"),
        "project-source hook must fire (merge): {firings:?}"
    );
    assert!(
        firings.iter().any(|t| t == "relay"),
        "--settings relay hook must fire alongside (merge, not replace): {firings:?}"
    );
}

/// OQ-2 live: `--setting-sources=` (empty) suppresses the standard source
/// while the `--settings` file still loads. Re-measures fixture contracts
/// `setting_sources_empty_suppresses_standard_sources` and
/// `settings_file_loads_despite_setting_sources_empty`.
#[test]
#[ignore = "live probe: runs the real claude binary (sandboxed HOME); cargo test -- --ignored"]
fn live_empty_setting_sources_suppresses_but_settings_file_loads() {
    let Some(claude_bin) = live_preconditions() else {
        eprintln!("skip: claude or API auth unavailable");
        return;
    };
    let sandbox = ProbeSandbox::new();
    sandbox.install_session_start_hook(
        &sandbox.proj.join(".claude/settings.json"),
        "project",
        &sandbox.proj,
    );
    let relay_settings = sandbox._dir.path().join("relay-settings.json");
    sandbox.install_session_start_hook(&relay_settings, "relay", sandbox._dir.path());

    let exit = run_probe_turn(
        &claude_bin,
        &sandbox,
        &[
            "--setting-sources=",
            &format!("--settings={}", relay_settings.to_string_lossy()),
        ],
    );
    let exit = exit.expect("claude run must finish inside the probe timeout");

    assert_eq!(exit, 0, "probe turn must exit 0");
    let firings = sandbox.firings();
    assert!(
        !firings.iter().any(|t| t == "project"),
        "empty --setting-sources= must suppress the standard source: {firings:?}"
    );
    assert!(
        firings.iter().any(|t| t == "relay"),
        "the --settings file must still load under empty --setting-sources=: {firings:?}"
    );
}

// `json!` is only used by the sandbox helpers above.
use serde_json::json;
