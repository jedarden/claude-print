//! Drift-pin for AGENTS.md's §"Build commands" test-invocation contract
//! (bead claudepr-a34a6399).
//!
//! The section carries one prohibition that is operational contract, not
//! style advice: the wildcard `--test` selector — quoted or bare — must
//! never be prescribed, and `cargo test --tests` is the sanctioned
//! all-targets form. The wildcard resolves fine under stock Cargo (glob
//! target selection), but the fleet path destroys it:
//! `~/.local/bin/cargo-remote` flattens the invocation into one string
//! (`TEST_ARGS="${*:2}"` drops the quoting) and the `rust-verify` pod
//! re-expands that string unquoted, so the literal `*` glob-expands to the
//! clone's top-level files — `error: unexpected argument 'Cargo.toml'
//! found` (verified 2026-09-24, claudepr-07a82368). A clean checkout is
//! exactly the case that offloads to iad-ci, so the wildcard fails
//! precisely where a fleet agent following the docs would run it — and
//! precisely where no behavioral test could catch the regression, since
//! the form works under stock Cargo. Only a documentation pin can.
//!
//! The sibling guard `tests/docs_build_layout.rs` (claudepr-9c5f098f)
//! pins §"Where the build output lands" and stops at the artifact table;
//! the commands fence above it — including the prohibition — had no
//! stated pin.
//!
//! Two surfaces, both pinned here:
//!
//! - **§"Build commands" presence** — the bolded prohibition sentence,
//!   the `cargo test --tests` alternative it prescribes in the same
//!   breath, the named single-target selector guidance, and the exact
//!   all-targets line in the section's command fence; and the fence
//!   itself may never prescribe a wildcard selector — the fence is the
//!   copy-paste surface.
//! - **Repo-wide reintroduction scan** — every file of AGENTS.md,
//!   README.md, `docs/`, and `scripts/` is parsed with a selector grammar
//!   (`--test NAME`, `--test=NAME`, surrounding quotes stripped) and any
//!   selector carrying a glob metacharacter (`*`, `?`, `[`) fails the
//!   build. The single sanctioned occurrence is the AGENTS.md prohibition
//!   sentence itself, anchored by content (the exact bolded lead), not by
//!   file or line — so the exemption cannot hide a prescription: a
//!   negative meta-test re-flags the hazardous line the moment the
//!   prohibition lead leaves it.
//!
//! The guard's own failure behavior is pinned the same way as
//! `tests/docs_build_layout.rs` (claudepr-4d967120): always-on negative
//! meta-tests strip each pinned fragment from the live section in memory
//! and plant every hazardous spelling (`'*'`, `"*"`, bare `*`, the
//! `=`-joined form, `?`, a character class) into synthetic scanned files,
//! requiring the owning check to panic naming the drift — the
//! non-vacuity claim is re-proven on every run, not asserted once in a
//! discarded scratch run.
//!
//! Library-level like the rows it guards: reads AGENTS.md, README.md,
//! `docs/`, and `scripts/`; spawns nothing.

use std::fs;
use std::path::{Path, PathBuf};

/// The heading of the section whose test-invocation contract is pinned
/// here. Locating the section by heading keeps the guard immune to
/// surrounding edits and fails loudly (not vacuously) if the section is
/// renamed or removed.
const BUILD_COMMANDS_HEADING: &str = "## Build commands";

/// The prohibition sentence's exact bolded lead — both the fragment whose
/// presence the section pin requires and the content anchor that sanctions
/// the one hazardous-form occurrence the repo-wide scan must tolerate (the
/// prohibition itself).
const PROHIBITION: &str = "**Never use `cargo test --test '*'`**";

/// The alternative the prohibition prescribes in the same sentence.
const PRESCRIBED_ALTERNATIVE: &str = "use `cargo test --tests`";

/// The sanctioned single-target selector the section teaches.
const NAMED_SELECTOR_GUIDANCE: &str = "`cargo test --test <name>`";

