use crate::error::{Error, Result};
use nix::sys::stat::Mode;
use nix::unistd::mkfifo;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tempfile::TempDir;

/// Sweep and remove orphaned temp directories from previous crashed runs.
///
/// This looks for directories matching the pattern `claude-print-*` in the
/// system temp directory and removes any that are older than 60 seconds.
/// This prevents accumulation of stale temp dirs from crashes.
///
/// This function is called at the start of main() to ensure orphans are
/// cleaned up on all invocations, not just when a session runs.
pub fn cleanup_orphans() {
    let temp_dir = std::env::temp_dir();

    if let Ok(entries) = std::fs::read_dir(&temp_dir) {
        for entry in entries.filter_map(|e| e.ok()) {
            let path = entry.path();

            // Check if the entry name matches our pattern
            let name = path.file_name().and_then(|n| n.to_str());
            if let Some(name) = name {
                if name.starts_with("claude-print-") {
                    // Check if it's a directory and old enough (> 60 seconds)
                    if let Ok(metadata) = entry.metadata() {
                        if metadata.is_dir() {
                            if let Ok(created) = metadata.created() {
                                if let Ok(age) = created.elapsed() {
                                    // Remove if older than 60 seconds
                                    // Shorter threshold prevents orphans from accumulating
                                    // while avoiding deletion of active instances
                                    if age > std::time::Duration::from_secs(60) {
                                        // Try to remove the FIFO first if it exists
                                        let fifo_path = path.join("stop.fifo");
                                        if let Err(e) = std::fs::remove_file(&fifo_path) {
                                            eprintln!("claude-print: warning: failed to remove FIFO {:?}: {}", fifo_path, e);
                                        }
                                        // Remove the entire temp directory
                                        if let Err(e) = std::fs::remove_dir_all(&path) {
                                            eprintln!("claude-print: warning: failed to remove orphaned temp dir {:?}: {}", path, e);
                                        } else {
                                            eprintln!(
                                                "claude-print: cleaned up orphaned temp dir: {:?}",
                                                path
                                            );
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
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

    #[test]
    fn cleanup_orphans_does_not_panic() {
        // This test verifies that cleanup_orphans() doesn't panic
        // It's hard to test the actual behavior without creating real orphans,
        // but we can at least verify it runs without error
        crate::hook::cleanup_orphans();
    }

    #[test]
    fn cleanup_can_be_called_multiple_times() {
        let installer = HookInstaller::new().unwrap();
        installer.cleanup();
        installer.cleanup(); // Should not panic
        drop(installer);
    }
}
