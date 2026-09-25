//! Scheduled contract-drift watch (bead claudepr-e6e54313).
//!
//! `docs/notes/claude-contract-probes.md` §Maintenance makes drift a
//! push-triggered CI gate, but Claude Code auto-updates independently of
//! repo activity — between pushes the stamp and fixture pins can be stale
//! with nothing red. `scripts/contract-drift-watch.sh` is the scheduled
//! detector that closes that window (daily systemd user timer, installed by
//! `scripts/install-contract-drift-watch.sh`), and these tests pin its
//! contract so the schedule cannot silently detach from the doc:
//!
//! - exit-code mirror — the watcher's exit is the detector's (0 current /
//!   1 drift / 2 indeterminate), against a stubbed `claude` on the
//!   single-entry-stub-PATH pattern of `tests/contract_maintenance.rs`;
//! - state line — PASS/DRIFT/INDETERMINATE written atomically to
//!   `last-result` in the billing-canary shape (`STATUS timestamp=…
//!   key=value …`, redirected via `CLAUDE_PRINT_DRIFT_STATE_DIR`);
//! - drift filing — exactly one `bead create` carrying
//!   `--unique-ref claude-contract-drift:live-<version>` (the CLI's atomic
//!   idempotent create), `--label contract-drift`, the pin/live pair in the
//!   title, the `claude-contract-drift live=<version>` marker in the body,
//!   and cwd = the contract repo (the bead workspace);
//! - idempotent hits — `EXISTING`/`EXISTING_CLOSED` create results are
//!   recorded as `existing`/`existing-closed` without failing the run;
//! - degraded channels — a missing or failing `bead` is recorded as
//!   `not-filed`/`failed` in the state line while the drift exit stands;
//!   an indeterminate verdict never files anything;
//! - repo resolution — `CLAUDE_PRINT_CONTRACT_REPO` drives the watcher from
//!   outside the tree (the libexec mode the service unit runs), and omitting
//!   it out-of-tree fails closed as INDETERMINATE;
//! - wiring fragments — the units (`OnCalendar=daily`, `Persistent=true`,
//!   ExecStart/Environment the installer places) and §Maintenance's
//!   Scheduled-watch paragraph stay attached to the scripts.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn repo_path(relative: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(relative)
}

/// The version the contract evidence is currently pinned to, parsed from the
/// maintenance doc the same way `scripts/check-claude-version-bump.sh` does.
fn doc_pin() -> String {
    let doc = fs::read_to_string(repo_path("docs/notes/claude-contract-probes.md"))
        .expect("docs/notes/claude-contract-probes.md must exist");
    let line = doc
        .lines()
        .find(|l| l.starts_with("**Measured against:**"))
        .expect("the doc must carry a **Measured against:** stamp");
    line.split(|c: char| !(c.is_ascii_digit() || c == '.'))
        .filter(|t| !t.is_empty())
        .find(|t| {
            t.split('.').count() == 3
                && t.split('.')
                    .all(|p| !p.is_empty() && p.chars().all(|d| d.is_ascii_digit()))
        })
        .expect("the stamp must contain an x.y.z version")
        .to_string()
}

/// A version that can never equal the pin: patch component +1.
fn bumped(pin: &str) -> String {
    let mut parts: Vec<u32> = pin.split('.').map(|p| p.parse().unwrap()).collect();
    *parts.last_mut().unwrap() += 1;
    parts
        .iter()
        .map(|p| p.to_string())
        .collect::<Vec<_>>()
        .join(".")
}

