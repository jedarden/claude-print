//! Documentation-contract test for the README §Install opening narrative
//! (bead claudepr-741a9c85) — the documented installation workflow's front
//! door, pinned against `install.sh` and the `claude-print-ci`
//! WorkflowTemplate that makes each of its claims true.
//!
//! The section's first paragraph after the `sh install.sh` fence is the
//! one place a reader learns what an install *is*: it downloads a
//! pre-built static musl binary from GitHub Releases
//! (`jedarden/claude-print`) — the supported distribution channel — runs
//! `--check` to verify the setup, and registers the NEEDLE adapter by
//! copying `claude-print.yaml` into `~/.needle/agents/` when NEEDLE is
//! detected. The *behavior* behind every one of those claims is pinned,
//! but each pinning suite owns its own slice of the section and none of
//! them holds the narrative itself:
//!
//! - `tests/install_sh.rs` pins the supply-chain paragraph (the
//!   fail-closed checksum clauses, the `SKIP_MOCK_CLAUDE` opt-out) and
//!   the NEEDLE leg's note↔installer↔test triangle;
//! - `tests/install_sh_release_source.rs` pins the default-source and
//!   mirror-redirect paragraphs;
//! - `tests/platform_matrix_docs.rs` pins the Supported-platforms matrix
//!   section, and `tests/install_sh_arch.rs` its per-row behavior.
//!
//! So the opening narrative could rot silently in each of the ways this
//! suite exists to close: the `sh install.sh` fence could become any
//! other invocation shape; "static musl binary" could survive a CI
//! toolchain change that stopped producing one (or claim staticness for
//! a publisher that never built musl); "runs `--check`" could outlive an
//! installer whose smoke gate was removed; the NEEDLE parenthetical
//! (detection arms, checkout-beside-the-script source, mode 0644,
//! overwrite) could drift from the installer's leg or drop its link to
//! the authoritative note — all green under every existing test, because
//! those tests assert behavior and their own slices, never this
//! paragraph.
//!
//! Every check reads the narrative and judges it against the two
//! artifacts it describes — `install.sh`'s source and the
//! WorkflowTemplate's publication legs — never the reverse: the
//! workflow side re-derives the release toolchain set the way
//! `tests/platform_matrix_docs.rs` does (deliberately independently —
//! two readers deriving the same names from the template is the pin),
//! and the installer side pins the implementing fragments verbatim, with
//! the repo slug extracted from the narrative rather than hand-typed a
//! second time. Where a claim's *behavior* is already enforced by a
//! named test in `tests/install_sh.rs`, the clause is mapped to that
//! test (the `INSTALL_GUARANTEES` pattern), so a guarantee whose
//! enforcing test disappears degrades into documentation-only and
//! fails here.
//!
//! Every agreement leg is mutation-checked by always-on negative
//! meta-tests (the committed `tests/docs_build_commands.rs` pattern):
//! each README, installer, and WorkflowTemplate mutation the pin guards
//! against is applied in memory on every run and must fail the owning
//! check naming the drift.
//!
//! Library-level: reads README.md, `install.sh`, the WorkflowTemplate,
//! `tests/install_sh.rs`, and the needle-adapter note; spawns nothing.

use std::fs;
use std::path::{Path, PathBuf};

// ── repo-root resolution ────────────────────────────────────────────────────

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
/// claudepr-270570be; the same chain as `tests/install_sh.rs`). Candidates,
/// most authoritative first, each probe-verified before use:
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

/// Read a repo file from the checkout under test, resolving the root
/// through [`repo_root`]'s runtime-first candidate chain.
fn repo_file(relative: &str) -> String {
    fs::read_to_string(repo_root().join(relative))
        .unwrap_or_else(|e| panic!("read {relative} from the checkout under test: {e}"))
}

// ── README extraction ───────────────────────────────────────────────────────

/// The README's `## Install` section — from its heading up to the next
/// level-2 heading (`## Self-check`), subsections included. Scoped like
/// the sibling suites' extractors so a claim that merely survives
/// elsewhere in the README cannot satisfy these pins.
fn install_section(readme: &str) -> &str {
    let start = readme
        .find("\n## Install\n")
        .unwrap_or_else(|| panic!("README.md must carry an `## Install` heading"))
        + 1; // keep the heading line itself in the slice
    let rest = &readme[start..];
    // `\n## ` (with the trailing space) matches only level-2 headings,
    // not the `###` subsections inside Install.
    let end = rest.find("\n## ").map(|i| i + 1).unwrap_or(rest.len());
    &rest[..end]
}

