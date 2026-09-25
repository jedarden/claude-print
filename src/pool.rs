// Warm PTY pool implementation (ADR-005)
//
// This module implements a pool of pre-warmed `claude` PTY processes that
// have completed trust-dismiss and idle-settle, but never past prompt injection.
// Workers are handed out to clients over a Unix domain socket.
//
// ## IPC Protocol (Client → Server)
//
// ### Request: Acquire Worker
// ```json
// {
//   "type": "acquire",
//   "timeout_secs": 60
// }
// ```
//
// ### Request: Release Worker
// ```json
// {
//   "type": "release",
//   "worker_id": "uuid"
// }
// ```
//
// ## IPC Protocol (Server → Client)
//
// ### Response: Worker Assigned
// ```json
// {
//   "type": "worker_assigned",
//   "worker_id": "uuid",
//   "message": "Worker ready",
//   "stop_fifo": "/tmp/claude-print-<daemon-pid>-xxxx/stop.fifo",
//   "pid": 12345,
//   "cwd": "/working/dir"
// }
// ```
// Note: The actual PTY master fd is sent as ancillary data (SCM_RIGHTS) after
// the JSON frame. `stop_fifo` is the daemon-side Stop FIFO the worker's hook
// writes into — the acquiring client reads the payload from it, exactly as the
// ordinary session path reads its own; `pid` feeds the client-side watchdog;
// `cwd` is the working directory the worker was spawned in (the daemon's), which
// is where the child writes its `~/.claude/projects/<cwd-slug>/` transcripts and
// therefore the directory the client's stream-json discovery must watch. All
// three are optional on the wire (older daemons omit them); a client that
// receives a `worker_assigned` without them treats the acquire as a protocol
// failure — it cannot drive the session without them.
//
// ## Client fallback contract (ADR-005)
//
// `--pool-socket` is additive latency work, never a new failure surface for the
// ordinary path. Failures are classified in [`AcquireFailure`]:
//
//   * **Absent / unreachable** (missing socket file, refused connect, connect
//     timeout, permission) and **correctly-answered but unavailable** (pool
//     full, shutting down, internal error) → the caller falls back to the
//     stateless session path, with a diagnostic when `--verbose` is on.
//   * **Protocol failure** (garbage response, malformed frame, incomplete
//     worker assignment, failed fd transfer) → a hard error. The pool was
//     reachable and answered; falling back would silently mask a broken
//     daemon behind full-price stateless sessions. Every protocol stage is
//     bounded by the caller's acquire deadline, so a hung daemon cannot
//     stall the client past it.
//
// The invocation path implements this contract in [`acquire_for_invocation`]
// — the entry point main() calls when `--pool-socket` is set — which returns
// an [`InvocationAcquisition`] classification the caller owes an outcome to.
//
// ### Response: Error
// ```json
// {
//   "type": "error",
//   "error": "Pool full",
//   "code": "pool_full"
// }
// ```

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::os::fd::{FromRawFd, OwnedFd};
use std::os::unix::io::{AsRawFd, IntoRawFd, RawFd};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};
use uuid::Uuid;

/// Default socket path for the pool daemon
pub const DEFAULT_SOCKET_PATH: &str = "/tmp/claude-print-pool.sock";

/// Maximum time a client will wait for a worker acquisition. Also the cap on
/// the client's own acquire budget — see the client section below.
pub const DEFAULT_ACQUIRE_TIMEOUT_SECS: u64 = 60;

/// Maximum time to wait for a worker to complete its warmup phase
const WARMUP_TIMEOUT_SECS: u64 = 120;

/// Hard cap on `--pool-size`. Each worker is a full `claude` PTY process, so a
/// typo like `--pool-size 100000` must fail argv validation with exit 2 rather
/// than fork-bombing the host.
pub const MAX_POOL_SIZE: usize = 256;

/// poll(2) timeout for the serve accept loop, in milliseconds. Bounds how long
/// a shutdown request can go unnoticed even without the signal self-pipe, and
/// doubles as the maintain() tick (top-up respawn cadence).
const ACCEPT_POLL_TIMEOUT_MS: i32 = 250;

/// How long a worker is given to exit on SIGTERM before SIGKILL. Bounds the
/// whole daemon shutdown: `shutdown_all` destroys workers serially, so every
/// worker pays at most this constant (a SIGTERM-compliant child pays none of
/// it — teardown observes its exit on the first poll).
const DESTROY_GRACE: Duration = Duration::from_secs(2);

/// Cap on one client request frame. Legitimate acquire/release JSON is a few
/// hundred bytes, so this leaves three orders of magnitude of headroom while
/// keeping a hostile 4-byte length prefix from pinning gigabytes of daemon
/// memory (the wire length is an unbounded u32).
const MAX_REQUEST_BYTES: usize = 64 * 1024;

/// Validate a `--pool-size` argument before the daemon starts. `0` would
/// maintain an empty pool that serves nothing, and anything past
/// [`MAX_POOL_SIZE`] forks that many full `claude` PTY processes — both are
/// argv-validation failures the caller must reject with exit 2 rather than
/// enter the server loop.
pub fn validate_pool_size(size: usize) -> Result<(), String> {
    if size == 0 {
        return Err(
            "--pool-size must be at least 1 (a pool of 0 workers would serve nothing)".to_string(),
        );
    }
    if size > MAX_POOL_SIZE {
        return Err(format!(
            "--pool-size {size} exceeds the maximum of {MAX_POOL_SIZE} (each worker is a full claude PTY process)"
        ));
    }
    Ok(())
}

/// Serve-mode shutdown signal, flipped by the SIGINT/SIGTERM handler. A signal
/// handler may only touch statics (it cannot lock the manager's Mutex), so the
/// accept loop relays this into `manager.shutdown()` on its next tick — within
/// [`ACCEPT_POLL_TIMEOUT_MS`] of the signal arriving.
static SERVE_SIGNALED: AtomicBool = AtomicBool::new(false);

extern "C" fn serve_signal_handler(_sig: libc::c_int) {
    SERVE_SIGNALED.store(true, Ordering::SeqCst);
}

/// Install SIGINT/SIGTERM handlers for serve mode so a stopped daemon tears
/// down cleanly: the accept loop notices the relayed flag, `shutdown_all`
/// destroys every worker, and the socket file is removed. Only serve mode
/// installs these; the ordinary session path keeps default signal dispositions.
pub fn install_serve_signal_handlers() {
    use nix::sys::signal::{signal, SigHandler, Signal};
    unsafe {
        let _ = signal(Signal::SIGINT, SigHandler::Handler(serve_signal_handler));
        let _ = signal(Signal::SIGTERM, SigHandler::Handler(serve_signal_handler));
    }
}

/// Quiet window a worker must see after trust-dismiss before it counts as
/// Ready (ADR-005 "idle-settle"). Feeds the warmup sequencer's idle gap so
/// the pool path shares the ordinary session path's settle constant and both
/// paths agree on what "settled" means.
const WARMUP_SETTLE_MS: u64 = crate::startup::DEFAULT_POST_DISMISS_IDLE_MS;

/// Message a warmup thread sends back to the pool manager when a warmup
/// attempt reaches a terminal outcome. Every warmup attempt sends exactly
/// one notification, so `maintain()` can mark the worker Ready or
/// destroy-and-respawn it — a warmup that terminates silently would otherwise
/// pin its slot as `Warming` forever and permanently shrink the pool.
#[derive(Debug)]
enum WarmupNotification {
    Ready(String),
    Failed(String),
}

/// Client request types sent to the pool daemon
#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PoolRequest {
    /// Acquire a warmed worker from the pool
    Acquire {
        /// Maximum time to wait for a worker (default: 60s)
        #[serde(default = "default_acquire_timeout")]
        timeout_secs: u64,
    },
    /// Release a worker back to the pool (destroys it)
    Release {
        /// Worker ID to release
        worker_id: String,
    },
}

fn default_acquire_timeout() -> u64 {
    DEFAULT_ACQUIRE_TIMEOUT_SECS
}

/// Server response types sent to the client
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PoolResponse {
    /// Worker successfully assigned (PTY fd sent as SCM_RIGHTS after this
    /// frame).
    ///
    /// `stop_fifo`, `pid`, and `cwd` carry everything the client needs to drive
    /// the session end to end: the daemon-side Stop FIFO to read the payload
    /// from, the worker process for watchdog signalling, and the working
    /// directory the child writes its transcripts under (the stream-json
    /// discovery directory). All three default to empty on the wire so an
    /// older client can still parse the frame; a *current* client treats any
    /// empty one as a protocol failure (see [`AcquiredWorker::validate_parts`]).
    WorkerAssigned {
        worker_id: String,
        message: String,
        #[serde(default)]
        stop_fifo: String,
        #[serde(default)]
        pid: u32,
        #[serde(default)]
        cwd: String,
    },
    /// Error response
    Error { error: String, code: ErrorCode },
}

impl From<serde_json::Error> for PoolResponse {
    fn from(err: serde_json::Error) -> Self {
        PoolResponse::Error {
            error: format!("JSON error: {}", err),
            code: ErrorCode::InternalError,
        }
    }
}

/// Error codes for pool responses
#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    /// Pool is at capacity, no workers available
    PoolFull,
    /// Acquire timeout expired without getting a worker
    AcquireTimeout,
    /// Invalid worker ID
    InvalidWorkerId,
    /// Internal pool error
    InternalError,
    /// Pool is shutting down
    ShuttingDown,
}

/// Worker state in the pool lifecycle
#[derive(Debug, Clone, PartialEq)]
pub enum WorkerState {
    /// Worker is being spawned and warmed up
    Warming,
    /// Worker is ready and idle in the pool
    Ready,
    /// Worker is assigned to a client
    InUse,
    /// Worker is being replaced after use
    Replacing,
}

/// Warmup phase tracking for pool daemon internal state machine
#[derive(Debug, Clone, PartialEq)]
enum WarmupPhase {
    Starting,
    Settling,
    Ready,
    Failed,
}

/// A pooled worker - a pre-warmed Claude Code PTY process
pub struct PoolWorker {
    /// Unique worker ID
    pub id: String,
    /// Current state
    pub state: WorkerState,
    /// PTY master file descriptor
    pub master_fd: RawFd,
    /// Child process PID
    pub child_pid: nix::unistd::Pid,
    /// When the worker entered its current state
    pub state_since: Instant,
    /// Hook installer for this worker (cleanup on drop)
    pub hook_installer: Option<crate::hook::HookInstaller>,
}

/// Pool manager - maintains N warmed workers
pub struct PoolManager {
    /// Map of worker_id -> worker
    workers: HashMap<String, PoolWorker>,
    /// Target pool size
    target_size: usize,
    /// Path to claude binary
    claude_bin: std::path::PathBuf,
    /// Verbose logging
    verbose: bool,
    /// Shutdown flag
    shutdown: Arc<AtomicBool>,
    /// Channel for warmup outcome notifications
    warmup_tx: mpsc::Sender<WarmupNotification>,
    /// Channel for warmup outcome notifications (receiver)
    warmup_rx: mpsc::Receiver<WarmupNotification>,
}

impl PoolManager {
    /// Create a new pool manager
    pub fn new(target_size: usize, claude_bin: std::path::PathBuf, verbose: bool) -> Self {
        let (warmup_tx, warmup_rx) = mpsc::channel();

        Self {
            workers: HashMap::new(),
            target_size,
            claude_bin,
            verbose,
            shutdown: Arc::new(AtomicBool::new(false)),
            warmup_tx,
            warmup_rx,
        }
    }

