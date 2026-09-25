//! Contract pin for `install.sh`'s default release source — the download
//! path every other installer suite leaves unexercised.
//!
//! The README names GitHub Releases (`jedarden/claude-print`) as the
//! supported distribution channel, and the hermetic installer suites
//! (`tests/install_sh.rs`, `tests/install_sh_arch.rs`) exercise every
//! download through the `CLAUDE_PRINT_RELEASE_URL` override pointed at a
//! `file://` fake release. What nothing pinned is the *default*: with the
//! override unset, install.sh resolves
//! `https://github.com/${REPO}/releases/latest/download` and fetches the
//! checksum manifest first and then the arch-specific asset names from
//! that base. Until this suite the default line could drift — a moved
//! host, a repointed repo slug, a baked-in tag, a manifest that is no
//! longer fetched first — with every test green, because the override
//! masked the default on every documented run.
//!
//! The mechanism is a recording `curl` shim placed first on the child's
//! PATH: it appends each requested URL to a log, then serves the bytes of
//! the request's basename from a fake release directory — the
//! release-host equivalent of the siblings' `file://` dir. A name the
//! release does not carry exits 22 (the code curl's `-f` fails with on an
//! HTTP error), so install.sh's error handling sees an ordinary download
//! failure. The script keeps doing all of its own resolution: the
//! behavioral asserts parse no install.sh source, so the logged URLs are
//! what the script actually builds, in the order it builds them.
//!
//! The source-level and WorkflowTemplate asserts are the "against the
//! release workflow" half of the contract: the default base's repo slug
//! must be the repo the workflow publishes to and clones from, and the
//! tag-less `releases/latest/download` resolution must stay coupled to the
//! workflow's `v${VERSION}` release naming — the installer floats to
//! whatever release is latest, and the version discipline lives entirely
//! in the publisher (the runbook's §"Version verification"). The
//! publication-order and manifest-coverage halves of that workflow
//! contract are pinned by `tests/release_runbook_docs.rs`; this suite pins
//! what a *downloader* of those releases must agree to.
//!
//! Asset names are pinned to the x86_64 layout, the only one CI publishes
//! (README "Supported platforms") — the same standing assumption
//! `tests/install_sh.rs` runs under.
//!
//! Hermetic: no network (the shim answers every fetch locally), temp
//! `HOME`, fake `claude` satisfies the preflight.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use tempfile::TempDir;

/// The same const names and shapes as the sibling installer suites:
/// `tests/platform_matrix_docs.rs::installer_suites_fake_up_releases_from_the_published_asset_names`
/// pins these constants to the names derived from the WorkflowTemplate's
/// toolchain set, so a CI asset rename fails there before this suite's
/// fake release can go stale against the real publisher.
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

/// The repo slug the default base is built from — defined once in
/// install.sh's `REPO="…"`, pinned to the workflow's publish target and
/// the canonical Forgejo repo by
/// [`default_repo_slug_matches_the_workflow_publisher_and_the_canonical_repo`].
const REPO_SLUG: &str = "jedarden/claude-print";

/// The default release base install.sh resolves when
/// `CLAUDE_PRINT_RELEASE_URL` is unset — built from [`REPO_SLUG`] so the
/// behavioral asserts and the slug pin cannot fork.
fn default_base() -> String {
    format!("https://github.com/{REPO_SLUG}/releases/latest/download")
}

/// The declared default line, verbatim: overridable only through
/// `CLAUDE_PRINT_RELEASE_URL`, composed from `REPO`, and tag-less. The
/// behavioral tests prove the script *runs* this default; this pin proves
/// the script *declares* it in the documented shape a mirror operator
/// reads (install.sh's header comment) rather than through some other
/// assignment the tests happen not to trip.
const DECLARED_DEFAULT_LINE: &str = r#"RELEASE_URL="${CLAUDE_PRINT_RELEASE_URL:-https://github.com/${REPO}/releases/latest/download}""#;

/// The Forgejo URL the workflow clones from — the canonical repo the
/// default slug must name (the GitHub mirror is never a clone source).
const FORGEJO_CLONE_URL: &str = "https://git.ardenone.com/jedarden/claude-print.git";

