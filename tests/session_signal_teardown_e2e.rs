//! Binary-level session signal teardown contract (bead claudepr-f49a9797).
//!
//! The compatibility contract's SIGINT/SIGTERM session rows promise the same
//! observable result for either parent signal: the session intercepts it,
//! tears down the child with SIGTERM followed by a bounded SIGKILL fallback,
//! exits 130 with the interrupted error, and removes its temporary relay
//! artifacts. Existing relay tests prove SIGINT forwarding inside
//! `PtySpawner::relay`; this suite drives the compiled `claude-print` binary
//! through `Session::run`, where the relevant child action is the teardown
//! SIGTERM rather than a same-signal relay.
//!
//! The mock's `MOCK_TRAP_TERM_REPORT` handler records `ready` and then
//! survives SIGTERM. The `sigterm` marker therefore proves the session sent
//! SIGTERM to the actual child; survival forces the production two-second
//! grace to expire and exercise the SIGKILL cleanup path. The test also keeps
//! the child's PID and checks `/proc` after the parent exits, so a passing exit
//! code cannot hide a leaked or unreaped child.
//!
//! Each case uses child-env overrides only: HOME, XDG_CONFIG_HOME, and TMPDIR
//! point at throwaway directories, and the mock binary is selected explicitly.
//! The test is one sequential function so its two cases have one clear
//! cleanup boundary and cannot leave a signal-driven child behind if an
//! assertion fails.

use nix::sys::signal::{kill, Signal};
use nix::unistd::Pid;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};
use tempfile::TempDir;

const READY_BUDGET: Duration = Duration::from_secs(15);
const EXIT_BUDGET: Duration = Duration::from_secs(15);
const CLEANUP_BUDGET: Duration = Duration::from_secs(3);

fn workspace_bin(name: &str) -> PathBuf {
    let exe = std::env::current_exe().expect("current_exe");
    exe.parent()
        .and_then(|p| p.parent())
        .expect("test binary must live under target/<profile>/deps/")
        .join(name)
}

fn proc_state(pid: u32) -> Option<char> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let rest = stat.rsplit_once(')')?.1.trim_start();
    rest.chars().next()
}

fn children_of(ppid: u32) -> Vec<u32> {
    let mut children = Vec::new();
    let Ok(entries) = fs::read_dir("/proc") else {
        return children;
    };
    for entry in entries.flatten() {
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        let Ok(pid) = name.parse::<u32>() else {
            continue;
        };
        let Ok(stat) = fs::read_to_string(format!("/proc/{pid}/stat")) else {
            continue;
        };
        let Some(rest) = stat.rsplit_once(')').map(|(_, rest)| rest.trim_start()) else {
            continue;
        };
        let mut fields = rest.split_whitespace();
        fields.next(); // state
        if fields.next().and_then(|field| field.parse().ok()) == Some(ppid) {
            children.push(pid);
        }
    }
    children
}

fn wait_for_child(parent_pid: u32, budget: Duration) -> Vec<u32> {
    let deadline = Instant::now() + budget;
    loop {
        let children = children_of(parent_pid);
        if !children.is_empty() {
            return children;
        }
        assert!(
            Instant::now() < deadline,
            "claude-print never exposed its mock child in /proc/{parent_pid}/stat"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn wait_for_marker(path: &Path, marker: &str, child: &mut Child, budget: Duration) {
    let deadline = Instant::now() + budget;
    loop {
        if fs::read_to_string(path)
            .map(|content| content.contains(marker))
            .unwrap_or(false)
        {
            return;
        }
        if let Some(status) = child.try_wait().expect("poll claude-print") {
            panic!(
                "claude-print exited before mock readiness: {status}; report={:?}",
                fs::read_to_string(path).ok()
            );
        }
        assert!(
            Instant::now() < deadline,
            "mock did not report {marker:?} within {budget:?}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn wait_for_exit(child: &mut Child, budget: Duration) -> std::process::ExitStatus {
    let deadline = Instant::now() + budget;
    loop {
        if let Some(status) = child.try_wait().expect("poll claude-print exit") {
            return status;
        }
        assert!(
            Instant::now() < deadline,
            "claude-print did not exit within {budget:?} after the session signal"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn assert_processes_gone(pids: &[u32]) {
    let deadline = Instant::now() + CLEANUP_BUDGET;
    loop {
        let survivors: Vec<(u32, char)> = pids
            .iter()
            .copied()
            .filter_map(|pid| proc_state(pid).map(|state| (pid, state)))
            .collect();
        if survivors.is_empty() {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "session child remained after claude-print exit (live or zombie): {survivors:?}"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
}

fn assert_relay_artifacts_gone(tmpdir: &Path) {
    let deadline = Instant::now() + CLEANUP_BUDGET;
    loop {
        let leftovers: Vec<_> = fs::read_dir(tmpdir)
            .expect("read isolated TMPDIR")
            .flatten()
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with("claude-print-")
            })
            .map(|entry| entry.path())
            .collect();
        if leftovers.is_empty() {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "session relay artifacts were not cleaned up: {leftovers:?}"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
}

fn run_signal_case(signal: Signal) {
    let bin = workspace_bin("claude-print");
    let mock = workspace_bin("mock-claude");
    assert!(
        bin.is_file(),
        "claude-print binary missing at {}",
        bin.display()
    );
    assert!(
        mock.is_file(),
        "mock-claude binary missing at {}",
        mock.display()
    );

    let sandbox = TempDir::new().expect("signal-case sandbox");
    let home = sandbox.path().join("home");
    let tmpdir = sandbox.path().join("tmp");
    fs::create_dir(&home).expect("create isolated HOME");
    fs::create_dir(&tmpdir).expect("create isolated TMPDIR");
    let report = sandbox.path().join("term-report");

    let mut child = Command::new(&bin)
        .args(["--claude-binary"])
        .arg(&mock)
        .arg("signal teardown prompt")
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", &home)
        .env("TMPDIR", &tmpdir)
        .env("MOCK_TRAP_TERM_REPORT", &report)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap_or_else(|e| panic!("spawn claude-print for {signal:?}: {e}"));

    wait_for_marker(&report, "ready", &mut child, READY_BUDGET);
    let child_pids = wait_for_child(child.id(), READY_BUDGET);
    assert_eq!(
        child_pids.len(),
        1,
        "the signal case must have exactly one mock child: {child_pids:?}"
    );

    kill(Pid::from_raw(child.id() as i32), signal)
        .unwrap_or_else(|e| panic!("send {signal:?} to claude-print: {e}"));
    let status = wait_for_exit(&mut child, EXIT_BUDGET);
    let output = child
        .wait_with_output()
        .unwrap_or_else(|e| panic!("collect {signal:?} output: {e}"));

    assert_eq!(
        status.code(),
        Some(130),
        "{signal:?} session interruption must map to exit 130; stderr={:?}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.stdout.is_empty(),
        "interrupted text mode must not emit a success result on stdout: {:?}",
        String::from_utf8_lossy(&output.stdout)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("error: interrupted by signal"),
        "{signal:?} must use the documented interrupted error shape: {stderr:?}"
    );

    let report_contents = fs::read_to_string(&report).expect("read child signal report");
    assert_eq!(
        report_contents, "ready\nsigterm\n",
        "{signal:?} must reach the child as the teardown SIGTERM before SIGKILL"
    );

    assert_processes_gone(&child_pids);
    assert_relay_artifacts_gone(&tmpdir);
}

#[test]
fn session_signals_forward_teardown_and_clean_up() {
    run_signal_case(Signal::SIGINT);
    run_signal_case(Signal::SIGTERM);
}