    /// Get the shutdown flag for external signal handlers
    pub fn shutdown_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.shutdown)
    }

    /// Signal shutdown to the pool
    pub fn shutdown(&self) {
        self.shutdown.store(true, Ordering::SeqCst);
    }

    /// Count workers in a given state
    fn count_state(&self, state: WorkerState) -> usize {
        self.workers.values().filter(|w| w.state == state).count()
    }

    /// Get a ready worker if one exists
    pub fn acquire_worker(&mut self) -> Result<String, PoolResponse> {
        if self.shutdown.load(Ordering::SeqCst) {
            return Err(PoolResponse::Error {
                error: "Pool is shutting down".to_string(),
                code: ErrorCode::ShuttingDown,
            });
        }

        // Find a ready worker
        for (id, worker) in self.workers.iter_mut() {
            if worker.state == WorkerState::Ready {
                worker.state = WorkerState::InUse;
                worker.state_since = Instant::now();
                return Ok(id.clone());
            }
        }

        Err(PoolResponse::Error {
            error: "Pool full - no ready workers".to_string(),
            code: ErrorCode::PoolFull,
        })
    }

    /// Mark a worker as released (will be destroyed and replaced)
    pub fn release_worker(&mut self, worker_id: &str) -> Result<(), PoolResponse> {
        if let Some(mut worker) = self.workers.remove(worker_id) {
            worker.state = WorkerState::Replacing;
            // Clean up the worker
            self.destroy_worker(worker);
            Ok(())
        } else {
            Err(PoolResponse::Error {
                error: format!("Invalid worker ID: {}", worker_id),
                code: ErrorCode::InvalidWorkerId,
            })
        }
    }

    /// Signal the worker's whole process group, falling back to the bare pid.
    ///
    /// `PtySpawner`'s `login_tty(3)` makes every worker a session *and* process
    /// group leader (pgid == its own pid), so `killpg` reaches the worker plus
    /// the descendants it has spawned, where a plain `kill(pid)` would strand
    /// those descendants running after the daemon exits. Must only be called
    /// while the direct child is still unreaped: a live or zombie child pins
    /// its pid against recycling, so the fallback can never hit an unrelated
    /// process.
    fn signal_worker_group(pid: nix::unistd::Pid, sig: nix::sys::signal::Signal) {
        use nix::sys::signal::{kill, killpg};
        if killpg(pid, sig).is_err() {
            // No such group: every member is gone, or the child moved itself
            // to another group (nothing in this codebase does). Either way the
            // direct child is still our unreaped child, so signalling its pid
            // is safe.
            let _ = kill(pid, sig);
        }
    }

    /// SIGKILL whatever remains of the worker's process group. Unlike
    /// [`Self::signal_worker_group`] this is safe to call *after* the direct
    /// child has been reaped: a pgid cannot be recycled while its group has
    /// any member, so the signal-0 probe and the kill bracketed around it can
    /// only ever hit a member of this worker's own group. There is
    /// deliberately no bare-pid fallback here — once the child is reaped its
    /// pid may already belong to someone else.
    fn kill_surviving_group(pid: nix::unistd::Pid) {
        use nix::sys::signal::{killpg, Signal};
        if killpg(pid, None).is_ok() {
            let _ = killpg(pid, Signal::SIGKILL);
        }
    }

    /// Destroy a worker and clean up its resources.
    ///
    /// Teardown is: close the pty master, SIGTERM the worker's process group,
    /// hold a grace period while observing the direct child with waitpid,
    /// SIGKILL the group if anything survived, and return only once the direct
    /// child's exit has been observed and reaped — so shutdown never leaks a
    /// worker, never leaves a zombie behind, and never signals a pid that has
    /// already been reaped.
    fn destroy_worker(&self, worker: PoolWorker) {
        if self.verbose {
            eprintln!(
                "[claude-print pool] Destroying worker {} (pid {})",
                worker.id, worker.child_pid
            );
        }

        // Close the PTY master first. The kernel hangs the child's session up
        // (its controlling terminal dies with the master), which is the
        // PTY-level teardown every reader of the slave sees before any signal
        // is even sent.
        let _ = nix::unistd::close(worker.master_fd);

        let pid = worker.child_pid;

        // SIGTERM the worker's whole group — the worker itself plus any
        // descendants it spawned.
        Self::signal_worker_group(pid, nix::sys::signal::Signal::SIGTERM);

        // Grace period: poll the direct child without blocking so a prompt
        // exit ends teardown immediately, and *observe* the exit — waitpid
        // reaps the zombie, which a teardown that only sent signals would
        // leave behind.
        let start = Instant::now();
        let mut observed = false;
        while start.elapsed() < DESTROY_GRACE {
            match nix::sys::wait::waitpid(pid, Some(nix::sys::wait::WaitPidFlag::WNOHANG)) {
                Ok(nix::sys::wait::WaitStatus::Exited(..))
                | Ok(nix::sys::wait::WaitStatus::Signaled(..)) => {
                    observed = true;
                    break;
                }
                // Reaped out from under us (nothing in the daemon does this
                // today) or never our child: nothing left to wait for.
                Err(nix::errno::Errno::ECHILD) => {
                    observed = true;
                    break;
                }
                Err(nix::errno::Errno::EINTR) => continue,
                // Unknown waitpid failure: stop waiting; the escalation below
                // still ends in a blocking waitpid rather than a silent drop.
                Err(_) => break,
                // Still alive (or stopped/continued): keep polling.
                _ => std::thread::sleep(Duration::from_millis(25)),
            }
        }

        if !observed {
            // The grace expired with the child still running: SIGKILL the
            // group — untrappable, so no worker configuration can outstub
            // teardown — then block on the direct child so its exit is
            // observed before we return. The child is still unreaped here, so
            // its pid (and therefore its pgid) is pinned.
            Self::signal_worker_group(pid, nix::sys::signal::Signal::SIGKILL);
            // A signal-interrupted wait is retried, not abandoned: serve mode
            // keeps SIGINT/SIGTERM armed through teardown, and a second
            // signal landing inside this wait must not skip the reap and
            // strand the child as a zombie for the daemon's remaining life.
            loop {
                match nix::sys::wait::waitpid(pid, None) {
                    Ok(_) => break,
                    Err(nix::errno::Errno::EINTR) => continue,
                    // Gone already, or never ours to wait on.
                    Err(_) => break,
                }
            }
        } else {
            // The direct child's exit was observed and reaped — but any
            // descendants it spawned were orphaned by that exit and are no
            // longer our children to wait on. A group that still exists has a
            // member that survived SIGTERM: SIGKILL it.
            Self::kill_surviving_group(pid);
        }

        // Drop the hook installer to clean up temp dir
        drop(worker.hook_installer);
    }

    /// Maintain the pool at target size
    pub fn maintain(&mut self) -> Result<(), anyhow::Error> {
        // Once shutdown is requested the pool must not respawn anything —
        // every spawn here would race the reaper in `shutdown_all`.
        if self.shutdown.load(Ordering::SeqCst) {
            return Ok(());
        }

        // Process any warmup completions
        self.process_warmup_completions();

        // Count warming and ready workers
        let warming_count = self.count_state(WorkerState::Warming);
        let ready_count = self.count_state(WorkerState::Ready);
        let total_active = warming_count + ready_count;

        if self.verbose {
            eprintln!(
                "[claude-print pool] Pool status: {}/{} ready, {} warming",
                ready_count, self.target_size, warming_count
            );
        }

        // Spawn more workers if below target
        if total_active < self.target_size {
            let to_spawn = self.target_size - total_active;
            for _ in 0..to_spawn {
                if let Err(e) = self.spawn_worker() {
                    eprintln!("[claude-print pool] Failed to spawn worker: {}", e);
                }
            }
        }

        Ok(())
    }

    /// Process warmup terminal-outcome notifications
    fn process_warmup_completions(&mut self) {
        // Drain the channel of all terminal warmup outcomes
        while let Ok(notification) = self.warmup_rx.try_recv() {
            match notification {
                WarmupNotification::Ready(worker_id) => {
                    if let Some(worker) = self.workers.get_mut(&worker_id) {
                        worker.state = WorkerState::Ready;
                        worker.state_since = Instant::now();
                        if self.verbose {
                            eprintln!("[claude-print pool] Worker {} marked as Ready", worker_id);
                        }
                    }
                }
                WarmupNotification::Failed(worker_id) => {
                    // A failed warmup must not pin its slot as Warming forever:
                    // destroy the worker so the maintain() count below sees the
                    // missing slot and respawns a replacement.
                    if let Some(worker) = self.workers.remove(&worker_id) {
                        if self.verbose {
                            eprintln!("[claude-print pool] Retiring failed worker {}", worker_id);
                        }
                        self.destroy_worker(worker);
                    }
                }
            }
        }
    }

    /// Spawn a new worker and add it to the pool
    fn spawn_worker(&mut self) -> Result<(), anyhow::Error> {
        let worker_id = Uuid::new_v4().to_string();

        if self.verbose {
            eprintln!("[claude-print pool] Spawning worker {}", worker_id);
        }

        // Create a new worker via the PTY spawner
        let worker = self.create_worker(&worker_id)?;

        // Extract the master_fd before moving worker into the HashMap
        let master_fd = worker.master_fd;

        self.workers.insert(worker_id.clone(), worker);

        // Warm the worker in the background
        let worker_id_clone = worker_id.clone();
        let shutdown_flag = Arc::clone(&self.shutdown);
        let warmup_tx = self.warmup_tx.clone();
        let verbose = self.verbose;
        std::thread::spawn(move || {
            Self::warm_worker_background(
                worker_id_clone,
                master_fd,
                shutdown_flag,
                warmup_tx,
                verbose,
            );
        });

        Ok(())
    }

    /// Create a new worker (PTY + claude process)
    fn create_worker(&self, worker_id: &str) -> Result<PoolWorker, anyhow::Error> {
        // Set up hook installer
        let hook_installer = crate::hook::HookInstaller::new()?;

        // Resolve claude binary
        let cmd = std::ffi::CString::new(self.claude_bin.to_string_lossy().as_bytes())
            .map_err(|e| anyhow::anyhow!("claude_bin path invalid: {e}"))?;

        // Build child args (just the settings flag for now)
        let mut args: Vec<std::ffi::CString> = Vec::new();
        args.push(
            std::ffi::CString::new(format!(
                "--settings={}",
                hook_installer.settings_path.to_string_lossy()
            ))
            .map_err(|e| anyhow::anyhow!("settings path invalid: {e}"))?,
        );
        args.push(std::ffi::CString::new("--setting-sources=").unwrap()); // No user hooks in pool

        // Spawn the PTY
        let spawner = crate::pty::PtySpawner::spawn(&cmd, &args)?;

        // Transfer ownership of the master fd out of the OwnedFd: `spawner`
        // is dropped at the end of this function, and dropping its OwnedFd
        // would close the master behind the pool's back — every later
        // read/poll on the worker would see EBADF/POLLNVAL. The raw fd is
        // now owned by the PoolWorker and closed by destroy_worker.
        let master_fd = spawner.master.into_raw_fd();

        Ok(PoolWorker {
            id: worker_id.to_string(),
            state: WorkerState::Warming,
            master_fd,
            child_pid: spawner.child_pid,
            state_since: Instant::now(),
            hook_installer: Some(hook_installer),
        })
    }

    /// Background task to warm a worker through trust-dismiss and idle-settle
    ///
    /// This is the core warmup logic per ADR-005: workers are pre-warmed through
    /// trust-dismiss and idle-settle, but never past prompt injection. Every
    /// attempt reaches a terminal outcome and sends exactly one
    /// [`WarmupNotification`] — a warmup that terminated silently would pin its
    /// slot as `Warming` forever and permanently shrink the pool.
    fn warm_worker_background(
        worker_id: String,
        master_fd: RawFd,
        shutdown_flag: Arc<AtomicBool>,
        warmup_tx: mpsc::Sender<WarmupNotification>,
        verbose: bool,
    ) {
        let outcome = Self::warm_worker_to_outcome(worker_id, master_fd, &shutdown_flag, verbose);
        // Exactly one terminal-outcome notification per warmup attempt, sent
        // on every path. A send failure means the manager is gone (daemon
        // exiting), which the reaper side already handles.
        let _ = warmup_tx.send(outcome);
    }

    /// Log a warmup failure and build its terminal [`WarmupNotification`].
    fn fail(worker_id: String, reason: &str) -> WarmupNotification {
        eprintln!(
            "[claude-print pool] Worker {} warmup failed: {}",
            worker_id, reason
        );
        WarmupNotification::Failed(worker_id)
    }

    /// Write one byte to the self-pipe so the blocked poll(2) inside
    /// [`crate::event_loop::EventLoop::run`] returns immediately.
    fn wake_event_loop(pipe_w_raw: RawFd) {
        let byte = [0u8];
        let _ =
            unsafe { libc::write(pipe_w_raw, byte.as_ptr() as *const libc::c_void, byte.len()) };
    }

    /// Drive one warmup attempt to its terminal outcome.
    ///
    /// The phase machine runs inside the event-loop callback: the startup
    /// timers (dismiss keys, idle-settle) only fire there, because `run`
    /// itself blocks until the child exits — a machine placed outside the
    /// callback would never advance while the child is healthy. Terminal
    /// outcomes write the self-pipe so `run` returns promptly instead of
    /// blocking until the child dies. The pipe fds are closed exactly once by
    /// their OwnedFd drops when this function returns.
    fn warm_worker_to_outcome(
        worker_id: String,
        master_fd: RawFd,
        shutdown_flag: &AtomicBool,
        verbose: bool,
    ) -> WarmupNotification {
        if shutdown_flag.load(Ordering::SeqCst) {
            return Self::fail(worker_id, "pool shutdown before warmup started");
        }

        if verbose {
            eprintln!(
                "[claude-print pool] Starting warmup for worker {}",
                worker_id
            );
        }

        // Self-pipe: EventLoop requires one; warmup's terminal outcomes write
        // it to break out of `run` (nobody else writes it — this thread
        // installs no signal handler).
        let (pipe_r, pipe_w) = match nix::unistd::pipe() {
            Ok(p) => p,
            Err(e) => return Self::fail(worker_id, &format!("failed to create self-pipe: {e}")),
        };
        let pipe_r_raw = pipe_r.as_raw_fd();
        let pipe_w_raw = pipe_w.as_raw_fd();

        // Create event loop with the PTY master fd
        let mut event_loop = crate::event_loop::EventLoop::new(master_fd, pipe_r_raw);

        // Empty prompt: warmup drives trust-dismiss and idle-settle only. The
        // idle gap IS the settle window (ADR-005), so it shares the session
        // path's quiet-window constant.
        let mut startup_seq =
            crate::startup::StartupSeq::with_idle_gap(Vec::new(), WARMUP_SETTLE_MS);

        // Create terminal emulator for probe responses (default window size: 220x50)
        let mut terminal_emu = crate::terminal::TerminalEmu::new(220, 50);

        // Warmup timeout tracking — enforced on the 50 ms timer tick inside
        // the callback, so it fires even while a chatty child keeps the PTY
        // busy (the callback runs on every tick, not just on PTY data).
        let warmup_start = Instant::now();
        let warmup_timeout = Duration::from_secs(WARMUP_TIMEOUT_SECS);

        // Warmup phases: Starting → Settling → Ready; any failure → Failed.
        // We never inject a prompt during warmup (that happens when the client
        // acquires the worker).
        let mut warmup_phase = WarmupPhase::Starting;
        let mut failed_reason: Option<String> = None;

        // Handle one StartupAction. Trust-dismissal keys are written to the
        // master; the idle-gap injection payload never is — when the sequencer
        // advances to PromptInjected the payload is the prompt injection,
        // which warmup must not deliver (ADR-005), and its firing IS the
        // settle-complete event. Returns true on any terminal outcome.
        let handle_action = |startup_seq: &mut crate::startup::StartupSeq,
                             warmup_phase: &mut WarmupPhase,
                             failed_reason: &mut Option<String>,
                             action: crate::startup::StartupAction|
         -> bool {
            use crate::startup::{StartupAction, StartupPhase};
            let terminal = match action {
                StartupAction::Write(bytes) => {
                    if *startup_seq.phase() == StartupPhase::PromptInjected {
                        // Idle-settle window completed: the worker is Ready.
                        *warmup_phase = WarmupPhase::Ready;
                        true
                    } else {
                        // Trust-dismissal keys (from feed, or held until the
                        // render burst went quiet) — deliver them.
                        let _ = unsafe {
                            libc::write(
                                master_fd,
                                bytes.as_ptr() as *const libc::c_void,
                                bytes.len(),
                            )
                        };
                        false
                    }
                }
                StartupAction::None => false,
                StartupAction::HardTimeout => {
                    *failed_reason = Some("hard timeout during warmup".to_string());
                    *warmup_phase = WarmupPhase::Failed;
                    true
                }
                StartupAction::Refuse(reason) => {
                    *failed_reason = Some(format!("trust dismissal refused: {reason}"));
                    *warmup_phase = WarmupPhase::Failed;
                    true
                }
            };
            if terminal {
                // Wake the poll loop: run() returns only on its own exit
                // conditions, so without this a terminal outcome reached
                // inside the callback would block until the child exits.
                Self::wake_event_loop(pipe_w_raw);
            }
            terminal
        };

        let reason = event_loop.run(|chunk| {
            if !chunk.is_empty() {
                // Feed chunk to terminal emulator for probe responses
                let responses = terminal_emu.feed(chunk);
                if !responses.is_empty() {
                    let _ = unsafe {
                        libc::write(
                            master_fd,
                            responses.as_ptr() as *const libc::c_void,
                            responses.len(),
                        )
                    };
                }

                // Feed chunk to startup sequencer
                let action = startup_seq.feed(chunk);
                if handle_action(
                    &mut startup_seq,
                    &mut warmup_phase,
                    &mut failed_reason,
                    action,
                ) {
                    return;
                }
            }

            // Check startup timers (runs on every poll wakeup, including the
            // 50 ms tick)
            let action = startup_seq.poll_timers();
            if handle_action(
                &mut startup_seq,
                &mut warmup_phase,
                &mut failed_reason,
                action,
            ) {
                return;
            }

            // Overall warmup deadline — see warmup_timeout above.
            if warmup_start.elapsed() > warmup_timeout {
                failed_reason = Some(format!(
                    "warmup timeout after {:.1}s",
                    warmup_start.elapsed().as_secs_f64()
                ));
                warmup_phase = WarmupPhase::Failed;
                Self::wake_event_loop(pipe_w_raw);
                return;
            }

            // Stop warming as soon as shutdown is requested.
            if shutdown_flag.load(Ordering::SeqCst) {
                if verbose {
                    eprintln!(
                        "[claude-print pool] Worker {} warmup cancelled (shutdown)",
                        worker_id
                    );
                }
                failed_reason = Some("pool shutdown during warmup".to_string());
                warmup_phase = WarmupPhase::Failed;
                Self::wake_event_loop(pipe_w_raw);
                return;
            }

            // Phase machine: trust dismissed → settling.
            if warmup_phase == WarmupPhase::Starting
                && *startup_seq.phase() == crate::startup::StartupPhase::TrustDismissed
            {
                warmup_phase = WarmupPhase::Settling;
                if verbose {
                    eprintln!(
                        "[claude-print pool] Worker {} trust dismissed, settling...",
                        worker_id
                    );
                }
            }
        });

        match reason {
            Ok(crate::event_loop::ExitReason::ChildExited) => {
                // The child exited — or the master fd was closed under us
                // (POLLNVAL, e.g. the pool shutdown path destroyed this
                // worker from another thread). Either way warming is over.
                let reason = failed_reason
                    .take()
                    .unwrap_or_else(|| "child exited during warmup".to_string());
                Self::fail(worker_id, &reason)
            }
            Ok(crate::event_loop::ExitReason::Interrupted) => {
                // Self-pipe wake: the callback reached a terminal phase.
                match warmup_phase {
                    WarmupPhase::Ready => {
                        if verbose {
                            eprintln!(
                                "[claude-print pool] Worker {} settled and ready in {:.1}s",
                                worker_id,
                                warmup_start.elapsed().as_secs_f64()
                            );
                        }
                        WarmupNotification::Ready(worker_id)
                    }
                    WarmupPhase::Failed => Self::fail(
                        worker_id,
                        failed_reason.as_deref().unwrap_or("warmup failed"),
                    ),
                    // Nothing else writes this pipe, so a wake before a
                    // terminal phase cannot happen; fail loudly rather than
                    // re-enter a loop that would spin.
                    _ => Self::fail(worker_id, "warmup interrupted mid-phase"),
                }
            }
            Ok(crate::event_loop::ExitReason::FifoPayload(_)) => {
                // The stop FIFO is never registered during warmup, so this is
                // unexpected; fail rather than re-poll.
                Self::fail(worker_id, "unexpected FIFO payload during warmup")
            }
            Err(e) => Self::fail(worker_id, &format!("warmup event loop error: {e}")),
        }
    }

    /// Clean up all workers on shutdown
    pub fn shutdown_all(&mut self) {
        eprintln!(
            "[claude-print pool] Shutting down, cleaning up {} workers",
            self.workers.len()
        );

        let workers: Vec<_> = self.workers.drain().map(|(_, w)| w).collect();

        for worker in workers {
            self.destroy_worker(worker);
        }
    }

    /// Get a reference to a worker by ID (for socket server to extract fd)
    pub fn get_worker(&self, worker_id: &str) -> Option<&PoolWorker> {
        self.workers.get(worker_id)
    }

    /// Update worker state (for warmup completion)
    pub fn update_worker_state(&mut self, worker_id: &str, new_state: WorkerState) -> bool {
        if let Some(worker) = self.workers.get_mut(worker_id) {
            worker.state = new_state;
            worker.state_since = Instant::now();
            true
        } else {
            false
        }
    }
}

/// Bind the pool's listening socket at `path` with user-only (0600) permissions.
///
/// Identity of the socket file this daemon created: the (device, inode) of
/// the filesystem node the bind placed at the socket path, captured
/// immediately after binding.
///
/// A Unix socket node is just a directory entry naming an in-kernel socket.
/// While the daemon runs, that entry can be replaced — another daemon taking
/// the same path, an administrator, a test — without the bound socket
/// noticing, and the daemon stays reachable only through the entry that was
/// removed. Shutdown must therefore never blindly unlink the path: by the
/// time it runs, the path may name a file this daemon never created.
/// [`PoolServer::cleanup`] removes the path only when it still resolves to
/// the exact inode recorded here.
#[derive(Debug, Clone, Copy)]
struct SocketIdentity {
    dev: u64,
    ino: u64,
}

/// Capture the socket node's identity by stat'ing the path immediately after
/// the bind that created it.
///
/// This must stat the *path*, not fstat the listener: a Unix socket fd
/// surfaces its sockfs pseudo-inode, which shares nothing with the filesystem
/// node the path names. The bind-to-stat window is real but microscopic (the
/// node was created microseconds earlier), and a replacement landing inside
/// it merely degenerates this one shutdown to the unguarded behavior — it
/// cannot strand a foreign file with a stale claim on it.
fn socket_identity(path: &std::path::Path) -> Option<SocketIdentity> {
    use std::os::unix::fs::MetadataExt;
    let metadata = std::fs::metadata(path).ok()?;
    Some(SocketIdentity {
        dev: metadata.dev(),
        ino: metadata.ino(),
    })
}

/// Bind the pool's listening socket at `path` with user-only (0600) permissions.
///
/// A Unix socket node is created with `0777 & ~umask`; under a permissive or
/// cleared umask a plain bind would hand `/tmp` a world-connectable pool socket
/// — anyone who can connect to it can acquire a warmed `claude` worker. The
/// process umask is therefore narrowed to 0077 across the bind so the node is
/// owner-only from the first instant, and the mode is then set explicitly so
/// the guarantee does not depend on umask semantics at all. Serve mode calls
/// this before any worker thread exists, so no concurrent file creation sees
/// the narrowed mask. A stale socket (or any other leftover file) at `path` is
/// replaced, matching the daemon's restart story — the *shutdown* side of this
/// bargain is narrower: cleanup removes only the socket the daemon itself
/// created (see [`PoolServer::cleanup`]).
pub fn bind_socket(path: &std::path::Path) -> std::io::Result<std::os::unix::net::UnixListener> {
    use std::os::unix::fs::PermissionsExt;

    if path.exists() {
        std::fs::remove_file(path)?;
    }

    let previous_mask = unsafe { libc::umask(0o077) };
    let bind_result = std::os::unix::net::UnixListener::bind(path);
    unsafe {
        libc::umask(previous_mask);
    }
    let listener = bind_result?;

    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;

    Ok(listener)
}

/// Unix socket server for pool daemon
///
/// Listens on a Unix domain socket and handles client requests for workers.
/// PTY file descriptors are sent via SCM_RIGHTS (ancillary data).
pub struct PoolServer {
    /// Socket path
    socket_path: std::path::PathBuf,
    /// Listener socket
    listener: Option<std::os::unix::net::UnixListener>,
    /// (dev, ino) of the socket file this daemon created, captured at bind
    /// time. `None` until `run` binds successfully; `cleanup` removes nothing
    /// without it.
    bound_identity: Option<SocketIdentity>,
    /// Pool manager
    manager: Arc<Mutex<PoolManager>>,
    /// Verbose logging
    verbose: bool,
}

impl PoolServer {
    /// Create a new pool server
    pub fn new(socket_path: Option<String>, manager: PoolManager, verbose: bool) -> Self {
        let path = socket_path
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::path::PathBuf::from(DEFAULT_SOCKET_PATH));

