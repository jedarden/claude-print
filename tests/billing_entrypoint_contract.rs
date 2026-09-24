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
use std::fs::{self, FileTimes};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, UNIX_EPOCH};

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
    let output = Command::new(&binary)
        .arg("--check")
        .output()
        .expect("run claude-print --check");
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
        String::from_utf8_lossy(&output.stderr).contains("No top-level entrypoint field"),
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

// ── Path selection, transcript discovery, and exit codes ───────────────────
//
// The gate has two modes (AGENTS.md, scripts table): a transcript path
// argument is graded exactly — the automated canary's protection against
// concurrent NEEDLE sessions — and no argument means "the newest transcript
// under the discovery tree", the manual release gate. The tests below pin
// both modes against sandboxed fixtures only: every run redirects HOME into
// the temp root, and default-mode runs point CLAUDE_PRINT_TRANSCRIPTS_DIR —
// the script's own override, pinned here as part of the contract — at a
// synthetic tree, so no test can ever grade this machine's real transcripts.

/// Run check-billing.sh with `args` (0 for default mode, 1 for exact-path
/// mode) over a sandboxed HOME, applying `envs` on top. A caller opts into
/// the synthetic discovery tree by passing CLAUDE_PRINT_TRANSCRIPTS_DIR;
/// without it the script resolves its real default under the sandboxed HOME.
fn run_check_billing_sandboxed(
    sandbox: &Path,
    envs: &[(&str, &Path)],
    args: &[&Path],
) -> std::process::Output {
    let mut cmd = Command::new("bash");
    cmd.arg(repo_path("scripts/check-billing.sh"));
    for arg in args {
        cmd.arg(arg);
    }
    cmd.env("HOME", sandbox.join("home"));
    for (key, value) in envs {
        cmd.env(key, value);
    }
    cmd.output().expect("run scripts/check-billing.sh")
}

/// Default-mode run whose discovery tree lives at `<root>/transcripts` — the
/// directory `write_transcript_at` populates below.
fn run_check_billing_discovery(root: &Path) -> std::process::Output {
    run_check_billing_sandboxed(
        root,
        &[("CLAUDE_PRINT_TRANSCRIPTS_DIR", &root.join("transcripts"))],
        &[],
    )
}

/// A transcript record shaped like the real evidence: ordinary TUI-record
/// fields with the classification carried as a top-level `entrypoint` field.
fn evidence_line(entrypoint: &str) -> String {
    format!(r#"{{"type":"system","sessionId":"sess","entrypoint":"{entrypoint}"}}"#) + "\n"
}

/// Write a transcript, creating its per-project parent directories, and pin
/// its mtime so "newest transcript" is an ordered fact rather than a race
/// between files created in the same second.
fn write_transcript_at(path: &Path, body: &str, epoch_secs: u64) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create transcript directory");
    }
    fs::write(path, body).expect("write transcript");
    fs::OpenOptions::new()
        .write(true)
        .open(path)
        .expect("open transcript to stamp its mtime")
        .set_times(FileTimes::new().set_modified(UNIX_EPOCH + Duration::from_secs(epoch_secs)))
        .expect("stamp transcript mtime");
}

