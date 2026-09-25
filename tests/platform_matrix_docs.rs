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
//! Library-level: reads three files, spawns nothing. The installer's
//! *behavior* per matrix row (exit 1 before any download, nothing placed,
//! the actionable message on stderr) is pinned hermetically by
//! `tests/install_sh_arch.rs`; this file pins what the three artifacts
//! claim about each other.

use std::fs;
use std::path::PathBuf;

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

/// Read a repo file, resolving the root from the *runtime*
/// `CARGO_MANIFEST_DIR` (compile-time value as fallback). The compile-time
/// value alone bakes the building checkout's path into the test binary;
/// when the shared target cache reuses that binary from a different
/// extraction — exactly the clean-tree verification NEEDLE re-runs — the
/// read would hit a directory that no longer exists. See
/// `tests/docs_slug_consistency.rs::doc_files` for the full rationale.
fn repo_file(relative: &str) -> String {
    let root = PathBuf::from(
        std::env::var("CARGO_MANIFEST_DIR")
            .unwrap_or_else(|_| env!("CARGO_MANIFEST_DIR").to_string()),
    );
    fs::read_to_string(root.join(relative))
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
