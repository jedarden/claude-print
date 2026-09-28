//! Contract tests for the recorded verification-evidence validator.
//!
//! The validator is deliberately a repository script: evidence is written in
//! bead close reasons and review notes, outside the compiled application. The
//! four valid fixtures cover the remote/local × complete/targeted matrix, and
//! the negative fixtures plus mutation tests prove that provenance and
//! coverage are derived from captured output rather than trusted from prose.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

const SCRIPT: &str = "scripts/check-verification-evidence.sh";
const FIXTURES: &str = "tests/fixtures";
const FIXTURE_MANIFEST: &str = "tests/fixtures/verification_evidence_cases_v1.json";

fn repo_root() -> PathBuf {
    std::env::var_os("CLAUDE_PRINT_TEST_REPO")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")))
}

fn repo_path(relative: &str) -> PathBuf {
    repo_root().join(relative)
}

fn fixture(name: &str) -> PathBuf {
    repo_path(FIXTURES).join(name)
}

fn fixture_manifest() -> serde_json::Value {
    serde_json::from_str(
        &fs::read_to_string(repo_path(FIXTURE_MANIFEST)).expect("fixture manifest"),
    )
    .expect("fixture manifest JSON")
}

fn run_file(path: &Path) -> Output {
    Command::new("bash")
        .arg(repo_path(SCRIPT))
        .arg(path)
        .output()
        .unwrap_or_else(|error| panic!("run {}: {error}", path.display()))
}

