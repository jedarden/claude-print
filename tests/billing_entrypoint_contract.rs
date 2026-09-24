// The billing-entrypoint contract, pinned end-to-end (bead claudepr-6274dcc6).
//
// Three names circulate around the subscription-billing invariant and they are
// not interchangeable (docs/notes/billing-context.md has the full table):
//
//   cc_entrypoint           — Anthropic's wire-level billing header field on
//                             API requests. Never an environment variable,
//                             never directly observable; no transcript or env
//                             ever carries the literal name.
//   CLAUDE_CODE_ENTRYPOINT  — the authoritative environment input. Forced to
//                             `cli` in the child by FORCED_ENV (src/pty.rs),
//                             overriding any inherited `sdk-cli`.
//   entrypoint (JSONL)      — the JSON evidence: the top-level transcript
//                             field recording the classification Claude Code
//                             actually chose. Asserted by
//                             scripts/check-billing.sh (AS-4).
//
// `CLAUDE_CC_ENTRYPOINT` is a phantom: it appeared once in AGENTS.md invariant
// 5 as something operators were told to verify and matches no variable Claude
// Code reads, sets, or documents. These tests keep the contract's halves
// aligned and the phantom out of operational surfaces.
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn repo_path(relative: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(relative)
}

/// The env-input half: the shipped child-env construction must force exactly
/// one `CLAUDE_CODE_ENTRYPOINT=cli` (and no session markers) over an
/// environment that inherited the SDK path plus every scrubbed marker. This
/// is what `claude-print --check`'s billing row probes (src/check.rs
/// `probe_billing_entrypoint`), so a regression here fails install.sh's
/// post-install smoke and the release gate together.
#[test]
fn child_env_forces_cli_entrypoint_over_inherited_sdk_cli() {
    assert!(
        claude_print::pty::child_env_forces_cli_entrypoint(),
        "child env must force CLAUDE_CODE_ENTRYPOINT=cli over any inherited \
         value and drop every scrubbed session marker"
    );
}

/// The phantom variable must not appear anywhere code or scripts could read
/// or set it. Docs may still mention it — AGENTS.md invariant 5 and
/// docs/notes/billing-context.md name it precisely to deny that it exists —
/// but `src/` and `scripts/` (and the installer that runs them) may not.
#[test]
fn phantom_claude_cc_entrypoint_absent_from_operational_surfaces() {
    let mut offenders = Vec::new();
    for dir in ["src", "scripts"] {
        let root = repo_path(dir);
        let mut stack = vec![root.clone()];
        for entry in walk(&mut stack) {
            let Ok(text) = fs::read_to_string(&entry) else {
                continue; // binary or unreadable — not a textual surface
            };
            if text.contains("CLAUDE_CC_ENTRYPOINT") {
                offenders.push(entry);
            }
        }
    }
    // install.sh sits at the repo root, not under scripts/.
    let install = repo_path("install.sh");
    if let Ok(text) = fs::read_to_string(&install) {
        if text.contains("CLAUDE_CC_ENTRYPOINT") {
            offenders.push(install);
        }
    }

    assert!(
        offenders.is_empty(),
        "CLAUDE_CC_ENTRYPOINT is a phantom variable (no such env var is read \
         or set by claude or claude-print) and must not appear in \
         operational surfaces; found in: {:?}",
        offenders
    );
}

/// The --check output carries the credential-free billing row, so the
/// post-install smoke (install.sh) and the release checklist exercise the
/// env-input half without credentials or a session. The full table prints
/// even when unrelated rows fail (e.g. no `claude` on PATH), so this asserts
/// on row presence, not exit code.
#[test]
fn check_output_carries_the_billing_entrypoint_row() {
    let binary = PathBuf::from(env!("CARGO_BIN_EXE_claude-print"));
    let output = Command::new(&binary).arg("--check").output().expect("run claude-print --check");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("billing entrypoint"),
        "--check must print the billing entrypoint row; got:\n{}",
        stdout
    );
    assert!(
        stdout.contains("CLAUDE_CODE_ENTRYPOINT=cli"),
        "--check billing row must name the forced variable; got:\n{}",
        stdout
    );
}