        Self {
            socket_path: path,
            listener: None,
            bound_identity: None,
            manager: Arc::new(Mutex::new(manager)),
            verbose,
        }
    }

    /// Start the server - bind to socket and accept connections
    pub fn run(&mut self) -> Result<(), anyhow::Error> {
        let listener = bind_socket(&self.socket_path).map_err(|e| {
            anyhow::anyhow!(
                "failed to set up pool socket {}: {} — check that the parent \
                 directory exists and is writable, and that no other daemon is \
                 already using this socket path",
                self.socket_path.display(),
                e
            )
        })?;

        // Record what we created before anything can replace the path — this
        // is what makes shutdown's unlink ownership-checked.
        self.bound_identity = socket_identity(&self.socket_path);

        self.listener = Some(listener);

        if self.verbose {
            eprintln!(
                "[claude-print pool] Listening on {}",
                self.socket_path.display()
            );
        }

        // Accept connections until shutdown. Whatever the outcome, this
        // daemon stops accepting here: dropping the listener closes the fd
        // and ends the socket's listening state before worker teardown and
        // path removal run (teardown order — stop accepting, reap children,
        // remove socket — is the run_serve contract).
        let result = self.accept_loop();
        drop(self.listener.take());
        result
    }

    /// Accept and handle client connections
    fn accept_loop(&self) -> Result<(), anyhow::Error> {
        let listener = self.listener.as_ref().unwrap();

        // poll(2) with a bounded timeout rather than non-blocking accept plus
        // sleep: a pending connection is accepted the instant poll reports the
        // listener readable, a shutdown request is noticed within
        // ACCEPT_POLL_TIMEOUT_MS even without the signal self-pipe, and the
        // same tick drives maintain() (top-up respawn cadence).
        let mut poll_fds = [libc::pollfd {
            fd: listener.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        }];

        loop {
            {
                let mut manager = self.manager.lock().unwrap();
                // Relay an async signal into the manager's own flag: the
                // handler could only touch the SERVE_SIGNALED static, and
                // this tick is what turns it into a real shutdown.
                if SERVE_SIGNALED.load(Ordering::SeqCst) {
                    manager.shutdown();
                }
                if manager.shutdown.load(Ordering::SeqCst) {
                    break;
                }
                // Pool maintenance tick: mark finished warmups Ready, retire
                // failed ones, top the pool back up to target size. No-ops
                // once shutdown is requested (maintain checks the flag too).
                if let Err(e) = manager.maintain() {
                    eprintln!("[claude-print pool] Maintain failed: {}", e);
                }
            }

            poll_fds[0].revents = 0;
            let ret = unsafe { libc::poll(poll_fds.as_mut_ptr(), 1, ACCEPT_POLL_TIMEOUT_MS) };
            if ret < 0 {
                let errno = nix::errno::Errno::last();
                if errno == nix::errno::Errno::EINTR {
                    continue;
                }
                return Err(anyhow::anyhow!("pool listener poll failed: {errno}"));
            }
            if ret == 0 {
                continue; // timeout → re-check shutdown + run the maintain tick
            }

            // The listener fd itself went bad — nothing more to accept.
            if poll_fds[0].revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0 {
                return Err(anyhow::anyhow!(
                    "pool listener fd error (revents = {})",
                    poll_fds[0].revents
                ));
            }

            if poll_fds[0].revents & libc::POLLIN == 0 {
                continue; // spurious wakeup of some other kind
            }

            match listener.accept() {
                Ok((stream, _addr)) => {
                    let manager = Arc::clone(&self.manager);
                    let verbose = self.verbose;

                    // Handle each connection in a thread
                    std::thread::spawn(move || {
                        if let Err(e) = Self::handle_connection(stream, manager, verbose) {
                            eprintln!("[claude-print pool] Connection error: {}", e);
                        }
                    });
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    // The connection vanished from the backlog between poll
                    // and accept; the next poll cycle retries.
                    continue;
                }
                Err(e) => {
                    if !self.manager.lock().unwrap().shutdown.load(Ordering::SeqCst) {
                        eprintln!("[claude-print pool] Accept error: {}", e);
                    }
                    break;
                }
            }
        }

        Ok(())
    }

    /// Read one length-prefixed request frame from `stream`.
    ///
    /// Returns `Ok(None)` when the peer closed without sending a complete
    /// frame — including the partial cases (half a length prefix, or a body
    /// shorter than its prefix claims). EOF must be checked on each read's
    /// own return, never on the running total: a truncated frame makes every
    /// later `read` return `Ok(0)` immediately, so a total-based check never
    /// fires and the connection thread spins on EOF at 100% CPU forever, one
    /// core per bad client. Length prefixes past [`MAX_REQUEST_BYTES`] are a
    /// protocol violation: an arbitrary 32-bit length would otherwise pin up
    /// to 4 GiB of daemon memory per connection.
    fn read_frame(stream: &mut std::os::unix::net::UnixStream) -> std::io::Result<Option<Vec<u8>>> {
        // Read the 4-byte length prefix
        let mut len_buf = [0u8; 4];
        let mut n_read = 0;
        while n_read < len_buf.len() {
            let n = stream.read(&mut len_buf[n_read..])?;
            if n == 0 {
                return Ok(None); // EOF: clean close, even mid-prefix
            }
            n_read += n;
        }

        let msg_len = u32::from_be_bytes(len_buf) as usize;
        if msg_len > MAX_REQUEST_BYTES {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "request length prefix {} exceeds the {}-byte maximum",
                    msg_len, MAX_REQUEST_BYTES
                ),
            ));
        }

        // Read the request body
        let mut buf = vec![0u8; msg_len];
        let mut n_read = 0;
        while n_read < buf.len() {
            let n = stream.read(&mut buf[n_read..])?;
            if n == 0 {
                return Ok(None); // EOF: client went away mid-frame
            }
            n_read += n;
        }

        Ok(Some(buf))
    }

    /// Handle a single client connection
    fn handle_connection(
        mut stream: std::os::unix::net::UnixStream,
        manager: Arc<Mutex<PoolManager>>,
        verbose: bool,
    ) -> Result<(), anyhow::Error> {
        // Set read timeout to prevent hanging
        stream.set_read_timeout(Some(Duration::from_secs(5)))?;

        // One request frame; a client that closed (cleanly or mid-frame)
        // just ends this connection, like any other close.
        let buf = match Self::read_frame(&mut stream) {
            Ok(Some(buf)) => buf,
            Ok(None) => return Ok(()),
            Err(e) => return Err(e.into()),
        };

        // Parse JSON request
        let request: PoolRequest = serde_json::from_slice(&buf)?;

        match request {
            PoolRequest::Acquire { timeout_secs } => {
                Self::handle_acquire(stream, manager, timeout_secs, verbose)?;
            }
            PoolRequest::Release { worker_id } => {
                Self::handle_release(stream, manager, &worker_id, verbose)?;
            }
        }

        Ok(())
    }

    /// Handle acquire request - assign a worker and send PTY fd
    fn handle_acquire(
        mut stream: std::os::unix::net::UnixStream,
        manager: Arc<Mutex<PoolManager>>,
        _timeout_secs: u64,
        verbose: bool,
    ) -> Result<(), anyhow::Error> {
        // Try to acquire a worker immediately
        let worker_id = {
            let mut mgr = manager.lock().unwrap();
            mgr.acquire_worker()
        };

        match worker_id {
            Ok(id) => {
                // Get the worker to extract its PTY fd and everything the
                // client needs to drive the session. The Stop FIFO lives in
                // the worker's own hook installer (per-worker temp dir), the
                // pid feeds the client watchdog, and the cwd is the daemon's
                // working directory — the child inherited it at spawn, so it
                // is where the child writes `~/.claude/projects/` transcripts
                // and the directory the client's stream-json discovery must
                // watch. The daemon never chdirs, so the process cwd is the
                // worker's cwd for every worker it holds.
                let (master_fd, stop_fifo, pid) = {
                    let mgr = manager.lock().unwrap();
                    let worker = mgr.get_worker(&id).unwrap();
                    let fifo = worker
                        .hook_installer
                        .as_ref()
                        .map(|h| h.fifo_path.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    let pid = u32::try_from(worker.child_pid.as_raw()).unwrap_or(0);
                    (worker.master_fd, fifo, pid)
                };
                let cwd = std::env::current_dir()
                    .map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or_default();

                // Send success response + PTY fd via SCM_RIGHTS
                let response = PoolResponse::WorkerAssigned {
                    worker_id: id.clone(),
                    message: "Worker ready".to_string(),
                    stop_fifo,
                    pid,
                    cwd,
                };

                let json = serde_json::to_vec(&response)?;

                // Send response length + JSON
                let len_buf = (json.len() as u32).to_be_bytes();
                stream.write_all(&len_buf)?;
                stream.write_all(&json)?;

                // Send PTY fd as ancillary data
                Self::send_fd(&stream, master_fd)?;

                if verbose {
                    eprintln!("[claude-print pool] Assigned worker {}", id);
                }

                Ok(())
            }
            Err(response) => {
                // Send error response
                let json = serde_json::to_vec(&response)?;
                let len_buf = (json.len() as u32).to_be_bytes();
                stream.write_all(&len_buf)?;
                stream.write_all(&json)?;
                Ok(())
            }
        }
    }

    /// Handle release request - destroy a worker
    fn handle_release(
        mut stream: std::os::unix::net::UnixStream,
        manager: Arc<Mutex<PoolManager>>,
        worker_id: &str,
        verbose: bool,
    ) -> Result<(), anyhow::Error> {
        let result = {
            let mut mgr = manager.lock().unwrap();
            mgr.release_worker(worker_id)
        };

        let response = match result {
            Ok(()) => PoolResponse::WorkerAssigned {
                worker_id: worker_id.to_string(),
                message: "Worker released".to_string(),
                // A release reply carries no assignment payload — the client
                // treats any WorkerAssigned here as success and ignores the
                // extras.
                stop_fifo: String::new(),
                pid: 0,
                cwd: String::new(),
            },
            Err(err) => err,
        };

        let json = serde_json::to_vec(&response)?;
        let len_buf = (json.len() as u32).to_be_bytes();
        stream.write_all(&len_buf)?;
        stream.write_all(&json)?;

        if verbose {
            eprintln!("[claude-print pool] Released worker {}", worker_id);
        }

        Ok(())
    }

    /// Send a file descriptor over a Unix socket via SCM_RIGHTS
    fn send_fd(stream: &std::os::unix::net::UnixStream, fd: RawFd) -> Result<(), anyhow::Error> {
        use std::os::unix::io::AsRawFd;

        let socket_fd = stream.as_raw_fd();

        // Create ancillary message with the fd
        let iov = [std::io::IoSlice::new(&[0u8; 1])];

        unsafe {
            let mut cmsg: libc::cmsghdr = std::mem::zeroed();
            // cmsg_len is usize on glibc but u32 on musl — cast into the
            // field's own type so both targets compile.
            cmsg.cmsg_len =
                (std::mem::size_of::<libc::cmsghdr>() + std::mem::size_of::<RawFd>()) as _;
            cmsg.cmsg_level = libc::SOL_SOCKET;
            cmsg.cmsg_type = libc::SCM_RIGHTS;

            let mut msg: libc::msghdr = std::mem::zeroed();
            msg.msg_iov = iov.as_ptr() as *mut libc::iovec;
            msg.msg_iovlen = 1;
            msg.msg_control = &mut cmsg as *mut _ as *mut _;
            msg.msg_controllen = cmsg.cmsg_len;

            // Copy the fd into the cmsg data
            let cmsg_data = (msg.msg_control as *mut u8).add(std::mem::size_of::<libc::cmsghdr>());
            *(cmsg_data as *mut RawFd) = fd;

            let ret = libc::sendmsg(socket_fd, &msg, 0);
            if ret < 0 {
                return Err(anyhow::anyhow!(
                    "sendmsg failed: {}",
                    nix::errno::Errno::last()
                ));
            }
        }

        Ok(())
    }

    /// Remove the socket file — but only the one this daemon created.
    ///
    /// The path can stop naming this daemon's socket while it runs (another
    /// daemon rebinding the same path, an administrator, a test), and a daemon
    /// that never bound has no socket at all. Unlinking whatever sits at the
    /// path at shutdown would delete a file this daemon never created, so the
    /// removal is guarded by the (dev, ino) identity captured at bind time:
    /// a path that is already gone, or that now resolves to some other inode,
    /// is left exactly as found.
    ///
    /// The guard also requires the occupant to be a *socket*: inode numbers
    /// are recycled after unlink — deleting our node and dropping a regular
    /// file at the same path can hand the replacement the very same inode
    /// number — but a recycled number reused for a non-socket is excluded
    /// here. The residual window (the number recycled for another *socket*
    /// before shutdown) is theoretical; nothing richer is exposed to detect
    /// it.
    pub fn cleanup(&self) {
        use std::os::unix::fs::FileTypeExt;
        use std::os::unix::fs::MetadataExt;

        let identity = match self.bound_identity {
            Some(identity) => identity,
            None => return,
        };
        let metadata = match std::fs::metadata(&self.socket_path) {
            Ok(metadata) => metadata,
            // Already gone (or unreadable): nothing of ours left to remove.
            Err(_) => return,
        };
        let ours = metadata.file_type().is_socket()
            && metadata.dev() == identity.dev
            && metadata.ino() == identity.ino;
        if ours {
            if self.verbose {
                eprintln!(
                    "[claude-print pool] Removed pool socket {}",
                    self.socket_path.display()
                );
            }
            let _ = std::fs::remove_file(&self.socket_path);
        }
    }

    /// Get reference to the pool manager (for signal handling)
    pub fn manager(&self) -> &Arc<Mutex<PoolManager>> {
        &self.manager
    }

    /// Get mutable reference to the pool manager (for initialization)
    pub fn manager_mut(&mut self) -> std::sync::MutexGuard<'_, PoolManager> {
        self.manager.lock().unwrap()
    }
}

/// Why an acquire did not yield a usable worker, and what the caller owes it
/// (ADR-005 fallback contract — see the module header).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AcquireFailure {
    /// No pool answered at the socket path: missing socket file, refused or
    /// otherwise failed connect, connect timeout, permission. ADR-005's
    /// documented stateless fallback applies.
    Unreachable {
        socket: std::path::PathBuf,
        reason: String,
    },
    /// A well-formed pool answered but cannot serve right now (`pool_full`,
    /// `shutting_down`, `internal_error`, `acquire_timeout`). The pool is up;
    /// it just has nothing to hand out. Stateless fallback applies here too —
    /// the flag is additive latency work, never a new way to fail.
    PoolUnavailable { code: String, error: String },
    /// The pool answered with protocol garbage, an incomplete worker
    /// assignment, or a failed fd transfer. The daemon is reachable but
    /// broken — a hard error, NOT a fallback: silently spawning an unpooled
    /// full-price session would mask the breakage. Always produced within the
    /// caller's acquire deadline; a daemon that accepts and then goes silent
    /// surfaces as this variant when the deadline expires.
    Protocol(String),
}

impl std::fmt::Display for AcquireFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AcquireFailure::Unreachable { socket, reason } => {
                write!(f, "no pool reachable at {}: {}", socket.display(), reason)
            }
            AcquireFailure::PoolUnavailable { code, error } => {
                write!(f, "pool cannot serve a worker ({}: {})", code, error)
            }
            AcquireFailure::Protocol(msg) => write!(f, "pool protocol failure: {msg}"),
        }
    }
}

/// Does this failure route the caller to the ADR-005 stateless fallback?
pub fn is_stateless_fallback(failure: &AcquireFailure) -> bool {
    matches!(
        failure,
        AcquireFailure::Unreachable { .. } | AcquireFailure::PoolUnavailable { .. }
    )
}

/// What the invocation path owes for each acquire outcome (the caller-facing
/// half of the ADR-005 client contract).
#[derive(Debug)]
pub enum InvocationAcquisition {
    /// `--pool-socket` was absent: no pool code ran, the ordinary hot path is
    /// untouched.
    NotRequested,
    /// A prewarmed worker was acquired. The caller owes it exactly one
    /// prompt, driven through [`crate::session::Session::run_pooled`]; the
    /// worker is released exactly once — explicitly after a successful drive
    /// (before the stream-json drain), via the [`AcquiredWorker`] drop path on
    /// every other exit that leaves the process alive (see
    /// [`AcquiredWorker`] for the hard boundary) — so the daemon tears it
    /// down and spawns a replacement.
    Acquired(AcquiredWorker),
    /// The pool did not yield a worker for a reason the ADR-005 contract
    /// routes to the stateless path. By construction the carried failure
    /// satisfies [`is_stateless_fallback`]; the caller owes one verbose
    /// diagnostic line and then the ordinary stateless session.
    Fallback(AcquireFailure),
}

/// The invocation-path acquisition step (ADR-005): when `--pool-socket` names
/// a pool daemon, acquire one prewarmed worker inside the caller's existing
/// timeout budget and classify the outcome.
///
/// `socket: None` (flag absent) returns [`InvocationAcquisition::NotRequested`]
/// before a [`PoolClient`] is ever constructed — the default path runs no pool
/// code and pays nothing.
///
/// The budget is [`DEFAULT_ACQUIRE_TIMEOUT_SECS`] (60s) capped by the
/// caller's invocation timeout: a short `--timeout` is never spent mostly on
/// waiting for a busy pool, and the client cap keeps a wedged daemon from
/// stalling an hour-long invocation for an hour. The budget bounds the whole
/// exchange (connect, request, response, fd transfer), so no outcome of this
/// function can hang.
pub fn acquire_for_invocation(
    socket: Option<&std::path::Path>,
    caller_timeout_secs: u64,
) -> Result<InvocationAcquisition, AcquireFailure> {
    let Some(socket) = socket else {
        return Ok(InvocationAcquisition::NotRequested);
    };
    let budget_secs = DEFAULT_ACQUIRE_TIMEOUT_SECS.min(caller_timeout_secs.max(1));
    match PoolClient::new(socket.to_path_buf()).acquire(budget_secs) {
        Ok(worker) => Ok(InvocationAcquisition::Acquired(worker)),
        Err(failure) if is_stateless_fallback(&failure) => {
            Ok(InvocationAcquisition::Fallback(failure))
        }
        Err(protocol) => Err(protocol),
    }
}

/// One-time budget for a client release exchange. Teardown is best-effort by
/// contract (a dead daemon has nothing left to release), but it must never
/// hang the client: this bounds the connect + frame + reply.
const RELEASE_TIMEOUT_SECS: u64 = 10;

/// Wait until `fd` is ready for `events` (POLLIN/POLLOUT) or `deadline`
/// passes. The returned error names the timeout so protocol failures can
/// distinguish a hung daemon from a closed one.
fn wait_ready(fd: RawFd, events: i16, deadline: Instant) -> Result<(), String> {
    loop {
        let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
            return Err("timed out waiting for the pool".to_string());
        };
        let timeout_ms = remaining.as_millis().min(i32::MAX as u128) as i32;
        let mut fds = [libc::pollfd {
            fd,
            events,
            revents: 0,
        }];
        let ret = unsafe { libc::poll(fds.as_mut_ptr(), 1, timeout_ms) };
        if ret < 0 {
            let errno = nix::errno::Errno::last();
            if errno == nix::errno::Errno::EINTR {
                continue;
            }
            return Err(format!("poll failed: {errno}"));
        }
        if ret == 0 {
            return Err("timed out waiting for the pool".to_string());
        }
        if fds[0].revents & libc::POLLNVAL != 0 {
            return Err("pool socket fd went invalid".to_string());
        }
        return Ok(());
    }
}

