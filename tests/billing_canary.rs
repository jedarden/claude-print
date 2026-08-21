use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn repo_path(relative: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(relative)
}

fn install_fake_claude_print(root: &Path) -> PathBuf {
    let fake = root.join("fake-claude-print");
    fs::write(
        &fake,
        r#"#!/bin/bash
set -eu
if [ "${FAKE_INVOCATION_FAIL:-0}" = 1 ]; then
    echo "synthetic invocation failure" >&2
    exit 23
fi
session_id=canary-session
slug=$(printf '%s' "$PWD" | sed 's/[^A-Za-z0-9_-]/-/g')
transcript_dir="$HOME/.claude/projects/$slug"
mkdir -p "$transcript_dir"
printf '{"type":"system","entrypoint":"%s"}\n' \
    "${FAKE_ENTRYPOINT:-cli}" > "$transcript_dir/$session_id.jsonl"
# A concurrent, newer fleet transcript must not influence the canary result.
mkdir -p "$HOME/.claude/projects/unrelated-project"
printf '{"type":"system","entrypoint":"sdk-cli"}\n' \
    > "$HOME/.claude/projects/unrelated-project/unrelated-session.jsonl"
if [ "${FAKE_NULL_SESSION:-0}" = 1 ]; then
    printf '{"type":"result","session_id":null}\n'
else
    printf '{"type":"result","session_id":"%s"}\n' "$session_id"
fi
"#,
    )
    .unwrap();
    fs::set_permissions(&fake, fs::Permissions::from_mode(0o755)).unwrap();
    fake
}

fn run_canary(root: &Path, fake: &Path, entrypoint: &str) -> Output {
    let home = root.join("home");
    let state = root.join("state");
    fs::create_dir_all(&home).unwrap();

    // Redirect HOME only in the child so the canary uses an isolated hierarchy.
    Command::new("bash")
        .arg(repo_path("scripts/billing-canary.sh"))
        .env("HOME", &home)
        .env("XDG_STATE_HOME", &state)
        .env("CLAUDE_PRINT_BIN", fake)
        .env(
            "CLAUDE_PRINT_CHECK_BILLING",
            repo_path("scripts/check-billing.sh"),
        )
        .env("FAKE_ENTRYPOINT", entrypoint)
        .output()
        .unwrap()
}

fn result_file(root: &Path) -> String {
    fs::read_to_string(root.join("state/claude-print/billing-canary/last-result")).unwrap()
}

#[test]
fn canary_passes_for_its_exact_cli_transcript() {
    let root = tempfile::tempdir().unwrap();
    let fake = install_fake_claude_print(root.path());

    let output = run_canary(root.path(), &fake, "cli");

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result = result_file(root.path());
    assert!(result.starts_with("PASS timestamp="), "{result}");
    assert!(result.contains("entrypoint=cli"), "{result}");
    assert!(result.contains("session_id=canary-session"), "{result}");
}

#[test]
fn canary_fails_and_records_unexpected_entrypoint() {
    let root = tempfile::tempdir().unwrap();
    let fake = install_fake_claude_print(root.path());

    let output = run_canary(root.path(), &fake, "sdk-cli");

    assert!(!output.status.success());
    let result = result_file(root.path());
    assert!(result.starts_with("FAIL timestamp="), "{result}");
    assert!(result.contains("reason=billing_classification"), "{result}");
    assert!(result.contains("entrypoint=sdk-cli"), "{result}");
}

#[test]
fn canary_finds_its_dedicated_transcript_when_result_session_id_is_null() {
    let root = tempfile::tempdir().unwrap();
    let fake = install_fake_claude_print(root.path());
    let home = root.path().join("home");
    let state = root.path().join("state");
    fs::create_dir_all(&home).unwrap();

    // Redirect HOME only in the child so the canary uses an isolated hierarchy.
    let output = Command::new("bash")
        .arg(repo_path("scripts/billing-canary.sh"))
        .env("HOME", &home)
        .env("XDG_STATE_HOME", &state)
        .env("CLAUDE_PRINT_BIN", &fake)
        .env(
            "CLAUDE_PRINT_CHECK_BILLING",
            repo_path("scripts/check-billing.sh"),
        )
        .env("FAKE_NULL_SESSION", "1")
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result = result_file(root.path());
    assert!(result.starts_with("PASS timestamp="), "{result}");
    assert!(result.contains("session_id=canary-session"), "{result}");
}

#[test]
fn canary_records_invocation_failures() {
    let root = tempfile::tempdir().unwrap();
    let fake = install_fake_claude_print(root.path());
    let home = root.path().join("home");
    let state = root.path().join("state");
    fs::create_dir_all(&home).unwrap();

    // Redirect HOME only in the child so the canary uses an isolated hierarchy.
    let output = Command::new("bash")
        .arg(repo_path("scripts/billing-canary.sh"))
        .env("HOME", &home)
        .env("XDG_STATE_HOME", &state)
        .env("CLAUDE_PRINT_BIN", &fake)
        .env(
            "CLAUDE_PRINT_CHECK_BILLING",
            repo_path("scripts/check-billing.sh"),
        )
        .env("FAKE_INVOCATION_FAIL", "1")
        .output()
        .unwrap();

    assert!(!output.status.success());
    let result = result_file(root.path());
    assert!(result.starts_with("FAIL timestamp="), "{result}");
    assert!(result.contains("reason=invocation_failed"), "{result}");
    assert!(result.contains("exit_code=23"), "{result}");
}