/// The JSON-evidence half: scripts/check-billing.sh must locate the
/// top-level `entrypoint` field on ANY candidate line — not just the first
/// one — because the substring `"entrypoint"` can also appear nested inside
/// quoted message text, which is what made the manual newest-transcript gate
/// false-fail (and false-pass, had the nested value been `cli`).
///
/// The decoy occurrences are written the way they must appear inside valid
/// JSON string values — `\"entrypoint\":\"…\"` with escaped quotes — so the
/// fixtures exercise the jq path and the sed fallback identically: neither
/// extraction can mistake an escaped, string-embedded occurrence for the
/// top-level field.
#[test]
fn check_billing_finds_top_level_entrypoint_past_a_nested_decoy() {
    let dir = tempfile_dir();
    let transcript = dir.join("decoy.jsonl");
    fs::write(
        &transcript,
        concat!(
            // A line where the substring only appears nested in message text:
            // the old `grep -m1` + jq pass stopped here and false-failed.
            r#"{"type":"user","message":{"content":"the old transcript said \"entrypoint\":\"sdk-cli\" inside a quote"}}"#,
            "\n",
            // The real evidence, carried top-level on a later event.
            r#"{"type":"system","sessionId":"abc","entrypoint":"cli"}"#,
            "\n",
        ),
    )
    .expect("write decoy transcript");

    let output = run_check_billing(&transcript);
    assert!(
        output.status.success(),
        "top-level entrypoint on a later line must be found; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("entrypoint: cli"),
        "expected the top-level cli evidence on stdout"
    );
    cleanup(&dir);
}

/// A transcript whose only `"entrypoint"` occurrences are nested substrings
/// carries no billing evidence: the script must fail rather than report a
/// value extracted from inside quoted message text.
#[test]
fn check_billing_fails_when_only_nested_occurrences_exist() {
    let dir = tempfile_dir();
    let transcript = dir.join("nested_only.jsonl");
    fs::write(
        &transcript,
        concat!(
            r#"{"type":"user","message":{"content":"the old transcript said \"entrypoint\":\"cli\" inside a quote"}}"#,
            "\n",
        ),
    )
    .expect("write nested-only transcript");

    let output = run_check_billing(&transcript);
    assert!(
        !output.status.success(),
        "nested-only occurrences must not count as billing evidence"
    );
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("No top-level entrypoint field"),
        "failure must name the missing top-level evidence; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    cleanup(&dir);
}

/// The evidence must read `cli`. A transcript recording any other
/// classification (here `sdk-cli`, the metered Agent SDK pool) is a billing
/// regression and must fail the gate.
#[test]
fn check_billing_fails_on_non_cli_entrypoint() {
    let dir = tempfile_dir();
    let transcript = dir.join("sdk.jsonl");
    fs::write(
        &transcript,
        concat!(
            r#"{"type":"system","sessionId":"abc","entrypoint":"sdk-cli"}"#,
            "\n",
        ),
    )
    .expect("write sdk-cli transcript");

    let output = run_check_billing(&transcript);
    assert!(
        !output.status.success(),
        "entrypoint=sdk-cli is a billing regression and must fail"
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("AGENT SDK CREDIT POOL"),
        "failure must name the wrong pool; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    cleanup(&dir);
}

fn run_check_billing(transcript: &Path) -> std::process::Output {
    Command::new("bash")
        .arg(repo_path("scripts/check-billing.sh"))
        .arg(transcript)
        .output()
        .expect("run scripts/check-billing.sh")
}

fn tempfile_dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "claude-print-billing-contract-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
    fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

fn cleanup(dir: &Path) {
    let _ = fs::remove_dir_all(dir);
}

fn walk(stack: &mut Vec<PathBuf>) -> Vec<PathBuf> {
    let mut found = Vec::new();
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else {
                found.push(path);
            }
        }
    }
    found
}