/// Symlink the real core utilities the watcher and the detector it drives
/// need into the stub bin dir (the contract_maintenance.rs set plus the
/// watcher's own sed/tail/date/mktemp/chmod/mv). Idempotent.
fn link_coreutils(bin: &Path) {
    fs::create_dir_all(bin).unwrap();
    for tool in [
        "bash", "grep", "head", "cat", "mkdir", "dirname", "sort", "wc", "tr", "sed", "tail",
        "date", "mktemp", "chmod", "mv",
    ] {
        let dest = bin.join(tool);
        if dest.symlink_metadata().is_ok() {
            continue;
        }
        let real = std::env::var_os("PATH")
            .and_then(|path| {
                std::env::split_paths(&path)
                    .map(|dir| dir.join(tool))
                    .find(|p| p.exists())
            })
            .unwrap_or_else(|| panic!("{tool} must be on PATH to build the stub bin dir"));
        std::os::unix::fs::symlink(&real, &dest).unwrap();
    }
}

fn stub_path(bin: &Path) -> String {
    link_coreutils(bin);
    bin.display().to_string()
}

fn write_stub(dir: &Path, name: &str, body: &str) {
    fs::create_dir_all(dir).unwrap();
    let stub = dir.join(name);
    fs::write(&stub, format!("#!/usr/bin/env bash\n{body}\n")).unwrap();
    fs::set_permissions(&stub, fs::Permissions::from_mode(0o755)).unwrap();
}

/// A `claude` whose `--version` first line is exactly `line`.
fn stub_claude(dir: &Path, line: &str) {
    write_stub(dir, "claude", &format!("printf '%s\\n' {:?}", line));
}

/// A `bead` that records each invocation (`$*`, plus the cwd it ran under)
/// to `$BEAD_ARGS_FILE` and answers `create` with `$BEAD_CREATE_RESULT`
/// (default: a fresh id). `BEAD_CREATE_FAIL=1` fails creation.
fn stub_bead(dir: &Path) {
    write_stub(
        dir,
        "bead",
        r#"set -u
printf '%s\n' "$*" >> "${BEAD_ARGS_FILE:?BEAD_ARGS_FILE not set}"
printf 'cwd=%s\n' "$PWD" >> "$BEAD_ARGS_FILE"
if [ "${BEAD_CREATE_FAIL:-0}" = 1 ]; then
    echo 'synthetic bead create failure' >&2
    exit 1
fi
printf '%s\n' "${BEAD_CREATE_RESULT:-claudepr-stub0f1e}"
"#,
    );
}

/// Run the watcher with the hermetic stub PATH, a redirected state dir, and
/// extra env pairs (stub controls, repo override).
fn run_watch(bin: Option<&Path>, envs: &[(&str, String)]) -> Output {
    let mut cmd = Command::new("bash");
    cmd.arg(repo_path("scripts/contract-drift-watch.sh"));
    if let Some(bin) = bin {
        cmd.env("PATH", stub_path(bin));
    }
    for (key, value) in envs {
        cmd.env(key, value);
    }
    cmd.output().unwrap()
}

fn stderr_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).to_string()
}

fn stdout_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).to_string()
}

fn read_text(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_default()
}

// ── CURRENT: green run, no follow-up channel touched ─────────────────────────

#[test]
fn watch_current_exits_zero_writes_pass_and_never_touches_bead() {
    let pin = doc_pin();
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("bin");
    stub_claude(&bin, &format!("{pin} (Claude Code)"));
    stub_bead(&bin);
    let bead_args = dir.path().join("bead-args.txt");
    let state = dir.path().join("state");

    let out = run_watch(
        Some(&bin),
        &[
            ("CLAUDE_PRINT_DRIFT_STATE_DIR", state.display().to_string()),
            ("BEAD_ARGS_FILE", bead_args.display().to_string()),
        ],
    );

    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr_of(&out));
    assert!(stdout_of(&out).contains("CURRENT"), "{}", stdout_of(&out));
    let result = read_text(&state.join("last-result"));
    assert!(
        result.starts_with("PASS timestamp=") && result.contains("verdict=current"),
        "state line must record the green verdict: {result}"
    );
    let mode = fs::metadata(state.join("last-result"))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(
        mode & 0o777,
        0o600,
        "the state line stays mode 600: {result}"
    );
    assert!(
        !bead_args.exists(),
        "a CURRENT watch must not invoke bead for any reason"
    );
}

