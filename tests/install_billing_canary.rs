//! End-to-end tests for `scripts/install-billing-canary.sh`, the installer
//! behind the documented "Install on each host" workflow
//! (`scripts/billing-canary.md`).
//!
//! The installer is driven hermetically: `HOME` and `XDG_CONFIG_HOME` are
//! redirected into temp dirs so nothing touches the real
//! `~/.local/libexec` or `~/.config/systemd/user`, and the child's `PATH` is
//! a single temp dir containing only fake `systemctl` / `claude-print` /
//! `loginctl` scripts plus symlinks to the coreutils the script needs
//! (`dirname`, `install`, `id`). No real systemd, no root, no network —
//! which also means the prerequisite-failure cases are genuine: with the
//! fake `systemctl` (or `claude-print`) simply absent from that PATH, the
//! script's `command -v` preflight really fails instead of finding a real
//! binary further down an ambient PATH.
//!
//! Pinned behavior: the four files land at the documented paths with the
//! documented modes and byte-identical to the repo copies (unit contents
//! stay the source of truth in `scripts/`), the units the timer activates
//! still point at the libexec path the installer populates, `daemon-reload`
//! precedes `enable --now`, a second run is idempotent and restores drifted
//! copies/modes, and every prerequisite failure aborts before anything is
//! written — no partial installation.

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
fn build_installer_env(
    root: &Path,
    systemctl: Systemctl,
    with_claude_print: bool,
    linger: Option<&str>,
) -> InstallerEnv {
    let home = root.join("home");
    let config = root.join("config");
    let bin = root.join("bin");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&config).unwrap();
    fs::create_dir_all(&bin).unwrap();

    let systemctl_log = root.join("systemctl.log");

    // Only what install-billing-canary.sh execs: the fakes above plus the
    // coreutils it uses for path resolution and copying. The ambient PATH
    // is invisible to the child, so absent fakes are genuinely absent.
    link_real_tool(&bin, "dirname");
    link_real_tool(&bin, "install");
    link_real_tool(&bin, "id");
    if with_claude_print {
        write_executable(&bin.join("claude-print"), "#!/bin/sh\nexit 0\n");
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
        .arg(repo_path("scripts/install-billing-canary.sh"))
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

/// Repo-root probes: a directory holding both is a claude-print checkout.
const ROOT_PROBES: [&str; 2] = ["AGENTS.md", "Cargo.toml"];

/// Where repo-relative files are read from, resolved at *runtime* — never
/// the bare compile-time `env!("CARGO_MANIFEST_DIR")`, which bakes the
/// building checkout's path into the test binary. The local cargo wrapper
/// maps `.git`-less `git archive` extractions onto one shared target dir,
/// so an extraction of unchanged content instant-reuses a cached test
/// binary compiled in an extraction that has since been deleted; a
/// baked-only root then fails every later run of that binary with
/// file-NotFound panics that have nothing to do with drift (bead
/// claudepr-270570be; the same chain as `tests/install_sh.rs`). Candidates, most
/// authoritative first, each probe-verified before use:
///
/// 1. `$CLAUDE_PRINT_TEST_REPO` — explicit override for direct binary runs;
///    when set it is authoritative and must itself be a checkout.
/// 2. the runtime `CARGO_MANIFEST_DIR` cargo sets in the test process to
///    the package under test — the live extraction even in a cache-reused
///    binary.
/// 3. the compile-time `CARGO_MANIFEST_DIR` — last resort for running the
///    test binary directly, where cargo sets neither variable.
///
/// If no candidate survives its probe the panic names every candidate it
/// rejected — loud, never a vacuous pass off a wrong tree.
fn repo_path(relative: &str) -> PathBuf {
    resolve_repo_root(
        std::env::var("CLAUDE_PRINT_TEST_REPO").ok().as_deref(),
        std::env::var("CARGO_MANIFEST_DIR").ok().as_deref(),
        env!("CARGO_MANIFEST_DIR"),
    )
    .unwrap_or_else(|e| panic!("locating the repo root to read {relative} from: {e}"))
    .join(relative)
}

/// [`repo_path`]'s candidate chain as a pure function, so the precedence
/// and the loud failure are testable without racing the process-wide
/// environment from parallel tests (the same shape as
/// `tests/install_sh.rs`).
fn resolve_repo_root(
    env_override: Option<&str>,
    runtime_manifest: Option<&str>,
    baked_manifest: &str,
) -> Result<PathBuf, String> {
    if let Some(override_root) = env_override {
        if is_repo_root(Path::new(override_root)) {
            return Ok(PathBuf::from(override_root));
        }
        return Err(format!(
            "$CLAUDE_PRINT_TEST_REPO={override_root:?} is set but not a claude-print \
             checkout (probe: {:?} + {:?}) — an explicit override is authoritative and \
             is never silently skipped for another candidate",
            ROOT_PROBES[0], ROOT_PROBES[1]
        ));
    }
    // Runtime value first, baked value only as fallback; one chain so the
    // failure names everything that was tried.
    let mut chain: Vec<(&str, &str)> = vec![("compile-time", baked_manifest)];
    if let Some(runtime) = runtime_manifest {
        if runtime != baked_manifest {
            chain.insert(0, ("runtime", runtime));
        }
    }
    let mut rejected = Vec::new();
    for (origin, candidate) in chain {
        let path = Path::new(candidate);
        if is_repo_root(path) {
            return Ok(path.to_path_buf());
        }
        rejected.push(format!("{origin} CARGO_MANIFEST_DIR={}", path.display()));
    }
    Err(format!(
        "no candidate repo root is a claude-print checkout (probe: {:?} + {:?}): {} — \
         run via `cargo test` from a checkout, or set $CLAUDE_PRINT_TEST_REPO to one",
        ROOT_PROBES[0],
        ROOT_PROBES[1],
        rejected.join("; ")
    ))
}

/// Whether `p` holds this suite's root probes.
fn is_repo_root(p: &Path) -> bool {
    ROOT_PROBES.iter().all(|f| p.join(f).is_file())
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
            source: "scripts/billing-canary.sh",
            destination: env.libexec().join("billing-canary.sh"),
            mode: 0o755,
        },
        InstalledFile {
            source: "scripts/check-billing.sh",
            destination: env.libexec().join("check-billing.sh"),
            mode: 0o755,
        },
        InstalledFile {
            source: "scripts/claude-print-billing-canary.service",
            destination: env.units_dir().join("claude-print-billing-canary.service"),
            mode: 0o644,
        },
        InstalledFile {
            source: "scripts/claude-print-billing-canary.timer",
            destination: env.units_dir().join("claude-print-billing-canary.timer"),
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

/// The happy path: both canary scripts land in
/// `~/.local/libexec/claude-print/` (dir 0700) and both units in
/// `$XDG_CONFIG_HOME/systemd/user/` (dir 0755), byte-identical to the repo
/// copies with 0755/0644 modes; the service the timer activates still
/// ExecStarts the libexec copy the installer just placed; and systemd is
/// driven in order — daemon-reload strictly before enable --now.
#[test]
fn installer_places_the_documented_files_paths_modes_and_unit_contents() {
    let root = tempfile::tempdir().unwrap();
    let env = build_installer_env(root.path(), Systemctl::Fake, true, None);

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

    // The units stay internally consistent with the installer: the service
    // runs the exact libexec path that was just populated, and the timer
    // activates that service, daily and persistent, on timers.target.
    let service =
        fs::read_to_string(env.units_dir().join("claude-print-billing-canary.service")).unwrap();
    assert!(
        service.contains("ExecStart=%h/.local/libexec/claude-print/billing-canary.sh"),
        "the service must ExecStart the libexec copy the installer places:\n{service}"
    );
    let timer =
        fs::read_to_string(env.units_dir().join("claude-print-billing-canary.timer")).unwrap();
    for line in [
        "OnCalendar=daily",
        "Persistent=true",
        "Unit=claude-print-billing-canary.service",
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
        .position(|l| l == "--user enable --now claude-print-billing-canary.timer")
        .expect("the timer must be enabled and started");
    assert!(
        reload < enable,
        "daemon-reload must precede enable --now; log: {log:?}"
    );
    assert!(
        log.iter()
            .any(|l| l == "--user list-timers claude-print-billing-canary.timer --no-pager"),
        "the installer must show the resulting timer; log: {log:?}"
    );
    let stdout = stdout_of(&output);
    assert!(
        stdout.contains("Installed and enabled claude-print-billing-canary.timer"),
        "stdout must report the enabled timer: {stdout}"
    );
}

/// Running the installer again is idempotent: it succeeds, re-reloads and
/// re-enables the timer, and — because every artifact is re-`install`ed —
/// restores a copy an operator drifted (edited the timer in place, chmod'd
/// the script) back to the repo bytes and documented modes.
#[test]
fn installer_is_idempotent_and_restores_drifted_copies() {
    let root = tempfile::tempdir().unwrap();
    let env = build_installer_env(root.path(), Systemctl::Fake, true, None);

    let first = run_installer(&env, false);
    assert!(first.status.success(), "first run: {}", stderr_of(&first));

    // Local drift between runs: an edited unit and a loosened script mode.
    let timer_path = env.units_dir().join("claude-print-billing-canary.timer");
    let mut drifted = fs::read_to_string(&timer_path).unwrap();
    drifted.push_str("# local drift\n");
    fs::write(&timer_path, drifted).unwrap();
    let script_path = env.libexec().join("billing-canary.sh");
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
        .filter(|l| l == "--user enable --now claude-print-billing-canary.timer")
        .count();
    assert_eq!(enables, 2, "each run must enable the timer");
}

/// With no `systemctl` on PATH the installer aborts with its error message
/// and exit 1 — before creating the libexec dir or the systemd user dir.
/// Nothing partial may remain.
#[test]
fn installer_fails_cleanly_when_systemctl_is_absent() {
    let root = tempfile::tempdir().unwrap();
    let env = build_installer_env(root.path(), Systemctl::Absent, true, None);

    let output = run_installer(&env, false);

    assert_eq!(output.status.code(), Some(1));
    let stderr = stderr_of(&output);
    assert!(
        stderr.contains("systemctl is required"),
        "stderr must name the missing prerequisite: {stderr}"
    );
    env.assert_nothing_installed();
}

/// With `claude-print` missing from PATH the installer aborts with its
/// error message and exit 1 — again before writing anything, even though
/// `systemctl` exists and the copy sources are present.
#[test]
fn installer_fails_cleanly_when_claude_print_is_absent() {
    let root = tempfile::tempdir().unwrap();
    let env = build_installer_env(root.path(), Systemctl::Fake, false, None);

    let output = run_installer(&env, false);

    assert_eq!(output.status.code(), Some(1));
    let stderr = stderr_of(&output);
    assert!(
        stderr.contains("claude-print is not installed"),
        "stderr must name the missing prerequisite: {stderr}"
    );
    env.assert_nothing_installed();
}

/// A `systemctl` that fails at `enable --now` (e.g. no user bus) must not
/// report success: the installer exits non-zero and never prints its
/// "Installed and enabled" line, so an operator cannot believe a timer is
/// armed when it is not. The files are already on disk at that point — the
/// contract pinned here is the loud failure, not rollback.
#[test]
fn installer_does_not_claim_success_when_enabling_the_timer_fails() {
    let root = tempfile::tempdir().unwrap();
    let env = build_installer_env(root.path(), Systemctl::Fake, true, None);

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
/// command to hand an administrator.
#[test]
fn installer_warns_but_proceeds_when_lingering_is_disabled() {
    let root = tempfile::tempdir().unwrap();
    let env = build_installer_env(root.path(), Systemctl::Fake, true, Some("no"));

    let output = run_installer(&env, false);

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
    assert_installed_file(&installed_files(&env)[0]);
}

/// With lingering already on, the same run stays quiet about it.
#[test]
fn installer_does_not_warn_when_lingering_is_enabled() {
    let root = tempfile::tempdir().unwrap();
    let env = build_installer_env(root.path(), Systemctl::Fake, true, Some("yes"));

    let output = run_installer(&env, false);

    assert!(output.status.success(), "stderr: {}", stderr_of(&output));
    assert!(
        !stderr_of(&output).contains("lingering"),
        "no linger warning is expected when Linger=yes"
    );
}

// The repo-root resolution itself: pinned so a future edit can't quietly
// reintroduce a baked-only root — the failure mode the candidate chain
// exists for (a close gate re-running this suite in a fresh extraction of
// unchanged content instant-reuses the cached binary, and a baked-only
// root fails every filesystem test there with FileNotFound, which reads
// as drift but is cache state; bead claudepr-270570be).

#[test]
fn repo_root_resolution_follows_the_candidate_chain() {
    // The live checkout the suite is running in — the same chain the
    // suite's repo-root resolution uses, minus the override.
    let live = resolve_repo_root(
        None,
        std::env::var("CARGO_MANIFEST_DIR").ok().as_deref(),
        env!("CARGO_MANIFEST_DIR"),
    )
    .expect("the running suite has a usable repo root");
    let live_str = live.display().to_string();
    // A second, minimal checkout: resolution only stats the probe files, so
    // empty ones are enough to make it a valid candidate.
    let other = tempfile::tempdir().expect("tempdir for a second repo root");
    for probe in ROOT_PROBES {
        std::fs::write(other.path().join(probe), "").expect("writing root probe file");
    }
    let other_str = other.path().display().to_string();

    // 1. the override outranks the runtime manifest when both are checkouts
    assert_eq!(
        resolve_repo_root(Some(&other_str), Some(&live_str), &live_str),
        Ok(other.path().to_path_buf())
    );
    // 2. the runtime manifest outranks the baked value — the cache-reuse
    //    case: a dead baked path loses to the live extraction
    assert_eq!(
        resolve_repo_root(None, Some(&other_str), &live_str),
        Ok(other.path().to_path_buf())
    );
    // 3. the baked value is the fallback (direct binary runs: cargo sets
    //    no runtime manifest)
    assert_eq!(
        resolve_repo_root(None, None, &other_str),
        Ok(other.path().to_path_buf())
    );
}

#[test]
fn repo_root_resolution_fails_loudly_naming_every_candidate() {
    // An existing directory without the probe files — the shape a deleted
    // extraction's path, or a typo'd path, has.
    let not_a_checkout = tempfile::tempdir().expect("tempdir that is not a checkout");
    let bogus = not_a_checkout.path().display().to_string();
    let err = resolve_repo_root(None, Some(&bogus), &bogus).unwrap_err();
    assert!(
        err.contains(&bogus),
        "the failure must name the rejected candidate: {err}"
    );
    assert!(
        err.contains("CLAUDE_PRINT_TEST_REPO"),
        "the failure must name the escape hatch: {err}"
    );
    assert!(
        err.contains(ROOT_PROBES[0]) && err.contains(ROOT_PROBES[1]),
        "the failure must name the probe files so the gap is actionable: {err}"
    );
}

#[test]
fn a_set_repo_root_override_is_authoritative() {
    let live = resolve_repo_root(
        None,
        std::env::var("CARGO_MANIFEST_DIR").ok().as_deref(),
        env!("CARGO_MANIFEST_DIR"),
    )
    .expect("the running suite has a usable repo root");
    let live_str = live.display().to_string();
    let not_a_checkout = tempfile::tempdir().expect("tempdir that is not a checkout");
    let bogus = not_a_checkout.path().display().to_string();
    let err = resolve_repo_root(Some(&bogus), Some(&live_str), &live_str).unwrap_err();
    assert!(
        err.contains("$CLAUDE_PRINT_TEST_REPO") && err.contains(&bogus),
        "a set-but-wrong override must fail naming itself, not fall through to \
         another tree: {err}"
    );
}