/// Read exactly `buf.len()` bytes from a non-blocking `stream`, or fail once
/// `deadline` passes. Unlike `read_exact` over a socket timeout, the deadline
/// is enforced across the WHOLE read: a daemon dribbling one byte per interval
/// cannot stretch the exchange past it.
fn read_exact_bounded(
    stream: &mut std::os::unix::net::UnixStream,
    buf: &mut [u8],
    deadline: Instant,
) -> Result<(), String> {
    let mut filled = 0;
    while filled < buf.len() {
        wait_ready(stream.as_raw_fd(), libc::POLLIN, deadline)?;
        match stream.read(&mut buf[filled..]) {
            Ok(0) => {
                return Err("pool closed the connection mid-response".to_string());
            }
            Ok(n) => filled += n,
            Err(e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::Interrupted =>
            {
                continue;
            }
            Err(e) => return Err(format!("read from pool failed: {e}")),
        }
    }
    Ok(())
}

/// Write all of `buf` to a non-blocking `stream`, or fail once `deadline`
/// passes (same whole-exchange bound as [`read_exact_bounded`]).
fn write_all_bounded(
    stream: &mut std::os::unix::net::UnixStream,
    mut buf: &[u8],
    deadline: Instant,
) -> Result<(), String> {
    while !buf.is_empty() {
        wait_ready(stream.as_raw_fd(), libc::POLLOUT, deadline)?;
        match stream.write(buf) {
            Ok(0) => return Err("wrote nothing to the pool".to_string()),
            Ok(n) => buf = &buf[n..],
            Err(e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::Interrupted =>
            {
                continue;
            }
            Err(e) => return Err(format!("write to pool failed: {e}")),
        }
    }
    Ok(())
}

/// Send one length-prefixed frame (the client half of the daemon's
/// [`PoolServer::read_frame`] wire format).
fn send_frame(
    stream: &mut std::os::unix::net::UnixStream,
    payload: &[u8],
    deadline: Instant,
) -> Result<(), String> {
    let len = u32::try_from(payload.len())
        .map_err(|_| "request frame does not fit the wire length prefix".to_string())?;
    write_all_bounded(stream, &len.to_be_bytes(), deadline)?;
    write_all_bounded(stream, payload, deadline)
}

/// Read one length-prefixed frame, rejecting prefixes past
/// [`MAX_REQUEST_BYTES`] (the same cap the daemon applies to requests — the
/// wire length is an unbounded u32 and must not pin client memory either).
fn read_frame_bounded(
    stream: &mut std::os::unix::net::UnixStream,
    deadline: Instant,
) -> Result<Vec<u8>, String> {
    let mut len_buf = [0u8; 4];
    read_exact_bounded(stream, &mut len_buf, deadline)?;
    let msg_len = u32::from_be_bytes(len_buf) as usize;
    if msg_len > MAX_REQUEST_BYTES {
        return Err(format!(
            "response length prefix {} exceeds the {}-byte maximum",
            msg_len, MAX_REQUEST_BYTES
        ));
    }
    let mut buf = vec![0u8; msg_len];
    read_exact_bounded(stream, &mut buf, deadline)?;
    Ok(buf)
}

/// Receive one file descriptor via SCM_RIGHTS from a non-blocking `stream`,
/// bounded by `deadline`.
///
/// The control buffer is a properly sized, properly aligned byte array that
/// the kernel fills in — never a `cmsghdr` with 1 KiB of tail read past it,
/// and the payload iovec points at real storage (a NULL base with a nonzero
/// length faults the recvmsg the moment the sender's data byte arrives,
/// losing the fd with it).
fn recv_fd_bounded(
    stream: &mut std::os::unix::net::UnixStream,
    deadline: Instant,
) -> Result<OwnedFd, String> {
    use std::os::unix::io::AsRawFd;

    /// Big enough for one SCM_RIGHTS cmsghdr carrying several fds, aligned
    /// for `cmsghdr` (which contains a usize length).
    #[repr(C, align(8))]
    struct ControlBuffer([u8; 128]);

    let mut control = ControlBuffer([0u8; 128]);
    let mut payload_byte = [0u8; 1];

    loop {
        wait_ready(stream.as_raw_fd(), libc::POLLIN, deadline)?;
        let mut iov = [libc::iovec {
            iov_base: payload_byte.as_mut_ptr() as *mut libc::c_void,
            iov_len: payload_byte.len(),
        }];
        let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
        msg.msg_iov = iov.as_mut_ptr();
        msg.msg_iovlen = 1;
        msg.msg_control = control.0.as_mut_ptr() as *mut libc::c_void;
        // msg_controllen is usize on glibc but u32 on musl.
        msg.msg_controllen = control.0.len() as _;

        let ret = unsafe {
            libc::recvmsg(
                stream.as_raw_fd(),
                &mut msg,
                libc::MSG_DONTWAIT | libc::MSG_CMSG_CLOEXEC,
            )
        };
        if ret < 0 {
            let errno = nix::errno::Errno::last();
            if errno == nix::errno::Errno::EINTR || errno == nix::errno::Errno::EAGAIN {
                continue;
            }
            return Err(format!("failed to receive the worker fd: {errno}"));
        }

        // Walk every ancillary header the kernel wrote and take the first
        // SCM_RIGHTS fd. CMSG_NXTHDR is the only safe iteration: headers are
        // kernel-formatted and their lengths are data.
        let mut hdr = unsafe { libc::CMSG_FIRSTHDR(&msg) };
        while !hdr.is_null() {
            let hdr_ref = unsafe { &*hdr };
            if hdr_ref.cmsg_level == libc::SOL_SOCKET && hdr_ref.cmsg_type == libc::SCM_RIGHTS {
                let data_len = hdr_ref
                    .cmsg_len
                    // CMSG_LEN(0) is usize on glibc but u32 on musl, matching
                    // cmsg_len on each — cast into the header's own type.
                    .saturating_sub(unsafe { libc::CMSG_LEN(0) } as _);
                if data_len as usize >= std::mem::size_of::<RawFd>() {
                    let fd =
                        unsafe { std::ptr::read_unaligned(libc::CMSG_DATA(hdr) as *const RawFd) };
                    if fd >= 0 {
                        // SAFETY: the kernel installed a fresh descriptor in
                        // this process's table; we now own its lifetime.
                        // MSG_CMSG_CLOEXEC keeps it out of any child exec.
                        return Ok(unsafe { OwnedFd::from_raw_fd(fd) });
                    }
                }
            }
            hdr = unsafe { libc::CMSG_NXTHDR(&msg, hdr) };
        }

        // Data arrived (ret >= 0) but no usable fd: the daemon's transfer is
        // broken — do not wait for a second message, fail.
        return Err("pool sent no usable fd with the worker assignment".to_string());
    }
}

/// A worker acquired from a pool daemon: a prewarmed PTY plus everything the
/// client session needs to drive it to exactly one prompt (ADR-005).
///
/// Owning this struct owns the PTY master fd. Dropping it releases the worker
/// back to the daemon, which tears the worker down and spawns a replacement: a
/// released worker is never handed a second prompt, so no session id, hook
/// path, transcript offset, environment, or fd can leak between callers.
///
/// The release runs exactly once on every exit path that leaves the process
/// alive to run destructors: the explicit release after a successful drive,
/// every `?` / error return, and a panic under an unwinding panic strategy
/// (all test builds, through `Session::run_pooled`'s `catch_unwind`). It
/// cannot run when the process dies without destructors — a panic under the
/// shipped `panic = "abort"` release profile, `SIGKILL`, or a signal at its
/// default disposition. Nothing client-side can release then; the daemon
/// reclaims the worker at its own shutdown. (A daemon-side lease would be the
/// fix if that window ever matters; it is deliberately out of scope here.)
pub struct AcquiredWorker {
    worker_id: String,
    master: OwnedFd,
    child_pid: u32,
    stop_fifo: std::path::PathBuf,
    cwd: std::path::PathBuf,
    socket_path: std::path::PathBuf,
    released: bool,
}

impl std::fmt::Debug for AcquiredWorker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AcquiredWorker")
            .field("worker_id", &self.worker_id)
            .field("child_pid", &self.child_pid)
            .field("stop_fifo", &self.stop_fifo)
            .field("cwd", &self.cwd)
            .field("socket_path", &self.socket_path)
            .field("released", &self.released)
            .finish_non_exhaustive()
    }
}

impl AcquiredWorker {
    /// Cross-check the assignment against what driving a session actually
    /// requires. `serde(default)` lets a legacy (pre-fifo) daemon's frame
    /// parse with empty fields; any empty field means the daemon predates the
    /// client and the acquire is a protocol failure, not a session.
    fn validate_parts(worker_id: &str, stop_fifo: &str, pid: u32, cwd: &str) -> Result<(), String> {
        if worker_id.is_empty() {
            return Err("worker_assigned carried an empty worker_id".to_string());
        }
        if stop_fifo.is_empty() {
            return Err(
                "worker_assigned carried no stop_fifo — the daemon predates the \
                 Stop-FIFO handoff this client requires"
                    .to_string(),
            );
        }
        if pid == 0 {
            return Err("worker_assigned carried no worker pid".to_string());
        }
        if cwd.is_empty() {
            return Err("worker_assigned carried no worker cwd".to_string());
        }
        Ok(())
    }

    fn from_parts(
        worker_id: String,
        stop_fifo: String,
        pid: u32,
        cwd: String,
        master: OwnedFd,
        socket_path: std::path::PathBuf,
    ) -> Result<Self, String> {
        Self::validate_parts(&worker_id, &stop_fifo, pid, &cwd)?;
        Ok(Self {
            worker_id,
            master,
            child_pid: pid,
            stop_fifo: std::path::PathBuf::from(stop_fifo),
            cwd: std::path::PathBuf::from(cwd),
            socket_path,
            released: false,
        })
    }

    /// The daemon's id for this worker.
    pub fn worker_id(&self) -> &str {
        &self.worker_id
    }

    /// The worker's process id. Fed to the client watchdog's deadline
    /// machinery, which on the pool path never signals it
    /// ([`crate::watchdog::Watchdog::without_child_signals`]): the worker is
    /// daemon-owned, so deadline enforcement reroutes through worker release
    /// → daemon teardown instead of a direct signal.
    pub fn pid(&self) -> nix::unistd::Pid {
        nix::unistd::Pid::from_raw(self.child_pid as i32)
    }

    /// The prewarmed PTY master fd — polled by the client's event loop, closed
    /// when this struct drops.
    pub fn master_fd(&self) -> RawFd {
        self.master.as_raw_fd()
    }

    /// The daemon-side Stop FIFO the worker's hook writes its payload into.
    /// The client reads the payload from here instead of a FIFO in its own
    /// temp dir — the child was launched with the daemon's hook settings.
    pub fn stop_fifo(&self) -> &std::path::Path {
        &self.stop_fifo
    }

    /// The working directory the worker was spawned in (the daemon's). The
    /// child writes its transcripts under `$HOME/.claude/projects/<this-cwd's
    /// slug>/`, so this — not the client's cwd — is the directory the
    /// stream-json discovery must watch.
    pub fn worker_cwd(&self) -> &std::path::Path {
        &self.cwd
    }

    /// Whether the worker has already been released.
    pub fn is_released(&self) -> bool {
        self.released
    }

    /// Release the worker: tell the daemon to tear it down and spawn a
    /// replacement. Idempotent; best-effort (a dead daemon has nothing left
    /// to release, and Drop must never panic). Also runs on Drop for every
    /// exit path that did not reach an explicit release.
    pub fn release(&mut self) {
        if self.released {
            return;
        }
        self.released = true;
        if let Err(reason) = self.release_inner() {
            eprintln!(
                "claude-print: pool: releasing worker {} failed ({reason}) — \
                 the daemon tears the worker down on its own shutdown either way",
                self.worker_id
            );
        }
    }

    /// Wire half of [`Self::release`]: one bounded exchange asking the daemon
    /// to destroy the worker. Errors are collected, never panicked on.
    fn release_inner(&self) -> Result<(), String> {
        let deadline = Instant::now() + Duration::from_secs(RELEASE_TIMEOUT_SECS);
        let mut stream = connect_socket(&self.socket_path, deadline)
            .map_err(|f| format!("pool unreachable: {f}"))?;
        let request = PoolRequest::Release {
            worker_id: self.worker_id.clone(),
        };
        let json =
            serde_json::to_vec(&request).map_err(|e| format!("serialize release request: {e}"))?;
        send_frame(&mut stream, &json, deadline)?;
        // Read the reply so the daemon's connection thread completes cleanly;
        // the content is informational — the teardown is already triggered.
        let _ = read_frame_bounded(&mut stream, deadline);
        Ok(())
    }
}

impl Drop for AcquiredWorker {
    fn drop(&mut self) {
        self.release();
        // `master` (OwnedFd) closes here — after the release, so the daemon's
        // teardown never races a client still polling the PTY.
    }
}

/// Connect to the pool daemon at `socket_path`, with the whole attempt
/// bounded by `deadline`. std has no connect-timeout for Unix sockets, so
/// this drives a raw non-blocking connect and polls for completion — a full
/// backlog or a wedged daemon fails the attempt instead of hanging the
/// caller. The returned stream is non-blocking, which the bounded read/write
/// helpers require anyway.
fn connect_socket(
    socket_path: &std::path::Path,
    deadline: Instant,
) -> Result<std::os::unix::net::UnixStream, AcquireFailure> {
    use std::os::unix::ffi::OsStrExt;

    let unreachable = |reason: String| AcquireFailure::Unreachable {
        socket: socket_path.to_path_buf(),
        reason,
    };

    let path_bytes = socket_path.as_os_str().as_bytes();
    if path_bytes.is_empty() {
        return Err(unreachable("empty socket path".to_string()));
    }

    let mut addr: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    addr.sun_family = libc::AF_UNIX as libc::sa_family_t;
    if path_bytes.len() >= addr.sun_path.len() {
        return Err(unreachable(format!(
            "socket path exceeds the {}-byte sockaddr_un limit",
            addr.sun_path.len() - 1
        )));
    }
    unsafe {
        std::ptr::copy_nonoverlapping(
            path_bytes.as_ptr(),
            addr.sun_path.as_mut_ptr() as *mut u8,
            path_bytes.len(),
        );
    }

    // SAFETY: plain socket creation; CLOEXEC keeps the descriptor out of any
    // child exec, NONBLOCK bounds the connect below.
    let fd = unsafe {
        libc::socket(
            libc::AF_UNIX,
            libc::SOCK_STREAM | libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC,
            0,
        )
    };
    if fd < 0 {
        return Err(unreachable(format!(
            "socket(2) failed: {}",
            nix::errno::Errno::last()
        )));
    }
    let owned = unsafe { OwnedFd::from_raw_fd(fd) };

    // SAFETY: `addr` is a fully initialized sockaddr_un for this family and
    // `fd` is a live descriptor we just created.
    let connect_result = unsafe {
        libc::connect(
            fd,
            &addr as *const libc::sockaddr_un as *const libc::sockaddr,
            std::mem::size_of::<libc::sockaddr_un>() as libc::socklen_t,
        )
    };
    if connect_result < 0 {
        let errno = nix::errno::Errno::last();
        // EINPROGRESS (and EAGAIN on a full backlog): completion is pending —
        // poll for writability, then read the outcome out of SO_ERROR.
        if errno != nix::errno::Errno::EINPROGRESS && errno != nix::errno::Errno::EAGAIN {
            return Err(unreachable(errno.to_string()));
        }
        wait_ready(fd, libc::POLLOUT, deadline).map_err(unreachable)?;
        let mut so_error: libc::c_int = 0;
        let mut len = std::mem::size_of::<libc::c_int>() as libc::socklen_t;
        // SAFETY: standard SO_ERROR probe on a live descriptor.
        if unsafe {
            libc::getsockopt(
                fd,
                libc::SOL_SOCKET,
                libc::SO_ERROR,
                &mut so_error as *mut libc::c_int as *mut libc::c_void,
                &mut len,
            )
        } < 0
        {
            return Err(unreachable(format!(
                "getsockopt(SO_ERROR) failed: {}",
                nix::errno::Errno::last()
            )));
        }
        if so_error != 0 {
            return Err(unreachable(
                nix::errno::Errno::from_raw(so_error).to_string(),
            ));
        }
    }

    Ok(std::os::unix::net::UnixStream::from(owned))
}

/// Client for connecting to the pool daemon
pub struct PoolClient {
    /// Socket path
    socket_path: std::path::PathBuf,
}

impl PoolClient {
    /// Create a new pool client
    pub fn new(socket_path: std::path::PathBuf) -> Self {
        Self { socket_path }
    }

