//! Version-straddle guard for the contract probes (bead claudepr-9fe76ef4).
//!
//! On 2026-09-24 the dev host's auto-updater repointed claude 2.1.281 →
//! 2.1.282 mid-re-pin and the whole probe suite had to run twice — but the
//! recoverable-annoyance shape is not the dangerous one. A straddle that is
//! *not* noticed mixes measurements from two Claude versions into one probe
//! session, and a re-pin built on it stamps one version while individual
//! evidence numbers came from another: the **Measured against:** stamp and
//! the active fixtures "agree" (satisfying the one-pin invariant
//! `tests/contract_maintenance.rs` enforces) while being false. Detection
//! after the fact cannot catch that, so `scripts/probe-version-guard.sh`
//! makes the probes refuse to produce mixable evidence, and these tests pin
//! that guard:
//!
//! - the library under a stubbed `claude` (hermetic single-entry PATH of
//!   real-coreutils symlinks, the `contract_maintenance` pattern): a stable
//!   binary resolves+ pins and stamps `verdict=single-version`; a binary
//!   whose version flips mid-run aborts exit 1 with the STRADDLED verdict;
//!   a repointed *launcher* cannot redirect the pinned run (the evidence
//!   stays single-version and the host-moved-on note fires); a missing or
//!   unparsable claude fails closed exit 2;
//! - wiring: every `probe-*.sh` sources the guard and calls begin/end, with
//!   no private `CLAUDE_BIN` resolution left behind;
//! - the maintenance gate brackets itself the same way: a version flip
//!   between its start capture and its end capture voids the verdict
//!   (exit 2 INDETERMINATE, `version-stability: straddled` in
//!   contract-status.txt, straddle-specific next-steps), while a stable
//!   version is recorded as such;
//! - the recording rule that makes a re-pin provably single-version: the
//!   active `claude_contracts` fixture carries a `version_guard` object
//!   whose `start`/`end` equal `claude_version` — or, for measurements that
//!   predate the guard, an honest `retroactive` provenance (allowed only
//!   for `measured_at` older than the guard's introduction).

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn repo_path(relative: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(relative)
}

/// The version the contract evidence is currently pinned to, parsed from the
/// maintenance doc the same way the detector does (first x.y.z token of the
/// **Measured against:** line).
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

/// Symlink the real core utilities the bash scripts need into the stub bin
/// dir, so a single-entry PATH is self-contained (the `contract_maintenance`
/// pattern). `readlink` is the guard's pin mechanism; `ln` lets a stub
/// repoint a launcher symlink mid-run — the auto-update shape being guarded
/// against. Idempotent: existing links (and stubs) are left alone.
fn link_coreutils(bin: &Path) {
    fs::create_dir_all(bin).unwrap();
    for tool in [
        "bash", "grep", "head", "cat", "mkdir", "dirname", "sort", "wc", "tr", "readlink", "ln",
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

/// The hermetic PATH for guard/gate runs: exactly one stub bin dir. Every
/// binary the scripts reach is either a stub written by the test or a
/// coreutils symlink, so a binary that was not placed there (`claude` in the
/// fail-closed test) is genuinely unreachable on any host layout.
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

/// A `claude` whose `--version` prints `first` for the first `flip_after`
/// invocations and `then` afterwards — a mid-run auto-update, counted in a
/// file so the flip survives across the guard's separate `--version` calls.
fn stub_flipping_claude(dir: &Path, first: &str, then: &str, flip_after: u32, count_file: &Path) {
    write_stub(
        dir,
        "claude",
        &format!(
            r#"set -u
count_file="${{CLAUDE_COUNT_FILE:?CLAUDE_COUNT_FILE not set}}"
n=0
[ -f "$count_file" ] && n=$(cat "$count_file")
n=$((n + 1))
printf '%s' "$n" > "$count_file"
if [ "$n" -gt "{flip_after}" ]; then
    printf '{then} (Claude Code)\n'
else
    printf '{first} (Claude Code)\n'
fi"#
        ),
    );
    let _ = count_file;
}

/// Run a bash snippet with the guard library sourced, on the stub PATH.
fn run_guard(bin: &Path, body: &str, envs: &[(&str, String)]) -> Output {
    let mut cmd = Command::new("bash");
    cmd.arg("-c")
        .arg(format!(
            "source '{}' ; {}",
            repo_path("scripts/probe-version-guard.sh").display(),
            body
        ))
        .env("PATH", stub_path(bin));
    for (key, value) in envs {
        cmd.env(key, value);
    }
    cmd.output().unwrap()
}

/// Run one of the repo's bash scripts on the stub PATH.
fn run_script(script: &str, bin: &Path, args: &[String], envs: &[(&str, String)]) -> Output {
    let mut cmd = Command::new("bash");
    cmd.arg(repo_path(script)).env("PATH", stub_path(bin));
    for (key, value) in envs {
        cmd.env(key, value);
    }
    cmd.args(args).output().unwrap()
}

fn gate_args(dir: &Path) -> Vec<String> {
    vec![
        "--evidence-dir".to_string(),
        dir.join("evidence").display().to_string(),
        "--version-file".to_string(),
        dir.join("last-claude-version.txt").display().to_string(),
        "--skip-live-tests".to_string(),
    ]
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

fn stdout_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).to_string()
}

// ── The guard library against a stubbed claude ───────────────────────────────

#[test]
fn guard_confirms_a_stable_claude() {
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("bin");
    stub_claude(&bin, "9.9.1 (Claude Code)");

    let out = run_guard(&bin, "probe_version_guard_begin; probe_version_guard_end", &[]);

    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr_of(&out));
    let stdout = stdout_of(&out);
    assert!(
        stdout.contains("claude version (start): 9.9.1 (Claude Code)"),
        "{stdout}"
    );
    assert!(
        stdout.contains("claude version (end):   9.9.1 (Claude Code)"),
        "{stdout}"
    );
    assert!(
        stdout.contains("verdict=single-version start=9.9.1 end=9.9.1"),
        "{stdout}"
    );
    // The verdict line names the pinned binary, so a re-pin can record which
    // concrete file produced the evidence.
    assert!(
        stdout.contains(&format!("binary={}", bin.join("claude").display())),
        "{stdout}"
    );
}

