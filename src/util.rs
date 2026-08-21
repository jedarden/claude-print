use crate::error::{Error, Result};
use std::ffi::OsString;
use std::io::Write;
use std::path::{Path, PathBuf};

/// Resolves the current user's home directory from `HOME`.
///
/// HOME handling is intentionally strict. Claude's configuration and transcripts
/// belong to the current user, so guessing a directory could read or write another
/// user's data. In particular, `/root` is not a portable fallback: it can be absent
/// in a chroot or container and is incorrect whenever the process is not root.
/// Callers therefore receive a clear configuration error instead of a guessed path.
/// This helper is the sole production access to `HOME`; modules that resolve
/// user-owned paths must use it so the policy cannot drift between call sites.
///
/// Chroots and minimal containers must provision `HOME` before starting
/// `claude-print`. The path must already exist, be a directory, and permit a
/// temporary file to be created and written. A short-lived hidden probe verifies
/// the same operation Claude Code needs for trust state and transcripts; it is
/// removed before this function returns. Missing paths, search-permission failures,
/// Unix read-only permission modes, read-only mounts, and other write failures are
/// reported against the configured path. None cause a fallback to `/root`, the
/// passwd database, or the current directory.
///
/// [`var_os`](std::env::var_os) is used so valid non-UTF-8 Unix paths are preserved.
///
/// # Errors
///
/// Returns [`Error::Config`] when `HOME` is unset or empty, does not name an
/// accessible directory, or is not writable.
pub fn get_home() -> Result<PathBuf> {
    // Keep the environment read here so every production caller gets the strict
    // validation and none can introduce an implicit fallback.
    let home = home_from_env_value(std::env::var_os("HOME"))?;
    validate_home_path(&home)?;
    Ok(home)
}

fn home_from_env_value(home: Option<OsString>) -> Result<PathBuf> {
    home.filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| {
            Error::Config(
                "HOME environment variable not set or empty; set HOME to the user's home directory"
                    .to_string(),
            )
        })
}

fn validate_home_path(home: &Path) -> Result<()> {
    validate_home_path_with_probe(home, write_home_probe)
}

fn validate_home_path_with_probe(
    home: &Path,
    write_probe: impl FnOnce(&Path) -> std::io::Result<()>,
) -> Result<()> {
    let metadata = std::fs::metadata(home).map_err(|error| {
        Error::Config(format!(
            "HOME path '{}' is not accessible: {error}; set HOME to an existing, writable directory",
            home.display()
        ))
    })?;

    if !metadata.is_dir() {
        return Err(Error::Config(format!(
            "HOME path '{}' is not a directory; set HOME to an existing, writable directory",
            home.display()
        )));
    }

    // This deterministic check also works when tests (or the caller) run as
    // root, which can otherwise bypass ordinary Unix mode-bit write checks.
    if metadata.permissions().readonly() {
        return Err(home_not_writable(
            home,
            "directory permissions are read-only",
        ));
    }

    // Mode bits alone cannot detect ACL denial, a read-only mount, a full
    // filesystem, or a remote filesystem policy. Exercise an actual create and
    // write, then close and unlink the probe before returning.
    write_probe(home).map_err(|error| home_not_writable(home, &error.to_string()))?;

    Ok(())
}

fn write_home_probe(home: &Path) -> std::io::Result<()> {
    let mut probe = tempfile::Builder::new()
        .prefix(".claude-print-home-check-")
        .tempfile_in(home)?;
    probe.write_all(b"claude-print HOME write check\n")?;
    probe.close()
}

fn home_not_writable(home: &Path, reason: &str) -> Error {
    Error::Config(format!(
        "HOME path '{}' is not writable: {reason}; grant write permission or set HOME to an existing, writable directory",
        home.display()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_configured_home() {
        assert_eq!(
            home_from_env_value(Some(OsString::from("/home/alice"))).unwrap(),
            PathBuf::from("/home/alice")
        );
    }

    #[test]
    fn accepts_existing_writable_home() {
        let home = tempfile::tempdir().unwrap();
        validate_home_path(home.path()).unwrap();
        assert_eq!(std::fs::read_dir(home.path()).unwrap().count(), 0);
    }

    #[test]
    fn rejects_unset_home() {
        let error = home_from_env_value(None).unwrap_err();
        let Error::Config(message) = error else {
            panic!("unset HOME should produce Error::Config, got {error:?}");
        };
        assert_eq!(
            message,
            "HOME environment variable not set or empty; set HOME to the user's home directory"
        );
    }

    #[test]
    fn rejects_empty_home() {
        let error = home_from_env_value(Some(OsString::new())).unwrap_err();
        assert!(error
            .to_string()
            .contains("HOME environment variable not set"));
    }

    #[test]
    fn rejects_nonexistent_home_with_actionable_path_error() {
        let root = tempfile::tempdir().unwrap();
        let missing_home = root.path().join("not-mounted/home/service");
        assert!(!missing_home.exists());

        let error = validate_home_path(&missing_home).unwrap_err().to_string();
        assert!(error.contains(&missing_home.display().to_string()));
        assert!(error.contains("not accessible"));
        assert!(error.contains("existing, writable directory"));
        assert!(!error.contains("/root"));
    }

    #[test]
    fn rejects_regular_file_as_home() {
        let root = tempfile::tempdir().unwrap();
        let file = root.path().join("home-is-a-file");
        std::fs::write(&file, b"not a directory").unwrap();

        let error = validate_home_path(&file).unwrap_err().to_string();
        assert!(error.contains(&file.display().to_string()));
        assert!(error.contains("not a directory"));
    }

    #[cfg(unix)]
    #[test]
    fn rejects_read_only_home_with_write_permission_error() {
        use std::os::unix::fs::PermissionsExt;

        let home = tempfile::tempdir().unwrap();
        let original_mode = std::fs::metadata(home.path()).unwrap().permissions().mode();
        std::fs::set_permissions(home.path(), std::fs::Permissions::from_mode(0o555)).unwrap();

        let error = validate_home_path(home.path()).unwrap_err().to_string();

        std::fs::set_permissions(home.path(), std::fs::Permissions::from_mode(original_mode))
            .unwrap();
        assert!(error.contains(&home.path().display().to_string()));
        assert!(error.contains("not writable"));
        assert!(error.contains("write permission"));
    }

    #[cfg(unix)]
    #[test]
    fn read_only_mount_probe_error_is_actionable() {
        let home = tempfile::tempdir().unwrap();
        let error = validate_home_path_with_probe(home.path(), |_| {
            Err(std::io::Error::from_raw_os_error(libc::EROFS))
        })
        .unwrap_err()
        .to_string();

        assert!(error.contains(&home.path().display().to_string()));
        assert!(error.contains("not writable"));
        assert!(error.contains("Read-only file system"));
        assert!(error.contains("grant write permission"));
    }

    #[cfg(unix)]
    #[test]
    fn preserves_non_utf8_home_paths() {
        use std::os::unix::ffi::OsStringExt;

        let root = tempfile::tempdir().unwrap();
        let segment = OsString::from_vec(b"non-utf8-\xff".to_vec());
        let home = root.path().join(segment);
        std::fs::create_dir(&home).unwrap();

        validate_home_path(&home).unwrap();
        assert_eq!(
            home_from_env_value(Some(home.as_os_str().to_owned())).unwrap(),
            home
        );
    }
}