// ── DRIFT: exit 1 + exactly one idempotent bead filed in the repo workspace ──

#[test]
fn watch_drift_files_per_version_bead_from_the_repo_workspace() {
    let pin = doc_pin();
    let live = bumped(&pin);
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("bin");
    stub_claude(&bin, &format!("{live} (Claude Code)"));
    stub_bead(&bin);
    let bead_args = dir.path().join("bead-args.txt");
    let state = dir.path().join("state");

    let out = run_watch(
        Some(&bin),
        &[
            ("CLAUDE_PRINT_DRIFT_STATE_DIR", state.display().to_string()),
            ("BEAD_ARGS_FILE", bead_args.display().to_string()),
        ],
    );

    assert_eq!(out.status.code(), Some(1), "stderr: {}", stderr_of(&out));
    let result = read_text(&state.join("last-result"));
    assert!(result.starts_with("DRIFT timestamp="), "{result}");
    for fragment in [
        "verdict=re-run-due",
        &format!("pinned={pin}"),
        &format!("live={live}"),
        "follow-up=filed claudepr-stub0f1e",
    ] {
        assert!(result.contains(fragment), "state line: {result}");
    }
    assert!(
        stderr_of(&out).contains("ALERT"),
        "the drift alert must surface on stderr: {}",
        stderr_of(&out)
    );

    // The filing: one create, keyed per installed version, marker in the
    // body, filed from the contract repo's workspace (bead operates on cwd).
    let bead = read_text(&bead_args);
    assert!(
        bead.contains("create"),
        "drift must file via bead create: {bead}"
    );
    assert!(
        bead.contains(&format!("--unique-ref claude-contract-drift:live-{live}")),
        "the create must be idempotent per installed version: {bead}"
    );
    assert!(
        bead.contains(&format!(
            "--title Claude contract drift: live {live}, evidence pinned to {pin}"
        )),
        "the title must carry the pin/live pair: {bead}"
    );
    assert!(
        bead.contains(&format!("claude-contract-drift live={live}")),
        "the body must carry the grep marker shared with the gate's gh issue: {bead}"
    );
    assert!(
        bead.contains("--label contract-drift") && bead.contains("--priority 2"),
        "the bead must be labeled and prioritized for the queue: {bead}"
    );
    assert!(
        bead.contains(&format!("cwd={}", env!("CARGO_MANIFEST_DIR"))),
        "bead must run from the contract repo so the follow-up lands in its workspace: {bead}"
    );
}

#[test]
fn watch_drift_idempotent_hits_are_recorded_not_duplicated() {
    let live = bumped(&doc_pin());
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("bin");
    stub_claude(&bin, &format!("{live} (Claude Code)"));
    stub_bead(&bin);

    // The two idempotent-create results bead-rs documents for --unique-ref:
    // a repeat while the drift persists, and a binding that already points
    // at a closed bead. Both stay exit-1 drifts; only the record differs.
    for (result_line, recorded) in [
        (
            "EXISTING claudepr-stub1111",
            "follow-up=existing claudepr-stub1111",
        ),
        (
            "EXISTING_CLOSED claudepr-stub2222",
            "follow-up=existing-closed claudepr-stub2222",
        ),
    ] {
        let state = dir
            .path()
            .join(format!("state-{}", recorded.split(' ').next().unwrap()));
        let out = run_watch(
            Some(&bin),
            &[
                ("CLAUDE_PRINT_DRIFT_STATE_DIR", state.display().to_string()),
                (
                    "BEAD_ARGS_FILE",
                    dir.path().join("bead-args.txt").display().to_string(),
                ),
                ("BEAD_CREATE_RESULT", result_line.to_string()),
            ],
        );
        assert_eq!(out.status.code(), Some(1), "stderr: {}", stderr_of(&out));
        let line = read_text(&state.join("last-result"));
        assert!(
            line.contains(recorded),
            "state line must record `{recorded}`: {line}"
        );
    }
}