    /// Connect to the pool and acquire one prewarmed worker (ADR-005).
    ///
    /// `timeout_secs` bounds the WHOLE exchange — connect, request, response,
    /// and fd transfer — so a hung daemon can never stall the caller past it.
    /// On success the returned [`AcquiredWorker`] owns the PTY master fd and
    /// releases the worker on drop; the caller drives exactly one prompt
    /// through it.
    pub fn acquire(&self, timeout_secs: u64) -> Result<AcquiredWorker, AcquireFailure> {
        let deadline = Instant::now() + Duration::from_secs(timeout_secs.max(1));

        let mut stream = connect_socket(&self.socket_path, deadline)?;

        // Everything past a successful connect is protocol territory: the
        // daemon exists and answered, so failures are hard errors, not
        // fallbacks.
        let protocol = |msg: String| AcquireFailure::Protocol(msg);

        let request = PoolRequest::Acquire {
            timeout_secs: timeout_secs.max(1),
        };
        let json = serde_json::to_vec(&request)
            .map_err(|e| protocol(format!("serialize acquire request: {e}")))?;
        send_frame(&mut stream, &json, deadline).map_err(protocol)?;

        let body = read_frame_bounded(&mut stream, deadline).map_err(protocol)?;
        let response: PoolResponse = serde_json::from_slice(&body)
            .map_err(|e| protocol(format!("malformed response: {e}")))?;

        let (worker_id, stop_fifo, pid, cwd) = match response {
            PoolResponse::Error { error, code } => {
                let code = match code {
                    ErrorCode::PoolFull => "pool_full",
                    ErrorCode::AcquireTimeout => "acquire_timeout",
                    ErrorCode::InvalidWorkerId => "invalid_worker_id",
                    ErrorCode::InternalError => "internal_error",
                    ErrorCode::ShuttingDown => "shutting_down",
                };
                return Err(AcquireFailure::PoolUnavailable {
                    code: code.to_string(),
                    error,
                });
            }
            PoolResponse::WorkerAssigned {
                worker_id,
                stop_fifo,
                pid,
                cwd,
                ..
            } => (worker_id, stop_fifo, pid, cwd),
        };

        // The assignment is only usable with its fd: receive it before
        // validating the frame so a broken transfer is reported as exactly
        // that. (The kernel discards an un-received in-flight fd when this
        // socket closes, so failing here leaks nothing.)
        let master = recv_fd_bounded(&mut stream, deadline)
            .map_err(|e| protocol(format!("fd transfer failed: {e}")))?;

        AcquiredWorker::from_parts(
            worker_id,
            stop_fifo,
            pid,
            cwd,
            master,
            self.socket_path.clone(),
        )
        .map_err(protocol)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_acquire_timeout() {
        assert_eq!(default_acquire_timeout(), DEFAULT_ACQUIRE_TIMEOUT_SECS);
    }

    #[test]
    fn test_pool_request_deserialize() {
        let json = r#"{"type": "acquire", "timeout_secs": 30}"#;
        let req: PoolRequest = serde_json::from_str(json).unwrap();
        match req {
            PoolRequest::Acquire { timeout_secs } => {
                assert_eq!(timeout_secs, 30);
            }
            _ => panic!("Wrong request type"),
        }

        let json2 = r#"{"type": "release", "worker_id": "test-123"}"#;
        let req2: PoolRequest = serde_json::from_str(json2).unwrap();
        match req2 {
            PoolRequest::Release { worker_id } => {
                assert_eq!(worker_id, "test-123");
            }
            _ => panic!("Wrong request type"),
        }
    }

    #[test]
    fn test_pool_response_serialize() {
        let resp = PoolResponse::WorkerAssigned {
            worker_id: "uuid-123".to_string(),
            message: "Worker ready".to_string(),
            stop_fifo: "/tmp/stop.fifo".to_string(),
            pid: 100,
            cwd: "/srv/daemon".to_string(),
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("worker_assigned"));
        assert!(json.contains("uuid-123"));

        let err = PoolResponse::Error {
            error: "Pool full".to_string(),
            code: ErrorCode::PoolFull,
        };
        let json2 = serde_json::to_string(&err).unwrap();
        assert!(json2.contains("error"));
        assert!(json2.contains("pool_full"));
    }

    #[test]
    fn test_pool_manager_target_size() {
        let manager = PoolManager::new(2, std::path::PathBuf::from("/usr/bin/claude"), false);
        assert_eq!(manager.target_size, 2);
    }

    #[test]
    fn test_shutdown_flag() {
        let manager = PoolManager::new(1, std::path::PathBuf::from("/usr/bin/claude"), false);
        let flag = manager.shutdown_flag();
        assert!(!flag.load(Ordering::SeqCst));

        manager.shutdown();
        assert!(flag.load(Ordering::SeqCst));
    }

    // ── Signal→flag transition (claudepr-6fc0cad7 audit repair) ──────────────
    //
    // Historical trap: an in-flight wiring attempt installed handlers that
    // never set the flag, leaving serve with default dispositions in disguise
    // — the first SIGINT killed the daemon outright and leaked every worker.
    // This pin exercises the real delivery path (install → kernel delivery →
    // handler → store): `raise` targets the calling thread, so the delivery
    // cannot EINTR a blocking syscall on another test's thread, and a missing
    // installation would kill this process by default disposition instead of
    // reaching the polls below. Only this test touches SERVE_SIGNALED in this
    // binary, so it starts clear.
    #[test]
    fn serve_signal_delivery_flips_the_observable_flag() {
        // The raises below are process-directed: take the drive lock so one
        // cannot land inside a concurrent session drive's scoped handler
        // window (and a drive's SignalGuard restore cannot leave these
        // raises hitting a default disposition).
        let _drive = drive_signal_lock();
        assert!(
            !SERVE_SIGNALED.load(Ordering::SeqCst),
            "SERVE_SIGNALED must start clear; another test leaked a delivery"
        );

        install_serve_signal_handlers();

        assert_eq!(unsafe { libc::raise(libc::SIGINT) }, 0, "raise(SIGINT)");
        wait_for_serve_flag();

        // SIGTERM shares the handler; reset the latched flag between legs so
        // this one proves SIGTERM's own delivery, not the SIGINT store.
        SERVE_SIGNALED.store(false, Ordering::SeqCst);
        assert_eq!(unsafe { libc::raise(libc::SIGTERM) }, 0, "raise(SIGTERM)");
        wait_for_serve_flag();
    }

    /// Spin until the async signal handler's store becomes visible. Returning
    /// at all proves the handler ran; timing out means the handler exists but
    /// never flips the flag — the exact historical trap this pin guards.
    fn wait_for_serve_flag() {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !SERVE_SIGNALED.load(Ordering::SeqCst) {
            assert!(
                Instant::now() < deadline,
                "signal delivered but SERVE_SIGNALED never transitioned — \
                 handler is not wired to the flag"
            );
            std::thread::yield_now();
        }
    }

    #[test]
    fn validate_pool_size_accepts_the_whole_legal_range() {
        assert_eq!(validate_pool_size(1), Ok(()));
        assert_eq!(validate_pool_size(MAX_POOL_SIZE), Ok(()));
    }

    #[test]
    fn validate_pool_size_rejects_zero_with_actionable_message() {
        let err = validate_pool_size(0).unwrap_err();
        assert!(
            err.contains("--pool-size"),
            "message must name the flag: {err}"
        );
        assert!(
            err.contains("at least 1"),
            "message must state the minimum: {err}"
        );
    }

    #[test]
    fn validate_pool_size_rejects_above_max_with_actionable_message() {
        let err = validate_pool_size(MAX_POOL_SIZE + 1).unwrap_err();
        assert!(
            err.contains("--pool-size"),
            "message must name the flag: {err}"
        );
        assert!(
            err.contains(&MAX_POOL_SIZE.to_string()),
            "message must state the cap: {err}"
        );
        assert!(
            err.contains(&(MAX_POOL_SIZE + 1).to_string()),
            "message must echo the rejected value: {err}"
        );
    }

    // serve must hand clients a socket nobody else can connect to, whatever the
    // invoking shell's umask was — bind_socket sets 0600 explicitly after the
    // bind, and the narrowed-umask window keeps the node owner-only in between.
    #[test]
    fn bind_socket_creates_owner_only_socket() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pool.sock");

        let _listener = bind_socket(&path).unwrap();

        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(
            mode & 0o777,
            0o600,
            "socket must be user-only, got {mode:o}"
        );
    }

    // Restart story: a leftover socket file from a previous daemon must not
    // force the new one into a bind failure.
    #[test]
    fn bind_socket_replaces_stale_socket() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pool.sock");

        // First bind creates a live socket file...
        drop(bind_socket(&path).unwrap());
        assert!(path.exists());
        // ...and a second bind over the leftover path succeeds.
        let _second = bind_socket(&path).unwrap();
        assert!(path.exists());
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    // An unusable path (parent is a regular file) surfaces the OS error instead
    // of panicking — main.rs turns this into exit 2 with actionable stderr.
    #[test]
    fn bind_socket_surfaces_unusable_path_as_error() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("blocker");
        std::fs::write(&file, b"not a directory").unwrap();
        let path = file.join("pool.sock"); // parent is a file → ENOTDIR

