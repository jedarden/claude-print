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

/// What install.sh names the rollback copy it preserves beside the binary
/// (see docs/notes/installer-rollback.md for the documented semantics).
const PREV_INSTALL_NAME: &str = "claude-print.prev";

/// A distinguishable binary generation for the rollback tests: same
/// exit-0 shape as [`BINARY_BODY`], carrying a generation marker so content
/// asserts identify which install (or pre-placement) a file came from.
fn generation_body(marker: &str) -> String {
    format!("#!/bin/sh\nprintf 'fake claude-print {marker}\\n'\n")
}

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
    build_release_with_binary_body(names, BINARY_BODY)
}

/// [`build_release`] with the main binary's body overridden, so rollback
/// tests can tell installed generations apart by content (every generation
/// body still exits 0 under install.sh's `--check`/`--version` smoke run).
fn build_release_with_binary_body(names: &[&str], binary_body: &str) -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    for name in names {
        let body = if *name == BINARY_ASSET {
            binary_body
        } else {
            asset_body(name)
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

fn run_install(home: &Path, release_dir: &Path) -> Output {
    run_install_with_env(home, release_dir, &[])
}

/// [`run_install`] with extra environment variables layered over the standard
/// isolation set — the path install.sh's documented toggles (e.g.
/// `SKIP_MOCK_CLAUDE=1`) reach the script through.
fn run_install_with_env(home: &Path, release_dir: &Path, envs: &[(&str, &str)]) -> Output {
    let bin_dir = home.join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    // Satisfies install.sh's `command -v claude` preflight without a real
    // Claude Code install.
    let fake_claude = bin_dir.join("claude");
    fs::write(&fake_claude, "#!/bin/sh\nexit 0\n").unwrap();
    fs::set_permissions(&fake_claude, fs::Permissions::from_mode(0o755)).unwrap();

    let mut command = Command::new("sh");
    command
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
        );
    for (key, value) in envs {
        command.env(key, value);
    }
    command.output().unwrap()
}

fn installed_path(home: &Path, install_name: &str) -> PathBuf {
    home.join(".local/bin").join(install_name)
}

/// Pre-place a prior installation the way a previous `install.sh` run leaves
/// it: a mode-755 binary at the install path, and optionally its rollback
/// copy beside it. This is the state an upgrade runs over.
fn preplace_prior_install(home: &Path, binary_body: &str, prev_body: Option<&str>) {
    let dir = home.join(".local/bin");
    fs::create_dir_all(&dir).unwrap();
    let place = |name: &str, body: &str| {
        let path = dir.join(name);
        fs::write(&path, body).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    };
    place(BINARY_INSTALL_NAME, binary_body);
    if let Some(prev) = prev_body {
        place(PREV_INSTALL_NAME, prev);
    }
}

/// Permission bits of a file, for pinning that the rollback copy keeps the
/// installed binary's executable mode.
fn mode_of(path: &Path) -> u32 {
    fs::metadata(path).unwrap().permissions().mode() & 0o777
}

fn stderr_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// Absolute path of the real `install`(1), for the placement-failure shim to
/// delegate its non-target invocations to: the first executable `install` on
/// this process's PATH. The shim's directory is prepended to the child's PATH
/// only, so it can never match here — and the resolved path is interpolated
/// into the shim precisely because the shim itself must not `command -v
/// install`: the child's PATH points at the shim first, so that would recurse.
fn real_install_path() -> PathBuf {
    let path = std::env::var_os("PATH").expect("PATH must be set to locate install(1)");
    for dir in path.to_string_lossy().split(':') {
        let candidate = Path::new(dir).join("install");
        if let Ok(metadata) = fs::metadata(&candidate) {
            if metadata.is_file() && metadata.permissions().mode() & 0o111 != 0 {
                return candidate;
            }
        }
    }
    panic!("no executable `install` on PATH — cannot build the placement-failure shim");
}

/// Shell body of the placement-failure `install` shim (`__REAL_INSTALL__` is
/// replaced by [`real_install_path`]): fails exactly the invocation whose
/// destination is the main binary — matched by basename, so `claude-print.prev`
/// (the rollback copy), `mock_claude`, and `~/.needle/agents/claude-print.yaml`
/// keep the real tool — and execs the real `install` for everything else.
const PLACEMENT_FAILURE_SHIM: &str = concat!(
    "#!/bin/sh\n",
    "last=\n",
    "for arg in \"$@\"; do\n",
    "  last=$arg\n",
    "done\n",
    "case \"$last\" in\n",
    "  */claude-print)\n",
    "    echo \"install: cannot create regular file '$last': No space left on device (simulated placement failure)\" >&2\n",
    "    exit 1\n",
    "    ;;\n",
    "  *)\n",
    "    exec '__REAL_INSTALL__' \"$@\"\n",
    "    ;;\n",
    "esac\n",
);

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

#[test]
fn skip_mock_claude_env_skips_a_shipped_fixture_while_the_binary_stays_verified_and_installed() {
    // SKIP_MOCK_CLAUDE=1 is the documented opt-out (install.sh header,
    // README "Set SKIP_MOCK_CLAUDE=1 to skip the mock_claude test fixture
    // download"). The release here ships AND lists the fixture, so without
    // the env the fixture leg would run in full (download, verify, install —
    // the mirror image of install_succeeds_when_artifacts_match_...): the
    // skip can only be the env branch, not a missing-asset skip. It must
    // bypass the fixture alone — the binary is still verified against the
    // manifest and installed, and the post-install --check smoke still runs.
    let release = build_release(&[BINARY_ASSET, MOCK_ASSET, VERSION_ASSET]);
    let home = tempfile::tempdir().unwrap();

    let output = run_install_with_env(home.path(), release.path(), &[("SKIP_MOCK_CLAUDE", "1")]);

    assert!(output.status.success(), "stderr: {}", stderr_of(&output));
    // The fixture is never placed, and its leg never starts: no download,
    // no verification, no install line for it.
    assert!(
        !installed_path(home.path(), MOCK_INSTALL_NAME).exists(),
        "SKIP_MOCK_CLAUDE=1 must leave the fixture uninstalled"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !stdout.contains(&format!("Downloading {MOCK_ASSET}")),
        "the fixture download must never start: {stdout}"
    );
    assert!(
        !stdout.contains(&format!("Verified {MOCK_ASSET}")),
        "the fixture must not be verified: {stdout}"
    );
    // The main binary keeps its full treatment.
    assert!(
        stdout.contains(&format!("Verified {BINARY_ASSET}")),
        "the binary must still be verified: {stdout}"
    );
    let binary = installed_path(home.path(), BINARY_INSTALL_NAME);
    assert_eq!(
        fs::read_to_string(&binary).unwrap(),
        BINARY_BODY,
        "the verified binary must be installed verbatim"
    );
    assert_eq!(mode_of(&binary), 0o755, "the binary stays executable");
}

// ---------------------------------------------------------------------------
// Rollback-copy semantics (docs/notes/installer-rollback.md): install.sh
// moves an existing ~/.local/bin/claude-print to claude-print.prev before
// installing the newly verified binary. Creation, single-generation
// replacement, scope, the verify-before-backup ordering, and the mid-install
// failure window are each pinned below with per-generation binary bodies, so
// a content assert identifies which install a file came from.
// ---------------------------------------------------------------------------

#[test]
fn upgrade_backs_up_the_existing_binary_as_the_rollback_copy() {
    let release = build_release_with_binary_body(
        &[BINARY_ASSET, MOCK_ASSET, VERSION_ASSET],
        &generation_body("v2"),
    );
    let home = tempfile::tempdir().unwrap();
    preplace_prior_install(home.path(), &generation_body("v1"), None);

    let output = run_install(home.path(), release.path());

    assert!(output.status.success(), "stderr: {}", stderr_of(&output));
    let prev = installed_path(home.path(), PREV_INSTALL_NAME);
    assert_eq!(
        fs::read_to_string(&prev).unwrap(),
        generation_body("v1"),
        "the prior binary must land at the rollback copy verbatim"
    );
    assert_eq!(mode_of(&prev), 0o755, "the rollback copy stays executable");
    assert_eq!(
        fs::read_to_string(installed_path(home.path(), BINARY_INSTALL_NAME)).unwrap(),
        generation_body("v2"),
        "the new binary replaces the live path"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Backing up existing binary"),
        "stdout must record the backup: {stdout}"
    );
    // The rollback copy covers only the main binary (documented scope):
    // mock_claude is overwritten in place with no copy of its own.
    assert!(
        !installed_path(home.path(), "mock_claude.prev").exists(),
        "mock_claude must not gain a rollback copy"
    );
}

#[test]
fn each_upgrade_replaces_the_rollback_copy_with_the_immediately_previous_binary() {
    // Two upgrades over a pre-placed v1: after the second, the rollback copy
    // holds v2 (the immediately previous install), not v1 — there is no
    // chain of copies and v1 is gone.
    let home = tempfile::tempdir().unwrap();
    preplace_prior_install(home.path(), &generation_body("v1"), None);

    for marker in ["v2", "v3"] {
        let release = build_release_with_binary_body(
            &[BINARY_ASSET, MOCK_ASSET, VERSION_ASSET],
            &generation_body(marker),
        );
        let output = run_install(home.path(), release.path());
        assert!(
            output.status.success(),
            "upgrade to {marker} failed: {}",
            stderr_of(&output)
        );
    }

    assert_eq!(
        fs::read_to_string(installed_path(home.path(), BINARY_INSTALL_NAME)).unwrap(),
        generation_body("v3"),
        "the live binary is the newest install"
    );
    assert_eq!(
        fs::read_to_string(installed_path(home.path(), PREV_INSTALL_NAME)).unwrap(),
        generation_body("v2"),
        "the rollback copy is replaced by the immediately previous binary"
    );
    // Exactly one rollback copy exists — no v1 anywhere under the install dir.
    let bin_dir = home.path().join(".local/bin");
    let prevs: Vec<_> = fs::read_dir(&bin_dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| name.contains("prev"))
        .collect();
    assert_eq!(
        prevs,
        vec![PREV_INSTALL_NAME.to_string()],
        "a single rollback copy may exist, got {prevs:?}"
    );
}

#[test]
fn fresh_install_creates_no_rollback_copy() {
    // No existing binary: nothing to back up, so no claude-print.prev may
    // appear beside the install.
    let release = build_release(&[BINARY_ASSET, MOCK_ASSET, VERSION_ASSET]);
    let home = tempfile::tempdir().unwrap();

    let output = run_install(home.path(), release.path());

    assert!(output.status.success(), "stderr: {}", stderr_of(&output));
    assert!(installed_path(home.path(), BINARY_INSTALL_NAME).exists());
    assert!(
        !installed_path(home.path(), PREV_INSTALL_NAME).exists(),
        "a fresh install must not create a rollback copy"
    );
}

#[test]
fn a_failed_install_disturbs_neither_the_live_binary_nor_the_existing_rollback_copy() {
    // install.sh backs up only after the downloaded artifact passes
    // verification, so a digest mismatch must leave both the live binary
    // and an already-existing rollback copy exactly as they were.
    let release = build_release_with_binary_body(
        &[BINARY_ASSET, MOCK_ASSET, VERSION_ASSET],
        &generation_body("v2"),
    );
    fs::write(
        release.path().join(BINARY_ASSET),
        format!("{}\n# tampered\n", generation_body("v2")),
    )
    .unwrap();
    let home = tempfile::tempdir().unwrap();
    preplace_prior_install(
        home.path(),
        &generation_body("v1"),
        Some(&generation_body("v0")),
    );

    let output = run_install(home.path(), release.path());

    assert!(
        !output.status.success(),
        "a tampered artifact must abort the install"
    );
    assert_eq!(
        fs::read_to_string(installed_path(home.path(), BINARY_INSTALL_NAME)).unwrap(),
        generation_body("v1"),
        "the live binary must be untouched by a failed install"
    );
    assert_eq!(
        fs::read_to_string(installed_path(home.path(), PREV_INSTALL_NAME)).unwrap(),
        generation_body("v0"),
        "an existing rollback copy must not be replaced by a failed install"
    );
}

#[test]
fn mid_install_placement_failure_leaves_the_previous_binary_recoverable_and_reports_the_failure() {
    // The mid-install failure window (docs/notes/installer-rollback.md
    // "The mid-install failure window"): an upgrade whose downloaded artifact
    // has PASSED checksum verification and whose rollback copy has already
    // been created, but whose final `install` of the new binary fails — the
    // canonical disk-filling-up-between-the-two-steps shape. The failure is
    // injected with an `install` shim first on PATH (the same PATH-stub
    // technique run_install uses for the fake `claude`) that fails only the
    // main binary's placement, so verification and the backup `mv` both run
    // for real before the failure strikes.
    let release = build_release_with_binary_body(
        &[BINARY_ASSET, MOCK_ASSET, VERSION_ASSET],
        &generation_body("v2"),
    );
    let home = tempfile::tempdir().unwrap();
    preplace_prior_install(home.path(), &generation_body("v1"), None);

    let bin_dir = home.path().join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let shim = bin_dir.join("install");
    fs::write(
        &shim,
        PLACEMENT_FAILURE_SHIM.replace(
            "__REAL_INSTALL__",
            &real_install_path().display().to_string(),
        ),
    )
    .unwrap();
    fs::set_permissions(&shim, fs::Permissions::from_mode(0o755)).unwrap();

    let output = run_install(home.path(), release.path());

    // The failure is reported: nonzero exit, with the placement tool's error
    // (naming its target) surfacing on stderr rather than being swallowed.
    assert!(
        !output.status.success(),
        "a failed final placement must fail the install"
    );
    let stderr = stderr_of(&output);
    assert!(
        stderr.contains("claude-print"),
        "stderr must carry the placement failure: {stderr}"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    // The failure struck inside the window: the artifact had passed
    // verification and the backup had already been taken.
    assert!(
        stdout.contains(&format!("Verified {BINARY_ASSET}")),
        "the binary must have been verified before the failure: {stdout}"
    );
    assert!(
        stdout.contains("Backing up existing binary"),
        "the rollback copy must have been created before the failure: {stdout}"
    );
    // No false success: nothing that runs after the placement may appear —
    // no per-artifact Installed line, no --check smoke, no completion banner.
    assert!(
        !stdout.contains(&format!(
            "Installed {}",
            installed_path(home.path(), BINARY_INSTALL_NAME).display()
        )),
        "a failed placement must not claim the binary was installed: {stdout}"
    );
    assert!(
        !stdout.contains("Running claude-print --check"),
        "the post-install smoke must not run after a failed placement: {stdout}"
    );
    assert!(
        !stdout.contains("Installation complete"),
        "a failed placement must never print the completion banner: {stdout}"
    );
    // Nor may anything further be placed: the fixture leg and everything
    // after it never ran.
    assert!(
        !installed_path(home.path(), MOCK_INSTALL_NAME).exists(),
        "nothing after the failed placement may be installed"
    );
    // The live path was consumed by the backup `mv` and never refilled, so
    // recovery must come from the rollback copy.
    assert!(
        !installed_path(home.path(), BINARY_INSTALL_NAME).exists(),
        "the live path stays vacant when the placement fails: {stdout}"
    );
    // The previous binary remains recoverable at .prev: verbatim content,
    // executable mode, and actually runnable.
    let prev = installed_path(home.path(), PREV_INSTALL_NAME);
    assert_eq!(
        fs::read_to_string(&prev).unwrap(),
        generation_body("v1"),
        "the rollback copy must hold the previous binary verbatim"
    );
    assert_eq!(mode_of(&prev), 0o755, "the rollback copy stays executable");
    let run_prev = Command::new(&prev).output().unwrap();
    assert!(
        run_prev.status.success(),
        "the rollback copy must still run: {}",
        stderr_of(&run_prev)
    );
    assert!(
        String::from_utf8_lossy(&run_prev.stdout).contains("v1"),
        "the rollback copy must identify as the previous generation: {}",
        String::from_utf8_lossy(&run_prev.stdout)
    );
    // The documented one-step rollback (docs/notes/installer-rollback.md
    // "Rollback workflow") then restores a working binary.
    fs::rename(&prev, installed_path(home.path(), BINARY_INSTALL_NAME)).unwrap();
    let restored = Command::new(installed_path(home.path(), BINARY_INSTALL_NAME))
        .output()
        .unwrap();
    assert!(
        restored.status.success(),
        "the rolled-back binary must run: {}",
        stderr_of(&restored)
    );
    assert!(
        String::from_utf8_lossy(&restored.stdout).contains("v1"),
        "the rolled-back binary must be the previous generation: {}",
        String::from_utf8_lossy(&restored.stdout)
    );
}
