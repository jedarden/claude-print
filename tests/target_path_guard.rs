//! Standing guard for the locator discipline in AGENTS.md §"Where the
//! build output lands" (bead claudepr-7a9e7130).
//!
//! The section's load-bearing sentence — tests locate `mock-claude` and
//! the crate binary "through cargo's own locators, never a written-out
//! path" — exists because one command has two output locations:
//! `./target/…` on a stock checkout and `/build/claude-print/…` on fleet
//! hosts, where `~/.local/bin/cargo` redirects `CARGO_TARGET_DIR`. A test
//! or script that writes `target/debug/…` therefore passes on a stock
//! checkout and fails only on fleet hosts — drift CI-on-a-fresh-checkout
//! can never catch. The HOME discipline has `tests/home_env_guard.rs`;
//! the doc claims have `tests/docs_build_layout.rs`, whose own
//! written-out-path lint covers `tests/**/*.rs` as one check among many.
//! This guard is the missing third piece: a code-discipline sweep of
//! every surface that resolves build output, failing the build on any
//! new hardcode.
//!
//! Scope: every regular file under `tests/` and `scripts/` (recursively,
//! any extension — fixtures included), plus `build.rs` at the repo root
//! (absent today; covered from the day one appears). Deliberately out of
//! scope: `src/` (production code does not resolve test artifacts),
//! `install.sh` and the CI WorkflowTemplates (release-tarball surfaces
//! pinned by the installer and platform-matrix suites), and
//! `test-fixtures/mock-claude/` (the fixture member, pinned by the
//! docs_build_layout member checks).
//!
//! Needles — the written-out forms of both columns of the artifact table:
//! `target/debug`, `target/release`, `target/<triple>` with the musl
//! triple derived from the CI WorkflowTemplate's `rustup target add` set
//! (so a toolchain widening keeps the needle live instead of silently
//! rotting it — `tests/docs_build_layout.rs` fails CI on that widening
//! until the docs and this needle move together), and `/build/` —
//! deliberately broader than today's `/build/claude-print` redirect, so
//! the legacy `/build/target-workers` spelling and any future
//! `/build/<name>` redirect are caught by the same needle.
//!
//! Comments are skipped by each file's own syntax: `//` (including `///`
//! and `//!`) for Rust, `#` for shell, Python, systemd units, and
//! markdown headings. A Python docstring is not a `#` comment — a
//! docstring quoting a redirect path as documentation takes an allowlist
//! entry like any other prose quote.
//!
//! Allowlist — two tiers, each entry carrying its rationale so the
//! exception and its justification land in the same commit:
//!
//! - `FILE_EXEMPT`, whole files: the drift guards whose own needle lists
//!   and in-memory meta-test mutations carry these literals as
//!   assertions by construction (this file and `docs_build_layout`).
//! - `LINE_EXEMPT`, exact trimmed-line matches: documentation and
//!   assertion quotes — a docstring teaching the very discipline, a
//!   redaction self-check whose input is the path it proves redacted.
//!   Anchored on line *content*, not line number: reordering stays
//!   valid, and any edit to an exempted line forces a deliberate
//!   allowlist update.
//!
//! Allowlist hygiene is itself enforced: an entry naming no scanned
//! file, matching no line (or several), covering a needle-free line, or
//! sitting inside a file exemption fails the guard — the allowlist
//! cannot rot into noise. The guard's failure behavior is pinned by
//! always-on negative meta-tests (the claudepr-4d967120 pattern):
//! planted hardcodes under `tests/`, `scripts/`, and `build.rs` must be
//! reported with file, line, and needle, commented mentions must not be,
//! and dropping a live line exemption must re-flag the line it covers —
//! proving the allowlist load-bearing, not decorative.
//!
//! Library-level like the rows it guards: reads the scanned tree and the
//! CI WorkflowTemplate; spawns nothing.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

/// The AGENTS.md section whose locator discipline this guard enforces —
/// also the heading its self-naming check scopes to.
const OUTPUT_LANDS_HEADING: &str = "### Where the build output lands";

