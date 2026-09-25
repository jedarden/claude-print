//! End-to-end exercise of the README's "HOME in containers and chroots"
//! provisioning recipes, driven against the compiled binary (bead
//! claudepr-ab982704).
//!
//! `tests/home_unset.rs` pins the *error contract* of `src/util.rs::get_home`
//! — which call sites fail, with which message, and that nothing falls back
//! to `/root`. What nothing pinned is the operator-facing half of the same
//! README section: the recipes a launcher follows to provision `HOME` in a
//! container, chroot, or service unit (`ENV HOME=/home/claude`, the
//! Kubernetes `env:` stanza, `HOME=/home/service claude-print "..."`), and
//! the failure shapes the section promises to detect (missing home mount,
//! read-only permissions, read-only filesystem). Until this suite those
//! recipes were documentation only: the README could drift from what
//! `get_home()` actually accepts with every test green, because no test both
//! read the section and drove the binary through each documented shape.
//!
//! Two layers, one per way the docs can drift:
//!
//! - **Text pins** — the section's fenced blocks (the ```text error line,
//!   the ```dockerfile `ENV`, the ```yaml stanza) and the Prerequisites
//!   `HOME path '/home/service' is not writable: ...` quote are extracted
//!   from README.md at runtime and compared against the binary's *actual*
//!   output, so the quoted lines cannot fork from `get_home()`'s messages.
//! - **Behavior pins** — each recipe is executed the way its reader would.
//!   A provisioned `HOME` (the ENV-stanza semantics: child env, fresh
//!   writable directory) takes `--version` — the README's own health-check
//!   form — and a full mock-claude prompt run to success, leaving no probe
//!   file behind. A genuine `chroot(2)` jail (user + mount namespaces, the
//!   `tests/home_unset.rs` harness pattern) runs the whole matrix with the
//!   *literal documented paths*: provisioned `/home/service` succeeds,
//!   chmod-0555 and a read-only tmpfs mount fail naming `/home/service`,
//!   and the never-provisioned `/home/claude` fails as a missing mount.
//!   `--help` renders without HOME — the one documented exception.
//!
//! Compiled-binaries: every test drives `CARGO_BIN_EXE_claude-print`; the
//! chroot matrix additionally needs `unshare`, `chroot`, and `ldd` (`mount`
//! for the read-only-filesystem leg) and skips with a reason where the host
//! cannot provide user namespaces. No environment mutation happens in this
//! process — HOME is always overridden in the child only.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

/// The exit status every documented HOME setup error uses — README
/// §Prerequisites: "the process exits with status 2".
const SETUP_EXIT: i32 = 2;

/// The complete text-mode unset-HOME stderr line, exactly as the README's
/// Troubleshooting section quotes it in its ```text block.
const UNSET_HOME_STDERR: &str =
    "error: invalid config: HOME environment variable not set or empty; set HOME to the user's home directory\n";

/// Read a repo file, resolving the root from the *runtime*
/// `CARGO_MANIFEST_DIR` (compile-time value as fallback). The compile-time
/// value alone bakes the building checkout's path into the test binary;
/// when the shared target cache reuses that binary from a different
/// extraction — exactly the clean-tree verification NEEDLE re-runs — the
/// read would hit a directory that no longer exists. See
/// `tests/platform_matrix_docs.rs::repo_file` for the full rationale.
fn repo_file(relative: &str) -> String {
    let root = PathBuf::from(
        std::env::var("CARGO_MANIFEST_DIR")
            .unwrap_or_else(|_| env!("CARGO_MANIFEST_DIR").to_string()),
    );
    fs::read_to_string(root.join(relative))
        .unwrap_or_else(|e| panic!("read {relative} from the checkout under test: {e}"))
}

/// Slice a markdown document from a heading line up to the next heading of
/// the same or higher level. Scoping pins to the slice means a string that
/// merely survives elsewhere in the document cannot satisfy them.
fn section_under_heading<'a>(document: &'a str, heading: &str, next_heading: &str) -> &'a str {
    let start = document
        .find(heading)
        .unwrap_or_else(|| panic!("README no longer has a {heading} heading"));
    let end = document[start + heading.len()..]
        .find(next_heading)
        .unwrap_or_else(|| panic!("README section {heading} is not followed by another heading"));
    &document[start..start + heading.len() + end]
}