/// The section's opening narrative — the paragraph starting
/// "`install.sh` downloads", up to the paragraph break — whitespace-
/// normalized so a rewrap still matches. This is the slice the bead
/// names: the one-paragraph statement of what an install does.
fn opening_narrative(section: &str) -> String {
    let start = section.find("`install.sh` downloads").unwrap_or_else(|| {
        panic!("the Install section must open its narrative with `install.sh` downloads")
    });
    let rest = &section[start..];
    let end = rest.find("\n\n").unwrap_or(rest.len());
    normalized(&rest[..end])
}

/// Read the README and return the opening Install narrative as owned text so
/// callers can pass it through the pure drift checks without borrowing from a
/// temporary string returned by [`repo_file`].
fn install_narrative() -> String {
    let readme = repo_file("README.md");
    opening_narrative(install_section(&readme))
}

/// The doc with every whitespace run collapsed to one space, so prose
/// assertions survive line re-wrapping (the claims being pinned are
/// sentences, not formatting).
fn normalized(doc: &str) -> String {
    doc.split_whitespace().collect::<Vec<_>>().join(" ")
}

// ── install.sh and WorkflowTemplate extraction ──────────────────────────────

/// The repo slug `install.sh` names in its `REPO="…"` definition — the
/// one place the installer states which GitHub project it downloads from.
fn installer_repo(installer: &str) -> String {
    installer
        .lines()
        .find_map(|line| {
            line.trim()
                .strip_prefix("REPO=\"")
                .and_then(|rest| rest.strip_suffix('"'))
        })
        .unwrap_or_else(|| panic!("install.sh must define REPO=\"<owner/repo>\""))
        .to_string()
}

/// The repo slug the narrative names in "GitHub Releases (`…`)" —
/// extracted, never hand-typed here, so the byte-level pins below compare
/// README text directly against the installer's definition with no third
/// copy of the slug to fork.
fn narrative_repo(narrative: &str) -> String {
    let lead = "GitHub Releases (`";
    let start = narrative
        .find(lead)
        .map(|i| i + lead.len())
        .unwrap_or_else(|| {
            panic!("the Install narrative must name the release source as GitHub Releases (`<owner/repo>`)")
        });
    let end = narrative[start..]
        .find('`')
        .unwrap_or_else(|| panic!("the narrative's GitHub Releases (`…`) span is never closed"));
    narrative[start..start + end].to_string()
}

/// Every `rustup target add <triple>` in the WorkflowTemplate — the set of
/// targets a release build can actually have linked std for (the same
/// derivation as `tests/platform_matrix_docs.rs`, deliberately
/// independent: a shared helper could drift with the template and leave
/// both docs stale).
fn rustup_target_adds(template: &str) -> Vec<String> {
    template
        .lines()
        .filter_map(|line| line.trim().strip_prefix("rustup target add "))
        .map(str::to_string)
        .collect()
}

/// 1-based-ish byte position helper: `None` when the fragment is absent so
/// callers can name the missing wiring in their panic message.
fn position_of(haystack: &str, fragment: &str) -> Option<usize> {
    haystack.find(fragment)
}

/// The suffix CI's build-target derivation and this suite's asset
/// derivation both strip: `<arch>-unknown-linux-musl` → `<arch>`.
const MUSL_TARGET_SUFFIX: &str = "-unknown-linux-musl";

/// The suffix the installer's arch→asset mapping appends to the runner
/// arch: `<arch>-linux`, the asset name segment a release carries.
const LINUX_ASSET_SUFFIX: &str = "-linux";

/// The note the narrative's NEEDLE parenthetical defers to — read through
/// the same root-relative path the README links, so the driver's read
/// doubles as the link-resolution half of that pin.
const NEEDLE_ADAPTER_NOTE: &str = "docs/notes/installer-needle-adapter.md";

// ── the checks (pure over their inputs, for the negative meta-tests) ────────

/// The section's opening fence is the sanctioned invocation: exactly
/// `sh install.sh`, the shape the adapter note's "source is the checkout"
/// reasoning and every behavioral suite's run line assume.
fn check_invocation_fence(section: &str) {
    assert!(
        section.contains("```bash\nsh install.sh\n```"),
        "the README Install section must open with the sanctioned invocation fence \
         ```bash\nsh install.sh\n``` — any other shape is not the documented workflow"
    );
}