/// The CI WorkflowTemplate the musl needle is derived from — the same
/// file `tests/docs_build_layout.rs` derives the artifact table's toolchain
/// from, so the two guards can never disagree about the triple.
const CI_TEMPLATE: &str = "claude-print-ci-workflowtemplate.yml";

/// Whole-file exemptions: drift guards whose non-comment code carries the
/// needle literals as data. Adding an entry here is legitimate only for
/// a file that *asserts on* these strings; anything that constructs a
/// path from them belongs on no allowlist at all.
const FILE_EXEMPT: &[(&str, &str)] = &[
    // The sibling doc-drift guard: its HARDCODED_PATH_NEEDLES array, its
    // stock-cell parse anchor, and its negative meta-test mutations are
    // assertions about these literals, not paths.
    (
        "tests/docs_build_layout.rs",
        "doc-drift guard whose needle list and meta-test mutations assert on the literals",
    ),
    // This guard's own needle list.
    (
        "tests/target_path_guard.rs",
        "this guard's own needle list and meta-test probes",
    ),
];

/// Exact-line exemptions: documentation and assertion quotes. Each entry
/// is `(repo-relative path, trimmed line content, rationale)`; the line
/// must keep matching exactly one scanned line that contains a needle,
/// or hygiene fails — an edited quote forces the entry to move with it.
const LINE_EXEMPT: &[(&str, &str, &str)] = &[
    (
        "tests/benchmark_reproducibility.rs",
        r#"for fragment in ["/build/", "target-workers"] {"#,
        "asserts the recorded schema-2 evidence redacts the legacy fleet \
         redirect — the string is data under test, not a path the test builds",
    ),
    (
        "scripts/bench_startup_overhead.py",
        "(e.g. /build/claude-print), on a stock checkout it is ./target, and",
        "module-docstring quote teaching the very discipline this guard \
         enforces — the script's code resolves the bin dir via cargo metadata",
    ),
    (
        "scripts/bench_startup_overhead.py",
        r#""/build/target-workers/release", "--samples", "10"]"#,
        "input to the redact_bin_dir self-check: the legacy redirect is the \
         data being proven redacted, not a path the script resolves",
    ),
];

/// One written-out build-output path the scan found.
#[derive(Debug, PartialEq)]
struct Violation {
    path: String,
    line_no: usize,
    needle: String,
    line: String,
}

/// Repository root this guard reads, resolved at *runtime* — never the
/// bare compile-time `env!("CARGO_MANIFEST_DIR")`, which bakes the
/// building checkout's path into the test binary; when the shared target
/// cache reuses that binary from a different extraction, a baked-only
/// root fails every later run with file-NotFound panics that have
/// nothing to do with drift (bead claudepr-23f81f16). The same candidate
/// chain as `tests/docs_test_classification.rs`: an explicit override,
/// then the runtime `CARGO_MANIFEST_DIR` cargo sets in the test process,
/// then the compile-time value — each probe-verified before use, and a
/// loud panic naming everything that was tried if none survives.
fn repo_root() -> PathBuf {
    let candidates: [Option<String>; 3] = [
        std::env::var("CLAUDE_PRINT_TEST_REPO").ok(),
        std::env::var("CARGO_MANIFEST_DIR").ok(),
        Some(env!("CARGO_MANIFEST_DIR").to_string()),
    ];
    let mut rejected = Vec::new();
    for candidate in candidates.into_iter().flatten() {
        let path = Path::new(&candidate);
        if ROOT_PROBES.iter().all(|probe| path.join(probe).is_file()) {
            return path.to_path_buf();
        }
        rejected.push(candidate);
    }
    panic!(
        "no candidate repo root is a claude-print checkout (probe: {:?}): \
         {rejected:?} — run via `cargo test` from a checkout, or set \
         $CLAUDE_PRINT_TEST_REPO to one",
        ROOT_PROBES
    );
}

/// Probe files that identify a claude-print checkout: a directory holding
/// both is a usable repo root for this guard.
const ROOT_PROBES: [&str; 2] = ["AGENTS.md", "Cargo.toml"];

fn read_repo(rel: &str) -> String {
    let path = repo_root().join(rel);
    let bytes = fs::read(&path).unwrap_or_else(|e| panic!("reading {rel}: {e}"));
    String::from_utf8_lossy(&bytes).into_owned()
}