/// The runnable all-targets form the section's command fence must carry.
const ALL_TARGETS_COMMAND: &str = "cargo test --tests";

/// Glob metacharacters that make a `--test` selector fragile: each crosses
/// the fleet's TEST_ARGS flatten as a literal and glob-expands in the
/// unquoted re-expansion. A named selector carries none, which is exactly
/// why it is the sanctioned single-target spelling.
const GLOB_METACHARS: [char; 3] = ['*', '?', '['];

/// The repo-wide scan scope: AGENTS.md itself (whose single sanctioned
/// occurrence is the prohibition sentence) plus the operational surfaces
/// the bead names — README.md, every file under `docs/`, every file under
/// `scripts/` (any extension, recursively).
const SCAN_FILES: [&str; 2] = ["AGENTS.md", "README.md"];
const SCAN_DIRS: [&str; 2] = ["docs", "scripts"];

/// Read a repo file, resolving the root from the *runtime*
/// `CARGO_MANIFEST_DIR` (compile-time value as fallback, probe-verified).
/// The compile-time value alone bakes the building checkout's path into
/// the test binary; when the shared target cache reuses that binary from a
/// different extraction — exactly the clean-tree verification NEEDLE
/// re-runs — the read would hit a directory that no longer exists. See
/// `tests/docs_slug_consistency.rs::doc_files` for the full rationale.
fn repo_file(relative: &str) -> String {
    fs::read_to_string(repo_root().join(relative))
        .unwrap_or_else(|e| panic!("read {relative} from the checkout under test: {e}"))
}

fn repo_root() -> PathBuf {
    let root = PathBuf::from(
        std::env::var("CARGO_MANIFEST_DIR")
            .unwrap_or_else(|_| env!("CARGO_MANIFEST_DIR").to_string()),
    );
    assert!(
        root.join("AGENTS.md").is_file(),
        "the checkout under test must hold AGENTS.md at {} — run via cargo \
         from a checkout, never the bare test binary",
        root.display()
    );
    root
}

fn agents_md() -> String {
    repo_file("AGENTS.md")
}

/// The §"Build commands" slice of `doc`: from the heading line up to the
/// next markdown heading *outside any fenced code block*. The section's
/// own bash fence is full of `#`-comment lines, so a fence-blind heading
/// cut would end the section at the first fence comment; the fence state
/// is tracked here for exactly that reason. Pure over `doc` so the
/// negative meta-tests can mutate it in memory. Panics when the heading is
/// gone — a missing section is drift, not a pass.
fn build_commands_section(doc: &str) -> String {
    let lines: Vec<&str> = doc.lines().collect();
    let start = lines
        .iter()
        .position(|l| l.trim() == BUILD_COMMANDS_HEADING)
        .unwrap_or_else(|| {
            panic!(
                "AGENTS.md must keep the {BUILD_COMMANDS_HEADING:?} heading — this \
                 guard scopes its test-invocation pins to that section"
            )
        });
    let mut in_fence = false;
    let mut end = lines.len();
    for (i, line) in lines.iter().enumerate().skip(start + 1) {
        if line.trim_start().starts_with("```") {
            in_fence = !in_fence;
            continue;
        }
        let pounds = line.len() - line.trim_start_matches('#').len();
        let is_heading = !in_fence
            && (1..=6).contains(&pounds)
            && (line[pounds..].is_empty() || line[pounds..].starts_with([' ', '\t']));
        if is_heading {
            end = i;
            break;
        }
    }
    lines[start..end].join("\n")
}

/// The lines inside `text`'s fenced code blocks, in order (delimiters
/// excluded). All fences, not just the first: every fenced block of the
/// section is copy-paste surface.
fn fenced_lines(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut in_fence = false;
    for line in text.lines() {
        if line.trim_start().starts_with("```") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            out.push(line);
        }
    }
    out
}

