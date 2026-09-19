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
        r#"#!/usr/bin/env bash
set -eu
# Record every argument the canary passed, so the tests can pin the flag
# contract: the pooled leg adds exactly --pool-socket <path>, and neither leg
# may ever carry a print/API-path flag.
if [ -n "${FAKE_ARGS_FILE:-}" ]; then
    printf '%s\n' "$@" > "$FAKE_ARGS_FILE"
fi
# Serve mode (the pooled leg's daemon). FAKE_SERVE_MODE picks the shape:
#   warm   — print the readiness contract line, then linger until TERM'd
#   exit   — die during warmup (pool_daemon_exited)
#   silent — never become ready (pool_daemon_warmup_timeout)
if [ "${1:-}" = "serve" ]; then
    if [ -n "${FAKE_SERVE_PIDFILE:-}" ]; then
        printf '%s\n' "$$" > "$FAKE_SERVE_PIDFILE"
    fi
    case "${FAKE_SERVE_MODE:-warm}" in
        exit)
            echo "fake pool daemon: dying during warmup" >&2
            exit 7
            ;;
        silent)
            echo "fake pool daemon: warming forever" >&2
            exec sleep 300
            ;;
        *)
            echo "[fake pool] Worker fake-worker settled and ready in 0.1s" >&2
            trap 'exit 0' TERM
            while :; do sleep 0.5; done
            ;;
    esac
fi
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

// ── Pooled leg (CLAUDE_PRINT_POOL=1, AS-4 on the warm-pool path) ────────────

/// Everything the pooled-leg tests share with the stateless ones, plus the
/// env knobs that drive the fake daemon and, where given, the arg-record file
/// the fake writes every canary-passed argument into.
struct PooledRun {
    output: Output,
    args: Option<String>,
    serve_pid: Option<u32>,
    socket: PathBuf,
}

fn run_pooled_canary(root: &Path, extra_env: &[(&str, &str)], record_args: bool) -> PooledRun {
    let fake = install_fake_claude_print(root);
    let home = root.join("home");
    let state = root.join("state");
    fs::create_dir_all(&home).unwrap();

    let args_file = root.join("fake-args.txt");
    let pidfile = root.join("fake-serve.pid");

    let mut cmd = Command::new("bash");
    cmd.arg(repo_path("scripts/billing-canary.sh"))
        .env("HOME", &home)
        .env("XDG_STATE_HOME", &state)
        .env("CLAUDE_PRINT_BIN", &fake)
        .env(
            "CLAUDE_PRINT_CHECK_BILLING",
            repo_path("scripts/check-billing.sh"),
        )
        .env("CLAUDE_PRINT_POOL", "1");
    if record_args {
        cmd.env("FAKE_ARGS_FILE", &args_file);
    }
    cmd.env("FAKE_SERVE_PIDFILE", &pidfile);
    for (key, value) in extra_env {
        cmd.env(key, value);
    }
    let output = cmd.output().unwrap();

    let args = record_args
        .then(|| fs::read_to_string(&args_file).expect("the fake must record its arguments"));
    let serve_pid = fs::read_to_string(&pidfile)
        .ok()
        .map(|s| s.trim().parse::<u32>().expect("pidfile pid"));
    let socket = state.join("claude-print/billing-canary/pool.sock");

    PooledRun {
        output,
        args,
        serve_pid,
        socket,
    }
}

/// The child session must stay on the CLI path: no print/API-path flag may
/// ever reach the wrapped binary, in either leg — that is the classification
/// the AS-4 canary exists to verify.
fn assert_no_print_path_flags(args: &str, leg: &str) {
    for line in args.lines() {
        assert_ne!(
            line, "-p",
            "{leg}: the canary must never pass the -p print-path flag"
        );
        assert_ne!(
            line, "--print",
            "{leg}: the canary must never pass the --print API-path flag"
        );
        assert!(
            !line.starts_with("--output-format="),
            "{leg}: claude-print's own --output-format is fine, but a = form \
             aimed at the wrapped CLI is not: {line}"
        );
    }
}

fn proc_alive(pid: u32) -> bool {
    Path::new(&format!("/proc/{pid}")).exists()
}