/// The one musl toolchain the CI WorkflowTemplate installs — the same
/// derivation `tests/docs_build_layout.rs` uses, so the artifact table
/// and this guard's needle share one source of truth. A widening (more
/// than one `rustup target add`) panics here and in the sibling until
/// the docs and the needles move together.
fn ci_musl_triple() -> String {
    let adds: Vec<String> = read_repo(CI_TEMPLATE)
        .lines()
        .filter_map(|l| l.trim().strip_prefix("rustup target add "))
        .map(str::to_string)
        .collect();
    assert_eq!(
        adds.len(),
        1,
        "this guard derives its musl needle from the WorkflowTemplate's \
         `rustup target add` set — expected exactly one entry, found {adds:?}"
    );
    adds[0].clone()
}

/// The written-out path fragments no scanned file may contain in
/// non-comment text: the stock layouts, the musl layout (triple derived
/// from CI), and the fleet `/build/` redirect however spelled.
fn needles() -> Vec<String> {
    vec![
        "target/debug".to_string(),
        "target/release".to_string(),
        format!("target/{}", ci_musl_triple()),
        "/build/".to_string(),
    ]
}

/// Whether `line` is a comment where it appears: `//` (any doc form) in
/// a `.rs` file, `#` everywhere else — shell, Python, systemd units,
/// markdown headings.
fn is_comment(line: &str, path: &str) -> bool {
    let trimmed = line.trim_start();
    if path.ends_with(".rs") {
        trimmed.starts_with("//")
    } else {
        trimmed.starts_with('#')
    }
}

/// Every needle hit on a non-comment line of `content`, one per line
/// (the first matching needle), with 1-based line numbers and the
/// trimmed line for reporting.
fn scan_content(path: &str, content: &str, needles: &[String]) -> Vec<Violation> {
    let mut out = Vec::new();
    for (i, line) in content.lines().enumerate() {
        if is_comment(line, path) {
            continue;
        }
        if let Some(needle) = needles.iter().find(|n| line.contains(n.as_str())) {
            out.push(Violation {
                path: path.to_string(),
                line_no: i + 1,
                needle: needle.clone(),
                line: line.trim().to_string(),
            });
        }
    }
    out
}

/// The scanned set: every regular file under `tests/` and `scripts/`
/// (repo-relative keys, any extension), plus `build.rs` when present.
fn scanned_files() -> BTreeMap<String, String> {
    let root = repo_root();
    let mut out = BTreeMap::new();
    for dir in ["tests", "scripts"] {
        let before = out.len();
        walk(&root, dir, &mut out);
        assert!(
            out.len() > before,
            "the {dir}/ sweep collected nothing — the walk is broken, the tree is not"
        );
    }
    if root.join("build.rs").is_file() {
        let path = root.join("build.rs");
        let bytes = fs::read(&path).unwrap_or_else(|e| panic!("reading build.rs: {e}"));
        out.insert(
            "build.rs".to_string(),
            String::from_utf8_lossy(&bytes).into_owned(),
        );
    }
    out
}

fn walk(root: &Path, rel: &str, out: &mut BTreeMap<String, String>) {
    let dir = root.join(rel);
    let entries = fs::read_dir(&dir).unwrap_or_else(|e| panic!("reading {rel}/ recursively: {e}"));
    for entry in entries {
        let path = entry
            .unwrap_or_else(|e| panic!("readdir entry in {rel}/: {e}"))
            .path();
        let name = path
            .file_name()
            .unwrap_or_else(|| panic!("{rel}/ entry without a file name"))
            .to_string_lossy()
            .into_owned();
        let child = format!("{rel}/{name}");
        if path.is_dir() {
            walk(root, &child, out);
        } else {
            let bytes = fs::read(&path).unwrap_or_else(|e| panic!("reading {child}: {e}"));
            out.insert(child, String::from_utf8_lossy(&bytes).into_owned());
        }
    }
}

