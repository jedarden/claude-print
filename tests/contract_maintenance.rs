//! Contract-probe maintenance wiring guard (bead claudepr-e8fc5744).
//!
//! `docs/notes/claude-contract-probes.md` §Maintenance defines the maintenance
//! step (detect → re-run → re-pin → file follow-ups) and
//! `scripts/contract-maintenance-gate.sh` is its executable owner, wired into
//! the `claude-print-ci` WorkflowTemplate so it runs on every push. These
//! tests pin that wiring so the automation cannot silently detach from the
//! doc again:
//!
//! - detector parse — `scripts/check-claude-version-bump.sh` exits 0/1/2
//!   against a stubbed `claude`, with the stub's version derived from the
//!   doc's live **Measured against:** stamp (re-pin-proof: re-stamping the
//!   doc moves both sides together, so nothing here goes stale);
//! - the gate's contract against a stubbed `claude`/`gh`/`cargo` (hermetic —
//!   a single stub-bin PATH, so unstubbed binaries are genuinely absent):
//!   exit code, evidence-bundle shape, the refreshed
//!   `target/last-claude-version.txt`, and `--file-follow-up` idempotency
//!   (marker search before issue create);
//! - real-environment self-consistency — the same path CI exercises, with no
//!   PATH override: the gate's verdict mirrors the detector's, and the
//!   version file matches the real `claude --version` (or records `unknown`);
//! - wiring fragments — the WorkflowTemplate invokes the gate on every push
//!   and stamps the status into release notes, and the maintenance doc /
//!   plan R-2 / README / AGENTS.md still name it.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn repo_path(relative: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(relative)
}

/// The version the contract evidence is currently pinned to, parsed from the
/// maintenance doc the same way `scripts/check-claude-version-bump.sh` does
/// (first x.y.z token of the **Measured against:** line).
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

/// Symlink the real core utilities the bash scripts need (bash, grep, head,
/// cat, mkdir, dirname) into the stub bin dir, so a single-entry PATH is
/// self-contained. `printf`/`command`/`cd` are bash builtins and need no
/// link. Idempotent: existing links (and stubs) are left alone.
fn link_coreutils(bin: &Path) {
    fs::create_dir_all(bin).unwrap();
    for tool in ["bash", "grep", "head", "cat", "mkdir", "dirname"] {
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

/// The hermetic PATH for gate/detector runs: exactly one stub bin dir (the
/// tests/install_billing_canary.rs pattern). Every binary the scripts reach
/// is either a stub written by the test or a coreutils symlink — so a binary
/// that was not placed there (`claude` in the indeterminate tests, `gh`, the
/// real `cargo`) is genuinely unreachable, on any host layout. That matters
/// here: this repo's dev host is NixOS, whose system profile dir holds both
/// the coreutils and a claude install, so filtering the real PATH could never
/// separate them.
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

/// A `gh` that records each invocation (`$*`, one line) to `$GH_ARGS_FILE`
/// and answers the three subcommands the gate uses. `GH_LIST_FOUND=1` makes
/// `issue list` report existing issue #42; `GH_CREATE_FAIL=1` fails creation.
fn stub_gh(dir: &Path) {
    write_stub(
        dir,
        "gh",
        r#"set -u
printf '%s\n' "$*" >> "${GH_ARGS_FILE:?GH_ARGS_FILE not set}"
case "$1 $2" in
    'issue list')
        if [ "${GH_LIST_FOUND:-0}" = 1 ]; then
            printf '[{"number":42}]\n'
        else
            printf '[]\n'
        fi
        ;;
    'issue create')
        if [ "${GH_CREATE_FAIL:-0}" = 1 ]; then
            echo 'synthetic gh create failure' >&2
            exit 1
        fi
        echo 'https://github.com/jedarden/claude-print/issues/43'
        ;;
    'issue comment') exit 0 ;;
    *) echo "stub gh: unexpected subcommand: $*" >&2; exit 1 ;;
esac
"#,
    );
}

/// A `cargo` that records its arguments and passes, standing in for the
/// cheap live-contract run (`cargo test --test claude_contracts -- --ignored`).
fn stub_cargo(dir: &Path) {
    write_stub(
        dir,
        "cargo",
        r#"set -u
printf '%s\n' "$*" >> "${CARGO_ARGS_FILE:?CARGO_ARGS_FILE not set}"
echo 'stub cargo: test claude_contracts ... ok'
exit 0
"#,
    );
}

