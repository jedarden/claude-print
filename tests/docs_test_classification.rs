//! Documentation-drift guard for the exhaustive test-target classification.
//!
//! AGENTS.md §"Execution requirements" claims every `tests/*.rs` target is
//! classified exactly once across the documented execution groups
//! (compiled-binaries, repo-scripts, real-claude, library-level) plus the
//! `config_error_helpers` helper carve-out, and warns that a new target
//! which fits none of them is documentation drift. Nothing enforced that
//! claim: a target added without a classification silently vanished from
//! the execution-requirements contract, a stale row survived its target's
//! deletion, and a row's group could quietly stop matching the target's
//! real dependencies (bead claudepr-26cf624a). This test re-derives the
//! classification from the tree and fails CI on the drift:
//!
//! 1. **Exhaustive bijection** — the classification table has exactly one
//!    row per top-level `tests/*.rs` file: unclassified new targets, stale
//!    rows, and duplicate classifications all fail.
//! 2. **Dependency claims** — each group's row is verified against the
//!    target's *compilation unit* (the target file plus every file it
//!    pulls in via `mod …;` — the same union cargo compiles):
//!    compiled-binaries rows must locate a built binary (`CARGO_BIN_EXE_*`
//!    or `current_exe()`), repo-scripts rows must spawn a stub/script child
//!    without either binary locator, real-claude rows must spawn a `"claude"`
//!    probe, and library-level rows must contain no `std::process::Command`
//!    — the one sanctioned exception being spawning confined to `#[ignore]`d
//!    tests with the carve-out declared in the row's notes (the
//!    `claude_contracts` live-probe shape).
//! 3. **Adjacent contracts** — the Ignored table matches the `#[ignore]`d
//!    tests actually present in the tree, the §"Test structure" table lists
//!    every target, and the Cargo.toml autodiscovery assumptions the
//!    filesystem enumeration rests on (no `[[test]]` sections, `autotests`
//!    not disabled, no `tests/<dir>/main.rs` suites) are pinned so the
//!    enumeration cannot silently diverge from cargo's real target set.
//!
//! Library-level like the rows it guards: reads AGENTS.md and the `tests/`
//! tree, spawns nothing.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

/// Probe files that identify a claude-print checkout: a directory holding
/// both is a usable repo root for this guard.
const ROOT_PROBES: [&str; 2] = ["AGENTS.md", "Cargo.toml"];

/// Repository root this guard reads, resolved at *runtime* — never the bare
/// compile-time `env!("CARGO_MANIFEST_DIR")`, which bakes the building
/// checkout's path into the test binary. The local cargo wrapper maps
/// `.git`-less extractions onto one shared target dir, so an extraction of
/// unchanged content instant-reuses a cached test binary compiled in an
/// extraction that has since been deleted; a baked-only root then fails
/// every later run of that binary with file-NotFound panics that have
/// nothing to do with drift (bead claudepr-23f81f16). Candidates, most
/// authoritative first, each probe-verified before use:
///
/// 1. `$CLAUDE_PRINT_TEST_REPO` — explicit override for direct binary runs;
///    when set it is authoritative and must itself be a checkout.
/// 2. the runtime `CARGO_MANIFEST_DIR` cargo sets in the test process to
///    the package under test — the live extraction even in a cache-reused
///    binary (the same dance as `tests/docs_slug_consistency.rs`).
/// 3. the compile-time `CARGO_MANIFEST_DIR` — last resort for running the
///    test binary directly, where cargo sets neither variable.
///
/// If no candidate survives its probe the guard panics naming every
/// candidate it rejected — loud, never a vacuous pass off a wrong tree.
fn repo_root() -> PathBuf {
    resolve_repo_root(
        std::env::var("CLAUDE_PRINT_TEST_REPO").ok().as_deref(),
        std::env::var("CARGO_MANIFEST_DIR").ok().as_deref(),
        env!("CARGO_MANIFEST_DIR"),
    )
    .unwrap_or_else(|e| panic!("locating the repo root to read AGENTS.md and tests/ from: {e}"))
}

/// [`repo_root`]'s candidate chain as a pure function, so the precedence
/// and the loud failure are testable without racing the process-wide
/// environment from parallel tests.
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

/// Whether `p` holds this guard's root probes.
fn is_repo_root(p: &Path) -> bool {
    ROOT_PROBES.iter().all(|f| p.join(f).is_file())
}