/// The README §Troubleshooting subsection carrying the provisioning
/// recipes this suite exercises.
fn recipe_section() -> String {
    section_under_heading(
        &repo_file("README.md"),
        "### HOME in containers and chroots",
        "\n### ",
    )
    .to_string()
}

/// The README §Prerequisites bullet that links to the recipes and quotes
/// the path-specific not-writable error with its `/home/service` example.
fn prerequisites_section() -> String {
    section_under_heading(&repo_file("README.md"), "## Prerequisites", "\n## ").to_string()
}

/// Return the contents of the single fenced code block tagged `info`
/// (contents include the newline before the closing fence). Fails loudly if
/// the block is absent, unterminated, or duplicated — all three are doc
/// drift a copy-pasting reader would inherit.
fn fenced_block<'a>(section: &'a str, info: &str) -> &'a str {
    let opening = format!("```{info}\n");
    let first = section.find(&opening).unwrap_or_else(|| {
        panic!("section no longer carries a ```{info} block for the recipe");
    });
    let others = section[first + 1..].matches(&opening).count();
    assert_eq!(others, 0, "section carries more than one ```{info} block");
    let content_start = first + opening.len();
    let close = section[content_start..]
        .find("\n```")
        .unwrap_or_else(|| panic!("```{info} block is not closed"));
    &section[content_start..content_start + close + 1]
}

/// The backtick-quoted span beginning with `anchor` — used to pull the
/// Prerequisites not-writable error quote out of its bullet without pinning
/// the surrounding prose.
fn backtick_quote<'a>(section: &'a str, anchor: &str) -> &'a str {
    let start = section
        .find(&format!("`{anchor}"))
        .unwrap_or_else(|| panic!("section no longer quotes a span starting {anchor:?}"));
    let content_start = start + 1;
    let end = section[content_start..]
        .find('`')
        .unwrap_or_else(|| panic!("quoted span starting {anchor:?} is never closed"));
    &section[content_start..content_start + end]
}