/// Run one of the repo's bash scripts. `bin` = Some(stub dir) switches to the
/// hermetic minimal PATH; None keeps the real environment (CI's path).
fn run_script(
    script: &str,
    bin: Option<&Path>,
    args: &[String],
    envs: &[(&str, String)],
) -> Output {
    let mut cmd = Command::new("bash");
    cmd.arg(repo_path(script));
    if let Some(bin) = bin {
        cmd.env("PATH", stub_path(bin));
    }
    for (key, value) in envs {
        cmd.env(key, value);
    }
    cmd.args(args).output().unwrap()
}

fn gate_args(dir: &Path, skip_live: bool, extra: &[&str]) -> Vec<String> {
    let mut args = vec![
        "--evidence-dir".to_string(),
        dir.join("evidence").display().to_string(),
        "--version-file".to_string(),
        dir.join("last-claude-version.txt").display().to_string(),
    ];
    if skip_live {
        args.push("--skip-live-tests".to_string());
    }
    args.extend(extra.iter().map(|s| s.to_string()));
    args
}

fn read_text(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_default()
}

/// One `key: value` line out of contract-status.txt.
fn status_value(evidence: &Path, key: &str) -> String {
    let status = fs::read_to_string(evidence.join("contract-status.txt"))
        .expect("contract-status.txt must exist");
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix(&format!("{key}: ")) {
            return rest.trim().to_string();
        }
    }
    panic!("no '{key}:' line in contract-status.txt:\n{status}");
}

fn stderr_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).to_string()
}

// ── Detector parse (scripts/check-claude-version-bump.sh) ────────────────────

#[test]
fn detector_current_when_installed_matches_pin() {
    let pin = doc_pin();
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("bin");
    stub_claude(&bin, &format!("{pin} (Claude Code)"));

    let out = run_script("scripts/check-claude-version-bump.sh", Some(&bin), &[], &[]);

    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr_of(&out));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("CURRENT"), "{stdout}");
}

#[test]
fn detector_drift_when_installed_differs_from_pin() {
    let live = bumped(&doc_pin());
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("bin");
    stub_claude(&bin, &format!("{live} (Claude Code)"));

    let out = run_script("scripts/check-claude-version-bump.sh", Some(&bin), &[], &[]);

    assert_eq!(out.status.code(), Some(1), "stderr: {}", stderr_of(&out));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("DRIFT"), "{stdout}");
    assert!(stdout.contains(&live), "{stdout}");
}

#[test]
fn detector_indeterminate_without_claude() {
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("bin");
    fs::create_dir_all(&bin).unwrap(); // no claude stub, minimal PATH

    let out = run_script("scripts/check-claude-version-bump.sh", Some(&bin), &[], &[]);

    assert_eq!(out.status.code(), Some(2), "stderr: {}", stderr_of(&out));
}

#[test]
fn detector_indeterminate_on_unparsable_version() {
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("bin");
    stub_claude(&bin, "definitely not a version");

    let out = run_script("scripts/check-claude-version-bump.sh", Some(&bin), &[], &[]);

    assert_eq!(out.status.code(), Some(2), "stderr: {}", stderr_of(&out));
}

// ── Gate contract, hermetic (stubbed claude / gh / cargo) ────────────────────

#[test]
fn gate_current_records_evidence_and_refreshes_version_file() {
    let pin = doc_pin();
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("bin");
    stub_claude(&bin, &format!("{pin} (Claude Code)"));

    let out = run_script(
        "scripts/contract-maintenance-gate.sh",
        Some(&bin),
        &gate_args(dir.path(), true, &[]),
        &[],
    );

    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr_of(&out));
    let evidence = dir.path().join("evidence");
    assert_eq!(status_value(&evidence, "alert"), "none");
    assert_eq!(status_value(&evidence, "pinned"), pin);
    assert_eq!(status_value(&evidence, "installed"), pin);
    // Version artifact: the full first line, the same shape
    // tests/version_compat.rs::test_claude_version_recorded writes.
    assert_eq!(
        read_text(&dir.path().join("last-claude-version.txt")),
        format!("{pin} (Claude Code)\n")
    );
    // Evidence bundle shape per §Wiring: detection + probes (SKIPPED) +
    // status + next-steps; live-contract-tests.txt only when tests ran.
    assert!(read_text(&evidence.join("detection.txt")).contains("detector-exit: 0"));
    for probe in [
        "probe-claude-contracts.sh",
        "probe-stop-toolallowed.sh",
        "probe-tui-second-turn.sh",
    ] {
        let note = read_text(&evidence.join("probes").join(format!("{probe}.txt")));
        assert!(note.contains("SKIPPED"), "{probe}: {note}");
    }
    assert!(!evidence.join("live-contract-tests.txt").exists());
    assert!(evidence.join("next-steps.txt").exists());
    assert_eq!(status_value(&evidence, "follow-up"), "n/a (no drift)");
}