/// Parse sentinels: the exact header lines of the three AGENTS.md tables
/// this guard consumes. Locating tables by header keeps the guard immune to
/// surrounding prose edits and fails loudly (not vacuously) if a table is
/// renamed or removed.
const CLASSIFICATION_HEADER: &str = "| Target | Group | Execution notes |";
const IGNORED_HEADER: &str = "| Test | Why it is ignored |";
const TEST_STRUCTURE_HEADER: &str = "| Location | What it tests |";

const GROUP_BINARIES: &str = "compiled-binaries";
const GROUP_SCRIPTS: &str = "repo-scripts";
const GROUP_REAL_CLAUDE: &str = "real-claude";
const GROUP_LIBRARY: &str = "library-level";
const GROUP_HELPER: &str = "helper";
const KNOWN_GROUPS: [&str; 5] = [
    GROUP_BINARIES,
    GROUP_SCRIPTS,
    GROUP_REAL_CLAUDE,
    GROUP_LIBRARY,
    GROUP_HELPER,
];

/// The two ways a test target locates a binary cargo just built (AGENTS.md
/// §"Where the build output lands"): the env var cargo sets per `[[bin]]`,
/// or the test binary's own on-disk location.
const BIN_LOCATORS: [&str; 2] = ["CARGO_BIN_EXE_", "current_exe"];

/// Source-text markers of a subprocess spawned by the test itself. Narrow
/// by design: `use std::process::Command` / `Command::new` catch every real
/// spawn while ignoring same-name types that spawn nothing (`clap::
/// CommandFactory`, the CLI's own `Command` enum).
const SPAWN_MARKERS: [&str; 2] = ["process::Command", "Command::new"];

/// This guard cannot meaningfully scan its own dependency shape — its
/// source names every marker above by construction. Every other rule in
/// this file applies to it like any other library-level row.
const SELF_TARGET: &str = "docs_test_classification";

fn read_repo(rel: &str) -> String {
    fs::read_to_string(repo_root().join(rel))
        .unwrap_or_else(|e| panic!("reading {rel} from the repo root: {e}"))
}

