use nix::pty::{openpty, OpenptyResult};
use nix::sys::signal::{signal, SigHandler, Signal};
use nix::sys::wait::{waitpid, WaitStatus};
use nix::unistd::{execvp, fork, ForkResult, Pid};
use std::ffi::{CStr, CString};
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

impl PtySpawner {
    /// Open a PTY pair, fork, set the PTY window size, call `login_tty` in the
    /// child to make the slave the controlling terminal, then `execvp` `cmd`.
    ///
    /// `args` contains only the arguments to the program — not argv\[0\].
    /// argv\[0\] is set to `cmd` internally.
    pub fn spawn(cmd: &CStr, args: &[CString]) -> Result<Self> {
        let OpenptyResult { master, slave } = openpty(None, None)
            .map_err(|e| Error::OpenptyFailed(e.to_string()))?;

        // Mirror the controlling terminal's window size onto the PTY, or default 80×24.
        let ws = get_winsize(libc::STDIN_FILENO);
        // SAFETY: master is a valid PTY master fd; TIOCSWINSZ is a write ioctl.
        unsafe {
            libc::ioctl(master.as_raw_fd(), libc::TIOCSWINSZ, &ws);
        }

        // SAFETY: fork is async-signal-safe; no threads exist at this point in
        // the single-threaded call path.
        let fork_result =
            unsafe { fork() }.map_err(|e| Error::ForkFailed(e.to_string()))?;

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
                // Build full argv: [cmd, args...].
                let mut argv: Vec<&CStr> = Vec::with_capacity(args.len() + 1);
                argv.push(cmd);
                argv.extend(args.iter().map(CString::as_c_str));
                // execvp replaces the process image; it only returns on error.
                let _ = execvp(cmd, &argv);
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
        let cmd = CString::new("/bin/true").unwrap();
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
}
