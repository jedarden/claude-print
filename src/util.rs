use crate::error::{Error, Result};
use std::ffi::OsString;
use std::path::PathBuf;

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
/// [`var_os`](std::env::var_os) is used so valid non-UTF-8 Unix paths are preserved.
///
/// # Errors
///
/// Returns [`Error::Config`] when `HOME` is unset or empty.
pub fn get_home() -> Result<PathBuf> {
    // Keep the environment read here so every production caller gets the strict
    // unset-or-empty check and none can introduce an implicit fallback.
    home_from_env_value(std::env::var_os("HOME"))
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
    fn preserves_nonexistent_home_without_eager_filesystem_checks() {
        let root = tempfile::tempdir().unwrap();
        let missing_home = root.path().join("not-mounted/home/service");
        assert!(!missing_home.exists());

        assert_eq!(
            home_from_env_value(Some(missing_home.as_os_str().to_owned())).unwrap(),
            missing_home
        );
    }

    #[cfg(unix)]
    #[test]
    fn preserves_non_utf8_home_paths() {
        use std::os::unix::ffi::OsStringExt;

        let home = OsString::from_vec(b"/home/non-utf8-\xff".to_vec());
        assert_eq!(
            home_from_env_value(Some(home.clone())).unwrap(),
            PathBuf::from(home)
        );
    }
}