// ── Degraded alert channels: the drift verdict stands, the gap is recorded ────

#[test]
fn watch_drift_without_bead_cli_records_not_filed() {
    let live = bumped(&doc_pin());
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("bin");
    stub_claude(&bin, &format!("{live} (Claude Code)"));
    // No bead stub: `command -v bead` genuinely fails on this PATH.
    let state = dir.path().join("state");

    let out = run_watch(
        Some(&bin),
        &[("CLAUDE_PRINT_DRIFT_STATE_DIR", state.display().to_string())],
    );

    assert_eq!(out.status.code(), Some(1), "stderr: {}", stderr_of(&out));
    let line = read_text(&state.join("last-result"));
    assert!(
        line.contains("follow-up=not-filed (bead CLI not on PATH"),
        "the missing channel must be recorded, not silent: {line}"
    );
    assert!(
        stderr_of(&out).contains("bead CLI not on PATH"),
        "a warning must point at the manual fallback: {}",
        stderr_of(&out)
    );
}

#[test]
fn watch_drift_when_bead_create_fails_records_failure() {
    let live = bumped(&doc_pin());
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("bin");
    stub_claude(&bin, &format!("{live} (Claude Code)"));
    stub_bead(&bin);
    let state = dir.path().join("state");

    let out = run_watch(
        Some(&bin),
        &[
            ("CLAUDE_PRINT_DRIFT_STATE_DIR", state.display().to_string()),
            (
                "BEAD_ARGS_FILE",
                dir.path().join("bead-args.txt").display().to_string(),
            ),
            ("BEAD_CREATE_FAIL", "1".to_string()),
        ],
    );

    // The drift alert stands; the failed filing is recorded, never silent
    // (the gate's failed-gh shape).
    assert_eq!(out.status.code(), Some(1), "stderr: {}", stderr_of(&out));
    let line = read_text(&state.join("last-result"));
    assert!(
        line.contains("follow-up=failed (bead create exit 1"),
        "state line: {line}"
    );
    assert!(
        stderr_of(&out).contains("synthetic bead create failure"),
        "the underlying error must surface: {}",
        stderr_of(&out)
    );
}

// ── INDETERMINATE: fail closed, file nothing ──────────────────────────────────

#[test]
fn watch_indeterminate_exits_two_without_filing() {
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("bin");
    fs::create_dir_all(&bin).unwrap(); // no claude stub, minimal PATH
    stub_bead(&bin);
    let bead_args = dir.path().join("bead-args.txt");
    let state = dir.path().join("state");

    let out = run_watch(
        Some(&bin),
        &[
            ("CLAUDE_PRINT_DRIFT_STATE_DIR", state.display().to_string()),
            ("BEAD_ARGS_FILE", bead_args.display().to_string()),
        ],
    );

    assert_eq!(out.status.code(), Some(2), "stderr: {}", stderr_of(&out));
    let line = read_text(&state.join("last-result"));
    assert!(
        line.starts_with("INDETERMINATE timestamp=") && line.contains("verdict=cannot-determine"),
        "state line: {line}"
    );
    assert!(
        stderr_of(&out).contains("ALERT"),
        "indeterminate must alert loudly: {}",
        stderr_of(&out)
    );
    assert!(
        !bead_args.exists(),
        "no version to key a bead on — nothing may be filed"
    );
}

// ── Repo resolution: the libexec mode the service unit drives ─────────────────

