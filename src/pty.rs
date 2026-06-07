use nix::pty::{openpty, OpenptyResult};
use nix::unistd::{execvp, fork, ForkResult, Pid};
use std::ffi::{CStr, CString};
use std::os::unix::io::{AsRawFd, IntoRawFd, OwnedFd};

use crate::error::{Error, Result};

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
            .map_err(|e| Error::Internal(anyhow::anyhow!("openpty failed: {e}")))?;

        // Mirror the controlling terminal's window size onto the PTY, or default 80×24.
        let ws = get_winsize(libc::STDIN_FILENO);
        // SAFETY: master is a valid PTY master fd; TIOCSWINSZ is a write ioctl.
        unsafe {
            libc::ioctl(master.as_raw_fd(), libc::TIOCSWINSZ, &ws);
        }

        // SAFETY: fork is async-signal-safe; no threads exist at this point in
        // the single-threaded call path.
        let fork_result = unsafe { fork() }
            .map_err(|e| Error::Internal(anyhow::anyhow!("fork failed: {e}")))?;

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
}
