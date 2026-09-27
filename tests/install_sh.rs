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
//!
//! The NEEDLE-adapter leg (docs/notes/installer-needle-adapter.md) is pinned
//! with the child `PATH` fully controlled — `command -v needle` must depend
//! only on what a test planted, never on whether the machine running the
//! suite has NEEDLE installed (the fleet's coding boxes do).

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

/// The documented exception to [`BINARY_BODY`]'s exit-0 contract: a binary
/// whose `--check` exits 1 — the post-install smoke's failure shape (a
/// release whose artifact passes verification and placement but is broken on
/// the installing machine). Only `--check` fails, with a marker line on
/// stderr; any other invocation prints how it was invoked, so stdout
/// asserts can tell the legs apart (a run whose smoke failed must never
/// reach the `--version` leg that follows it).
fn check_failing_body(marker: &str) -> String {
    format!(
        "#!/bin/sh\ncase \"$1\" in\n  --check)\n    printf 'fake claude-print {marker}: \
         simulated --check failure\\n' >&2\n    exit 1\n    ;;\n  *)\n    printf 'fake \
         claude-print {marker} invoked as: %s\\n' \"$1\"\n    ;;\nesac\n"
    )
}

/// Execution evidence planted inside the downloaded artifact by every
/// fail-closed verification test: run in any mode, the artifact prints a
/// distinctive line and touches `$EXECUTION_PROBE`. The fail-closed rule
/// these tests pin is not just "abort" but *where* — the README promises
/// every artifact is verified "before it is installed or executed", so the
/// tests need an artifact whose execution is observable from the outside.
/// The printf lands in install.sh's captured stdout (the script never
/// redirects its children); the touch is a side effect that survives even
/// an execution whose output was swallowed, and is harmless without the
/// env var set. Appended to [`BINARY_BODY`], the lines are also the
/// tamper: the digest of the probed body no longer matches a manifest
/// written over the plain body.
const EXECUTION_PROBE_LINES: &str = concat!(
    "printf 'artifact ran despite failed verification\\n'\n",
    "touch \"${EXECUTION_PROBE:-/dev/null}\" 2>/dev/null || true\n",
);

/// [`BINARY_BODY`] with the execution probe appended — the artifact the
/// fail-closed verification tests download.
fn probing_binary_body() -> String {
    format!("{BINARY_BODY}{EXECUTION_PROBE_LINES}")
}

/// Repo-root probes: a directory holding both is a claude-print checkout.
const ROOT_PROBES: [&str; 2] = ["AGENTS.md", "Cargo.toml"];

/// Where repo-relative fixtures (`install.sh`, the NEEDLE adapter template)
/// are read from, resolved at *runtime* — never the bare compile-time
/// `env!("CARGO_MANIFEST_DIR")`, which bakes the building checkout's path
/// into the test binary. The local cargo wrapper maps `.git`-less
/// `git archive` extractions onto one shared target dir, so an extraction
/// of unchanged content instant-reuses a cached test binary compiled in an
/// extraction that has since been deleted; a baked-only root then fails
/// every later run of that binary with file-NotFound panics that have
/// nothing to do with drift. `tests/docs_test_classification.rs` and
/// `tests/target_path_guard.rs` carry the same chain (added by bead
/// claudepr-23f81f16). Candidates, most authoritative first, each
/// probe-verified before use:
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
    .unwrap_or_else(|e| {
        panic!("locating the repo root to read install.sh and the adapter template from: {e}")
    })
    .join(relative)
}

/// [`repo_path`]'s candidate chain as a pure function, so the precedence
/// and the loud failure are testable without racing the process-wide
/// environment from parallel tests (the same shape as
/// `tests/docs_test_classification.rs`).
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

