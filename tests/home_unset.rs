//! End-to-end and cross-module coverage for the process-wide `HOME` contract.
//!
//! `config`, `poller`, and `session` depend on the shared strict resolver: HOME
//! must name an existing, writable directory. Chroots and containers get a
//! path-specific setup error instead of an implicit `/root` fallback.
//!
//! Tests which mutate this process's environment take one lock. Binary tests
//! use child-only environment overrides, matching `env -u HOME claude-print`.

use claude_print::cli::OutputFormat;
use claude_print::config::Config;
use claude_print::poller::{derive_transcript_path, projects_dir_for_cwd};
use claude_print::session::{LaunchOptions, Session};
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Mutex, MutexGuard};

const HOME_ERROR_DETAIL: &str =
    "HOME environment variable not set or empty; set HOME to the user's home directory";
const HOME_ERROR_STDERR: &str =
    "error: invalid config: HOME environment variable not set or empty; set HOME to the user's home directory\n";

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
    let session_error = Session::run(
        Path::new("/unused/claude"),
        &[],
        b"unused prompt".to_vec(),
        None,
        None,
        None,
        None,
        OutputFormat::Text,
        &LaunchOptions::default(),
    )
    .unwrap_err()
    .to_string();

    // Regression guard for the original inconsistency: config used to reject a
    // missing HOME while other paths could continue or derive paths under /root.
    assert!(config_error.contains(HOME_ERROR_DETAIL));
    assert_eq!(transcript_error, config_error);
    assert_eq!(projects_error, config_error);
    assert_eq!(session_error, config_error);
}

#[test]
fn unset_home_is_rejected_identically_by_config_poller_and_session() {
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
fn nonexistent_home_is_rejected_consistently_with_path_context() {
    let _lock = env_lock();
    let root = tempfile::tempdir().expect("create isolated root");
    let missing_home = root.path().join("not-mounted/home/service");
    assert!(!missing_home.exists());
    let _home = EnvGuard::set("HOME", &missing_home);
    let _xdg = EnvGuard::remove("XDG_CONFIG_HOME");

    for error in [
        Config::default_path().unwrap_err().to_string(),
        derive_transcript_path("session-id", "/srv/project")
            .unwrap_err()
            .to_string(),
        projects_dir_for_cwd().unwrap_err().to_string(),
        Session::run(
            Path::new("/unused/claude"),
            &[],
            b"unused prompt".to_vec(),
            None,
            None,
            None,
            None,
            OutputFormat::Text,
            &LaunchOptions::default(),
        )
        .unwrap_err()
        .to_string(),
    ] {
        assert!(
            error.contains(&missing_home.display().to_string()),
            "{error}"
        );
        assert!(error.contains("not accessible"), "{error}");
        assert!(error.contains("existing, writable directory"), "{error}");
        assert!(!error.contains("/root"), "unexpected fallback: {error}");
    }
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

#[cfg(target_os = "linux")]
fn command_on_path(name: &str) -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path)
            .map(|directory| directory.join(name))
            .find(|candidate| candidate.is_file())
    })
}

#[cfg(target_os = "linux")]
fn copy_file_into_chroot(source: &Path, destination: &Path, chroot: &Path) {
    let relative_destination = destination.strip_prefix("/").unwrap_or_else(|_| {
        panic!(
            "chroot destination must be absolute: {}",
            destination.display()
        )
    });
    let destination = chroot.join(relative_destination);
    std::fs::create_dir_all(destination.parent().expect("destination has parent"))
        .unwrap_or_else(|error| panic!("create {}: {error}", destination.display()));
    std::fs::copy(source, &destination).unwrap_or_else(|error| {
        panic!(
            "copy {} into chroot at {}: {error}",
            source.display(),
            destination.display()
        )
    });
}

/// Copy every absolute dependency reported by `ldd`, including the ELF
/// interpreter. Static binaries need no additional files.
#[cfg(target_os = "linux")]
fn install_binary_in_chroot(binary: &Path, ldd: &Path, chroot: &Path) {
    copy_file_into_chroot(binary, Path::new("/bin/claude-print"), chroot);

    let output = Command::new(ldd)
        .arg(binary)
        .output()
        .unwrap_or_else(|error| panic!("inspect {} with ldd: {error}", binary.display()));
    let report = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    if !output.status.success() {
        if report.contains("not a dynamic executable") || report.contains("statically linked") {
            return;
        }
        panic!("ldd failed for {}: {report}", binary.display());
    }
    assert!(
        !report.contains("=> not found"),
        "cannot construct chroot; ldd reported a missing dependency: {report}"
    );

    let mut copied = std::collections::HashSet::new();
    for dependency in report
        .split_whitespace()
        .map(Path::new)
        .filter(|path| path.is_absolute() && path.exists())
    {
        if copied.insert(dependency.to_path_buf()) {
            copy_file_into_chroot(dependency, dependency, chroot);
        }
    }
}

