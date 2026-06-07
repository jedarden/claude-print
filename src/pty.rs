use nix::pty::{openpty, OpenptyResult};
use nix::unistd::{fork, ForkResult, Pid};
use std::os::unix::io::{IntoRawFd, OwnedFd};

use crate::error::{Error, Result};

pub struct PtySpawner {
    pub master: OwnedFd,
    pub child_pid: Pid,
}

impl PtySpawner {
    /// Open a PTY pair, fork, and call `login_tty` on the child so it becomes
    /// the controlling terminal session leader. The child exits immediately
    /// after login_tty. No exec is performed — that is deferred to a later phase.
    pub fn spawn() -> Result<Self> {
        let OpenptyResult { master, slave } = openpty(None, None)
            .map_err(|e| Error::Internal(anyhow::anyhow!("openpty failed: {e}")))?;

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
                // login_tty(3): setsid, make slave the ctty, dup to stdio, close slave.
                let slave_fd = slave.into_raw_fd();
                // SAFETY: in child immediately after fork, single-threaded.
                unsafe { libc::login_tty(slave_fd) };
                std::process::exit(0);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nix::sys::wait::{waitpid, WaitStatus};

    #[test]
    fn fork_and_login_tty_does_not_panic() {
        let spawner = PtySpawner::spawn().expect("PtySpawner::spawn should succeed");

        let status = waitpid(spawner.child_pid, None).expect("waitpid should succeed");
        match status {
            WaitStatus::Exited(_, code) => assert_eq!(code, 0, "child exited non-zero"),
            other => panic!("unexpected wait status: {other:?}"),
        }
    }
}
