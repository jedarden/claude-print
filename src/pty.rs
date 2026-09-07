use nix::pty::{openpty, OpenptyResult};
use nix::sys::signal::{signal, SigHandler, Signal};
use nix::sys::wait::{waitpid, WaitStatus};
use nix::unistd::{execvpe, fork, ForkResult, Pid};
use std::ffi::{CStr, CString};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::io::{AsRawFd, IntoRawFd, OwnedFd};
use std::sync::atomic::{AtomicBool, Ordering};

use crate::error::{Error, Result};

static SIGWINCH_RECEIVED: AtomicBool = AtomicBool::new(false);
static SIGINT_RECEIVED: AtomicBool = AtomicBool::new(false);

extern "C" fn sigwinch_handler(_: libc::c_int) {
    // SAFETY: AtomicBool::store is async-signal-safe.
    SIGWINCH_RECEIVED.store(true, Ordering::Relaxed);
}

extern "C" fn sigint_handler(_: libc::c_int) {
    // SAFETY: AtomicBool::store is async-signal-safe.
    SIGINT_RECEIVED.store(true, Ordering::Relaxed);
}

pub struct PtySpawner {
    pub master: OwnedFd,
    pub child_pid: Pid,
}

/// Read the window size from `fd`, falling back to 80×24 if it is not a tty.
fn get_winsize(fd: i32) -> libc::winsize {
    let mut ws = libc::winsize {
        ws_row: 24,
        ws_col: 80,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    // SAFETY: TIOCGWINSZ is a read ioctl; `ws` lives on the stack for its duration.
    unsafe {
        libc::ioctl(fd, libc::TIOCGWINSZ, &mut ws);
    }
    if ws.ws_row == 0 {
        ws.ws_row = 24;
    }
    if ws.ws_col == 0 {
        ws.ws_col = 80;
    }
    ws
}

/// Claude Code session markers scrubbed from the child's environment so it
/// starts a fresh, *persistable* top-level session rather than a nested one.
///
/// `CLAUDE_CODE_CHILD_SESSION` is the critical one: claude 2.1.263 gates
/// transcript persistence on it. Inherited, the TUI renders "Transcript saving
/// is off — inherited CLAUDE_CODE_CHILD_SESSION marker" and never writes
/// `~/.claude/projects/<slug>/<session_id>.jsonl`, leaving claude-print's
/// "wait for Stop, read the transcript" design with nothing to read
/// (claudepr-26e7a0b6). Every agent- or NEEDLE-launched run inherits it from
/// its parent session, which is why this only ever bit under fleet dispatch.
const SCRUBBED_ENV: &[&str] = &[
    "CLAUDE_CODE_SESSION_ID",
    "CLAUDECODE",
    "CLAUDE_CODE_CHILD_SESSION",
    "CLAUDE_CODE_SKIP_PROMPT_HISTORY",
];

/// Variables forced on in the child regardless of what the parent had.
///
/// `CLAUDE_CODE_ENTRYPOINT=cli` is the subscription-billing invariant: the
/// parent may have inherited `sdk-cli`, and propagating that would bill the
/// metered SDK pool. `CLAUDE_CODE_FORCE_SESSION_PERSISTENCE=1` is claude's own
/// documented remedy for the persistence gate, and keeps the design assumption
/// alive even if a future claude derives child-session-ness some other way.
const FORCED_ENV: &[(&str, &str)] = &[
    ("CLAUDE_CODE_ENTRYPOINT", "cli"),
    ("CLAUDE_CODE_FORCE_SESSION_PERSISTENCE", "1"),
];

/// Build the child's environment **in the parent, before `fork()`**.
///
/// This must not be done between `fork()` and `exec()`. POSIX permits only
/// async-signal-safe calls there, and `setenv`/`unsetenv` are not: both may
/// allocate. claude-print pool mode is multithreaded (`main.rs` shutdown
/// monitor, `pool.rs` warmup threads and its connection-per-thread accept
/// loop), so a fork racing another thread that holds the allocator lock would
/// deadlock the child before it ever reached `exec` — a rare, load-dependent
/// hang whose profile is precisely the fleet-dispatch workload this code
/// exists to serve. Building the environment up-front and handing it to
/// `execvpe` leaves the child executing only `login_tty` and `execvpe`.
fn build_child_env() -> Vec<CString> {
    scrub_env(std::env::vars_os())
}

/// Pure core of [`build_child_env`], taking the source environment explicitly.
///
/// Split out so it can be tested without mutating the process environment.
/// `HOME`-style global env mutation in tests races across parallel test
/// threads and produces exactly the pass-alone/fail-in-suite flake this crate
/// already has elsewhere; a pure function has no such hazard.
fn scrub_env<I, K, V>(vars: I) -> Vec<CString>
where
    I: IntoIterator<Item = (K, V)>,
    K: AsRef<std::ffi::OsStr>,
    V: AsRef<std::ffi::OsStr>,
{
    let mut env: Vec<CString> = Vec::new();
    for (key, value) in vars {
        let key = key.as_ref();
        let value = value.as_ref();
        let key_bytes = key.as_bytes();
        if SCRUBBED_ENV.iter().any(|s| s.as_bytes() == key_bytes)
            || FORCED_ENV.iter().any(|(s, _)| s.as_bytes() == key_bytes)
        {
            continue;
        }
        let value_bytes = value.as_bytes();
        let mut entry = Vec::with_capacity(key_bytes.len() + 1 + value_bytes.len());
        entry.extend_from_slice(key_bytes);
        entry.push(b'=');
        entry.extend_from_slice(value_bytes);
        // A key or value containing an interior NUL cannot be represented in
        // envp; such an entry is unreachable from the real environment, so
        // dropping it is correct rather than fatal.
        if let Ok(entry) = CString::new(entry) {
            env.push(entry);
        }
    }
    for (key, value) in FORCED_ENV {
        if let Ok(entry) = CString::new(format!("{key}={value}")) {
            env.push(entry);
        }
    }
    env
}

impl PtySpawner {
    /// Open a PTY pair, fork, set the PTY window size, call `login_tty` in the
    /// child to make the slave the controlling terminal, then `execvp` `cmd`.
    ///
    /// `args` contains only the arguments to the program — not argv\[0\].
    /// argv\[0\] is set to `cmd` internally.
    pub fn spawn(cmd: &CStr, args: &[CString]) -> Result<Self> {
        // Built before fork: the child may not allocate. See build_child_env.
        let child_env = build_child_env();

        let OpenptyResult { master, slave } =
            openpty(None, None).map_err(|e| Error::OpenptyFailed(e.to_string()))?;

        // Mirror the controlling terminal's window size onto the PTY, or default 80×24.
        let ws = get_winsize(libc::STDIN_FILENO);
        // SAFETY: master is a valid PTY master fd; TIOCSWINSZ is a write ioctl.
        unsafe {
            libc::ioctl(master.as_raw_fd(), libc::TIOCSWINSZ, &ws);
        }

        // SAFETY: fork is async-signal-safe; no threads exist at this point in
        // the single-threaded call path.
        let fork_result = unsafe { fork() }.map_err(|e| Error::ForkFailed(e.to_string()))?;

        match fork_result {
            ForkResult::Parent { child } => {
                drop(slave);
                Ok(PtySpawner {
                    master,
                    child_pid: child,
                })
            }
            ForkResult::Child => {
                drop(master);
                let slave_fd = slave.into_raw_fd();
                // login_tty(3): setsid, make slave the ctty, dup2 to stdio, close slave.
                // SAFETY: child is single-threaded immediately after fork.
                if unsafe { libc::login_tty(slave_fd) } != 0 {
                    unsafe { libc::_exit(127) };
                }
                // NOTE: no setenv/unsetenv here. The child's environment was
                // built in the parent (build_child_env) and is passed to execvpe
                // below, because neither call is async-signal-safe post-fork.
                // Build full argv: [cmd, args...].
                let mut argv: Vec<&CStr> = Vec::with_capacity(args.len() + 1);
                argv.push(cmd);
                argv.extend(args.iter().map(CString::as_c_str));
                // execvp replaces the process image; it only returns on error.
                let _ = execvpe(cmd, &argv, &child_env);
                unsafe { libc::_exit(127) };
            }
        }
    }

    /// Forward SIGWINCH to the child PTY, relay I/O between the master fd and
    /// stdin/stdout, wait for the child to exit, and return its exit code.
    pub fn relay(&self) -> Result<i32> {
        // Install SIGWINCH handler — sigwinch_handler only touches SIGWINCH_RECEIVED,
        // which is async-signal-safe.
        unsafe {
            signal(Signal::SIGWINCH, SigHandler::Handler(sigwinch_handler))
                .map_err(|e| Error::SignalHandlerFailed(format!("SIGWINCH: {e}")))?;
        }

        // Install SIGINT handler — sigint_handler only touches SIGINT_RECEIVED,
        // which is async-signal-safe. This ensures Ctrl-C is forwarded to the child.
        unsafe {
            signal(Signal::SIGINT, SigHandler::Handler(sigint_handler))
                .map_err(|e| Error::SignalHandlerFailed(format!("SIGINT: {e}")))?;
        }

        let master_fd = self.master.as_raw_fd();
        let mut buf = [0u8; 4096];
        let mut stdin_open = true;

        'relay: loop {
            // Forward SIGINT to child if received.
            if SIGINT_RECEIVED.swap(false, Ordering::Relaxed) {
                // SAFETY: kill is async-signal-safe; child_pid is valid.
                unsafe {
                    libc::kill(self.child_pid.as_raw(), libc::SIGINT);
                }
            }

            // Apply any pending window-size change to the master PTY.
            if SIGWINCH_RECEIVED.swap(false, Ordering::Relaxed) {
                let ws = get_winsize(libc::STDIN_FILENO);
                // SAFETY: master_fd is a valid PTY master fd; TIOCSWINSZ is a write ioctl.
                unsafe {
                    libc::ioctl(master_fd, libc::TIOCSWINSZ, &ws);
                }
            }

            let mut fds = [
                libc::pollfd {
                    fd: master_fd,
                    events: libc::POLLIN,
                    revents: 0,
                },
                libc::pollfd {
                    fd: libc::STDIN_FILENO,
                    events: if stdin_open { libc::POLLIN } else { 0 },
                    revents: 0,
                },
            ];

            // 100 ms timeout so SIGWINCH is handled promptly even if poll is not interrupted.
            let ret = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, 100) };

            if ret < 0 {
                if nix::errno::Errno::last() == nix::errno::Errno::EINTR {
                    continue;
                }
                break 'relay;
            }

            // Drain PTY master output → caller's stdout.
            let master_rev = fds[0].revents;
            if master_rev & libc::POLLIN != 0 {
                let n = unsafe {
                    libc::read(master_fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len())
                };
                if n < 0 {
                    if nix::errno::Errno::last() == nix::errno::Errno::EINTR {
                        continue;
                    }
                    break 'relay; // EIO when child has closed the slave side
                }
                if n == 0 {
                    break 'relay;
                }
                let mut off = 0usize;
                let n = n as usize;
                while off < n {
                    let w = unsafe {
                        libc::write(
                            libc::STDOUT_FILENO,
                            buf[off..].as_ptr() as *const libc::c_void,
                            n - off,
                        )
                    };
                    if w <= 0 {
                        break 'relay;
                    }
                    off += w as usize;
                }
            }
            if master_rev & (libc::POLLHUP | libc::POLLERR) != 0 {
                break 'relay;
            }

            // Forward caller's stdin → PTY master (child input).
            if stdin_open {
                let stdin_rev = fds[1].revents;
                if stdin_rev & libc::POLLIN != 0 {
                    let n = unsafe {
                        libc::read(
                            libc::STDIN_FILENO,
                            buf.as_mut_ptr() as *mut libc::c_void,
                            buf.len(),
                        )
                    };
                    if n <= 0 {
                        stdin_open = false;
                    } else {
                        let mut off = 0usize;
                        let n = n as usize;
                        while off < n {
                            let w = unsafe {
                                libc::write(
                                    master_fd,
                                    buf[off..].as_ptr() as *const libc::c_void,
                                    n - off,
                                )
                            };
                            if w <= 0 {
                                break 'relay;
                            }
                            off += w as usize;
                        }
                    }
                }
                if stdin_rev & libc::POLLHUP != 0 {
                    stdin_open = false;
                }
            }
        }

        // Restore default SIGWINCH and SIGINT handling.
        unsafe {
            let _ = signal(Signal::SIGWINCH, SigHandler::SigDfl);
            let _ = signal(Signal::SIGINT, SigHandler::SigDfl);
        }

        // Wait for child exit and surface the exit code.
        loop {
            match waitpid(self.child_pid, None) {
                Ok(WaitStatus::Exited(_, code)) => return Ok(code),
                Ok(WaitStatus::Signaled(_, sig, _)) => return Ok(128 + sig as i32),
                Ok(_) => continue,
                Err(nix::errno::Errno::EINTR) => continue,
                Err(e) => return Err(Error::WaitpidFailed(e.to_string())),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nix::sys::wait::{waitpid, WaitStatus};

    #[test]
    fn spawn_bin_true_exits_zero() {
        // Resolve `true` via PATH: a hardcoded `/bin/true` breaks on non-FHS
        // systems (e.g. NixOS, where /bin/true does not exist).
        let true_path = which::which("true").expect("'true' should be resolvable on PATH");
        let cmd = CString::new(true_path.as_os_str().as_encoded_bytes()).unwrap();
        let spawner = PtySpawner::spawn(&cmd, &[]).expect("PtySpawner::spawn should succeed");

        let status = waitpid(spawner.child_pid, None).expect("waitpid should succeed");
        match status {
            WaitStatus::Exited(_, code) => assert_eq!(code, 0, "child exited non-zero"),
            other => panic!("unexpected wait status: {other:?}"),
        }
    }

    #[test]
    fn master_fd_carries_child_stdout() {
        let cmd = CString::new("echo").unwrap();
        let args = vec![CString::new("hello").unwrap()];
        let spawner = PtySpawner::spawn(&cmd, &args).expect("spawn should succeed");

        let master_fd = spawner.master.as_raw_fd();
        let mut output = Vec::new();
        let mut buf = [0u8; 256];

        loop {
            let n =
                unsafe { libc::read(master_fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
            if n <= 0 {
                break;
            }
            output.extend_from_slice(&buf[..n as usize]);
        }

        let _ = waitpid(spawner.child_pid, None);

        // PTY translates \n → \r\n; verify the text is present.
        let text = String::from_utf8_lossy(&output);
        assert!(
            text.contains("hello"),
            "expected 'hello' in PTY output, got: {text:?}"
        );
    }

    #[test]
    fn relay_echo_exits_zero_and_produces_output() {
        let cmd = CString::new("echo").unwrap();
        let args = vec![CString::new("relay-test").unwrap()];
        let spawner = PtySpawner::spawn(&cmd, &args).expect("spawn should succeed");
        let code = spawner.relay().expect("relay should succeed");
        assert_eq!(code, 0, "echo should exit with code 0");
    }

    #[test]
    fn relay_surfaces_nonzero_exit_code() {
        let cmd = CString::new("/bin/sh").unwrap();
        let args = vec![
            CString::new("-c").unwrap(),
            CString::new("exit 42").unwrap(),
        ];
        let spawner = PtySpawner::spawn(&cmd, &args).expect("spawn should succeed");
        let code = spawner.relay().expect("relay should succeed");
        assert_eq!(code, 42, "exit code should be 42");
    }

    // ── Child environment scrub (claudepr-26e7a0b6) ──────────────────────────
    //
    // These assert the transcript-persistence and billing invariants at the
    // env-construction level. They are pure: `scrub_env` takes its source
    // environment as an argument, so nothing here mutates the process
    // environment or races other tests.

    fn env_of(entries: &[(&str, &str)]) -> Vec<String> {
        scrub_env(entries.iter().map(|(k, v)| (*k, *v)))
            .into_iter()
            .map(|c| c.into_string().expect("entries are valid UTF-8 here"))
            .collect()
    }

    #[test]
    fn scrub_env_drops_child_session_marker() {
        // The bug: inherited, this marker makes claude 2.1.263 refuse to
        // persist the transcript, so claude-print has nothing to read.
        let env = env_of(&[("CLAUDE_CODE_CHILD_SESSION", "1"), ("PATH", "/usr/bin")]);
        assert!(
            !env.iter()
                .any(|e| e.starts_with("CLAUDE_CODE_CHILD_SESSION=")),
            "CLAUDE_CODE_CHILD_SESSION must not reach the child: {env:?}"
        );
        assert!(
            env.iter().any(|e| e == "PATH=/usr/bin"),
            "unrelated variables must be preserved: {env:?}"
        );
    }

    #[test]
    fn scrub_env_drops_all_session_markers() {
        let env = env_of(&[
            ("CLAUDE_CODE_SESSION_ID", "abc"),
            ("CLAUDECODE", "1"),
            ("CLAUDE_CODE_CHILD_SESSION", "1"),
            ("CLAUDE_CODE_SKIP_PROMPT_HISTORY", "1"),
        ]);
        for marker in SCRUBBED_ENV {
            assert!(
                !env.iter().any(|e| e.starts_with(&format!("{marker}="))),
                "{marker} must be scrubbed: {env:?}"
            );
        }
    }

    #[test]
    fn scrub_env_forces_persistence_and_cli_entrypoint() {
        let env = env_of(&[("PATH", "/usr/bin")]);
        assert!(
            env.iter()
                .any(|e| e == "CLAUDE_CODE_FORCE_SESSION_PERSISTENCE=1"),
            "persistence must be forced on: {env:?}"
        );
        assert!(
            env.iter().any(|e| e == "CLAUDE_CODE_ENTRYPOINT=cli"),
            "billing invariant: entrypoint must be cli: {env:?}"
        );
    }

    #[test]
    fn scrub_env_overrides_inherited_sdk_entrypoint() {
        // Propagating an inherited sdk-cli would bill the metered SDK pool
        // instead of the subscription — the exact failure claude-print exists
        // to prevent. The forced value must win, and must not be duplicated.
        let env = env_of(&[
            ("CLAUDE_CODE_ENTRYPOINT", "sdk-cli"),
            ("CLAUDE_CODE_FORCE_SESSION_PERSISTENCE", "0"),
        ]);
        assert!(
            !env.iter().any(|e| e == "CLAUDE_CODE_ENTRYPOINT=sdk-cli"),
            "inherited sdk-cli must not survive: {env:?}"
        );
        assert_eq!(
            env.iter()
                .filter(|e| e.starts_with("CLAUDE_CODE_ENTRYPOINT="))
                .count(),
            1,
            "exactly one entrypoint entry: {env:?}"
        );
        assert_eq!(
            env.iter()
                .filter(|e| e.starts_with("CLAUDE_CODE_FORCE_SESSION_PERSISTENCE="))
                .count(),
            1,
            "exactly one persistence entry: {env:?}"
        );
        assert!(env.iter().any(|e| e == "CLAUDE_CODE_ENTRYPOINT=cli"));
        assert!(env
            .iter()
            .any(|e| e == "CLAUDE_CODE_FORCE_SESSION_PERSISTENCE=1"));
    }

    #[test]
    fn build_child_env_scrubs_the_real_environment() {
        // Guards the wiring: the pure core is reached from the real env.
        let env = build_child_env();
        assert!(env
            .iter()
            .any(|e| e.to_bytes() == b"CLAUDE_CODE_ENTRYPOINT=cli"));
        assert!(!env
            .iter()
            .any(|e| e.to_bytes().starts_with(b"CLAUDE_CODE_CHILD_SESSION=")));
    }
}
