//! Drift-pin for AGENTS.md's build/test documentation (bead claudepr-9c5f098f).
//!
//! The repo's established pattern pins doc claims to standing tests — the
//! README Configuration summary via `tests/config_contract.rs`, the
//! platform matrix via `tests/platform_matrix_docs.rs`, the HOME call-site
//! table via `tests/home_env_guard.rs` — but AGENTS.md's build/test
//! documentation stayed free prose with no guard. Three surfaces could
//! silently rot:
//!
//! - **§"Where the build output lands"** — the stock-vs-fleet artifact
//!   table documents where `cargo build` puts `claude-print` and
//!   `mock-claude` under both layouts, and the prose claims the test
//!   suites resolve those artifacts through cargo's own locators
//!   (`current_exe()` / `CARGO_BIN_EXE_*`), never a written-out path.
//!   A renamed `[[bin]]`, a CI toolchain widening, or a suite quietly
//!   hardcoding `target/debug/…` would leave every claim green.
//! - **The `cargo metadata` derivation** — the documented way to resolve
//!   the target dir without hardcoding either column.
//! - **§"Test structure"** — the fixtures row documents an exhaustive
//!   inventory of `tests/fixtures/`, and descriptions throughout the
//!   table name test files and fixtures that must keep existing.
//!
//! What this guard deliberately does **not** pin: the fleet wrapper's
//! per-repo redirect (`/build/claude-print`) is an environment fact of the
//! hosts, not a repository fact — the guard only requires the fleet column
//! to be one consistent absolute base carrying the same cargo-relative
//! suffixes as the stock column, and reads that base from the table
//! itself instead of hardcoding it here.
//!
//! One claim was already drifted when this guard landed and is fixed in
//! the same commit: the locator prose attributed `current_exe()` to
//! `tests/pty_integration.rs` alone and "CARGO_BIN_EXE_* elsewhere",
//! while most e2e suites actually resolve `current_exe()`-relative and
//! only the config/HOME suites use the compile-time env var. The prose
//! now names real examples of both, and this test verifies every named
//! example against its source.
//!
//! Library-level like the rows it guards: reads AGENTS.md, Cargo.toml,
//! the CI WorkflowTemplate, and the `tests/` tree; resolves the built
//! artifacts through cargo's real locators at runtime; spawns nothing.

use std::collections::BTreeSet;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

/// Parse sentinels: the exact header lines of the two AGENTS.md tables
/// this guard consumes. Locating tables by header keeps the guard immune
/// to surrounding prose edits and fails loudly (not vacuously) if a
/// table is renamed or removed.
const ARTIFACT_TABLE_HEADER: &str = "| Artifact | Stock checkout | Fleet hosts (wrapper) |";
const TEST_STRUCTURE_HEADER: &str = "| Location | What it tests |";

/// The heading of the section whose claims are pinned here — used to scope
/// prose assertions so a string surviving elsewhere in the document cannot
/// satisfy them vacuously.
const OUTPUT_LANDS_HEADING: &str = "### Where the build output lands";

/// The paragraph this guard locates by sentinel — the locator claim.
const LOCATOR_PARAGRAPH_SENTINEL: &str = "Tests need no path pinning";

/// This guard cannot lint its own sources for hardcoded build-output
/// paths: its needle list is itself a set of those literals, and its
/// grammar checker names the shapes it parses.
const SELF_TARGET: &str = "docs_build_layout";

/// `tests/benchmark_reproducibility.rs` carries the same literals as its
/// own lint needles (`"/build/"` et al.) — it asserts on those strings, it
/// does not build paths from them.
const LINT_EXEMPT: [&str; 1] = ["benchmark_reproducibility"];

/// Written-out build-output path fragments no test source may contain in
/// non-comment code: the stock layout, the musl release layout, and the
/// fleet redirect. AGENTS.md §"Where the build output lands" says "Don't
/// hardcode either column"; this is that sentence, enforced.
const HARDCODED_PATH_NEEDLES: [&str; 4] =
    ["target/debug", "target/release", "musl/release", "/build/"];

/// Read a repo file, resolving the root from the *runtime*
/// `CARGO_MANIFEST_DIR` (compile-time value as fallback). The compile-time
/// value alone bakes the building checkout's path into the test binary;
/// when the shared target cache reuses that binary from a different
/// extraction — exactly the clean-tree verification NEEDLE re-runs — the
/// read would hit a directory that no longer exists. See
/// `tests/docs_slug_consistency.rs::doc_files` for the full rationale.
fn repo_file(relative: &str) -> String {
    let root = repo_root();
    fs::read_to_string(root.join(relative))
        .unwrap_or_else(|e| panic!("read {relative} from the checkout under test: {e}"))
}

