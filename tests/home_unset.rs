//! End-to-end and cross-module coverage for the process-wide `HOME` contract.
//!
//! `config` and `poller` both depend on the shared strict resolver: an unset or
//! empty value is rejected, while any non-empty path is preserved without an
//! eager filesystem check. The latter matters in containers and chroots where
//! the home hierarchy may be mounted or created after path resolution.
//!
//! Tests which mutate this process's environment take one lock. Binary tests
//! use child-only environment overrides, matching `env -u HOME claude-print`.

use claude_print::config::Config;
use claude_print::poller::{derive_transcript_path, projects_dir_for_cwd};
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Mutex, MutexGuard};

const HOME_ERROR_DETAIL: &str =
    "HOME environment variable not set or empty; set HOME to the user's home directory";

static ENV_LOCK: Mutex<()> = Mutex::new(());

fn env_lock() -> MutexGuard<'static, ()> {
    ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Restores an environment variable even when an assertion panics.
struct EnvGuard {
    key: &'static str,
    previous: Option<OsString>,
}

impl EnvGuard {
    fn set(key: &'static str, value: impl AsRef<OsStr>) -> Self {
        let guard = Self {
            key,
            previous: std::env::var_os(key),
        };
        std::env::set_var(key, value);
        guard
    }

    fn remove(key: &'static str) -> Self {
        let guard = Self {
            key,
            previous: std::env::var_os(key),
        };
        std::env::remove_var(key);
        guard
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        match &self.previous {
            Some(value) => std::env::set_var(self.key, value),
            None => std::env::remove_var(self.key),
        }
    }
}

struct CwdGuard(PathBuf);

impl CwdGuard {
    fn set(path: &Path) -> Self {
        let previous = std::env::current_dir().expect("read current directory");
        std::env::set_current_dir(path).expect("enter test directory");
        Self(previous)
    }
}

impl Drop for CwdGuard {
    fn drop(&mut self) {
        std::env::set_current_dir(&self.0).expect("restore current directory");
    }
}

fn assert_shared_home_error() {
    let config_error = Config::default_path().unwrap_err().to_string();
    let transcript_error = derive_transcript_path("session-id", "/workspace")
        .unwrap_err()
        .to_string();
    let projects_error = projects_dir_for_cwd().unwrap_err().to_string();

    // Regression guard for the original inconsistency: config used to reject a
    // missing HOME while poller silently derived transcript paths under /root.
    assert!(config_error.contains(HOME_ERROR_DETAIL));
    assert_eq!(transcript_error, config_error);
    assert_eq!(projects_error, config_error);
}

#[test]
fn unset_home_is_rejected_identically_by_config_and_poller() {
    let _lock = env_lock();
    let _home = EnvGuard::remove("HOME");
    let _xdg = EnvGuard::remove("XDG_CONFIG_HOME");

    assert_shared_home_error();
}

#[test]
fn empty_home_is_equivalent_to_unset_home() {
    let _lock = env_lock();
    let _home = EnvGuard::set("HOME", "");
    let _xdg = EnvGuard::remove("XDG_CONFIG_HOME");

    assert_shared_home_error();
}

#[test]
fn valid_home_roots_every_derived_path() {
    let _lock = env_lock();
    let home = tempfile::tempdir().expect("create HOME");
    let _home = EnvGuard::set("HOME", home.path());
    let _xdg = EnvGuard::remove("XDG_CONFIG_HOME");

    assert_eq!(
        Config::default_path().unwrap(),
        home.path().join(".config/claude-print/config.toml")
    );
    assert_eq!(
        derive_transcript_path("session-id", "/srv/project").unwrap(),
        home.path()
            .join(".claude/projects/srv-project/session-id.jsonl")
    );
    assert!(projects_dir_for_cwd()
        .unwrap()
        .starts_with(home.path().join(".claude/projects")));
}

#[test]
fn nonexistent_home_is_accepted_consistently_without_eager_io() {
    let _lock = env_lock();
    let root = tempfile::tempdir().expect("create isolated root");
    let missing_home = root.path().join("not-mounted/home/service");
    assert!(!missing_home.exists());
    let _home = EnvGuard::set("HOME", &missing_home);
    let _xdg = EnvGuard::remove("XDG_CONFIG_HOME");

    let config_path = Config::default_path().unwrap();
    let transcript_path = derive_transcript_path("session-id", "/srv/project").unwrap();
    let projects_path = projects_dir_for_cwd().unwrap();

    assert_eq!(
        config_path,
        missing_home.join(".config/claude-print/config.toml")
    );
    assert_eq!(
        transcript_path,
        missing_home.join(".claude/projects/srv-project/session-id.jsonl")
    );
    assert!(projects_path.starts_with(missing_home.join(".claude/projects")));
    assert!(
        Config::load_or_default(&config_path)
            .unwrap()
            .defaults
            .is_none(),
        "a not-yet-created HOME should behave like a missing optional config"
    );
}

#[test]
fn chroot_like_layout_never_falls_back_to_root_home() {
    let _lock = env_lock();
    let jail = tempfile::tempdir().expect("create fake chroot");
    let home = jail.path().join("home/service");
    let workspace = jail.path().join("work/app");
    std::fs::create_dir_all(&home).expect("create fake chroot HOME");
    std::fs::create_dir_all(&workspace).expect("create fake chroot workspace");
    assert!(!jail.path().join("root").exists());

    let _home = EnvGuard::set("HOME", &home);
    let _xdg = EnvGuard::remove("XDG_CONFIG_HOME");
    let _cwd = CwdGuard::set(&workspace);

    let config_path = Config::default_path().unwrap();
    let transcript_path = derive_transcript_path("session-id", "/work/app").unwrap();
    let projects_path = projects_dir_for_cwd().unwrap();

    for path in [&config_path, &transcript_path, &projects_path] {
        assert!(
            path.starts_with(&home),
            "{} escaped fake HOME",
            path.display()
        );
        assert!(
            !path.starts_with("/root"),
            "{} used /root fallback",
            path.display()
        );
    }
}

#[test]
fn binary_reports_actionable_home_error_in_every_output_format() {
    let binary = PathBuf::from(env!("CARGO_BIN_EXE_claude-print"));
    // Config loading fails before the backend can start. An existing executable
    // is sufficient to pass the earlier binary-presence check; using this test
    // executable keeps the regression hermetic without a mock session.
    let unused_backend = std::env::current_exe().expect("locate test executable");
    assert!(binary.exists(), "missing binary at {}", binary.display());

    for format in ["text", "json", "stream-json"] {
        let output = Command::new(&binary)
            .arg("--claude-binary")
            .arg(&unused_backend)
            .arg("--output-format")
            .arg(format)
            .arg("test prompt")
            .env_remove("HOME")
            .env_remove("XDG_CONFIG_HOME")
            .stdin(Stdio::null())
            .output()
            .expect("run claude-print with HOME unset");

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let combined = format!("{stdout}{stderr}");
        assert_eq!(
            output.status.code(),
            Some(2),
            "{format} output unexpectedly succeeded: {combined}"
        );
        assert!(
            combined.contains(HOME_ERROR_DETAIL),
            "{format} output did not explain how to fix HOME: {combined}"
        );
    }
}