/// Exercise the compiled CLI after a real `chroot(2)`, not merely with paths
/// that resemble a jail. The minimal root intentionally has no `/root`.
///
/// Platform prerequisites are Linux, `unshare`, `chroot`, and `ldd`, plus an
/// enabled unprivileged user namespace with mount-namespace support. The test
/// emits a skip reason when the host cannot provide that isolation facility.
#[cfg(target_os = "linux")]
#[test]
fn actual_chroot_without_root_home_matches_unset_home_error() {
    let Some(unshare) = command_on_path("unshare") else {
        eprintln!("skipping real-chroot HOME test: unshare is not installed");
        return;
    };
    let Some(chroot_command) = command_on_path("chroot") else {
        eprintln!("skipping real-chroot HOME test: chroot is not installed");
        return;
    };
    let Some(ldd) = command_on_path("ldd") else {
        eprintln!("skipping real-chroot HOME test: ldd is not installed");
        return;
    };

    // Probe before building the fixture. `--map-root-user` grants
    // CAP_SYS_CHROOT only inside the new user namespace; it does not require or
    // grant host root privileges.
    let namespace_probe = Command::new(&unshare)
        .args(["--user", "--map-root-user", "--mount", "--"])
        .arg(&chroot_command)
        .args(["/", "/bin/true"])
        .stdin(Stdio::null())
        .output()
        .expect("probe user-namespace chroot support");
    if !namespace_probe.status.success() {
        eprintln!(
            "skipping real-chroot HOME test: user/mount namespaces are unavailable: {}",
            String::from_utf8_lossy(&namespace_probe.stderr).trim()
        );
        return;
    }

    let binary = PathBuf::from(env!("CARGO_BIN_EXE_claude-print"));
    let jail = tempfile::tempdir().expect("create isolated chroot");
    install_binary_in_chroot(&binary, &ldd, jail.path());
    assert!(
        !jail.path().join("root").exists(),
        "fixture must not create /root"
    );

    let outside = Command::new(&binary)
        .arg("--version")
        .env_remove("HOME")
        .env_remove("XDG_CONFIG_HOME")
        .stdin(Stdio::null())
        .output()
        .expect("run non-chroot missing-HOME baseline");
    let inside = Command::new(&unshare)
        .args(["--user", "--map-root-user", "--mount", "--"])
        .arg(&chroot_command)
        .arg(jail.path())
        .args(["/bin/claude-print", "--version"])
        .env_remove("HOME")
        .env_remove("XDG_CONFIG_HOME")
        .stdin(Stdio::null())
        .output()
        .expect("run claude-print in chroot without /root");

    assert_eq!(outside.status.code(), Some(2));
    assert!(outside.stdout.is_empty());
    assert_eq!(outside.stderr, HOME_ERROR_STDERR.as_bytes());
    assert_eq!(inside.status.code(), outside.status.code());
    assert_eq!(inside.stdout, outside.stdout);
    assert_eq!(inside.stderr, outside.stderr);
    assert!(
        !jail.path().join("root").exists(),
        "claude-print must not synthesize /root as a HOME fallback"
    );
}

#[test]
fn env_u_home_version_fails_with_actionable_error() {
    let binary = PathBuf::from(env!("CARGO_BIN_EXE_claude-print"));
    assert!(binary.exists(), "missing binary at {}", binary.display());

    // Exercise the acceptance scenario literally rather than relying on an
    // in-process environment mutation: env -u HOME claude-print --version.
    let output = Command::new("env")
        .arg("-u")
        .arg("HOME")
        .arg(&binary)
        .arg("--version")
        .stdin(Stdio::null())
        .output()
        .expect("run env -u HOME claude-print --version");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        output.status.code(),
        Some(2),
        "--version unexpectedly succeeded without HOME: stdout={stdout:?}, stderr={stderr:?}"
    );
    assert!(
        stderr.contains(HOME_ERROR_DETAIL),
        "--version error did not explain how to fix HOME: {stderr}"
    );
    assert!(
        !stdout.contains("claude-print "),
        "--version printed a success version despite missing HOME: {stdout}"
    );
}

/// Run the compiled CLI with HOME removed from the child environment and pin
/// the complete text-mode failure contract shared by every HOME-dependent
/// startup path.
fn assert_cli_home_unset_error(command: &mut Command, case: &str) {
    let output = command
        .env_remove("HOME")
        .stdin(Stdio::null())
        .output()
        .unwrap_or_else(|error| panic!("{case}: run claude-print with HOME unset: {error}"));

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        output.status.code(),
        Some(2),
        "{case}: expected setup exit 2, stdout={stdout:?}, stderr={stderr:?}"
    );
    assert!(
        stdout.is_empty(),
        "{case}: text-mode setup failure must not write stdout: {stdout:?}"
    );
    assert_eq!(
        stderr, HOME_ERROR_STDERR,
        "{case}: stderr must name HOME and explain how to set it"
    );
}