/// The pooled leg passes end to end against a warm fake daemon: the canary
/// acquires through `--pool-socket` (the daemon's readiness line was honored,
/// the flag reached the invocation), the child session still classifies as
/// `entrypoint: cli`, the result is recorded `mode=pooled`, and teardown on
/// the success path leaves neither a daemon process nor a socket file behind.
#[test]
fn canary_pooled_leg_passes_through_a_warm_worker_without_the_print_path() {
    let root = tempfile::tempdir().unwrap();
    let run = run_pooled_canary(root.path(), &[], true);

    assert!(
        run.output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&run.output.stderr)
    );
    let result = result_file(root.path());
    assert!(result.starts_with("PASS timestamp="), "{result}");
    assert!(result.contains("mode=pooled"), "{result}");
    assert!(result.contains("entrypoint=cli"), "{result}");
    assert!(result.contains("session_id=canary-session"), "{result}");

    // The pooled invocation went through the pool socket: the recorded
    // arguments carry --pool-socket followed by the canary's private socket
    // path, and nothing else about the invocation changed.
    let args = run.args.as_deref().expect("arguments recorded");
    let lines: Vec<&str> = args.lines().collect();
    let socket_arg = lines
        .iter()
        .position(|l| *l == "--pool-socket")
        .expect("the pooled leg must pass --pool-socket");
    assert_eq!(
        lines.get(socket_arg + 1).map(|l| (*l).to_string()),
        Some(run.socket.to_string_lossy().into_owned()),
        "--pool-socket must name the canary's own socket; args: {args}"
    );
    assert_no_print_path_flags(args, "pooled");

    // Teardown on the success path: the daemon is gone (TERM'd by cleanup)
    // and the private socket file is removed.
    let pid = run
        .serve_pid
        .expect("the fake daemon must write its pidfile");
    assert!(
        !proc_alive(pid),
        "the canary must tear its pool daemon down on exit"
    );
    assert!(
        !run.socket.exists(),
        "the canary must remove its private pool socket on exit"
    );
}

/// The stateless leg (the daily timer's path) passes no pool flag at all —
/// and no print-path flag either.
#[test]
fn canary_stateless_leg_records_no_pool_flags() {
    let root = tempfile::tempdir().unwrap();
    let fake = install_fake_claude_print(root.path());
    let home = root.path().join("home");
    let state = root.path().join("state");
    fs::create_dir_all(&home).unwrap();

    let args_file = root.path().join("fake-args.txt");
    let output = Command::new("bash")
        .arg(repo_path("scripts/billing-canary.sh"))
        .env("HOME", &home)
        .env("XDG_STATE_HOME", &state)
        .env("CLAUDE_PRINT_BIN", &fake)
        .env(
            "CLAUDE_PRINT_CHECK_BILLING",
            repo_path("scripts/check-billing.sh"),
        )
        .env("FAKE_ARGS_FILE", &args_file)
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let args = fs::read_to_string(&args_file).unwrap();
    assert!(
        !args.contains("--pool-socket"),
        "the stateless leg must not touch the pool; args: {args}"
    );
    assert_no_print_path_flags(&args, "stateless");
}

/// A pool daemon that dies during warmup fails the pooled canary loudly with
/// `pool_daemon_exited` — it must never degrade into a stateless run and
/// report a PASS that proves nothing about the pool path.
#[test]
fn canary_pooled_leg_fails_when_the_daemon_exits_during_warmup() {
    let root = tempfile::tempdir().unwrap();
    let run = run_pooled_canary(root.path(), &[("FAKE_SERVE_MODE", "exit")], false);

    assert!(!run.output.status.success());
    let result = result_file(root.path());
    assert!(result.starts_with("FAIL timestamp="), "{result}");
    assert!(result.contains("mode=pooled"), "{result}");
    assert!(result.contains("reason=pool_daemon_exited"), "{result}");
    // The dead daemon left nothing to clean up.
    assert!(!run.socket.exists());
}

/// A daemon that never becomes ready fails the pooled canary with
/// `pool_daemon_warmup_timeout` once the (here, shortened) warmup budget is
/// spent, and the lingering daemon is torn down on the failure path.
#[test]
fn canary_pooled_leg_fails_on_warmup_timeout_and_still_tears_down() {
    let root = tempfile::tempdir().unwrap();
    let run = run_pooled_canary(
        root.path(),
        &[
            ("FAKE_SERVE_MODE", "silent"),
            // 10 tries * 0.1 s — the failure shape, without waiting out the
            // real 120 s operational budget.
            ("CLAUDE_PRINT_POOL_WARMUP_TRIES", "10"),
        ],
        false,
    );

    assert!(!run.output.status.success());
    let result = result_file(root.path());
    assert!(result.starts_with("FAIL timestamp="), "{result}");
    assert!(
        result.contains("reason=pool_daemon_warmup_timeout"),
        "{result}"
    );
    assert!(result.contains("mode=pooled"), "{result}");

    // Cleanup ran on the failure path too: the never-ready daemon is gone.
    let pid = run
        .serve_pid
        .expect("the fake daemon must write its pidfile");
    assert!(
        !proc_alive(pid),
        "the canary must kill its pool daemon on the failure path"
    );
    assert!(!run.socket.exists());
}
