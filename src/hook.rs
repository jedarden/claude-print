use crate::error::{Error, Result};
use nix::sys::stat::Mode;
use nix::unistd::mkfifo;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime};
use tempfile::TempDir;

/// Sweep and remove orphaned temp directories left behind by crashed runs.
///
/// Scans `$TMPDIR` for `claude-print-<pid>-<rand>` directories older than 60s
/// and removes them — but ONLY when the PID embedded in the directory name no
/// longer refers to a running process. A directory whose owner is still alive
/// is left untouched even when aged past the threshold, so a concurrent
/// (long-running) claude-print session's `stop.fifo` IPC is never deleted out
/// from under it. This preserves plan EC-1's no-cross-contamination guarantee
/// for concurrent instances under the NEEDLE fleet (AS-3).
///
/// This function is called at the start of main() to ensure orphans are
/// cleaned up on all invocations, not just when a session runs.
pub fn cleanup_orphans() {
    cleanup_orphans_in(
        &std::env::temp_dir(),
        SystemTime::now(),
        Duration::from_secs(60),
        &is_live_process,
    );
}

/// Parse the owning PID out of a temp-dir name of the form
/// `claude-print-<pid>-<rand>` produced by [`HookInstaller::new`]. Returns
/// `None` when the name doesn't carry a numeric PID.
fn owner_pid_from_name(name: &str) -> Option<u32> {
    let rest = name.strip_prefix("claude-print-")?;
    rest.split('-').next()?.parse::<u32>().ok()
}

/// Returns `true` if `pid` refers to a running process on this host.
///
/// Uses the null signal (`kill(pid, 0)`), which probes existence without
/// delivering a signal. A process owned by another user yields `EPERM` rather
/// than success — that is also treated as "alive" so we never delete a temp dir
/// whose owner we cannot positively identify as dead.
fn is_live_process(pid: u32) -> bool {
    match nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid as i32), None) {
        Ok(()) => true,
        Err(nix::errno::Errno::EPERM) => true,
        Err(_) => false,
    }
}

/// Pure, injectable core of [`cleanup_orphans`]. Scans `dir` for
/// `claude-print-*` directories whose mtime is older than `threshold` relative
/// to `now`, and removes any whose embedded owner PID is not currently running.
///
/// The liveness predicate `is_alive` is injected so tests can drive the decision
/// deterministically; the production entry point passes [`is_live_process`].
///
/// Age is computed from `mtime` (not `btime`): `metadata.created()` returns
/// `Err` on filesystems without birth-time support, which would silently
/// disable cleanup entirely. `mtime` is universally available and matches the
/// orphan scan in `check.rs`.
fn cleanup_orphans_in<F>(dir: &Path, now: SystemTime, threshold: Duration, is_alive: &F)
where
    F: Fn(u32) -> bool,
{
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
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
        // Only consider dirs aged past the threshold.
        if age <= threshold {
            continue;
        }
        // Never delete a temp dir whose owning process is still alive — it may
        // be a concurrent claude-print session mid-turn. If the name carries no
        // parseable PID we also leave it alone (can't prove it's orphaned).
        match owner_pid_from_name(name) {
            Some(pid) if is_alive(pid) => continue,
            None => continue,
            _ => {}
        }
        // Aged out AND owner is dead → safe to reclaim. Try the FIFO first (it
        // may carry different perms) then the whole directory.
        let fifo_path = path.join("stop.fifo");
        if let Err(e) = std::fs::remove_file(&fifo_path) {
            eprintln!(
                "claude-print: warning: failed to remove FIFO {:?}: {}",
                fifo_path, e
            );
        }
        if let Err(e) = std::fs::remove_dir_all(&path) {
            eprintln!(
                "claude-print: warning: failed to remove orphaned temp dir {:?}: {}",
                path, e
            );
        } else {
            eprintln!("claude-print: cleaned up orphaned temp dir: {:?}", path);
        }
    }
}

