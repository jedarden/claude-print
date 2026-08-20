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
use std::os::unix::io::{AsRawFd, RawFd};
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
    /// Channel for warmup completion notifications
    warmup_tx: mpsc::Sender<String>,
    /// Channel for warmup completion notifications (receiver)
    warmup_rx: mpsc::Receiver<String>,
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

    /// Destroy a worker and clean up its resources
    fn destroy_worker(&self, worker: PoolWorker) {
        if self.verbose {
            eprintln!(
                "[claude-print pool] Destroying worker {} (pid {})",
                worker.id, worker.child_pid
            );
        }

        // Close the PTY master fd
        let _ = nix::unistd::close(worker.master_fd);

        // Send SIGTERM to the child, then SIGKILL after grace period
        let pid = worker.child_pid;
        match nix::sys::signal::kill(pid, nix::sys::signal::Signal::SIGTERM) {
            Ok(_) => {
                // Wait up to 2 seconds for graceful exit
                let start = Instant::now();
                while start.elapsed() < Duration::from_secs(2) {
                    match nix::sys::wait::waitpid(pid, Some(nix::sys::wait::WaitPidFlag::WNOHANG)) {
                        Ok(nix::sys::wait::WaitStatus::Exited(_, _)) => break,
                        Ok(nix::sys::wait::WaitStatus::Signaled(_, _, _)) => break,
                        Err(_) => break,
                        _ => {
                            std::thread::sleep(Duration::from_millis(100));
                            continue;
                        }
                    }
                }
                // If still running, force kill
                let _ = nix::sys::signal::kill(pid, nix::sys::signal::Signal::SIGKILL);
                let _ = nix::sys::wait::waitpid(pid, None);
            }
            Err(_) => {
                // Process already gone
            }
        }

        // Drop the hook installer to clean up temp dir
        drop(worker.hook_installer);
    }

    /// Maintain the pool at target size
    pub fn maintain(&mut self) -> Result<(), anyhow::Error> {
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

    /// Process warmup completion notifications
    fn process_warmup_completions(&mut self) {
        // Drain the channel of all completed warmups
        while let Ok(worker_id) = self.warmup_rx.try_recv() {
            if let Some(worker) = self.workers.get_mut(&worker_id) {
                worker.state = WorkerState::Ready;
                worker.state_since = Instant::now();
                if self.verbose {
                    eprintln!("[claude-print pool] Worker {} marked as Ready", worker_id);
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

        Ok(PoolWorker {
            id: worker_id.to_string(),
            state: WorkerState::Warming,
            master_fd: spawner.master.as_raw_fd(),
            child_pid: spawner.child_pid,
            state_since: Instant::now(),
            hook_installer: Some(hook_installer),
        })
    }

    /// Background task to warm a worker through trust-dismiss and idle-settle
    ///
    /// This is the core warmup logic per ADR-005: workers are pre-warmed through
    /// trust-dismiss and idle-settle, but never past prompt injection.
    fn warm_worker_background(
        worker_id: String,
        master_fd: RawFd,
        shutdown_flag: Arc<AtomicBool>,
        warmup_tx: mpsc::Sender<String>,
        verbose: bool,
    ) {
        if shutdown_flag.load(Ordering::SeqCst) {
            return;
        }

        if verbose {
            eprintln!(
                "[claude-print pool] Starting warmup for worker {}",
                worker_id
            );
        }

        // Create a self-pipe for signal handling (minimal setup)
        let (pipe_r, pipe_w) = match nix::unistd::pipe() {
            Ok(p) => p,
            Err(e) => {
                eprintln!("[claude-print pool] Failed to create self-pipe: {}", e);
                return;
            }
        };

        // Get raw fds for the pipe ends
        let pipe_r_raw = pipe_r.as_raw_fd();
        let pipe_w_raw = pipe_w.as_raw_fd();

        // Create event loop with the PTY master fd
        let mut event_loop = crate::event_loop::EventLoop::new(master_fd, pipe_r_raw);

        // Create startup sequencer (empty prompt - we won't inject during warmup)
        let mut startup_seq = crate::startup::StartupSeq::new(Vec::new());

        // Create terminal emulator for probe responses (default window size: 220x50)
        let mut terminal_emu = crate::terminal::TerminalEmu::new(220, 50);

        // Warmup timeout tracking
        let warmup_start = Instant::now();
        let warmup_timeout = Duration::from_secs(WARMUP_TIMEOUT_SECS);

        // Warmup phases: Starting → Settling → Ready
        // We never inject a prompt during warmup (that happens when client acquires)
        let mut warmup_phase = WarmupPhase::Starting;

        loop {
            // Check shutdown flag
            if shutdown_flag.load(Ordering::SeqCst) {
                if verbose {
                    eprintln!(
                        "[claude-print pool] Worker {} warmup cancelled (shutdown)",
                        worker_id
                    );
                }
                let _ = nix::unistd::close(pipe_r_raw);
                let _ = nix::unistd::close(pipe_w_raw);
                std::mem::forget(pipe_r); // Avoid double-close
                std::mem::forget(pipe_w);
                return;
            }

            // Check warmup timeout
            if warmup_start.elapsed() > warmup_timeout {
                eprintln!(
                    "[claude-print pool] Worker {} warmup timeout after {:.1}s",
                    worker_id,
                    warmup_start.elapsed().as_secs_f64()
                );
                let _ = nix::unistd::close(pipe_r_raw);
                let _ = nix::unistd::close(pipe_w_raw);
                std::mem::forget(pipe_r);
                std::mem::forget(pipe_w);
                return;
            }

            // Run event loop for one iteration
            match event_loop.run(|chunk| {
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
                    match action {
                        crate::startup::StartupAction::Write(bytes) => {
                            let _ = unsafe {
                                libc::write(
                                    master_fd,
                                    bytes.as_ptr() as *const libc::c_void,
                                    bytes.len(),
                                )
                            };
                        }
                        crate::startup::StartupAction::None => {}
                        crate::startup::StartupAction::HardTimeout => {
                            eprintln!(
                                "[claude-print pool] Worker {} hard timeout during warmup",
                                worker_id
                            );
                            warmup_phase = WarmupPhase::Failed;
                        }
                    }
                }

                // Check startup timers
                let action = startup_seq.poll_timers();
                match action {
                    crate::startup::StartupAction::Write(bytes) => {
                        let _ = unsafe {
                            libc::write(
                                master_fd,
                                bytes.as_ptr() as *const libc::c_void,
                                bytes.len(),
                            )
                        };
                    }
                    crate::startup::StartupAction::None => {}
                    crate::startup::StartupAction::HardTimeout => {
                        eprintln!(
                            "[claude-print pool] Worker {} hard timeout during warmup",
                            worker_id
                        );
                        warmup_phase = WarmupPhase::Failed;
                    }
                }
            }) {
                Ok(crate::event_loop::ExitReason::ChildExited) => {
                    // Child exited during warmup - this is a failure
                    eprintln!(
                        "[claude-print pool] Worker {} child exited during warmup",
                        worker_id
                    );
                    let _ = nix::unistd::close(pipe_r_raw);
                    let _ = nix::unistd::close(pipe_w_raw);
                    return;
                }
                Ok(crate::event_loop::ExitReason::Interrupted) => {
                    // SIGINT/SIGTERM - exit gracefully
                    if verbose {
                        eprintln!(
                            "[claude-print pool] Worker {} warmup interrupted",
                            worker_id
                        );
                    }
                    let _ = nix::unistd::close(pipe_r_raw);
                    let _ = nix::unistd::close(pipe_w_raw);
                    return;
                }
                Ok(crate::event_loop::ExitReason::FifoPayload(_)) => {
                    // Should not happen during warmup (FIFO not opened yet)
                    eprintln!(
                        "[claude-print pool] Worker {} unexpected FIFO payload during warmup",
                        worker_id
                    );
                }
                Err(e) => {
                    eprintln!(
                        "[claude-print pool] Worker {} warmup event loop error: {}",
                        worker_id, e
                    );
                    let _ = nix::unistd::close(pipe_r_raw);
                    let _ = nix::unistd::close(pipe_w_raw);
                    return;
                }
            }

            // Check if we've reached the Ready state
            // We're ready when trust has been dismissed and we've settled
            if warmup_phase == WarmupPhase::Starting {
                if startup_seq.phase() == &crate::startup::StartupPhase::TrustDismissed {
                    warmup_phase = WarmupPhase::Settling;
                    if verbose {
                        eprintln!(
                            "[claude-print pool] Worker {} trust dismissed, settling...",
                            worker_id
                        );
                    }
                }
            } else if warmup_phase == WarmupPhase::Settling {
                // Check if we've been idle long enough after trust dismiss
                let action = startup_seq.poll_timers();
                match action {
                    crate::startup::StartupAction::None => {
                        // We've settled - worker is ready
                        warmup_phase = WarmupPhase::Ready;
                        if verbose {
                            eprintln!("[claude-print pool] Worker {} settled and ready", worker_id);
                        }
                        break;
                    }
                    _ => {
                        // Still settling or some other action needed
                        continue;
                    }
                }
            } else if warmup_phase == WarmupPhase::Failed {
                eprintln!("[claude-print pool] Worker {} warmup failed", worker_id);
                let _ = nix::unistd::close(pipe_r_raw);
                let _ = nix::unistd::close(pipe_w_raw);
                std::mem::forget(pipe_r);
                std::mem::forget(pipe_w);
                return;
            }

            // Small sleep to prevent tight loop
            std::thread::sleep(Duration::from_millis(10));
        }

        // Clean up pipe fds
        let _ = nix::unistd::close(pipe_r_raw);
        let _ = nix::unistd::close(pipe_w_raw);
        // Prevent the OwnedFds from closing again when they drop
        std::mem::forget(pipe_r);
        std::mem::forget(pipe_w);

        // Notify main thread that warmup is complete
        if !shutdown_flag.load(Ordering::SeqCst) && warmup_phase == WarmupPhase::Ready {
            let _ = warmup_tx.send(worker_id.clone());
            if verbose {
                eprintln!(
                    "[claude-print pool] Worker {} warmup complete in {:.1}s",
                    worker_id,
                    warmup_start.elapsed().as_secs_f64()
                );
            }
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

/// Unix socket server for pool daemon
///
/// Listens on a Unix domain socket and handles client requests for workers.
/// PTY file descriptors are sent via SCM_RIGHTS (ancillary data).
pub struct PoolServer {
    /// Socket path
    socket_path: std::path::PathBuf,
    /// Listener socket
    listener: Option<std::os::unix::net::UnixListener>,
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
            manager: Arc::new(Mutex::new(manager)),
            verbose,
        }
    }

    /// Start the server - bind to socket and accept connections
    pub fn run(&mut self) -> Result<(), anyhow::Error> {
        // Remove existing socket if present
        if self.socket_path.exists() {
            std::fs::remove_file(&self.socket_path)?;
        }

        // Create Unix domain socket listener
        let listener = std::os::unix::net::UnixListener::bind(&self.socket_path)?;

        self.listener = Some(listener);

        if self.verbose {
            eprintln!(
                "[claude-print pool] Listening on {}",
                self.socket_path.display()
            );
        }

        // Accept connections loop
        self.accept_loop()?;

        Ok(())
    }

    /// Accept and handle client connections
    fn accept_loop(&self) -> Result<(), anyhow::Error> {
        let listener = self.listener.as_ref().unwrap();

        // Set non-blocking mode for accept to check shutdown flag
        listener.set_nonblocking(true)?;

        while !self.manager.lock().unwrap().shutdown.load(Ordering::SeqCst) {
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
                    // No connection pending, sleep briefly and check shutdown flag
                    std::thread::sleep(Duration::from_millis(100));
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

    /// Handle a single client connection
    fn handle_connection(
        mut stream: std::os::unix::net::UnixStream,
        manager: Arc<Mutex<PoolManager>>,
        verbose: bool,
    ) -> Result<(), anyhow::Error> {
        // Set read timeout to prevent hanging
        stream.set_read_timeout(Some(Duration::from_secs(5)))?;

        // Read request length (4 bytes)
        let mut len_buf = [0u8; 4];
        let mut n_read = 0;
        while n_read < 4 {
            n_read += stream.read(&mut len_buf[n_read..])?;
            if n_read == 0 {
                return Ok(()); // Client closed
            }
        }

        let msg_len = u32::from_be_bytes(len_buf) as usize;

        // Read request JSON
        let mut buf = vec![0u8; msg_len];
        let mut n_read = 0;
        while n_read < msg_len {
            n_read += stream.read(&mut buf[n_read..])?;
        }

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

    /// Clean up socket on shutdown
    pub fn cleanup(&self) {
        if self.socket_path.exists() {
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
}