/// The narrative's release-source claims, judged against the installer's
/// own definition and the workflow's publication leg:
///
/// - the narrative names GitHub Releases (`<slug>`) as where the binary
///   comes from, and that slug must be exactly `install.sh`'s `REPO`
///   (extracted from the narrative — no third copy);
/// - "the supported distribution channel for release artifacts" — the
///   sentence that makes GitHub Releases a channel, not just a host;
/// - the WorkflowTemplate must still publish releases there
///   (`gh release create` carrying `--repo <slug>`), or the channel the
///   narrative names would be a claim about a publisher that no longer
///   exists. The deep slug↔workflow coupling (every `--repo` flag, the
///   Forgejo clone URL) is `tests/install_sh_release_source.rs`'s.
fn check_release_source_narrative(narrative: &str, installer: &str, template_norm: &str) {
    let slug = narrative_repo(narrative);
    assert!(
        !slug.contains(' '),
        "the narrative's GitHub Releases slug must be a bare <owner/repo>: {slug:?}"
    );
    assert!(
        narrative.contains(&format!("from GitHub Releases (`{slug}`)")),
        "the Install narrative must state the binary comes from GitHub Releases \
         (`{slug}`) — the release source"
    );
    assert!(
        narrative.contains("the supported distribution channel for release artifacts"),
        "the Install narrative must keep calling GitHub Releases the supported \
         distribution channel for release artifacts"
    );
    let defined = installer_repo(installer);
    assert_eq!(
        defined, slug,
        "install.sh's REPO slug must equal the repo the Install narrative names — \
         the narrative and the installer cannot point at two different release sources"
    );
    assert!(
        template_norm.contains("gh release create \"v${VERSION}\""),
        "the WorkflowTemplate must keep `gh release create \"v${{VERSION}}\"` — the \
         channel the Install narrative names as the supported distribution channel"
    );
    assert!(
        template_norm.contains(&format!("--repo {slug}")),
        "the WorkflowTemplate must publish to {slug:?} — the same repo the Install \
         narrative names as the release source"
    );
}

/// The narrative's static-artifact claim, judged against the publisher
/// that makes it true and the installer that selects the artifact:
///
/// - the narrative says the download is a "pre-built static musl binary";
/// - the WorkflowTemplate installs exactly one toolchain, derives its
///   build target with the matching musl suffix, and builds the binary
///   for that target as a STATIC musl binary — the producer of the
///   artifact the narrative describes;
/// - `install.sh`'s supported arch arm maps that same derived arch to
///   the `<arch>-linux` asset suffix and requests
///   `claude-print-${TARGET}` — the selection of the static artifact the
///   narrative promises. (The full matrix is
///   `tests/platform_matrix_docs.rs`'s; this leg pins the Install
///   sentence to the same derivation.)
fn check_static_musl_selection(narrative: &str, installer: &str, template: &str) {
    assert!(
        narrative.contains("downloads a pre-built static musl binary"),
        "the Install narrative must keep describing the download as a pre-built \
         static musl binary — the artifact selection the workflow and installer pins \
         below stand behind"
    );
    let targets = rustup_target_adds(template);
    assert_eq!(
        targets.len(),
        1,
        "the WorkflowTemplate must install exactly one release toolchain (found \
         {:?}) — the static musl artifact the Install narrative describes is \
         produced by a single-target publisher",
        targets
    );
    let only = &targets[0];
    let arch = only
        .strip_suffix(MUSL_TARGET_SUFFIX)
        .unwrap_or_else(|| panic!("the toolchain {only} lost its musl suffix"));
    assert!(
        template.contains(&format!("MUSL_TARGET=\"${{ARCH}}{MUSL_TARGET_SUFFIX}\"")),
        "the WorkflowTemplate must derive its build target from the runner arch \
         with the {MUSL_TARGET_SUFFIX:?} suffix — the wiring that makes the \
         published artifact a musl binary"
    );
    assert!(
        template.contains("cargo build --release --target \"${MUSL_TARGET}\" --bin claude-print"),
        "the WorkflowTemplate must build claude-print for the derived musl target — \
         the static binary the Install narrative says is downloaded"
    );
    assert!(
        normalized(template).contains("STATIC musl binary"),
        "the WorkflowTemplate's build step must keep stating it builds a STATIC \
         musl binary — the producer-side claim behind the narrative's \"static \
         musl binary\""
    );
    let arm = format!("Linux-{arch}) TARGET=\"{arch}{LINUX_ASSET_SUFFIX}\"");
    assert!(
        installer.contains(&arm),
        "install.sh's supported-platform arm must stay `{arm}` — the mapping that \
         selects the {arch} musl asset the narrative's \"static musl binary\" names"
    );
    assert!(
        installer.contains("BINARY_ASSET=\"claude-print-${TARGET}\""),
        "install.sh must request the binary as BINARY_ASSET=\"claude-print-${{TARGET}}\" \
         — the static musl artifact whose selection the narrative describes"
    );
}

