//! End-to-end tests for `install.sh`'s release-artifact integrity checking.
//!
//! Each test builds a fake release directory — asset files plus a
//! `sha256sums.txt` manifest — and points `install.sh` at it through the
//! `CLAUDE_PRINT_RELEASE_URL` override as a `file://` URL. `HOME` is
//! redirected into a temp dir so the install is fully isolated, and a fake
//! `claude` earlier on `PATH` satisfies the script's preflight; the assets
//! themselves are tiny POSIX scripts so the post-install `--check` smoke
//! test passes.
//!
//! Asset names are pinned to the x86_64 layout, the only one CI publishes
//! (README "Architectures: x86_64 only").

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use tempfile::TempDir;

const BINARY_ASSET: &str = "claude-print-x86_64-linux";
const MOCK_ASSET: &str = "mock_claude-x86_64-linux";
const VERSION_ASSET: &str = "last-claude-version.txt";
const CHECKSUMS_ASSET: &str = "sha256sums.txt";

/// What install.sh names the artifacts inside `~/.local/bin` (unsuffixed,
/// unlike the release asset names).
const BINARY_INSTALL_NAME: &str = "claude-print";
const MOCK_INSTALL_NAME: &str = "mock_claude";

/// Artifact bodies are scripts: install.sh runs the installed claude-print
/// with `--check` and `--version` and both must exit 0.
const BINARY_BODY: &str = "#!/bin/sh\nprintf 'fake claude-print\\n'\n";
const MOCK_BODY: &str = "#!/bin/sh\nprintf 'fake mock_claude\\n'\n";

fn repo_path(relative: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(relative)
}

fn asset_body(name: &str) -> &'static str {
    if name == MOCK_ASSET {
        MOCK_BODY
    } else {
        BINARY_BODY
    }
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
        fs::write(dir.path().join(name), asset_body(name)).unwrap();
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

fn run_install(home: &Path, release_dir: &Path) -> Output {
    let bin_dir = home.join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    // Satisfies install.sh's `command -v claude` preflight without a real
    // Claude Code install.
    let fake_claude = bin_dir.join("claude");
    fs::write(&fake_claude, "#!/bin/sh\nexit 0\n").unwrap();
    fs::set_permissions(&fake_claude, fs::Permissions::from_mode(0o755)).unwrap();

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
        .output()
        .unwrap()
}

fn installed_path(home: &Path, install_name: &str) -> PathBuf {
    home.join(".local/bin").join(install_name)
}

fn stderr_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn install_succeeds_when_artifacts_match_the_published_checksums() {
    let release = build_release(&[BINARY_ASSET, MOCK_ASSET, VERSION_ASSET]);
    let home = tempfile::tempdir().unwrap();

    let output = run_install(home.path(), release.path());

    assert!(output.status.success(), "stderr: {}", stderr_of(&output));
    let installed = fs::read_to_string(installed_path(home.path(), BINARY_INSTALL_NAME)).unwrap();
    assert_eq!(installed, BINARY_BODY);
    let mock = fs::read_to_string(installed_path(home.path(), MOCK_INSTALL_NAME)).unwrap();
    assert_eq!(mock, MOCK_BODY);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains(&format!("Verified {BINARY_ASSET}")),
        "stdout must record the binary verification: {stdout}"
    );
    assert!(
        stdout.contains(&format!("Verified {MOCK_ASSET}")),
        "stdout must record the fixture verification: {stdout}"
    );
}

#[test]
fn install_fails_closed_when_the_checksum_manifest_is_missing() {
    // Assets exist but the publisher published no manifest at all.
    let release = tempfile::tempdir().unwrap();
    fs::write(release.path().join(BINARY_ASSET), BINARY_BODY).unwrap();
    fs::write(release.path().join(MOCK_ASSET), MOCK_BODY).unwrap();
    let home = tempfile::tempdir().unwrap();

    let output = run_install(home.path(), release.path());

    assert!(
        !output.status.success(),
        "install must fail without a manifest"
    );
    assert!(
        !installed_path(home.path(), BINARY_INSTALL_NAME).exists(),
        "nothing may be installed when the manifest is missing"
    );
    assert!(!installed_path(home.path(), MOCK_INSTALL_NAME).exists());
    let stderr = stderr_of(&output);
    assert!(
        stderr.contains(CHECKSUMS_ASSET),
        "stderr must name the missing manifest: {stderr}"
    );
}