fn agents_md() -> String {
    read_repo("AGENTS.md")
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

/// The classification table as `target -> (group, notes)`, failing on
/// malformed rows, unknown groups, or a target classified twice.
fn classification() -> BTreeMap<String, (String, String)> {
    let rows = table_rows(&agents_md(), CLASSIFICATION_HEADER);
    assert!(
        !rows.is_empty(),
        "the classification table ({CLASSIFICATION_HEADER}) has no rows"
    );
    let mut map = BTreeMap::new();
    for row in rows {
        assert_eq!(
            row.len(),
            3,
            "classification rows must be `| Target | Group | Execution notes |` — a \
             literal `|` inside a cell breaks the parse: {row:?}"
        );
        let target = row[0].trim_matches('`').to_string();
        let (group, notes) = (row[1].clone(), row[2].clone());
        assert!(
            KNOWN_GROUPS.contains(&group.as_str()),
            "unknown group `{group}` for `{target}` — known groups: {KNOWN_GROUPS:?}"
        );
        assert!(
            !target.is_empty()
                && target
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_'),
            "target cells must be bare stems like `binary_e2e` (no path, no `::`): \
             `{target}`"
        );
        if let Some((old, _)) = map.get(&target) {
            panic!(
                "`{target}` is classified twice ({old} and {group}) — the claim is \
                 exactly one row per target"
            );
        }
        map.insert(target, (group, notes));
    }
    map
}

fn targets_in(classification: &BTreeMap<String, (String, String)>, group: &str) -> Vec<String> {
    classification
        .iter()
        .filter(|(_, (g, _))| g == group)
        .map(|(t, _)| t.clone())
        .collect()
}

/// Cargo's autodiscovered integration-test targets: one per top-level
/// `tests/*.rs` file. The assumptions behind this enumeration are pinned by
/// `cargo_autodiscovery_assumptions_hold`.
fn filesystem_targets() -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for entry in fs::read_dir(repo_root().join("tests")).expect("reading tests/") {
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

/// One target's compilation unit — the target file plus every file it pulls
/// in with `mod name;` (honoring `#[path = "…"]`), recursively — mapped as
/// repo-relative path to content. This is the union cargo actually compiles
/// for the target, so it is where dependency markers live: e.g.
/// `config_startup_errors` locates the binary only through its
/// `config_error_helpers` module.
fn compilation_unit(target: &str) -> BTreeMap<String, String> {
    let root = repo_root();
    let mut unit: BTreeMap<String, String> = BTreeMap::new();
    let mut queue = vec![format!("tests/{target}.rs")];
    while let Some(rel) = queue.pop() {
        if unit.contains_key(&rel) {
            continue;
        }
        let content = read_repo(&rel);
        for dep in mod_includes(&root, &rel, &content) {
            if !unit.contains_key(&dep) {
                queue.push(dep);
            }
        }
        unit.insert(rel, content);
    }
    unit
}

/// Repo-relative paths of the `mod name;` declarations in `content`.
/// Inline `mod name { … }` blocks (no semicolon) declare no file and are
/// ignored. `root` anchors the probe that decides between the two file
/// layouts, since `base` is repo-relative, not process-relative.
fn mod_includes(root: &Path, rel: &str, content: &str) -> Vec<String> {
    let base = Path::new(rel).parent().unwrap_or_else(|| Path::new("."));
    let mut out = Vec::new();
    let mut pending_path: Option<String> = None;
    for line in content.lines() {
        let t = line.trim();
        if t.starts_with("#[") {
            if let Some(p) = parse_path_attr(t) {
                pending_path = Some(p);
            }
            continue;
        }
        if let Some(name) = mod_name(t) {
            let file = match pending_path.take() {
                Some(p) => base.join(p),
                None => default_mod_path(root, base, &name),
            };
            out.push(file.display().to_string());
        } else if !t.is_empty() && !t.starts_with("//") {
            pending_path = None;
        }
    }
    out
}

/// The declared name of a `mod name;` line (any visibility), else `None`.
fn mod_name(t: &str) -> Option<String> {
    let body = t
        .strip_prefix("pub(crate) mod ")
        .or_else(|| t.strip_prefix("pub mod "))
        .or_else(|| t.strip_prefix("mod "))?;
    let name = body.strip_suffix(';')?;
    (!name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'))
        .then(|| name.to_string())
}

/// The quoted path of a `#[path = "…"]` attribute line, if it is one.
fn parse_path_attr(t: &str) -> Option<String> {
    let rest = t.strip_prefix("#[path")?;
    if !rest.starts_with([' ', '=']) {
        return None;
    }
    let q1 = rest.find('"')? + 1;
    let q2 = rest[q1..].find('"')? + q1;
    Some(rest[q1..q2].to_string())
}

/// Where a plain `mod name;` resolves: `name.rs` beside the includer, else
/// `name/main.rs` — probed under `root`, not the process working directory.
fn default_mod_path(root: &Path, parent: &Path, name: &str) -> PathBuf {
    let flat = parent.join(format!("{name}.rs"));
    if root.join(&flat).is_file() {
        flat
    } else {
        parent.join(name).join("main.rs")
    }
}

/// First occurrence of any needle in the unit's non-comment source, as
/// `(file, needle, 1-based line)`. `//` lines are skipped so prose mentions
/// of a marker can neither satisfy a presence check nor trip an absence
/// one; real code never hides behind a comment.
fn find_in_unit<'a>(
    unit: &BTreeMap<String, String>,
    needles: &[&'a str],
) -> Option<(String, &'a str, usize)> {
    for (file, content) in unit {
        for (i, line) in content.lines().enumerate() {
            if line.trim_start().starts_with("//") {
                continue;
            }
            for needle in needles {
                if line.contains(needle) {
                    return Some((file.clone(), needle, i + 1));
                }
            }
        }
    }
    None
}

/// `(target, fn)` for every `#[ignore…]`-attributed function in every
/// target's compilation unit — the set the Ignored table must match.
/// Attribute lines inside comments (`//`, `///`, `//!`) never start a line
/// here, so prose mentions are naturally excluded.
fn ignored_tests_in_tree(targets: &BTreeSet<String>) -> BTreeSet<(String, String)> {
    let mut out = BTreeSet::new();
    for target in targets {
        for (file, content) in compilation_unit(target) {
            let lines: Vec<&str> = content.lines().collect();
            for (i, line) in lines.iter().enumerate() {
                if !line.trim_start().starts_with("#[ignore") {
                    continue;
                }
                let fn_name = lines[i + 1..]
                    .iter()
                    .take(8)
                    .find_map(|l| fn_name_of(l.trim()));
                match fn_name {
                    Some(name) => {
                        out.insert((target.clone(), name));
                    }
                    None => panic!(
                        "`#[ignore` at {file}:{} is not followed by a fn within 8 lines — \
                         extend tests/{SELF_TARGET}.rs for the new attribute shape",
                        i + 1
                    ),
                }
            }
        }
    }
    out
}

/// The function name of a `fn name(…)` / `pub fn name(…)` line, if it is one.
fn fn_name_of(t: &str) -> Option<String> {
    let body = t
        .strip_prefix("pub fn ")
        .or_else(|| t.strip_prefix("fn "))?;
    let name = body.split('(').next().unwrap_or("").trim();
    (!name.is_empty()).then(|| name.to_string())
}

#[test]
fn classification_table_matches_the_tests_tree_exactly() {
    let classification = classification();
    let targets = filesystem_targets();
    let classified: BTreeSet<_> = classification.keys().cloned().collect();

    let unclassified: Vec<_> = targets.difference(&classified).collect();
    assert!(
        unclassified.is_empty(),
        "tests/*.rs targets with no classification row — AGENTS.md §\"Execution \
         requirements\" claims every target appears exactly once; classify them in \
         the commit that adds them: {unclassified:?}"
    );

    let stale: Vec<_> = classified.difference(&targets).collect();
    assert!(
        stale.is_empty(),
        "classification rows for targets that no longer exist — remove the rows: \
         {stale:?}"
    );
}

#[test]
fn cargo_autodiscovery_assumptions_hold() {
    let manifest: toml::Value =
        toml::from_str(&read_repo("Cargo.toml")).expect("parsing Cargo.toml");
    assert!(
        manifest.get("test").is_none(),
        "an explicit [[test]] section desyncs this guard's top-level tests/*.rs \
         enumeration from cargo's real target set — extend tests/{SELF_TARGET}.rs \
         when adding one"
    );
    let autotests = manifest.get("autotests").and_then(toml::Value::as_bool);
    assert!(
        autotests != Some(false),
        "`autotests = false` disables the autodiscovery this guard enumerates with — \
         extend tests/{SELF_TARGET}.rs"
    );
    let tests_dir = repo_root().join("tests");
    for entry in fs::read_dir(&tests_dir).expect("reading tests/") {
        let path = entry.expect("readdir entry in tests/").path();
        if path.is_dir() {
            assert!(
                !path.join("main.rs").is_file(),
                "{} would be an autodiscovered target invisible to the top-level \
                 tests/*.rs enumeration — use a top-level file, or extend \
                 tests/{SELF_TARGET}.rs",
                path.join("main.rs").display()
            );
        }
    }
}

#[test]
fn compiled_binary_rows_locate_a_built_binary() {
    let classification = classification();
    for target in targets_in(&classification, GROUP_BINARIES) {
        let unit = compilation_unit(&target);
        assert!(
            find_in_unit(&unit, &BIN_LOCATORS).is_some(),
            "`{target}` is classified {GROUP_BINARIES} but its compilation unit ({}) \
             references neither `CARGO_BIN_EXE_*` nor `current_exe` — either it spawns \
             no built binary (reclassify the row) or it locates the binary a new way \
             (extend BIN_LOCATORS)",
            unit.keys().cloned().collect::<Vec<_>>().join(", ")
        );
    }
}

#[test]
fn repo_script_rows_spawn_without_built_binaries() {
    let classification = classification();
    for target in targets_in(&classification, GROUP_SCRIPTS) {
        let unit = compilation_unit(&target);
        if let Some((file, marker, line)) = find_in_unit(&unit, &BIN_LOCATORS) {
            panic!(
                "`{target}` is classified {GROUP_SCRIPTS} (needs neither compiled \
                 binary) but {file}:{line} references `{marker}` — reclassify the row, \
                 or move the binary-spawning case to a compiled-binaries target"
            );
        }
        assert!(
            find_in_unit(&unit, &SPAWN_MARKERS).is_some()
                || find_in_unit(&unit, &["PtySpawner"]).is_some(),
            "`{target}` is classified {GROUP_SCRIPTS}, whose contract is spawning \
             stub/script children (`std::process::Command` or the library's \
             `PtySpawner`), but its compilation unit spawns nothing — reclassify it \
             as library-level"
        );
    }
}

#[test]
fn real_claude_rows_probe_the_installed_claude() {
    let classification = classification();
    for target in targets_in(&classification, GROUP_REAL_CLAUDE) {
        let unit = compilation_unit(&target);
        assert!(
            find_in_unit(&unit, &BIN_LOCATORS).is_none(),
            "`{target}` is classified {GROUP_REAL_CLAUDE} but its compilation unit \
             references a built-binary locator — a real-claude row depends on the \
             installed `claude` only; reclassify it"
        );
        assert!(
            find_in_unit(&unit, &SPAWN_MARKERS).is_some(),
            "`{target}` is classified {GROUP_REAL_CLAUDE} but its compilation unit \
             spawns nothing — the group's contract is an argv/`--version` probe of \
             the installed binary"
        );
        assert!(
            find_in_unit(&unit, &["\"claude\""]).is_some(),
            "`{target}` is classified {GROUP_REAL_CLAUDE} but never names the \
             `claude` binary it is supposed to probe"
        );
    }
}

#[test]
fn library_level_rows_spawn_no_process() {
    let classification = classification();
    for target in targets_in(&classification, GROUP_LIBRARY) {
        if target == SELF_TARGET {
            continue; // this guard's source names every marker by construction
        }
        let unit = compilation_unit(&target);
        if let Some((file, marker, line)) = find_in_unit(&unit, &SPAWN_MARKERS) {
            // The one sanctioned shape: spawning confined to `#[ignore]`d
            // tests, with the carve-out declared in the row's notes (the
            // claude_contracts credentials-gated live probes).
            let (_, notes) = &classification[&target];
            let has_ignored = find_in_unit(&unit, &["#[ignore"]).is_some();
            assert!(
                has_ignored && notes.contains("#[ignore]"),
                "`{target}` is classified {GROUP_LIBRARY} (\"spawn no process\") but \
                 {file}:{line} contains `{marker}`. A library-level row may spawn only \
                 behind `#[ignore]`d tests with the carve-out declared in its \
                 Execution-notes cell — otherwise reclassify the row"
            );
        }
    }
}

#[test]
fn helper_rows_are_modules_shared_by_classified_targets() {
    let classification = classification();
    for helper in targets_in(&classification, GROUP_HELPER) {
        let helper_file = format!("tests/{helper}.rs");
        let includers: Vec<String> = classification
            .iter()
            .filter(|(t, (g, _))| {
                g != GROUP_HELPER && compilation_unit(t).contains_key(&helper_file)
            })
            .map(|(t, _)| t.clone())
            .collect();
        assert!(
            !includers.is_empty(),
            "`{helper}` is a helper row but no classified target `mod`-includes \
             {helper_file} — a helper nothing includes is not a helper; classify it \
             in a real group"
        );
    }
}

#[test]
fn ignored_table_matches_the_ignored_tests_in_the_tree() {
    let doc: BTreeSet<(String, String)> = table_rows(&agents_md(), IGNORED_HEADER)
        .iter()
        .map(|row| {
            let cell = row
                .first()
                .expect("Ignored-table row has a Test cell")
                .trim_matches('`');
            let (target, fn_name) = cell
                .split_once("::")
                .unwrap_or_else(|| panic!("Ignored-table cell {cell:?} is not `target::fn`"));
            (target.to_string(), fn_name.to_string())
        })
        .collect();
    let tree = ignored_tests_in_tree(&filesystem_targets());
    let undocumented: Vec<_> = tree.difference(&doc).collect();
    let stale: Vec<_> = doc.difference(&tree).collect();
    assert!(
        undocumented.is_empty() && stale.is_empty(),
        "AGENTS.md's Ignored table and the `#[ignore]`d tests in the tree disagree — \
         undocumented `#[ignore]`d tests (add rows): {undocumented:?}; stale rows \
         (drop them): {stale:?}"
    );
}

#[test]
fn test_structure_table_lists_every_target() {
    let listed: BTreeSet<String> = table_rows(&agents_md(), TEST_STRUCTURE_HEADER)
        .iter()
        .filter_map(|row| {
            let cell = row.first()?.trim_matches('`');
            cell.strip_prefix("tests/")?
                .strip_suffix(".rs")
                .map(str::to_string)
        })
        .collect();
    let targets = filesystem_targets();
    let missing: Vec<_> = targets.difference(&listed).collect();
    let extra: Vec<_> = listed.difference(&targets).collect();
    assert!(
        missing.is_empty() && extra.is_empty(),
        "AGENTS.md §\"Test structure\" must list every tests/*.rs target — missing \
         rows: {missing:?}; rows for nonexistent targets: {extra:?}"
    );
}

#[test]
fn repo_root_resolution_follows_the_candidate_chain() {
    let live = repo_root();
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
    // 2. the runtime manifest outranks the baked value
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
    // extraction's parent, or a typo'd path, has.
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
    let live = repo_root();
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