/// Exact-path mode grades the file it was handed and nothing else. Even with
/// a *newer* contradicting transcript sitting in the discovery tree — the
/// concurrent-NEEDLE-session shape the canary exists to defuse — the explicit
/// path decides, and its cli evidence passes.
#[test]
fn check_billing_grades_exactly_the_transcript_path_it_is_given() {
    let root = tempfile::tempdir().expect("tempdir");
    let mine = root.path().join("canary-session.jsonl");
    write_transcript_at(&mine, &evidence_line("cli"), 1_700_000_000);
    let newer = root
        .path()
        .join("transcripts/unrelated-project/newer.jsonl");
    write_transcript_at(&newer, &evidence_line("sdk-cli"), 1_700_000_500);

    let output = run_check_billing_sandboxed(root.path(), &[], &[&mine]);

    assert_eq!(
        output.status.code(),
        Some(0),
        "the graded file says cli; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("entrypoint: cli"), "{stdout}");
    assert!(stdout.contains("SUBSCRIPTION"), "{stdout}");
    let mine = mine.to_string_lossy().into_owned();
    assert!(
        stdout.contains(mine.as_str()),
        "the explicit path must be the transcript inspected; stdout: {stdout}"
    );
}

/// A path argument pointing at nothing is an input failure, not a usage
/// failure: exit 1 (2 is reserved for argument-count errors below) naming the
/// missing file.
#[test]
fn check_billing_missing_transcript_path_exits_1() {
    let root = tempfile::tempdir().expect("tempdir");
    let missing = root.path().join("no-such-session.jsonl");

    let output = run_check_billing_sandboxed(root.path(), &[], &[&missing]);

    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Transcript not found") && stderr.contains("no-such-session.jsonl"),
        "{stderr}"
    );
}

/// More than one argument never grades anything: usage on stderr and exit 2 —
/// the code that tells the canary installer (and a human) that the
/// invocation, not the transcript, is wrong.
#[test]
fn check_billing_extra_arguments_exit_2_with_usage() {
    let root = tempfile::tempdir().expect("tempdir");
    let a = root.path().join("a.jsonl");
    let b = root.path().join("b.jsonl");
    write_transcript_at(&a, &evidence_line("cli"), 1_700_000_000);
    write_transcript_at(&b, &evidence_line("cli"), 1_700_000_001);

    let output = run_check_billing_sandboxed(root.path(), &[], &[&a, &b]);

    assert_eq!(output.status.code(), Some(2));
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("Usage:"),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Default mode — the manual release gate — selects the newest transcript by
/// mtime, recursing into the per-project directories, and never a
/// non-.jsonl file no matter how fresh. The newest .jsonl here records
/// `sdk-cli`, so a correct selection FAILS the gate; picking the older cli
/// transcript (first-found, alphabetical) would wrongly pass — exactly the
/// false release signal this test catches. The slug with a blank also pins
/// that discovery keeps whole paths, not just their first whitespace token.
#[test]
fn check_billing_default_mode_selects_the_newest_jsonl() {
    let root = tempfile::tempdir().expect("tempdir");
    let dir = root.path().join("transcripts");
    write_transcript_at(
        &dir.join("old-project/older.jsonl"),
        &evidence_line("cli"),
        1_700_000_000,
    );
    write_transcript_at(
        &dir.join("new project/newer.jsonl"),
        &evidence_line("sdk-cli"),
        1_700_000_100,
    );
    // Newest node in the tree, wrong extension: invisible to discovery.
    write_transcript_at(
        &dir.join("new project/notes.txt"),
        &evidence_line("sdk-cli"),
        1_700_000_200,
    );

    let output = run_check_billing_discovery(root.path());

    assert_eq!(
        output.status.code(),
        Some(1),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("new project/newer.jsonl"),
        "the newest transcript must be the one inspected; stdout: {stdout}"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("AGENT SDK CREDIT POOL"), "{stderr}");
    assert!(stderr.contains("sdk-cli"), "{stderr}");
}

/// The same discovery in the passing direction: when the newest transcript
/// carries `cli` the gate passes — and a selection that picked the older
/// contradicting transcript would fail it, so both orderings are pinned.
#[test]
fn check_billing_default_mode_passes_when_the_newest_transcript_is_cli() {
    let root = tempfile::tempdir().expect("tempdir");
    let dir = root.path().join("transcripts");
    write_transcript_at(
        &dir.join("old-project/older.jsonl"),
        &evidence_line("sdk-cli"),
        1_700_000_000,
    );
    write_transcript_at(
        &dir.join("new-project/newer.jsonl"),
        &evidence_line("cli"),
        1_700_000_100,
    );

    let output = run_check_billing_discovery(root.path());

    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("entrypoint: cli"), "{stdout}");
    assert!(stdout.contains("SUBSCRIPTION"), "{stdout}");
    assert!(
        stdout.contains("new-project/newer.jsonl"),
        "the newest transcript must be the one inspected; stdout: {stdout}"
    );
}

