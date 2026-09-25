//! Doc-consistency pin for the release runbook (`docs/notes/release-runbook.md`)
//! against the `claude-print-ci` WorkflowTemplate it documents.
//!
//! The README's [Repository & contributions] section states the provenance
//! split — Forgejo is the source of truth, GitHub Releases hosts the
//! artifacts — and the runbook turns that into the operator procedure:
//! commit/tag selection, mirror publication ordering, the asset table,
//! `sha256sums.txt` generation, version verification, artifact validation.
//! What nothing pinned is the agreement between the runbook and the
//! workflow it describes. `tests/platform_matrix_docs.rs` already binds the
//! WorkflowTemplate to the README matrix and `install.sh`'s mapping, but it
//! deliberately covers the *matrix* — asset names and the toolchain set.
//! It never reads the runbook, and the runbook makes claims the matrix pin
//! does not carry:
//!
//! - the publication **order** — tag pushed to Forgejo before
//!   `gh release create` references it (the 2026-08-16 draft-revert root
//!   cause: the GitHub mirror prunes GitHub-only refs, and GitHub demotes a
//!   release whose tag was deleted), draft/publish idempotency before the
//!   build, manifest generation before the upload;
//! - **manifest coverage** — `sha256sums.txt` lists exactly the uploaded
//!   assets except itself, with bare filenames matching the asset names;
//! - the **mode and version wiring** — the `tag` parameter's verify-only /
//!   release split, the version read from the cloned tree's `Cargo.toml`,
//!   and the `--version` / `--check` acceptance pair.
//!
//! Until claudepr-8d43bae4 each could drift alone: CI could reorder
//! publication (silently reintroducing the draft-demotion failure mode the
//! runbook's core invariant section exists for), attach an upload with no
//! manifest entry (uninstallable by the installer's fail-closed policy),
//! rename or add an asset (stale runbook table), or change the version
//! source (stale runbook "Version verification" section) — all green under
//! every existing test.
//!
//! The asset-name derivation is deliberately duplicated from
//! `tests/platform_matrix_docs.rs` rather than shared: two readers deriving
//! the same names independently from the template is the pin — a shared
//! helper could drift with the template and leave both docs stale.
//!
//! Library-level: reads the runbook, the WorkflowTemplate, `install.sh`,
//! and the README; spawns nothing.

use std::fs;
use std::path::PathBuf;

/// The one release toolchain CI is documented to install — the runbook's
/// "installs exactly one toolchain" claim, verified against the
/// WorkflowTemplate's `rustup target add` set.
const ONLY_TOOLCHAIN: &str = "x86_64-unknown-linux-musl";
const MUSL_TARGET_SUFFIX: &str = "-unknown-linux-musl";
const LINUX_ASSET_SUFFIX: &str = "-linux";

/// The manifest-generation line, pinned verbatim: its inputs are the
/// manifest's coverage (asserted against the upload set), and their bare
/// form is the runbook's byte-for-byte-asset-name claim.
const MANIFEST_LINE: &str =
    "sha256sum \"${CLAUDE_PRINT_ASSET}\" \"${MOCK_ASSET}\" last-claude-version.txt > sha256sums.txt";

/// Read a repo file, resolving the root from the *runtime*
/// `CARGO_MANIFEST_DIR` (compile-time value as fallback) so a test binary
/// reused from a different extraction by the shared target cache reads the
/// checkout under test — see `tests/platform_matrix_docs.rs::repo_file`
/// and `tests/docs_slug_consistency.rs::doc_files` for the rationale.
fn repo_file(relative: &str) -> String {
    let root = PathBuf::from(
        std::env::var("CARGO_MANIFEST_DIR")
            .unwrap_or_else(|_| env!("CARGO_MANIFEST_DIR").to_string()),
    );
    fs::read_to_string(root.join(relative))
        .unwrap_or_else(|e| panic!("read {relative} from the checkout under test: {e}"))
}

