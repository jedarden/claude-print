//! End-to-end tests for `scripts/install-contract-drift-watch.sh`, the
//! installer behind the scheduled contract-drift watch
//! (`docs/notes/claude-contract-probes.md` §Maintenance → Scheduled watch;
//! bead claudepr-e6e54313) — the `tests/install_billing_canary.rs` pattern.
//!
//! The installer is driven hermetically: `HOME` and `XDG_CONFIG_HOME` are
//! redirected into temp dirs so nothing touches the real
//! `~/.local/libexec` or `~/.config/systemd/user`, and the child's `PATH`
//! is a single temp dir containing only fake `systemctl` / `bead` /
//! `loginctl` scripts (a fake `claude` where a test wants one) plus
//! symlinks to the coreutils the script needs (`dirname`, `install`, `id`).
//! No real systemd, no root, no bead store — so the prerequisite-failure
//! cases are genuine: with a fake simply absent from that PATH, the
//! script's `command -v` preflight really fails instead of finding a real
//! binary further down an ambient PATH.
//!
//! Pinned behavior: the watcher and both units land at the documented paths
//! with the documented modes and byte-identical to the repo copies (unit
//! contents stay the source of truth in `scripts/`), the installed service
//! still ExecStarts the libexec copy and pins the contract repo,
//! `daemon-reload` precedes `enable --now`, a second run is idempotent and
//! restores drifted copies/modes, every hard prerequisite failure (`systemctl`
//! or `bead` absent, or a failing `enable`) aborts or fails loudly with
//! nothing partial claimed as success, a missing `claude` only warns (the
//! watch degrades to a loud INDETERMINATE, it does not lose its alert), and
//! the linger warning fires only when `Linger=no`.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// Which `systemctl` the hermetic PATH offers the installer.
enum Systemctl {
    /// Absent from PATH — the `command -v systemctl` preflight fails.
    Absent,
    /// Records every invocation; exits 0 unless the run sets
    /// `FAKE_SYSTEMCTL_FAIL_ENABLE=1`, which makes the `enable --now` step
    /// exit 1 so the install fails after the files are placed but before
    /// success is reported.
    Fake,
}

/// Everything a run needs: the redirected hierarchy plus the hermetic bin
/// dir that is the child's entire PATH.
struct InstallerEnv {
    home: PathBuf,
    config: PathBuf,
    bin: PathBuf,
    systemctl_log: PathBuf,
}

impl InstallerEnv {
    fn libexec(&self) -> PathBuf {
        self.home.join(".local/libexec/claude-print")
    }

    fn units_dir(&self) -> PathBuf {
        self.config.join("systemd/user")
    }

    /// Nothing the installer creates may exist when a preflight has failed.
    fn assert_nothing_installed(&self) {
        assert!(
            !self.libexec().exists(),
            "no libexec copy may survive a failed preflight"
        );
        assert!(
            !self.units_dir().exists(),
            "no systemd units may survive a failed preflight"
        );
    }
}

/// Symlink a real coreutils binary into the hermetic PATH — the script
/// shells out to these before and during the copies.
fn link_real_tool(bin: &Path, name: &str) {
    let real = which::which(name).unwrap_or_else(|_| panic!("{name} must exist on this host"));
    std::os::unix::fs::symlink(real, bin.join(name)).unwrap();
}

fn write_executable(path: &Path, body: &str) {
    fs::write(path, body).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

/// Build the isolated world the installer runs in. `linger` selects the
/// `loginctl` shape: `None` leaves it off PATH entirely (the check is
/// skipped), `Some("yes"|"no")` fakes `show-user … -p Linger --value`.
/// `with_claude` adds a fake `claude` (its absence is only a warning).
fn build_installer_env(
    root: &Path,
    systemctl: Systemctl,
    with_bead: bool,
    with_claude: bool,
    linger: Option<&str>,
) -> InstallerEnv {
    let home = root.join("home");
    let config = root.join("config");
    let bin = root.join("bin");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&config).unwrap();
    fs::create_dir_all(&bin).unwrap();

    let systemctl_log = root.join("systemctl.log");

    // Only what install-contract-drift-watch.sh execs: the fakes above plus
    // the coreutils it uses for path resolution and copying. The ambient
    // PATH is invisible to the child, so absent fakes are genuinely absent.
    link_real_tool(&bin, "dirname");
    link_real_tool(&bin, "install");
    link_real_tool(&bin, "id");
    if with_bead {
        write_executable(&bin.join("bead"), "#!/bin/sh\nexit 0\n");
    }
    if with_claude {
        write_executable(&bin.join("claude"), "#!/bin/sh\nexit 0\n");
    }
    if let Some(value) = linger {
        write_executable(
            &bin.join("loginctl"),
            &format!("#!/bin/sh\nprintf '%s\\n' '{value}'\n"),
        );
    }
    match systemctl {
        Systemctl::Absent => {}
        Systemctl::Fake => {
            write_executable(
                &bin.join("systemctl"),
                r#"#!/bin/sh
# Record one line per invocation ("$*" space-joined); succeed unless the
# test asked for a failing enable, which simulates a broken user bus. The
# enable step is "systemctl --user enable --now …", so match enable as a
# whole argument anywhere in the line, not just as $1.
printf '%s\n' "$*" >> "$FAKE_SYSTEMCTL_LOG"
if [ "$FAKE_SYSTEMCTL_FAIL_ENABLE" = 1 ]; then
    case " $* " in
        *" enable "*)
            echo "fake systemctl: enable failed" >&2
            exit 1
            ;;
    esac
fi
exit 0
"#,
            );
        }
    }

    InstallerEnv {
        home,
        config,
        bin,
        systemctl_log,
    }
}