/// `s` minus one layer of matching surrounding single or double quotes.
/// Partially wrapped tokens (markdown punctuation glued to a closing
/// quote) come back unchanged — the metacharacter check below does not
/// care about the wrapper, only the payload.
fn unquote(s: &str) -> &str {
    let b = s.as_bytes();
    if b.len() >= 2
        && ((b[0] == b'"' && b[b.len() - 1] == b'"') || (b[0] == b'\'' && b[b.len() - 1] == b'\''))
    {
        &s[1..s.len() - 1]
    } else {
        s
    }
}

/// The `--test` selector `line` prescribes, quotes stripped — from
/// `--test NAME` (the next whitespace token) or `--test=NAME` — or `None`
/// when the line carries no selector. Exact-token matching only:
/// `--tests` and `--test-threads=N` are different flags and never parse
/// as selectors.
fn test_selector(line: &str) -> Option<String> {
    let tokens: Vec<&str> = line.split_whitespace().collect();
    for (i, token) in tokens.iter().enumerate() {
        let value = if let Some(v) = token.strip_prefix("--test=") {
            v.to_string()
        } else if *token == "--test" {
            match tokens.get(i + 1) {
                Some(next) => (*next).to_string(),
                // a trailing `--test` with no argument selects nothing
                None => continue,
            }
        } else {
            continue;
        };
        return Some(unquote(&value).to_string());
    }
    None
}

/// The wildcard selector `line` prescribes, when it prescribes one: a
/// `--test` argument carrying any glob metacharacter, quoted or not.
fn hazardous_selector(line: &str) -> Option<String> {
    test_selector(line).filter(|sel| sel.chars().any(|c| GLOB_METACHARS.contains(&c)))
}

/// The §"Build commands" presence contract, checkable against
/// caller-supplied section text (the live section for the always-on test,
/// mutated text for the negative meta-tests). Panics on drift.
fn check_build_commands_section(section: &str) {
    assert!(
        section.contains(PROHIBITION),
        "AGENTS.md §\"Build commands\" must keep the prohibition sentence {PROHIBITION:?} — \
         the wildcard `--test` selector cannot survive the fleet's cargo-remote TEST_ARGS \
         flatten (verified 2026-09-24, claudepr-07a82368), so the prohibition is \
         operational contract; rewording it away is the drift this guard exists to catch"
    );
    assert!(
        section.contains(PRESCRIBED_ALTERNATIVE),
        "the prohibition must keep prescribing its alternative in the same breath \
         ({PRESCRIBED_ALTERNATIVE:?}) — a ban with no replacement command leaves the \
         wildcard as the only all-targets spelling a reader remembers"
    );
    assert!(
        section.contains(NAMED_SELECTOR_GUIDANCE),
        "§\"Build commands\" must keep the named single-target selector guidance \
         ({NAMED_SELECTOR_GUIDANCE:?}) — the sanctioned one-target spelling; without it \
         the section teaches no form between `--tests` and the prohibited wildcard"
    );
    let fence = fenced_lines(section);
    assert!(
        !fence.is_empty() && fence.iter().any(|l| l.trim() == ALL_TARGETS_COMMAND),
        "the section's command fence must carry the exact `{ALL_TARGETS_COMMAND}` line — \
         it is the runnable form of the all-targets prescription; the prose alternative \
         alone is not a command a reader can copy"
    );
    for line in &fence {
        if let Some(selector) = hazardous_selector(line) {
            panic!(
                "the §\"Build commands\" command fence prescribes the wildcard `--test` \
                 selector {selector:?} (fence line {:?}) — the fence is the copy-paste \
                 surface and must carry only sanctioned spellings (`cargo test --tests`, \
                 a named `--test <name>` selector)",
                line.trim()
            );
        }
    }
}