fn repo_path(relative: &str) -> PathBuf {
    repo_root().join(relative)
}

fn repo_root() -> PathBuf {
    PathBuf::from(
        std::env::var("CARGO_MANIFEST_DIR")
            .unwrap_or_else(|_| env!("CARGO_MANIFEST_DIR").to_string()),
    )
}

fn agents_md() -> String {
    repo_file("AGENTS.md")
}

/// Rows of the markdown table introduced by the exact header line
/// `header`, cells trimmed; the `|---|` separator row is skipped. Panics if
/// the header is absent — a missing table is drift, not a pass.
fn table_rows(doc: &str, header: &str) -> Vec<Vec<String>> {
    let start = doc
        .lines()
        .position(|l| l.trim() == header)
        .unwrap_or_else(|| {
            panic!(
                "AGENTS.md no longer contains the table header {header:?} — \
                    this guard locates its tables by that sentinel line"
            )
        });
    let mut rows = Vec::new();
    for line in doc.lines().skip(start + 1) {
        let t = line.trim();
        if !t.starts_with('|') {
            if rows.is_empty() && t.is_empty() {
                continue;
            }
            break;
        }
        let cells: Vec<&str> = t.trim_matches('|').split('|').collect();
        let is_separator = cells.iter().all(|c| {
            let c = c.trim().trim_start_matches(':').trim_end_matches(':');
            !c.is_empty() && c.chars().all(|d| d == '-')
        });
        if is_separator {
            continue;
        }
        rows.push(cells.into_iter().map(|c| c.trim().to_string()).collect());
    }
    rows
}

/// The doc slice from `heading` up to the next heading of any level.
fn section<'a>(doc: &'a str, heading: &str) -> &'a str {
    let start = doc
        .find(heading)
        .unwrap_or_else(|| panic!("AGENTS.md must keep the {heading:?} heading this guard scopes"));
    let rest = &doc[start..];
    // `\n#` matches any heading level, so a re-nesting cannot hide the end.
    let end = rest[1..].find("\n#").map(|i| i + 1).unwrap_or(rest.len());
    &rest[..end]
}

/// Backtick-quoted tokens of `text`, in order: chunks at odd indices of a
/// split on the backtick character.
fn backticked(text: &str) -> Vec<String> {
    text.split('`')
        .enumerate()
        .filter(|(i, _)| i % 2 == 1)
        .map(|(_, t)| t.to_string())
        .collect()
}

/// Expand a single-level `{a,b,c}` brace form into its alternatives
/// (`stream_json_golden_v2.1.282.{input,expected,errors}.jsonl` → the
/// three file names). Brace-free tokens come back as themselves.
fn expand_braces(token: &str) -> Vec<String> {
    if let (Some(open), Some(close_rel)) = (token.find('{'), token.find('}')) {
        if open < close_rel {
            let close = close_rel;
            let (prefix, suffix) = (&token[..open], &token[close + 1..]);
            return token[open + 1..close]
                .split(',')
                .flat_map(|alt| expand_braces(&format!("{prefix}{alt}{suffix}")))
                .collect();
        }
    }
    vec![token.to_string()]
}

/// True if `content` contains `needle` on a non-comment line. `//` lines
/// (including `///` and `//!`) are skipped so prose mentions of a marker
/// can neither satisfy a presence check nor trip an absence one.
fn noncomment_contains(content: &str, needle: &str) -> bool {
    content
        .lines()
        .any(|l| !l.trim_start().starts_with("//") && l.contains(needle))
}

/// Cargo's autodiscovered integration-test targets: one per top-level
/// `tests/*.rs` file (the enumeration assumptions behind this listing are
/// pinned by `tests/docs_test_classification.rs`).
fn filesystem_targets() -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for entry in fs::read_dir(repo_path("tests")).expect("reading tests/") {
        let path = entry.expect("readdir entry in tests/").path();
        if path.is_file() && path.extension().is_some_and(|e| e == "rs") {
            let name = path.file_name().expect("file name").to_string_lossy();
            if let Some(stem) = name.strip_suffix(".rs") {
                out.insert(stem.to_string());
            }
        }
    }
    assert!(!out.is_empty(), "no test targets found under tests/");
    out
}

