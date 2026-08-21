use nix::pty::openpty;
use nix::sys::stat::Mode;
use nix::unistd::mkfifo;
use std::ffi::CString;
use std::os::unix::io::IntoRawFd;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

struct Row {
    name: &'static str,
    pass: bool,
    detail: String,
}

struct Orphan {
    path: PathBuf,
    age: Duration,
}

struct OrphanReport {
    messages: Vec<String>,
    all_removed: bool,
}

fn find_in_path(name: &str) -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths).find_map(|dir| {
            let p = dir.join(name);
            if p.is_file() {
                Some(p)
            } else {
                None
            }
        })
    })
}

/// Plan step 1: verify the `claude` binary is resolvable. An explicit
/// `--claude-binary` path is honored if it points at an existing file; a bare
/// name (or a stale path) falls back to a PATH lookup of its name component.
/// With no flag, the literal "claude" is looked up on PATH.
fn probe_claude_binary(claude_binary: Option<&Path>) -> Row {
    let (resolved, source) = match claude_binary {
        Some(p) if p.is_file() => (Some(p.to_path_buf()), "--claude-binary".to_string()),
        Some(p) => {
            let name = p.file_name().and_then(|n| n.to_str());
            (name.and_then(find_in_path), "PATH".to_string())
        }
        None => (find_in_path("claude"), "PATH".to_string()),
    };

    match resolved {
        Some(p) => Row {
            name: "claude binary",
            pass: true,
            detail: format!("found: {} ({})", p.display(), source),
        },
        None => Row {
            name: "claude binary",
            pass: false,
            detail: "'claude' not found on PATH — pass --claude-binary <path>".to_string(),
        },
    }
}

/// Format one orphan warning line in the plan's documented style:
/// `WARNING: found orphaned temp dir /tmp/claude-print-12345-abc (1.2h old) — run rm -rf to clean up`
fn format_orphan_warning(path: &Path, age: Duration) -> String {
    let hours = age.as_secs_f64() / 3600.0;
    format!(
        "WARNING: found orphaned temp dir {} ({:.1}h old) — run rm -rf to clean up",
        path.display(),
        hours
    )
}

fn format_orphan_removed(path: &Path, age: Duration) -> String {
    let hours = age.as_secs_f64() / 3600.0;
    format!(
        "CLEANED: removed orphaned temp dir {} ({:.1}h old)",
        path.display(),
        hours
    )
}

/// Scan `dir` for `claude-print-*` directories whose mtime is older than
/// `threshold` relative to `now`.
///
/// Pure/injectable (dir + clock supplied by caller) so tests can supply an
/// artificial TMPDIR and a fixed `now` instead of touching real env. This is
/// the shared scan used by both warn-only and cleanup modes.
fn find_orphans_in(dir: &Path, now: SystemTime, threshold: Duration) -> Vec<Orphan> {
    let mut orphans = Vec::new();
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return orphans,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !name.starts_with("claude-print-") {
            continue;
        }
        let Ok(md) = entry.metadata() else {
            continue;
        };
        if !md.is_dir() {
            continue;
        }
        let Ok(modified) = md.modified() else {
            continue;
        };
        let Ok(age) = now.duration_since(modified) else {
            continue;
        };
        if age >= threshold {
            orphans.push(Orphan { path, age });
        }
    }
    orphans.sort_by(|a, b| a.path.cmp(&b.path));
    orphans
}

fn process_orphans_in(
    dir: &Path,
    now: SystemTime,
    threshold: Duration,
    clean: bool,
) -> OrphanReport {
    let mut messages = Vec::new();
    let mut all_removed = true;

    for orphan in find_orphans_in(dir, now, threshold) {
        if !clean {
            messages.push(format_orphan_warning(&orphan.path, orphan.age));
            continue;
        }

        match std::fs::remove_dir_all(&orphan.path) {
            Ok(()) => messages.push(format_orphan_removed(&orphan.path, orphan.age)),
            Err(error) => {
                all_removed = false;
                messages.push(format!(
                    "WARNING: failed to remove orphaned temp dir {}: {}",
                    orphan.path.display(),
                    error
                ));
            }
        }
    }

    OrphanReport {
        messages,
        all_removed,
    }
}

