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

pub struct HookInstaller {
    pub dir: TempDir,
    pub settings_path: PathBuf,
    pub hook_path: PathBuf,
    pub fifo_path: PathBuf,
    /// Flag to track whether cleanup has already been performed.
    /// This prevents double-panic issues during cleanup.
    cleanup_performed: Arc<AtomicBool>,
}

impl HookInstaller {
    pub fn new() -> Result<Self> {
        let dir = tempfile::Builder::new()
            .prefix(&format!("claude-print-{}-", std::process::id()))
            .tempdir()
            .map_err(|e| Error::Internal(anyhow::anyhow!("failed to create temp dir: {e}")))?;

        let dir_str = dir.path().to_string_lossy();
        if dir_str.contains('\'') {
            return Err(Error::Internal(anyhow::anyhow!(
                "temp dir path contains single-quote: {dir_str}"
            )));
        }

        let settings_path = dir.path().join("settings.json");
        let hook_path = dir.path().join("hook.sh");
        let fifo_path = dir.path().join("stop.fifo");

        write_hook_sh(&hook_path, &fifo_path)?;
        write_settings_json(&settings_path, &hook_path)?;

        mkfifo(&fifo_path, Mode::S_IRUSR | Mode::S_IWUSR)
            .map_err(|e| Error::Internal(anyhow::anyhow!("mkfifo failed: {e}")))?;

        Ok(HookInstaller {
            dir,
            settings_path,
            hook_path,
            fifo_path,
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

fn write_hook_sh(hook_path: &Path, fifo_path: &Path) -> Result<()> {
    let fifo_str = fifo_path.to_string_lossy();
    let content = format!("#!/bin/sh\ncat > '{}' 2>/dev/null || true\n", fifo_str);
    std::fs::write(hook_path, &content)
        .map_err(|e| Error::Internal(anyhow::anyhow!("failed to write hook.sh: {e}")))?;

    // Make executable
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(hook_path)
            .map_err(|e| Error::Internal(anyhow::anyhow!("stat hook.sh: {e}")))?
            .permissions();
        perms.set_mode(0o750);
        std::fs::set_permissions(hook_path, perms)
            .map_err(|e| Error::Internal(anyhow::anyhow!("chmod hook.sh: {e}")))?;
    }

    Ok(())
}

fn write_settings_json(settings_path: &Path, hook_path: &Path) -> Result<()> {
    let hook_str = hook_path.to_string_lossy();
    let json = serde_json::json!({
        "hooks": {
            "Stop": [{
                "hooks": [{"type": "command", "command": hook_str, "timeout": 10}]
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
}