/// Name of the per-drive session-identity file the UserPromptSubmit relay hook
/// writes, sibling of `stop.fifo` in the drive's temp dir.
///
/// The stream-json reader binds to the transcript this file names (bead
/// claudepr-a927ec0c): the Stop payload carries the same fields but only
/// arrives after the turn, far too late for live forwarding, so identity has
/// to come from a hook that fires at prompt-submission time. Pool clients
/// derive the path as `stop_fifo().with_file_name(SESSION_IDENTITY_FILE)` —
/// the worker's Stop FIFO lives in the same HookInstaller dir, so the sibling
/// relationship is part of the daemon/client contract.
pub const SESSION_IDENTITY_FILE: &str = "session-identity.json";

pub struct HookInstaller {
    pub dir: TempDir,
    pub settings_path: PathBuf,
    pub hook_path: PathBuf,
    pub fifo_path: PathBuf,
    /// Per-drive identity file the UserPromptSubmit relay hook writes its
    /// payload into (see [`SESSION_IDENTITY_FILE`]).
    pub identity_path: PathBuf,
    /// Flag to track whether cleanup has already been performed.
    /// This prevents double-panic issues during cleanup.
    cleanup_performed: Arc<AtomicBool>,
}

impl HookInstaller {
    pub fn new() -> Result<Self> {
        use std::os::unix::fs::PermissionsExt;

        // Owner-only from the first instant (plan T-1, docs/notes/hook-design.md):
        // a default tempdir() is created `0o777 & ~umask` — typically 0755 —
        // handing the Stop payload (session id, prompt text) to every local
        // user. Requesting 0700 up front means the umask can only tighten the
        // mode, never loosen it.
        let dir = tempfile::Builder::new()
            .prefix(&format!("claude-print-{}-", std::process::id()))
            .permissions(std::fs::Permissions::from_mode(0o700))
            .tempdir()
            .map_err(|e| Error::Internal(anyhow::anyhow!("failed to create temp dir: {e}")))?;

        let settings_path = dir.path().join("settings.json");
        let hook_path = dir.path().join("hook.sh");
        let fifo_path = dir.path().join("stop.fifo");
        let identity_path = dir.path().join(SESSION_IDENTITY_FILE);

        let identity_hook_path = dir.path().join("identity.sh");
        write_hook_sh(&hook_path, &fifo_path)?;
        write_identity_sh(&identity_hook_path, &identity_path)?;
        write_settings_json(&settings_path, &hook_path, &identity_hook_path)?;

        mkfifo(&fifo_path, Mode::S_IRUSR | Mode::S_IWUSR)
            .map_err(|e| Error::Internal(anyhow::anyhow!("mkfifo failed: {e}")))?;

        // Pin the relay-artifact modes exactly (0700 dir / 0600 FIFO,
        // docs/notes/hook-design.md): creation-time modes are
        // `requested & ~umask`, so a restrictive umask could land the FIFO
        // below 0600 and break hook.sh's write. chmod is unconditional, so
        // both modes hold verbatim whatever umask the invoking shell carried.
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).map_err(
            |e| Error::Internal(anyhow::anyhow!("failed to pin temp dir mode 0700: {e}")),
        )?;
        std::fs::set_permissions(&fifo_path, std::fs::Permissions::from_mode(0o600)).map_err(
            |e| Error::Internal(anyhow::anyhow!("failed to pin stop.fifo mode 0600: {e}")),
        )?;

        Ok(HookInstaller {
            dir,
            settings_path,
            hook_path,
            fifo_path,
            identity_path,
            cleanup_performed: Arc::new(AtomicBool::new(false)),
        })
    }

    pub fn dir_path(&self) -> &Path {
        self.dir.path()
    }
}

impl Drop for HookInstaller {
    fn drop(&mut self) {
        // Clean up on drop to ensure temp dirs are removed even if
        // explicit cleanup() is not called.
        self.cleanup();
    }
}