/// The narrative's `--check` claim, judged against the installer's smoke
/// gate and mapped to the behavioral test that enforces it:
///
/// - the narrative says the install "runs `--check` to verify the setup";
/// - `install.sh` really gates on it: the *installed* binary is invoked
///   with `--check` under `if !`, a nonzero exit prints the documented
///   error and fails the install, and the gate sits before the
///   `Installation complete.` banner — so no run that skipped the smoke
///   can claim completion;
/// - the failure shape is enforced behaviorally by
///   `a_failed_post_install_check_aborts_the_install_with_the_new_binary_live_and_prev_intact`
///   in `tests/install_sh.rs` — the clause is pinned to that test, so the
///   guarantee cannot degrade into documentation-only.
fn check_check_smoke(narrative: &str, installer: &str, install_sh_tests: &str) {
    assert!(
        narrative.contains("runs `--check` to verify the setup"),
        "the Install narrative must keep stating the installer runs `--check` to \
         verify the setup — the post-install smoke the gate pins below implement"
    );
    let gate = "if ! \"${INSTALL_DIR}/claude-print\" --check; then";
    assert!(
        installer.contains(gate),
        "install.sh must gate the install on the installed binary's `--check` — \
         {gate:?} is the leg the narrative's \"runs `--check`\" describes"
    );
    assert!(
        installer.contains("echo \"Error: claude-print --check failed\" >&2"),
        "install.sh must report a failed `--check` smoke with the documented error \
         line — the failure the narrative's claim implies"
    );
    let smoke = position_of(installer, gate);
    let banner = position_of(installer, "Installation complete.");
    assert!(
        matches!((smoke, banner), (Some(s), Some(b)) if s < b),
        "install.sh's `--check` smoke must precede the `Installation complete.` \
         banner (smoke at {smoke:?}, banner at {banner:?}) — a completed install \
         is one whose check ran"
    );
    let enforcing =
        "a_failed_post_install_check_aborts_the_install_with_the_new_binary_live_and_prev_intact";
    assert!(
        install_sh_tests.contains(&format!("fn {enforcing}()")),
        "the narrative's `--check` clause is pinned to `{enforcing}`, which no \
         longer exists in tests/install_sh.rs — restore the test or re-point the pin"
    );
}

/// One row of the narrative's NEEDLE parenthetical: (documented clause,
/// `install.sh` fragment implementing it, enforcing test in
/// `tests/install_sh.rs`). The clause is matched verbatim in the
/// narrative, the fragment verbatim in the installer's source, and the
/// test under exactly this name — the same three-way shape as
/// `INSTALL_GUARANTEES` and the adapter note's own pin (whose
/// note↔installer↔test triangle stays `tests/install_sh.rs`'s; this table
/// holds the README sentence to the same standard).
const NEEDLE_NARRATIVE_ROWS: &[(&str, &str, &str)] = &[
    (
        "copies `claude-print.yaml` to `~/.needle/agents/` if NEEDLE is present",
        "NEEDLE_AGENTS_DIR=\"${HOME}/.needle/agents\"",
        "needle_on_the_path_installs_the_repo_adapter_template_into_the_agents_dir",
    ),
    (
        "detected via `needle` on `PATH` or an existing `~/.needle/agents/`",
        "command -v needle >/dev/null 2>&1 || [ -d \"${NEEDLE_AGENTS_DIR}\" ]",
        "an_existing_agents_dir_alone_triggers_the_adapter_leg",
    ),
    (
        "the template is copied from the checkout beside the script",
        "SCRIPT_DIR=\"$(cd \"$(dirname \"$0\")\" && pwd)\"",
        "no_adapter_beside_the_script_skips_the_needle_leg_with_a_note",
    ),
    (
        "installed mode 0644, overwriting any existing copy",
        "install -m 644 \"${SCRIPT_DIR}/claude-print.yaml\" \"${NEEDLE_AGENTS_DIR}/claude-print.yaml\"",
        "an_existing_adapter_is_overwritten_in_place_at_0644_with_no_backup_copy",
    ),
];