/// Shell body of the recording `curl` (`$CURL_LOG` receives one URL per
/// line, in request order; `$CURL_RELEASE_DIR` is the fake release). The
/// arg shape is install.sh's exact `curl -fsSL URL -o OUT`: any `-`-led
/// argument is a flag cluster, the value after `-o` is the output file,
/// and the first bare argument is the URL.
const RECORDING_CURL: &str = concat!(
    "#!/bin/sh\n",
    "url=\n",
    "out=\n",
    "prev=\n",
    "for arg in \"$@\"; do\n",
    "  case \"${prev}\" in\n",
    "    o)\n",
    "      out=${arg}\n",
    "      prev=\n",
    "      continue\n",
    "      ;;\n",
    "  esac\n",
    "  case ${arg} in\n",
    "    -o) prev=o ;;\n",
    "    -*) ;;\n",
    "    *)\n",
    "      if [ -z \"${url}\" ]; then\n",
    "        url=${arg}\n",
    "      fi\n",
    "      ;;\n",
    "  esac\n",
    "done\n",
    "printf '%s\\n' \"${url}\" >> \"${CURL_LOG}\"\n",
    "if [ -n \"${out}\" ] && [ -f \"${CURL_RELEASE_DIR}/${url##*/}\" ]; then\n",
    "  cat \"${CURL_RELEASE_DIR}/${url##*/}\" > \"${out}\"\n",
    "  exit 0\n",
    "fi\n",
    "echo \"curl: (22) The requested URL returned error: 404\" >&2\n",
    "exit 22\n",
);

/// Read a repo file, resolving the root from the *runtime*
/// `CARGO_MANIFEST_DIR` (compile-time value as fallback). The compile-time
/// value alone bakes the building checkout's path into the test binary;
/// when the shared target cache reuses that binary from a different
/// extraction — exactly the clean-tree verification NEEDLE re-runs — the
/// read would hit a directory that no longer exists. See
/// `tests/docs_slug_consistency.rs::doc_files` for the full rationale.
fn repo_file(relative: &str) -> String {
    fs::read_to_string(repo_path(relative))
        .unwrap_or_else(|e| panic!("read {relative} from the checkout under test: {e}"))
}

