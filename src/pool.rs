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
//   "message": "Worker ready"
// }
// ```
// Note: The actual PTY master fd is sent as ancillary data (SCM_RIGHTS)
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
use std::os::unix::io::{AsRawFd, IntoRawFd, RawFd};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};
use uuid::Uuid;

/// Default socket path for the pool daemon
pub const DEFAULT_SOCKET_PATH: &str = "/tmp/claude-print-pool.sock";

/// Maximum time a client will wait for a worker acquisition
const DEFAULT_ACQUIRE_TIMEOUT_SECS: u64 = 60;

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
    /// Worker successfully assigned (PTY fd sent as SCM_RIGHTS)
    WorkerAssigned { worker_id: String, message: String },
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
#[derive(Debug, Serialize, Deserialize, Clone, Copy)]
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
                // Get the worker to extract its PTY fd
                let master_fd = {
                    let mgr = manager.lock().unwrap();
                    let worker = mgr.get_worker(&id).unwrap();
                    worker.master_fd
                };

                // Send success response + PTY fd via SCM_RIGHTS
                let response = PoolResponse::WorkerAssigned {
                    worker_id: id.clone(),
                    message: "Worker ready".to_string(),
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
            cmsg.cmsg_len = std::mem::size_of::<libc::cmsghdr>() + std::mem::size_of::<RawFd>();
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

    /// Connect to the pool and acquire a worker
    ///
    /// Returns the worker ID and PTY master fd
    pub fn acquire(&self, timeout_secs: u64) -> Result<(String, RawFd), PoolResponse> {
        let mut stream =
            std::os::unix::net::UnixStream::connect(&self.socket_path).map_err(|_| {
                PoolResponse::Error {
                    error: format!(
                        "Failed to connect to pool at {}",
                        self.socket_path.display()
                    ),
                    code: ErrorCode::InternalError,
                }
            })?;

        // Send acquire request
        let request = PoolRequest::Acquire { timeout_secs };
        let json = serde_json::to_vec(&request)?;

        // Send length + JSON
        let len_buf = (json.len() as u32).to_be_bytes();
        stream
            .write_all(&len_buf)
            .map_err(|_| PoolResponse::Error {
                error: "Failed to send request".to_string(),
                code: ErrorCode::InternalError,
            })?;
        stream.write_all(&json).map_err(|_| PoolResponse::Error {
            error: "Failed to send request".to_string(),
            code: ErrorCode::InternalError,
        })?;

        // Receive response length
        let mut len_buf = [0u8; 4];
        stream
            .read_exact(&mut len_buf)
            .map_err(|_| PoolResponse::Error {
                error: "Failed to read response".to_string(),
                code: ErrorCode::InternalError,
            })?;
        let msg_len = u32::from_be_bytes(len_buf) as usize;

        // Receive response JSON
        let mut buf = vec![0u8; msg_len];
        stream
            .read_exact(&mut buf)
            .map_err(|_| PoolResponse::Error {
                error: "Failed to read response".to_string(),
                code: ErrorCode::InternalError,
            })?;

        let response: PoolResponse = serde_json::from_slice(&buf)?;

        match response {
            PoolResponse::WorkerAssigned { worker_id, .. } => {
                // Receive the PTY fd via SCM_RIGHTS
                let fd = Self::recv_fd(&stream)?;
                Ok((worker_id, fd))
            }
            PoolResponse::Error { .. } => Err(response),
        }
    }

    /// Release a worker back to the pool
    pub fn release(&self, worker_id: &str) -> Result<(), PoolResponse> {
        let mut stream =
            std::os::unix::net::UnixStream::connect(&self.socket_path).map_err(|_| {
                PoolResponse::Error {
                    error: format!(
                        "Failed to connect to pool at {}",
                        self.socket_path.display()
                    ),
                    code: ErrorCode::InternalError,
                }
            })?;

        let request = PoolRequest::Release {
            worker_id: worker_id.to_string(),
        };
        let json = serde_json::to_vec(&request)?;

        let len_buf = (json.len() as u32).to_be_bytes();
        stream
            .write_all(&len_buf)
            .map_err(|_| PoolResponse::Error {
                error: "Failed to send request".to_string(),
                code: ErrorCode::InternalError,
            })?;
        stream.write_all(&json).map_err(|_| PoolResponse::Error {
            error: "Failed to send request".to_string(),
            code: ErrorCode::InternalError,
        })?;

        let mut len_buf = [0u8; 4];
        stream
            .read_exact(&mut len_buf)
            .map_err(|_| PoolResponse::Error {
                error: "Failed to read response".to_string(),
                code: ErrorCode::InternalError,
            })?;
        let msg_len = u32::from_be_bytes(len_buf) as usize;

        let mut buf = vec![0u8; msg_len];
        stream
            .read_exact(&mut buf)
            .map_err(|_| PoolResponse::Error {
                error: "Failed to read response".to_string(),
                code: ErrorCode::InternalError,
            })?;

        let response: PoolResponse = serde_json::from_slice(&buf)?;

        match response {
            PoolResponse::WorkerAssigned { .. } => Ok(()),
            PoolResponse::Error { .. } => Err(response),
        }
    }

    /// Receive a file descriptor via SCM_RIGHTS
    fn recv_fd(stream: &std::os::unix::net::UnixStream) -> Result<RawFd, PoolResponse> {
        use std::os::unix::io::AsRawFd;

        let socket_fd = stream.as_raw_fd();

        unsafe {
            let mut cmsg: libc::cmsghdr = std::mem::zeroed();
            let mut msg: libc::msghdr = std::mem::zeroed();

            let mut iov = [std::mem::zeroed::<libc::iovec>(); 1];
            iov[0].iov_base = std::ptr::null_mut();
            iov[0].iov_len = 1;

            msg.msg_iov = iov.as_mut_ptr();
            msg.msg_iovlen = 1;
            msg.msg_control = &mut cmsg as *mut _ as *mut _;
            msg.msg_controllen = std::mem::size_of::<libc::cmsghdr>() + 1024;

            let fd_buf: [RawFd; 1] = [-1];
            let cmsg_data = (msg.msg_control as *mut u8).add(std::mem::size_of::<libc::cmsghdr>());
            *(cmsg_data as *mut RawFd) = fd_buf[0];

            let ret = libc::recvmsg(socket_fd, &mut msg, 0);
            if ret < 0 {
                return Err(PoolResponse::Error {
                    error: "Failed to receive fd".to_string(),
                    code: ErrorCode::InternalError,
                });
            }

            // Extract fd from cmsghdr
            if cmsg.cmsg_type == libc::SCM_RIGHTS {
                let data_ptr =
                    (msg.msg_control as *const u8).add(std::mem::size_of::<libc::cmsghdr>());
                let fd = *(data_ptr as *const RawFd);
                if fd >= 0 {
                    return Ok(fd);
                }
            }

            Err(PoolResponse::Error {
                error: "No fd received".to_string(),
                code: ErrorCode::InternalError,
            })
        }
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
}