/// The narrative's NEEDLE-adapter claims: every row of
/// [`NEEDLE_NARRATIVE_ROWS`] must survive verbatim in the narrative, its
/// installer fragment verbatim in `install.sh`, and its enforcing test in
/// `tests/install_sh.rs`. The parenthetical must also defer to the
/// authoritative note by link, and the note (read through that same path
/// by the driver) must still claim the authority the deferral promises.
fn check_needle_adapter_semantics(narrative: &str, installer: &str, install_sh_tests: &str) {
    for (clause, fragment, enforcing) in NEEDLE_NARRATIVE_ROWS {
        assert!(
            narrative.contains(clause),
            "the Install narrative must keep stating the NEEDLE-adapter clause \
             verbatim: {clause:?} — update README and installer together"
        );
        assert!(
            installer.contains(fragment),
            "the narrative's clause {clause:?} is pinned to the installer fragment \
             {fragment:?}, which install.sh no longer carries — the documented \
             NEEDLE semantics and the installer have forked"
        );
        assert!(
            install_sh_tests.contains(&format!("fn {enforcing}()")),
            "the narrative's clause {clause:?} is pinned to `{enforcing}`, which \
             no longer exists in tests/install_sh.rs — restore the test or \
             re-point the pin"
        );
    }
    let link = format!("[`{NEEDLE_ADAPTER_NOTE}`]({NEEDLE_ADAPTER_NOTE})");
    assert!(
        narrative.contains(&link),
        "the Install narrative's NEEDLE parenthetical must keep deferring to the \
         authoritative note via {link} — \"full semantics\" needs a target"
    );
}

/// The note's authority claim — the deferral target of the narrative's
/// "full semantics in …" pointer must still present itself as the
/// authoritative statement of the leg's semantics, or the README is
/// deferring to a note that no longer claims the role.
fn check_note_claims_authority(note_norm: &str) {
    assert!(
        note_norm.contains("this note is the authoritative statement"),
        "the needle-adapter note must keep claiming to be the authoritative \
         statement of the NEEDLE leg's semantics — the README Install narrative \
         defers to it as \"full semantics\""
    );
}

/// The workflow's own statement of the checksum contract, judged against
/// the installer's policy — the release-workflow half of the Install
/// section's verification story (the README clauses themselves are
/// `tests/install_sh.rs`'s `INSTALL_GUARANTEES`):
///
/// - the WorkflowTemplate's release notes tell every release-page reader
///   what `sha256sums.txt` is for: "install.sh verifies against this
///   before installing";
/// - `install.sh` states and implements exactly that policy: its header
///   declares every artifact verified "before it is installed or
///   executed", it defines the manifest asset the notes name, and the
///   binary's `verify_artifact` call sits before its `install` placement
///   in the script's own order — the "before installing" half,
///   source-side.
fn check_workflow_manifest_statement(installer: &str, template_norm: &str) {
    assert!(
        template_norm.contains(
            "sha256 checksums of every asset; install.sh verifies against this before installing"
        ),
        "the WorkflowTemplate's release notes must keep stating that install.sh \
         verifies against sha256sums.txt before installing — the publisher's half \
         of the verification contract the Install section documents"
    );
    assert!(
        installer.contains("before it is installed or executed"),
        "install.sh's header must keep declaring artifacts verified \"before it \
         is installed or executed\" — the policy the workflow's release notes \
         attribute to the installer"
    );
    assert!(
        installer.contains("CHECKSUMS_ASSET=\"sha256sums.txt\""),
        "install.sh must define CHECKSUMS_ASSET=\"sha256sums.txt\" — the manifest \
         asset the workflow's release notes describe"
    );
    let verify = position_of(installer, "verify_artifact \"${TMP_BIN}\"");
    let place = position_of(installer, "install -m 755 \"${TMP_BIN}\"");
    assert!(
        matches!((verify, place), (Some(v), Some(p)) if v < p),
        "install.sh must verify_artifact the binary before install places it \
         (verify at {verify:?}, place at {place:?}) — the \"before installing\" \
         order both the header and the release notes state"
    );
}