/// Run the real installer against [`InstallerEnv`]. The repo's own
/// `scripts/` directory is the install source, so the installed bytes are
/// compared against exactly what a checkout ships.
fn run_installer(env: &InstallerEnv, fail_enable: bool) -> Output {
    // The interpreter is resolved from the ambient PATH and passed as an
    // absolute path: std::process::Command searches the *child's* PATH, and
    // the child's PATH is the hermetic bin dir, which has no bash.
    let bash = which::which("bash").expect("bash must exist on this host");
    Command::new(bash)
        .arg(repo_path("scripts/install-contract-drift-watch.sh"))
        .env("HOME", &env.home)
        .env("XDG_CONFIG_HOME", &env.config)
        .env("PATH", &env.bin)
        .env("FAKE_SYSTEMCTL_LOG", &env.systemctl_log)
        .env(
            "FAKE_SYSTEMCTL_FAIL_ENABLE",
            if fail_enable { "1" } else { "0" },
        )
        .output()
        .unwrap()
}

fn repo_path(relative: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(relative)
}

fn stderr_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn stdout_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn mode_of(path: &Path) -> u32 {
    fs::metadata(path).unwrap().permissions().mode() & 0o777
}

fn systemctl_log(env: &InstallerEnv) -> Vec<String> {
    fs::read_to_string(&env.systemctl_log)
        .unwrap()
        .lines()
        .map(str::to_string)
        .collect()
}

/// One (repo source, installed destination, mode) contract row: the
/// installer copies this file, byte-identical, to that path, with that
/// mode.
struct InstalledFile {
    source: &'static str,
    destination: PathBuf,
    mode: u32,
}

fn installed_files(env: &InstallerEnv) -> Vec<InstalledFile> {
    vec![
        InstalledFile {
            source: "scripts/contract-drift-watch.sh",
            destination: env.libexec().join("contract-drift-watch.sh"),
            mode: 0o755,
        },
        InstalledFile {
            source: "scripts/claude-print-contract-drift-watch.service",
            destination: env.units_dir().join("claude-print-contract-drift-watch.service"),
            mode: 0o644,
        },
        InstalledFile {
            source: "scripts/claude-print-contract-drift-watch.timer",
            destination: env.units_dir().join("claude-print-contract-drift-watch.timer"),
            mode: 0o644,
        },
    ]
}

/// Assert one installed file: present, byte-identical to the repo copy,
/// and carrying its documented mode.
fn assert_installed_file(file: &InstalledFile) {
    assert!(
        file.destination.exists(),
        "{} must be installed",
        file.destination.display()
    );
    let installed = fs::read(&file.destination).unwrap();
    let source = fs::read(repo_path(file.source)).unwrap();
    assert_eq!(
        installed,
        source,
        "{} must be byte-identical to {}",
        file.destination.display(),
        file.source
    );
    assert_eq!(
        mode_of(&file.destination),
        file.mode,
        "{} must carry mode {:o}",
        file.destination.display(),
        file.mode
    );
}