/// Collapse all whitespace runs to single spaces so a rewrapped paragraph
/// still matches its pinned claim.
fn normalized(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Assert `section` states `claim` verbatim modulo line wrapping.
fn states(section: &str, claim: &str) {
    assert!(
        normalized(section).contains(&normalized(claim)),
        "README section no longer states the pinned claim: {}",
        normalized(claim)
    );
}

fn claude_print_binary() -> PathBuf {
    let binary = PathBuf::from(env!("CARGO_BIN_EXE_claude-print"));
    assert!(
        binary.is_file(),
        "claude-print binary missing at {}",
        binary.display()
    );
    binary
}

fn mock_claude_binary() -> PathBuf {
    let mock = claude_print_binary().with_file_name("mock-claude");
    assert!(
        mock.is_file(),
        "mock-claude fixture missing at {}",
        mock.display()
    );
    mock
}

/// Run `claude-print --version` with HOME absent from the child
/// environment, matching `env -u HOME claude-print --version`.
fn run_version_without_home() -> Output {
    Command::new(claude_print_binary())
        .arg("--version")
        .env_remove("HOME")
        .env_remove("XDG_CONFIG_HOME")
        .stdin(Stdio::null())
        .output()
        .expect("run claude-print --version without HOME")
}

fn stdout_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// The documented unset-HOME line and the binary's actual stderr for the
/// same condition must be the same bytes — the ```text block a reader
/// copies into a health-check assertion is exactly what the tool emits.
#[test]
fn readme_unset_home_error_block_matches_binary_stderr_byte_for_byte() {
    let section = recipe_section();
    let quoted = fenced_block(&section, "text");
    assert_eq!(
        quoted,
        "error: invalid config: HOME environment variable not set or empty; set HOME to the user's home directory\n",
        "the README's quoted unset-HOME line changed; update the pin and the health checks that copy it together"
    );

    let output = run_version_without_home();
    assert_eq!(
        output.status.code(),
        Some(SETUP_EXIT),
        "unset HOME must be a setup error: stdout={:?}, stderr={:?}",
        stdout_of(&output),
        stderr_of(&output)
    );
    assert!(
        stdout_of(&output).is_empty(),
        "a setup error must not print success output: {:?}",
        stdout_of(&output)
    );
    assert_eq!(
        stderr_of(&output),
        quoted,
        "the binary's unset-HOME stderr forked from the README's ```text block"
    );
    assert_eq!(stderr_of(&output), UNSET_HOME_STDERR);
}

/// Both container recipes — the Dockerfile `ENV` and the Kubernetes `env:`
/// stanza — reduce to "run the process with HOME naming a directory the
/// image provisioned". Pin both blocks verbatim, then prove the semantics
/// against the real binary: a freshly provisioned writable HOME takes
/// `--version` to success with no probe residue, with and without
/// `XDG_CONFIG_HOME` (the section's caveat: XDG relocates the config file,
/// it does not remove the HOME requirement).
#[test]
fn dockerfile_and_kubernetes_recipes_provision_a_home_the_binary_accepts() {
    let section = recipe_section();
    assert_eq!(
        fenced_block(&section, "dockerfile"),
        "ENV HOME=/home/claude\n",
        "the Dockerfile recipe changed"
    );
    assert_eq!(
        fenced_block(&section, "yaml"),
        "# Kubernetes container specification\nenv:\n  - name: HOME\n    value: /home/claude\n",
        "the Kubernetes env-stanza recipe changed"
    );
    states(
        &section,
        "Setting `XDG_CONFIG_HOME` does not remove the `HOME` requirement: it can relocate \
         `claude-print`'s config file, but Claude Code state and transcripts still live under \
         `$HOME`.",
    );
    states(
        &section,
        "The probe file is removed immediately; failures name the configured path and never \
         fall back to `/root`.",
    );

    let home = tempfile::tempdir().expect("create provisioned HOME");
    for xdg in [None, Some(home.path().join(".config"))] {
        let mut command = Command::new(claude_print_binary());
        command
            .arg("--version")
            .env("HOME", home.path())
            .stdin(Stdio::null());
        match &xdg {
            Some(path) => {
                command.env("XDG_CONFIG_HOME", path);
            }
            None => {
                command.env_remove("XDG_CONFIG_HOME");
            }
        }
        let output = command
            .output()
            .unwrap_or_else(|e| panic!("run --version with provisioned HOME: {e}"));
        let stdout = stdout_of(&output);
        let stderr = stderr_of(&output);
        let case = if xdg.is_some() {
            "provisioned HOME + XDG_CONFIG_HOME"
        } else {
            "provisioned HOME"
        };
        assert_eq!(
            output.status.code(),
            Some(0),
            "{case}: a provisioned HOME must pass validation: stdout={stdout:?}, stderr={stderr:?}"
        );
        assert!(
            stdout.starts_with("claude-print ") && stdout.contains("(wrapping claude"),
            "{case}: expected the version banner, got {stdout:?}"
        );
        assert!(stderr.is_empty(), "{case}: unexpected stderr {stderr:?}");
    }

    assert_eq!(
        fs::read_dir(home.path())
            .expect("read provisioned HOME")
            .count(),
        0,
        "HOME validation must not leave a probe file behind a successful run"
    );
}

/// The chroot / one-off service recipe is `HOME=/home/service claude-print
/// "..."`. Pin the line verbatim, then run the real thing: a full prompt
/// session against mock-claude with a provisioned HOME must complete —
/// proving the recipe provisions enough for a session, not just `--version`.
#[test]
fn chroot_service_recipe_runs_a_full_prompt_with_provisioned_home() {
    let section = recipe_section();
    assert!(
        section.contains("HOME=/home/service claude-print \"...\""),
        "the documented chroot/service invocation line changed"
    );
    states(
        &section,
        "In a container, chroot, or service unit, set `HOME` explicitly to the home of the \
         account that runs `claude-print`. Create that directory with the correct ownership and \
         mount or provision the user's authenticated Claude Code state there.",
    );

    let home = tempfile::tempdir().expect("create provisioned HOME");
    let workspace = tempfile::tempdir().expect("create scratch cwd");
    let output = Command::new(claude_print_binary())
        .arg("--claude-binary")
        .arg(mock_claude_binary())
        .arg("test prompt")
        .env("HOME", home.path())
        .env_remove("XDG_CONFIG_HOME")
        .current_dir(workspace.path())
        .stdin(Stdio::null())
        .output()
        .expect("run the documented one-off service invocation shape");

    let stdout = stdout_of(&output);
    let stderr = stderr_of(&output);
    assert_eq!(
        output.status.code(),
        Some(0),
        "a provisioned HOME must carry a full session: stdout={stdout:?}, stderr={stderr:?}"
    );
    assert!(
        stdout.contains("Hello from mock_claude"),
        "the session did not emit the mock reply: {stdout:?}"
    );
    assert!(
        !stderr.contains("HOME"),
        "no HOME complaint is expected once the recipe is followed: {stderr:?}"
    );
}

/// "This detects missing home mounts" — the container whose image never
/// created the HOME directory. The failure names the configured path.
#[test]
fn missing_home_mount_fails_with_the_documented_not_accessible_line() {
    let section = recipe_section();
    states(
        &section,
        "Startup verifies that the configured path exists, is a directory, and permits a \
         temporary file to be created and written. This detects missing home mounts, permission \
         problems, and read-only filesystems before Claude Code starts.",
    );

    let root = tempfile::tempdir().expect("create isolated root");
    let never_mounted = root.path().join("not-mounted/home/service");
    assert!(!never_mounted.exists());

    let output = Command::new(claude_print_binary())
        .arg("--version")
        .env("HOME", &never_mounted)
        .env_remove("XDG_CONFIG_HOME")
        .stdin(Stdio::null())
        .output()
        .expect("run --version against a HOME that was never provisioned");

    let stdout = stdout_of(&output);
    let stderr = stderr_of(&output);
    assert_eq!(
        output.status.code(),
        Some(SETUP_EXIT),
        "stdout={stdout:?}, stderr={stderr:?}"
    );
    assert!(stdout.is_empty(), "unexpected success output: {stdout:?}");
    let prefix = format!(
        "error: invalid config: HOME path '{}' is not accessible: ",
        never_mounted.display()
    );
    let suffix = "; set HOME to an existing, writable directory\n";
    assert!(
        stderr.starts_with(&prefix) && stderr.ends_with(suffix),
        "missing home mount must name the path and the fix, got {stderr:?}"
    );
    // The OS error text between the pins is platform-owned (the
    // `tests/home_unset.rs` precedent); it must be present, not empty.
    let os_error = &stderr[prefix.len()..stderr.len() - suffix.len()];
    assert!(!os_error.is_empty(), "no OS error surfaced: {stderr:?}");
    assert!(!stderr.contains("/root"), "unexpected fallback: {stderr:?}");
}

/// The read-only shape — README Prerequisites quotes
/// `HOME path '/home/service' is not writable: ...; grant write permission
/// or set HOME to an existing, writable directory`. Split that quote at its
/// ellipsis and rebuild the expected line around the deterministic
/// mode-bit reason: the README's template and the binary's output must be
/// the same text.
#[test]
fn read_only_home_fails_with_the_readme_quoted_not_writable_line() {
    use std::os::unix::fs::PermissionsExt;

    let prerequisites = prerequisites_section();
    let quote = backtick_quote(&prerequisites, "HOME path '");
    let (before_ellipsis, after_ellipsis) = quote
        .split_once("...")
        .unwrap_or_else(|| panic!("the quoted not-writable error lost its ellipsis: {quote}"));
    assert_eq!(
        before_ellipsis, "HOME path '/home/service' is not writable: ",
        "the quoted example path or wording changed"
    );
    assert_eq!(
        after_ellipsis, "; grant write permission or set HOME to an existing, writable directory",
        "the quoted remedy changed"
    );

    let home = tempfile::tempdir().expect("create HOME to make read-only");
    let original_mode = fs::metadata(home.path())
        .expect("stat HOME")
        .permissions()
        .mode();
    fs::set_permissions(home.path(), fs::Permissions::from_mode(0o555))
        .expect("make HOME read-only");

    let output = Command::new(claude_print_binary())
        .arg("--version")
        .env("HOME", home.path())
        .env_remove("XDG_CONFIG_HOME")
        .stdin(Stdio::null())
        .output()
        .expect("run --version with a read-only HOME");

    fs::set_permissions(home.path(), fs::Permissions::from_mode(original_mode))
        .expect("restore HOME permissions");

    let stdout = stdout_of(&output);
    let stderr = stderr_of(&output);
    let expected = format!(
        "error: invalid config: {}directory permissions are read-only{}\n",
        before_ellipsis.replace("/home/service", &home.path().display().to_string()),
        after_ellipsis
    );
    assert_eq!(
        output.status.code(),
        Some(SETUP_EXIT),
        "stdout={stdout:?}, stderr={stderr:?}"
    );
    assert!(stdout.is_empty(), "unexpected success output: {stdout:?}");
    assert_eq!(
        stderr, expected,
        "the binary's read-only-HOME line forked from the README's quoted template"
    );
    assert_eq!(
        fs::read_dir(home.path()).expect("read HOME").count(),
        0,
        "HOME validation must not leave a probe file"
    );
}

/// "is a directory" is part of the documented acceptance check: a HOME
/// that exists but is a regular file fails naming the path.
#[test]
fn home_that_is_a_regular_file_fails_with_documented_not_a_directory_line() {
    let root = tempfile::tempdir().expect("create isolated root");
    let home_file = root.path().join("home-file");
    fs::write(&home_file, "not a directory").expect("create HOME-as-file");

    let output = Command::new(claude_print_binary())
        .arg("--version")
        .env("HOME", &home_file)
        .env_remove("XDG_CONFIG_HOME")
        .stdin(Stdio::null())
        .output()
        .expect("run --version with HOME pointing at a regular file");

    let stdout = stdout_of(&output);
    let stderr = stderr_of(&output);
    assert_eq!(
        output.status.code(),
        Some(SETUP_EXIT),
        "stdout={stdout:?}, stderr={stderr:?}"
    );
    assert!(stdout.is_empty(), "unexpected success output: {stdout:?}");
    assert_eq!(
        stderr,
        format!(
            "error: invalid config: HOME path '{}' is not a directory; set HOME to an existing, writable directory\n",
            home_file.display()
        )
    );
}

/// The one documented exception: argument-parser help renders before HOME
/// validation, so `env -u HOME claude-print --help` still works.
#[test]
fn help_renders_without_home_as_the_readme_documents() {
    states(
        &recipe_section(),
        "Argument-parser help (`--help`) is still rendered before runtime HOME validation.",
    );

    let output = Command::new(claude_print_binary())
        .arg("--help")
        .env_remove("HOME")
        .env_remove("XDG_CONFIG_HOME")
        .stdin(Stdio::null())
        .output()
        .expect("run --help without HOME");

    let stdout = stdout_of(&output);
    let stderr = stderr_of(&output);
    assert_eq!(
        output.status.code(),
        Some(0),
        "--help must render without HOME: stdout={stdout:?}, stderr={stderr:?}"
    );
    assert!(stdout.contains("Usage:"), "expected usage text: {stdout:?}");
    assert!(
        !stderr.contains("HOME"),
        "--help must not fail on HOME validation: {stderr:?}"
    );
}

// ---------------------------------------------------------------------------
// Real `chroot(2)` matrix — the chroot recipe exercised in an actual jail,
// with the literal documented paths (`/home/service`, `/home/claude`).
// ---------------------------------------------------------------------------

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
    fs::create_dir_all(destination.parent().expect("destination has parent"))
        .unwrap_or_else(|error| panic!("create {}: {error}", destination.display()));
    fs::copy(source, &destination).unwrap_or_else(|error| {
        panic!(
            "copy {} into chroot at {}: {error}",
            source.display(),
            destination.display()
        )
    });
}