/// The 2026-09-24 shape, made fatal: the binary the run pinned changes
/// version between the begin and end captures, so the run aborts as failed
/// and its evidence is explicitly unpinnable.
#[test]
fn guard_aborts_when_the_pinned_binary_changes_mid_run() {
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("bin");
    let count = dir.path().join("claude-count");
    stub_flipping_claude(&bin, "9.9.1", "9.9.2", 1, &count);

    let out = run_guard(
        &bin,
        "probe_version_guard_begin; probe_version_guard_end",
        &[("CLAUDE_COUNT_FILE", count.display().to_string())],
    );

    assert_eq!(out.status.code(), Some(1), "stderr: {}", stderr_of(&out));
    let stdout = stdout_of(&out);
    assert!(
        stdout.contains("verdict=STRADDLED start=9.9.1 end=9.9.2"),
        "{stdout}"
    );
    let stderr = stderr_of(&out);
    assert!(
        stderr.contains("MUST NOT be pinned"),
        "the abort must name the evidence untrustworthy: {stderr}"
    );
    assert!(
        stderr.contains("9.9.1 -> 9.9.2"),
        "the abort must name both versions: {stderr}"
    );
}

/// The PIN half of the guard: `~/.local/bin/claude` is a symlink the
/// auto-updater repoints, so a probe that kept re-resolving it would
/// straddle. The guard resolves once — here the launcher is repointed AFTER
/// the begin capture (the stub does it itself, standing in for the
/// auto-updater), and the run still measures exactly one version, with the
/// host-moved-on note making the drift visible instead of silent.
#[test]
fn guard_pins_the_binary_and_survives_a_repointed_launcher() {
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("bin");
    // `claude` (the launcher symlink) initially points at claude-real-a,
    // which repoints the launcher to claude-real-b after answering — the
    // mid-run auto-update — while the guard keeps invoking claude-real-a.
    write_stub(
        &bin,
        "claude-real-a",
        &format!(
            "printf '8.8.1 (Claude Code)\\n'\nln -sfn {}/claude-real-b {}/claude",
            bin.display(),
            bin.display()
        ),
    );
    write_stub(&bin, "claude-real-b", "printf '8.8.2 (Claude Code)\\n'");
    std::os::unix::fs::symlink(bin.join("claude-real-a"), bin.join("claude")).unwrap();

    let out = run_guard(
        &bin,
        "probe_version_guard_begin; probe_version_guard_end",
        &[],
    );

    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr_of(&out));
    let stdout = stdout_of(&out);
    // The run measured the pinned claude-real-a only: single-version 8.8.1.
    assert!(
        stdout.contains(&format!("binary={}", bin.join("claude-real-a").display())),
        "the guard must pin the resolved target, not the launcher: {stdout}"
    );
    assert!(
        stdout.contains("verdict=single-version start=8.8.1 end=8.8.1"),
        "{stdout}"
    );
    assert!(!stdout.contains("verdict=STRADDLED"), "{stdout}");
    // And the repoint is reported, not hidden: the host moved to 8.8.2.
    assert!(
        stdout.contains("note=host-claude-moved-on path-now=8.8.2"),
        "{stdout}"
    );
}