#[test]
fn gate_drift_exits_one_and_files_marker_issue() {
    let pin = doc_pin();
    let live = bumped(&pin);
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("bin");
    stub_claude(&bin, &format!("{live} (Claude Code)"));
    stub_gh(&bin);
    let gh_args = dir.path().join("gh-args.txt");

    let out = run_script(
        "scripts/contract-maintenance-gate.sh",
        Some(&bin),
        &gate_args(dir.path(), true, &["--file-follow-up"]),
        &[("GH_ARGS_FILE", gh_args.display().to_string())],
    );

    assert_eq!(out.status.code(), Some(1), "stderr: {}", stderr_of(&out));
    let evidence = dir.path().join("evidence");
    assert_eq!(status_value(&evidence, "alert"), "re-run-due");
    assert_eq!(status_value(&evidence, "pinned"), pin);
    assert_eq!(status_value(&evidence, "installed"), live);
    // The version artifact is re-anchored to the drifted version.
    assert_eq!(
        read_text(&dir.path().join("last-claude-version.txt")),
        format!("{live} (Claude Code)\n")
    );
    // Follow-up: the marker makes the issue idempotent per installed version.
    let gh = read_text(&gh_args);
    assert!(
        gh.contains(&format!("claude-contract-drift live={live}")),
        "gh invocations must carry the per-version marker:\n{gh}"
    );
    assert!(gh.contains("issue create"), "{gh}");
    assert!(
        status_value(&evidence, "follow-up").contains("filed"),
        "{}",
        status_value(&evidence, "follow-up")
    );
}

#[test]
fn gate_drift_updates_existing_issue_for_same_version() {
    let live = bumped(&doc_pin());
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("bin");
    stub_claude(&bin, &format!("{live} (Claude Code)"));
    stub_gh(&bin);
    let gh_args = dir.path().join("gh-args.txt");

    let out = run_script(
        "scripts/contract-maintenance-gate.sh",
        Some(&bin),
        &gate_args(dir.path(), true, &["--file-follow-up"]),
        &[
            ("GH_ARGS_FILE", gh_args.display().to_string()),
            ("GH_LIST_FOUND", "1".to_string()),
        ],
    );

    assert_eq!(out.status.code(), Some(1), "stderr: {}", stderr_of(&out));
    let gh = read_text(&gh_args);
    assert!(gh.contains("issue comment 42"), "{gh}");
    assert!(!gh.contains("issue create"), "{gh}");
    assert_eq!(
        status_value(&dir.path().join("evidence"), "follow-up"),
        "updated issue #42"
    );
}

#[test]
fn gate_drift_without_follow_up_flag_never_calls_gh() {
    let live = bumped(&doc_pin());
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("bin");
    stub_claude(&bin, &format!("{live} (Claude Code)"));
    stub_gh(&bin);
    let gh_args = dir.path().join("gh-args.txt");

    let out = run_script(
        "scripts/contract-maintenance-gate.sh",
        Some(&bin),
        &gate_args(dir.path(), true, &[]),
        &[("GH_ARGS_FILE", gh_args.display().to_string())],
    );

    assert_eq!(out.status.code(), Some(1), "stderr: {}", stderr_of(&out));
    assert!(
        !gh_args.exists(),
        "gh must not be invoked without --file-follow-up"
    );
    assert_eq!(
        status_value(&dir.path().join("evidence"), "follow-up"),
        "not-requested (pass --file-follow-up)"
    );
}