// ── the pins ────────────────────────────────────────────────────────────────

#[test]
fn install_section_opens_with_the_sanctioned_invocation() {
    let readme = repo_file("README.md");
    let section = install_section(&readme);
    assert!(
        !section.is_empty(),
        "README.md must carry a non-empty `## Install` section"
    );
    check_invocation_fence(section);
}

#[test]
fn install_narrative_names_the_release_source_the_installer_defaults_to() {
    check_release_source_narrative(
        &install_narrative(),
        &repo_file("install.sh"),
        &normalized(&repo_file("claude-print-ci-workflowtemplate.yml")),
    );
}

#[test]
fn install_narrative_static_claim_matches_the_publisher_and_the_installer_selection() {
    check_static_musl_selection(
        &install_narrative(),
        &repo_file("install.sh"),
        &repo_file("claude-print-ci-workflowtemplate.yml"),
    );
}

#[test]
fn install_narrative_check_claim_matches_the_installer_smoke_gate() {
    check_check_smoke(
        &install_narrative(),
        &repo_file("install.sh"),
        &repo_file("tests/install_sh.rs"),
    );
}

#[test]
fn install_narrative_needle_clauses_match_the_installer_leg_and_the_note() {
    // Read through the same root-relative path the narrative links (see
    // NEEDLE_ADAPTER_NOTE): this line is the link-resolution half of the
    // deferral pin — a renamed or deleted note fails the read before any
    // assertion can fire.
    let note = repo_file(NEEDLE_ADAPTER_NOTE);
    check_needle_adapter_semantics(
        &install_narrative(),
        &repo_file("install.sh"),
        &repo_file("tests/install_sh.rs"),
    );
    check_note_claims_authority(&normalized(&note));
}

#[test]
fn workflow_release_notes_manifest_claim_matches_the_installer_policy() {
    check_workflow_manifest_statement(
        &repo_file("install.sh"),
        &normalized(&repo_file("claude-print-ci-workflowtemplate.yml")),
    );
}

// ── negative meta-tests: the pins must FAIL when their inputs rot ───────────
//
// The committed non-vacuity pattern of `tests/docs_build_commands.rs`:
// every leg mutates the live document in memory — nothing is written to
// disk — and requires the owning check to panic naming the drift. This is
// the bead's mutation-check acceptance, running on every invocation rather
// than as a one-off edit.

/// Run `check` and require it to panic with every fragment of `expected` in
/// the message — the failure must be the planted drift, not an incidental
/// one. The panic hook is silenced for the caught unwind so expected
/// failures never pollute the log; it is restored before any real assertion
/// here can fire.
fn assert_drift<F>(check: F, expected: &[&str])
where
    F: FnOnce() + std::panic::UnwindSafe,
{
    let prev_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let outcome = std::panic::catch_unwind(check);
    std::panic::set_hook(prev_hook);
    let message = match outcome {
        Ok(()) => panic!(
            "the mutated input PASSED the check — the drift guard is vacuous \
             for this mutation"
        ),
        Err(payload) => payload
            .downcast_ref::<&str>()
            .map(|s| (*s).to_string())
            .or_else(|| payload.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "<non-string panic payload>".to_string()),
    };
    for fragment in expected {
        assert!(
            message.contains(fragment),
            "the check failed, but not for the planted drift — panic message:\n\
             {message}\nmissing fragment: {fragment:?}"
        );
    }
}

/// `text` with every occurrence of `from` replaced by `to`. Panics when
/// `from` is absent, so a meta-test can never "mutate" an input the live
/// document no longer carries and silently test something else. Every
/// occurrence, not just one: a clause stated on more than one surface is
/// only gone when it is gone everywhere.
fn replaced_all(text: &str, from: &str, to: &str) -> String {
    assert!(
        text.contains(from),
        "the negative meta-tests mutate {from:?} in the live document — it is gone"
    );
    text.replace(from, to)
}

/// Changing the sanctioned invocation fence — or letting the section open
/// with any other shape — fails the fence check naming `sh install.sh`.
#[test]
fn negative_meta_changed_invocation_fence_fails() {
    let section = install_section(&repo_file("README.md")).to_string();
    assert_drift(
        || {
            check_invocation_fence(&replaced_all(
                &section,
                "```bash\nsh install.sh\n```",
                "```bash\nbash install.sh\n```",
            ))
        },
        &["sh install.sh"],
    );
}