#[test]
fn guard_fails_closed_without_claude() {
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("bin"); // coreutils only — no claude stub

    let out = run_guard(&bin, "probe_version_guard_begin", &[]);

    assert_eq!(out.status.code(), Some(2), "stderr: {}", stderr_of(&out));
    assert!(
        stderr_of(&out).contains("no claude on PATH"),
        "{}",
        stderr_of(&out)
    );
}

#[test]
fn guard_fails_closed_on_unparsable_version() {
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("bin");
    stub_claude(&bin, "definitely not a version");

    let out = run_guard(&bin, "probe_version_guard_begin", &[]);

    assert_eq!(out.status.code(), Some(2), "stderr: {}", stderr_of(&out));
    assert!(
        stderr_of(&out).contains("unparsable"),
        "{}",
        stderr_of(&out)
    );
}

#[test]
fn guard_end_without_begin_fails_closed() {
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("bin");
    stub_claude(&bin, "9.9.1 (Claude Code)");

    let out = run_guard(&bin, "probe_version_guard_end", &[]);

    assert_eq!(out.status.code(), Some(2), "stderr: {}", stderr_of(&out));
    assert!(
        stderr_of(&out).contains("without a preceding"),
        "{}",
        stderr_of(&out)
    );
}

// ── Wiring: every probe sources the guard ────────────────────────────────────

/// The four model-turn probe scripts must bracket their whole run with the
/// shared guard — begin owning CLAUDE_BIN resolution (no private
/// `command -v claude` resolution may survive, or a probe could still pin
/// nothing) and end as the run's last verdict.
#[test]
fn every_probe_script_is_guarded() {
    for probe in [
        "probe-claude-contracts.sh",
        "probe-stop-toolallowed.sh",
        "probe-tui-second-turn.sh",
        "probe-stop-edge-contracts.sh",
    ] {
        let src = fs::read_to_string(repo_path("scripts").join(probe))
            .unwrap_or_else(|e| panic!("scripts/{probe}: {e}"));
        assert!(
            src.contains("probe-version-guard.sh"),
            "{probe} must source the shared version guard"
        );
        assert!(
            src.contains("probe_version_guard_begin"),
            "{probe} must open the version bracket"
        );
        assert!(
            src.contains("probe_version_guard_end"),
            "{probe} must close the version bracket"
        );
        assert!(
            !src.contains("CLAUDE_BIN=\"$(command -v claude)\""),
            "{probe} must not resolve CLAUDE_BIN itself — the guard owns resolution+ pinning"
        );
    }
}

// ── The maintenance gate brackets itself ─────────────────────────────────────

/// The same straddle voids the gate's own verdict: a version flip between
/// the gate's start capture (before detection) and its end capture means
/// neither CURRENT nor DRIFT is certifiable — exit 2 INDETERMINATE, fail
/// closed, with the straddle named in the status and next-steps. The stub
/// flips after the third `--version` call: gate start, detector, version
/// artifact all see the pin; only the end capture sees the bump.
#[test]
fn gate_fails_closed_when_claude_updates_mid_gate() {
    let pin = doc_pin();
    let live = bumped(&pin);
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("bin");
    let count = dir.path().join("claude-count");
    stub_flipping_claude(&bin, &pin, &live, 3, &count);

    let out = run_script(
        "scripts/contract-maintenance-gate.sh",
        &bin,
        &gate_args(dir.path()),
        &[("CLAUDE_COUNT_FILE", count.display().to_string())],
    );

    assert_eq!(out.status.code(), Some(2), "stderr: {}", stderr_of(&out));
    let evidence = dir.path().join("evidence");
    assert_eq!(status_value(&evidence, "alert"), "indeterminate");
    assert_eq!(
        status_value(&evidence, "version-stability"),
        format!("straddled (start={pin} end={live})"),
        "the straddle must be named with both versions"
    );
    assert_eq!(status_value(&evidence, "gate-exit"), "2");
    let next = read_text(&evidence.join("next-steps.txt"));
    assert!(
        next.contains("STRADDLED") && next.contains(&format!("start={pin} -> end={live}")),
        "next-steps must explain the straddle and the re-run:\n{next}"
    );
}

/// A gate run whose version holds records that fact — `version-stability:
/// stable (<v>)` — and is otherwise exactly the gate it was before the
/// bracket existed.
#[test]
fn gate_stable_run_records_version_stability() {
    let pin = doc_pin();
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("bin");
    stub_claude(&bin, &format!("{pin} (Claude Code)"));

    let out = run_script(
        "scripts/contract-maintenance-gate.sh",
        &bin,
        &gate_args(dir.path()),
        &[],
    );

    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr_of(&out));
    let evidence = dir.path().join("evidence");
    assert_eq!(status_value(&evidence, "version-stability"), format!("stable ({pin})"));
}

// ── The recording rule: the fixture proves its own single-version run ───────