/// Copy every absolute dependency reported by `ldd`, including the ELF
/// interpreter. Static binaries need no additional files. Same technique as
/// `tests/home_unset.rs`, which introduced it.
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

/// Probe the isolation facility before building fixtures: `--map-root-user`
/// grants CAP_SYS_CHROOT only inside the new user namespace. `/bin/sh` is
/// the probe command because it is the one path POSIX mandates —
/// `tests/home_unset.rs` probes with `/bin/true`, which e.g. NixOS hosts
/// do not carry, and skips on hosts where the namespace would work.
#[cfg(target_os = "linux")]
fn namespace_chroot_supported(unshare: &Path, chroot: &Path) -> bool {
    Command::new(unshare)
        .args(["--user", "--map-root-user", "--mount", "--"])
        .arg(chroot)
        .arg("/")
        .args(["/bin/sh", "-c", "true"])
        .stdin(Stdio::null())
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

/// Run `/bin/claude-print --version` inside a real `chroot(2)` jail under a
/// user + mount namespace, with HOME set in the child only.
#[cfg(target_os = "linux")]
fn run_version_in_chroot(unshare: &Path, chroot: &Path, jail: &Path, home: &str) -> Output {
    Command::new(unshare)
        .args(["--user", "--map-root-user", "--mount", "--"])
        .arg(chroot)
        .arg(jail)
        .args(["/bin/claude-print", "--version"])
        .env("HOME", home)
        .env_remove("XDG_CONFIG_HOME")
        .stdin(Stdio::null())
        .output()
        .unwrap_or_else(|error| panic!("run claude-print in chroot with HOME={home}: {error}"))
}

/// The full recipe matrix against a real jail, using the README's literal
/// example paths:
///
/// - `/home/service` provisioned (the chroot/service recipe) → `--version`
///   succeeds and leaves no probe file behind it;
/// - `/home/service` chmod-0555 → exit 2 with the exact not-writable line
///   the Prerequisites bullet quotes, ellipsis resolved by the
///   deterministic mode-bit reason;
/// - `/home/service` under a read-only tmpfs mount → exit 2 with the same
///   template, reason from the actual failed write probe ("read-only
///   filesystems" in the README's own words);
/// - `/home/claude` — the Dockerfile/Kubernetes path, never provisioned in
///   the jail → exit 2 as a missing home mount, naming that path.
///
/// Skips with a reason where the host cannot provide user namespaces, like
/// the `tests/home_unset.rs` chroot test.
#[cfg(target_os = "linux")]
#[test]
fn chroot_jail_runs_the_documented_recipe_matrix_with_literal_paths() {
    use std::os::unix::fs::PermissionsExt;

    let Some(unshare) = command_on_path("unshare") else {
        eprintln!("skipping chroot recipe matrix: unshare is not installed");
        return;
    };
    let Some(chroot_command) = command_on_path("chroot") else {
        eprintln!("skipping chroot recipe matrix: chroot is not installed");
        return;
    };
    let Some(ldd) = command_on_path("ldd") else {
        eprintln!("skipping chroot recipe matrix: ldd is not installed");
        return;
    };
    if !namespace_chroot_supported(&unshare, &chroot_command) {
        eprintln!("skipping chroot recipe matrix: user/mount namespaces are unavailable");
        return;
    }

    let binary = claude_print_binary();
    let jail = tempfile::tempdir().expect("create chroot jail");
    install_binary_in_chroot(&binary, &ldd, jail.path());
    let service = jail.path().join("home/service");
    fs::create_dir_all(&service).expect("provision /home/service in the jail");
    assert!(
        !jail.path().join("home/claude").exists(),
        "the Dockerfile path must stay unprovisioned for the missing-mount leg"
    );
    assert!(
        !jail.path().join("root").exists(),
        "the recipes must not need a /root in the jail"
    );

    // Leg 1 — the recipe followed: provisioned /home/service.
    let provisioned =
        run_version_in_chroot(&unshare, &chroot_command, jail.path(), "/home/service");
    let stdout = stdout_of(&provisioned);
    let stderr = stderr_of(&provisioned);
    assert_eq!(
        provisioned.status.code(),
        Some(0),
        "provisioned /home/service must pass HOME validation in the jail: stdout={stdout:?}, stderr={stderr:?}"
    );
    assert!(
        stdout.starts_with("claude-print ") && stdout.contains("(wrapping claude"),
        "expected the version banner inside the jail, got {stdout:?}"
    );
    assert!(
        stderr.is_empty(),
        "unexpected stderr inside the jail: {stderr:?}"
    );
    assert_eq!(
        fs::read_dir(&service).expect("read jail HOME").count(),
        0,
        "the write probe must be removed even inside the jail"
    );

    // Leg 2 — read-only permissions on /home/service.
    fs::set_permissions(&service, fs::Permissions::from_mode(0o555))
        .expect("make jail HOME read-only");
    let read_only = run_version_in_chroot(&unshare, &chroot_command, jail.path(), "/home/service");
    fs::set_permissions(&service, fs::Permissions::from_mode(0o755))
        .expect("restore jail HOME permissions");

    let prerequisites = prerequisites_section();
    let (before_ellipsis, after_ellipsis) = backtick_quote(&prerequisites, "HOME path '")
        .split_once("...")
        .expect("README not-writable quote carries its ellipsis");
    let stdout = stdout_of(&read_only);
    let stderr = stderr_of(&read_only);
    assert_eq!(
        read_only.status.code(),
        Some(SETUP_EXIT),
        "stdout={stdout:?}, stderr={stderr:?}"
    );
    assert!(stdout.is_empty(), "unexpected success output: {stdout:?}");
    // Inside the jail the configured path IS the quoted example path, so
    // the README's template resolves to the binary's line byte-for-byte.
    assert_eq!(
        stderr,
        format!(
            "error: invalid config: {before_ellipsis}directory permissions are read-only{after_ellipsis}\n"
        ),
        "the jailed read-only-HOME line must be the README quote with its ellipsis resolved"
    );

    // Leg 3 — a genuinely read-only *filesystem*: mode bits stay writable,
    // so only the actual create/write probe can catch it. The tmpfs exists
    // solely in the child's mount namespace and vanishes with it.
    if command_on_path("mount").is_some() {
        if let Some(sh) = command_on_path("sh") {
            let mounted = Command::new(&unshare)
                .args(["--user", "--map-root-user", "--mount", "--"])
                .arg(&sh)
                .arg("-c")
                .arg(format!(
                    "mount -t tmpfs -o ro tmpfs {mnt} && {chroot} {jail} /bin/claude-print --version",
                    mnt = service.display(),
                    chroot = chroot_command.display(),
                    jail = jail.path().display(),
                ))
                .env("HOME", "/home/service")
                .env_remove("XDG_CONFIG_HOME")
                .stdin(Stdio::null())
                .output()
                .expect("run claude-print in chroot over a read-only tmpfs HOME");
            let stdout = stdout_of(&mounted);
            let stderr = stderr_of(&mounted);
            assert_eq!(
                mounted.status.code(),
                Some(SETUP_EXIT),
                "a read-only filesystem HOME must fail like any other unwritable HOME: stdout={stdout:?}, stderr={stderr:?}"
            );
            assert!(stdout.is_empty(), "unexpected success output: {stdout:?}");
            let prefix = format!("error: invalid config: {before_ellipsis}");
            let suffix = format!("{after_ellipsis}\n");
            assert!(
                stderr.starts_with(&prefix) && stderr.ends_with(&suffix),
                "the read-only-filesystem failure must use the quoted template, got {stderr:?}"
            );
            let reason = &stderr[prefix.len()..stderr.len() - suffix.len()];
            assert!(
                reason.to_lowercase().contains("read-only"),
                "the probe failure should name the read-only filesystem, got {reason:?}"
            );
        } else {
            eprintln!("skipping read-only-tmpfs leg: no sh on PATH");
        }
    } else {
        eprintln!("skipping read-only-tmpfs leg: mount is not installed");
    }

    // Leg 4 — the missing home mount: the container image that never
    // created the Dockerfile/Kubernetes HOME path.
    let missing = run_version_in_chroot(&unshare, &chroot_command, jail.path(), "/home/claude");
    let stdout = stdout_of(&missing);
    let stderr = stderr_of(&missing);
    assert_eq!(
        missing.status.code(),
        Some(SETUP_EXIT),
        "stdout={stdout:?}, stderr={stderr:?}"
    );
    assert!(stdout.is_empty(), "unexpected success output: {stdout:?}");
    assert!(
        stderr.starts_with("error: invalid config: HOME path '/home/claude' is not accessible: ")
            && stderr.ends_with("; set HOME to an existing, writable directory\n"),
        "the unprovisioned Dockerfile path must fail as a missing mount naming that path: {stderr:?}"
    );
    assert!(!stderr.contains("/root"), "unexpected fallback: {stderr:?}");
}