/// Dropping the channel clause, removing the slug from the narrative,
/// repointing the installer's `REPO`, or unpublishing releases from the
/// workflow — each fails the release-source check naming the drift.
#[test]
fn negative_meta_dropped_release_source_claim_fails() {
    let narrative = install_narrative();
    let installer = repo_file("install.sh");
    let template_norm = normalized(&repo_file("claude-print-ci-workflowtemplate.yml"));

    assert_drift(
        || {
            check_release_source_narrative(
                &replaced_all(
                    &narrative,
                    "the supported distribution channel for release artifacts",
                    "a distribution channel",
                ),
                &installer,
                &template_norm,
            )
        },
        &["supported distribution channel"],
    );
    assert_drift(
        || {
            check_release_source_narrative(
                &replaced_all(
                    &narrative,
                    "GitHub Releases (`jedarden/claude-print`)",
                    "GitHub Releases",
                ),
                &installer,
                &template_norm,
            )
        },
        &["GitHub Releases (`<owner/repo>`)"],
    );
    assert_drift(
        || {
            check_release_source_narrative(
                &narrative,
                &replaced_all(
                    &installer,
                    "REPO=\"jedarden/claude-print\"",
                    "REPO=\"example/other\"",
                ),
                &template_norm,
            )
        },
        &["two different release sources"],
    );
    assert_drift(
        || {
            check_release_source_narrative(
                &narrative,
                &installer,
                &replaced_all(&template_norm, "gh release create \"v${VERSION}\"", "true"),
            )
        },
        &["gh release create"],
    );
}

/// Rewording "static musl" away, widening the workflow's toolchain set,
/// breaking the musl build wiring, or renaming the installer's arch arm —
/// each fails the static-selection check naming the drift.
#[test]
fn negative_meta_reworded_static_musl_claim_fails() {
    let narrative = install_narrative();
    let installer = repo_file("install.sh");
    let template = repo_file("claude-print-ci-workflowtemplate.yml");

    assert_drift(
        || {
            check_static_musl_selection(
                &replaced_all(
                    &narrative,
                    "pre-built static musl binary",
                    "pre-built binary",
                ),
                &installer,
                &template,
            )
        },
        &["pre-built static musl binary"],
    );
    assert_drift(
        || {
            check_static_musl_selection(
                &narrative,
                &installer,
                &replaced_all(
                    &template,
                    "rustup target add x86_64-unknown-linux-musl",
                    // The newline makes the widened set two real
                    // `rustup target add` lines — the shape the
                    // derivation scans for.
                    "rustup target add x86_64-unknown-linux-musl\n            \
                     rustup target add aarch64-unknown-linux-musl",
                ),
            )
        },
        &["exactly one release toolchain"],
    );
    assert_drift(
        || {
            check_static_musl_selection(
                &narrative,
                &installer,
                &replaced_all(
                    &template,
                    "cargo build --release --target \"${MUSL_TARGET}\" --bin claude-print",
                    "cargo build --release --bin claude-print",
                ),
            )
        },
        &["musl target"],
    );
    assert_drift(
        || {
            check_static_musl_selection(
                &narrative,
                &replaced_all(
                    &installer,
                    "Linux-x86_64) TARGET=\"x86_64-linux\"",
                    "Linux-x86_64) TARGET=\"x86_64-gnu\"",
                ),
                &template,
            )
        },
        &["Linux-x86_64) TARGET=\"x86_64-linux\""],
    );
}