        let err = bind_socket(&path).unwrap_err();
        assert_eq!(err.raw_os_error(), Some(libc::ENOTDIR));
    }

    // The other unbindable shape: the parent directory does not exist at all.
    // Bind must surface ENOENT the same way — exit-2 material — rather than
    // attempting to create directories or panicking.
    #[test]
    fn bind_socket_surfaces_missing_parent_as_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("no-such-dir").join("pool.sock");

        let err = bind_socket(&path).unwrap_err();
        assert_eq!(err.raw_os_error(), Some(libc::ENOENT));
    }

    // ── read_frame: request framing (claudepr-b78932de audit repair) ─────────
    //
    // A client that closes mid-frame must end its connection, not wedge it:
    // the pre-repair reader checked EOF against the running total instead of
    // each read's own return, so a truncated frame spun its connection thread
    // on sticky EOF at 100% CPU forever. Each of these failures hangs the test
    // suite if the regression returns.

    /// A connected socket pair (writer, reader) for framing tests.
    fn stream_pair() -> (
        std::os::unix::net::UnixStream,
        std::os::unix::net::UnixStream,
    ) {
        std::os::unix::net::UnixStream::pair().unwrap()
    }

    #[test]
    fn read_frame_reads_a_complete_length_prefixed_frame() {
        let (mut w, mut r) = stream_pair();
        w.write_all(&[0, 0, 0, 5]).unwrap();
        w.write_all(b"hello").unwrap();

        let frame = PoolServer::read_frame(&mut r).unwrap().unwrap();
        assert_eq!(frame, b"hello");
    }

    #[test]
    fn read_frame_returns_none_on_immediate_close() {
        let (w, mut r) = stream_pair();
        drop(w);

        assert!(matches!(PoolServer::read_frame(&mut r), Ok(None)));
    }

    // Half a length prefix then close: the accumulator-based EOF check could
    // never fire here (2 += 0 stays 2), which is exactly the hot-spin input.
    #[test]
    fn read_frame_returns_none_on_truncated_length_prefix() {
        let (mut w, mut r) = stream_pair();
        w.write_all(&[0, 0]).unwrap();
        drop(w);

        assert!(matches!(PoolServer::read_frame(&mut r), Ok(None)));
    }

    // A body shorter than its prefix claims (the body loop had no EOF check
    // at all) must also end the connection cleanly.
    #[test]
    fn read_frame_returns_none_on_truncated_body() {
        let (mut w, mut r) = stream_pair();
        w.write_all(&[0, 0, 0, 100]).unwrap(); // claims 100 bytes…
        w.write_all(b"only ten").unwrap(); // …sends 8, then closes
        drop(w);

        assert!(matches!(PoolServer::read_frame(&mut r), Ok(None)));
    }

    // An absurd length prefix is a protocol violation, not a frame: honouring
    // it would let one 4-byte write pin up to 4 GiB of daemon memory.
    #[test]
    fn read_frame_rejects_length_prefix_over_the_cap() {
        let (mut w, mut r) = stream_pair();
        let huge = (MAX_REQUEST_BYTES as u32 + 1).to_be_bytes();
        w.write_all(&huge).unwrap();

        let err = PoolServer::read_frame(&mut r).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
        assert!(
            err.to_string().contains(&MAX_REQUEST_BYTES.to_string()),
            "error must state the cap: {err}"
        );
    }

    // The cap boundary is inclusive: a frame of exactly MAX_REQUEST_BYTES is
    // legitimate wire data and must be read, not refused. An off-by-one in the
    // refusal (`>=`) would reject the largest legal frame.
    #[test]
    fn read_frame_accepts_a_frame_exactly_at_the_cap() {
        let (mut w, mut r) = stream_pair();
        let payload = vec![b'x'; MAX_REQUEST_BYTES];
        w.write_all(&(MAX_REQUEST_BYTES as u32).to_be_bytes())
            .unwrap();
        w.write_all(&payload).unwrap();

        let frame = PoolServer::read_frame(&mut r).unwrap().unwrap();
        assert_eq!(frame.len(), MAX_REQUEST_BYTES);
        assert_eq!(frame[0], b'x');
        assert_eq!(frame[MAX_REQUEST_BYTES - 1], b'x');
    }

    // ── destroy_worker: reaping, escalation, descendant sweep ────────────────
    //
    // (claudepr-ffaf4def audit repair) The pre-repair teardown signalled the
    // child after its exit had already been observed (a reaped pid can be
    // recycled), never reached descendants a worker had spawned, and left the
    // exit "observed" only implicitly. These pins exercise the real teardown
    // against real PTY children — no mock at this level.

    /// The process state letter from `/proc/<pid>/stat`, or `None` once the
    /// pid no longer has a /proc entry. `Z` means dead-but-unreaped.
    fn proc_state(pid: nix::unistd::Pid) -> Option<String> {
        let stat = std::fs::read_to_string(format!("/proc/{}", pid.as_raw())).ok()?;
        let rest = stat.rsplit_once(')')?.1.trim_start();
        rest.split_whitespace().next().map(str::to_owned)
    }

    /// Poll until `pid` has no /proc entry at all — neither alive nor zombie.
    /// destroy_worker observes (reaps) the direct child before returning, so
    /// this is belt-and-suspenders on the instant case, but a zombie would
    /// survive here indefinitely and fail the test.
    fn assert_proc_gone(pid: nix::unistd::Pid, budget: Duration) {
        let deadline = Instant::now() + budget;
        while let Some(state) = proc_state(pid) {
            assert!(
                Instant::now() < deadline,
                "pid {} still in /proc after destroy_worker (state {state}) — \
                 leaked or left as a zombie",
                pid.as_raw()
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// Every live process whose process group is `pgid`, scanned from /proc
    /// (stat field 5: state, ppid, pgrp after the comm field).
    fn group_members(pgid: nix::unistd::Pid) -> Vec<u32> {
        let mut found = Vec::new();
        let Ok(entries) = std::fs::read_dir("/proc") else {
            return found;
        };
        for entry in entries.flatten() {
            let Ok(name) = entry.file_name().into_string() else {
                continue;
            };
            let Ok(pid) = name.parse::<u32>() else {
                continue;
            };
            if let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) {
                let rest = match stat.rsplit_once(')') {
                    Some((_, rest)) => rest.trim_start(),
                    None => continue,
                };
                let pgrp = rest.split_whitespace().nth(2);
                if pgrp == Some(pgid.as_raw().to_string().as_str()) {
                    found.push(pid);
                }
            }
        }
        found
    }

    /// A PoolWorker wrapping a live PtySpawner child, ready for
    /// destroy_worker (hook_installer is None — the temp dir is not part of
    /// these contracts).
    fn worker_from(spawner: crate::pty::PtySpawner) -> (PoolWorker, nix::unistd::Pid) {
        let pid = spawner.child_pid;
        let worker = PoolWorker {
            id: "destroy-test".to_string(),
            state: WorkerState::Ready,
            master_fd: spawner.master.into_raw_fd(),
            child_pid: pid,
            state_since: Instant::now(),
            hook_installer: None,
        };
        (worker, pid)
    }

    fn spawn_pty(path: &std::path::Path, args: &[&str]) -> crate::pty::PtySpawner {
        let cmd = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).unwrap();
        let args: Vec<std::ffi::CString> = args
            .iter()
            .map(|a| std::ffi::CString::new(*a).unwrap())
            .collect();
        crate::pty::PtySpawner::spawn(&cmd, &args).expect("PtySpawner::spawn")
    }

    /// A SIGTERM-compliant child (dies on the default disposition) must be
    /// reaped promptly: its exit is observed, it burns none of the grace
    /// period, and nothing — leaked or zombie — is left in /proc.
    #[test]
    fn destroy_worker_reaps_a_sigterm_compliant_child() {
        let manager = PoolManager::new(1, std::path::PathBuf::from("claude"), false);
        let sleep = which::which("sleep").expect("'sleep' on PATH");
        let spawner = spawn_pty(&sleep, &["30"]);
        let (worker, pid) = worker_from(spawner);

        let start = Instant::now();
        manager.destroy_worker(worker);
        let elapsed = start.elapsed();

        assert!(
            elapsed < DESTROY_GRACE,
            "a SIGTERM-compliant child must not burn the grace period, took {elapsed:?}"
        );
        assert_proc_gone(pid, Duration::from_secs(2));
    }

    /// A child that ignores SIGTERM (and the SIGHUP from the master close)
    /// must be ended by the SIGKILL escalation — after the full grace period,
    /// with its exit still observed — and any descendants it left in its
    /// process group must go too: the group is swept, so the daemon leaves no
    /// surviving descendants behind.
    #[test]
    fn destroy_worker_escalates_to_sigkill_and_sweeps_surviving_descendants() {
        let manager = PoolManager::new(1, std::path::PathBuf::from("claude"), false);
        let sh = which::which("sh").expect("'sh' on PATH");
        let dir = tempfile::tempdir().unwrap();
        let sentinel = dir.path().join("traps-installed");

        // trap first, so the sentinel proves the dispositions are live; the
        // sleep respawn loop keeps a descendant in the group at all times.
        let script = format!(
            "trap \"\" TERM HUP; : > {}; while true; do sleep 5 & wait $!; done",
            sentinel.display()
        );
        let spawner = spawn_pty(&sh, &["-c", &script]);
        let (worker, pid) = worker_from(spawner);

        // Deterministic start: only destroy once the traps are installed, so
        // the test cannot race the child's pre-trap window.
        let start = Instant::now();
        while !sentinel.exists() {
            assert!(
                start.elapsed() < Duration::from_secs(10),
                "child never installed its traps"
            );
            std::thread::sleep(Duration::from_millis(10));
        }

        let start = Instant::now();
        manager.destroy_worker(worker);
        let elapsed = start.elapsed();

        assert!(
            elapsed >= DESTROY_GRACE,
            "a SIGTERM-immune child must be held for the full grace before \
             SIGKILL, torn down in {elapsed:?}"
        );
        assert_proc_gone(pid, Duration::from_secs(2));

        // No descendant may survive in the worker's group. The SIGKILLed
        // sleep is orphaned to init and reaped there within moments, so a
        // short poll absorbs only that reap.
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let members = group_members(pid);
            if members.is_empty() {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "the worker's process group still has surviving members after \
                 teardown: {members:?}"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    // ── manager state machine vs. the shutdown flag (claudepr-1feb2d1b) ──────
    //
    // The flag-side contracts of the shutdown chain, pinned at the level where
    // they are cheapest to fail loudly: once shutdown is requested the pool
    // must hand out nothing and respawn nothing — every respawn would race
    // `shutdown_all`'s reaper. The e2e suite pins the observable half (the
    // "Spawning worker" count never rises after the signal); these pins hold
    // the manager's own entry points to it directly.

    /// A PoolWorker record with no real process behind it — enough for the
    /// manager's bookkeeping contracts (acquire/release state transitions),
    /// which never touch the fd or the pid. The *reaping* contracts use
    /// [`worker_from`], which wraps a live PTY child instead.
    fn record_worker(id: &str, state: WorkerState) -> PoolWorker {
        PoolWorker {
            id: id.to_string(),
            state,
            master_fd: -1,
            child_pid: nix::unistd::Pid::from_raw(0),
            state_since: Instant::now(),
            hook_installer: None,
        }
    }

    /// Acquire hands out exactly one Ready worker and marks it InUse; the next
    /// acquire finds nothing Ready and answers pool_full rather than re-issuing
    /// the same worker to a second client.
    #[test]
    fn acquire_worker_marks_the_worker_in_use_and_reports_pool_full_when_empty() {
        let mut manager = PoolManager::new(1, std::path::PathBuf::from("claude"), false);
        manager
            .workers
            .insert("w1".to_string(), record_worker("w1", WorkerState::Ready));

        let id = manager
            .acquire_worker()
            .expect("a Ready worker must be acquirable");
        assert_eq!(id, "w1");
        assert_eq!(
            manager.workers["w1"].state,
            WorkerState::InUse,
            "an acquired worker must be marked InUse"
        );

        let err = manager.acquire_worker().unwrap_err();
        assert!(
            matches!(
                err,
                PoolResponse::Error {
                    code: ErrorCode::PoolFull,
                    ..
                }
            ),
            "no second assignment may come from one Ready worker: {err:?}"
        );
    }

    /// Once the shutdown flag is set, acquire_worker refuses new assignments —
    /// a client racing the teardown must not pull a worker out of the pool
    /// that `shutdown_all` is about to reap — and the worker it refused stays
    /// untouched.
    #[test]
    fn acquire_worker_refuses_assignments_after_shutdown_is_requested() {
        let mut manager = PoolManager::new(1, std::path::PathBuf::from("claude"), false);
        manager
            .workers
            .insert("w1".to_string(), record_worker("w1", WorkerState::Ready));

        manager.shutdown();

        let err = manager.acquire_worker().unwrap_err();
        assert!(
            matches!(
                err,
                PoolResponse::Error {
                    code: ErrorCode::ShuttingDown,
                    ..
                }
            ),
            "acquire after shutdown must answer shutting_down: {err:?}"
        );
        assert_eq!(
            manager.workers["w1"].state,
            WorkerState::Ready,
            "the refused assignment must not disturb the worker"
        );
    }

    /// maintain() is a no-op once shutdown is requested: an empty, below-target
    /// pool stays empty. Pre-repair, a respawn here would fork fresh workers
    /// directly into the path of the reaper.
    #[test]
    fn maintain_spawns_nothing_once_shutdown_is_requested() {
        let mut manager = PoolManager::new(2, std::path::PathBuf::from("claude"), false);
        manager.shutdown();

        manager
            .maintain()
            .expect("maintain under shutdown must be a clean no-op, not an error");

        assert!(
            manager.workers.is_empty(),
            "no worker may be spawned after shutdown is requested"
        );
    }

    /// release_worker destroys the worker — a real PTY child is reaped, not
    /// merely dropped — removes it from the pool, and answers invalid_worker_id
    /// for an id the pool does not hold.
    #[test]
    fn release_worker_reaps_the_worker_and_rejects_unknown_ids() {
        let mut manager = PoolManager::new(1, std::path::PathBuf::from("claude"), false);
        let sleep = which::which("sleep").expect("'sleep' on PATH");
        let spawner = spawn_pty(&sleep, &["30"]);
        let (worker, pid) = worker_from(spawner);
        manager.workers.insert(worker.id.clone(), worker);

        let err = manager.release_worker("no-such-worker").unwrap_err();
        assert!(
            matches!(
                err,
                PoolResponse::Error {
                    code: ErrorCode::InvalidWorkerId,
                    ..
                }
            ),
            "an unknown id must be rejected, not ignored: {err:?}"
        );

        manager
            .release_worker("destroy-test")
            .expect("a held worker id must release");
        assert_proc_gone(pid, Duration::from_secs(2));
        assert!(
            !manager.workers.contains_key("destroy-test"),
            "a released worker must leave the pool"
        );
    }

    // ── listener teardown + socket-removal ownership (claudepr-ffaf4def) ─────

    /// A server whose accept loop ends immediately (shutdown preset before
    /// run): binds, returns at once, and never spawns a worker.
    fn bind_then_stop(server: &mut PoolServer) {
        server.manager_mut().shutdown();
        server
            .run()
            .expect("bind with shutdown preset must bind and return at once");
    }

    fn quiet_server(path: &std::path::Path) -> PoolServer {
        PoolServer::new(
            Some(path.to_string_lossy().into_owned()),
            PoolManager::new(1, std::path::PathBuf::from("claude"), false),
            false,
        )
    }

    // When run() returns, the daemon must have stopped accepting: the
    // listener fd is closed even though the socket *file* is still there, so
    // a connection is refused rather than queued into a daemon that is
    // leaving. (Pre-repair, the listener stayed open until process exit.)
    #[test]
    fn run_closes_the_listener_when_the_accept_loop_ends() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pool.sock");
        let mut server = quiet_server(&path);

        bind_then_stop(&mut server);
        assert!(path.exists(), "bind creates the socket file");

        let err = std::os::unix::net::UnixStream::connect(&path).unwrap_err();
        assert_eq!(
            err.raw_os_error(),
            Some(libc::ECONNREFUSED),
            "a returned server must not accept connections: {err}"
        );
    }

    // The daemon removes the socket it created.
    #[test]
    fn cleanup_removes_the_socket_the_daemon_created() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pool.sock");
        let mut server = quiet_server(&path);

        bind_then_stop(&mut server);
        assert!(path.exists());

        server.cleanup();
        assert!(!path.exists(), "cleanup must remove the daemon's socket");
    }

    // The heart of the ownership contract: if the path was replaced while the
    // daemon ran (another daemon, an admin, a test), cleanup must leave the
    // replacement exactly as found — the daemon holds its socket by inode,
    // and the path's current occupant is somebody else's file.
    #[test]
    fn cleanup_leaves_a_replacement_file_at_the_path_alone() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pool.sock");
        let mut server = quiet_server(&path);

        bind_then_stop(&mut server);

        std::fs::remove_file(&path).expect("remove the daemon's socket");
        std::fs::write(&path, b"not the daemon's file").expect("write replacement");

        server.cleanup();

        assert_eq!(
            std::fs::read(&path).expect("replacement file must survive cleanup"),
            b"not the daemon's file",
            "cleanup deleted a file the daemon never created"
        );
    }

    // A daemon that never bound has no socket identity — and must not
    // "clean up" a pre-existing file at the configured path either.
    #[test]
    fn cleanup_without_a_bind_removes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pool.sock");
        std::fs::write(&path, b"pre-existing").unwrap();

        let server = quiet_server(&path);
        server.cleanup();

        assert_eq!(
            std::fs::read(&path).expect("pre-existing file must survive"),
            b"pre-existing",
            "cleanup without a bind must be a no-op"
        );
    }

    // A socket already removed before cleanup must not trip an error.
    #[test]
    fn cleanup_tolerates_an_already_gone_socket() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pool.sock");
        let mut server = quiet_server(&path);

        bind_then_stop(&mut server);
        std::fs::remove_file(&path).unwrap();

        server.cleanup();
        assert!(!path.exists());
    }

    // ---- client (PoolClient::acquire) ----

    /// Serve exactly `connections` sequential connections from a background
    /// thread, handing each to `handler`. Returns once the thread is parked;
    /// the listener (and its socket file, inside `dir`) dies with the test.
    fn fake_daemon(
        path: &std::path::Path,
        connections: usize,
        handler: impl Fn(std::os::unix::net::UnixStream) + Send + 'static,
    ) {
        let listener = bind_socket(path).unwrap();
        std::thread::spawn(move || {
            for _ in 0..connections {
                match listener.accept() {
                    Ok((stream, _)) => handler(stream),
                    Err(_) => break,
                }
            }
        });
    }

    fn write_frame(stream: &mut std::os::unix::net::UnixStream, payload: &[u8]) {
        stream
            .write_all(&(payload.len() as u32).to_be_bytes())
            .unwrap();
        stream.write_all(payload).unwrap();
    }

    #[test]
    fn worker_assigned_parses_with_and_without_the_extended_fields() {
        let full: PoolResponse = serde_json::from_str(
            r#"{"type":"worker_assigned","worker_id":"w1","message":"ready",
                "stop_fifo":"/tmp/f","pid":99,"cwd":"/srv"}"#,
        )
        .unwrap();
        let PoolResponse::WorkerAssigned {
            worker_id,
            stop_fifo,
            pid,
            cwd,
            ..
        } = full
        else {
            panic!("wrong variant");
        };
        assert_eq!(worker_id, "w1");
        assert_eq!(stop_fifo, "/tmp/f");
        assert_eq!(pid, 99);
        assert_eq!(cwd, "/srv");

        // A pre-fifo daemon's frame carries only the original two fields.
        let legacy: PoolResponse = serde_json::from_str(
            r#"{"type":"worker_assigned","worker_id":"w2","message":"ready"}"#,
        )
        .unwrap();
        let PoolResponse::WorkerAssigned {
            stop_fifo,
            pid,
            cwd,
            ..
        } = legacy
        else {
            panic!("wrong variant");
        };
        assert_eq!(stop_fifo, "");
        assert_eq!(pid, 0);
        assert_eq!(cwd, "");
    }

    #[test]
    fn from_parts_rejects_a_legacy_assignment_missing_any_field() {
        // Any live fd works — validation rejects the frame before the
        // descriptor is ever used, and drop closes it.
        let (probe, _probe_keep) = stream_pair();
        let master: OwnedFd = unsafe { OwnedFd::from_raw_fd(probe.into_raw_fd()) };
        let err = AcquiredWorker::from_parts(
            "w1".to_string(),
            String::new(),
            99,
            "/srv".to_string(),
            master,
            std::path::PathBuf::from("/tmp/x.sock"),
        )
        .unwrap_err();
        assert!(err.contains("stop_fifo"), "unexpected error: {err}");
    }

    #[test]
    fn send_and_recv_roundtrip_a_real_descriptor() {
        // `a` sends the control message, `b` receives it; (p1, p2) is the
        // descriptor under transfer. Write on p2, read through the received
        // fd — proves recv recovered the same open file description, not
        // just some fd.
        let (a, mut b) = stream_pair();
        let (p1, mut p2) = stream_pair();

        PoolServer::send_fd(&a, p1.as_raw_fd()).unwrap();
        drop(p1); // the receiver's copy must keep the description alive

        let deadline = Instant::now() + Duration::from_secs(5);
        b.set_nonblocking(true).unwrap();
        let received = recv_fd_bounded(&mut b, deadline).unwrap();
        let mut received_stream = std::os::unix::net::UnixStream::from(received);
        assert!(received_stream.as_raw_fd() >= 0);

        p2.write_all(b"pong").unwrap();

        let mut buf = [0u8; 4];
        received_stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        received_stream.read_exact(&mut buf).unwrap();
        assert_eq!(&buf, b"pong");
    }

    #[test]
    fn acquire_reports_unreachable_for_a_missing_socket() {
        let dir = tempfile::tempdir().unwrap();
        let client = PoolClient::new(dir.path().join("absent.sock"));

        let failure = client.acquire(2).unwrap_err();
        assert!(matches!(failure, AcquireFailure::Unreachable { .. }));
        assert!(is_stateless_fallback(&failure));
    }

    #[test]
    fn acquire_reports_unreachable_for_a_stale_regular_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("stale.sock");
        std::fs::write(&path, b"not a socket").unwrap();

        let failure = PoolClient::new(path).acquire(2).unwrap_err();
        assert!(matches!(failure, AcquireFailure::Unreachable { .. }));
        assert!(is_stateless_fallback(&failure));
    }

    #[test]
    fn acquire_reports_pool_unavailable_on_an_error_frame() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pool.sock");
        fake_daemon(&path, 1, |stream| {
            let mut stream = stream;
            let request = PoolServer::read_frame(&mut stream).unwrap().unwrap();
            let _request: PoolRequest = serde_json::from_slice(&request).unwrap();
            let response = PoolResponse::Error {
                error: "all workers busy".to_string(),
                code: ErrorCode::PoolFull,
            };
            write_frame(&mut stream, &serde_json::to_vec(&response).unwrap());
        });

        let failure = PoolClient::new(path).acquire(5).unwrap_err();
        assert_eq!(
            failure,
            AcquireFailure::PoolUnavailable {
                code: "pool_full".to_string(),
                error: "all workers busy".to_string(),
            }
        );
        assert!(is_stateless_fallback(&failure));
    }

    #[test]
    fn acquire_yields_protocol_failure_on_garbage_response() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pool.sock");
        fake_daemon(&path, 1, |stream| {
            let mut stream = stream;
            let _ = PoolServer::read_frame(&mut stream).unwrap().unwrap();
            write_frame(&mut stream, b"\x00\xff not json");
        });

        let failure = PoolClient::new(path).acquire(5).unwrap_err();
        assert!(
            matches!(failure, AcquireFailure::Protocol(_)),
            "{failure:?}"
        );
        assert!(!is_stateless_fallback(&failure));
    }

    #[test]
    fn acquire_yields_protocol_failure_within_the_caller_deadline_when_silent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pool.sock");
        fake_daemon(&path, 1, |stream| {
            let mut stream = stream;
            // Read the request, then say nothing — the wedged-daemon case.
            let _ = PoolServer::read_frame(&mut stream).unwrap().unwrap();
            std::thread::sleep(Duration::from_secs(30));
        });

        let started = Instant::now();
        let failure = PoolClient::new(path).acquire(2).unwrap_err();
        let elapsed = started.elapsed();
        assert!(matches!(failure, AcquireFailure::Protocol(ref m) if m.contains("timed out")));
        assert!(
            elapsed < Duration::from_secs(10),
            "acquire must honor the caller deadline, took {elapsed:?}"
        );
    }

    #[test]
    fn acquire_rejects_a_legacy_assignment_without_a_stop_fifo() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pool.sock");
        fake_daemon(&path, 1, |stream| {
            let mut stream = stream;
            let _ = PoolServer::read_frame(&mut stream).unwrap().unwrap();
            let response = PoolResponse::WorkerAssigned {
                worker_id: "w-old".to_string(),
                message: "Worker ready".to_string(),
                stop_fifo: String::new(),
                pid: 0,
                cwd: String::new(),
            };
            write_frame(&mut stream, &serde_json::to_vec(&response).unwrap());
            let (_pty, probe) = stream_pair();
            PoolServer::send_fd(&stream, probe.into_raw_fd()).unwrap();
        });

        let failure = PoolClient::new(path).acquire(5).unwrap_err();
        assert!(
            matches!(failure, AcquireFailure::Protocol(ref m) if m.contains("stop_fifo")),
            "{failure:?}"
        );
    }

    #[test]
    fn acquire_rejects_a_response_length_prefix_over_the_cap() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pool.sock");
        fake_daemon(&path, 1, |stream| {
            let mut stream = stream;
            let _ = PoolServer::read_frame(&mut stream).unwrap().unwrap();
            // 1 GiB length prefix: must be refused before any allocation.
            stream.write_all(&0x4000_0000u32.to_be_bytes()).unwrap();
        });

        let failure = PoolClient::new(path).acquire(5).unwrap_err();
        assert!(
            matches!(failure, AcquireFailure::Protocol(ref m) if m.contains("maximum")),
            "{failure:?}"
        );
    }

    #[test]
    fn acquire_and_release_speak_the_documented_wire_format() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pool.sock");

        let mode = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mode_for_handler = mode.clone();
        fake_daemon(&path, 2, move |stream| {
            let mut stream = stream;
            let request = PoolServer::read_frame(&mut stream).unwrap().unwrap();
            match mode_for_handler.fetch_add(1, Ordering::SeqCst) {
                0 => {
                    let _request: PoolRequest = serde_json::from_slice(&request).unwrap();
                    let response = PoolResponse::WorkerAssigned {
                        worker_id: "w-1".to_string(),
                        message: "Worker ready".to_string(),
                        stop_fifo: "/tmp/stop.fifo".to_string(),
                        pid: 4321,
                        cwd: "/srv/daemon".to_string(),
                    };
                    write_frame(&mut stream, &serde_json::to_vec(&response).unwrap());
                    let (_pty, probe) = stream_pair();
                    PoolServer::send_fd(&stream, probe.into_raw_fd()).unwrap();
                }
                _ => {
                    // Release connection: assert the frame, then echo it back
                    // as the reply so the client's bounded read completes.
                    let request: PoolRequest = serde_json::from_slice(&request).unwrap();
                    assert!(
                        matches!(request, PoolRequest::Release { ref worker_id } if worker_id == "w-1")
                    );
                    write_frame(&mut stream, &serde_json::to_vec(&request).unwrap());
                }
            }
        });

        let mut worker = PoolClient::new(path).acquire(5).unwrap();
        assert_eq!(worker.worker_id(), "w-1");
        assert_eq!(worker.pid(), nix::unistd::Pid::from_raw(4321));
        assert_eq!(worker.stop_fifo(), std::path::Path::new("/tmp/stop.fifo"));
        assert_eq!(worker.worker_cwd(), std::path::Path::new("/srv/daemon"));
        assert!(worker.master_fd() >= 0);
        assert!(!worker.is_released());

        worker.release();
        assert!(worker.is_released());
        // Idempotent: a second release must not re-send or error.
        worker.release();
    }

    #[test]
    fn dropping_a_worker_releases_it_implicitly() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pool.sock");

        let released = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let released_for_handler = released.clone();
        fake_daemon(&path, 2, move |stream| {
            let mut stream = stream;
            let request = PoolServer::read_frame(&mut stream).unwrap().unwrap();
            match serde_json::from_slice::<PoolRequest>(&request).unwrap() {
                PoolRequest::Acquire { .. } => {
                    let response = PoolResponse::WorkerAssigned {
                        worker_id: "w-drop".to_string(),
                        message: "Worker ready".to_string(),
                        stop_fifo: "/tmp/stop.fifo".to_string(),
                        pid: 7,
                        cwd: "/srv".to_string(),
                    };
                    write_frame(&mut stream, &serde_json::to_vec(&response).unwrap());
                    let (_pty, probe) = stream_pair();
                    PoolServer::send_fd(&stream, probe.into_raw_fd()).unwrap();
                }
                PoolRequest::Release { worker_id } => {
                    assert_eq!(worker_id, "w-drop");
                    released_for_handler.store(true, Ordering::SeqCst);
                    write_frame(&mut stream, &request);
                }
            }
        });

        {
            let _worker = PoolClient::new(path).acquire(5).unwrap();
            // Dropped here without an explicit release.
        }
        assert!(released.load(Ordering::SeqCst));
    }

    // ---- invocation-path acquisition (acquire_for_invocation) ----

    #[test]
    fn invocation_acquisition_is_not_requested_without_a_socket() {
        // The flag-absent hot path: classified before any client exists, so
        // no pool code runs and nothing is owed.
        match acquire_for_invocation(None, 3600).unwrap() {
            InvocationAcquisition::NotRequested => {}
            other => panic!("no --pool-socket must stay off the pool path: {other:?}"),
        }
    }

    #[test]
    fn invocation_acquisition_falls_back_on_an_absent_socket() {
        let dir = tempfile::tempdir().unwrap();
        let outcome = acquire_for_invocation(Some(&dir.path().join("absent.sock")), 60).unwrap();
        match outcome {
            InvocationAcquisition::Fallback(ref failure) => {
                assert!(matches!(failure, AcquireFailure::Unreachable { .. }));
                assert!(is_stateless_fallback(failure));
            }
            other => panic!("absent socket must classify as fallback, got {other:?}"),
        }
    }

    #[test]
    fn invocation_acquisition_falls_back_on_an_unreachable_socket() {
        // A real socket file whose listener is gone: connect answers
        // ECONNREFUSED — the daemon-died-and-left-the-file case, distinct
        // from the socket never existing at all.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("stale.sock");
        let listener = bind_socket(&path).unwrap();
        drop(listener);

        match acquire_for_invocation(Some(&path), 60).unwrap() {
            InvocationAcquisition::Fallback(ref failure) => {
                assert!(matches!(failure, AcquireFailure::Unreachable { .. }));
                assert!(is_stateless_fallback(failure));
            }
            other => panic!("unreachable socket must classify as fallback, got {other:?}"),
        }
    }

    #[test]
    fn invocation_acquisition_falls_back_when_the_pool_reports_it_busy() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pool.sock");
        fake_daemon(&path, 1, |stream| {
            let mut stream = stream;
            let _ = PoolServer::read_frame(&mut stream).unwrap().unwrap();
            let response = PoolResponse::Error {
                error: "Pool full - no ready workers".to_string(),
                code: ErrorCode::PoolFull,
            };
            write_frame(&mut stream, &serde_json::to_vec(&response).unwrap());
        });

        match acquire_for_invocation(Some(&path), 30).unwrap() {
            InvocationAcquisition::Fallback(failure) => {
                assert_eq!(
                    failure,
                    AcquireFailure::PoolUnavailable {
                        code: "pool_full".to_string(),
                        error: "Pool full - no ready workers".to_string(),
                    }
                );
            }
            other => panic!("pool_full must classify as fallback, got {other:?}"),
        }
    }

    #[test]
    fn invocation_acquisition_hard_errors_on_a_malformed_response() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pool.sock");
        fake_daemon(&path, 1, |stream| {
            let mut stream = stream;
            let _ = PoolServer::read_frame(&mut stream).unwrap().unwrap();
            write_frame(&mut stream, b"\x00\xff garbage");
        });

        let failure = acquire_for_invocation(Some(&path), 5).unwrap_err();
        assert!(
            matches!(failure, AcquireFailure::Protocol(_)),
            "{failure:?}"
        );
        assert!(!is_stateless_fallback(&failure));
    }

    #[test]
    fn invocation_acquisition_hard_errors_within_the_caller_budget_when_silent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pool.sock");
        fake_daemon(&path, 1, |stream| {
            let mut stream = stream;
            // Read the request, then say nothing — the wedged-daemon case.
            let _ = PoolServer::read_frame(&mut stream).unwrap().unwrap();
            std::thread::sleep(Duration::from_secs(30));
        });

        // A caller budget below the 60s default also proves the budget is
        // the caller's, not the compiled-in cap.
        let started = Instant::now();
        let failure = acquire_for_invocation(Some(&path), 2).unwrap_err();
        assert!(
            matches!(failure, AcquireFailure::Protocol(ref m) if m.contains("timed out")),
            "{failure:?}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "acquire must honor the caller budget, took {:?}",
            started.elapsed()
        );
    }

    #[test]
    fn invocation_acquisition_sends_the_capped_budget_to_the_pool() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pool.sock");
        fake_daemon(&path, 1, |stream| {
            let mut stream = stream;
            let request = PoolServer::read_frame(&mut stream).unwrap().unwrap();
            let request: PoolRequest = serde_json::from_slice(&request).unwrap();
            // The wire carries min(DEFAULT_ACQUIRE_TIMEOUT_SECS, caller), so
            // the daemon-side wait cannot outlive the invocation's budget.
            assert!(
                matches!(request, PoolRequest::Acquire { timeout_secs: 5 }),
                "expected the capped budget on the wire, got {request:?}"
            );
            let response = PoolResponse::Error {
                error: "shutting down".to_string(),
                code: ErrorCode::ShuttingDown,
            };
            write_frame(&mut stream, &serde_json::to_vec(&response).unwrap());
        });

        // Caller budget 5 < default 60: the cap applied, and the (fallback
        // classified) shutting_down error still resolves to Ok.
        assert!(matches!(
            acquire_for_invocation(Some(&path), 5).unwrap(),
            InvocationAcquisition::Fallback(_)
        ));
    }

    #[test]
    fn invocation_acquisition_hands_back_a_worker_the_drop_path_releases_once() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pool.sock");
        let releases = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let releases_for_handler = releases.clone();
        fake_daemon(&path, 2, move |stream| {
            let mut stream = stream;
            let request = PoolServer::read_frame(&mut stream).unwrap().unwrap();
            match serde_json::from_slice::<PoolRequest>(&request).unwrap() {
                PoolRequest::Acquire { .. } => {
                    let response = PoolResponse::WorkerAssigned {
                        worker_id: "w-inv".to_string(),
                        message: "Worker ready".to_string(),
                        stop_fifo: "/tmp/stop.fifo".to_string(),
                        pid: 4242,
                        cwd: "/srv/daemon".to_string(),
                    };
                    write_frame(&mut stream, &serde_json::to_vec(&response).unwrap());
                    let (_pty, probe) = stream_pair();
                    PoolServer::send_fd(&stream, probe.into_raw_fd()).unwrap();
                }
                PoolRequest::Release { .. } => {
                    releases_for_handler.fetch_add(1, Ordering::SeqCst);
                    write_frame(&mut stream, &request);
                }
            }
        });

        let worker = match acquire_for_invocation(Some(&path), 5).unwrap() {
            InvocationAcquisition::Acquired(worker) => worker,
            other => panic!("a ready daemon must hand back a worker, got {other:?}"),
        };
        assert_eq!(worker.worker_id(), "w-inv");

        // Dropping a worker the caller never drove (any exit before the
        // pooled session's explicit release) must send EXACTLY ONE
        // release — the daemon then tears the worker down and spawns a
        // replacement. Zero (leaked assignment) or two (double
        // send) both fail this pin.
        drop(worker);
        let deadline = Instant::now() + Duration::from_secs(5);
        while releases.load(Ordering::SeqCst) == 0 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(releases.load(Ordering::SeqCst), 1);
    }

    // ── pooled session exit paths: the integrated drive (claudepr-8b0e6e78) ──
    //
    // The AcquiredWorker contract — the worker is released exactly once on
    // every exit path that leaves the process alive, and nothing crosses
    // between two callers of the same pool — is exercised here through
    // `Session::run_pooled`, the exact integrated invocation path, against a
    // scripted daemon handing out real per-worker identities (its own
    // freshly-created Stop FIFO per worker, its own PTY-pair end, its own
    // pid). tests/serve.rs pins the same contract through the compiled binary;
    // these pins hold at the level where each exit path can be driven
    // deterministically, with a fake worker on the far side of the PTY pair.

    /// Serializes every in-process session drive and every process-directed
    /// signal in this binary.
    ///
    /// `Session::run_pooled` installs process-global SIGINT/SIGTERM handlers
    /// for the drive and its `SignalGuard` restores default dispositions at
    /// the end; a SIGINT raised for one test while another test sits outside
    /// that window would kill the whole test binary at default disposition.
    /// The same exclusivity protects `serve_signal_delivery_flips_the_
    /// observable_flag`'s `raise` from landing inside a drive's handler
    /// window (and vice versa). Holding the lock across a whole drive also
    /// keeps `SELF_PIPE_WRITE` — the handler's process-global target —
    /// pointing at the driving test's own pipe.
    static DRIVE_SIGNAL_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Take [`DRIVE_SIGNAL_LOCK`], tolerating a poisoned lock: a panic inside
    /// one drive must not cascade into every later drive.
    fn drive_signal_lock() -> std::sync::MutexGuard<'static, ()> {
        DRIVE_SIGNAL_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// The daemon-side end of a scripted worker's PTY pair: bytes the client
    /// writes to its master arrive here; bytes written here arrive on the
    /// client's master. Dropping it hangs the client's master up — the
    /// `ExitReason::ChildExited` input.
    type WorkerSide = std::os::unix::net::UnixStream;

    /// What one scripted acquire advertised to its caller.
    #[derive(Debug, Clone)]
    struct ScriptedAssignment {
        worker_id: String,
        stop_fifo: std::path::PathBuf,
        pid: u32,
        cwd: std::path::PathBuf,
    }

    #[derive(Debug)]
    struct ScriptedPoolState {
        dir: std::path::PathBuf,
        next_index: usize,
        assigned: Vec<ScriptedAssignment>,
        released: Vec<String>,
        worker_sides: HashMap<String, Option<WorkerSide>>,
    }

    /// A scripted pool daemon for driving whole pooled sessions: every
    /// acquire is answered with one distinct worker (fresh id, its own
    /// newly-created Stop FIFO, its own PTY-pair end, its own pid) and every
    /// release frame is counted by worker id. Serves `2 * workers + 2`
    /// connections — the one-acquire-one-release shape of each driven
    /// invocation plus slack, so a DOUBLE release is still served and counted
    /// by the assertion instead of dying on a closed listener.
    struct ScriptedPool {
        socket: std::path::PathBuf,
        state: Arc<Mutex<ScriptedPoolState>>,
    }

    impl ScriptedPool {
        fn assignment_of(&self, worker_id: &str) -> ScriptedAssignment {
            self.state
                .lock()
                .unwrap()
                .assigned
                .iter()
                .find(|a| a.worker_id == worker_id)
                .cloned()
                .unwrap_or_else(|| panic!("no scripted assignment for {worker_id}"))
        }

        fn take_worker_side(&self, worker_id: &str) -> WorkerSide {
            self.state
                .lock()
                .unwrap()
                .worker_sides
                .get_mut(worker_id)
                .and_then(|slot| slot.take())
                .unwrap_or_else(|| panic!("no worker side for {worker_id}"))
        }

        fn released(&self) -> Vec<String> {
            self.state.lock().unwrap().released.clone()
        }

        fn release_count(&self) -> usize {
            self.state.lock().unwrap().released.len()
        }
    }

    fn scripted_pool(base: &std::path::Path, workers: usize) -> ScriptedPool {
        let socket = base.join("scripted.sock");
        let listener = bind_socket(&socket).unwrap();
        let state = Arc::new(Mutex::new(ScriptedPoolState {
            dir: base.to_path_buf(),
            next_index: 0,
            assigned: Vec::new(),
            released: Vec::new(),
            worker_sides: HashMap::new(),
        }));
        let state_for_thread = state.clone();
        std::thread::spawn(move || {
            for _ in 0..(workers * 2 + 2) {
                let Ok((stream, _)) = listener.accept() else {
                    break;
                };
                let mut stream = stream;
                let frame = match PoolServer::read_frame(&mut stream) {
                    Ok(Some(frame)) => frame,
                    _ => break,
                };
                let request: PoolRequest = match serde_json::from_slice(&frame) {
                    Ok(request) => request,
                    Err(_) => break,
                };
                match request {
                    PoolRequest::Acquire { .. } => {
                        let assignment = {
                            let mut st = state_for_thread.lock().unwrap();
                            let idx = st.next_index;
                            st.next_index += 1;
                            let stop_fifo = st.dir.join(format!("stop-{idx}.fifo"));
                            nix::unistd::mkfifo(
                                &stop_fifo,
                                nix::sys::stat::Mode::from_bits_truncate(0o600),
                            )
                            .expect("create the scripted worker's Stop FIFO");
                            let assignment = ScriptedAssignment {
                                worker_id: format!("w-script-{idx}"),
                                stop_fifo,
                                pid: 4000 + idx as u32,
                                cwd: st.dir.clone(),
                            };
                            st.assigned.push(assignment.clone());
                            assignment
                        };
                        let (client_side, worker_side) = stream_pair();
                        state_for_thread
                            .lock()
                            .unwrap()
                            .worker_sides
                            .insert(assignment.worker_id.clone(), Some(worker_side));
                        let response = PoolResponse::WorkerAssigned {
                            worker_id: assignment.worker_id.clone(),
                            message: "Worker ready".to_string(),
                            stop_fifo: assignment.stop_fifo.to_string_lossy().into_owned(),
                            pid: assignment.pid,
                            cwd: assignment.cwd.to_string_lossy().into_owned(),
                        };
                        write_frame(&mut stream, &serde_json::to_vec(&response).unwrap());
                        PoolServer::send_fd(&stream, client_side.as_raw_fd()).unwrap();
                        // `client_side` drops here: the sent copy is the
                        // client's master fd; nothing else holds it.
                    }
                    PoolRequest::Release { worker_id } => {
                        state_for_thread.lock().unwrap().released.push(worker_id);
                        // Echo the frame so the client's bounded read
                        // completes; the content is informational.
                        write_frame(&mut stream, &frame);
                    }
                }
            }
        });
        ScriptedPool { socket, state }
    }

    /// What the scripted worker does once it has observed its caller's
    /// injected prompt (the worker-side behaviour per scenario).
    enum WorkerScript {
        /// Deliver this Stop payload on this worker's own Stop FIFO.
        Stop(String),
        /// Hang up the PTY pair: the worker died without a Stop payload.
        HangUp,
        /// Hold the PTY open and do nothing (deadline/signal scenarios).
        Hold,
        /// Run this after the prompt is observed (signal scenarios).
        Custom(Box<dyn FnOnce() + Send>),
    }

    /// Spawn the worker-side half of a scripted session: wait for the
    /// client's bracketed-paste prompt on the worker's PTY-pair end (recording
    /// everything seen into `prompt_sink`), then run `script`. The PTY end is
    /// held open for as long as the scenario needs it; the thread detaches.
    fn drive_worker(
        worker_side: WorkerSide,
        stop_fifo: std::path::PathBuf,
        script: WorkerScript,
        prompt_sink: Arc<Mutex<Vec<u8>>>,
    ) {
        std::thread::spawn(move || {
            let mut worker_side = worker_side;
            let _ = worker_side.set_read_timeout(Some(Duration::from_millis(100)));
            let mut seen: Vec<u8> = Vec::new();
            let deadline = Instant::now() + Duration::from_secs(20);
            // The bracketed-paste end marker is what the session traces as
            // "prompt injected"; scan with a window as long as the marker
            // itself (a shorter window can never equal it).
            while !seen.windows(b"\x1b[201~".len()).any(|w| w == b"\x1b[201~") {
                assert!(
                    Instant::now() < deadline,
                    "the client never injected its prompt; saw {} bytes so far",
                    seen.len()
                );
                let mut buf = [0u8; 4096];
                match worker_side.read(&mut buf) {
                    Ok(0) => panic!("worker side saw EOF before the prompt was injected"),
                    Ok(n) => {
                        seen.extend_from_slice(&buf[..n]);
                        prompt_sink.lock().unwrap().extend_from_slice(&buf[..n]);
                    }
                    Err(e)
                        if e.kind() == std::io::ErrorKind::WouldBlock
                            || e.kind() == std::io::ErrorKind::TimedOut =>
                    {
                        continue;
                    }
                    Err(e) => panic!("worker side read failed: {e}"),
                }
            }
            match script {
                WorkerScript::Stop(payload) => {
                    deliver_stop(&stop_fifo, &payload);
                    hold_pty_open(worker_side);
                }
                WorkerScript::HangUp => drop(worker_side),
                WorkerScript::Hold => hold_pty_open(worker_side),
                WorkerScript::Custom(after) => {
                    after();
                    hold_pty_open(worker_side);
                }
            }
        });
    }

    /// Keep the scripted worker's PTY end open past the scenario (a pending
    /// deadline or signal wins over a hang-up only while the hang-up never
    /// comes). Detached: the test process exits long before this sleeps out.
    fn hold_pty_open(side: WorkerSide) {
        std::thread::sleep(Duration::from_secs(10));
        drop(side);
    }

    /// Deliver a Stop payload on `stop_fifo` (the client opens the read end
    /// at session start, before the prompt is injected, so the writer's open
    /// never blocks once the drive has begun).
    fn deliver_stop(stop_fifo: &std::path::Path, payload: &str) {
        let mut w = std::fs::OpenOptions::new()
            .write(true)
            .open(stop_fifo)
            .expect("the client holds the Stop FIFO's read end open");
        w.write_all(payload.as_bytes())
            .expect("write the Stop payload");
        // Dropping the writer ends the payload: the client's bounded FIFO
        // read sees EOF after the complete line, exactly as the hook writes.
    }

    /// A transcript JSONL in the mock-claude shape: one assistant event (with
    /// non-zero usage, so transcript-sourced capture is distinguishable from
    /// the payload fallback) and one result event naming the session.
    fn write_transcript(path: &std::path::Path, answer: &str, session: &str) {
        let assistant = format!(
            "{{\"type\":\"assistant\",\"message\":{{\"id\":\"msg_{session}\",\
             \"content\":[{{\"type\":\"text\",\"text\":\"{answer}\"}}],\
             \"usage\":{{\"input_tokens\":10,\"output_tokens\":25,\
             \"cache_creation_input_tokens\":5,\"cache_read_input_tokens\":15}}}}}}"
        );
        let result =
            format!("{{\"type\":\"result\",\"session_id\":\"{session}\",\"is_error\":false}}");
        std::fs::write(path, format!("{assistant}\n{result}\n")).unwrap();
    }

    /// The Stop payload advertising `session` and its transcript.
    fn stop_payload(session: &str, transcript: &std::path::Path, answer: &str) -> String {
        format!(
            "{{\"hook_event_name\":\"Stop\",\"session_id\":\"{session}\",\
             \"transcript_path\":\"{path}\",\"cwd\":\"{cwd}\",\
             \"last_assistant_message\":\"{answer}\"}}\n",
            path = transcript.display(),
            cwd = transcript
                .parent()
                .unwrap_or(std::path::Path::new("/"))
                .display(),
        )
    }

    /// A real executable for the version probe `run_pooled` runs at entry
    /// (`claude_bin --version`); unit tests cannot assume the workspace
    /// binaries are built.
    fn version_probe_bin() -> std::path::PathBuf {
        which::which("bash").expect("bash must be on PATH for the version probe")
    }

    /// Poll until the scripted daemon has seen `n` release frames.
    fn await_release_count(pool: &ScriptedPool, n: usize) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while pool.release_count() < n {
            assert!(
                Instant::now() < deadline,
                "only {} of {n} expected releases arrived; released: {:?}",
                pool.release_count(),
                pool.released()
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// After the expected count, no further release may arrive: the explicit
    /// release and the Drop path must not both go over the wire.
    fn assert_release_quiet(pool: &ScriptedPool, n: usize) {
        std::thread::sleep(Duration::from_millis(500));
        assert_eq!(
            pool.release_count(),
            n,
            "unexpected extra release(s); released: {:?}",
            pool.released()
        );
    }

    /// What `/proc/self/fd/<fd>` currently points at, if anything.
    fn fd_target(fd: RawFd) -> Option<std::path::PathBuf> {
        std::fs::read_link(format!("/proc/self/fd/{fd}")).ok()
    }

    /// The master fd the caller owned must be closed by the time `run_pooled`
    /// returned. The number may have been reused by another thread in this
    /// heavily parallel test process — reuse resolves to a DIFFERENT target
    /// and passes; only the original open file description still sitting at
    /// the number is the leak this catches.
    fn assert_master_fd_closed(fd: RawFd, was: &Option<std::path::PathBuf>) {
        match fd_target(fd) {
            None => {} // closed
            Some(now) => assert_ne!(
                &Some(now),
                was,
                "master fd {fd} is still open after run_pooled returned"
            ),
        }
    }

    /// The whole-process environment must still equal the pre-drive snapshot:
    /// the pooled drive sets no variable, so a value a drive leaked between
    /// callers would persist forever. Other tests in this binary (config's
    /// EnvGuard pins) mutate process env on their own threads and restore it
    /// when they finish, so a mismatch is only meaningful if it PERSISTS —
    /// tolerate the transient, fail on what survives the grace window. Only
    /// differing KEY names are named: values are not this test's business.
    fn assert_env_stable(before: &std::collections::BTreeMap<String, String>) {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let now: std::collections::BTreeMap<String, String> = std::env::vars().collect();
            if now == *before {
                return;
            }
            let changed: Vec<String> = before
                .keys()
                .chain(now.keys())
                .filter(|k| before.get(*k) != now.get(*k))
                .cloned()
                .collect();
            assert!(
                Instant::now() < deadline,
                "the pooled drives must not touch the environment, and the \
                 change never settled: changed keys: {changed:?}"
            );
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    #[test]
    fn pooled_session_success_releases_the_worker_exactly_once() {
        let _drive = drive_signal_lock();
        let dir = tempfile::tempdir().unwrap();
        let pool = scripted_pool(dir.path(), 1);

        let transcript = dir.path().join("t1.jsonl");
        write_transcript(&transcript, "answer one", "sess-one");

        let worker = PoolClient::new(pool.socket.clone()).acquire(10).unwrap();
        let assignment = pool.assignment_of(worker.worker_id());
        let master_fd = worker.master_fd();
        let master_target = fd_target(master_fd);
        let prompt_sink = Arc::new(Mutex::new(Vec::new()));
        drive_worker(
            pool.take_worker_side(worker.worker_id()),
            assignment.stop_fifo.clone(),
            WorkerScript::Stop(stop_payload("sess-one", &transcript, "answer one")),
            prompt_sink.clone(),
        );

        let result = crate::session::Session::run_pooled(
            &version_probe_bin(),
            worker,
            b"prompt one".to_vec(),
            Some(60),
            Some(0),
            None,
            Some(10),
            crate::cli::OutputFormat::Text,
            true,
            false,
        )
        .expect("the driven pooled session must succeed");

        // The answer came from THIS invocation's transcript, read through the
        // shared Stop tail.
        assert!(
            result.transcript.text.contains("answer one"),
            "transcript text: {:?}",
            result.transcript.text
        );
        assert_eq!(result.transcript.session_id.as_deref(), Some("sess-one"));
        assert!(!result.transcript.is_error);
        assert_eq!(result.transcript_path, transcript);

        // Exactly one release — the explicit pre-drain release; the Drop path
        // stayed silent.
        await_release_count(&pool, 1);
        assert_release_quiet(&pool, 1);
        assert_eq!(pool.released(), vec![assignment.worker_id.clone()]);

        // The worker's master fd closed with the run.
        assert_master_fd_closed(master_fd, &master_target);

        // The injected prompt reached THIS worker.
        assert!(
            prompt_sink
                .lock()
                .unwrap()
                .windows(10)
                .any(|w| w == b"prompt one"),
            "the worker must receive its caller's prompt"
        );
    }

    #[test]
    fn pooled_session_child_exit_releases_the_worker_exactly_once() {
        let _drive = drive_signal_lock();
        let dir = tempfile::tempdir().unwrap();
        let pool = scripted_pool(dir.path(), 1);

        let worker = PoolClient::new(pool.socket.clone()).acquire(10).unwrap();
        let assignment = pool.assignment_of(worker.worker_id());
        let prompt_sink = Arc::new(Mutex::new(Vec::new()));
        drive_worker(
            pool.take_worker_side(worker.worker_id()),
            assignment.stop_fifo.clone(),
            WorkerScript::HangUp,
            prompt_sink,
        );

        let err = crate::session::Session::run_pooled(
            &version_probe_bin(),
            worker,
            b"prompt one".to_vec(),
            Some(60),
            Some(0),
            None,
            Some(10),
            crate::cli::OutputFormat::Text,
            false,
            false,
        )
        .expect_err("a worker that dies without a Stop payload is an error");
        assert!(
            matches!(err, crate::error::Error::Internal(ref e)
                if e.to_string().contains("Child exited without sending Stop payload")),
            "unexpected error: {err:?}"
        );

        // The error return dropped the worker → exactly one release.
        await_release_count(&pool, 1);
        assert_release_quiet(&pool, 1);
    }

    #[test]
    fn pooled_session_timeout_releases_the_worker_exactly_once() {
        let _drive = drive_signal_lock();
        let dir = tempfile::tempdir().unwrap();
        let pool = scripted_pool(dir.path(), 1);

        let worker = PoolClient::new(pool.socket.clone()).acquire(10).unwrap();
        let assignment = pool.assignment_of(worker.worker_id());
        let prompt_sink = Arc::new(Mutex::new(Vec::new()));
        drive_worker(
            pool.take_worker_side(worker.worker_id()),
            assignment.stop_fifo.clone(),
            WorkerScript::Hold,
            prompt_sink,
        );

        let started = Instant::now();
        let err = crate::session::Session::run_pooled(
            &version_probe_bin(),
            worker,
            b"prompt one".to_vec(),
            Some(60),
            Some(0),
            None,
            Some(2), // the stop-hook deadline is the one that fires
            crate::cli::OutputFormat::Text,
            false,
            false,
        )
        .expect_err("a worker that never fires Stop must hit the deadline");
        assert!(
            matches!(err, crate::error::Error::Timeout(ref m) if m.contains("Stop hook")),
            "unexpected error: {err:?}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(15),
            "the deadline must end the session promptly, took {:?}",
            started.elapsed()
        );

        // Enforcement rerouted through the daemon: exactly one release, and
        // (via Watchdog::without_child_signals) the worker pid was never
        // signalled client-side — the scripted pid stays a fiction.
        await_release_count(&pool, 1);
        assert_release_quiet(&pool, 1);
    }

    /// The `?`-after-Stop route: a Stop payload that fails to parse returns
    /// from the shared Stop tail BEFORE the explicit pre-drain release is
    /// reached, so this is the one error exit where the release happens via
    /// Drop after the drive already got far enough to attempt it. The
    /// "every `?` / error return" promise needs this shape pinned separately
    /// from the pre-Stop error returns (child exit, timeout, signal).
    #[test]
    fn pooled_session_stop_payload_parse_error_releases_the_worker_exactly_once() {
        let _drive = drive_signal_lock();
        let dir = tempfile::tempdir().unwrap();
        let pool = scripted_pool(dir.path(), 1);

        let worker = PoolClient::new(pool.socket.clone()).acquire(10).unwrap();
        let assignment = pool.assignment_of(worker.worker_id());
        let prompt_sink = Arc::new(Mutex::new(Vec::new()));
        drive_worker(
            pool.take_worker_side(worker.worker_id()),
            assignment.stop_fifo.clone(),
            // Garbage on the FIFO: the payload fires (a real Stop transition
            // from the driver's perspective) but parse_stop_payload rejects it.
            WorkerScript::Stop("not json at all".to_string()),
            prompt_sink,
        );

        let err = crate::session::Session::run_pooled(
            &version_probe_bin(),
            worker,
            b"prompt one".to_vec(),
            Some(60),
            Some(0),
            None,
            Some(10),
            crate::cli::OutputFormat::Text,
            false,
            false,
        )
        .expect_err("an unparseable Stop payload is an error");
        assert!(
            matches!(err, crate::error::Error::Internal(ref e)
                if e.to_string().contains("stop payload")),
            "unexpected error: {err:?}"
        );

        // The `?` return dropped the worker → exactly one release.
        await_release_count(&pool, 1);
        assert_release_quiet(&pool, 1);
    }

    #[test]
    fn pooled_session_sigint_releases_the_worker_exactly_once() {
        let _drive = drive_signal_lock();
        let dir = tempfile::tempdir().unwrap();
        let pool = scripted_pool(dir.path(), 1);

        let worker = PoolClient::new(pool.socket.clone()).acquire(10).unwrap();
        let assignment = pool.assignment_of(worker.worker_id());
        let prompt_sink = Arc::new(Mutex::new(Vec::new()));
        // The prompt marker proves the session installed its scoped SIGINT
        // handler (step 4 precedes the injection); the interrupt lands inside
        // the handler window, never at default disposition.
        drive_worker(
            pool.take_worker_side(worker.worker_id()),
            assignment.stop_fifo.clone(),
            WorkerScript::Custom(Box::new(|| {
                nix::sys::signal::kill(nix::unistd::Pid::this(), nix::sys::signal::Signal::SIGINT)
                    .expect("raise SIGINT");
            })),
            prompt_sink,
        );

        let err = crate::session::Session::run_pooled(
            &version_probe_bin(),
            worker,
            b"prompt one".to_vec(),
            Some(60),
            Some(0),
            None,
            Some(30),
            crate::cli::OutputFormat::Text,
            false,
            false,
        )
        .expect_err("a SIGINTed pooled session is Interrupted");
        assert!(
            matches!(err, crate::error::Error::Interrupted(_)),
            "unexpected error: {err:?}"
        );

        // The Interrupted return dropped the worker → exactly one release.
        await_release_count(&pool, 1);
        assert_release_quiet(&pool, 1);
    }

    /// Session::run_pooled's panic boundary shape: the worker is owned inside
    /// a `catch_unwind` closure that has ALREADY released it (the success-path
    /// discipline) when the panic hits. The unwind drop must not re-send.
    #[test]
    fn explicit_release_then_panic_releases_exactly_once() {
        let dir = tempfile::tempdir().unwrap();
        let pool = scripted_pool(dir.path(), 1);

        let worker = PoolClient::new(pool.socket.clone()).acquire(10).unwrap();
        let assignment = pool.assignment_of(worker.worker_id());
        await_release_count(&pool, 0); // nothing released yet (trivially true)

        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
            let mut worker = worker;
            worker.release();
            panic!("the session exploded after a successful drive");
        }));
        assert!(outcome.is_err(), "the panic must surface as Err");

        await_release_count(&pool, 1);
        assert_release_quiet(&pool, 1);
        assert_eq!(pool.released(), vec![assignment.worker_id]);
    }

    /// The other half of the boundary: a panic while still HOLDING the worker
    /// drops it during unwinding, which releases exactly once — a leaked
    /// assignment (zero) or a double send (two) both fail this pin.
    #[test]
    fn panic_while_holding_the_worker_releases_exactly_once() {
        let dir = tempfile::tempdir().unwrap();
        let pool = scripted_pool(dir.path(), 1);

        let socket = pool.socket.clone();
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
            let _worker = PoolClient::new(socket).acquire(10).unwrap();
            panic!("the session exploded holding its worker");
        }));
        assert!(outcome.is_err(), "the panic must surface as Err");

        await_release_count(&pool, 1);
        assert_release_quiet(&pool, 1);
    }

    /// The other half of [`AcquiredWorker::release`]'s "best-effort" promise:
    /// a dead daemon has nothing left to release, so the release attempt must
    /// fail fast (the unreachable connect is refused immediately, never held
    /// for the whole [`RELEASE_TIMEOUT_SECS`] budget), never panic, still
    /// mark the worker released — and the subsequent Drop must be a silent
    /// no-op rather than a second attempt. Every exit-path pin above runs
    /// against a LIVE daemon; this holds the boundary where the daemon is
    /// already gone.
    #[test]
    fn release_against_a_dead_daemon_is_bounded_and_best_effort() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pool.sock");
        // Serve only the acquire: after this one connection the listener
        // thread ends and the listening socket closes, leaving the socket
        // FILE naming a daemon that no longer answers.
        fake_daemon(&path, 1, move |stream| {
            let mut stream = stream;
            let _ = PoolServer::read_frame(&mut stream).unwrap().unwrap();
            let response = PoolResponse::WorkerAssigned {
                worker_id: "w-dead".to_string(),
                message: "Worker ready".to_string(),
                stop_fifo: "/tmp/stop.fifo".to_string(),
                pid: 4242,
                cwd: "/srv/daemon".to_string(),
            };
            write_frame(&mut stream, &serde_json::to_vec(&response).unwrap());
            let (_pty, probe) = stream_pair();
            PoolServer::send_fd(&stream, probe.into_raw_fd()).unwrap();
        });

        let mut worker = PoolClient::new(path).acquire(5).unwrap();
        assert_eq!(worker.worker_id(), "w-dead");
        assert!(!worker.is_released());

        let started = Instant::now();
        worker.release();
        let elapsed = started.elapsed();
        assert!(
            elapsed < Duration::from_secs(5),
            "a release against a dead daemon must fail fast, not burn its \
             bounded exchange budget: took {elapsed:?}"
        );
        // The failed attempt still counts: Drop must never retry it.
        assert!(worker.is_released());
        drop(worker);
    }

    /// Two sequential invocations against the same pool must observe zero
    /// leakage: distinct worker identity (id, pid, Stop FIFO, PTY) per
    /// caller, each driven through its own hook path and transcript, each
    /// worker receiving only its own caller's prompt, the first caller's
    /// master fd closed before the second runs, and no environment value
    /// crossing callers. `tests/serve.rs` pins the same contract through the
    /// compiled binary against the real daemon; this holds it at the level
    /// where every identity is directly observable.
    #[test]
    fn two_sequential_pooled_invocations_observe_zero_cross_caller_leakage() {
        let _drive = drive_signal_lock();
        let dir = tempfile::tempdir().unwrap();
        let pool = scripted_pool(dir.path(), 2);

        let env_before: std::collections::BTreeMap<String, String> = std::env::vars().collect();

        // ── invocation 1 ────────────────────────────────────────────────────
        let transcript1 = dir.path().join("t-one.jsonl");
        write_transcript(&transcript1, "answer one", "sess-one");
        let worker1 = PoolClient::new(pool.socket.clone()).acquire(10).unwrap();
        let a1 = pool.assignment_of(worker1.worker_id());
        let master_fd1 = worker1.master_fd();
        let master_target1 = fd_target(master_fd1);
        let sink1 = Arc::new(Mutex::new(Vec::new()));
        drive_worker(
            pool.take_worker_side(worker1.worker_id()),
            a1.stop_fifo.clone(),
            WorkerScript::Stop(stop_payload("sess-one", &transcript1, "answer one")),
            sink1.clone(),
        );
        let result1 = crate::session::Session::run_pooled(
            &version_probe_bin(),
            worker1,
            b"prompt one".to_vec(),
            Some(60),
            Some(0),
            None,
            Some(10),
            crate::cli::OutputFormat::Text,
            false,
            false,
        )
        .expect("invocation 1 must succeed");
        await_release_count(&pool, 1);

        assert_eq!(result1.transcript.session_id.as_deref(), Some("sess-one"));
        assert!(result1.transcript.text.contains("answer one"));
        assert_eq!(result1.transcript_path, transcript1);
        assert_master_fd_closed(master_fd1, &master_target1);

        // Invocation 1's hook path is gone: its Stop FIFO has no reader left,
        // so a late payload for the OLD caller is undeliverable (ENXIO) —
        // nothing of invocation 1 is still listening for invocation 2 to
        // trip over.
        let stale = {
            use std::os::unix::fs::OpenOptionsExt;
            std::fs::OpenOptions::new()
                .write(true)
                .custom_flags(libc::O_NONBLOCK)
                .open(&a1.stop_fifo)
        };
        assert_eq!(
            stale
                .expect_err("invocation 1's FIFO must have no reader left")
                .raw_os_error(),
            Some(nix::errno::Errno::ENXIO as i32),
            "opening a readerless FIFO must fail with ENXIO"
        );

        // ── invocation 2 ────────────────────────────────────────────────────
        let transcript2 = dir.path().join("t-two.jsonl");
        write_transcript(&transcript2, "answer two", "sess-two");
        let worker2 = PoolClient::new(pool.socket.clone()).acquire(10).unwrap();
        let a2 = pool.assignment_of(worker2.worker_id());
        let master_fd2 = worker2.master_fd();
        let master_target2 = fd_target(master_fd2);

        // Identity: every field the daemon advertises differs per caller.
        assert_ne!(
            a1.worker_id, a2.worker_id,
            "worker id reused across callers"
        );
        assert_ne!(a1.pid, a2.pid, "worker pid reused across callers");
        assert_ne!(
            a1.stop_fifo, a2.stop_fifo,
            "hook path reused across callers"
        );
        assert_ne!(
            master_target1, master_target2,
            "the second caller was handed the first caller's PTY"
        );

        let sink2 = Arc::new(Mutex::new(Vec::new()));
        drive_worker(
            pool.take_worker_side(worker2.worker_id()),
            a2.stop_fifo.clone(),
            WorkerScript::Stop(stop_payload("sess-two", &transcript2, "answer two")),
            sink2.clone(),
        );
        let result2 = crate::session::Session::run_pooled(
            &version_probe_bin(),
            worker2,
            b"prompt two".to_vec(),
            Some(60),
            Some(0),
            None,
            Some(10),
            crate::cli::OutputFormat::Text,
            false,
            false,
        )
        .expect("invocation 2 must succeed");
        await_release_count(&pool, 2);
        assert_release_quiet(&pool, 2);

        // Session identity did not cross: invocation 2 resolved ITS stop
        // payload, ITS transcript, and none of invocation 1's data.
        assert_eq!(result2.transcript.session_id.as_deref(), Some("sess-two"));
        assert!(result2.transcript.text.contains("answer two"));
        assert!(
            !result2.transcript.text.contains("answer one"),
            "invocation 2 must not read invocation 1's transcript"
        );
        assert_eq!(result2.transcript_path, transcript2);
        assert_ne!(result1.transcript_path, result2.transcript_path);
        assert_master_fd_closed(master_fd2, &master_target2);

        // Exactly one release per caller, each naming its own worker.
        assert_eq!(
            pool.released(),
            vec![a1.worker_id.clone(), a2.worker_id.clone()]
        );

        // Prompts did not cross: each scripted worker saw exactly its own
        // caller's prompt.
        let seen1 = sink1.lock().unwrap().clone();
        let seen2 = sink2.lock().unwrap().clone();
        assert!(seen1.windows(10).any(|w| w == b"prompt one"));
        assert!(!seen1.windows(10).any(|w| w == b"prompt two"));
        assert!(seen2.windows(10).any(|w| w == b"prompt two"));
        assert!(!seen2.windows(10).any(|w| w == b"prompt one"));

        // No environment value crossed the two drives.
        assert_env_stable(&env_before);
    }

    /// An acquire that finds no ready worker waits out the daemon's hold on
    /// the request, bounded by the caller's deadline, and then resolves to
    /// the child-1 fallback classification (ADR-005 stateless path), never a
    /// hang and never a hard error.
    #[test]
    fn acquire_with_no_ready_worker_waits_out_the_daemon_hold_then_falls_back() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pool.sock");
        const HOLD: Duration = Duration::from_millis(1200);
        fake_daemon(&path, 1, move |stream| {
            let mut stream = stream;
            let _ = PoolServer::read_frame(&mut stream).unwrap().unwrap();
            // The daemon holds the acquire while waiting for a worker to
            // free up, then reports none became available.
            std::thread::sleep(HOLD);
            let response = PoolResponse::Error {
                error: "Pool full - no ready workers".to_string(),
                code: ErrorCode::PoolFull,
            };
            write_frame(&mut stream, &serde_json::to_vec(&response).unwrap());
        });

        let started = Instant::now();
        match acquire_for_invocation(Some(&path), 30).unwrap() {
            InvocationAcquisition::Fallback(failure) => {
                assert_eq!(
                    failure,
                    AcquireFailure::PoolUnavailable {
                        code: "pool_full".to_string(),
                        error: "Pool full - no ready workers".to_string(),
                    }
                );
                assert!(is_stateless_fallback(&failure));
            }
            other => panic!("a pool with no ready worker must classify as fallback, got {other:?}"),
        }
        let elapsed = started.elapsed();
        assert!(
            elapsed >= HOLD,
            "the caller must wait out the daemon's hold, returned after {elapsed:?}"
        );
        assert!(
            elapsed < Duration::from_secs(10),
            "the wait must stay bounded by the caller deadline, took {elapsed:?}"
        );
    }

    // (A throwaway fd-probe test that lived here during development was
    // removed once the drive_worker marker scan was fixed — its diagnosis,
    // that the scripted worker side does receive the client's prompt bytes,
    // is pinned by every drive test above.)
}
