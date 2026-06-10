use crate::error::{Error, Result};
use std::os::unix::io::RawFd;

/// Outcome returned by [`EventLoop::run`].
#[derive(Debug)]
pub enum ExitReason {
    /// EIO (or read-0 / POLLHUP) on master_fd: child closed the PTY slave.
    ChildExited,
    /// stop_fifo became readable; contains the raw bytes read from it.
    FifoPayload(Vec<u8>),
    /// self-pipe was written (SIGINT / SIGTERM signal path).
    Interrupted,
}

/// Single-threaded poll(2) event loop over a PTY master fd.
///
/// Initial fd set: master_fd + self_pipe_read (2 fds).
/// At PROMPT_INJECTED, call [`add_fifo_fd`] to register the stop FIFO as a
/// third fd in the same poll call.
pub struct EventLoop {
    /// [master_fd, self_pipe_read] initially; FIFO pushed at PROMPT_INJECTED.
    fds: Vec<libc::pollfd>,
    buf: [u8; 4096],
}

const MASTER_IDX: usize = 0;
const SELF_PIPE_IDX: usize = 1;
const FIFO_IDX: usize = 2;

impl EventLoop {
    pub fn new(master_fd: RawFd, self_pipe_read: RawFd) -> Self {
        let fds = vec![
            libc::pollfd {
                fd: master_fd,
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: self_pipe_read,
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        EventLoop {
            fds,
            buf: [0u8; 4096],
        }
    }

    /// Register the stop FIFO read-end.  Must be called before the bracketed
    /// paste is written (PROMPT_INJECTED transition) so Stop cannot fire while
    /// the read-end is still unopened.
    pub fn add_fifo_fd(&mut self, fd: RawFd) {
        debug_assert!(
            self.fds.len() == FIFO_IDX,
            "add_fifo_fd called more than once"
        );
        self.fds.push(libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        });
    }

    /// Run the poll loop.  `on_output` is called with every chunk read from
    /// the PTY master.  Returns when the child exits, the FIFO fires, or the
    /// self-pipe is written.
    pub fn run<F>(&mut self, mut on_output: F) -> Result<ExitReason>
    where
        F: FnMut(&[u8]),
    {
        loop {
            for pfd in &mut self.fds {
                pfd.revents = 0;
            }

            let ret =
                unsafe { libc::poll(self.fds.as_mut_ptr(), self.fds.len() as libc::nfds_t, -1) };

            if ret < 0 {
                let errno = nix::errno::Errno::last();
                if errno == nix::errno::Errno::EINTR {
                    continue;
                }
                return Err(Error::Internal(anyhow::anyhow!("poll failed: {errno}")));
            }

            // Self-pipe: signal arrived; highest priority.
            if self.fds[SELF_PIPE_IDX].revents & libc::POLLIN != 0 {
                return Ok(ExitReason::Interrupted);
            }

            // Stop FIFO readable (only after add_fifo_fd).
            if self.fds.len() > FIFO_IDX && self.fds[FIFO_IDX].revents & libc::POLLIN != 0 {
                let mut payload: Vec<u8> = Vec::new();
                loop {
                    let n = unsafe {
                        libc::read(
                            self.fds[FIFO_IDX].fd,
                            self.buf.as_mut_ptr() as *mut libc::c_void,
                            self.buf.len(),
                        )
                    };
                    if n <= 0 {
                        break;
                    }
                    payload.extend_from_slice(&self.buf[..n as usize]);
                }
                return Ok(ExitReason::FifoPayload(payload));
            }

            // PTY master output.
            let master_revents = self.fds[MASTER_IDX].revents;
            if master_revents & libc::POLLIN != 0 {
                let n = unsafe {
                    libc::read(
                        self.fds[MASTER_IDX].fd,
                        self.buf.as_mut_ptr() as *mut libc::c_void,
                        self.buf.len(),
                    )
                };
                if n < 0 {
                    let errno = nix::errno::Errno::last();
                    if errno == nix::errno::Errno::EINTR {
                        continue;
                    }
                    // EIO: slave side closed (child exited).
                    return Ok(ExitReason::ChildExited);
                }
                if n == 0 {
                    return Ok(ExitReason::ChildExited);
                }
                on_output(&self.buf[..n as usize]);
            }

            // POLLHUP/POLLERR with no POLLIN → child exited, no data pending.
            if master_revents & (libc::POLLHUP | libc::POLLERR) != 0
                && master_revents & libc::POLLIN == 0
            {
                return Ok(ExitReason::ChildExited);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pty::PtySpawner;
    use nix::sys::wait::waitpid;
    use std::ffi::CString;
    use std::os::unix::io::AsRawFd;

    fn make_self_pipe() -> (std::os::unix::io::OwnedFd, std::os::unix::io::OwnedFd) {
        nix::unistd::pipe().expect("pipe() failed")
    }

    #[test]
    fn test_event_loop_reads_pty_output() {
        let (pipe_r, _pipe_w) = make_self_pipe();

        let cmd = CString::new("echo").unwrap();
        let args = vec![CString::new("hello-event-loop").unwrap()];
        let spawner = PtySpawner::spawn(&cmd, &args).expect("PtySpawner::spawn");

        let mut el = EventLoop::new(spawner.master.as_raw_fd(), pipe_r.as_raw_fd());

        let mut output = Vec::<u8>::new();
        let reason = el.run(|chunk| output.extend_from_slice(chunk)).unwrap();

        assert!(
            matches!(reason, ExitReason::ChildExited),
            "expected ChildExited, got {reason:?}"
        );

        let text = String::from_utf8_lossy(&output);
        assert!(
            text.contains("hello-event-loop"),
            "expected 'hello-event-loop' in PTY output, got: {text:?}"
        );

        let _ = waitpid(spawner.child_pid, None);
    }

    #[test]
    fn test_event_loop_detects_child_exit() {
        let (pipe_r, _pipe_w) = make_self_pipe();

        let cmd = CString::new("/bin/true").unwrap();
        let spawner = PtySpawner::spawn(&cmd, &[]).expect("PtySpawner::spawn");

        let mut el = EventLoop::new(spawner.master.as_raw_fd(), pipe_r.as_raw_fd());

        let reason = el.run(|_| {}).unwrap();

        assert!(
            matches!(reason, ExitReason::ChildExited),
            "expected ChildExited, got {reason:?}"
        );

        let _ = waitpid(spawner.child_pid, None);
    }
}