/// Every `tests/**/*.rs` file as a repo-relative path — a flat superset of
/// every compilation unit's files (targets plus `mod`-included helpers),
/// which is what the hardcoded-path lint wants to sweep.
fn tests_tree_rs_files() -> Vec<String> {
    fn walk(dir: &Path, out: &mut Vec<String>) {
        for entry in fs::read_dir(dir).expect("reading a tests/ subdirectory") {
            let path = entry.expect("readdir entry").path();
            if path.is_dir() {
                walk(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push(path.display().to_string());
            }
        }
    }
    let mut out = Vec::new();
    // repo-rooted prefix for the per-file exemption checks below
    let prefix = repo_root().display().to_string();
    walk(&repo_path("tests"), &mut out);
    for rel in &mut out {
        if let Some(stripped) = rel.strip_prefix(&format!("{prefix}/")) {
            *rel = stripped.to_string();
        }
    }
    out
}

/// The package's `[[bin]]` names from Cargo.toml — the set of binaries any
/// `cargo build`/`cargo test` produces, and therefore the only names the
/// artifact table may document.
fn cargo_bins() -> Vec<String> {
    let manifest: toml::Value =
        toml::from_str(&repo_file("Cargo.toml")).expect("parsing Cargo.toml");
    let bins = manifest
        .get("bin")
        .and_then(|b| b.as_array())
        .expect("Cargo.toml must declare its [[bin]] targets");
    let names: Vec<String> = bins
        .iter()
        .map(|b| {
            b.get("name")
                .and_then(|n| n.as_str())
                .expect("[[bin]] name")
                .to_string()
        })
        .collect();
    assert!(
        names.contains(&"claude-print".to_string()) && names.contains(&"mock-claude".to_string()),
        "the artifact table and this guard assume Cargo.toml builds the `claude-print` \
         binary and the `mock-claude` fixture as [[bin]] targets — found {names:?}"
    );
    names
}

/// The one musl toolchain the CI WorkflowTemplate installs, derived the
/// same way `tests/platform_matrix_docs.rs` derives it. The artifact
/// table's musl row and AGENTS.md's musl build command both take their
/// triple from here, so CI widening the release fails this guard until
/// the documentation is deliberately updated.
fn ci_musl_triple() -> String {
    let template = repo_file("claude-print-ci-workflowtemplate.yml");
    let adds: Vec<String> = template
        .lines()
        .filter_map(|l| l.trim().strip_prefix("rustup target add "))
        .map(str::to_string)
        .collect();
    assert_eq!(
        adds.len(),
        1,
        "this guard derives the musl artifact layout from the WorkflowTemplate's \
         `rustup target add` set — expected exactly one entry, found {adds:?}. A \
         widening must update AGENTS.md §\"Where the build output lands\" and its \
         build commands together (mirroring tests/platform_matrix_docs.rs)"
    );
    adds[0].clone()
}

/// Parse a stock-checkout cell into `(profile_shape, bin)`, where the shape
/// is `debug`, `release`, or `<triple>/release` with the triple CI builds.
/// Panics on any other shape or a bin Cargo.toml does not build.
fn parse_stock_cell(stock: &str, triple: &str, bins: &[String]) -> (String, String) {
    let suffix = stock.strip_prefix("target/").unwrap_or_else(|| {
        panic!(
            "stock-checkout cell {stock:?} must be the checkout-rooted layout \
             `target/<suffix>` — a written-out host path belongs in neither column"
        )
    });
    let parts: Vec<&str> = suffix.split('/').collect();
    let (shape, bin) = match parts.as_slice() {
        [profile, bin] if *profile == "debug" || *profile == "release" => {
            ((*profile).to_string(), (*bin).to_string())
        }
        [t, "release", bin] if *t == triple => (format!("{t}/release"), (*bin).to_string()),
        _ => panic!(
            "artifact-path suffix {suffix:?} is neither `debug/<bin>`, `release/<bin>`, \
             nor `<triple>/release/<bin>` with triple {triple:?} from the CI \
             WorkflowTemplate — the table documents real cargo layouts"
        ),
    };
    assert!(
        bins.contains(&bin),
        "artifact row names bin {bin:?}, which Cargo.toml does not build — the \
         table may only document real [[bin]] outputs (found: {bins:?})"
    );
    (shape, bin)
}

/// Fixture names mentioned in `text`: backticked tokens ending in a
/// fixture extension that are either bare names or `tests/fixtures/`
/// paths, brace forms expanded.
/// Fixture names from `tests/fixtures/`-prefixed backticked tokens — the
/// unambiguous fixture references usable anywhere in the table.
fn prefixed_fixture_names(text: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for token in backticked(text) {
        if let Some(name) = token.strip_prefix("tests/fixtures/") {
            if name.is_empty() {
                continue; // the inventory row's own Location cell
            }
            for expanded in expand_braces(name) {
                out.insert(expanded);
            }
        }
    }
    out
}

/// Fixture names from every backticked token of the fixtures inventory row:
/// bare filenames and `tests/fixtures/` paths ending in a fixture
/// extension, brace forms expanded. Bare extension tokens (`` `.jsonl` ``)
/// and other-tree artifacts (`target/last-claude-version.txt`,
/// `sha256sums.txt`) never appear in that row, so scoping the loose bare
/// heuristic to it keeps both directions exact.
fn fixture_row_names(text: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for token in backticked(text) {
        let is_fixture_ext =
            token.ends_with(".json") || token.ends_with(".jsonl") || token.ends_with(".txt");
        if !is_fixture_ext || token.starts_with('.') {
            continue;
        }
        if token.contains('/') && !token.starts_with("tests/fixtures/") {
            continue;
        }
        let name = token.strip_prefix("tests/fixtures/").unwrap_or(&token);
        for expanded in expand_braces(name) {
            out.insert(expanded);
        }
    }
    out
}

fn is_executable(path: &Path) -> bool {
    fs::metadata(path)
        .map(|m| m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

/// The artifact table as `(stock cell, fleet cell)` pairs, validated for
/// row shape and duplicate rows.
fn artifact_rows() -> Vec<(String, String)> {
    let rows = table_rows(&agents_md(), ARTIFACT_TABLE_HEADER);
    assert!(
        !rows.is_empty(),
        "the stock-vs-fleet artifact table ({ARTIFACT_TABLE_HEADER}) has no rows"
    );
    let mut out = Vec::new();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    for row in rows {
        assert_eq!(
            row.len(),
            3,
            "artifact-table rows must be `| Artifact | Stock checkout | Fleet hosts \
             (wrapper) |` — a literal `|` inside a cell breaks the parse: {row:?}"
        );
        let stock = row[1].trim_matches('`').to_string();
        assert!(
            seen.insert(stock.clone()),
            "duplicate artifact row for stock cell {stock:?} — one row per artifact"
        );
        out.push((stock, row[2].trim_matches('`').to_string()));
    }
    out
}

#[test]
fn stock_fleet_artifact_table_matches_cargo_bins_and_ci_toolchain() {
    let bins = cargo_bins();
    let triple = ci_musl_triple();
    let mut documented: BTreeSet<(String, String)> = BTreeSet::new();
    let mut fleet_cells: Vec<(String, String)> = Vec::new(); // (stock suffix, fleet cell)

    for (stock, fleet) in artifact_rows() {
        let (shape, bin) = parse_stock_cell(&stock, &triple, &bins);
        documented.insert((shape, bin));
        let suffix = stock
            .strip_prefix("target/")
            .expect("grammar checked above");
        fleet_cells.push((suffix.to_string(), fleet));
    }

    // The documented inventory: the deployed binary in all three profile
    // shapes plus the fixture where the suites resolve it. Anything more is
    // fine as long as it parses; anything less is drift.
    for (shape, bin) in [
        ("debug".to_string(), "claude-print".to_string()),
        ("release".to_string(), "claude-print".to_string()),
        (format!("{triple}/release"), "claude-print".to_string()),
        ("debug".to_string(), "mock-claude".to_string()),
    ] {
        assert!(
            documented.contains(&(shape.clone(), bin.clone())),
            "the artifact table must document the `{bin}` output for the `{shape}` \
             layout — derived from Cargo.toml's [[bin]] set and the WorkflowTemplate's \
             {triple} toolchain, so a rename or widening fails here until the table \
             follows"
        );
    }

    // Fleet column: one consistent absolute redirect base carrying the same
    // cargo-relative suffixes as the stock column. The base itself (the
    // wrapper's per-repo redirect) is a fleet-environment fact and is read
    // from the table, not asserted here — only its internal consistency is
    // a repository claim.
    let debug_suffix = "debug/claude-print";
    let debug_fleet = fleet_cells
        .iter()
        .find(|(s, _)| s == debug_suffix)
        .expect("the debug-binary row checked above")
        .1
        .clone();
    let base = debug_fleet
        .strip_suffix(debug_suffix)
        .unwrap_or_else(|| {
            panic!(
                "fleet cell {debug_fleet:?} must carry the stock suffix \
                 {debug_suffix:?} — the two columns differ only in where the \
                 target dir lives"
            )
        })
        .to_string();
    assert!(
        base.starts_with('/'),
        "the fleet column must be an absolute redirected target dir — derived \
         base {base:?} is not"
    );
    for (suffix, fleet) in &fleet_cells {
        assert_eq!(
            *fleet,
            format!("{base}{suffix}"),
            "fleet cell {fleet:?} must be the cargo-relative suffix {suffix:?} under \
             the one redirect base {base:?} derived from the debug-binary row — two \
             redirect targets in one table is drift"
        );
    }
}

#[test]
fn documented_musl_build_command_matches_the_ci_toolchain() {
    let triple = ci_musl_triple();
    let expected = format!("cargo build --target {triple} --release");
    assert!(
        agents_md().contains(&expected),
        "AGENTS.md's build commands must build the musl release CI actually \
         publishes — expected {expected:?}, derived from the WorkflowTemplate's \
         `rustup target add` set (tests/platform_matrix_docs.rs pins the README \
         and install.sh side of the same matrix)"
    );
}

#[test]
fn target_dir_snippet_derives_the_artifact_it_names() {
    let doc = agents_md();
    let scoped = section(&doc, OUTPUT_LANDS_HEADING);
    // The documented derivation, verbatim: cargo reports the target dir and
    // the artifact is addressed relative to it.
    for fragment in [
        "cargo metadata --no-deps --format-version 1 | jq -r .target_directory",
        "\"$TARGET/debug/claude-print\" --check",
    ] {
        assert!(
            scoped.contains(fragment),
            "AGENTS.md {OUTPUT_LANDS_HEADING} lost the target-dir derivation \
             fragment {fragment:?} — the documented way to resolve build output \
             must stay the cargo-derived one, never a written-out path"
        );
    }
    // The snippet's example artifact is the table's stock debug row.
    let stock_debug = "target/debug/claude-print";
    assert!(
        artifact_rows()
            .iter()
            .any(|(stock, _)| stock == stock_debug),
        "the snippet addresses {stock_debug:?}, which the artifact table no \
         longer documents — snippet and table must name the same artifacts"
    );
}

#[test]
fn locator_examples_in_the_path_resolution_prose_are_real() {
    let doc = agents_md();
    let para = doc
        .split("\n\n")
        .find(|p| p.contains(LOCATOR_PARAGRAPH_SENTINEL))
        .unwrap_or_else(|| {
            panic!(
                "AGENTS.md must keep the \"{LOCATOR_PARAGRAPH_SENTINEL}\" paragraph — \
                 it is the locator claim this guard pins"
            )
        });
    assert!(
        para.contains("current_exe()") && para.contains("CARGO_BIN_EXE_claude-print"),
        "the locator paragraph must name both documented locators — \
         `current_exe()` and `CARGO_BIN_EXE_claude-print`"
    );

    // Positional attribution: example files named before the CARGO_BIN_EXE
    // mention are documented as current_exe() users, after it as
    // CARGO_BIN_EXE users. The split point includes the token's opening
    // backtick so neither half cuts a backticked token in two (which would
    // flip the parity of every token after it). Rewording the paragraph
    // regrouping the examples fails here — update the prose or this guard
    // together.
    let split = para.find("`CARGO_BIN_EXE").expect("checked above");
    let (head, tail) = para.split_at(split);
    let targets = filesystem_targets();
    let mentioned = |text: &str| -> Vec<String> {
        backticked(text)
            .into_iter()
            .filter_map(|t| {
                let stem = t.strip_prefix("tests/")?.strip_suffix(".rs")?;
                targets.contains(stem).then(|| stem.to_string())
            })
            .collect()
    };
    let via_current_exe = mentioned(head);
    let via_env = mentioned(tail);
    assert!(
        !via_current_exe.is_empty() && !via_env.is_empty(),
        "both locators need at least one named `tests/<name>.rs` example — the \
         examples are what keeps the attribution checkable (current_exe(): \
         {via_current_exe:?}, CARGO_BIN_EXE: {via_env:?})"
    );
    for stem in via_current_exe {
        let src = repo_file(&format!("tests/{stem}.rs"));
        assert!(
            noncomment_contains(&src, "current_exe"),
            "`tests/{stem}.rs` is documented as a current_exe() example but its \
             code never calls it — repoint the example at a suite that actually \
             resolves the binary that way"
        );
    }
    for stem in via_env {
        let src = repo_file(&format!("tests/{stem}.rs"));
        assert!(
            noncomment_contains(&src, "CARGO_BIN_EXE_claude-print"),
            "`tests/{stem}.rs` is documented as a CARGO_BIN_EXE_claude-print \
             example but its code never reads it — repoint the example at a suite \
             that actually resolves the binary that way"
        );
    }
}

#[test]
fn no_test_source_hardcodes_a_build_output_location() {
    for rel in tests_tree_rs_files() {
        let exempt = rel == format!("tests/{SELF_TARGET}.rs")
            || LINT_EXEMPT.iter().any(|t| rel == format!("tests/{t}.rs"));
        if exempt {
            continue; // this guard's own needles, and the reproducibility lint's
        }
        let src = repo_file(&rel);
        for (i, line) in src.lines().enumerate() {
            if line.trim_start().starts_with("//") {
                continue;
            }
            for needle in HARDCODED_PATH_NEEDLES {
                assert!(
                    !line.contains(needle),
                    "{rel}:{} hardcodes a build-output location ({needle:?}) — \
                     AGENTS.md §\"Where the build output lands\": resolve through \
                     cargo (current_exe()/CARGO_BIN_EXE_*/a cargo-metadata-derived \
                     target dir), never a written-out stock or fleet path",
                    i + 1
                );
            }
        }
    }
}

#[test]
fn documented_locators_resolve_the_artifacts_cargo_just_built() {
    // The documented current_exe() mechanism: test binaries sit in
    // <target>/<profile>/deps/, the package bins beside them in
    // <target>/<profile>/.
    let exe = std::env::current_exe().expect("current_exe");
    let deps = exe.parent().expect("the test binary has a parent dir");
    assert_eq!(
        deps.file_name().and_then(|n| n.to_str()),
        Some("deps"),
        "the documented current_exe() resolution assumes test binaries live \
         under <target>/<profile>/deps/ — found this one at {}",
        exe.display()
    );
    let profile_dir = deps.parent().expect("deps/ has a parent");
    let profile = profile_dir
        .file_name()
        .and_then(|n| n.to_str())
        .expect("profile dir has a name")
        .to_string();
    assert!(
        ["debug", "release"].contains(&profile.as_str()),
        "the profile dir {profile:?} matches no artifact-row shape — the table \
         documents `debug/…` and `release/…` layouts only"
    );

    for bin in cargo_bins() {
        let path = profile_dir.join(&bin);
        assert!(
            path.is_file(),
            "`{bin}` must sit beside the test binaries in <target>/{profile}/ — \
             that is the current_exe()-relative location AGENTS.md documents and \
             the suites resolve: {} is missing",
            path.display()
        );
        assert!(
            is_executable(&path),
            "`{bin}` at {} is not executable — a build artifact the suites spawn",
            path.display()
        );
    }

    // The compile-time locator must agree with the runtime one: both are
    // views of the same <target>/<profile> dir, which is why the suites
    // "pass wherever cargo puts the build". canonicalize both sides so a
    // symlinked target dir cannot fork them spuriously.
    let via_env_main = env!("CARGO_BIN_EXE_claude-print");
    let via_env_mock = option_env!("CARGO_BIN_EXE_mock-claude").unwrap_or_else(|| {
        "cargo sets CARGO_BIN_EXE_<name> for every package bin when building \
             integration tests — mock-claude is a [[bin]] of this package (see the \
             Cargo.toml note), so the env var must be set for this target"
    });
    for (name, via_env) in [
        ("claude-print", via_env_main),
        ("mock-claude", via_env_mock),
    ] {
        let from_env = fs::canonicalize(via_env)
            .unwrap_or_else(|e| panic!("CARGO_BIN_EXE_{name} = {via_env} does not resolve: {e}"));
        let from_exe = fs::canonicalize(profile_dir.join(name)).unwrap_or_else(|e| {
            panic!(
                "the current_exe()-derived {} does not resolve: {e}",
                profile_dir.join(name).display()
            )
        });
        assert_eq!(
            from_env, from_exe,
            "CARGO_BIN_EXE_{name} and the current_exe()-derived profile dir disagree \
             — the two documented locators are no longer two views of one build"
        );
    }
}

#[test]
fn test_structure_table_mentions_resolve_and_fixtures_row_is_exhaustive() {
    let rows = table_rows(&agents_md(), TEST_STRUCTURE_HEADER);
    assert!(
        !rows.is_empty(),
        "the Test structure table ({TEST_STRUCTURE_HEADER}) has no rows"
    );
    let joined = rows
        .iter()
        .map(|r| r.join(" | "))
        .collect::<Vec<_>>()
        .join("\n");

    // (a) Every backticked `tests/…​.rs` / `src/…​.rs` mention in the table
    // resolves to a real file — a renamed target or moved module must
    // ripple through the descriptions or fail here. Shorthand tokens
    // (glob/wildcard/placeholder forms like `src/*.rs` or
    // `tests/<name>.rs`) and non-path tokens are skipped.
    for token in backticked(&joined) {
        if token.contains('*') || token.contains('<') || token.contains('>') || !token.is_ascii() {
            continue;
        }
        let Some(rs_at) = token.find(".rs") else {
            continue;
        };
        let path = &token[..rs_at + 3];
        if path.starts_with("tests/") || path.starts_with("src/") {
            assert!(
                repo_path(path).is_file(),
                "AGENTS.md's Test structure table mentions {path:?}, which does not \
                 exist — a rename or move must ripple through the table"
            );
        }
    }

    // (b) Every fixture named anywhere in the table (unambiguous prefixed
    // form) exists on disk.
    for name in prefixed_fixture_names(&joined) {
        assert!(
            repo_path(&format!("tests/fixtures/{name}")).is_file(),
            "the Test structure table names the fixture {name:?}, which is not in \
             tests/fixtures/ — a moved or renamed fixture must ripple through the \
             table"
        );
    }

    // (c) The `tests/fixtures/` inventory row is exhaustive: every fixture
    // on disk is named in it (brace forms expanded), so a fixture can no
    // longer be added silently.
    let fixtures_row = rows
        .iter()
        .find(|r| r[0].trim_matches('`') == "tests/fixtures/")
        .expect("the Test structure table must keep a `tests/fixtures/` inventory row");
    let row_mentioned = fixture_row_names(&fixtures_row.join(" | "));
    let on_disk: BTreeSet<String> = fs::read_dir(repo_path("tests/fixtures"))
        .expect("reading tests/fixtures/")
        .map(|e| {
            e.expect("readdir entry")
                .file_name()
                .to_string_lossy()
                .to_string()
        })
        .collect();
    assert!(!on_disk.is_empty(), "tests/fixtures/ has no fixtures?");
    let undocumented: Vec<_> = on_disk.difference(&row_mentioned).collect();
    let stale: Vec<_> = row_mentioned.difference(&on_disk).collect();
    assert!(
        undocumented.is_empty() && stale.is_empty(),
        "the AGENTS.md fixtures inventory row and tests/fixtures/ disagree — \
         fixtures present but not documented (add rows to the cell): \
         {undocumented:?}; documented but absent (drop them): {stale:?}"
    );

    // (d) The module-directory row: `tests/integration/` exists and is
    // included by `tests/integration.rs`, not an autodiscovered target.
    assert!(
        repo_path("tests/integration").is_dir(),
        "the table documents `tests/integration/` as a sub-module directory — the \
         directory must exist"
    );
    let integration = repo_file("tests/integration.rs");
    assert!(
        noncomment_contains(&integration, "mod")
            && noncomment_contains(&integration, "integration/"),
        "the `tests/integration/` row claims a sub-module directory of \
         tests/integration.rs — keep the mod include (with its #[path] attribute) \
         that makes it one"
    );
}

#[test]
fn where_output_lands_section_names_this_guard() {
    let doc = agents_md();
    let scoped = section(&doc, OUTPUT_LANDS_HEADING);
    assert!(
        scoped.contains(&format!("tests/{SELF_TARGET}.rs")),
        "AGENTS.md {OUTPUT_LANDS_HEADING} must point at its drift guard \
         tests/{SELF_TARGET}.rs — an unfindable guard rots (the \
         benchmark_reproducibility pattern)"
    );
}