/// The repo-wide reintroduction contract, checkable against
/// caller-supplied `(relative path, content)` pairs (the live scan scope
/// for the always-on test, planted text for the negative meta-tests).
/// Panics on the first hazardous line, naming file, line, and selector.
fn check_no_hazardous_selectors(rel: &str, content: &str) {
    for (i, line) in content.lines().enumerate() {
        if let Some(selector) = hazardous_selector(line) {
            // The one sanctioned occurrence: the AGENTS.md prohibition
            // sentence itself, recognized by its exact bolded lead on the
            // same line — content-anchored, so the exemption can never
            // cover a prescription that merely lives in the same file.
            if rel == "AGENTS.md" && line.contains(PROHIBITION) {
                continue;
            }
            panic!(
                "{rel}:{} prescribes the wildcard `--test` selector {selector:?} \
                 (line: {:?}) — AGENTS.md §\"Build commands\" prohibits that form: the \
                 fleet's cargo-remote TEST_ARGS flatten drops the quoting and the \
                 rust-verify pod re-expands it unquoted, glob-expanding `*` onto the \
                 checkout's top-level files. Use `cargo test --tests` (every target) \
                 or a named `cargo test --test <name>` (one target). The only \
                 sanctioned occurrence is the AGENTS.md prohibition sentence itself; \
                 if this line is a deliberate quotation of it, extend this guard's \
                 exemption in the same commit",
                i + 1,
                line.trim()
            );
        }
    }
}

/// The scan scope as `(repo-relative path, content)` pairs: the two root
/// files plus every regular file under the scan directories, recursively.
fn scanned_files() -> Vec<(String, String)> {
    let root = repo_root();
    let mut out: Vec<(String, String)> = SCAN_FILES
        .iter()
        .map(|rel| (rel.to_string(), repo_file(rel)))
        .collect();
    for dir in SCAN_DIRS {
        walk_files(&root.join(dir), dir, &mut out);
    }
    assert!(
        !out.is_empty(),
        "the scan scope (README.md, docs/, scripts/) came back empty — the guard \
         cannot be vacuously passing off an unread tree"
    );
    out
}

fn walk_files(dir: &Path, rel_prefix: &str, out: &mut Vec<(String, String)>) {
    for entry in fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("reading the scan directory {rel_prefix}/: {e}"))
    {
        let path = entry
            .unwrap_or_else(|e| panic!("readdir entry in {rel_prefix}/: {e}"))
            .path();
        let name = path
            .file_name()
            .expect("readdir entry has a file name")
            .to_string_lossy()
            .to_string();
        let rel = format!("{rel_prefix}/{name}");
        if path.is_dir() {
            walk_files(&path, &rel, out);
        } else {
            let content = fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("reading scan-scope file {rel}: {e}"));
            out.push((rel, content));
        }
    }
}

#[test]
fn build_commands_section_keeps_the_wildcard_prohibition_and_tests_guidance() {
    check_build_commands_section(&build_commands_section(&agents_md()));
}

#[test]
fn operational_docs_and_scripts_never_prescribe_a_wildcard_test_selector() {
    for (rel, content) in scanned_files() {
        check_no_hazardous_selectors(&rel, &content);
    }
}

// ── Negative meta-tests: the guard must FAIL when its inputs rot ─────────────
//
// Every leg mutates the live document in memory — nothing is written to
// disk — and requires the owning check to panic naming the drift, the
// committed non-vacuity pattern of `tests/docs_build_layout.rs`
// (claudepr-4d967120).

/// Run `check` and require it to panic with every fragment of `expected`
/// in the message — the failure must be the drift the mutation plants, not
/// an incidental one. The panic hook is silenced for the caught unwind so
/// expected-failure output never pollutes the log; it is restored before
/// any real assertion here can fire.
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

/// `text` with the single occurrence of `from` replaced by `to`. Panics on
/// any count but one, so a meta-test can never "mutate" an input the live
/// document no longer carries (or carries twice) and silently test
/// something else.
fn replaced_once(text: &str, from: &str, to: &str) -> String {
    let count = text.matches(from).count();
    assert_eq!(
        count, 1,
        "the negative meta-tests mutate {from:?} in the live document — \
         expected exactly one occurrence, found {count}"
    );
    text.replacen(from, to, 1)
}