/// The scan over a caller-supplied file set — the live tree for the
/// always-on test, mutated maps for the negative meta-tests — with the
/// two exemption tiers applied. Pure: nothing is read or written.
fn violations_with(
    files: &BTreeMap<String, String>,
    needles: &[String],
    file_exempt: &[(&str, &str)],
    line_exempt: &[(&str, &str, &str)],
) -> Vec<Violation> {
    let mut out = Vec::new();
    for (path, content) in files {
        if file_exempt.iter().any(|(p, _)| path.as_str() == *p) {
            continue;
        }
        out.extend(
            scan_content(path, content, needles)
                .into_iter()
                .filter(|v| {
                    !line_exempt
                        .iter()
                        .any(|(p, line, _)| path.as_str() == *p && v.line == *line)
                }),
        );
    }
    out
}

/// Allowlist hygiene over a caller-supplied file set: every entry must
/// stay live, pointed, unambiguous, needle-bearing, and non-redundant.
/// Pure over its inputs; returns the problem strings (empty = healthy).
fn allowlist_problems(
    files: &BTreeMap<String, String>,
    needles: &[String],
    file_exempt: &[(&str, &str)],
    line_exempt: &[(&str, &str, &str)],
) -> Vec<String> {
    let mut problems = Vec::new();
    for (path, rationale) in file_exempt {
        if !files.contains_key(*path) {
            problems.push(format!(
                "FILE_EXEMPT entry {path:?} names no scanned file \
                 (rationale: {rationale:?}) — remove the stale entry"
            ));
        }
    }
    for (path, wanted, rationale) in line_exempt.iter().copied() {
        let Some(content) = files.get(path) else {
            problems.push(format!(
                "LINE_EXEMPT entry {path:?} names no scanned file \
                 (rationale: {rationale:?}) — remove the stale entry"
            ));
            continue;
        };
        let matched: Vec<usize> = content
            .lines()
            .enumerate()
            .filter(|(_, line)| line.trim() == wanted)
            .map(|(i, _)| i + 1)
            .collect();
        match matched.as_slice() {
            [] => problems.push(format!(
                "LINE_EXEMPT entry {path:?} no longer matches any line — it \
                 exempted {wanted:?} ({rationale}); update or remove the entry \
                 in the same commit that changed the file"
            )),
            [only] => {
                if !needles.iter().any(|n| wanted.contains(n.as_str())) {
                    problems.push(format!(
                        "LINE_EXEMPT entry {path:?}:{only} covers a line \
                         containing no needle — a pointless entry; remove it"
                    ));
                }
                if file_exempt.iter().any(|(p, _)| *p == path) {
                    problems.push(format!(
                        "LINE_EXEMPT entry {path:?}:{only} sits inside a \
                         file exemption — redundant; drop one of the two"
                    ));
                }
            }
            several => problems.push(format!(
                "LINE_EXEMPT entry {path:?} matches {} lines ({several:?}) — \
                 disambiguate the quote or split the entries",
                several.len()
            )),
        }
    }
    problems
}

/// The doc slice from `heading` up to the next heading of any level —
/// the same scoping helper `tests/docs_build_layout.rs` uses, so a
/// string surviving elsewhere in the document cannot satisfy a
/// section-scoped check vacuously.
fn section<'a>(doc: &'a str, heading: &str) -> &'a str {
    let start = doc
        .find(heading)
        .unwrap_or_else(|| panic!("AGENTS.md must keep the {heading:?} heading"));
    let rest = &doc[start..];
    let end = rest[1..].find("\n#").map(|i| i + 1).unwrap_or(rest.len());
    &rest[..end]
}

#[test]
fn no_hardcoded_target_paths_outside_the_allowlist() {
    let flagged = violations_with(&scanned_files(), &needles(), FILE_EXEMPT, LINE_EXEMPT);
    let listed = flagged
        .iter()
        .map(|v| format!("  {}:{} [{}] {}", v.path, v.line_no, v.needle, v.line))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        flagged.is_empty(),
        "hardcoded build-output path(s) outside the allowlist — AGENTS.md \
         §\"Where the build output lands\": resolve artifacts through cargo's \
         own locators (current_exe(), CARGO_BIN_EXE_*, or a cargo-metadata-derived \
         target dir), never a written-out stock or fleet path; a hardcode passes \
         on a stock checkout and fails only on fleet hosts. If this occurrence \
         quotes a path as documentation or assertion data rather than resolving \
         it, add a reasoned LINE_EXEMPT entry in tests/target_path_guard.rs in \
         the same commit:\n{listed}"
    );
}