/// The happy path: the watcher lands in `~/.local/libexec/claude-print/`
/// (dir 0700) and both units in `$XDG_CONFIG_HOME/systemd/user/` (dir
/// 0755), byte-identical to the repo copies with 0755/0644 modes; the
/// installed service still ExecStarts the libexec copy just placed and
/// pins the contract repo the detector reads; and systemd is driven in
/// order — daemon-reload strictly before enable --now.
#[test]
fn installer_places_the_documented_files_paths_modes_and_unit_contents() {
    let root = tempfile::tempdir().unwrap();
    let env = build_installer_env(root.path(), Systemctl::Fake, true, true, None);

    let output = run_installer(&env, false);

    assert!(output.status.success(), "stderr: {}", stderr_of(&output));
    for file in &installed_files(&env) {
        assert_installed_file(file);
    }
    assert_eq!(mode_of(&env.libexec()), 0o700, "libexec dir must be 0700");
    assert_eq!(
        mode_of(&env.units_dir()),
        0o755,
        "systemd user dir must be 0755"
    );

    // The installed units stay internally consistent with the installer:
    // the service runs the exact libexec path that was just populated (and
    // pins the checkout the detector measures), and the timer activates
    // that service, daily and persistent, on timers.target.
    let service =
        fs::read_to_string(env.units_dir().join("claude-print-contract-drift-watch.service"))
            .unwrap();
    assert!(
        service.contains("ExecStart=%h/.local/libexec/claude-print/contract-drift-watch.sh"),
        "the service must ExecStart the libexec copy the installer places:\n{service}"
    );
    assert!(
        service.contains("Environment=CLAUDE_PRINT_CONTRACT_REPO=%h/claude-print"),
        "the service must pin the contract repo:\n{service}"
    );
    let timer =
        fs::read_to_string(env.units_dir().join("claude-print-contract-drift-watch.timer"))
            .unwrap();
    for line in [
        "OnCalendar=daily",
        "Persistent=true",
        "Unit=claude-print-contract-drift-watch.service",
        "WantedBy=timers.target",
    ] {
        assert!(
            timer.contains(line),
            "the timer must keep its documented `{line}` setting:\n{timer}"
        );
    }

    // systemd was reloaded first, then the timer enabled, then listed.
    let log = systemctl_log(&env);
    let reload = log
        .iter()
        .position(|l| l == "--user daemon-reload")
        .expect("daemon-reload must run");
    let enable = log
        .iter()
        .position(|l| l == "--user enable --now claude-print-contract-drift-watch.timer")
        .expect("the timer must be enabled and started");
    assert!(
        reload < enable,
        "daemon-reload must precede enable --now; log: {log:?}"
    );
    assert!(
        log.iter().any(|l| l
            == "--user list-timers claude-print-contract-drift-watch.timer --no-pager"),
        "the installer must show the resulting timer; log: {log:?}"
    );
    let stdout = stdout_of(&output);
    assert!(
        stdout.contains("Installed and enabled claude-print-contract-drift-watch.timer"),
        "stdout must report the enabled timer: {stdout}"
    );
}

/// Running the installer again is idempotent: it succeeds, re-reloads and
/// re-enables the timer, and — because every artifact is re-`install`ed —
/// restores a copy an operator drifted (edited the unit in place, chmod'd
/// the script) back to the repo bytes and documented modes.
#[test]
fn installer_is_idempotent_and_restores_drifted_copies() {
    let root = tempfile::tempdir().unwrap();
    let env = build_installer_env(root.path(), Systemctl::Fake, true, true, None);

    let first = run_installer(&env, false);
    assert!(first.status.success(), "first run: {}", stderr_of(&first));

    // Local drift between runs: an edited unit and a loosened script mode.
    let timer_path = env.units_dir().join("claude-print-contract-drift-watch.timer");
    let mut drifted = fs::read_to_string(&timer_path).unwrap();
    drifted.push_str("# local drift\n");
    fs::write(&timer_path, drifted).unwrap();
    let script_path = env.libexec().join("contract-drift-watch.sh");
    fs::set_permissions(&script_path, fs::Permissions::from_mode(0o644)).unwrap();

    let second = run_installer(&env, false);
    assert!(
        second.status.success(),
        "second run must succeed: {}",
        stderr_of(&second)
    );

    for file in &installed_files(&env) {
        assert_installed_file(file);
    }

    // Both runs drove systemd: enable --now fired twice.
    let enables = systemctl_log(&env)
        .into_iter()
        .filter(|l| l == "--user enable --now claude-print-contract-drift-watch.timer")
        .count();
    assert_eq!(enables, 2, "each run must enable the timer");
}