#[test]
fn install_fails_closed_on_a_tampered_binary() {
    let release = build_release(&[BINARY_ASSET, MOCK_ASSET, VERSION_ASSET]);
    // Mutate the artifact after the manifest was written over its bytes.
    fs::write(
        release.path().join(BINARY_ASSET),
        format!("{BINARY_BODY}\n# tampered\n"),
    )
    .unwrap();
    let home = tempfile::tempdir().unwrap();

    let output = run_install(home.path(), release.path());

    assert!(
        !output.status.success(),
        "a digest mismatch must abort the install"
    );
    assert!(
        !installed_path(home.path(), BINARY_INSTALL_NAME).exists(),
        "a tampered artifact must never reach the install dir"
    );
    assert!(!installed_path(home.path(), MOCK_INSTALL_NAME).exists());
    let stderr = stderr_of(&output);
    assert!(
        stderr.contains("sha256 mismatch") && stderr.contains(BINARY_ASSET),
        "stderr must report the mismatch: {stderr}"
    );
}

#[test]
fn install_fails_closed_when_an_asset_has_no_checksum_entry() {
    // The manifest exists but describes only the version file — the binary is
    // missing metadata, which is as fatal as a mismatch.
    let release = tempfile::tempdir().unwrap();
    fs::write(release.path().join(BINARY_ASSET), BINARY_BODY).unwrap();
    fs::write(release.path().join(VERSION_ASSET), "unknown\n").unwrap();
    fs::write(
        release.path().join(CHECKSUMS_ASSET),
        format!(
            "{}  {VERSION_ASSET}\n",
            sha256_of(&release.path().join(VERSION_ASSET))
        ),
    )
    .unwrap();
    let home = tempfile::tempdir().unwrap();

    let output = run_install(home.path(), release.path());

    assert!(
        !output.status.success(),
        "missing metadata must fail the install"
    );
    assert!(
        !installed_path(home.path(), BINARY_INSTALL_NAME).exists(),
        "an unlisted artifact must never be installed"
    );
    let stderr = stderr_of(&output);
    assert!(
        stderr.contains("no entry") && stderr.contains(BINARY_ASSET),
        "stderr must report the missing entry: {stderr}"
    );
}

#[test]
fn install_fails_closed_on_a_tampered_mock_claude() {
    let release = build_release(&[BINARY_ASSET, MOCK_ASSET, VERSION_ASSET]);
    fs::write(release.path().join(MOCK_ASSET), "#!/bin/sh\ntampered\n").unwrap();
    let home = tempfile::tempdir().unwrap();

    let output = run_install(home.path(), release.path());

    assert!(
        !output.status.success(),
        "a tampered fixture must abort the install"
    );
    // Per-artifact verify-then-install: the verified main binary is in place,
    // but the tampered fixture never is.
    assert!(installed_path(home.path(), BINARY_INSTALL_NAME).exists());
    assert!(!installed_path(home.path(), MOCK_INSTALL_NAME).exists());
    let stderr = stderr_of(&output);
    assert!(
        stderr.contains("sha256 mismatch") && stderr.contains(MOCK_ASSET),
        "stderr must report the fixture mismatch: {stderr}"
    );
}

#[test]
fn install_skips_mock_claude_when_it_is_absent_from_the_release() {
    // Releases that predate the fixture list no checksum entry for it; that
    // documented skip survives the integrity checks.
    let release = build_release(&[BINARY_ASSET, VERSION_ASSET]);
    let home = tempfile::tempdir().unwrap();

    let output = run_install(home.path(), release.path());

    assert!(output.status.success(), "stderr: {}", stderr_of(&output));
    assert!(installed_path(home.path(), BINARY_INSTALL_NAME).exists());
    assert!(!installed_path(home.path(), MOCK_INSTALL_NAME).exists());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("skipping mock_claude"),
        "stdout must note the skip: {stdout}"
    );
}
