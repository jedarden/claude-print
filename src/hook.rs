use crate::error::{Error, Result};
use nix::sys::stat::Mode;
use nix::unistd::mkfifo;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

pub struct HookInstaller {
    pub dir: TempDir,
    pub settings_path: PathBuf,
    pub hook_path: PathBuf,
    pub fifo_path: PathBuf,
}

impl HookInstaller {
    pub fn new() -> Result<Self> {
        // Clean up any orphaned temp dirs from previous crashed runs
        Self::cleanup_orphans();

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
        })
    }

    pub fn dir_path(&self) -> &Path {
        self.dir.path()
    }

    /// Explicitly clean up the temporary directory and FIFO.
    ///
    /// This is called automatically on Drop, but can be called explicitly
    /// to ensure cleanup on all exit paths (normal, error, timeout, signal).
    pub fn cleanup(&self) {
        // Remove the FIFO first (it may have different permissions)
        let _ = std::fs::remove_file(&self.fifo_path);
        // Note: TempDir's Drop will handle the rest when self.dir is dropped
        // We don't call close() here because it takes ownership
    }

    /// Sweep and remove orphaned temp directories from previous crashed runs.
    ///
    /// This looks for directories matching the pattern `claude-print-*` in the
    /// system temp directory and removes any that are older than 1 hour.
    /// This prevents accumulation of stale temp dirs from crashes.
    pub fn cleanup_orphans() {
        let temp_dir = std::env::temp_dir();

        if let Ok(entries) = std::fs::read_dir(&temp_dir) {
            for entry in entries.filter_map(|e| e.ok()) {
                let path = entry.path();

                // Check if the entry name matches our pattern
                let name = path.file_name().and_then(|n| n.to_str());
                if let Some(name) = name {
                    if name.starts_with("claude-print-") {
                        // Check if it's a directory and old enough (> 1 hour)
                        if let Ok(metadata) = entry.metadata() {
                            if metadata.is_dir() {
                                if let Ok(created) = metadata.created() {
                                    if let Ok(age) = created.elapsed() {
                                        // Only remove if older than 1 hour to avoid
                                        // deleting active sessions from other processes
                                        if age > std::time::Duration::from_secs(3600) {
                                            let _ = std::fs::remove_dir_all(&path);
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
        HookInstaller::cleanup_orphans();
    }

    #[test]
    fn cleanup_can_be_called_multiple_times() {
        let installer = HookInstaller::new().unwrap();
        installer.cleanup();
        installer.cleanup(); // Should not panic
        drop(installer);
    }
}