/// With no `systemctl` on PATH the installer aborts with its error message
/// and exit 1 — before creating the libexec dir or the systemd user dir.
/// Nothing partial may remain.
#[test]
fn installer_fails_cleanly_when_systemctl_is_absent() {
    let root = tempfile::tempdir().unwrap();
    let env = build_installer_env(root.path(), Systemctl::Absent, true, true, None);

    let output = run_installer(&env, false);

    assert_eq!(output.status.code(), Some(1));
    let stderr = stderr_of(&output);
    assert!(
        stderr.contains("systemctl is required"),
        "stderr must name the missing prerequisite: {stderr}"
    );
    env.assert_nothing_installed();
}

/// With `bead` missing from PATH the installer aborts with its error
/// message and exit 1 — again before writing anything. The filed bead is
/// the drift alert channel; installing a watch that cannot file would
/// reintroduce the blind window this timer exists to close.
#[test]
fn installer_fails_cleanly_when_bead_is_absent() {
    let root = tempfile::tempdir().unwrap();
    let env = build_installer_env(root.path(), Systemctl::Fake, false, true, None);

    let output = run_installer(&env, false);

    assert_eq!(output.status.code(), Some(1));
    let stderr = stderr_of(&output);
    assert!(
        stderr.contains("bead is required"),
        "stderr must name the missing prerequisite: {stderr}"
    );
    env.assert_nothing_installed();
}

/// A missing `claude` is a warning, not a blocker: the installer completes
/// (the watch degrades to a loud INDETERMINATE — exit 2, red unit, state
/// line — rather than losing its alert), while telling the operator how to
/// fix the environment. With `claude` present the same run stays quiet.
#[test]
fn installer_warns_but_proceeds_when_claude_is_absent() {
    let root = tempfile::tempdir().unwrap();
    let without = build_installer_env(root.path(), Systemctl::Fake, true, false, None);

    let output = run_installer(&without, false);

    assert!(
        output.status.success(),
        "a missing claude must not block the install: {}",
        stderr_of(&output)
    );
    let stderr = stderr_of(&output);
    assert!(
        stderr.contains("claude is not on PATH"),
        "the warning must name the degradation: {stderr}"
    );
    assert!(
        stderr.contains("https://claude.ai/install.sh"),
        "the warning must name the remedy: {stderr}"
    );
    assert_installed_file(&installed_files(&without)[0]);

    let with = build_installer_env(&root.path().join("with"), Systemctl::Fake, true, true, None);
    let output = run_installer(&with, false);
    assert!(output.status.success(), "stderr: {}", stderr_of(&output));
    assert!(
        !stderr_of(&output).contains("claude is not on PATH"),
        "no claude warning is expected when it is present"
    );
}

/// A `systemctl` that fails at `enable --now` (e.g. no user bus) must not
/// report success: the installer exits non-zero and never prints its
/// "Installed and enabled" line, so an operator cannot believe a timer is
/// armed when it is not. The files are already on disk at that point — the
/// contract pinned here is the loud failure, not rollback.
#[test]
fn installer_does_not_claim_success_when_enabling_the_timer_fails() {
    let root = tempfile::tempdir().unwrap();
    let env = build_installer_env(root.path(), Systemctl::Fake, true, true, None);

    let output = run_installer(&env, true);

    assert!(
        !output.status.success(),
        "a failed enable must fail the installer"
    );
    assert!(
        !stdout_of(&output).contains("Installed and enabled"),
        "no success message may be printed when enabling failed"
    );
    let stderr = stderr_of(&output);
    assert!(
        stderr.contains("enable failed"),
        "the enable failure must surface: {stderr}"
    );
}

/// Disabled lingering is a warning, not a blocker: the installer still
/// completes, while telling the operator which `loginctl enable-linger`
/// command to hand an administrator. With lingering on, it stays quiet.
#[test]
fn installer_linger_warning_fires_only_when_lingering_is_disabled() {
    let root = tempfile::tempdir().unwrap();
    let off = build_installer_env(root.path(), Systemctl::Fake, true, true, Some("no"));

    let output = run_installer(&off, false);

    assert!(
        output.status.success(),
        "disabled linger must not block the install: {}",
        stderr_of(&output)
    );
    let stderr = stderr_of(&output);
    assert!(
        stderr.contains("lingering is disabled"),
        "the warning must explain the consequence: {stderr}"
    );
    assert!(
        stderr.contains("loginctl enable-linger"),
        "the warning must name the remedy: {stderr}"
    );

    let on = build_installer_env(&root.path().join("on"), Systemctl::Fake, true, true, Some("yes"));
    let output = run_installer(&on, false);
    assert!(output.status.success(), "stderr: {}", stderr_of(&output));
    assert!(
        !stderr_of(&output).contains("lingering"),
        "no linger warning is expected when Linger=yes"
    );
}