/// Stripping any pinned fragment of the section contract fails the owning
/// presence check — including the fence degrading to the very form the
/// section prohibits.
#[test]
fn negative_meta_stripped_contract_fragments_fail_the_section_pin() {
    let section = build_commands_section(&agents_md());

    assert_drift(
        || check_build_commands_section(&replaced_once(&section, PROHIBITION, "")),
        &["prohibition sentence"],
    );
    assert_drift(
        || {
            check_build_commands_section(&replaced_once(
                &section,
                PRESCRIBED_ALTERNATIVE,
                "use `cargo test --lib`",
            ))
        },
        &["prescribing its alternative"],
    );
    assert_drift(
        || {
            check_build_commands_section(&replaced_once(
                &section,
                NAMED_SELECTOR_GUIDANCE,
                "`cargo test --lib`",
            ))
        },
        &["named single-target selector guidance"],
    );
    // The exact fence line demoted to a fence comment: the command a
    // reader copies is gone even though the prose still says it.
    assert_drift(
        || {
            check_build_commands_section(&replaced_once(
                &section,
                "\ncargo test --tests\n",
                "\n# cargo test --tests\n",
            ))
        },
        &["command fence"],
    );
    // A hazardous line planted in the fence on a different command line
    // (the sanctioned all-targets line stays, so the presence check above
    // does not fire first) — the copy-paste surface itself goes hazardous.
    assert_drift(
        || {
            check_build_commands_section(&replaced_once(
                &section,
                "\ncargo test --lib\n",
                "\ncargo test --test '*'\n",
            ))
        },
        &["command fence", "wildcard `--test` selector"],
    );
}

/// Every hazardous spelling fails the scan — quoted (single and double),
/// bare, `=`-joined, `?`, and a character class — while the sanctioned
/// spellings (a named selector, the `<name>` placeholder, `--tests`, and
/// cargo's own `--test-threads` flag) never trip the grammar.
#[test]
fn negative_meta_every_hazardous_spelling_fails_the_scan() {
    for (rel, line) in [
        ("scripts/probe-example.sh", "cargo test --test '*'"),
        ("scripts/probe-example.sh", "cargo test --test \"*\""),
        ("scripts/probe-example.sh", "cargo test --test *"),
        ("docs/notes/example.md", "cargo test --test='*'"),
        ("docs/notes/example.md", "    cargo test --test '?' || true"),
        (
            "docs/notes/example.md",
            "run `cargo test --test [st]*` today",
        ),
        ("README.md", "cargo test --test '*' -- --nocapture"),
    ] {
        assert_drift(
            || check_no_hazardous_selectors(rel, &format!("echo setup\n{line}\n")),
            &[rel, "wildcard `--test` selector"],
        );
    }

    // The grammar's negative space: none of these may trip.
    check_no_hazardous_selectors(
        "docs/notes/example.md",
        "cargo test --test home_unset\ncargo test --test <name>\n\
         cargo test --tests\ncargo test -- --test-threads=2\n",
    );
}

/// The scan exemption is content-anchored, not file-wide: the hazardous
/// line passes only while it carries the exact prohibition lead, and is
/// re-flagged the moment that lead leaves it — the exemption can never
/// hide a prescription elsewhere in AGENTS.md.
#[test]
fn negative_meta_prohibition_lead_anchors_the_scan_exemption() {
    let doc = agents_md();
    // Sanity for the anchor itself: the live document's one hazardous line
    // is the sanctioned prohibition and passes.
    check_no_hazardous_selectors("AGENTS.md", &doc);

    // The quoted form remains on the line, the bolded prohibition lead is
    // gone: no longer the sanctioned sentence, must be flagged.
    let mutated = replaced_once(&doc, PROHIBITION, "`cargo test --test '*'`");
    assert_drift(
        || check_no_hazardous_selectors("AGENTS.md", &mutated),
        &["AGENTS.md", "wildcard `--test` selector"],
    );
}