#[test]
fn allowlist_entries_stay_live_pointed_and_unambiguous() {
    let problems = allowlist_problems(&scanned_files(), &needles(), FILE_EXEMPT, LINE_EXEMPT);
    let listed = problems
        .iter()
        .map(|p| format!("  {p}"))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        problems.is_empty(),
        "the target-path allowlist has rotted — entries must move in the same \
         commit as the files they exempt:\n{listed}"
    );
}

#[test]
fn agents_md_names_this_guard_in_the_output_lands_section() {
    let doc = read_repo("AGENTS.md");
    let scoped = section(&doc, OUTPUT_LANDS_HEADING);
    assert!(
        scoped.contains("tests/target_path_guard.rs"),
        "AGENTS.md {OUTPUT_LANDS_HEADING} must point at its code-discipline \
         guard tests/target_path_guard.rs — an unfindable guard rots (the \
         benchmark_reproducibility pattern)"
    );
}

// ── Negative meta-tests: the guard must FAIL when its inputs rot ─────────────
//
// The claudepr-4d967120 pattern, committed rather than scratch-run: every
// leg mutates the in-memory file set (nothing is written to disk) and
// requires the scan or the hygiene check to report the planted defect —
// a mutation that passes means the guard is vacuous for it.

/// Require a reported violation at exactly `path:line_no` carrying
/// `needle`, and print everything the scan did report when it is missing.
fn expect_flagged(flagged: &[Violation], path: &str, line_no: usize, needle: &str) {
    let hit = flagged
        .iter()
        .find(|v| v.path == path && v.line_no == line_no);
    let Some(hit) = hit else {
        panic!(
            "planted hardcode at {path}:{line_no} was NOT reported — the \
             scan is vacuous there; reported: {flagged:?}"
        );
    };
    assert_eq!(
        hit.needle, needle,
        "the planted hardcode at {path}:{line_no} was reported under the \
         wrong needle"
    );
}

/// Require nothing reported at `path:line_no` — the commented-mention legs.
fn expect_not_flagged(flagged: &[Violation], path: &str, line_no: usize) {
    assert!(
        !flagged
            .iter()
            .any(|v| v.path == path && v.line_no == line_no),
        "a commented mention at {path}:{line_no} was reported — comments \
         quoting a layout are documentation, not hardcodes; reported: \
         {flagged:?}"
    );
}

/// Require a hygiene problem containing `fragment`.
fn expect_problem(problems: &[String], fragment: &str) {
    assert!(
        problems.iter().any(|p| p.contains(fragment)),
        "the planted allowlist defect ({fragment:?}) was not reported — \
         hygiene is vacuous for it; reported: {problems:?}"
    );
}

/// A planted hardcode in each of the three scanned locations — a new
/// test writing `target/debug/…` (the bead's motivating case: green on a
/// stock checkout, fleet-only failure), a script resolving the fleet
/// redirect, and a `build.rs` naming the musl layout — must be reported
/// with file, line, and needle, while the commented mentions planted
/// beside them must not be.
#[test]
fn negative_meta_planted_hardcodes_are_reported_across_the_whole_scope() {
    let triple = ci_musl_triple();
    let mut files = scanned_files();
    files.insert(
        "tests/planted_probe.rs".to_string(),
        "// prose: the stock layout is target/debug/claude-print\n\
         fn probe() {\n    let binary = \"target/debug/claude-print\";\n}\n"
            .to_string(),
    );
    files.insert(
        "scripts/planted_probe.sh".to_string(),
        "# cleanup sketch: rm -rf target/release leftovers\n\
         ls /build/claude-print/debug >/dev/null\n"
            .to_string(),
    );
    files.insert(
        "build.rs".to_string(),
        format!(
            "fn main() {{\n    println!(\"cargo:rustc-link-search=target/{triple}/release\");\n}}\n"
        ),
    );
    let flagged = violations_with(&files, &needles(), FILE_EXEMPT, LINE_EXEMPT);

    expect_flagged(&flagged, "tests/planted_probe.rs", 3, "target/debug");
    expect_flagged(&flagged, "scripts/planted_probe.sh", 2, "/build/");
    expect_flagged(&flagged, "build.rs", 2, &format!("target/{triple}"));
    expect_not_flagged(&flagged, "tests/planted_probe.rs", 1);
    expect_not_flagged(&flagged, "scripts/planted_probe.sh", 1);
}