#[test]
fn watch_resolves_the_contract_repo_from_env_when_run_out_of_tree() {
    let pin = doc_pin();
    let dir = tempfile::tempdir().unwrap();
    // The installed layout: the watcher in a libexec dir, away from the repo.
    let libexec = dir.path().join("libexec");
    fs::create_dir_all(&libexec).unwrap();
    fs::copy(
        repo_path("scripts/contract-drift-watch.sh"),
        libexec.join("contract-drift-watch.sh"),
    )
    .unwrap();
    fs::set_permissions(
        libexec.join("contract-drift-watch.sh"),
        fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    let bin = dir.path().join("bin");
    stub_claude(&bin, &format!("{pin} (Claude Code)"));
    stub_bead(&bin);
    let state = dir.path().join("state");

    // Positive: CLAUDE_PRINT_CONTRACT_REPO (exactly what the .service sets)
    // points the watcher at the checkout — detector and bead workspace both.
    let out = Command::new("bash")
        .arg(libexec.join("contract-drift-watch.sh"))
        .env("PATH", stub_path(&bin))
        .env("CLAUDE_PRINT_CONTRACT_REPO", env!("CARGO_MANIFEST_DIR"))
        .env("CLAUDE_PRINT_DRIFT_STATE_DIR", &state)
        .env("BEAD_ARGS_FILE", dir.path().join("bead-args.txt"))
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(read_text(&state.join("last-result")).starts_with("PASS timestamp="));

    // Negative: without the env, an out-of-tree watcher has no detector and
    // must fail closed as INDETERMINATE — never silently measure nothing.
    let out = Command::new("bash")
        .arg(libexec.join("contract-drift-watch.sh"))
        .env("PATH", stub_path(&bin))
        .env("CLAUDE_PRINT_DRIFT_STATE_DIR", dir.path().join("state2"))
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    let line = read_text(&dir.path().join("state2/last-result"));
    assert!(
        line.contains("reason=detector_missing"),
        "state line: {line}"
    );
}

// ── Wiring: units, installer, and §Maintenance stay attached ──────────────────

#[test]
fn units_installer_and_maintenance_doc_stay_wired() {
    // The service runs the installed watcher and pins the checkout it
    // measures; both bead (~/.cargo/bin) and claude (~/.local/bin) are
    // reachable on its PATH.
    let service = fs::read_to_string(repo_path(
        "scripts/claude-print-contract-drift-watch.service",
    ))
    .unwrap();
    for fragment in [
        "ExecStart=%h/.local/libexec/claude-print/contract-drift-watch.sh",
        "Environment=CLAUDE_PRINT_CONTRACT_REPO=%h/claude-print",
        "%h/.cargo/bin",
        "%h/.local/bin",
    ] {
        assert!(
            service.contains(fragment),
            "service unit lost wiring fragment: {fragment}\n{service}"
        );
    }

    // The timer: daily, persistent (a missed window fires on the next boot),
    // activating exactly that service.
    let timer =
        fs::read_to_string(repo_path("scripts/claude-print-contract-drift-watch.timer")).unwrap();
    for fragment in [
        "OnCalendar=daily",
        "Persistent=true",
        "Unit=claude-print-contract-drift-watch.service",
        "WantedBy=timers.target",
    ] {
        assert!(
            timer.contains(fragment),
            "timer lost wiring fragment: {fragment}\n{timer}"
        );
    }

    // §Maintenance documents the cadence beside the re-pin checklist.
    let doc = fs::read_to_string(repo_path("docs/notes/claude-contract-probes.md")).unwrap();
    let maintenance = doc
        .split("## Maintenance")
        .nth(1)
        .expect("§Maintenance section must exist");
    for fragment in [
        "scripts/contract-drift-watch.sh",
        "scripts/install-contract-drift-watch.sh",
        "claude-print-contract-drift-watch.timer",
        "OnCalendar=daily",
        "--unique-ref claude-contract-drift:live-<version>",
        "contract-drift-watch/last-result",
    ] {
        assert!(
            maintenance.contains(fragment),
            "§Maintenance must keep documenting `{fragment}` (the scheduled-watch cadence)"
        );
    }

    // Both scripts exist and are executable, like the other probe scripts.
    for script in [
        "scripts/contract-drift-watch.sh",
        "scripts/install-contract-drift-watch.sh",
    ] {
        let path = repo_path(script);
        assert!(path.is_file(), "{script} must exist");
        let mode = fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o111, 0o111, "{script} must be executable");
    }
}