/// Dropping the `--check` sentence, removing the installer's smoke gate,
/// moving completion ahead of the check, or renaming the enforcing
/// behavioral test — each fails the smoke check naming the drift.
#[test]
fn negative_meta_dropped_check_smoke_claim_fails() {
    let narrative = install_narrative();
    let installer = repo_file("install.sh");
    let install_sh_tests = repo_file("tests/install_sh.rs");

    assert_drift(
        || {
            check_check_smoke(
                &replaced_all(
                    &narrative,
                    "runs `--check` to verify the setup",
                    "verifies the setup",
                ),
                &installer,
                &install_sh_tests,
            )
        },
        &["runs `--check`"],
    );
    assert_drift(
        || {
            check_check_smoke(
                &narrative,
                &replaced_all(
                    &installer,
                    "if ! \"${INSTALL_DIR}/claude-print\" --check; then",
                    "\"${INSTALL_DIR}/claude-print\" --check",
                ),
                &install_sh_tests,
            )
        },
        &["gate the install"],
    );
    assert_drift(
        || {
            check_check_smoke(
                &narrative,
                &replaced_all(
                    &installer,
                    "echo \"Installation complete.\"",
                    "echo \"Done.\"",
                ),
                &install_sh_tests,
            )
        },
        &["must precede"],
    );
    assert_drift(
        || {
            check_check_smoke(
                &narrative,
                &installer,
                &replaced_all(
                    &install_sh_tests,
                    "fn a_failed_post_install_check_aborts_the_install_with_the_new_binary_live_and_prev_intact()",
                    "fn renamed_smoke_test()",
                ),
            )
        },
        &["a_failed_post_install_check_aborts_the_install_with_the_new_binary_live_and_prev_intact"],
    );
}

/// Dropping any NEEDLE clause from the narrative, forking the installer's
/// leg from its documented fragment, renaming an enforcing test, or
/// unlinking the note — each fails the NEEDLE check naming the drift.
#[test]
fn negative_meta_dropped_needle_claim_fails() {
    let narrative = install_narrative();
    let installer = repo_file("install.sh");
    let install_sh_tests = repo_file("tests/install_sh.rs");

    for (clause, fragment, enforcing) in NEEDLE_NARRATIVE_ROWS {
        assert_drift(
            || {
                check_needle_adapter_semantics(
                    &replaced_all(&narrative, clause, "registers the adapter"),
                    &installer,
                    &install_sh_tests,
                )
            },
            &[clause],
        );
        assert_drift(
            || {
                check_needle_adapter_semantics(
                    &narrative,
                    &replaced_all(&installer, fragment, "true"),
                    &install_sh_tests,
                )
            },
            &[clause],
        );
        assert_drift(
            || {
                check_needle_adapter_semantics(
                    &narrative,
                    &installer,
                    &replaced_all(
                        &install_sh_tests,
                        &format!("fn {enforcing}()"),
                        "fn renamed_needle_test()",
                    ),
                )
            },
            &[enforcing],
        );
    }
    assert_drift(
        || {
            check_needle_adapter_semantics(
                &replaced_all(
                    &narrative,
                    &format!("[`{NEEDLE_ADAPTER_NOTE}`]({NEEDLE_ADAPTER_NOTE})"),
                    "",
                ),
                &installer,
                &install_sh_tests,
            )
        },
        &["full semantics"],
    );
}

/// The note dropping its authority claim fails the deferral pin.
#[test]
fn negative_meta_note_denied_authority_fails() {
    let note = normalized(&repo_file(NEEDLE_ADAPTER_NOTE));
    assert_drift(
        || {
            check_note_claims_authority(&replaced_all(
                &note,
                "this note is the authoritative statement",
                "this note is a summary",
            ))
        },
        &["authoritative statement"],
    );
}

/// Weakening the workflow's release-notes manifest claim, or forking the
/// installer's verify-before-place order or stated policy — each fails
/// the manifest-statement check naming the drift.
#[test]
fn negative_meta_diverged_workflow_manifest_statement_fails() {
    let installer = repo_file("install.sh");
    let template_norm = normalized(&repo_file("claude-print-ci-workflowtemplate.yml"));

    assert_drift(
        || {
            check_workflow_manifest_statement(
                &installer,
                &replaced_all(
                    &template_norm,
                    "install.sh verifies against this before installing",
                    "install.sh may verify against this",
                ),
            )
        },
        &["verifies against sha256sums.txt before installing"],
    );
    assert_drift(
        || {
            check_workflow_manifest_statement(
                &replaced_all(
                    &installer,
                    "before it is installed or executed",
                    "eventually",
                ),
                &template_norm,
            )
        },
        &["before it is installed or executed"],
    );
    assert_drift(
        || {
            check_workflow_manifest_statement(
                &replaced_all(
                    &installer,
                    "verify_artifact \"${TMP_BIN}\" \"${BINARY_ASSET}\" \"${TMP_CHECKSUMS}\"",
                    "true",
                ),
                &template_norm,
            )
        },
        &["verify_artifact"],
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