#[test]
fn cli_home_unset_default_config_path_has_strict_error_contract() {
    let binary = PathBuf::from(env!("CARGO_BIN_EXE_claude-print"));
    let unused_backend = std::env::current_exe().expect("locate test executable");

    let mut command = Command::new(binary);
    command
        .arg("--claude-binary")
        .arg(unused_backend)
        .arg("test prompt")
        // Force the default config path to depend on HOME. This is equivalent
        // to `env -u HOME -u XDG_CONFIG_HOME claude-print ...` and cannot read
        // the invoking user's actual home directory.
        .env_remove("XDG_CONFIG_HOME");

    assert_cli_home_unset_error(&mut command, "default config path");
}

#[test]
fn cli_home_unset_xdg_config_and_transcript_discovery_share_strict_error_contract() {
    let binary = PathBuf::from(env!("CARGO_BIN_EXE_claude-print"));
    let mock_claude = binary.with_file_name("mock-claude");
    assert!(
        mock_claude.exists(),
        "mock-claude binary missing at {}",
        mock_claude.display()
    );

    // A complete XDG path bypasses HOME for config lookup. If startup reaches
    // the Stop payload, omitting transcript_path selects poller's HOME-backed
    // transcript discovery branch. The CLI's process-wide HOME validation may
    // reject this environment earlier, but the externally visible strict
    // policy must remain identical to the default-config path above.
    let xdg = tempfile::tempdir().expect("create temporary XDG_CONFIG_HOME");
    let config_dir = xdg.path().join("claude-print");
    std::fs::create_dir(&config_dir).expect("create temporary config directory");
    std::fs::write(
        config_dir.join("config.toml"),
        "[defaults]\nmodel = \"claude-haiku-4-5\"\n",
    )
    .expect("write valid temporary config");

    let mut command = Command::new(binary);
    command
        .arg("--claude-binary")
        .arg(mock_claude)
        .arg("test prompt")
        .env("XDG_CONFIG_HOME", xdg.path())
        .env("MOCK_OMIT_TRANSCRIPT_PATH", "1");

    assert_cli_home_unset_error(&mut command, "XDG config and transcript discovery");
}

#[test]
fn nonexistent_home_version_fails_without_root_fallback() {
    let binary = PathBuf::from(env!("CARGO_BIN_EXE_claude-print"));
    let missing_home = Path::new("/nonexistent");
    assert!(
        !missing_home.exists(),
        "test requires /nonexistent to be absent"
    );

    let output = Command::new(&binary)
        .arg("--version")
        .env("HOME", missing_home)
        .stdin(Stdio::null())
        .output()
        .expect("run HOME=/nonexistent claude-print --version");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        output.status.code(),
        Some(2),
        "stdout={stdout:?}, stderr={stderr:?}"
    );
    assert!(
        stderr.contains("HOME path '/nonexistent' is not accessible"),
        "{stderr}"
    );
    assert!(stderr.contains("existing, writable directory"), "{stderr}");
    assert!(!stderr.contains("/root"), "unexpected fallback: {stderr}");
    assert!(stdout.is_empty(), "unexpected success output: {stdout}");
}

#[cfg(unix)]
#[test]
fn read_only_home_version_reports_write_permission_problem() {
    use std::os::unix::fs::PermissionsExt;

    let binary = PathBuf::from(env!("CARGO_BIN_EXE_claude-print"));
    let home = tempfile::tempdir().expect("create read-only HOME");
    let original_mode = std::fs::metadata(home.path())
        .expect("stat HOME")
        .permissions()
        .mode();
    std::fs::set_permissions(home.path(), std::fs::Permissions::from_mode(0o555))
        .expect("make HOME read-only");

    let output = Command::new(&binary)
        .arg("--version")
        .env("HOME", home.path())
        .stdin(Stdio::null())
        .output()
        .expect("run claude-print with read-only HOME");

    std::fs::set_permissions(home.path(), std::fs::Permissions::from_mode(original_mode))
        .expect("restore HOME permissions");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        output.status.code(),
        Some(2),
        "stdout={stdout:?}, stderr={stderr:?}"
    );
    assert!(
        stderr.contains(&home.path().display().to_string()),
        "{stderr}"
    );
    assert!(stderr.contains("not writable"), "{stderr}");
    assert!(stderr.contains("write permission"), "{stderr}");
    assert!(stdout.is_empty(), "unexpected success output: {stdout}");
    assert_eq!(
        std::fs::read_dir(home.path()).expect("read HOME").count(),
        0,
        "HOME validation must not leave a probe file"
    );
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
        // Remove HOME only from the child to exercise the strict CLI error path.
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