fn repo_path(relative: &str) -> PathBuf {
    PathBuf::from(
        std::env::var("CARGO_MANIFEST_DIR")
            .unwrap_or_else(|_| env!("CARGO_MANIFEST_DIR").to_string()),
    )
    .join(relative)
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

/// A fake release directory — `names` written to disk plus a
/// `sha256sums.txt` whose entries match those bytes exactly — what the CI
/// publisher emits and the recording curl serves by basename.
fn build_release(names: &[&str]) -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    for name in names {
        let body = match *name {
            BINARY_ASSET => BINARY_BODY,
            MOCK_ASSET => MOCK_BODY,
            // The version artifact's bytes are never executed, only hashed
            // and (by the manifest) covered.
            _ => "2.1.282\n",
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

/// [`build_release`] with one extra manifest entry naming an asset whose
/// bytes the release does not carry — the shape of a release whose
/// manifest lists more than its host serves.
fn build_release_with_dangling_manifest_entry(names: &[&str], dangling: &str) -> TempDir {
    let dir = build_release(names);
    let mut manifest = fs::read_to_string(dir.path().join(CHECKSUMS_ASSET)).unwrap();
    manifest.push_str(&format!("{}  {dangling}\n", "0".repeat(64)));
    fs::write(dir.path().join(CHECKSUMS_ASSET), manifest).unwrap();
    dir
}

/// Run install.sh the way no other suite does: with
/// `CLAUDE_PRINT_RELEASE_URL` *removed* from the child env, so the script
/// exercises its documented default, and with the recording `curl` first
/// on the child PATH serving `release`'s bytes. Returns the isolated HOME
/// (kept alive by the caller), the exit output, and every URL the script
/// requested, in order.
fn run_default_install(release: &TempDir) -> (TempDir, Output, Vec<String>) {
    let home = tempfile::tempdir().unwrap();
    let bin_dir = home.path().join("bin");
    fs::create_dir_all(&bin_dir).unwrap();

    // Satisfies install.sh's `command -v claude` preflight without a real
    // Claude Code install (the sibling suites' shape).
    let fake_claude = bin_dir.join("claude");
    fs::write(&fake_claude, "#!/bin/sh\nexit 0\n").unwrap();
    fs::set_permissions(&fake_claude, fs::Permissions::from_mode(0o755)).unwrap();

    // The recording curl, first on the child's PATH, so all three of
    // install.sh's fetches resolve to it.
    let curl_shim = bin_dir.join("curl");
    fs::write(&curl_shim, RECORDING_CURL).unwrap();
    fs::set_permissions(&curl_shim, fs::Permissions::from_mode(0o755)).unwrap();

    let log = home.path().join("requested-urls.log");
    let output = Command::new("sh")
        .arg(repo_path("install.sh"))
        .env("HOME", home.path())
        .env(
            "PATH",
            format!(
                "{}:{}",
                bin_dir.display(),
                std::env::var("PATH").unwrap_or_default()
            ),
        )
        // The whole point of this suite: the default, not the override —
        // and the un-toggled default, so the fixture leg runs.
        .env_remove("CLAUDE_PRINT_RELEASE_URL")
        .env_remove("SKIP_MOCK_CLAUDE")
        .env("CURL_LOG", &log)
        .env("CURL_RELEASE_DIR", release.path())
        .output()
        .unwrap();

    let requested = fs::read_to_string(&log)
        .unwrap_or_else(|e| panic!("the recording curl logged nothing ({e}) — it never ran"))
        .lines()
        .map(str::to_string)
        .collect();
    (home, output, requested)
}

fn installed(home: &Path, name: &str) -> String {
    fs::read_to_string(home.join(".local/bin").join(name)).unwrap()
}

/// Whitespace-normalized form of `doc`, so a needle survives README line
/// re-wrapping (the claims being pinned are wording, not wrapping).
fn normalized(doc: &str) -> String {
    doc.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The README's `## Install` section — from its heading up to the next
/// level-2 heading. Scoped like `tests/install_sh.rs`'s extractor (which
/// keeps the heading line itself, so the end-of-section search cannot
/// match the heading this slice starts at): a default-source sentence that
/// merely survives elsewhere in the README cannot satisfy these pins.
fn readme_install_section() -> String {
    let readme = repo_file("README.md");
    let start = readme
        .find("\n## Install\n")
        .unwrap_or_else(|| panic!("README.md must carry an `## Install` heading"))
        + 1; // keep the heading line itself in the slice
    let rest = &readme[start..];
    let end = rest.find("\n## ").unwrap_or(rest.len());
    rest[..end].to_string()
}

#[test]
fn default_source_installs_the_published_assets_from_github_releases() {
    let release = build_release(&[BINARY_ASSET, MOCK_ASSET, VERSION_ASSET]);
    let (home, output, requested) = run_default_install(&release);

    assert!(
        output.status.success(),
        "the default path must install end to end: stderr {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // The fetch list IS the contract: the manifest first (nothing can be
    // verified without it), then the arch-specific asset names, each
    // joined onto the default base exactly as a release host serves them.
    let base = default_base();
    assert_eq!(
        requested,
        vec![
            format!("{base}/{CHECKSUMS_ASSET}"),
            format!("{base}/{BINARY_ASSET}"),
            format!("{base}/{MOCK_ASSET}"),
        ],
        "with CLAUDE_PRINT_RELEASE_URL unset, install.sh must fetch \
         {CHECKSUMS_ASSET} first and then exactly the published asset \
         names from the default GitHub Releases base"
    );

    // And the default path really installs: both artifacts placed
    // verbatim under the redirected HOME.
    assert_eq!(installed(home.path(), BINARY_INSTALL_NAME), BINARY_BODY);
    assert_eq!(installed(home.path(), MOCK_INSTALL_NAME), MOCK_BODY);
}

#[test]
fn the_fetch_log_records_requests_not_successes() {
    // A release whose manifest lists an asset the host does not serve:
    // the fixture leg must die as a download failure (the fail-closed
    // rule for a *listed* asset — the unlisted shape is install_sh.rs's
    // skip case), and the log must still show the 404'd URL. A log of
    // only the successful fetches would be an instrument that cannot
    // observe the failure it exists to pin.
    let release =
        build_release_with_dangling_manifest_entry(&[BINARY_ASSET, VERSION_ASSET], MOCK_ASSET);
    let (home, output, requested) = run_default_install(&release);

    assert!(
        !output.status.success(),
        "a listed-but-unserved asset must abort the install"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(&format!(
            "{MOCK_ASSET} is listed in {CHECKSUMS_ASSET} but could not be downloaded"
        )),
        "the abort must be the listed-but-undownloadable error: {stderr}"
    );
    // Nothing from the failed leg was placed, and the requested URLs —
    // including the failed one — are all in the log, in order.
    let base = default_base();
    assert_eq!(
        requested,
        vec![
            format!("{base}/{CHECKSUMS_ASSET}"),
            format!("{base}/{BINARY_ASSET}"),
            format!("{base}/{MOCK_ASSET}"),
        ],
        "the log must record every request, including the one that 404'd"
    );
    assert!(
        !home
            .path()
            .join(".local/bin")
            .join(MOCK_INSTALL_NAME)
            .exists(),
        "the unservable fixture must not be placed"
    );
}

#[test]
fn default_resolution_is_tag_less_and_tracks_the_workflows_v_version_releases() {
    let release = build_release(&[BINARY_ASSET, MOCK_ASSET, VERSION_ASSET]);
    let (_home, _output, requested) = run_default_install(&release);

    // The default resolves `releases/latest/download` — a tag-less base
    // that floats to whatever release is latest. No fetched URL may carry
    // a tag segment: a version baked into the installer would freeze it
    // to one release and silently stop tracking the publisher.
    let base = default_base();
    for url in &requested {
        assert!(
            url.starts_with(&format!("{base}/")),
            "every default-path fetch must sit under the tag-less \
             {base:?} base: {url:?}"
        );
        assert!(
            !url.contains("releases/download/v"),
            "a default-path fetch carries a pinned tag segment: {url:?} — \
             the installer must float to the latest release, never a \
             baked one"
        );
    }

    // The same discipline, source-side: install.sh names no
    // `releases/download/<tag>` URL at all — the tag-less base is the
    // only release URL shape the installer carries.
    let installer = repo_file("install.sh");
    assert!(
        !installer.contains("releases/download/"),
        "install.sh hardcodes a tagged download URL — the installer must \
         resolve `releases/latest/download`, never a pinned tag"
    );

    // The workflow side of the coupling: every release the default can
    // resolve to is named `v${VERSION}`, with the version read from the
    // cloned tree's Cargo.toml (the runbook's §"Version verification" —
    // publication order and manifest coverage are
    // tests/release_runbook_docs.rs's territory, not repeated here).
    let template = repo_file("claude-print-ci-workflowtemplate.yml");
    assert!(
        template.contains("VERSION=$(grep -m1 '^version' Cargo.toml"),
        "the WorkflowTemplate must read the released version from the \
         cloned tree's Cargo.toml — the version the default URL's latest \
         release is tagged with"
    );
    assert!(
        template.contains("gh release create \"v${VERSION}\""),
        "the WorkflowTemplate must name every release v${{VERSION}} — the \
         tag-shaped name the default URL's latest-release resolution \
         floats to"
    );
}

#[test]
fn default_repo_slug_matches_the_workflow_publisher_and_the_canonical_repo() {
    let installer = repo_file("install.sh");
    let template = repo_file("claude-print-ci-workflowtemplate.yml");
    let readme = repo_file("README.md");

    // The slug is defined once in the installer, as `REPO="…"`.
    let defined = installer
        .lines()
        .find_map(|line| {
            line.trim()
                .strip_prefix("REPO=\"")
                .and_then(|rest| rest.strip_suffix('"'))
        })
        .unwrap_or_else(|| panic!("install.sh must define REPO=\"<owner/repo>\""));
    assert_eq!(
        defined, REPO_SLUG,
        "install.sh's REPO slug must stay {REPO_SLUG:?} — it is the repo \
         the default URL downloads from and the workflow publishes to"
    );

    // It is the repo the workflow publishes to: every `--repo` flag on
    // the template's gh calls names the same slug, so the default URL and
    // the publisher can never name two different repos.
    let repo_flags = template.match_indices("--repo ").count();
    let pinned_flags = template
        .match_indices(&format!("--repo {REPO_SLUG}"))
        .count();
    assert!(
        repo_flags > 0 && repo_flags == pinned_flags,
        "every --repo flag in the WorkflowTemplate must name {REPO_SLUG:?} \
         (found {pinned_flags} of {repo_flags}) — the default release URL \
         and the publisher must name the same repo"
    );

    // And the repo it clones from — Forgejo is canonical, so the slug
    // must resolve to the same project on the source-of-truth host (the
    // GitHub mirror is never a clone source).
    assert!(
        template.contains(FORGEJO_CLONE_URL),
        "the WorkflowTemplate must clone from the canonical {FORGEJO_CLONE_URL:?} \
         — the same {REPO_SLUG:?} project the default URL downloads"
    );

    // The README documents the same slug as the distribution channel.
    assert!(
        readme.contains(&format!("GitHub Releases (`{REPO_SLUG}`)")),
        "the README must name GitHub Releases (`{REPO_SLUG}`) as the \
         distribution channel — the repo the default URL resolves to"
    );
}

#[test]
fn readme_documents_the_default_source_and_mirror_redirect_against_the_installer() {
    let section = normalized(&readme_install_section());
    let installer = repo_file("install.sh");

    // The channel claim, verbatim — the sentence that makes the default
    // URL a *GitHub* URL and the override a mirror redirect rather than a
    // second first-class source.
    assert!(
        section.contains(
            "Release artifacts are published only to GitHub Releases \
             — Forgejo hosts no release assets — so the default download \
             URL is a GitHub URL"
        ),
        "the README Install section must state that GitHub Releases is the \
         only artifact host, making the default URL a GitHub URL"
    );

    // The documented redirect: the override is spelled as a base URL over
    // the *same* assets, so a mirror redistributes but cannot bypass
    // verification — the property the fail-closed suites pin behaviorally.
    assert!(
        section.contains(
            "set `CLAUDE_PRINT_RELEASE_URL` to a base URL serving that \
             release's assets"
        ),
        "the README Install section must document CLAUDE_PRINT_RELEASE_URL \
         as a base-URL redirect serving the same assets"
    );
    assert!(
        section.contains("a mirror can redistribute the artifacts but cannot bypass verification"),
        "the README Install section must state that a mirror cannot bypass \
         checksum verification"
    );

    // The section names the two assets the behavioral default run fetched
    // first and second — the same identifiers this suite pins.
    assert!(
        section.contains(&format!("`{CHECKSUMS_ASSET}`")),
        "the README Install section must name the manifest asset \
         `{CHECKSUMS_ASSET}`"
    );
    assert!(
        section.contains(&format!("`{BINARY_ASSET}`")),
        "the README Install section must name the published asset \
         `{BINARY_ASSET}`"
    );

    // The installer declares exactly the documented default: the tag-less
    // GitHub Releases base over `REPO`, overridable only through the
    // variable the README names. (That the script *runs* this default is
    // default_source_installs_the_published_assets_from_github_releases.)
    assert!(
        installer.contains(DECLARED_DEFAULT_LINE),
        "install.sh must declare the default as {DECLARED_DEFAULT_LINE} — \
         the shape the README's default-source sentence describes"
    );
    // The section must point readers at the suite enforcing the
    // supply-chain guarantees (tests/install_sh.rs) — and, since this
    // suite, at the default-source pin too.
    assert!(
        section.contains("`tests/install_sh.rs`"),
        "the README Install section must point at tests/install_sh.rs"
    );
    assert!(
        section.contains("`tests/install_sh_release_source.rs`"),
        "the README Install section must point at \
         tests/install_sh_release_source.rs, the default-source pin"
    );
}