#[test]
fn gate_current_never_files_a_follow_up() {
    let pin = doc_pin();
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("bin");
    stub_claude(&bin, &format!("{pin} (Claude Code)"));
    stub_gh(&bin);
    let gh_args = dir.path().join("gh-args.txt");

    let out = run_script(
        "scripts/contract-maintenance-gate.sh",
        Some(&bin),
        &gate_args(dir.path(), true, &["--file-follow-up"]),
        &[("GH_ARGS_FILE", gh_args.display().to_string())],
    );

    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr_of(&out));
    assert!(!gh_args.exists(), "no drift means no follow-up work");
}

#[test]
fn gate_indeterminate_without_claude_records_unknown() {
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("bin");
    fs::create_dir_all(&bin).unwrap(); // no claude stub, minimal PATH

    let out = run_script(
        "scripts/contract-maintenance-gate.sh",
        Some(&bin),
        &gate_args(dir.path(), true, &[]),
        &[],
    );

    assert_eq!(out.status.code(), Some(2), "stderr: {}", stderr_of(&out));
    let evidence = dir.path().join("evidence");
    assert_eq!(status_value(&evidence, "alert"), "indeterminate");
    assert_eq!(status_value(&evidence, "installed"), "unknown");
    assert_eq!(
        read_text(&dir.path().join("last-claude-version.txt")),
        "unknown\n"
    );
    assert!(read_text(&evidence.join("next-steps.txt")).contains("could not be determined"));
}

#[test]
fn gate_drift_with_failing_gh_records_failed_follow_up() {
    let live = bumped(&doc_pin());
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("bin");
    stub_claude(&bin, &format!("{live} (Claude Code)"));
    stub_gh(&bin);
    let gh_args = dir.path().join("gh-args.txt");

    let out = run_script(
        "scripts/contract-maintenance-gate.sh",
        Some(&bin),
        &gate_args(dir.path(), true, &["--file-follow-up"]),
        &[
            ("GH_ARGS_FILE", gh_args.display().to_string()),
            ("GH_CREATE_FAIL", "1".to_string()),
        ],
    );

    // The drift alert stands; the failed filing is recorded, never silent.
    assert_eq!(out.status.code(), Some(1), "stderr: {}", stderr_of(&out));
    assert!(status_value(&dir.path().join("evidence"), "follow-up").contains("failed"));
    assert!(!stderr_of(&out).trim().is_empty());
}

#[test]
fn gate_runs_cheap_live_contracts_when_not_skipped() {
    let pin = doc_pin();
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("bin");
    stub_claude(&bin, &format!("{pin} (Claude Code)"));
    stub_cargo(&bin);
    let cargo_args = dir.path().join("cargo-args.txt");

    let out = run_script(
        "scripts/contract-maintenance-gate.sh",
        Some(&bin),
        &gate_args(dir.path(), false, &[]),
        &[("CARGO_ARGS_FILE", cargo_args.display().to_string())],
    );

    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr_of(&out));
    let evidence = dir.path().join("evidence");
    let live = read_text(&evidence.join("live-contract-tests.txt"));
    assert!(live.contains("live-tests-exit: 0"), "{live}");
    assert!(live.contains("stub cargo"), "{live}");
    assert_eq!(status_value(&evidence, "live-tests"), "ran (exit 0)");
    let cargo = read_text(&cargo_args);
    assert!(
        cargo.contains("--test claude_contracts") && cargo.contains("--ignored"),
        "gate must run the cheap live contracts verbatim: {cargo}"
    );
}

#[test]
fn gate_live_tests_skip_flag_leaves_no_transcript() {
    let pin = doc_pin();
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("bin");
    stub_claude(&bin, &format!("{pin} (Claude Code)"));

    let out = run_script(
        "scripts/contract-maintenance-gate.sh",
        Some(&bin),
        &gate_args(dir.path(), true, &[]),
        &[],
    );

    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr_of(&out));
    let evidence = dir.path().join("evidence");
    assert!(!evidence.join("live-contract-tests.txt").exists());
    assert_eq!(
        status_value(&evidence, "live-tests"),
        "skipped (--skip-live-tests)"
    );
}

// ── Real-environment self-consistency (CI's exercise of the gate) ────────────

