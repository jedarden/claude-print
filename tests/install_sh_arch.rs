//! Platform-matrix pin for `install.sh`'s `uname` → release-asset mapping.
//!
//! The README's "Supported platforms" table says x86_64 Linux is the only
//! combination CI publishes (the release toolchain installs just
//! `x86_64-unknown-linux-musl`, so `x86_64-linux` is the only asset name a
//! release ever carries), and `tests/install_sh.rs` pins its fake releases
//! to that layout. Until claudepr-b583cd6a the installer nonetheless mapped
//! `Linux-aarch64` to an `aarch64-linux` asset that is never produced, so
//! an aarch64 host passed the platform gate and died later on a download
//! failure — a 404 in place of a supported-platform statement.
//!
//! These tests pin the whole matrix row by row by faking `uname -s` and
//! `uname -m` through a stub earlier on `PATH` (env-driven, so the outcomes
//! never depend on the machine running the tests): the supported row must
//! fetch and verify the `x86_64-linux` assets, and every unsupported row
//! must exit 1 with the actionable message — platform named, supported
//! matrix stated, way forward given — before any download starts and with
//! nothing placed. The refusal tests serve a fully valid x86_64 release so
//! a reintroduced arch→asset mapping would fail as a download error, which
//! these asserts distinguish from the up-front refusal.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use tempfile::TempDir;

const BINARY_ASSET: &str = "claude-print-x86_64-linux";
const MOCK_ASSET: &str = "mock_claude-x86_64-linux";
const VERSION_ASSET: &str = "last-claude-version.txt";
const CHECKSUMS_ASSET: &str = "sha256sums.txt";

const BINARY_INSTALL_NAME: &str = "claude-print";
const MOCK_INSTALL_NAME: &str = "mock_claude";

/// Artifact bodies are scripts: install.sh runs the installed claude-print
/// with `--check` and `--version` and both must exit 0.
const BINARY_BODY: &str = "#!/bin/sh\nprintf 'fake claude-print\\n'\n";
const MOCK_BODY: &str = "#!/bin/sh\nprintf 'fake mock_claude\\n'\n";

/// The supported-matrix line every refusal must carry (install.sh prints it
/// for every unsupported combination).
const MATRIX_LINE: &str = "x86_64 Linux only";

fn repo_path(relative: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(relative)
}

fn sha256_of(path: &Path) -> String {
    let out = Command::new("sha256sum").arg(path).output().unwrap();
    assert!(out.status.success(), "sha256sum failed for {path:?}");
    String::from_utf8(out.stdout)
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap()
        .to_string()
}

/// A fake release directory: `names` written to disk, plus a `sha256sums.txt`
/// whose entries match those bytes exactly — what the CI publisher emits.
fn build_release(names: &[&str]) -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    for name in names {
        let body = if *name == MOCK_ASSET {
            MOCK_BODY
        } else {
            BINARY_BODY
        };
        fs::write(dir.path().join(name), body).unwrap();
    }
    let mut manifest = String::new();
    for name in names {
        manifest.push_str(&sha256_of(&dir.path().join(name)));
        manifest.push_str("  ");
        manifest.push_str(name);
        manifest.push('\n');
    }
    fs::write(dir.path().join(CHECKSUMS_ASSET), manifest).unwrap();
    dir
}

/// A `uname` stub that answers `-s`/`-m` from `FAKE_UNAME_S`/`FAKE_UNAME_M`
/// — install.sh's only two uname calls. The `:?` guards make an unset
/// variable fail loudly instead of silently reporting an empty platform.
const UNAME_STUB: &str = "#!/bin/sh
case \"$1\" in
  -s) printf '%s\\n' \"${FAKE_UNAME_S:?FAKE_UNAME_S must be set}\" ;;
  -m) printf '%s\\n' \"${FAKE_UNAME_M:?FAKE_UNAME_M must be set}\" ;;
esac
";

/// Run install.sh as if `uname -s`/`uname -m` reported `os`/`arch`: the stub
/// above goes into the same PATH-prepended bin dir as the fake `claude` the
/// preflight needs, `HOME` is redirected into a temp dir, and the release is
/// served from `release_dir` as a `file://` URL — the same isolation shell
/// `tests/install_sh.rs` uses, with the platform layered on top.
fn run_install_as(home: &Path, release_dir: &Path, os: &str, arch: &str) -> Output {
    let bin_dir = home.join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let fake_claude = bin_dir.join("claude");
    fs::write(&fake_claude, "#!/bin/sh\nexit 0\n").unwrap();
    fs::set_permissions(&fake_claude, fs::Permissions::from_mode(0o755)).unwrap();
    let fake_uname = bin_dir.join("uname");
    fs::write(&fake_uname, UNAME_STUB).unwrap();
    fs::set_permissions(&fake_uname, fs::Permissions::from_mode(0o755)).unwrap();

    Command::new("sh")
        .arg(repo_path("install.sh"))
        .env("HOME", home)
        .env(
            "PATH",
            format!(
                "{}:{}",
                bin_dir.display(),
                std::env::var("PATH").unwrap_or_default()
            ),
        )
        .env(
            "CLAUDE_PRINT_RELEASE_URL",
            format!("file://{}", release_dir.display()),
        )
        .env("FAKE_UNAME_S", os)
        .env("FAKE_UNAME_M", arch)
        .output()
        .unwrap()
}