/// Plan step 5: scan the real `$TMPDIR` for orphaned `claude-print-*` dirs
/// older than one hour. Plain `--check` only warns; `--check --clean` removes
/// the same matches and reports each successful removal.
fn process_orphans(clean: bool) -> OrphanReport {
    process_orphans_in(
        &std::env::temp_dir(),
        SystemTime::now(),
        Duration::from_secs(3600),
        clean,
    )
}

fn probe_openpty() -> Row {
    match openpty(None, None) {
        Ok(pty) => {
            drop(pty.master);
            drop(pty.slave);
            Row {
                name: "openpty",
                pass: true,
                detail: "openpty() syscall succeeded".into(),
            }
        }
        Err(e) => Row {
            name: "openpty",
            pass: false,
            detail: format!("openpty() failed: {e}"),
        },
    }
}

fn probe_mkfifo() -> Row {
    let tmp = std::env::temp_dir();
    let path = tmp.join(format!("claude-print-check-{}.fifo", std::process::id()));
    match mkfifo(&path, Mode::S_IRUSR | Mode::S_IWUSR) {
        Ok(_) => {
            let _ = std::fs::remove_file(&path);
            Row {
                name: "mkfifo",
                pass: true,
                detail: format!("mkfifo succeeded (dir: {})", tmp.display()),
            }
        }
        Err(e) => Row {
            name: "mkfifo",
            pass: false,
            detail: format!("mkfifo failed: {e}"),
        },
    }
}