#[test]
fn gate_real_environment_self_consistent() {
    let dir = tempfile::tempdir().unwrap();
    // No PATH override and no --file-follow-up: exactly what CI drives, with
    // the real claude (or none) deciding the verdict.
    let gate = run_script(
        "scripts/contract-maintenance-gate.sh",
        None,
        &gate_args(dir.path(), true, &[]),
        &[],
    );
    let det = run_script("scripts/check-claude-version-bump.sh", None, &[], &[]);

    let gate_exit = gate.status.code().unwrap();
    assert_eq!(
        gate_exit,
        det.status.code().unwrap(),
        "gate exit must mirror the detector's in the real environment"
    );

    let evidence = dir.path().join("evidence");
    let alert = status_value(&evidence, "alert");
    match gate_exit {
        0 => assert_eq!(alert, "none"),
        1 => assert_eq!(alert, "re-run-due"),
        2 => assert_eq!(alert, "indeterminate"),
        other => panic!("unexpected gate exit {other}"),
    }

    // The refreshed artifact matches the real claude, or records unknown.
    let version_file = read_text(&dir.path().join("last-claude-version.txt"));
    match Command::new("claude").arg("--version").output() {
        Ok(output) if output.status.success() => {
            let combined = format!(
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            let first = combined.lines().next().unwrap_or("").trim();
            assert!(!first.is_empty());
            assert_eq!(version_file.trim(), first);
        }
        _ => assert_eq!(version_file.trim(), "unknown"),
    }
}

// ── Wiring fragments: the gate cannot silently detach from CI or the docs ────

#[test]
fn ci_workflowtemplate_wires_the_gate_on_every_push() {
    let template = fs::read_to_string(repo_path("claude-print-ci-workflowtemplate.yml")).unwrap();
    for fragment in [
        // the gate itself, evidence dir, follow-up flag — both CI modes
        "bash scripts/contract-maintenance-gate.sh",
        "--evidence-dir target/contract-maintenance",
        "--file-follow-up",
        // drift is an alert, not a red build: the exit is captured, not fatal
        "GATE_EXIT",
        // claude is installed first so detection compares a real version
        "https://claude.ai/install.sh",
        // release path stamps the status and refreshes the version asset
        "target/contract-maintenance/contract-status.txt",
        "cp target/last-claude-version.txt last-claude-version.txt",
    ] {
        assert!(
            template.contains(fragment),
            "WorkflowTemplate lost wiring fragment: {fragment}"
        );
    }
    // The gate runs before the verify-only early exit, so every push hits it
    // (anchored to the exit's own message — "Verify-only mode" alone first
    // appears in the clone-branch echo higher up the template).
    let gate_at = template
        .find("bash scripts/contract-maintenance-gate.sh")
        .unwrap();
    let verify_at = template
        .find("Verify-only mode: all quality gates passed")
        .expect("verify-only early exit must exist");
    assert!(
        gate_at < verify_at,
        "the gate must run before the verify-only exit"
    );
}

#[test]
fn maintenance_docs_still_name_the_gate() {
    let doc = fs::read_to_string(repo_path("docs/notes/claude-contract-probes.md")).unwrap();
    let maintenance = doc
        .split("## Maintenance")
        .nth(1)
        .expect("§Maintenance section must exist");
    assert!(
        maintenance.contains("scripts/contract-maintenance-gate.sh"),
        "§Maintenance must name the gate as the executable owner"
    );
    assert!(maintenance.contains("--file-follow-up"));

    let plan = fs::read_to_string(repo_path("docs/plan/plan.md")).unwrap();
    let r2 = plan
        .lines()
        .find(|l| l.contains("| R-2 |"))
        .expect("plan R-2 row");
    assert!(
        r2.contains("contract-maintenance-gate.sh"),
        "plan R-2 must cite the gate: {r2}"
    );

    let readme = fs::read_to_string(repo_path("README.md")).unwrap();
    assert!(readme.contains("scripts/contract-maintenance-gate.sh"));

    let agents = fs::read_to_string(repo_path("AGENTS.md")).unwrap();
    assert!(agents.contains("tests/contract_maintenance.rs"));
}

#[test]
fn gate_script_exists_and_is_executable() {
    let gate = repo_path("scripts/contract-maintenance-gate.sh");
    assert!(gate.is_file(), "gate script must exist");
    let mode = fs::metadata(&gate).unwrap().permissions().mode();
    assert_eq!(
        mode & 0o111,
        0o111,
        "the gate must be executable like the other probe scripts"
    );
}