/// Without the override the discovery tree is the script's real default under
/// HOME — `$HOME/.claude/projects/<slug>/<session>.jsonl`, the layout the
/// release checklist relies on — resolved here against a sandboxed HOME.
#[test]
fn check_billing_default_projects_dir_resolves_under_home() {
    let root = tempfile::tempdir().expect("tempdir");
    write_transcript_at(
        &root
            .path()
            .join("home/.claude/projects/-home-coding-proj/sess.jsonl"),
        &evidence_line("cli"),
        1_700_000_000,
    );

    let output = run_check_billing_sandboxed(root.path(), &[], &[]);

    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("entrypoint: cli"),
        "stdout: {}",
        String::from_utf8_lossy(&output.stdout)
    );
}

/// A discovery tree that exists but holds no transcript is its own failure:
/// exit 1 saying nothing was ever inspected, never a pass by absence.
#[test]
fn check_billing_default_mode_without_transcripts_exits_1() {
    let root = tempfile::tempdir().expect("tempdir");
    fs::create_dir_all(root.path().join("transcripts")).expect("create empty discovery tree");

    let output = run_check_billing_discovery(root.path());

    assert_eq!(output.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("No transcript JSONL files found"),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// No discovery tree at all is the first-run shape: exit 1 naming the missing
/// directory — never a panic, never a pass.
#[test]
fn check_billing_default_mode_without_projects_dir_exits_1() {
    let root = tempfile::tempdir().expect("tempdir");

    let output = run_check_billing_discovery(root.path());

    assert_eq!(output.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("Claude projects directory not found"),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// A truncated event must not poison an otherwise useful transcript: the
/// script isolates lines (its own comment promises this), so a partial line —
/// even one carrying a truncated `"entrypoint"` — is skipped and the valid
/// evidence on the next line decides. Interruptions really do leave sessions
/// ending mid-event.
#[test]
fn check_billing_skips_a_partial_line_and_uses_valid_evidence() {
    let root = tempfile::tempdir().expect("tempdir");
    let transcript = root.path().join("partial-line.jsonl");
    fs::write(
        &transcript,
        concat!(
            // Truncated mid-record: no closing quote, no closing brace. Both
            // extraction paths (jq parse, sed's closing-quote match) must
            // reject it rather than crash the scan.
            r#"{"type":"result","entrypoint":"cl"#,
            "\n",
            r#"{"type":"system","sessionId":"sess","entrypoint":"cli"}"#,
            "\n",
        ),
    )
    .expect("write partial-line transcript");

    let output = run_check_billing_sandboxed(root.path(), &[], &[&transcript]);

    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("entrypoint: cli"),
        "stdout: {}",
        String::from_utf8_lossy(&output.stdout)
    );
}

/// Transcripts carrying no evidence at all — empty, or non-JSON noise
/// without the `"entrypoint"` substring — fail closed: exit 1 and the
/// no-evidence message. Absence of billing data is never a pass.
#[test]
fn check_billing_fails_closed_without_any_evidence() {
    for (name, body) in [
        ("empty.jsonl", ""),
        ("not-json.jsonl", "session crash log\nno json here\n"),
    ] {
        let root = tempfile::tempdir().expect("tempdir");
        let transcript = root.path().join(name);
        fs::write(&transcript, body).expect("write evidence-free transcript");

        let output = run_check_billing_sandboxed(root.path(), &[], &[&transcript]);

        assert_eq!(output.status.code(), Some(1), "{name}");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("No top-level entrypoint field"),
            "{name}: {stderr}"
        );
    }
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