fn wait_with_timeout(pid: nix::unistd::Pid, timeout_secs: u64) -> Option<i32> {
    use nix::sys::wait::{waitpid, WaitPidFlag, WaitStatus};
    let start = std::time::Instant::now();
    loop {
        match waitpid(pid, Some(WaitPidFlag::WNOHANG)) {
            Ok(WaitStatus::Exited(_, code)) => return Some(code),
            Ok(WaitStatus::Signaled(_, _, _)) => return None,
            _ => {}
        }
        if start.elapsed().as_secs() >= timeout_secs {
            let _ = nix::sys::signal::kill(pid, nix::sys::signal::Signal::SIGKILL);
            let _ = waitpid(pid, None);
            return None;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

fn probe_mock_claude_pty(mock_path: &Path) -> Row {
    use nix::unistd::{fork, ForkResult};

    let tmp = std::env::temp_dir();
    let fifo_path = tmp.join(format!(
        "claude-print-check-mock-{}.fifo",
        std::process::id()
    ));

    if mkfifo(&fifo_path, Mode::S_IRUSR | Mode::S_IWUSR).is_err() {
        return Row {
            name: "mock_claude PTY",
            pass: false,
            detail: "mkfifo for round-trip failed".into(),
        };
    }

    // Open FIFO read end O_RDONLY|O_NONBLOCK — on Linux this succeeds immediately
    // even without a writer, allowing mock_claude's O_WRONLY open to succeed.
    let fifo_cstr =
        CString::new(fifo_path.to_string_lossy().as_bytes()).expect("fifo path is valid CStr");
    let fifo_rfd = unsafe { libc::open(fifo_cstr.as_ptr(), libc::O_RDONLY | libc::O_NONBLOCK) };
    if fifo_rfd < 0 {
        let _ = std::fs::remove_file(&fifo_path);
        return Row {
            name: "mock_claude PTY",
            pass: false,
            detail: "open FIFO O_RDONLY|O_NONBLOCK failed".into(),
        };
    }

    let nix::pty::OpenptyResult { master, slave } = match openpty(None, None) {
        Ok(p) => p,
        Err(e) => {
            unsafe { libc::close(fifo_rfd) };
            let _ = std::fs::remove_file(&fifo_path);
            return Row {
                name: "mock_claude PTY",
                pass: false,
                detail: format!("openpty for round-trip failed: {e}"),
            };
        }
    };

    let mock_cstr =
        CString::new(mock_path.to_string_lossy().as_bytes()).expect("mock path is valid CStr");
    let fifo_arg = fifo_cstr.clone();

    let child = match unsafe { fork() } {
        Ok(ForkResult::Parent { child }) => {
            drop(slave);
            child
        }
        Ok(ForkResult::Child) => {
            drop(master);
            unsafe { libc::close(fifo_rfd) };
            let slave_fd = slave.into_raw_fd();
            if unsafe { libc::login_tty(slave_fd) } != 0 {
                unsafe { libc::_exit(127) }
            }
            let _ = nix::unistd::execvp(
                mock_cstr.as_c_str(),
                &[mock_cstr.as_c_str(), fifo_arg.as_c_str()],
            );
            unsafe { libc::_exit(127) }
        }
        Err(e) => {
            drop(master);
            drop(slave);
            unsafe { libc::close(fifo_rfd) };
            let _ = std::fs::remove_file(&fifo_path);
            return Row {
                name: "mock_claude PTY",
                pass: false,
                detail: format!("fork failed: {e}"),
            };
        }
    };

    let exit_code = wait_with_timeout(child, 5);

    drop(master);
    unsafe { libc::close(fifo_rfd) };
    let _ = std::fs::remove_file(&fifo_path);

    match exit_code {
        Some(0) => Row {
            name: "mock_claude PTY",
            pass: true,
            detail: format!(
                "PTY round-trip OK — isatty=true in child ({})",
                mock_path.display()
            ),
        },
        Some(code) => Row {
            name: "mock_claude PTY",
            pass: false,
            detail: format!("mock_claude exited {code} (expected 0; isatty=false)"),
        },
        None => Row {
            name: "mock_claude PTY",
            pass: false,
            detail: "mock_claude timed out or was killed".into(),
        },
    }
}

pub fn run(claude_binary: Option<&Path>) -> i32 {
    run_with_clean(claude_binary, false)
}

pub fn run_with_clean(claude_binary: Option<&Path>, clean: bool) -> i32 {
    let mut rows: Vec<Row> = Vec::new();
    let mut all_pass = true;

    // Step 1: claude binary resolvable on PATH (or via --claude-binary).
    let r = probe_claude_binary(claude_binary);
    if !r.pass {
        all_pass = false;
    }
    rows.push(r);

    // Step 2: openpty.
    let r = probe_openpty();
    if !r.pass {
        all_pass = false;
    }
    rows.push(r);

    // Step 3: mkfifo in $TMPDIR.
    let r = probe_mkfifo();
    if !r.pass {
        all_pass = false;
    }
    rows.push(r);

    // Step 4: optional mock_claude PTY round-trip.
    if let Some(mock_path) = find_in_path("mock_claude") {
        let r = probe_mock_claude_pty(&mock_path);
        if !r.pass {
            all_pass = false;
        }
        rows.push(r);
    }

    let name_w = 20usize;
    let res_w = 6usize;
    println!(
        "{:<name_w$} {:<res_w$} DETAIL",
        "CHECK",
        "RESULT",
        name_w = name_w,
        res_w = res_w
    );
    println!("{}", "-".repeat(72));
    for row in &rows {
        println!(
            "{:<name_w$} {:<res_w$} {}",
            row.name,
            if row.pass { "PASS" } else { "FAIL" },
            row.detail,
            name_w = name_w,
            res_w = res_w
        );
    }

    // Step 5: warn about old temp dirs, or remove them when explicitly asked.
    // Warn-only findings never fail the check; a requested removal failure does.
    let orphan_report = process_orphans(clean);
    if clean && !orphan_report.all_removed {
        all_pass = false;
    }
    println!();
    for message in &orphan_report.messages {
        println!("{message}");
    }

    // Step 6: final verdict.
    if all_pass {
        println!("All checks passed.");
        0
    } else {
        eprintln!("One or more checks FAILED.");
        2
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Set a path's mtime to `target` via FileTimes (works on a read-only fd
    /// for a dir/file the test process owns, as tempdir-created paths are).
    fn set_mtime(path: &Path, target: SystemTime) {
        let f = std::fs::File::open(path).expect("open for set_times");
        let times = std::fs::FileTimes::new().set_modified(target);
        f.set_times(times).expect("set_times");
    }

    #[test]
    fn orphan_warning_format_matches_plan_example() {
        // Exactly the example from plan.md "Doctor Command" step 5.
        let w = format_orphan_warning(
            Path::new("/tmp/claude-print-12345-abc"),
            Duration::from_secs(4320),
        );
        assert_eq!(
            w,
            "WARNING: found orphaned temp dir /tmp/claude-print-12345-abc (1.2h old) \
             — run rm -rf to clean up"
        );
    }

    #[test]
    fn orphan_scan_flags_only_old_claude_print_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let now = SystemTime::now();

        // Old orphan — should be flagged.
        let orphan = dir.join("claude-print-12345-abc");
        std::fs::create_dir(&orphan).unwrap();
        set_mtime(&orphan, now - Duration::from_secs(2 * 3600));

        // Young claude-print-* dir — under the 1h threshold, ignored.
        std::fs::create_dir(dir.join("claude-print-99999-fresh")).unwrap();

        // Old but not ours — ignored.
        let other = dir.join("some-other-old-dir");
        std::fs::create_dir(&other).unwrap();
        set_mtime(&other, now - Duration::from_secs(2 * 3600));

        let orphans = find_orphans_in(dir, now, Duration::from_secs(3600));
        assert_eq!(orphans.len(), 1, "only the one old claude-print-* dir");
        let w = format_orphan_warning(&orphans[0].path, orphans[0].age);
        assert!(
            w.contains("claude-print-12345-abc"),
            "names the orphan: {w}"
        );
        assert!(w.contains("(2.0h old)"), "reports age to 1 decimal: {w}");
        assert!(
            w.contains("— run rm -rf to clean up"),
            "includes cleanup hint: {w}"
        );
    }

    #[test]
    fn orphan_scan_ignores_recent_dirs_and_files() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let now = SystemTime::now();

        // Fresh dir + a claude-print-* FILE (not a dir) — neither is an orphan.
        std::fs::create_dir(dir.join("claude-print-recent")).unwrap();
        let file = dir.join("claude-print-notadir");
        std::fs::write(&file, b"x").unwrap();
        set_mtime(&file, now - Duration::from_secs(2 * 3600));

        let orphans = find_orphans_in(dir, now, Duration::from_secs(3600));
        assert!(orphans.is_empty(), "no orphans expected");
    }

    #[test]
    fn warn_only_scan_does_not_remove_orphans() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let now = SystemTime::now();
        let orphan = dir.join("claude-print-12345-old");
        std::fs::create_dir(&orphan).unwrap();
        set_mtime(&orphan, now - Duration::from_secs(2 * 3600));

        let report = process_orphans_in(dir, now, Duration::from_secs(3600), false);

        assert!(orphan.exists(), "warn-only mode must be non-destructive");
        assert!(report.all_removed);
        assert_eq!(report.messages.len(), 1);
        assert!(report.messages[0].starts_with("WARNING: found orphaned temp dir"));
    }

    #[test]
    fn clean_scan_removes_only_old_matching_directories() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let now = SystemTime::now();

        let orphan = dir.join("claude-print-12345-old");
        std::fs::create_dir(&orphan).unwrap();
        std::fs::write(orphan.join("stop.fifo"), b"fixture").unwrap();
        set_mtime(&orphan, now - Duration::from_secs(2 * 3600));

        let recent = dir.join("claude-print-99999-recent");
        std::fs::create_dir(&recent).unwrap();

        let unrelated = dir.join("unrelated-old-dir");
        std::fs::create_dir(&unrelated).unwrap();
        set_mtime(&unrelated, now - Duration::from_secs(2 * 3600));

        let report = process_orphans_in(dir, now, Duration::from_secs(3600), true);

        assert!(!orphan.exists(), "old matching directory should be removed");
        assert!(
            recent.exists(),
            "recent matching directory must be preserved"
        );
        assert!(unrelated.exists(), "unrelated directory must be preserved");
        assert!(report.all_removed);
        assert_eq!(report.messages.len(), 1);
        assert_eq!(
            report.messages[0],
            format!(
                "CLEANED: removed orphaned temp dir {} (2.0h old)",
                orphan.display()
            )
        );
    }

    #[test]
    fn claude_binary_probe_passes_for_existing_path() {
        // An explicit --claude-binary path that exists → PASS.
        let tmp = tempfile::tempdir().unwrap();
        let fake = tmp.path().join("claude");
        std::fs::write(&fake, b"#!/bin/sh\n").unwrap();
        let row = probe_claude_binary(Some(&fake));
        assert!(row.pass, "existing binary path should PASS: {}", row.detail);
        assert!(row.detail.contains(&fake.display().to_string()));
    }

    #[test]
    fn claude_binary_probe_fails_for_missing_path() {
        // A path that neither exists nor is on PATH → FAIL.
        let row = probe_claude_binary(Some(Path::new("/nonexistent/claude-bf44b9-zzz")));
        assert!(!row.pass, "missing binary should FAIL: {}", row.detail);
    }
}
