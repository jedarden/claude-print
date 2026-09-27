//! Doc-consistency pin for the README "Supported platforms" release matrix.
//!
//! The README's Prerequisites say "Linux on x86_64 only — see Supported
//! platforms for the full release matrix", the matrix section names
//! `claude-print-x86_64-linux` as the only prebuilt asset, and
//! `tests/install_sh_arch.rs` pins the installer's behavior row by row.
//! What nothing pinned is the *agreement* between the three artifacts the
//! matrix describes: the matrix is a claim about what `claude-print-ci`
//! publishes, and the publisher lives in
//! `claude-print-ci-workflowtemplate.yml` — a file the installer tests
//! never read (they hand-build their fake releases). Until claudepr-d0b97e47
//! each artifact could move alone:
//!
//! - CI could widen the release (a second `rustup target add`) while the
//!   README still said x86_64-only;
//! - CI could rename the assets while `install.sh` kept requesting names
//!   no release carries;
//! - `install.sh` could regain an arch→asset mapping (the claudepr-b583cd6a
//!   regression: aarch64 passed the gate and died on a download error)
//!   with the README matrix none the wiser;
//! - the README section could be deleted outright with every test green.
//!
//! The load-bearing fact is the toolchain set: CI installs exactly one
//! target, `x86_64-unknown-linux-musl`, and derives its build target from
//! `uname -m` (`${ARCH}-unknown-linux-musl`). Cargo refuses to build a
//! target whose std was never installed, so a successful release implies
//! the runner's arch equaled the installed toolchain's arch, which means
//! `TARGET="${ARCH}-linux"` can only ever have resolved to `x86_64-linux`.
//! These tests re-run that derivation instead of hardcoding its output:
//! the asset names are computed from the WorkflowTemplate's toolchain set
//! and then asserted against the README table and `install.sh`'s mapping,
//! so all three must agree or the build fails.
//!
//! Library-level: spawns nothing. The installer's *behavior* per matrix row
//! (exit 1 before any download, nothing placed, the actionable message on
//! stderr) is pinned hermetically by `tests/install_sh_arch.rs`; this file
//! pins what the artifacts claim about each other — README, WorkflowTemplate,
//! install.sh, and the installer suites' fakes. Since claudepr-aa1fe307 it
//! also pins the matrix's PTY/ConPTY statement to the implementation it is
//! a claim about: "PTY support requires POSIX — no Windows ConPTY"
//! (Prerequisites) and the non-Linux row's ConPTY reason stand on
//! `src/pty.rs`'s POSIX `openpty`/`login_tty` spawner and `src/check.rs`'s
//! same-API probe, so those wirings are pinned and no `cfg(windows)`
//! branch, `std::os::windows` import, or ConPTY mention under `src/`, and
//! no Windows PTY dependency in `Cargo.toml`, may appear while they stand —
//! a ConPTY port or a backend swap fails the build until the platform
//! claim moves in the same commit.

use std::fs;
use std::path::{Path, PathBuf};

/// The one release toolchain CI installs — the README's "installs only the
/// `x86_64-unknown-linux-musl` toolchain" claim, verified against the
/// WorkflowTemplate's `rustup target add` set.
const ONLY_TOOLCHAIN: &str = "x86_64-unknown-linux-musl";

/// Suffix CI strips from the toolchain triple to get the runner arch's
/// asset suffix: `${ARCH}-unknown-linux-musl` → `${ARCH}-linux`.
const MUSL_TARGET_SUFFIX: &str = "-unknown-linux-musl";
const LINUX_ASSET_SUFFIX: &str = "-linux";

/// The supported-matrix line the installer's refusal must carry — the same
/// string `tests/install_sh_arch.rs` asserts behaviorally (`MATRIX_LINE`),
/// so the doc claim and the tested message cannot fork.
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
fn repo_root() -> PathBuf {
    resolve_repo_root(
        std::env::var("CLAUDE_PRINT_TEST_REPO").ok().as_deref(),
        std::env::var("CARGO_MANIFEST_DIR").ok().as_deref(),
        env!("CARGO_MANIFEST_DIR"),
    )
    .unwrap_or_else(|e| panic!("locating the repo root to read repo files from: {e}"))
}

/// [`repo_root`]'s candidate chain as a pure function, so the precedence
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

/// Read a repo file from the checkout under test.
fn repo_file(relative: &str) -> String {
    fs::read_to_string(repo_root().join(relative))
        .unwrap_or_else(|e| panic!("read {relative} from the checkout under test: {e}"))
}