impl HookInstaller {
    /// Explicitly clean up the temporary directory and FIFO.
    ///
    /// This is called automatically on Drop, but can be called explicitly
    /// to ensure cleanup on all exit paths (normal, error, timeout, signal).
    ///
    /// This function is idempotent - calling it multiple times is safe.
    pub fn cleanup(&self) {
        // Use atomic swap to ensure we only cleanup once, even if called
        // from multiple threads or recursively during panic/abort.
        if self.cleanup_performed.swap(true, Ordering::SeqCst) {
            // Already cleaned up
            return;
        }

        // Remove the FIFO first (it may have different permissions)
        // The FIFO must be removed before the directory can be deleted.
        // Retry FIFO removal multiple times in case of transient errors.
        for fifo_attempt in 0..3 {
            let result = std::fs::remove_file(&self.fifo_path);
            if result.is_ok() {
                break; // FIFO successfully removed
            }
            // If this is not the last attempt, wait a bit before retrying
            if fifo_attempt < 2 {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        }
        // Ignore FIFO removal errors - it might not exist or be already removed

        // Explicitly remove the entire temp directory
        // This is more robust than relying on TempDir::drop, especially
        // during panic/abort where destructors may not run properly.
        let dir_path = self.dir.path();

        // Try multiple times to remove the directory in case of transient errors
        // (e.g., files still being locked or accessed by other processes)
        for attempt in 0..3 {
            let result = std::fs::remove_dir_all(dir_path);
            if result.is_ok() {
                break; // Successfully removed
            }
            // If this is not the last attempt, wait a bit before retrying
            if attempt < 2 {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        }
        // Ignore final error - we've done our best
    }
}

/// Single-quote a path for safe embedding in a POSIX shell command.
///
/// `'` => `'\''` — the canonical escape; the payload path is interpolated
/// inside single quotes in the generated hook scripts, so metacharacters in
/// the temp-dir path can never become shell syntax (bf-5sj7).
fn shell_single_quoted(path: &Path) -> String {
    path.to_string_lossy().replace('\'', "'\\''")
}

fn write_hook_sh(hook_path: &Path, fifo_path: &Path) -> Result<()> {
    write_cat_script(hook_path, fifo_path, "hook.sh")
}

/// Write the identity relay script: the UserPromptSubmit hook `cat`s its
/// stdin payload into the per-drive `session-identity.json` (truncating —
/// a one-prompt session fires the event once; a rewritten file always
/// describes the current session). Same shape and escaping as hook.sh.
fn write_identity_sh(hook_path: &Path, identity_path: &Path) -> Result<()> {
    write_cat_script(hook_path, identity_path, "identity.sh")
}

/// Shared body of the two relay scripts: `cat > '<target>'`, executable 0o750.
fn write_cat_script(script_path: &Path, target: &Path, name: &str) -> Result<()> {
    let content = format!(
        "#!/bin/sh\ncat > '{}' 2>/dev/null || true\n",
        shell_single_quoted(target)
    );
    std::fs::write(script_path, &content)
        .map_err(|e| Error::Internal(anyhow::anyhow!("failed to write {name}: {e}")))?;

    // Make executable
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(script_path)
            .map_err(|e| Error::Internal(anyhow::anyhow!("stat {name}: {e}")))?
            .permissions();
        perms.set_mode(0o750);
        std::fs::set_permissions(script_path, perms)
            .map_err(|e| Error::Internal(anyhow::anyhow!("chmod {name}: {e}")))?;
    }

    Ok(())
}

fn write_settings_json(
    settings_path: &Path,
    hook_path: &Path,
    identity_hook_path: &Path,
) -> Result<()> {
    let hook_str = hook_path.to_string_lossy();
    let identity_str = identity_hook_path.to_string_lossy();
    let json = serde_json::json!({
        "hooks": {
            "Stop": [{
                "hooks": [{"type": "command", "command": hook_str, "timeout": 10}]
            }],
            // claudepr-a927ec0c: second relay hook firing at prompt-submission
            // time. Its payload carries the same envelope as Stop (session_id,
            // transcript_path, cwd) but arrives BEFORE the assistant's first
            // transcript event, giving the stream-json reader a per-drive
            // identity to bind to instead of guessing among same-cwd
            // transcripts by mtime. Merges alongside user hooks exactly like
            // the Stop relay (PO-1).
            "UserPromptSubmit": [{
                "hooks": [{"type": "command", "command": identity_str, "timeout": 10}]
            }]
        }
    });
    let content = serde_json::to_string_pretty(&json)
        .map_err(|e| Error::Internal(anyhow::anyhow!("serialize settings.json: {e}")))?;
    std::fs::write(settings_path, content)
        .map_err(|e| Error::Internal(anyhow::anyhow!("write settings.json: {e}")))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_temp_dir_with_artifacts() {
        let installer = HookInstaller::new().unwrap();
        assert!(installer.settings_path.exists());
        assert!(installer.hook_path.exists());
        assert!(installer.fifo_path.exists());
    }

    #[test]
    fn settings_json_has_stop_hook() {
        let installer = HookInstaller::new().unwrap();
        let content = std::fs::read_to_string(&installer.settings_path).unwrap();
        let val: serde_json::Value = serde_json::from_str(&content).unwrap();
        let stop = &val["hooks"]["Stop"];
        assert!(stop.is_array());
        let hooks = &stop[0]["hooks"];
        assert!(hooks.is_array());
        assert_eq!(hooks[0]["type"], "command");
    }

    // ── claudepr-a927ec0c: UserPromptSubmit identity relay hook ──────────────

    /// The settings must carry a UserPromptSubmit relay pointing at
    /// identity.sh — the per-drive identity channel the stream-json reader
    /// binds to. A settings file that loses this hook silently regresses the
    /// reader to mtime-guessing among same-cwd transcripts.
    #[test]
    fn settings_json_has_user_prompt_submit_identity_hook() {
        let installer = HookInstaller::new().unwrap();
        let content = std::fs::read_to_string(&installer.settings_path).unwrap();
        let val: serde_json::Value = serde_json::from_str(&content).unwrap();
        let ups = &val["hooks"]["UserPromptSubmit"];
        assert!(ups.is_array(), "UserPromptSubmit must be an array");
        let hook = &ups[0]["hooks"][0];
        assert_eq!(hook["type"], "command");
        let cmd = hook["command"].as_str().unwrap_or("");
        assert!(
            cmd.contains("identity.sh"),
            "UserPromptSubmit relay must reference identity.sh, got: {cmd:?}"
        );
    }

    /// The identity file is the session-identity.json SIBLING of stop.fifo.
    /// Pool clients reconstruct the path from `stop_fifo()` alone, so this
    /// layout is a daemon/client contract, not an implementation detail.
    #[test]
    fn identity_path_is_stop_fifo_sibling() {
        let installer = HookInstaller::new().unwrap();
        assert_eq!(
            installer.identity_path.file_name().and_then(|n| n.to_str()),
            Some(SESSION_IDENTITY_FILE)
        );
        assert_eq!(
            installer.identity_path.parent(),
            installer.fifo_path.parent(),
            "session-identity.json must live in the same dir as stop.fifo"
        );
    }

    /// identity.sh mirrors hook.sh: cat stdin into its target, executable,
    /// shell-safe against metacharacters in the temp-dir path.
    #[test]
    fn identity_sh_is_executable_and_targets_identity_file() {
        let installer = HookInstaller::new().unwrap();
        let script = installer.dir_path().join("identity.sh");
        let content = std::fs::read_to_string(&script).unwrap();
        assert!(content.starts_with("#!/bin/sh"));
        assert!(content.contains("cat > '"));
        let quoted = shell_single_quoted(&installer.identity_path);
        assert!(
            content.contains(&quoted),
            "identity.sh must target {quoted:?}; got:\n{content}"
        );
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&script).unwrap().permissions().mode();
        assert!(mode & 0o100 != 0, "identity.sh must be executable by owner");
        // Syntactic validity: the script must survive sh -n even with
        // hostile bytes in the temp-dir path.
        let out = std::process::Command::new("sh")
            .arg("-n")
            .arg(&script)
            .output();
        assert!(matches!(out, Ok(ref o) if o.status.success()));
    }