/// A different but still well-formed 64-hex digest: the first character
/// flipped between `0` and `1`. The tampered-checksum test forges a manifest
/// value that no shape or parseability check could reject — only the digest
/// comparison itself (expected from the manifest vs actual from the bytes)
/// can catch it, which is exactly what that test pins.
fn flip_first_hex(digest: &str) -> String {
    let first = digest.chars().next().unwrap();
    let flipped = if first == '0' { '1' } else { '0' };
    format!("{flipped}{}", &digest[1..])
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

/// The ordering half of every fail-closed verification pin, asserted from
/// the outside: the abort must strike BEFORE the downloaded artifact is
/// executed in any mode (neither execution evidence exists — not the
/// probe's line, not the probe's marker, not even the plain body's output,
/// which the `--check`/`--version` smoke would print) and before the
/// post-install smoke starts (no `--check` line, no completion banner).
/// "Nonzero exit with nothing placed" alone cannot prove this: an install
/// that executed the unverified artifact first and failed somewhere later
/// would satisfy both — exactly the supply-chain sin the fail-closed rule
/// exists to prevent.
fn assert_aborted_before_execution_or_check(stdout: &str, execution_probe: &Path) {
    assert!(
        !stdout.contains("artifact ran despite failed verification"),
        "the unverified artifact must never be executed — its probe line \
         printed: {stdout}"
    );
    assert!(
        !execution_probe.exists(),
        "the unverified artifact must never be executed — its probe marker \
         was touched"
    );
    assert!(
        !stdout.contains("fake claude-print"),
        "the artifact must never run in any mode — the --check/--version \
         smoke prints this line: {stdout}"
    );
    assert!(
        !stdout.contains("Running claude-print --check"),
        "the abort must precede the post-install --check smoke: {stdout}"
    );
    assert!(
        !stdout.contains("Installation complete"),
        "a failed verification must never print the completion banner: {stdout}"
    );
}

/// First executable `tool` on this process's PATH. The placement-failure
/// shim uses it for the real `install`(1), and the NEEDLE-leg tests use it
/// to locate the host tools `install.sh` calls — where a host keeps those is
/// not universal (the fleet's coding boxes are NixOS: coreutils live in
/// /run/current-system/sw/bin, not /usr/bin), so a hardcoded prefix would
/// not resolve.
fn which(tool: &str) -> PathBuf {
    let path = std::env::var_os("PATH").expect("PATH must be set to locate host tools");
    for dir in path.to_string_lossy().split(':') {
        let candidate = Path::new(dir).join(tool);
        if let Ok(metadata) = fs::metadata(&candidate) {
            if metadata.is_file() && metadata.permissions().mode() & 0o111 != 0 {
                return candidate;
            }
        }
    }
    panic!("no executable `{tool}` on PATH — cannot build the hermetic install environment");
}

/// Absolute path of the real `install`(1), for the placement-failure shim to
/// delegate its non-target invocations to: the first executable `install` on
/// this process's PATH ([`which`]). The shim's directory is prepended to the
/// child's PATH only, so it can never match here — and the resolved path is
/// interpolated into the shim precisely because the shim itself must not
/// `command -v install`: the child's PATH points at the shim first, so that
/// would recurse.
fn real_install_path() -> PathBuf {
    which("install")
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
    // Assets exist but the publisher published no manifest at all. The
    // binary carries the execution probe so the run pins the ordering as
    // well as the abort: a script that gave up on verification entirely and
    // executed its way through the install could not pass unnoticed.
    let release = tempfile::tempdir().unwrap();
    fs::write(release.path().join(BINARY_ASSET), probing_binary_body()).unwrap();
    fs::write(release.path().join(MOCK_ASSET), MOCK_BODY).unwrap();
    let home = tempfile::tempdir().unwrap();
    let execution_probe = home.path().join("artifact-was-executed");

    let output = run_install_with_env(
        home.path(),
        release.path(),
        &[("EXECUTION_PROBE", execution_probe.to_str().unwrap())],
    );

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
    assert_aborted_before_execution_or_check(
        &String::from_utf8_lossy(&output.stdout),
        &execution_probe,
    );
}

#[test]
fn install_fails_closed_on_a_tampered_binary() {
    let release = build_release(&[BINARY_ASSET, MOCK_ASSET, VERSION_ASSET]);
    // Mutate the artifact after the manifest was written over its bytes —
    // into the execution-probing body, so the digest mismatch and any
    // hypothetical execution of the corrupted artifact are both observable.
    fs::write(release.path().join(BINARY_ASSET), probing_binary_body()).unwrap();
    let home = tempfile::tempdir().unwrap();
    let execution_probe = home.path().join("artifact-was-executed");

    let output = run_install_with_env(
        home.path(),
        release.path(),
        &[("EXECUTION_PROBE", execution_probe.to_str().unwrap())],
    );

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
    assert_aborted_before_execution_or_check(
        &String::from_utf8_lossy(&output.stdout),
        &execution_probe,
    );
}

#[test]
fn install_fails_closed_on_a_tampered_checksum_entry_in_the_manifest() {
    // The mirror image of install_fails_closed_on_a_tampered_binary: here the
    // artifact is exactly what the publisher built (the probing body, so any
    // hypothetical execution stays observable) and the corruption lives in
    // the MANIFEST — the digest VALUE on the binary's entry was altered to a
    // different but equally well-formed 64-hex string, the shape of a tampered
    // or mis-published checksum rather than a tampered artifact. The entry is
    // present and parseable, so presence and format checks all pass; only
    // comparing the published digest against the downloaded bytes can catch
    // it. Only the binary's line is rewritten (matched on the name field, not
    // by replacing the real digest string globally — a blanket replace could
    // in principle strike another asset's line and leave the target line
    // intact, which would make the install succeed and the assert below fail
    // for the wrong reason).
    let release = build_release_with_binary_body(
        &[BINARY_ASSET, MOCK_ASSET, VERSION_ASSET],
        &probing_binary_body(),
    );
    let tampered_manifest: String = fs::read_to_string(release.path().join(CHECKSUMS_ASSET))
        .unwrap()
        .lines()
        .map(|line| {
            let (digest, name) = line
                .split_once("  ")
                .unwrap_or_else(|| panic!("malformed manifest line: {line:?}"));
            if name == BINARY_ASSET {
                format!("{}  {name}", flip_first_hex(digest))
            } else {
                line.to_string()
            }
        })
        .fold(String::new(), |mut manifest, line| {
            manifest.push_str(&line);
            manifest.push('\n');
            manifest
        });
    fs::write(release.path().join(CHECKSUMS_ASSET), tampered_manifest).unwrap();
    let home = tempfile::tempdir().unwrap();
    let execution_probe = home.path().join("artifact-was-executed");

    let output = run_install_with_env(
        home.path(),
        release.path(),
        &[("EXECUTION_PROBE", execution_probe.to_str().unwrap())],
    );

    assert!(
        !output.status.success(),
        "a tampered checksum entry must abort the install"
    );
    assert!(
        !installed_path(home.path(), BINARY_INSTALL_NAME).exists(),
        "nothing may be installed against a tampered checksum entry"
    );
    assert!(!installed_path(home.path(), MOCK_INSTALL_NAME).exists());
    let stderr = stderr_of(&output);
    assert!(
        stderr.contains("sha256 mismatch") && stderr.contains(BINARY_ASSET),
        "stderr must report the mismatch against the tampered entry: {stderr}"
    );
    assert_aborted_before_execution_or_check(
        &String::from_utf8_lossy(&output.stdout),
        &execution_probe,
    );
}

#[test]
fn install_fails_closed_when_the_manifest_digest_is_malformed() {
    // The unparseable-entry shape, sibling of the tampered entry above: the
    // manifest carries a line for the binary in the canonical
    // "<digest>␠␠<name>" layout, but the digest token is not a sha256 digest
    // at all — eight hex characters, the truncated-publish shape. install.sh
    // performs no shape validation: checksum_entry_for extracts the token
    // and verify_artifact compares it exactly against the digest of the
    // downloaded bytes, so ANY malformed value — truncated, non-hex, garbage
    // — lands in the same fail-closed mismatch as the well-formed tampered
    // entry. That test pins the case no parseability check could reject;
    // this one pins the other half — an entry being present and line-shaped
    // is never enough to verify, and a malformed digest can never pass as
    // one. The binary carries the execution probe, so the run also pins the
    // ordering: the artifact is neither run nor placed.
    let release = build_release_with_binary_body(
        &[BINARY_ASSET, MOCK_ASSET, VERSION_ASSET],
        &probing_binary_body(),
    );
    let malformed_manifest: String = fs::read_to_string(release.path().join(CHECKSUMS_ASSET))
        .unwrap()
        .lines()
        .map(|line| {
            let (_, name) = line
                .split_once("  ")
                .unwrap_or_else(|| panic!("malformed manifest line: {line:?}"));
            if name == BINARY_ASSET {
                format!("deadbeef  {name}")
            } else {
                line.to_string()
            }
        })
        .fold(String::new(), |mut manifest, line| {
            manifest.push_str(&line);
            manifest.push('\n');
            manifest
        });
    fs::write(release.path().join(CHECKSUMS_ASSET), malformed_manifest).unwrap();
    let home = tempfile::tempdir().unwrap();
    let execution_probe = home.path().join("artifact-was-executed");

    let output = run_install_with_env(
        home.path(),
        release.path(),
        &[("EXECUTION_PROBE", execution_probe.to_str().unwrap())],
    );

    assert!(
        !output.status.success(),
        "a malformed digest entry must abort the install"
    );
    assert!(
        !installed_path(home.path(), BINARY_INSTALL_NAME).exists(),
        "nothing may be installed against a malformed digest entry"
    );
    assert!(!installed_path(home.path(), MOCK_INSTALL_NAME).exists());
    let stderr = stderr_of(&output);
    assert!(
        stderr.contains("sha256 mismatch") && stderr.contains(BINARY_ASSET),
        "stderr must report the malformed entry as a mismatch: {stderr}"
    );
    assert_aborted_before_execution_or_check(
        &String::from_utf8_lossy(&output.stdout),
        &execution_probe,
    );
}

#[test]
fn install_fails_closed_on_a_partially_transferred_artifact() {
    // The interrupted-transfer shape: the manifest was published over the
    // COMPLETE artifact, but the host delivered only a prefix of those bytes
    // (a mirror or proxy serving a truncated file — any partial delivery
    // curl still exits 0 on, so the download leg cannot be the guard). The
    // prefix is severed at the probe's final touch command, so the partial
    // artifact keeps its shebang and both printf evidence lines: executed in
    // any mode it still prints the probe line and the plain body's line, so
    // the ordering assert below can tell "aborted up front" from "ran first,
    // failed later". (The touch marker itself is severed with the tail — the
    // printf evidence is the live half of the probe here.) Nothing about the
    // truncation is detectable except by digest: the file exists, downloads
    // cleanly, and has a checksum entry, so this pins that verification
    // compares the actual bytes against the published digest of the complete
    // artifact — never mere presence or parseability.
    let release = build_release_with_binary_body(
        &[BINARY_ASSET, MOCK_ASSET, VERSION_ASSET],
        &probing_binary_body(),
    );
    // The manifest (written above) digests the complete body; now sever the
    // served artifact to a strict prefix of it.
    let full_body = probing_binary_body();
    let severed_at = full_body
        .rfind("touch ")
        .expect("the probing body must carry the probe's touch command");
    assert!(
        severed_at > 0 && severed_at < full_body.len(),
        "the severance must produce a strict, non-empty prefix"
    );
    fs::write(release.path().join(BINARY_ASSET), &full_body[..severed_at]).unwrap();
    let home = tempfile::tempdir().unwrap();
    let execution_probe = home.path().join("artifact-was-executed");

    let output = run_install_with_env(
        home.path(),
        release.path(),
        &[("EXECUTION_PROBE", execution_probe.to_str().unwrap())],
    );

    assert!(
        !output.status.success(),
        "a partially transferred artifact must abort the install"
    );
    assert!(
        !installed_path(home.path(), BINARY_INSTALL_NAME).exists(),
        "a partial artifact must never reach the install dir"
    );
    assert!(!installed_path(home.path(), MOCK_INSTALL_NAME).exists());
    let stderr = stderr_of(&output);
    assert!(
        stderr.contains("sha256 mismatch") && stderr.contains(BINARY_ASSET),
        "stderr must report the truncation as a digest mismatch: {stderr}"
    );
    assert_aborted_before_execution_or_check(
        &String::from_utf8_lossy(&output.stdout),
        &execution_probe,
    );
}

#[test]
fn install_fails_closed_when_an_asset_has_no_checksum_entry() {
    // The manifest exists but describes only the version file — the binary is
    // missing metadata, which is as fatal as a mismatch. The binary still
    // downloads (the abort strikes at verification, not before it), carrying
    // the execution probe: an unlisted artifact must be refused without ever
    // being run.
    let release = tempfile::tempdir().unwrap();
    fs::write(release.path().join(BINARY_ASSET), probing_binary_body()).unwrap();
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
    let execution_probe = home.path().join("artifact-was-executed");

    let output = run_install_with_env(
        home.path(),
        release.path(),
        &[("EXECUTION_PROBE", execution_probe.to_str().unwrap())],
    );

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
    assert_aborted_before_execution_or_check(
        &String::from_utf8_lossy(&output.stdout),
        &execution_probe,
    );
}

#[test]
fn install_fails_closed_when_the_manifest_is_empty() {
    // The truncated-publish shape of a malformed manifest: sha256sums.txt
    // exists — its URL resolves and curl exits 0, so the download leg cannot
    // be the guard — but the file arrived zero bytes long, leaving nothing
    // in the release verifiable. The binary downloads normally and carries
    // the execution probe, so the run pins both the abort (every asset is
    // unlisted against a manifest that describes nothing — the same no-entry
    // abort as install_fails_closed_when_an_asset_has_no_checksum_entry,
    // reached from the malformed side) and the ordering: the artifact is
    // neither run nor placed. The cleanup half rides along: the EXIT trap
    // must clear the download window's mktemp scratch files on this abort
    // path too, proven against a scratch dir the mktemp control below
    // validated.
    let scratch = scratch_dir_with_mktemp_control();
    let release = tempfile::tempdir().unwrap();
    fs::write(release.path().join(BINARY_ASSET), probing_binary_body()).unwrap();
    fs::write(release.path().join(MOCK_ASSET), MOCK_BODY).unwrap();
    fs::write(release.path().join(CHECKSUMS_ASSET), "").unwrap();
    let home = tempfile::tempdir().unwrap();
    let execution_probe = home.path().join("artifact-was-executed");

    let output = run_install_with_env(
        home.path(),
        release.path(),
        &[
            ("EXECUTION_PROBE", execution_probe.to_str().unwrap()),
            ("TMPDIR", scratch.path().to_str().unwrap()),
        ],
    );

    assert!(
        !output.status.success(),
        "an empty manifest must abort the install"
    );
    assert!(
        !installed_path(home.path(), BINARY_INSTALL_NAME).exists(),
        "nothing may be installed against an empty manifest"
    );
    assert!(!installed_path(home.path(), MOCK_INSTALL_NAME).exists());
    let stderr = stderr_of(&output);
    assert!(
        stderr.contains("no entry") && stderr.contains(BINARY_ASSET),
        "stderr must report the binary as unverifiable against the empty \
         manifest: {stderr}"
    );
    assert_aborted_before_execution_or_check(
        &String::from_utf8_lossy(&output.stdout),
        &execution_probe,
    );
    assert!(
        fs::read_dir(scratch.path()).unwrap().next().is_none(),
        "the EXIT trap must remove the mktemp scratch files on an \
         empty-manifest abort — debris left in {:?}",
        scratch.path()
    );
}

/// A fresh scratch directory usable for pinning EXIT-trap cleanup: mktemp is
/// run against it once as a control, proving the host's mktemp places its
/// files inside TMPDIR. Without that proof an empty directory after a run
/// means nothing — an mktemp that ignored TMPDIR would leave the dir empty
/// with the trap never having fired — so the control fails loudly instead of
/// letting a cleanup pin pass on a directory nothing ever touched. The
/// control file is removed before the directory is handed out.
fn scratch_dir_with_mktemp_control() -> TempDir {
    let scratch = tempfile::tempdir().unwrap();
    let control = Command::new("mktemp")
        .env("TMPDIR", scratch.path())
        .output()
        .unwrap();
    assert!(
        control.status.success(),
        "the mktemp TMPDIR control failed: {}",
        String::from_utf8_lossy(&control.stderr)
    );
    let control_path = PathBuf::from(String::from_utf8_lossy(&control.stdout).trim());
    assert!(
        control_path.starts_with(scratch.path()),
        "mktemp must honor TMPDIR for the empty-scratch assert below to be \
         meaningful (created {control_path:?})"
    );
    fs::remove_file(&control_path).unwrap();
    scratch
}

#[test]
fn a_failed_verification_leaves_no_scratch_files_behind() {
    // The cleanup half of the fail-closed contract (install.sh's scratch-file
    // comment): the three mktemp scratch files exist for the whole download +
    // verification window, and the EXIT trap "removes them on every exit
    // path, including a verification failure". Pinned on the tampered-
    // artifact abort with TMPDIR pointed at a fresh directory — a trap that
    // stopped firing would leave download debris in the installing user's
    // temp space on exactly the runs this suite is about. Non-vacuous via
    // [`scratch_dir_with_mktemp_control`].
    let scratch = scratch_dir_with_mktemp_control();
    let release = build_release(&[BINARY_ASSET, MOCK_ASSET, VERSION_ASSET]);
    fs::write(release.path().join(BINARY_ASSET), probing_binary_body()).unwrap();
    let home = tempfile::tempdir().unwrap();
    let execution_probe = home.path().join("artifact-was-executed");

    let output = run_install_with_env(
        home.path(),
        release.path(),
        &[
            ("EXECUTION_PROBE", execution_probe.to_str().unwrap()),
            ("TMPDIR", scratch.path().to_str().unwrap()),
        ],
    );

    assert!(
        !output.status.success(),
        "the tampered artifact must abort the install"
    );
    assert_aborted_before_execution_or_check(
        &String::from_utf8_lossy(&output.stdout),
        &execution_probe,
    );
    assert!(
        fs::read_dir(scratch.path()).unwrap().next().is_none(),
        "the EXIT trap must remove the mktemp scratch files on a \
         verification failure — debris left in {:?}",
        scratch.path()
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
    // The abort must also precede the smoke: the placed binary never runs —
    // no --check leg, no output from any mode, no completion banner.
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !stdout.contains("fake claude-print"),
        "the placed binary must never run — the fixture abort precedes the \
         smoke: {stdout}"
    );
    assert!(
        !stdout.contains("Running claude-print --check"),
        "the abort must precede the post-install --check smoke: {stdout}"
    );
    assert!(
        !stdout.contains("Installation complete"),
        "a failed verification must never print the completion banner: {stdout}"
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
fn a_manifest_that_omits_a_shipped_fixture_skips_it_rather_than_failing() {
    // The sharp shape of the optional-asset rule (README Install: "a release
    // whose manifest does not list it skips the fixture instead of
    // failing"): the fixture's bytes ship in the release, but the manifest —
    // already written over the other assets — does not list it. The skip
    // decision reads only the manifest, so the same omission that is fatal
    // for the main binary
    // (install_fails_closed_when_an_asset_has_no_checksum_entry) leaves the
    // fixture undownloaded and unplaced while the listed binary installs
    // normally.
    let release = build_release(&[BINARY_ASSET, VERSION_ASSET]);
    fs::write(release.path().join(MOCK_ASSET), MOCK_BODY).unwrap();
    let home = tempfile::tempdir().unwrap();

    let output = run_install(home.path(), release.path());

    assert!(output.status.success(), "stderr: {}", stderr_of(&output));
    assert!(
        !installed_path(home.path(), MOCK_INSTALL_NAME).exists(),
        "an unlisted fixture must never be installed, however available its bytes are"
    );
    assert!(installed_path(home.path(), BINARY_INSTALL_NAME).exists());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !stdout.contains(&format!("Downloading {MOCK_ASSET}")),
        "the unlisted fixture must not even be downloaded: {stdout}"
    );
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

#[test]
fn a_failed_post_install_check_aborts_the_install_with_the_new_binary_live_and_prev_intact() {
    // Row three of the failed-upgrade triage table
    // (docs/notes/installer-rollback.md): placement succeeded and a later
    // leg aborted — here the `--check` smoke itself, against a release whose
    // binary passes verification and placement but fails the smoke. The
    // installer must propagate the failure (nonzero exit, the documented
    // `Error: claude-print --check failed` line, the check's own output
    // surfacing), print no success output after it (no `--version` leg, no
    // completion banner), and leave the documented state: the NEW binary
    // live at the install path, the previous one verbatim at `.prev` — from
    // which the documented deliberate rollback restores a binary that passes
    // the post-rollback `--check`/`--version` gate.
    let release = build_release_with_binary_body(
        &[BINARY_ASSET, MOCK_ASSET, VERSION_ASSET],
        &check_failing_body("v2"),
    );
    let home = tempfile::tempdir().unwrap();
    preplace_prior_install(home.path(), &generation_body("v1"), None);

    let output = run_install(home.path(), release.path());

    // The failure propagates: nonzero exit, the documented error line on
    // stderr, and the failing check's own output surfacing rather than
    // being swallowed.
    assert!(
        !output.status.success(),
        "a failed post-install check must fail the install"
    );
    let stderr = stderr_of(&output);
    assert!(
        stderr.contains("Error: claude-print --check failed"),
        "stderr must carry the documented check-failure line: {stderr}"
    );
    assert!(
        stderr.contains("simulated --check failure"),
        "the failing check's own output must surface: {stderr}"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    // The run really reached the check leg — every earlier leg succeeded,
    // so the failure can only be the check's.
    assert!(
        stdout.contains("Backing up existing binary"),
        "the upgrade shape must be established before the failure: {stdout}"
    );
    assert!(
        stdout.contains(&format!(
            "Installed {}",
            installed_path(home.path(), BINARY_INSTALL_NAME).display()
        )),
        "the binary must have been placed before the check ran: {stdout}"
    );
    assert!(
        stdout.contains("Running claude-print --check"),
        "the smoke must have run — the failure is the check's, not an earlier leg's: {stdout}"
    );
    // No success output: the run stops short of the `--version` leg and the
    // completion banner (the note's success marker).
    assert!(
        !stdout.contains("invoked as:"),
        "the binary must never run in its printing mode — no --version leg after a failed check: {stdout}"
    );
    assert!(
        !stdout.contains("Installation complete"),
        "a failed check must never print the completion banner: {stdout}"
    );
    // The documented state (triage row three): the NEW binary is live, and
    // the previous one sits verbatim at the rollback copy.
    assert_eq!(
        fs::read_to_string(installed_path(home.path(), BINARY_INSTALL_NAME)).unwrap(),
        check_failing_body("v2"),
        "the live binary must be the new release — the failure struck after placement"
    );
    let prev = installed_path(home.path(), PREV_INSTALL_NAME);
    assert_eq!(
        fs::read_to_string(&prev).unwrap(),
        generation_body("v1"),
        "the rollback copy must hold the previous binary verbatim"
    );
    assert_eq!(mode_of(&prev), 0o755, "the rollback copy stays executable");
    // The check is the final leg, so everything placed before it stands —
    // the fixture too (contrast a `mock_claude`-leg failure, which skips
    // the legs after it).
    assert!(
        installed_path(home.path(), MOCK_INSTALL_NAME).exists(),
        "legs before the check must keep their placements"
    );
    // The documented deliberate rollback (README "Roll back one version in
    // one step" overwrites the untrusted new binary) then restores the
    // previous generation, which passes the post-rollback gate: `--check`
    // exits 0 and `--version` names the replaced generation.
    fs::rename(&prev, installed_path(home.path(), BINARY_INSTALL_NAME)).unwrap();
    for gate in ["--check", "--version"] {
        let run = Command::new(installed_path(home.path(), BINARY_INSTALL_NAME))
            .arg(gate)
            .output()
            .unwrap();
        assert!(
            run.status.success(),
            "the rolled-back binary must pass the post-rollback {gate} gate: {}",
            stderr_of(&run)
        );
    }
    let version = Command::new(installed_path(home.path(), BINARY_INSTALL_NAME))
        .arg("--version")
        .output()
        .unwrap();
    assert!(
        String::from_utf8_lossy(&version.stdout).contains("v1"),
        "the rolled-back binary must identify as the previous generation: {}",
        String::from_utf8_lossy(&version.stdout)
    );
}

// ---------------------------------------------------------------------------
// NEEDLE adapter installation (docs/notes/installer-needle-adapter.md):
// when install.sh detects NEEDLE it copies the repo-root claude-print.yaml
// template into ~/.needle/agents — from the checkout beside the script, not
// from the release. Detection (needle on PATH, or an existing agents dir),
// source and destination paths, the forced 0644 mode, in-place overwrite
// with no backup, the no-NEEDLE skip, the missing-source skip, and the
// ordering against failed installs are each pinned below with the child
// PATH pinned (see pinned_path) so detection never depends on the host.
// ---------------------------------------------------------------------------

/// What install.sh names the NEEDLE adapter at both ends of its copy: the
/// repo-root template beside the script, and the file inside
/// `~/.needle/agents/`.
const ADAPTER_NAME: &str = "claude-print.yaml";

/// The documented skip note for a script with no template beside it (the
/// `curl install.sh | sh` shape) — pinned verbatim so the wording cannot
/// drift away from the documented contract.
const ADAPTER_SKIP_NOTE: &str =
    "Note: claude-print.yaml not found alongside install.sh — skipping NEEDLE config";

fn needle_agents_dir(home: &Path) -> PathBuf {
    home.join(".needle/agents")
}

fn adapter_dest(home: &Path) -> PathBuf {
    needle_agents_dir(home).join(ADAPTER_NAME)
}

/// The checkout's adapter template — install.sh's source for the copy. Read
/// from the manifest dir rather than inlined so the assert compares the two
/// ends of the actual copy (checkout template vs installed file) instead of
/// a third transcription of the bytes.
fn repo_adapter_bytes() -> Vec<u8> {
    fs::read(repo_path(ADAPTER_NAME)).unwrap()
}

/// Every host tool `install.sh` invokes by name besides its own stubs, whose
/// directories make up the NEEDLE-leg PATH. `sh` itself is resolved by the
/// test process (not the child), `echo`/`command`/`pwd` are shell builtins,
/// and the stub scripts' `#!/bin/sh` is absolute — so this list is complete.
const HOST_TOOLS: &[&str] = &[
    "curl",
    "install",
    "uname",
    "awk",
    "sha256sum",
    "mktemp",
    "dirname",
    "mv",
    "mkdir",
    "rm",
];

/// A PATH for the NEEDLE-leg tests: the stub bin dir plus exactly the host
/// tool directories ([`which`] resolves them per host — NixOS keeps
/// coreutils outside /usr/bin), and never the host PATH itself, which on the
/// fleet's coding boxes carries the real `needle` at ~/.local/bin and would
/// flip install.sh's detection arm from machine to machine. Panics if any
/// included directory holds a `needle` binary, so the no-NEEDLE cases can
/// never pass vacuously on a NEEDLE-equipped host.
fn pinned_path(bin_dir: &Path) -> String {
    let mut dirs: Vec<PathBuf> = Vec::new();
    for tool in HOST_TOOLS {
        let dir = which(tool).parent().unwrap().to_path_buf();
        let needle = dir.join("needle");
        let real_needle = fs::metadata(&needle)
            .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
            .unwrap_or(false);
        assert!(
            !real_needle,
            "cannot pin a needle-free PATH: {needle:?} is a real NEEDLE install inside a host tool dir"
        );
        if !dirs.contains(&dir) {
            dirs.push(dir);
        }
    }
    let mut path = bin_dir.display().to_string();
    for dir in &dirs {
        path.push(':');
        path.push_str(&dir.display().to_string());
    }
    path
}

/// [`run_install_with_env`] with the two knobs the NEEDLE leg needs: the
/// child PATH is pinned (see [`pinned_path`]) so `command -v needle` sees
/// only the stub this test planted (`needle_on_path` — the command arm of
/// the detection), and the script under test is a parameter so the
/// directory beside install.sh — the adapter's source — is under the test's
/// control too. The `~/.needle/agents` half of the detection is driven by
/// what the test pre-creates under `home`.
fn run_install_needle_leg(
    script: &Path,
    home: &Path,
    release_dir: &Path,
    needle_on_path: bool,
) -> Output {
    let bin_dir = home.join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    // Same preflight stub as every other run, plus the detection stub.
    let mut stub_names = vec!["claude"];
    if needle_on_path {
        stub_names.push("needle");
    }
    for name in stub_names {
        let stub = bin_dir.join(name);
        fs::write(&stub, "#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(&stub, fs::Permissions::from_mode(0o755)).unwrap();
    }

    Command::new("sh")
        .arg(script)
        .env("HOME", home)
        .env("PATH", pinned_path(&bin_dir))
        .env(
            "CLAUDE_PRINT_RELEASE_URL",
            format!("file://{}", release_dir.display()),
        )
        .output()
        .unwrap()
}

#[test]
fn needle_on_the_path_installs_the_repo_adapter_template_into_the_agents_dir() {
    // The command arm of the detection, against a fresh HOME with no
    // ~/.needle at all: the agents dir is created and the checkout's
    // template lands in it.
    let release = build_release(&[BINARY_ASSET, MOCK_ASSET, VERSION_ASSET]);
    let home = tempfile::tempdir().unwrap();

    let output =
        run_install_needle_leg(&repo_path("install.sh"), home.path(), release.path(), true);

    assert!(output.status.success(), "stderr: {}", stderr_of(&output));
    let dest = adapter_dest(home.path());
    assert_eq!(
        fs::read(&dest).unwrap(),
        repo_adapter_bytes(),
        "the adapter must be the checkout's template verbatim (never a release artifact)"
    );
    assert_eq!(
        mode_of(&dest),
        0o644,
        "install -m forces 0644 (the repo copy is 0664)"
    );
    assert!(
        needle_agents_dir(home.path()).is_dir(),
        "the agents dir is created under a fresh HOME"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains(&format!("Installed {}", dest.display())),
        "stdout must record the adapter placement: {stdout}"
    );
    // The leg runs after the artifact legs, so its success line implies the
    // binary made it too.
    assert!(installed_path(home.path(), BINARY_INSTALL_NAME).is_file());
}

#[test]
fn an_existing_agents_dir_alone_triggers_the_adapter_leg() {
    // The second detection arm: ~/.needle/agents exists but no `needle`
    // binary is reachable (a NEEDLE install whose bin dir is off this
    // shell's PATH). The pre-existing dir is preserved, not recreated.
    let release = build_release(&[BINARY_ASSET, MOCK_ASSET, VERSION_ASSET]);
    let home = tempfile::tempdir().unwrap();
    fs::create_dir_all(needle_agents_dir(home.path())).unwrap();
    let marker = needle_agents_dir(home.path()).join("other-agent.yaml");
    fs::write(&marker, "# an unrelated NEEDLE agent\n").unwrap();

    let output =
        run_install_needle_leg(&repo_path("install.sh"), home.path(), release.path(), false);

    assert!(output.status.success(), "stderr: {}", stderr_of(&output));
    let dest = adapter_dest(home.path());
    assert_eq!(
        fs::read(&dest).unwrap(),
        repo_adapter_bytes(),
        "the dir arm must install the template exactly like the command arm"
    );
    assert_eq!(mode_of(&dest), 0o644, "the adapter lands at 0644");
    assert!(
        marker.is_file(),
        "a pre-existing agents dir keeps its contents — mkdir -p must not clear it"
    );
}

#[test]
fn without_needle_the_agents_dir_is_not_created_and_nothing_needle_related_is_printed() {
    // The no-NEEDLE case: neither detection arm holds (no command, no dir).
    // The skip must be total — not even ~/.needle may appear, because the
    // mkdir lives inside the detection branch — and the rest of the install
    // proceeds untouched.
    let release = build_release(&[BINARY_ASSET, MOCK_ASSET, VERSION_ASSET]);
    let home = tempfile::tempdir().unwrap();

    let output =
        run_install_needle_leg(&repo_path("install.sh"), home.path(), release.path(), false);

    assert!(output.status.success(), "stderr: {}", stderr_of(&output));
    assert!(
        !home.path().join(".needle").exists(),
        "without NEEDLE the leg must not even create ~/.needle"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !stdout.contains(".needle") && !stdout.contains("NEEDLE"),
        "nothing NEEDLE-related may print when undetected: {stdout}"
    );
    assert!(installed_path(home.path(), BINARY_INSTALL_NAME).is_file());
    assert!(installed_path(home.path(), MOCK_INSTALL_NAME).is_file());
}

#[test]
fn an_existing_adapter_is_overwritten_in_place_at_0644_with_no_backup_copy() {
    // Overwrite + permissions: a hand-edited, mode-0600 adapter at the
    // destination is replaced byte-for-byte with the checkout template and
    // forced back to 0644 — and gains no backup copy (contrast the main
    // binary's claude-print.prev; docs/notes/installer-rollback.md "Scope").
    let release = build_release(&[BINARY_ASSET, MOCK_ASSET, VERSION_ASSET]);
    let home = tempfile::tempdir().unwrap();
    let dest = adapter_dest(home.path());
    fs::create_dir_all(dest.parent().unwrap()).unwrap();
    fs::write(&dest, "# a stale hand-edited adapter\n").unwrap();
    fs::set_permissions(&dest, fs::Permissions::from_mode(0o600)).unwrap();

    let output =
        run_install_needle_leg(&repo_path("install.sh"), home.path(), release.path(), true);

    assert!(output.status.success(), "stderr: {}", stderr_of(&output));
    assert_eq!(
        fs::read(&dest).unwrap(),
        repo_adapter_bytes(),
        "a stale adapter must be replaced by the checkout template"
    );
    assert_eq!(
        mode_of(&dest),
        0o644,
        "the mode is forced back to 0644 from the drifted 0600"
    );
    let entries: Vec<_> = fs::read_dir(dest.parent().unwrap())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        entries,
        vec![ADAPTER_NAME.to_string()],
        "exactly the adapter may remain in the agents dir — no backup copy, got {entries:?}"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains(&format!("Installed {}", dest.display())),
        "stdout must record the overwrite as an ordinary placement: {stdout}"
    );
}

#[test]
fn rerunning_the_install_reverts_hand_edits_to_the_installed_adapter() {
    // Idempotence (the note's Semantics table): re-running install.sh
    // re-copies the template, so a hand edit to the installed adapter
    // survives only until the next run — README "Warm PTY pool" step 1's
    // --pool-socket edit is the workflow that matters. Unlike the
    // overwrite pin above, the stale copy here is produced by a real
    // first run and edited in place, so what is pinned is the rerun
    // shape, not a fabricated destination.
    let release = build_release(&[BINARY_ASSET, MOCK_ASSET, VERSION_ASSET]);
    let home = tempfile::tempdir().unwrap();

    let first = run_install_needle_leg(&repo_path("install.sh"), home.path(), release.path(), true);
    assert!(first.status.success(), "first run: {}", stderr_of(&first));
    let dest = adapter_dest(home.path());
    assert_eq!(
        fs::read(&dest).unwrap(),
        repo_adapter_bytes(),
        "the first run must install the checkout template"
    );

    // The hand edit, applied to the installed copy the way an operator
    // applies the pool opt-in, plus a drifted mode so the rerun must
    // re-force 0644 over an edited destination too.
    let hand_edited = format!(
        "{}\n# hand edit: --pool-socket added to the invoke template (README \
         \"Warm PTY pool\")\n",
        String::from_utf8_lossy(&fs::read(&dest).unwrap())
    );
    fs::write(&dest, &hand_edited).unwrap();
    fs::set_permissions(&dest, fs::Permissions::from_mode(0o600)).unwrap();
    assert_ne!(
        fs::read(&dest).unwrap(),
        repo_adapter_bytes(),
        "the hand edit must actually differ from the template before the rerun"
    );

    let second =
        run_install_needle_leg(&repo_path("install.sh"), home.path(), release.path(), true);
    assert!(second.status.success(), "rerun: {}", stderr_of(&second));
    assert_eq!(
        fs::read(&dest).unwrap(),
        repo_adapter_bytes(),
        "the rerun must revert the hand edit to the shipped template"
    );
    assert_eq!(
        mode_of(&dest),
        0o644,
        "the rerun re-forces 0644 over the edited copy's drifted mode"
    );
    let stdout = String::from_utf8_lossy(&second.stdout);
    assert!(
        stdout.contains(&format!("Installed {}", dest.display())),
        "the rerun records the placement again — it re-copies, it does not skip: {stdout}"
    );
}

#[test]
fn no_adapter_beside_the_script_skips_the_needle_leg_with_a_note() {
    // The missing-source case — the `curl install.sh | sh` shape, reproduced
    // by running a staging copy of the script with no claude-print.yaml
    // beside it. The skip is a note, not a failure, and per the documented
    // ordering the agents dir is still created (the mkdir precedes the
    // source check).
    let release = build_release(&[BINARY_ASSET, MOCK_ASSET, VERSION_ASSET]);
    let staging = tempfile::tempdir().unwrap();
    fs::copy(repo_path("install.sh"), staging.path().join("install.sh")).unwrap();
    let home = tempfile::tempdir().unwrap();

    let output = run_install_needle_leg(
        &staging.path().join("install.sh"),
        home.path(),
        release.path(),
        true,
    );

    assert!(
        output.status.success(),
        "a missing adapter source is a skip, not a failure: {}",
        stderr_of(&output)
    );
    assert!(
        !adapter_dest(home.path()).exists(),
        "no adapter may be placed without a source"
    );
    assert!(
        needle_agents_dir(home.path()).is_dir(),
        "the agents dir is still created — mkdir precedes the source check"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains(ADAPTER_SKIP_NOTE),
        "stdout must carry the documented skip note verbatim: {stdout}"
    );
    assert!(
        installed_path(home.path(), BINARY_INSTALL_NAME).is_file(),
        "the binary still installs — the skip costs nothing else"
    );
}

#[test]
fn a_failed_install_places_no_needle_adapter() {
    // Ordering: the adapter leg runs after the artifact legs, so an earlier
    // failure — here a tampered binary, the same mutation as
    // install_fails_closed_on_a_tampered_binary — must leave the NEEDLE side
    // untouched even though detection succeeds: no adapter, no ~/.needle at
    // all, and no adapter line on stdout.
    let release = build_release(&[BINARY_ASSET, MOCK_ASSET, VERSION_ASSET]);
    fs::write(
        release.path().join(BINARY_ASSET),
        format!("{BINARY_BODY}\n# tampered\n"),
    )
    .unwrap();
    let home = tempfile::tempdir().unwrap();

    let output =
        run_install_needle_leg(&repo_path("install.sh"), home.path(), release.path(), true);

    assert!(
        !output.status.success(),
        "the tampered release must abort the install"
    );
    assert!(
        !home.path().join(".needle").exists(),
        "a failed install must not create the NEEDLE agents dir"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !stdout.contains(".needle"),
        "no adapter line may print on a failed install: {stdout}"
    );
}

// ---------------------------------------------------------------------------
// README Install-section alignment — the layer-6 pattern of
// tests/config_contract.rs applied to the supply-chain paragraph. The README's
// `## Install` section is the user-facing statement of the guarantees the
// tests in this file enforce against real `install.sh` runs, so the two are
// pinned together: every documented clause is matched verbatim against the
// section, every clause is mapped to the adversarial test that enforces it
// (renaming, deleting, or gutting that test breaks the pin), and the two
// identifiers the README names are cross-checked against install.sh's own
// source. Rewording the guarantee on one side only fails here.
// ---------------------------------------------------------------------------

const README_MD: &str = include_str!("../README.md");
const INSTALL_SH_SOURCE: &str = include_str!("../install.sh");
const THIS_TEST_SOURCE: &str = include_str!("install_sh.rs");

/// The README's `## Install` section — from its heading up to the next
/// level-2 heading (`## Self-check`). Every README-side pin below is scoped
/// to this slice, so a guarantee sentence that merely survives elsewhere in
/// the README cannot satisfy an Install-section check.
fn readme_install_section() -> &'static str {
    let start = README_MD
        .find("\n## Install\n")
        .unwrap_or_else(|| panic!("README.md must carry an `## Install` heading"))
        + 1; // keep the heading line itself in the slice
    let rest = &README_MD[start..];
    // `\n## ` (with the trailing space) matches only level-2 headings, not
    // the `###` subsections inside Install.
    let end = rest.find("\n## ").map(|i| i + 1).unwrap_or(rest.len());
    &rest[..end]
}

/// A documented Install-section guarantee clause and the test in this file
/// that enforces it against a real `install.sh` run over a forged release.
/// The clause must appear verbatim in the section; the test must exist under
/// exactly this name — a guarantee whose enforcing test disappears degrades
/// into documentation-only, which is what this pin exists to prevent.
const INSTALL_GUARANTEES: &[(&str, &str)] = &[
    // The verification sentence's positive half: a release whose artifacts
    // all match the published manifest installs, with each verification
    // recorded.
    (
        "Every downloaded artifact is verified against the release's published \
         `sha256sums.txt` before it is installed or executed",
        "install_succeeds_when_artifacts_match_the_published_checksums",
    ),
    // The fail-closed triad, clause by clause: each documented abort has its
    // own adversarial test asserting the abort and that nothing was placed.
    (
        "A missing manifest",
        "install_fails_closed_when_the_checksum_manifest_is_missing",
    ),
    (
        "an asset with no checksum entry",
        "install_fails_closed_when_an_asset_has_no_checksum_entry",
    ),
    (
        "any digest mismatch aborts the install with nothing placed",
        "install_fails_closed_on_a_tampered_binary",
    ),
    // Both causes of a digest mismatch, pinned separately (the README names
    // each): corruption of the published checksum vs corruption of the
    // transferred bytes. A regression that checked only presence/format of
    // entries, or compared the manifest against itself, would pass the
    // tampered-artifact test's world only by accident of which side the
    // corruption landed on — these two close that axis.
    (
        "a tampered checksum entry (the manifest's digest altered, the \
         artifact itself pristine)",
        "install_fails_closed_on_a_tampered_checksum_entry_in_the_manifest",
    ),
    (
        "a partially transferred artifact (only a prefix of the bytes the \
         manifest digests)",
        "install_fails_closed_on_a_partially_transferred_artifact",
    ),
    // The malformed-manifest shapes, slice finer than the clauses above: an
    // entry present and line-shaped but carrying a digest that is not a
    // sha256 digest at all (no shape validation exists to slip it past — it
    // lands in the same exact-comparison mismatch), and a manifest truncated
    // to zero bytes (the download succeeds, nothing is verifiable — the same
    // no-entry abort as the unlisted-asset row).
    (
        "a manifest entry whose digest is not a sha256 digest (truncated or \
         non-hex) mismatches like any tampered entry",
        "install_fails_closed_when_the_manifest_digest_is_malformed",
    ),
    (
        "a manifest truncated to zero bytes leaves the asset with no entry",
        "install_fails_closed_when_the_manifest_is_empty",
    ),
    // The fixture is the sole optional asset: its manifest entry is the skip
    // decision, so the same omission that is fatal for the binary is a skip
    // for the fixture — even when the fixture's bytes ship in the release.
    // (install_skips_mock_claude_when_it_is_absent_from_the_release pins the
    // asset-absent variant of the same skip.)
    (
        "The `mock_claude` fixture remains optional: a release whose manifest \
         does not list it skips the fixture instead of failing",
        "a_manifest_that_omits_a_shipped_fixture_skips_it_rather_than_failing",
    ),
    // The documented opt-out, against a release that ships AND lists the
    // fixture — so the skip can only be the env branch.
    (
        "Set `SKIP_MOCK_CLAUDE=1` to skip the `mock_claude` test fixture download",
        "skip_mock_claude_env_skips_a_shipped_fixture_while_the_binary_stays_verified_and_installed",
    ),
];

#[test]
fn readme_install_documents_each_guarantee_against_the_adversarial_test_that_pins_it() {
    let section = readme_install_section();
    assert!(
        !section.is_empty(),
        "README.md must carry a non-empty `## Install` section"
    );
    for (clause, test_name) in INSTALL_GUARANTEES {
        assert!(
            section.contains(clause),
            "the README Install section must carry the guarantee verbatim: \
             {clause:?} — update README and tests together"
        );
        assert!(
            THIS_TEST_SOURCE.contains(&format!("fn {test_name}()")),
            "the guarantee {clause:?} is pinned to `{test_name}`, which no longer \
             exists in tests/install_sh.rs — restore the test or re-point the pin"
        );
    }
    // The section must also point readers at this file — the same
    // test-suite cross-reference the Supported-platforms section carries for
    // tests/install_sh_arch.rs.
    assert!(
        section.contains("`tests/install_sh.rs`"),
        "the README Install section must point at tests/install_sh.rs, the file \
         that pins the supply-chain guarantees"
    );
}

#[test]
fn readme_install_names_the_installer_s_own_manifest_asset_and_opt_out_var() {
    let section = readme_install_section();
    // The manifest filename the README names must be the one install.sh
    // fetches and verifies against — defined once in the installer.
    assert!(
        INSTALL_SH_SOURCE.contains("CHECKSUMS_ASSET=\"sha256sums.txt\""),
        "install.sh must define CHECKSUMS_ASSET=\"sha256sums.txt\" — the manifest \
         identifier the README section pins"
    );
    assert!(
        section.contains("`sha256sums.txt`"),
        "the README Install section must name the manifest asset `sha256sums.txt`"
    );
    // The opt-out the README documents must be the one install.sh reads.
    assert!(
        INSTALL_SH_SOURCE.contains("\"${SKIP_MOCK_CLAUDE:-0}\""),
        "install.sh must read the opt-out as ${{SKIP_MOCK_CLAUDE:-0}}"
    );
    assert!(
        section.contains("`SKIP_MOCK_CLAUDE=1`"),
        "the README Install section must spell the opt-out `SKIP_MOCK_CLAUDE=1`"
    );
}

// ---------------------------------------------------------------------------
// installer-needle-adapter note pinning — the note is the authoritative
// statement of the NEEDLE leg (its own intro says so, and the README's
// Install sentence defers to it), so the note itself is held inside the
// pinning scope with the same three-way shape as the README pin above:
// every Semantics row must survive verbatim in the note, the install.sh
// fragment that implements it must survive verbatim in the installer, and
// the note's Hermetic-coverage table must still name the test in this file
// that enforces it against a real run. Rewording the note, changing the
// installer's mechanics, or renaming/deleting a pinning test each break
// exactly one side of that triangle.
// ---------------------------------------------------------------------------

/// docs/notes/installer-needle-adapter.md — the NEEDLE-leg contract this
/// section pins against `install.sh` and the enforcing tests.
const NEEDLE_ADAPTER_NOTE_MD: &str = include_str!("../docs/notes/installer-needle-adapter.md");

/// The note's "Hermetic coverage" section — the coverage table plus the
/// note-pin paragraph after it, and the only part of the note where naming
/// a test counts as declaring coverage. Scoping the coverage assertions
/// here means a test name that survives only in the note's prose cannot
/// satisfy them vacuously.
fn needle_note_coverage_section() -> &'static str {
    NEEDLE_ADAPTER_NOTE_MD
        .split("## Hermetic coverage")
        .nth(1)
        .expect(
            "docs/notes/installer-needle-adapter.md must keep its \
             '## Hermetic coverage' section — the coverage pin has nowhere to live",
        )
}

/// One row of the Semantics table in docs/notes/installer-needle-adapter.md:
/// (documented clause, install.sh fragment implementing it, enforcing test).
/// The clause is matched verbatim against the note, the fragment verbatim
/// against install.sh's source, and the test both against this file (it must
/// exist under exactly this name) and against the note's Hermetic-coverage
/// table (it must be named there, in backticks).
const NEEDLE_NOTE_SEMANTICS: &[(&str, &str, &str)] = &[
    // Detection, command arm: `needle` on PATH.
    (
        "`needle` is found on `PATH` (`command -v needle`)",
        r#"command -v needle >/dev/null 2>&1 || [ -d "${NEEDLE_AGENTS_DIR}" ]"#,
        "needle_on_the_path_installs_the_repo_adapter_template_into_the_agents_dir",
    ),
    // Detection, directory arm: an existing agents dir alone.
    (
        "`~/.needle/agents` already exists as a directory",
        r#"[ -d "${NEEDLE_AGENTS_DIR}" ]"#,
        "an_existing_agents_dir_alone_triggers_the_adapter_leg",
    ),
    // Source: the checkout beside the script, never a release artifact —
    // pinned by the test comparing the installed bytes to the repo template.
    (
        "`claude-print.yaml` from the directory containing `install.sh` — the checkout, NOT a release artifact",
        r#"install -m 644 "${SCRIPT_DIR}/claude-print.yaml" "${NEEDLE_AGENTS_DIR}/claude-print.yaml""#,
        "needle_on_the_path_installs_the_repo_adapter_template_into_the_agents_dir",
    ),
    // Destination: $HOME-rooted agents dir.
    (
        "`~/.needle/agents/claude-print.yaml` (`$HOME`-rooted, absolute)",
        r#"NEEDLE_AGENTS_DIR="${HOME}/.needle/agents""#,
        "needle_on_the_path_installs_the_repo_adapter_template_into_the_agents_dir",
    ),
    // Permissions: install -m forces 0644 over the repo copy's 0664 and any
    // drifted destination mode (the enforcing test pre-chmods 0600).
    (
        "`install -m 644`: mode 0644 always",
        r#"install -m 644 "${SCRIPT_DIR}/claude-print.yaml""#,
        "an_existing_adapter_is_overwritten_in_place_at_0644_with_no_backup_copy",
    ),
    // Overwrite: unconditional, in place, no backup copy.
    (
        "An existing adapter is replaced in place, byte-for-byte with the checkout's template. There is no backup copy",
        r#"install -m 644 "${SCRIPT_DIR}/claude-print.yaml" "${NEEDLE_AGENTS_DIR}/claude-print.yaml""#,
        "an_existing_adapter_is_overwritten_in_place_at_0644_with_no_backup_copy",
    ),
    // Idempotence: the rerun re-copies the template, so hand edits to the
    // installed adapter revert on the next install run (the enforcing test
    // runs install.sh twice and hand-edits the installed copy in between).
    (
        "Re-running `install.sh` re-copies the template: hand edits to the installed adapter are reverted to the shipped template on the next run",
        r#"install -m 644 "${SCRIPT_DIR}/claude-print.yaml" "${NEEDLE_AGENTS_DIR}/claude-print.yaml""#,
        "rerunning_the_install_reverts_hand_edits_to_the_installed_adapter",
    ),
    // No-NEEDLE case: the mkdir lives inside the detection branch, so an
    // undetected machine gets no ~/.needle at all.
    (
        "the leg is skipped in silence: nothing NEEDLE-related is printed and `~/.needle` is not created",
        r#"mkdir -p "${NEEDLE_AGENTS_DIR}""#,
        "without_needle_the_agents_dir_is_not_created_and_nothing_needle_related_is_printed",
    ),
    // Missing source: the documented skip note, pinned verbatim on both
    // sides (the installer's echo and the note's quoting of it).
    (
        "Note: claude-print.yaml not found alongside install.sh — skipping NEEDLE config",
        "Note: claude-print.yaml not found alongside install.sh — skipping NEEDLE config",
        "no_adapter_beside_the_script_skips_the_needle_leg_with_a_note",
    ),
];

#[test]
fn needle_adapter_note_rows_match_the_installer_and_name_live_pinning_tests() {
    for (clause, installer_fragment, test_name) in NEEDLE_NOTE_SEMANTICS {
        assert!(
            NEEDLE_ADAPTER_NOTE_MD.contains(clause),
            "the note must carry the documented clause verbatim: {clause:?} — \
             update docs/notes/installer-needle-adapter.md and this pin together"
        );
        assert!(
            INSTALL_SH_SOURCE.contains(installer_fragment),
            "install.sh no longer carries the fragment implementing {clause:?}: \
             {installer_fragment:?} — the note and the installer have diverged"
        );
        assert!(
            THIS_TEST_SOURCE.contains(&format!("fn {test_name}()")),
            "the clause {clause:?} is pinned to `{test_name}`, which no longer \
             exists in tests/install_sh.rs — restore the test or re-point the pin"
        );
        assert!(
            needle_note_coverage_section().contains(&format!("`{test_name}`")),
            "the note's Hermetic-coverage table must name `{test_name}` as the \
             test enforcing {clause:?} — a row whose enforcing test goes unnamed \
             degrades into documentation-only"
        );
    }
}

#[test]
fn needle_adapter_note_ordering_claims_match_the_installer_s_control_flow() {
    // The note's two position claims are checkable against install.sh's own
    // source order: the adapter leg sits between the mock_claude leg and the
    // --check smoke, and the mkdir inside the detection branch precedes the
    // source check (why a missing source still creates the agents dir, and
    // why a no-NEEDLE machine gets no ~/.needle at all).
    let adapter_copy = INSTALL_SH_SOURCE
        .find(r#"install -m 644 "${SCRIPT_DIR}/claude-print.yaml""#)
        .expect("install.sh must copy the adapter template with install -m 644");
    let mock_leg = INSTALL_SH_SOURCE
        .find("Installed ${INSTALL_DIR}/mock_claude")
        .expect("install.sh must record the mock_claude placement");
    let check_leg = INSTALL_SH_SOURCE
        .find("Running claude-print --check")
        .expect("install.sh must run the --check smoke");
    let detection = INSTALL_SH_SOURCE
        .find(r#"command -v needle >/dev/null 2>&1 || [ -d "${NEEDLE_AGENTS_DIR}" ]"#)
        .expect("install.sh must carry the NEEDLE detection");
    let mkdir = INSTALL_SH_SOURCE
        .find(r#"mkdir -p "${NEEDLE_AGENTS_DIR}""#)
        .expect("install.sh must create the agents dir");
    let source_check = INSTALL_SH_SOURCE
        .find(r#"if [ -f "${SCRIPT_DIR}/claude-print.yaml" ]"#)
        .expect("install.sh must test for the template beside the script");

    assert!(
        mock_leg < adapter_copy && adapter_copy < check_leg,
        "the note pins the adapter leg between the mock_claude leg and the \
         --check smoke; install.sh's source order says otherwise"
    );
    assert!(
        detection < mkdir && mkdir < source_check,
        "the note pins the mkdir inside the detection branch and before the \
         source check; install.sh's source order says otherwise"
    );

    // The note must still make both claims, and its Hermetic-coverage table
    // must still name the test that enforces the ordering against a real
    // (tampered-release) run.
    for claim in [
        "The leg runs after the binary and `mock_claude` legs and before the `--check` smoke",
        "the `mkdir` precedes the source check",
    ] {
        assert!(
            NEEDLE_ADAPTER_NOTE_MD.contains(claim),
            "the note must carry the ordering claim verbatim: {claim:?}"
        );
    }
    let ordering_test = "a_failed_install_places_no_needle_adapter";
    assert!(
        THIS_TEST_SOURCE.contains(&format!("fn {ordering_test}()")),
        "the ordering claim is pinned to `{ordering_test}`, which no longer \
         exists in tests/install_sh.rs — restore the test or re-point the pin"
    );
    assert!(
        needle_note_coverage_section().contains(&format!("`{ordering_test}`")),
        "the note's Hermetic-coverage table must name `{ordering_test}` as the \
         test enforcing the ordering claim"
    );
}

// The repo-root resolution itself: pinned so a future edit can't quietly
// reintroduce a baked-only root — the failure mode the candidate chain
// exists for (a close gate re-running this suite in a fresh extraction of
// unchanged content instant-reuses the cached binary, and a baked-only
// root fails every filesystem test there with FileNotFound, which reads
// as installer drift but is cache state).

#[test]
fn repo_root_resolution_follows_the_candidate_chain() {
    // The live checkout the suite is running in — the same chain
    // `repo_path` uses, minus the override.
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
        fs::write(other.path().join(probe), "").expect("writing root probe file");
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
