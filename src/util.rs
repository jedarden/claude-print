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
///
/// [`var_os`](std::env::var_os) is used so valid non-UTF-8 Unix paths are preserved.
///
/// # Errors
///
/// Returns [`Error::Config`] when `HOME` is unset or empty.
pub fn get_home() -> Result<PathBuf> {
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
        assert_eq!(
            error.to_string(),
            "config error: HOME environment variable not set or empty; set HOME to the user's home directory"
        );
    }

    #[test]
    fn rejects_empty_home() {
        let error = home_from_env_value(Some(OsString::new())).unwrap_err();
        assert!(error
            .to_string()
            .contains("HOME environment variable not set"));
    }
}