    #[test]
    fn hook_sh_references_fifo() {
        let installer = HookInstaller::new().unwrap();
        let content = std::fs::read_to_string(&installer.hook_path).unwrap();
        assert!(content.contains("cat >"));
        assert!(content.contains("stop.fifo"));
    }

    #[test]
    fn fifo_is_named_pipe() {
        let installer = HookInstaller::new().unwrap();
        let meta = std::fs::metadata(&installer.fifo_path).unwrap();
        // file_type().is_fifo() requires Unix
        #[cfg(unix)]
        {
            use std::os::unix::fs::FileTypeExt;
            assert!(meta.file_type().is_fifo(), "stop.fifo must be a named pipe");
        }
    }

    // ── relay artifact permissions (docs/notes/hook-design.md) ───────────────

    /// hook-design.md promises the temp dir at mode 0700: the Stop payload
    /// (session id, transcript path, prompt text) is written here, and a
    /// world-readable dir hands it to every local user (plan T-1).
    #[test]
    fn temp_dir_mode_is_owner_only() {
        let installer = HookInstaller::new().unwrap();
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(installer.dir_path())
            .expect("temp dir metadata")
            .permissions()
            .mode();
        assert_eq!(
            mode & 0o777,
            0o700,
            "temp dir must be owner-only 0700, got {mode:o}"
        );
    }