fn installed_path(home: &Path, install_name: &str) -> PathBuf {
    home.join(".local/bin").join(install_name)
}

fn stdout_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// Assert the shape every unsupported-platform refusal must have: exit 1,
/// the actionable message on stderr (detected platform, supported matrix,
/// `extra` pinning the branch-specific remedy), no download ever started,
/// and nothing placed — the platform gate runs before `mkdir`/`mktemp`/any
/// fetch, so not even the install dir exists.
fn assert_refused(output: &Output, platform: &str, extra: &str, home: &Path) {
    assert_eq!(
        output.status.code(),
        Some(1),
        "refusal must exit 1, not {:?}; stdout: {} stderr: {}",
        output.status.code(),
        stdout_of(output),
        stderr_of(output)
    );
    let stderr = stderr_of(output);
    assert!(
        stderr.contains(platform),
        "stderr must name the detected platform {platform:?}: {stderr}"
    );
    assert!(
        stderr.contains(MATRIX_LINE),
        "stderr must state the supported matrix ({MATRIX_LINE:?}): {stderr}"
    );
    assert!(
        stderr.contains(extra),
        "stderr must carry the branch remedy {extra:?}: {stderr}"
    );
    let stdout = stdout_of(output);
    assert!(
        !stdout.contains("Downloading"),
        "the refusal must precede every download: {stdout}"
    );
    assert!(
        !home.join(".local/bin").exists(),
        "the refusal must place nothing — install dir must not exist"
    );
}

#[test]
fn linux_x86_64_installs_the_published_x86_64_assets() {
    // The one supported row: the mapping must resolve to the asset names CI
    // actually publishes and the install must complete against them.
    let release = build_release(&[BINARY_ASSET, MOCK_ASSET, VERSION_ASSET]);
    let home = tempfile::tempdir().unwrap();

    let output = run_install_as(home.path(), release.path(), "Linux", "x86_64");

    assert!(output.status.success(), "stderr: {}", stderr_of(&output));
    let stdout = stdout_of(&output);
    assert!(
        stdout.contains(&format!("Downloading {BINARY_ASSET}")),
        "the x86_64 asset must be the one fetched: {stdout}"
    );
    assert!(
        stdout.contains(&format!("Verified {BINARY_ASSET}")),
        "the fetched asset must be verified: {stdout}"
    );
    assert_eq!(
        fs::read_to_string(installed_path(home.path(), BINARY_INSTALL_NAME)).unwrap(),
        BINARY_BODY,
        "the verified binary must be installed verbatim"
    );
    assert_eq!(
        fs::read_to_string(installed_path(home.path(), MOCK_INSTALL_NAME)).unwrap(),
        MOCK_BODY,
        "the fixture must ride along on the supported row"
    );
    assert!(
        !stdout.contains("aarch64") && !stderr_of(&output).contains("aarch64"),
        "no other architecture's asset may be referenced: {stdout}"
    );
}

#[test]
fn linux_aarch64_is_refused_before_any_download_with_a_build_from_source_pointer() {
    // The release here is fully valid — a stale aarch64→asset mapping would
    // sail through the gate and die on a download error for an asset that
    // was never published. The refusal's remedy is building from source.
    let release = build_release(&[BINARY_ASSET, MOCK_ASSET, VERSION_ASSET]);
    let home = tempfile::tempdir().unwrap();

    let output = run_install_as(home.path(), release.path(), "Linux", "aarch64");

    assert_refused(&output, "Linux-aarch64", "build from source", home.path());
    let stdout = stdout_of(&output);
    assert!(
        !stdout.contains("aarch64-linux"),
        "no aarch64 asset may be requested — it is never published: {stdout}"
    );
}

#[test]
fn other_linux_architectures_get_the_same_build_from_source_refusal() {
    // The Linux branch must be architecture-generic, not an aarch64 special
    // case: any non-x86_64 Linux arch gets the same up-front refusal.
    let release = build_release(&[BINARY_ASSET, MOCK_ASSET, VERSION_ASSET]);
    let home = tempfile::tempdir().unwrap();

    let output = run_install_as(home.path(), release.path(), "Linux", "armv7l");

    assert_refused(&output, "Linux-armv7l", "build from source", home.path());
}

#[test]
fn non_linux_platforms_are_refused_with_the_linux_only_reason() {
    // Darwin-x86_64 isolates the OS half of the matrix: the architecture is
    // the supported one, so the refusal can only come from the Linux-only
    // branch — whose remedy is NOT a from-source build (the source needs
    // the same POSIX PTY machinery), which the assert below pins.
    let release = build_release(&[BINARY_ASSET, MOCK_ASSET, VERSION_ASSET]);
    let home = tempfile::tempdir().unwrap();

    let output = run_install_as(home.path(), release.path(), "Darwin", "x86_64");

    assert_refused(&output, "Darwin-x86_64", "Linux-only", home.path());
    assert!(
        !stderr_of(&output).contains("build from source"),
        "the non-Linux refusal must not offer a source build: {}",
        stderr_of(&output)
    );
}