/// The date the version guard landed (claudepr-9fe76ef4). A fixture measured
/// before it may carry an honest `retroactive` provenance instead of a
/// bracket — the guard did not exist to run — but a fixture measured on or
/// after it must record the bracket the probes stamped.
const GUARD_INTRODUCED: &str = "2026-09-26";

/// The active claude_contracts fixture (selected by tests/claude_contracts.rs)
/// must carry a `version_guard` record beside its write: either a real
/// bracket with `start == end == claude_version` — the provably
/// single-version re-pin — or a `retroactive` provenance, permitted only for
/// measurements older than the guard itself so the escape hatch cannot hide
/// a modern unguarded pin.
#[test]
fn active_fixture_records_its_single_version_guard() {
    let selector = fs::read_to_string(repo_path("tests/claude_contracts.rs")).unwrap();
    let marker = "include_str!(\"fixtures/claude_contracts_v";
    let at = selector
        .find(marker)
        .expect("tests/claude_contracts.rs must include_str! its version-pinned fixture");
    let rest = &selector[at + marker.len()..];
    let end = rest
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(rest.len());
    let version = rest[..end].trim_end_matches('.');
    let fixture_path = repo_path(&format!("tests/fixtures/claude_contracts_v{version}.json"));
    let fixture: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(&fixture_path)
            .unwrap_or_else(|e| panic!("active fixture {}: {e}", fixture_path.display())),
    )
    .unwrap();

    let claude_version = fixture["claude_version"]
        .as_str()
        .expect("claude_version must be a string")
        .to_string();
    let measured_at = fixture["measured_at"]
        .as_str()
        .expect("measured_at must be a string")
        .to_string();
    let guard = fixture
        .get("version_guard")
        .and_then(|g| g.as_object())
        .unwrap_or_else(|| {
            panic!(
                "the active claude_contracts_v{version}.json fixture must carry a \
                 version_guard record beside its write — the re-pin discipline of \
                 docs/notes/claude-contract-probes.md §Re-pin"
            )
        });

    if guard.get("retroactive").and_then(|r| r.as_bool()) == Some(true) {
        assert!(
            measured_at.as_str() < GUARD_INTRODUCED,
            "a retroactive version_guard is honest only for measurements older than the \
             guard itself ({GUARD_INTRODUCED}); this fixture claims measured_at \
             {measured_at} — record a real start/end bracket from the probes' \
             version-guard verdict lines instead"
        );
        let provenance = guard
            .get("provenance")
            .and_then(|p| p.as_str())
            .unwrap_or_default();
        assert!(
            !provenance.is_empty(),
            "a retroactive version_guard must say how single-version-ness was established"
        );
    } else {
        for field in ["start", "end"] {
            let value = guard
                .get(field)
                .and_then(|v| v.as_str())
                .unwrap_or_else(|| panic!("version_guard.{field} must be a version string"));
            assert_eq!(
                value, claude_version,
                "version_guard.{field} ({value}) must equal the fixture's claude_version \
                 ({claude_version}) — a re-pin records the probes' guard bracket, and a \
                 mismatch means the pin and the bracket disagree"
            );
        }
    }
}

// ── Wiring fragments: the guard cannot silently detach from the docs ─────────

#[test]
fn maintenance_doc_and_agents_pin_the_guard() {
    let doc = fs::read_to_string(repo_path("docs/notes/claude-contract-probes.md")).unwrap();
    let section = doc
        .split("## Version guard")
        .nth(1)
        .expect("the doc must keep its §Version guard section");
    for fragment in [
        "scripts/probe-version-guard.sh",
        "verdict=single-version start=<v> end=<v> binary=<path>",
        "verdict=STRADDLED",
        "note=host-claude-moved-on",
        "version-stability: straddled",
        "Pinning a whole re-pin session",
        "export CLAUDE_BIN=\"$HOME/.local/share/claude/versions/<v>\"",
    ] {
        assert!(
            section.contains(fragment),
            "§Version guard lost wiring fragment: {fragment}"
        );
    }
    // The re-pin procedure must keep the recording step beside the write.
    let re_pin = doc
        .split("**Re-pin**")
        .nth(1)
        .expect("§Re-pin must exist")
        .split("**File follow-ups**")
        .next()
        .unwrap();
    assert!(
        re_pin.contains("version_guard"),
        "§Re-pin must record the guard verdict in the fixture's version_guard object"
    );
    assert!(
        re_pin.contains("start == end == claude_version"),
        "§Re-pin must state the agreement the fixture record must show"
    );

    let agents = fs::read_to_string(repo_path("AGENTS.md")).unwrap();
    assert!(
        agents.contains("scripts/probe-version-guard.sh"),
        "AGENTS.md must name the guard script"
    );
    assert!(
        agents.contains("tests/probe_version_guard.rs"),
        "AGENTS.md must classify the guard's pinning suite"
    );
}