/// The README's `### Supported platforms` subsection — from its heading up
/// to the next heading of any level. Scoped to this slice so a
/// matrix-relevant string that merely survives elsewhere in the README
/// cannot satisfy a pin.
fn supported_platforms_section(readme: &str) -> &str {
    let start = readme
        .find("### Supported platforms")
        .unwrap_or_else(|| panic!("README.md must carry a `### Supported platforms` heading"));
    let rest = &readme[start..];
    // The next heading after the section body (`## Self-check` today);
    // `\n#` matches any level, so a re-nesting to `####` cannot hide it.
    let end = rest[1..].find("\n#").map(|i| i + 1).unwrap_or(rest.len());
    &rest[..end]
}

/// Every `rustup target add <triple>` in the WorkflowTemplate — the set of
/// targets a release build can actually have linked std for.
fn rustup_target_adds(template: &str) -> Vec<String> {
    template
        .lines()
        .filter_map(|line| line.trim().strip_prefix("rustup target add "))
        .map(str::to_string)
        .collect()
}

/// The runner arch the only installed toolchain can build for: the triple
/// minus its `-unknown-linux-musl` suffix.
fn toolchain_arch() -> String {
    ONLY_TOOLCHAIN
        .strip_suffix(MUSL_TARGET_SUFFIX)
        .unwrap_or_else(|| panic!("{ONLY_TOOLCHAIN} lost its musl suffix"))
        .to_string()
}

/// The asset names a successful release can carry, derived the way CI
/// derives them: installed-toolchain arch + `-linux`, prefixed with the
/// artifact names the template and install.sh both assemble as
/// `<name>-${TARGET}`.
fn published_assets() -> (String, String) {
    let arch = toolchain_arch();
    (
        format!("claude-print-{arch}{LINUX_ASSET_SUFFIX}"),
        format!("mock_claude-{arch}{LINUX_ASSET_SUFFIX}"),
    )
}

#[test]
fn readme_documents_a_supported_platforms_matrix() {
    let readme = repo_file("README.md");
    let section = supported_platforms_section(&readme);

    // The Prerequisites bullet cross-links the matrix and states the two
    // constraints the table elaborates: x86_64-only and POSIX-only.
    assert!(
        readme.contains("[Supported platforms](#supported-platforms)"),
        "Prerequisites must cross-link the Supported platforms matrix"
    );
    assert!(
        readme.contains("PTY support requires POSIX"),
        "Prerequisites must state the POSIX requirement behind Linux-only"
    );

    // The matrix table: the two uname axes and all three outcome rows.
    assert!(
        section.contains("| `uname -s` | `uname -m` |"),
        "the matrix table must carry its uname -s / uname -m header"
    );
    assert!(
        section.contains("| `Linux` | `x86_64` |"),
        "the supported row (Linux / x86_64) must exist"
    );
    assert!(
        section.contains("`aarch64`, `armv7l`"),
        "the non-x86_64 Linux row must name example architectures"
    );
    assert!(
        section.contains("any other OS") && section.contains("ConPTY"),
        "the non-Linux row must exist and carry the no-ConPTY reason"
    );

    // The claim these docs make about CI — the exact tokens the
    // ci_publishes_exactly_the_documented_x86_64_musl_matrix test verifies.
    assert!(
        section.contains(ONLY_TOOLCHAIN),
        "the section must state that CI installs only {ONLY_TOOLCHAIN}"
    );
    let (binary_asset, _) = published_assets();
    let asset_suffix = format!("{}{LINUX_ASSET_SUFFIX}", toolchain_arch());
    assert!(
        section.contains(&asset_suffix),
        "the section must state that {asset_suffix:?} is the only asset name a release carries"
    );
    assert!(
        section.contains(&binary_asset),
        "the section must name the prebuilt asset {binary_asset:?}"
    );

    // Both pins are discoverable from the section: the behavioral one and
    // this doc-consistency one.
    assert!(
        section.contains("tests/install_sh_arch.rs"),
        "the section must point at the behavioral matrix pin"
    );
    assert!(
        section.contains("tests/platform_matrix_docs.rs"),
        "the section must point at this doc-consistency pin"
    );

    // The arch-refusal remedy's anchor must resolve.
    assert!(
        section.contains("(#build-from-source)") && readme.contains("### Build from source"),
        "the build-from-source remedy link must point at a real heading"
    );
}

