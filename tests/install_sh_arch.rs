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