    /// hook-design.md promises the FIFO is created with `mkfifo(path, 0600)` —
    /// owner rw only, so only the invoking user can read the payload in flight.
    #[test]
    fn fifo_mode_is_owner_rw() {
        let installer = HookInstaller::new().unwrap();
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&installer.fifo_path)
            .expect("FIFO metadata")
            .permissions()
            .mode();
        assert_eq!(
            mode & 0o777,
            0o600,
            "stop.fifo must be owner-rw 0600, got {mode:o}"
        );
    }

    /// The promised modes must survive the artifacts being *used*, not just
    /// created: the identity relay writes `session-identity.json` into the dir
    /// and Stop relays write through the FIFO mid-session, and neither may
    /// loosen the dir's 0700 or the FIFO's 0600.
    #[test]
    fn relay_artifact_modes_survive_usage() {
        let installer = HookInstaller::new().unwrap();

        // Simulate the UserPromptSubmit relay writing its payload into the dir.
        std::fs::write(&installer.identity_path, br#"{"session_id":"x"}"#)
            .expect("write identity payload");

        // Round-trip the FIFO the way a Stop relay + poller would.
        let fifo = installer.fifo_path.clone();
        let reader = std::thread::spawn(move || {
            use std::io::Read;
            let mut f = std::fs::File::open(&fifo).expect("open FIFO for reading");
            let mut buf = Vec::new();
            f.read_to_end(&mut buf).expect("read FIFO");
            buf
        });
        std::thread::sleep(Duration::from_millis(50));
        std::fs::write(&installer.fifo_path, b"payload").expect("write FIFO");
        assert_eq!(reader.join().unwrap(), b"payload");

        use std::os::unix::fs::PermissionsExt;
        let dir_mode = std::fs::metadata(installer.dir_path())
            .expect("temp dir metadata")
            .permissions()
            .mode();
        assert_eq!(
            dir_mode & 0o777,
            0o700,
            "temp dir 0700 must survive relay usage, got {dir_mode:o}"
        );
        let meta = std::fs::metadata(&installer.fifo_path).expect("FIFO metadata");
        let fifo_mode = meta.permissions().mode();
        assert_eq!(
            fifo_mode & 0o777,
            0o600,
            "stop.fifo 0600 must survive relay usage, got {fifo_mode:o}"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::FileTypeExt;
            assert!(
                meta.file_type().is_fifo(),
                "stop.fifo must still be a named pipe"
            );
        }
    }

    /// Neither mode may lean on a restrictive umask: under a hostile umask 000,
    /// an unpinned mkdir would land the dir world-writable. Mirrors
    /// `serve_socket_is_owner_only_regardless_of_umask` in tests/serve.rs. The
    /// flip is process-wide but held only across `HookInstaller::new`; the
    /// pinned modes keep every concurrent hook test umask-tolerant in that
    /// window (no other test in this binary asserts a mode it doesn't set
    /// explicitly).
    #[test]
    fn artifact_modes_hold_under_hostile_umask() {
        let previous_mask = unsafe { libc::umask(0o000) };
        let installer = HookInstaller::new().unwrap();
        unsafe { libc::umask(previous_mask) };

        use std::os::unix::fs::PermissionsExt;
        let dir_mode = std::fs::metadata(installer.dir_path())
            .expect("temp dir metadata")
            .permissions()
            .mode();
        assert_eq!(
            dir_mode & 0o777,
            0o700,
            "temp dir must be owner-only even under umask 000, got {dir_mode:o}"
        );
        let fifo_mode = std::fs::metadata(&installer.fifo_path)
            .expect("FIFO metadata")
            .permissions()
            .mode();
        assert_eq!(
            fifo_mode & 0o777,
            0o600,
            "stop.fifo must be owner-rw even under umask 000, got {fifo_mode:o}"
        );
    }

    #[test]
    fn temp_dir_cleaned_up_on_drop() {
        let path = {
            let installer = HookInstaller::new().unwrap();
            installer.dir_path().to_path_buf()
        };
        assert!(!path.exists(), "temp dir must be removed after drop");
    }

    #[test]
    fn cleanup_explicitly_removes_fifo() {
        let installer = HookInstaller::new().unwrap();
        let fifo_path = installer.fifo_path.clone();
        let dir_path = installer.dir_path().to_path_buf();

        // Call cleanup explicitly
        installer.cleanup();

        // FIFO should be removed
        assert!(!fifo_path.exists(), "FIFO must be removed after cleanup");

        // Temp dir should still exist (owned by installer)
        // but will be cleaned when installer is dropped
        drop(installer);
        assert!(!dir_path.exists(), "temp dir must be removed after drop");
    }

    /// Set a path's mtime to `target` via FileTimes (works on a read-only fd
    /// for a dir/file the test process owns, as tempdir-created paths are).
    /// Mirrors the helper in check.rs.
    fn set_mtime(path: &Path, target: SystemTime) {
        let f = std::fs::File::open(path).expect("open for set_times");
        let times = std::fs::FileTimes::new().set_modified(target);
        f.set_times(times).expect("set_times");
    }

    #[test]
    fn owner_pid_parses_from_dir_name() {
        assert_eq!(
            owner_pid_from_name("claude-print-12345-AbCdEf"),
            Some(12345)
        );
        assert_eq!(owner_pid_from_name("claude-print-1-x"), Some(1));
        assert_eq!(owner_pid_from_name("claude-print-notanum-x"), None);
        assert_eq!(owner_pid_from_name("something-else"), None);
    }

    #[test]
    fn cleanup_orphans_does_not_panic() {
        // Smoke test against the real $TMPDIR — runs the production path.
        crate::hook::cleanup_orphans();
    }

    /// bf-kk4z: a temp dir aged well past the threshold whose embedded PID is a
    /// LIVE process (the test process itself) must survive cleanup.
    #[test]
    fn cleanup_preserves_temp_dir_with_live_owner_pid() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let now = SystemTime::now();
        let threshold = Duration::from_secs(60);

        // The test process is always alive — its PID is a live owner.
        let live_pid = std::process::id();
        let live_dir = dir.join(format!("claude-print-{}-live", live_pid));
        std::fs::create_dir(&live_dir).unwrap();
        set_mtime(&live_dir, now - Duration::from_secs(600));

        cleanup_orphans_in(dir, now, threshold, &is_live_process);

        assert!(
            live_dir.exists(),
            "must NOT delete a temp dir whose embedded PID is a live process"
        );
    }

    /// bf-kk4z: a temp dir aged past the threshold whose embedded PID is NOT a
    /// running process (a reaped child) must be removed.
    #[test]
    fn cleanup_removes_temp_dir_with_dead_owner_pid() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let now = SystemTime::now();
        let threshold = Duration::from_secs(60);

        // Spawn a child and reap it → its PID is no longer running. (Reuse
        // within this microsecond window is astronomically unlikely.)
        let dead_pid = {
            let mut child = std::process::Command::new("true")
                .spawn()
                .expect("spawn true");
            child.wait().expect("wait true");
            child.id()
        };
        assert!(
            !is_live_process(dead_pid),
            "precondition: reaped child PID must be dead"
        );

        let orphan = dir.join(format!("claude-print-{}-dead", dead_pid));
        std::fs::create_dir(&orphan).unwrap();
        set_mtime(&orphan, now - Duration::from_secs(600));

        cleanup_orphans_in(dir, now, threshold, &is_live_process);

        assert!(
            !orphan.exists(),
            "must delete a temp dir whose owner is dead and which is aged past the threshold"
        );
    }

    /// A young dir (under the threshold) is never deleted, regardless of owner.
    #[test]
    fn cleanup_leaves_young_dirs_alone() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let now = SystemTime::now();
        let threshold = Duration::from_secs(60);

        let young = dir.join("claude-print-999999999-young");
        std::fs::create_dir(&young).unwrap();
        // Just-created → age ~0, well under threshold.

        cleanup_orphans_in(dir, now, threshold, &|_| false);

        assert!(young.exists(), "young dirs must be left alone");
    }

    #[test]
    fn cleanup_can_be_called_multiple_times() {
        let installer = HookInstaller::new().unwrap();
        installer.cleanup();
        installer.cleanup(); // Should not panic
        drop(installer);
    }

    /// bf-5sj7: verify that FIFO paths containing shell metacharacters are
    /// properly escaped in the generated hook.sh. This prevents command injection
    /// when temp directory paths contain special characters.
    #[test]
    fn hook_sh_escaping_handles_shell_metacharacters() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();

        // Test various dangerous shell metacharacters
        let test_cases = vec![
            "normal_path",
            "path'with'quotes",         // Single quotes
            "path$with$dollar",         // Dollar signs (variable expansion)
            "path`with`backticks",      // Backticks (command substitution)
            r"path\with\backslash",     // Backslashes
            "path;with;semicolons",     // Semicolons (command separators)
            "path&with&ampersands",     // Ampersands (background operators)
            "path|with|pipes",          // Pipes (command chaining)
            "path\nwith\nnewlines",     // Newlines (command separators)
            r"path\with\mixed'quotes$", // Mixed special characters
        ];

        for test_name in test_cases {
            // Create a fifo path that simulates a temp dir with metacharacters
            let test_dir = dir.join(format!("claude-print-test-{}", test_name.replace('/', "_")));
            std::fs::create_dir(&test_dir).expect("create test dir");
            let fifo_path = test_dir.join("stop.fifo");
            let hook_path = test_dir.join("hook.sh");

            // Write the hook script (this is where escaping happens)
            write_hook_sh(&hook_path, &fifo_path).expect("write_hook_sh should handle all paths");

            // Verify the script is syntactically valid by checking it with sh -n
            let output = std::process::Command::new("sh")
                .arg("-n") // Syntax check only, don't execute
                .arg(&hook_path)
                .output();

            assert!(
                output.is_ok(),
                "hook.sh for path {:?} should be syntactically valid; stderr: {:?}",
                test_name,
                output.as_ref().err().map(|e| e.to_string())
            );

            let output = output.unwrap();
            assert!(
                output.status.success(),
                "hook.sh for path {:?} should pass shell syntax check; stdout: {}, stderr: {}",
                test_name,
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );

            // Clean up
            std::fs::remove_file(&hook_path).ok();
            std::fs::remove_dir(&test_dir).ok();
        }
    }

    /// bf-5sj7: verify that the actual hook.sh produced by HookInstaller can
    /// safely execute when the temp dir path contains shell metacharacters.
    /// This test requires a real TempDir with a controlled prefix.
    #[test]
    fn hook_sh_safely_escapes_real_tempdir_paths() {
        // Create a hook installer (which creates a real temp dir and hook.sh)
        let installer = HookInstaller::new().unwrap();

        // Read the generated hook.sh
        let hook_content =
            std::fs::read_to_string(&installer.hook_path).expect("hook.sh should be readable");

        // Verify the script is syntactically valid shell
        let output = std::process::Command::new("sh")
            .arg("-n")
            .arg(&installer.hook_path)
            .output()
            .expect("sh -n should execute");

        assert!(
            output.status.success(),
            "Generated hook.sh should pass shell syntax check; stdout: {}, stderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );

        // Verify the hook script contains the expected pattern
        assert!(hook_content.contains("#!/bin/sh"));
        assert!(hook_content.contains("cat > '"));
        assert!(hook_content.contains(" 2>/dev/null || true"));

        // The key security property: if the FIFO path contained a single quote,
        // it should be escaped as '\'' which splits into: '...'\''
        // Let's verify that any single quotes in the path are properly escaped
        let fifo_str = installer.fifo_path.to_string_lossy();
        if fifo_str.contains('\'') {
            // If the path has quotes, verify they're escaped in the script
            // The escape pattern is: ' -> '\''
            assert!(
                hook_content.contains("'\\''"),
                "Single quotes should be escaped"
            );
        }
    }

    /// bf-5sj7: verify that hook.sh can actually write to the FIFO when
    /// executed, proving that the escaping doesn't break functionality.
    #[test]
    fn hook_sh_can_write_to_fifo_after_escaping() {
        let installer = HookInstaller::new().unwrap();

        // Start a background reader on the FIFO
        let fifo_path = installer.fifo_path.clone();
        let reader_handle = std::thread::spawn(move || {
            // Open the FIFO for reading (this blocks until a writer opens it)
            use std::io::Read;
            let mut file = std::fs::File::open(&fifo_path).expect("open FIFO for reading");
            let mut buffer = Vec::new();
            file.read_to_end(&mut buffer).expect("read from FIFO");
            buffer
        });

        // Give the reader a moment to start and block on the FIFO
        std::thread::sleep(std::time::Duration::from_millis(50));

        // Execute the hook.sh script (it should write "test data" to the FIFO)
        let mut output = std::process::Command::new("sh")
            .arg(&installer.hook_path)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("spawn hook.sh");

        // Write test data to the hook script's stdin
        {
            let mut stdin = output.stdin.as_mut().expect("open stdin");
            std::io::Write::write_all(&mut stdin, b"test data from hook\n")
                .expect("write to hook stdin");
        } // Close stdin by dropping here to signal EOF

        // Wait for the hook script to complete
        let status = output.wait_with_output().expect("wait for hook.sh");
        assert!(
            status.status.success(),
            "hook.sh should execute successfully"
        );

        // Wait for the reader to finish and get the data
        let received_data = reader_handle.join().expect("reader thread should complete");

        // Verify the data was written correctly
        assert_eq!(
            received_data, b"test data from hook\n",
            "FIFO should receive the data written by hook.sh"
        );
    }

    /// bf-5sj7: direct unit test of the shell escaping logic.
    /// Verify that single quotes are replaced with '\'' which is the
    /// canonical shell escaping pattern.
    #[test]
    fn shell_escaping_pattern_is_correct() {
        let fifo_str = "/tmp/test'path'with'quotes";
        let escaped = fifo_str.replace('\'', "'\\''");

        // The escaped string should be: /tmp/test'\''path'\''with'\''quotes
        // This produces: '...'\''...'\''...'\''...
        assert_eq!(escaped, "/tmp/test'\\''path'\\''with'\\''quotes");

        // When we format it into the shell script, we get:
        // cat > '/tmp/test'\''path'\''with'\''quotes' 2>/dev/null || true
        // This is valid shell syntax that reconstructs the original path
        let content = format!("#!/bin/sh\ncat > '{}' 2>/dev/null || true\n", escaped);
        assert!(content.contains("'\\''"));
    }
}