/// Dropping a live LINE_EXEMPT entry must re-flag the line it covers —
/// the allowlist is what suppresses real detections, not a decoration
/// over an already-clean tree.
#[test]
fn negative_meta_dropping_a_live_exemption_reflags_its_line() {
    let files = scanned_files();
    let docstring = LINE_EXEMPT
        .iter()
        .copied()
        .find(|(_, line, _)| line.contains("(e.g. /build/claude-print"))
        .expect("a live exemption covering the bench docstring quote");
    let reduced: Vec<(&str, &str, &str)> = LINE_EXEMPT
        .iter()
        .copied()
        .filter(|entry| *entry != docstring)
        .collect();
    let flagged = violations_with(&files, &needles(), FILE_EXEMPT, &reduced);
    assert!(
        flagged
            .iter()
            .any(|v| v.path == docstring.0 && v.line == docstring.1),
        "dropping the LINE_EXEMPT for {:?} must re-flag the line it \
         covers — otherwise the allowlist is decorative; reported: {flagged:?}",
        docstring.0
    );
    // With the entry present the same tree stays clean for that file —
    // the entry suppresses exactly its quote, nothing wider.
    assert!(
        violations_with(&files, &needles(), FILE_EXEMPT, LINE_EXEMPT)
            .iter()
            .all(|v| v.path != docstring.0),
        "with the exemption present the bench script must be clean"
    );
}

/// Allowlist rot must fail hygiene: an entry whose line is gone, one
/// covering a needle-free line, one naming a file the sweep never sees
/// (line form and file form), and one redundant with a file exemption.
#[test]
fn negative_meta_stale_or_pointless_allowlist_entries_fail_hygiene() {
    let files = scanned_files();
    let ns = needles();

    let stale = [(
        "tests/benchmark_reproducibility.rs",
        "fn ghost_probe_line_never_present() {",
        "probe",
    )];
    expect_problem(
        &allowlist_problems(&files, &ns, FILE_EXEMPT, &stale),
        "no longer matches any line",
    );

    let mut planted = files.clone();
    planted.insert(
        "tests/planted_pointless.rs".to_string(),
        "fn clean() {}\n".to_string(),
    );
    let pointless = [("tests/planted_pointless.rs", "fn clean() {}", "probe")];
    expect_problem(
        &allowlist_problems(&planted, &ns, FILE_EXEMPT, &pointless),
        "containing no needle",
    );

    let ghost_line = [("tests/ghost_probe.rs", "anything at all", "probe")];
    expect_problem(
        &allowlist_problems(&files, &ns, FILE_EXEMPT, &ghost_line),
        "names no scanned file",
    );
    let ghost_file = [("tests/ghost_probe.rs", "probe")];
    expect_problem(
        &allowlist_problems(&files, &ns, &ghost_file, LINE_EXEMPT),
        "names no scanned file",
    );

    // Redundant: a line entry inside the file-exempted sibling guard.
    // Property-selected (first line carrying a needle) so the leg stays
    // valid when the sibling's own text changes.
    let sibling = "tests/docs_build_layout.rs";
    let quoted = files[sibling]
        .lines()
        .map(str::trim)
        .find(|l| ns.iter().any(|n| l.contains(n.as_str())))
        .unwrap_or_else(|| panic!("{sibling} carries a needle literal to quote"));
    let redundant = [(sibling, quoted, "probe")];
    expect_problem(
        &allowlist_problems(&files, &ns, FILE_EXEMPT, &redundant),
        "file exemption",
    );
}