#[test]
fn ci_publishes_exactly_the_documented_x86_64_musl_matrix() {
    let template = repo_file("claude-print-ci-workflowtemplate.yml");

    // The whole derivation rests on the toolchain set: exactly the one
    // musl target CI is documented to install. A second `rustup target
    // add` (widening the release) fails here until the README matrix and
    // install.sh mapping are deliberately updated to match.
    let targets = rustup_target_adds(&template);
    assert_eq!(
        targets,
        [ONLY_TOOLCHAIN.to_string()],
        "the documented matrix says CI installs only {ONLY_TOOLCHAIN} — \
         widening the published set requires updating README \"Supported \
         platforms\" and install.sh's mapping together"
    );

    // The target is derived from the runner's uname, and only installed
    // targets can build, so the derivation in the module doc holds only if
    // these wiring fragments survive.
    for fragment in [
        "ARCH=$(uname -m)",
        "TARGET=\"${ARCH}-linux\"",
        "MUSL_TARGET=\"${ARCH}-unknown-linux-musl\"",
        "cargo build --release --target \"${MUSL_TARGET}\" --bin claude-print",
        "CLAUDE_PRINT_ASSET=\"claude-print-${TARGET}\"",
        "MOCK_ASSET=\"mock_claude-${TARGET}\"",
        // musl means static: the HR-1 gate behind "static musl binary"
        "verify_static",
        // the manifest install.sh fails closed against
        "> sha256sums.txt",
        "\"./sha256sums.txt\"",
    ] {
        assert!(
            template.contains(fragment),
            "WorkflowTemplate lost the release-matrix wiring fragment: {fragment}"
        );
    }
}

#[test]
fn derived_asset_names_match_readme_and_installer() {
    let (binary_asset, _) = published_assets();
    let arch = toolchain_arch();
    let readme = repo_file("README.md");
    let readme_section = supported_platforms_section(&readme);
    let installer = repo_file("install.sh");

    // README side: the documented prebuilt asset is the derived one.
    assert!(
        readme_section.contains(&binary_asset),
        "README must name the published asset {binary_asset:?}, derived from \
         the WorkflowTemplate's {ONLY_TOOLCHAIN} toolchain"
    );

    // Installer side: exactly one arch→TARGET mapping, the derived one.
    // The claudepr-b583cd6a regression was a second mapping (aarch64) that
    // turned the supported-platform refusal into a 404 — so count the
    // mappings, don't just find one.
    let mapping = format!("Linux-{arch}) TARGET=\"{arch}{LINUX_ASSET_SUFFIX}\"");
    assert!(
        installer.contains(&mapping),
        "install.sh must map Linux-{arch} to the published asset suffix \
         ({arch}{LINUX_ASSET_SUFFIX}): {mapping:?} not found"
    );
    let mapping_count = installer.match_indices(") TARGET=\"").count();
    assert_eq!(
        mapping_count, 1,
        "install.sh must carry exactly one arch→TARGET mapping (found \
         {mapping_count}) — every other combination is refused"
    );

    // The installer assembles asset names with the same scheme the
    // publisher uses, so its computed names equal the published ones
    // byte-for-byte.
    assert!(
        installer.contains("BINARY_ASSET=\"claude-print-${TARGET}\""),
        "install.sh must derive the binary asset name from TARGET"
    );
    assert!(
        installer.contains("MOCK_ASSET=\"mock_claude-${TARGET}\""),
        "install.sh must derive the fixture asset name from TARGET"
    );
    assert!(
        installer.contains("CHECKSUMS_ASSET=\"sha256sums.txt\""),
        "install.sh must verify against the release's sha256sums.txt manifest"
    );

    // The refusal states the same matrix the README documents — the exact
    // line tests/install_sh_arch.rs pins behaviorally.
    assert!(
        installer.contains(MATRIX_LINE),
        "install.sh's refusal must state the supported matrix ({MATRIX_LINE:?}) \
         — the same line tests/install_sh_arch.rs asserts"
    );

    // "Refused before any download" (README) / "before anything is
    // downloaded" (install.sh header): the gate must textually precede the
    // first fetch, or those claims stop being true.
    let gate_at = installer
        .find("case \"${OS}-${ARCH}\" in")
        .expect("install.sh must gate on the detected platform");
    let first_fetch_at = installer
        .find("curl -fsSL")
        .expect("install.sh must download release assets");
    assert!(
        gate_at < first_fetch_at,
        "the platform gate must precede every download — README says \
         \"Refused before any download\""
    );
}

#[test]
fn installer_suites_fake_up_releases_from_the_published_asset_names() {
    // The behavioral suites hand-build their fake releases (via the
    // `file://` override, or a recording `curl` for the default-source
    // suite) — if CI ever renames an asset, those fakes would keep
    // passing against names no release carries. Pin their constants to
    // the names derived from the WorkflowTemplate's toolchain set, so a
    // rename fails here first.
    let (binary_asset, mock_asset) = published_assets();
    for suite in [
        "tests/install_sh.rs",
        "tests/install_sh_arch.rs",
        "tests/install_sh_release_source.rs",
    ] {
        let source = repo_file(suite);
        assert!(
            source.contains(&format!("const BINARY_ASSET: &str = \"{binary_asset}\";")),
            "{suite} must fake the published binary asset name {binary_asset:?}"
        );
        assert!(
            source.contains(&format!("const MOCK_ASSET: &str = \"{mock_asset}\";")),
            "{suite} must fake the published fixture asset name {mock_asset:?}"
        );
    }
}

