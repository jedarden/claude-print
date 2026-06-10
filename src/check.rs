use nix::pty::openpty;
use nix::sys::stat::Mode;
use nix::unistd::mkfifo;
use std::ffi::CString;
use std::os::unix::io::IntoRawFd;
use std::path::{Path, PathBuf};

struct Row {
    name: &'static str,
    pass: bool,
    detail: String,
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

pub fn run() -> i32 {
    let mut rows: Vec<Row> = Vec::new();
    let mut all_pass = true;

    let r = probe_openpty();
    if !r.pass {
        all_pass = false;
    }
    rows.push(r);

    let r = probe_mkfifo();
    if !r.pass {
        all_pass = false;
    }
    rows.push(r);

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
    println!();
    if all_pass {
        println!("All checks passed.");
        0
    } else {
        eprintln!("One or more checks FAILED.");
        2
    }
}