/// The doc with every whitespace run collapsed to one space, so prose
/// assertions survive line re-wrapping (the claims being pinned are
/// sentences, not formatting).
fn normalized(doc: &str) -> String {
    doc.split_whitespace().collect::<Vec<_>>().join(" ")
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

/// The asset names a successful release can carry, derived the way CI
/// derives them (installed-toolchain arch + `-linux`) rather than
/// hardcoded — independently of `tests/platform_matrix_docs.rs`.
fn published_assets() -> (String, String) {
    let targets = rustup_target_adds(&repo_file("claude-print-ci-workflowtemplate.yml"));
    assert_eq!(
        targets,
        [ONLY_TOOLCHAIN.to_string()],
        "the runbook and README matrix say CI installs only {ONLY_TOOLCHAIN} — \
         widening the published set requires updating them together with \
         install.sh's mapping"
    );
    let arch = ONLY_TOOLCHAIN
        .strip_suffix(MUSL_TARGET_SUFFIX)
        .unwrap_or_else(|| panic!("{ONLY_TOOLCHAIN} lost its musl suffix"));
    (
        format!("claude-print-{arch}{LINUX_ASSET_SUFFIX}"),
        format!("mock_claude-{arch}{LINUX_ASSET_SUFFIX}"),
    )
}

/// 1-based-ish byte position helper: `None` when the fragment is absent so
/// callers can name the missing wiring in their panic message.
fn position_of(haystack: &str, fragment: &str) -> Option<usize> {
    haystack.find(fragment)
}

#[test]
fn runbook_documents_the_release_provenance_contract() {
    let runbook = repo_file("docs/notes/release-runbook.md");
    let readme = repo_file("README.md");

    // The six sections the runbook exists to connect (bead claudepr-8d43bae4):
    // deleting or renaming any one is a rewrite of the contract, not an edit.
    for heading in [
        "## Commit and tag selection",
        "## Mirror publication",
        "## Release assets and the platform matrix",
        "## sha256sums.txt generation",
        "## Version verification",
        "## Artifact validation",
    ] {
        assert!(
            runbook.contains(heading),
            "the release runbook must carry its `{heading}` section"
        );
    }

    // The one-directional chain, stated in the doc's own fence.
    let runbook_norm = normalized(&runbook);
    assert!(
        runbook_norm.contains("Nothing flows back"),
        "the runbook must state that nothing flows back from GitHub"
    );
    assert!(
        runbook.contains("cloned from Forgejo"),
        "the runbook must state the workflow clones from Forgejo, never the mirror"
    );

    // README cross-links must resolve: the anchors name real headings.
    for (link, heading) in [
        (
            "../../README.md#repository--contributions",
            "### Repository & contributions",
        ),
        ("../../README.md#release-checklist", "## Release checklist"),
        (
            "../../README.md#supported-platforms",
            "### Supported platforms",
        ),
    ] {
        assert!(
            runbook.contains(link),
            "the runbook must cross-link the README via {link}"
        );
        assert!(
            readme.contains(heading),
            "the runbook's link {link} targets README heading {heading:?}, which is gone"
        );
    }

    // The Hermetic-coverage table must name every suite that pins this
    // territory, so a reader lands on the enforcement from the procedure.
    for suite in [
        "tests/platform_matrix_docs.rs",
        "tests/install_sh_arch.rs",
        "tests/install_sh.rs",
        "tests/release_runbook_docs.rs",
    ] {
        assert!(
            runbook.contains(suite),
            "the runbook's coverage table must name {suite}"
        );
    }
}

#[test]
fn runbook_asset_claims_match_the_publishable_matrix() {
    let (binary_asset, mock_asset) = published_assets();
    let runbook = repo_file("docs/notes/release-runbook.md");
    let template = repo_file("claude-print-ci-workflowtemplate.yml");
    let runbook_norm = normalized(&runbook);

    // The asset table: both derived names and both versionless assets. A
    // widened or renamed release fails here until the runbook's table —
    // and, via tests/platform_matrix_docs.rs, the README matrix and
    // install.sh — move with it.
    for asset in [
        binary_asset.as_str(),
        mock_asset.as_str(),
        "last-claude-version.txt",
        "sha256sums.txt",
    ] {
        assert!(
            runbook.contains(asset),
            "the runbook's asset table must name the published asset {asset:?}"
        );
    }

    // The claims behind the table: one toolchain, uname-derived target,
    // static-musl gate, size gate.
    assert!(
        runbook.contains(ONLY_TOOLCHAIN),
        "the runbook must state CI installs only {ONLY_TOOLCHAIN}"
    );
    assert!(
        runbook_norm.contains("installs exactly one toolchain"),
        "the runbook must explain the toolchain claim behind the asset names"
    );
    assert!(
        runbook.contains("verify_static") && runbook.contains("10 MiB"),
        "the runbook must document the static (HR-1) and 10 MiB build gates"
    );
    assert!(
        template.contains("verify_static")
            && template.contains("MAX_SIZE_BYTES=$((10 * 1024 * 1024))"),
        "WorkflowTemplate lost the gates the runbook documents (verify_static / size cap)"
    );
}

#[test]
fn workflow_publication_order_matches_the_runbook() {
    let template = repo_file("claude-print-ci-workflowtemplate.yml");
    let runbook_norm = normalized(&repo_file("docs/notes/release-runbook.md"));

    // The wiring fragments the ordering rests on.
    for fragment in [
        "git clone --depth 1 --branch \"$TAG\" --single-branch",
        "https://git.ardenone.com/jedarden/claude-print.git",
        "git push \"https://x-token:${FORGEJO_TOKEN}@git.ardenone.com/jedarden/claude-print.git\" \"refs/tags/v${VERSION}\"",
        "gh release view \"v${VERSION}\"",
        "gh release edit \"v${VERSION}\" --repo jedarden/claude-print --draft=false",
        "cargo build --release --target \"${MUSL_TARGET}\" --bin claude-print",
        "gh release create \"v${VERSION}\"",
        "--target main",
    ] {
        assert!(
            template.contains(fragment),
            "WorkflowTemplate lost the publication wiring fragment: {fragment}"
        );
    }

    // The order the runbook documents: tag on Forgejo FIRST (the mirror
    // prunes GitHub-only refs and GitHub demotes a release whose tag was
    // deleted — the 2026-08-16 draft-revert root cause), idempotency check
    // before spending build time, manifest before the upload references it.
    let mut cursor = 0usize;
    for stage in [
        "git push \"https://x-token:${FORGEJO_TOKEN}@git.ardenone.com/jedarden/claude-print.git\" \"refs/tags/v${VERSION}\"",
        "gh release view \"v${VERSION}\"",
        "cargo build --release --target \"${MUSL_TARGET}\" --bin claude-print",
        MANIFEST_LINE,
        "gh release create \"v${VERSION}\"",
    ] {
        let at = position_of(&template[cursor..], stage)
            .map(|i| i + cursor)
            .unwrap_or_else(|| panic!("publication stage {stage:?} not found after the previous stage"));
        cursor = at;
    }

    // The runbook states the invariant and the idempotency semantics the
    // order implements.
    assert!(
        runbook_norm.contains("must exist on Forgejo before any GitHub release references it"),
        "the runbook must state the tag-first invariant"
    );
    assert!(
        runbook_norm.contains("publish it in place (`gh release edit --draft=false`)"),
        "the runbook must document the draft publish-in-place path"
    );
    assert!(
        runbook_norm.contains("a re-run of an already-shipped release is a no-op"),
        "the runbook must document the published-release no-op path"
    );
}

#[test]
fn manifest_covers_exactly_the_uploaded_assets() {
    let template = repo_file("claude-print-ci-workflowtemplate.yml");
    let runbook_norm = normalized(&repo_file("docs/notes/release-runbook.md"));
    let installer = repo_file("install.sh");

    // The manifest line itself, verbatim — its inputs ARE the coverage.
    assert!(
        template.contains(MANIFEST_LINE),
        "WorkflowTemplate must generate sha256sums.txt with the documented line: \
         {MANIFEST_LINE}"
    );

    // The upload set: exactly the manifest's inputs plus the manifest,
    // each present as an upload argument after --target main (the --notes
    // block also quotes strings, so the uploads are scoped to the segment
    // between --target main and the closing echo).
    let upload_start = template
        .find("--target main")
        .expect("gh release create must carry --target main");
    let upload_end = template[upload_start..]
        .find("Release v${VERSION} created successfully")
        .map(|i| i + upload_start)
        .expect("the workflow must echo completion after gh release create");
    let upload_segment = &template[upload_start..upload_end];
    for upload in [
        "\"./${CLAUDE_PRINT_ASSET}\"",
        "\"./${MOCK_ASSET}\"",
        "\"./last-claude-version.txt\"",
        "\"./sha256sums.txt\"",
    ] {
        assert!(
            upload_segment.contains(upload),
            "gh release create must attach {upload} — the manifest covers exactly \
             the uploads besides sha256sums.txt"
        );
    }
    let upload_args = upload_segment.match_indices("\"./").count();
    assert_eq!(
        upload_args, 4,
        "a release carries exactly four uploaded assets (found {upload_args} `\"./` \
         args) — a new upload needs a sha256sums.txt entry and a runbook table row"
    );

    // The bare-filename claim and its consumer side: the generator emits
    // bare names (the pinned MANIFEST_LINE has no ./ on its inputs), the
    // runbook states why, and install.sh's parser still tolerates the
    // decorated forms when reading foreign manifests.
    assert!(
        runbook_norm.contains("every uploaded asset *except itself*"),
        "the runbook must state the manifest covers every uploaded asset except itself"
    );
    assert!(
        runbook_norm.contains("recorded without a `./` prefix"),
        "the runbook must state manifest entries are recorded with bare filenames"
    );
    assert!(
        installer.contains("sub(/^\\.\\//, \"\", name)"),
        "install.sh's checksum parser must keep tolerating a ./ prefix — the \
         runbook documents that tolerance"
    );
    assert!(
        installer.contains("CHECKSUMS_ASSET=\"sha256sums.txt\""),
        "install.sh must verify against the release's sha256sums.txt manifest"
    );
}

#[test]
fn runbook_mode_and_version_claims_match_the_workflow() {
    let template = repo_file("claude-print-ci-workflowtemplate.yml");
    let runbook = repo_file("docs/notes/release-runbook.md");
    let runbook_norm = normalized(&runbook);
    let installer = repo_file("install.sh");

    // Mode wiring: the parameter's default and both branches.
    assert!(
        template.contains(
            "# Empty = verify-only mode (build from main); set to \"vX.Y.Z\" for release"
        ),
        "WorkflowTemplate must document the tag parameter's two modes"
    );
    assert!(
        template.contains("Verify-only mode: building from main")
            && template.contains("Release mode: building from tag $TAG"),
        "WorkflowTemplate must implement both clone modes"
    );
    assert!(
        runbook_norm.contains("takes one parameter, `tag`")
            && runbook_norm.contains("verify-only")
            && runbook_norm.contains("release mode"),
        "the runbook must document the tag parameter and its two modes"
    );

    // Version wiring: the version is read from the cloned tree's
    // Cargo.toml and keyed off as v${VERSION}.
    assert!(
        template.contains("grep -m1 '^version' Cargo.toml"),
        "WorkflowTemplate must read the released version from Cargo.toml"
    );
    assert!(
        runbook.contains("`grep -m1 '^version'`")
            && runbook_norm.contains("keys off `v${VERSION}`"),
        "the runbook must tie the version source to the tag and release name"
    );

    // The recorded Claude version travels with the release.
    for fragment in [
        "target/last-claude-version.txt",
        "cp target/last-claude-version.txt last-claude-version.txt",
    ] {
        assert!(
            template.contains(fragment),
            "WorkflowTemplate lost the last-claude-version wiring: {fragment}"
        );
    }
    assert!(
        runbook.contains("target/last-claude-version.txt"),
        "the runbook must name the gate's version-recording path"
    );

    // The acceptance pair: --check then --version, both in install.sh's
    // install sequence and in the runbook's validation section.
    let check_at = installer
        .find("\"${INSTALL_DIR}/claude-print\" --check")
        .expect("install.sh must run the post-install --check");
    let version_at = installer
        .find("\"${INSTALL_DIR}/claude-print\" --version")
        .expect("install.sh must run the post-install --version");
    assert!(
        check_at < version_at,
        "install.sh must run --check before the closing --version"
    );
    assert!(
        runbook.contains("`CARGO_PKG_VERSION`"),
        "the runbook must tie --version output to the crate version"
    );
    assert!(
        runbook_norm.contains("must name the release it came from"),
        "the runbook must state the installed --version identifies the release"
    );
}