/// Every `.rs` file under `src/` (recursively — the module map is flat
/// today, but a Windows port would arrive as a new subtree), as
/// `(file_name, contents)` pairs sorted by path for deterministic failure
/// messages.
fn src_rust_sources() -> Vec<(String, String)> {
    fn collect(dir: &std::path::Path, out: &mut Vec<PathBuf>) {
        for entry in fs::read_dir(dir).unwrap_or_else(|e| panic!("read {}: {e}", dir.display())) {
            let path = entry
                .unwrap_or_else(|e| panic!("read {}: {e}", dir.display()))
                .path();
            if path.is_dir() {
                collect(&path, out);
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                out.push(path);
            }
        }
    }
    let mut paths = Vec::new();
    collect(&repo_root().join("src"), &mut paths);
    paths.sort();
    paths
        .into_iter()
        .map(|path| {
            let name = path
                .strip_prefix(repo_root())
                .unwrap_or(&path)
                .display()
                .to_string();
            let body = fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("read {name} from the checkout under test: {e}"));
            (name, body)
        })
        .collect()
}

/// The PTY/ConPTY half of the platform claim, pinned to the implementation
/// it is a claim about (claudepr-aa1fe307). The artifact pins above bind
/// README ↔ WorkflowTemplate ↔ install.sh, but none of them reads the PTY
/// implementation — until this test, "PTY support requires POSIX — no
/// Windows ConPTY" was verified only as README text, so a ConPTY port (a
/// `windows-sys` dependency, a `#[cfg(windows)]` branch in `src/pty.rs`)
/// or a swap of the PTY backend off POSIX `openpty` would have left the
/// statement stale with every test green.
#[test]
fn pty_conpty_statement_matches_the_implementation() {
    let readme = repo_file("README.md");
    let section = supported_platforms_section(&readme);

    // Doc side: both PTY/ConPTY statements, exactly as written.
    assert!(
        readme.contains("PTY support requires POSIX — no Windows ConPTY"),
        "Prerequisites must carry the POSIX requirement and its no-ConPTY \
         consequence in one sentence"
    );
    assert!(
        section.contains("claude-print is Linux-only"),
        "the non-Linux row must state the Linux-only scope"
    );
    assert!(
        section.contains("Windows would need ConPTY — a POSIX PTY does not exist there"),
        "the non-Linux row must carry the ConPTY reason verbatim"
    );

    // Implementation side: the PTY pair is allocated through the POSIX
    // openpty(3) API and the child's controlling terminal set by POSIX
    // login_tty(3) — the two calls "requires POSIX" rests on — and the
    // --check PTY probe uses the same API, so the self-check keeps
    // proving the claim rather than a different mechanism.
    let pty = repo_file("src/pty.rs");
    assert!(
        pty.contains("use nix::pty::{openpty"),
        "src/pty.rs must allocate the PTY pair through the POSIX openpty API \
         the README claim rests on"
    );
    assert!(
        pty.contains("libc::login_tty("),
        "src/pty.rs must set the child's controlling terminal via POSIX \
         login_tty — the claim \"PTY support requires POSIX\" rests on it"
    );
    assert!(
        repo_file("src/check.rs").contains("use nix::pty::openpty;"),
        "the --check PTY probe (src/check.rs) must use the same POSIX \
         openpty API as the spawner"
    );

    // Nothing Windows-shaped may exist under src/ while the claim stands.
    // The needles are deliberately precise so the slice method `.windows(`
    // (used by other suites' sources) can never trip one.
    for (name, body) in src_rust_sources() {
        for needle in [
            "cfg(windows)",
            "cfg(target_os = \"windows\")",
            "std::os::windows",
        ] {
            assert!(
                !body.contains(needle),
                "{name} carries {needle:?} — a Windows code path cannot appear \
                 while the README says \"no Windows ConPTY\"; move the platform \
                 claim (README matrix + this pin) in the same commit"
            );
        }
        assert!(
            !body.to_lowercase().contains("conpty"),
            "{name} mentions ConPTY — the README says there is no Windows \
             ConPTY support; reconcile the claim and this pin in one commit"
        );
    }

    // The dependency graph agrees: no Windows PTY backend may be declared
    // while the claim stands. The `windows` substring covers `windows-sys`,
    // `windows-args`, and any `[target.'cfg(windows)'.dependencies]`
    // section heading; the rest are the known portable/ConPTY backends.
    let cargo_toml = repo_file("Cargo.toml").to_lowercase();
    for needle in ["windows", "winapi", "conpty", "portable-pty", "wezterm-pty"] {
        assert!(
            !cargo_toml.contains(needle),
            "Cargo.toml names {needle:?} — a Windows/ConPTY PTY backend cannot \
             be declared while the README says \"no Windows ConPTY\""
        );
    }
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