fn run_fixture(name: &str) -> Output {
    run_file(&fixture(name))
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

#[test]
fn fixture_manifest_is_exhaustive_and_executable() {
    let manifest = fixture_manifest();
    assert_eq!(manifest["contract"], "verification-evidence-fixtures");
    assert_eq!(manifest["contract_version"], 1);
    assert_eq!(manifest["fixture_glob"], "verification_evidence_*.txt");

    let cases = manifest["cases"].as_array().expect("manifest cases array");
    assert_eq!(cases.len(), 13, "the v1 fixture inventory changed shape");

    let expected_ids = [
        "valid-remote-complete",
        "valid-remote-targeted",
        "valid-local-complete",
        "valid-local-targeted",
        "reject-failed-remote",
        "reject-site-claims-local",
        "reject-site-claims-remote",
        "reject-axes-unstated",
        "reject-targeted-selector-unnamed",
        "reject-annotation-on-executed-line",
        "reject-complete-one-leg",
        "reject-output-tells-unbacked",
        "reject-missing-output-fence",
    ];
    let actual_ids: Vec<&str> = cases
        .iter()
        .map(|case| case["id"].as_str().expect("case id"))
        .collect();
    assert_eq!(actual_ids, expected_ids);

    let fixture_files: std::collections::BTreeSet<String> = fs::read_dir(repo_path(FIXTURES))
        .expect("fixture directory")
        .map(|entry| {
            entry
                .expect("fixture entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .filter(|name| name.starts_with("verification_evidence_") && name.ends_with(".txt"))
        .collect();
    let manifest_files: std::collections::BTreeSet<String> = cases
        .iter()
        .map(|case| case["file"].as_str().expect("case fixture file").to_owned())
        .collect();
    assert_eq!(manifest_files, fixture_files);

    let expected_valid = [
        (
            "valid-remote-complete",
            0,
            "evidence valid: remote, complete",
        ),
        (
            "valid-remote-targeted",
            0,
            "evidence valid: remote, targeted",
        ),
        ("valid-local-complete", 0, "evidence valid: local, complete"),
        ("valid-local-targeted", 0, "evidence valid: local, targeted"),
    ];
    for (id, expected_exit, expected_summary) in expected_valid {
        let case = cases
            .iter()
            .find(|case| case["id"] == id)
            .expect("positive case in manifest");
        assert_eq!(case["expected"]["outcome"], "accept");
        assert_eq!(case["expected"]["exit_code"], expected_exit);
        assert_eq!(case["expected"]["summary"], expected_summary);
    }

    for case in cases {
        let id = case["id"].as_str().expect("case id");
        let file = case["file"].as_str().expect("case fixture file");
        let expected = &case["expected"];
        let rationale = case["rationale"].as_str().expect("case rationale");
        assert!(rationale.len() >= 20, "{id} needs a useful rationale");
        assert!(fixture(file).is_file(), "{id} points at a missing fixture");

        let result = run_fixture(file);
        let expected_exit = expected["exit_code"].as_i64().expect("expected exit code");
        assert_eq!(result.status.code(), Some(expected_exit as i32), "{id}");
        match expected["outcome"].as_str().expect("expected outcome") {
            "accept" => {
                assert_eq!(
                    text(&result.stdout),
                    format!("{}\n", expected["summary"].as_str().expect("summary")),
                    "{id} summary"
                );
                assert!(result.stderr.is_empty(), "{id}: unexpected stderr");
            }
            "reject" => {
                let rule = expected["rule"].as_str().expect("rejection rule");
                assert!(
                    text(&result.stderr).contains(&format!("FAIL {rule}:")),
                    "{id} should name {rule}: {}",
                    text(&result.stderr)
                );
                assert!(
                    result.stdout.is_empty(),
                    "{id}: rejected evidence printed valid output"
                );
            }
            outcome => panic!("{id}: unsupported manifest outcome {outcome:?}"),
        }
    }
}

#[test]
fn validator_script_has_the_documented_shape_and_vocabulary() {
    let path = repo_path(SCRIPT);
    let metadata = fs::metadata(&path).expect("validator script metadata");
    assert!(
        metadata.permissions().mode() & 0o111 != 0,
        "{SCRIPT} must be executable"
    );
    let body = fs::read_to_string(path).expect("validator script");
    assert_eq!(body.lines().next(), Some("#!/usr/bin/env bash"));
    assert!(body.contains("set -euo pipefail"));
    for vocabulary in [
        "[cargo-remote] submitting",
        "[cargo-remote] workflow:",
        "streaming logs from",
        "[cargo-remote] PASSED",
        "[cargo-remote] FAILED",
        "falling back to local",
        "no git remote",
        "uncommitted changes detected",
        "push failed",
        "submit failed",
        "--tests",
        "--doc",
        "verified:",
        "cargo-output",
    ] {
        assert!(
            body.contains(vocabulary),
            "{SCRIPT} must retain the evidence vocabulary {vocabulary:?}"
        );
    }
}

#[test]
fn four_execution_mode_corners_validate() {
    for (name, expected) in [
        (
            "verification_evidence_valid_remote_complete.txt",
            "evidence valid: remote, complete\n",
        ),
        (
            "verification_evidence_valid_remote_targeted.txt",
            "evidence valid: remote, targeted\n",
        ),
        (
            "verification_evidence_valid_local_complete.txt",
            "evidence valid: local, complete\n",
        ),
        (
            "verification_evidence_valid_local_targeted.txt",
            "evidence valid: local, targeted\n",
        ),
    ] {
        let result = run_fixture(name);
        assert!(result.status.success(), "{name}: {}", text(&result.stderr));
        assert_eq!(text(&result.stdout), expected, "{name} summary");
        assert!(result.stderr.is_empty(), "{name}: unexpected stderr");
    }
}

#[test]
fn misleading_fixtures_fail_with_a_rule() {
    for (name, rule) in [
        (
            "verification_evidence_misleading_site_claims_remote.txt",
            "site-mismatch",
        ),
        (
            "verification_evidence_misleading_site_claims_local.txt",
            "site-mismatch",
        ),
        (
            "verification_evidence_misleading_complete_one_leg.txt",
            "coverage-mismatch",
        ),
        (
            "verification_evidence_misleading_annotated_verified_line.txt",
            "annotation-on-executed-line",
        ),
        (
            "verification_evidence_misleading_unnamed_selector.txt",
            "selector-unnamed",
        ),
        (
            "verification_evidence_misleading_failed_remote_run.txt",
            "remote-outcome",
        ),
        (
            "verification_evidence_misleading_mode_unbacked.txt",
            "output-tells",
        ),
        (
            "verification_evidence_misleading_axes_unstated.txt",
            "axis-unstated",
        ),
    ] {
        let result = run_fixture(name);
        assert_eq!(result.status.code(), Some(1), "{name}");
        assert!(
            text(&result.stderr).contains(&format!("FAIL {rule}:")),
            "{name} should name {rule}: {}",
            text(&result.stderr)
        );
        assert!(
            result.stdout.is_empty(),
            "invalid evidence cannot print valid"
        );
    }
}

#[test]
fn malformed_or_missing_evidence_is_exit_two() {
    let malformed = run_fixture("verification_evidence_malformed_missing_output_fence.txt");
    assert_eq!(malformed.status.code(), Some(2));
    assert!(text(&malformed.stderr).contains("FAIL fence:"));

    let missing = Command::new("bash")
        .arg(repo_path(SCRIPT))
        .arg(repo_path("tests/fixtures/does-not-exist.txt"))
        .output()
        .expect("run missing evidence");
    assert_eq!(missing.status.code(), Some(2));
    assert!(text(&missing.stderr).contains("usage:"));

    let no_args = Command::new("bash")
        .arg(repo_path(SCRIPT))
        .output()
        .expect("run validator without arguments");
    assert_eq!(no_args.status.code(), Some(2));
    assert!(text(&no_args.stderr).contains("usage:"));
}

#[test]
fn stdin_is_an_equivalent_evidence_source() {
    let content = fs::read_to_string(fixture("verification_evidence_valid_local_targeted.txt"))
        .expect("read stdin fixture");
    let mut child = Command::new("bash")
        .arg(repo_path(SCRIPT))
        .arg("-")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn stdin validator");
    child
        .stdin
        .take()
        .expect("stdin pipe")
        .write_all(content.as_bytes())
        .expect("write stdin evidence");
    let result = child.wait_with_output().expect("wait for stdin validator");
    assert!(result.status.success(), "{}", text(&result.stderr));
    assert_eq!(text(&result.stdout), "evidence valid: local, targeted\n");
}

fn validate_mutation(label: &str, content: String, expected_rule: &str) {
    let directory = tempfile::tempdir().expect("mutation tempdir");
    let path = directory.path().join(format!("{label}.txt"));
    fs::write(&path, content).expect("mutation evidence");
    let result = run_file(&path);
    assert_eq!(result.status.code(), Some(1), "{label}");
    assert!(
        text(&result.stderr).contains(&format!("FAIL {expected_rule}:")),
        "{label} should fail {expected_rule}: {}",
        text(&result.stderr)
    );
}

#[test]
fn mutation_checks_prove_each_claim_is_read() {
    let complete = fs::read_to_string(fixture("verification_evidence_valid_remote_complete.txt"))
        .expect("complete fixture");
    validate_mutation(
        "missing-doc-leg",
        complete.replace("cargo test --doc\n", ""),
        "coverage-mismatch",
    );
    validate_mutation(
        "wrong-site",
        complete.replace("remote on iad-ci", "local fallback"),
        "site-mismatch",
    );
    validate_mutation(
        "annotated-command",
        complete.replace(
            "cargo test --tests\n",
            "cargo test --tests exit=0 (remote, complete)\n",
        ),
        "annotation-on-executed-line",
    );
    validate_mutation(
        "failed-terminal",
        complete.replace("[cargo-remote] PASSED", "[cargo-remote] FAILED"),
        "remote-outcome",
    );
    validate_mutation(
        "missing-tells",
        complete
            .replace("[cargo-remote] submitting to iad-ci (rust-verify)\n", "")
            .replace("[cargo-remote] workflow: rust-verify-m7x2q\n", "")
            .replace("[cargo-remote] streaming logs from the run\n", ""),
        "output-tells",
    );
    validate_mutation(
        "unnamed-selector",
        fs::read_to_string(fixture("verification_evidence_valid_remote_targeted.txt"))
            .expect("targeted fixture")
            .replacen(
                "cargo test --test docs_build_commands",
                "cargo test --test docs_build_layout",
                1,
            ),
        "selector-unnamed",
    );
}
