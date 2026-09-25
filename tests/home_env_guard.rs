//! Standing guard for the HOME call-site discipline in
//! `docs/notes/home-handling-strategy.md`.
//!
//! The strategy doc makes two enforceable claims: "Production modules must
//! call `get_home()` instead of reading HOME directly", and a call-site
//! table fixing which functions touch HOME resolution. Both were previously
//! enforced only by one-time audit beads (claudepr-28ff25d3,
//! claudepr-3b545bc5, claudepr-f2feb6b0, claudepr-4664fb1e), and the queue
//! of HOME-consistency beads shows the contract regressed between audits —
//! by the time this guard landed the table itself had already drifted (no
//! rows for the `main.rs` preflight or the `Session::run`/`run_pooled`
//! entry validations, and the poller row named `projects_dir_for_cwd()`
//! while the call lives in `projects_dir_for()`). This guard re-derives
//! the discipline from the tree on every run (bead claudepr-bdfac6f7):
//!
//! 1. `src/` outside `util.rs` contains no direct HOME environment read
//!    (`var("HOME")` / `var_os("HOME")`) and no `home_dir(` bypass.
//! 2. `src/util.rs` performs exactly one raw read — the strategy doc calls
//!    `get_home` the "sole production environment read", so a second one
//!    is drift, not growth.
//! 3. The documented call-site table matches the actual `get_home()`
//!    production call sites: every row names a function that exists in the
//!    module it names, the modules with production call sites are exactly
//!    the table's modules (plus `util.rs`), and a per-module snapshot of
//!    reference counts pins additions and removals — a new call site fails
//!    until the table and the snapshot move together in one commit.
//!
//! Runtime behavior (error shapes, exit codes) is pinned separately by
//! `tests/home_unset.rs` (and claudepr-f6e6aca6); this file pins
//! call-site discipline only.
//!
//! Library-level like the rows it guards: reads `src/` and the strategy
//! doc, spawns nothing, mutates no environment.
//!
//! Scanner shape, matching this tree's conventions (verified when the
//! guard was written): only `//` / `///` comments (no `/* */`), no `'"'`
//! or `'{'` char literals, and multi-line literals are raw strings only
//! (the TOML/JSON fixtures under `#[cfg(test)]`), which the scanner tracks
//! by their `#`-count so their quotes and braces never leak into code.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

/// Repository root, baked at compile time so the guard always reads the
/// extraction it runs from (same convention as `docs_test_classification`).
const REPO: &str = env!("CARGO_MANIFEST_DIR");

const STRATEGY_DOC: &str = "docs/notes/home-handling-strategy.md";

/// Parse sentinel: the exact header line of the strategy doc's call-site
/// table. Locating the table by header keeps the guard immune to
/// surrounding prose edits and fails loudly (not vacuously) if the table
/// is renamed or removed.
const TABLE_HEADER: &str = "| Module | Function | HOME behavior |";

/// The one module allowed to read the HOME environment variable directly.
const HOME_OWNER: &str = "util.rs";

/// Direct HOME reads, in the spellings that bypass `crate::util::get_home`.
/// `var("HOME")` and `var_os("HOME")` cover `std::env::var("HOME")`,
/// `env::var_os("HOME")`, and `use std::env::var; var("HOME")`;
/// `home_dir(` covers `std::env::home_dir` / `dirs::home_dir` /
/// `home::home_dir`, which on Unix read `$HOME` just as directly while
/// skipping `get_home`'s validation. Identifier-boundary checked on the
/// left, so `some_var("HOME")` (an unrelated function) and
/// `resolve_home_dir(` (an unrelated helper) do not trip.
const BANNED_READS: [&str; 3] = ["var(\"HOME\")", "var_os(\"HOME\")", "home_dir("];

/// Per-module count of `get_home` identifier occurrences on production
/// code — non-comment, non-`use` lines outside `#[cfg(test)]` regions.
/// This is the call-site inventory the strategy doc's table describes: a
/// new or removed call site changes one of these numbers and fails the
/// guard until the table in `docs/notes/home-handling-strategy.md` and
/// this snapshot move together in the same commit.
const EXPECTED_GET_HOME_REFERENCES: &[(&str, usize)] = &[
    // main(): preflight validation before dispatch
    ("main.rs", 1),
    // Config::default_path(), the XDG_CONFIG_HOME fallback
    ("config.rs", 1),
    // resolve_stop_info() passes the resolver in; resolve_stop_info_with
    // names it as its closure parameter and calls it lazily;
    // derive_transcript_path() and projects_dir_for() each resolve once
    ("poller.rs", 5),
    // Session::run() and Session::run_pooled() entry validation,
    // pretrust_cwd()
    ("session.rs", 3),
];

fn read_repo(rel: &str) -> String {
    fs::read_to_string(Path::new(REPO).join(rel))
        .unwrap_or_else(|e| panic!("reading {rel} from the repo root: {e}"))
}

/// Sorted `src/*.rs` file names, so failure messages are deterministic.
fn src_file_names() -> Vec<String> {
    let dir = Path::new(REPO).join("src");
    let mut names: Vec<String> = fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("reading src/: {e}"))
        .map(|entry| {
            entry
                .unwrap_or_else(|e| panic!("readdir entry in src/: {e}"))
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .filter(|name| name.ends_with(".rs"))
        .collect();
    names.sort();
    assert!(!names.is_empty(), "no sources found under src/");
    names
}

/// Literal-scanning state, carried across lines because raw strings in
/// this tree (TOML/JSON test fixtures) span lines.
#[derive(Default)]
struct Scan {
    /// Inside a normal `"…"` literal (escapes honored).
    in_string: bool,
    /// Inside a `r#"…"#` / `br#"…"#` literal with this many `#`s (no
    /// escapes; only `"###…` with the same count closes it).
    raw_hashes: Option<usize>,
}

/// If the `"` at `quote_at` opens a raw literal (`r#"` / `br#"` with any
/// number of `#`s, including none), how many `#`s it uses. The `r` (or
/// `br`) must start after an identifier boundary so an identifier merely
/// ending in those letters cannot masquerade as a raw opener.
fn raw_opener_hashes(line: &[u8], quote_at: usize) -> Option<usize> {
    let mut back = quote_at;
    let mut hashes = 0;
    while back > 0 && line[back - 1] == b'#' {
        hashes += 1;
        back -= 1;
    }
    if back == 0 || line[back - 1] != b'r' {
        return None;
    }
    back -= 1;
    if back > 0 && line[back - 1] == b'b' {
        back -= 1;
    }
    if back > 0 && is_ident_byte(line[back - 1]) {
        return None;
    }
    Some(hashes)
}

/// One line as a scanner sees it: `code` is the line with `//` comments
/// removed but string literals kept verbatim — the banned read patterns
/// quote `"HOME"`, so they must match inside literals — and `braces` is
/// the net brace delta of real code only, so fixture JSON/TOML braces
/// never affect `#[cfg(test)]` region accounting.
#[derive(Debug, Default)]
struct StrippedLine {
    code: String,
    braces: i64,
}

fn strip_line(line: &str, scan: &mut Scan) -> StrippedLine {
    let bytes = line.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut braces = 0i64;
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if let Some(hashes) = scan.raw_hashes {
            // Raw literal: no escapes; only the matching `"###…` closes.
            // The `#`s must actually be present — a content line merely
            // ending in `"` must not satisfy the check vacuously.
            let closes = c == b'"'
                && bytes.len() >= i + 1 + hashes
                && bytes[i + 1..i + 1 + hashes].iter().all(|&b| b == b'#');
            if closes {
                scan.raw_hashes = None;
                out.push(b'"');
                out.extend(std::iter::repeat_n(b'#', hashes));
                i += 1 + hashes;
            } else {
                out.push(c);
                i += 1;
            }
        } else if scan.in_string {
            if c == b'\\' && i + 1 < bytes.len() {
                // Escaped character; a trailing backslash (line
                // continuation) leaves the string open across lines.
                out.push(c);
                out.push(bytes[i + 1]);
                i += 2;
            } else {
                if c == b'"' {
                    scan.in_string = false;
                }
                out.push(c);
                i += 1;
            }
        } else if c == b'"' {
            scan.raw_hashes = raw_opener_hashes(bytes, i);
            scan.in_string = scan.raw_hashes.is_none();
            out.push(b'"');
            i += 1;
        } else if c == b'/' && bytes.get(i + 1) == Some(&b'/') {
            break;
        } else {
            if c == b'{' {
                braces += 1;
            } else if c == b'}' {
                braces -= 1;
            }
            out.push(c);
            i += 1;
        }
    }
    StrippedLine {
        code: String::from_utf8_lossy(&out).into_owned(),
        braces,
    }
}

/// All non-blank code lines with 1-based line numbers — `#[cfg(test)]`
/// regions included, because the direct-read ban is universal in `src/`:
/// the strategy doc lets tests set, remove, or redirect HOME, never read
/// it raw. Integration suites under `tests/` are outside this scan.
fn code_lines(text: &str) -> Vec<(usize, String)> {
    let mut scan = Scan::default();
    text.lines()
        .enumerate()
        .map(|(i, line)| (i + 1, strip_line(line, &mut scan)))
        .filter(|(_, stripped)| !stripped.code.trim().is_empty())
        .map(|(i, stripped)| (i, stripped.code))
        .collect()
}

/// Production code lines: [`code_lines`] minus `#[cfg(test)]` items. A
/// `#[cfg(test)]` attribute opens a skip that ends when the item's braces
/// (counted only in real code) balance back to zero. Every `#[cfg(test)]`
/// item in this tree is braced; a brace-less item (`#[cfg(test)] use …;`)
/// also closes the skip via its terminating semicolon, and an unterminated
/// region is a loud panic, not a silently wrong count.
fn production_code_lines(text: &str, file: &str) -> Vec<(usize, String)> {
    let mut scan = Scan::default();
    let mut out = Vec::new();
    let mut skipping = false;
    let mut skip_depth = 0i64;
    let mut skip_opened = false;
    for (i, raw) in text.lines().enumerate() {
        let stripped = strip_line(raw, &mut scan);
        if skipping {
            skip_depth += stripped.braces;
            if !skip_opened && skip_depth > 0 {
                skip_opened = true;
            }
            let closed = skip_opened && skip_depth <= 0;
            let braceless_item = !skip_opened && stripped.code.trim().ends_with(';');
            if closed || braceless_item {
                skipping = false;
            }
            continue;
        }
        if stripped.code.trim() == "#[cfg(test)]" {
            skipping = true;
            skip_depth = 0;
            skip_opened = false;
            continue;
        }
        if !stripped.code.trim().is_empty() {
            out.push((i + 1, stripped.code));
        }
    }
    assert!(
        !skipping,
        "src/{file}: #[cfg(test)] region never closed — the scanner's \
            brace accounting is off for this file"
    );
    out
}

fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// Whether `pattern` (starting with an identifier, e.g. `var("HOME")` or
/// `home_dir(`) occurs as a call in its own right — the byte before it
/// must not continue an identifier.
fn contains_call(code: &str, pattern: &str) -> bool {
    let mut from = 0;
    while let Some(rel) = code[from..].find(pattern) {
        let start = from + rel;
        let boundary_ok = start == 0 || !is_ident_byte(code.as_bytes()[start - 1]);
        if boundary_ok {
            return true;
        }
        from = start + pattern.len();
    }
    false
}

/// Occurrences of `ident` as a standalone identifier (boundaries on both
/// sides), so `get_home` matches but `get_home_at` / `my_get_home` do not.
fn count_ident(code: &str, ident: &str) -> usize {
    let mut count = 0;
    let mut from = 0;
    while let Some(rel) = code[from..].find(ident) {
        let start = from + rel;
        let end = start + ident.len();
        let before_ok = start == 0 || !is_ident_byte(code.as_bytes()[start - 1]);
        let after_ok = code.as_bytes().get(end).is_none_or(|b| !is_ident_byte(*b));
        if before_ok && after_ok {
            count += 1;
        }
        from = end;
    }
    count
}

/// `(module, function-path)` rows of the strategy doc's call-site table,
/// located by the [`TABLE_HEADER`] sentinel.
fn strategy_rows() -> Vec<(String, String)> {
    let doc = read_repo(STRATEGY_DOC);
    let start = doc
        .lines()
        .position(|l| l.trim() == TABLE_HEADER)
        .unwrap_or_else(|| {
            panic!(
                "{STRATEGY_DOC} no longer contains the table header \
                    {TABLE_HEADER:?} — this guard locates the call-site \
                    table by that sentinel line"
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
        let module = cells[0].trim().trim_matches('`').to_string();
        let function = cells[1].trim().trim_matches('`').to_string();
        assert!(
            module.ends_with(".rs"),
            "{STRATEGY_DOC}: call-site table row {module:?} does not name \
                a src/ module — fix the table"
        );
        assert!(
            !function.is_empty(),
            "{STRATEGY_DOC}: call-site table row for {module} has an empty \
                Function cell — fix the table"
        );
        rows.push((module, function));
    }
    assert!(
        !rows.is_empty(),
        "{STRATEGY_DOC}: call-site table has no rows"
    );
    rows
}

#[test]
fn direct_home_env_reads_stay_confined_to_util() {
    let mut violations = Vec::new();
    for name in src_file_names() {
        if name == HOME_OWNER {
            continue;
        }
        let text = read_repo(&format!("src/{name}"));
        for (line_no, code) in code_lines(&text) {
            for pattern in BANNED_READS {
                if contains_call(&code, pattern) {
                    violations.push(format!(
                        "src/{name}:{line_no}: {pattern} in: {}",
                        code.trim()
                    ));
                }
            }
        }
    }
    assert!(
        violations.is_empty(),
        "direct HOME environment read(s) outside src/{HOME_OWNER} — the \
            strategy doc requires production modules to call \
            crate::util::get_home() instead (docs/notes/home-handling-strategy.md):\n  {}",
        violations.join("\n  ")
    );
}

#[test]
fn util_performs_exactly_one_raw_home_read() {
    let text = read_repo("src/util.rs");
    let mut var_os_reads = 0usize;
    let mut others = Vec::new();
    for (line_no, code) in code_lines(&text) {
        if contains_call(&code, "var_os(\"HOME\")") {
            var_os_reads += 1;
        }
        for pattern in ["var(\"HOME\")", "home_dir("] {
            if contains_call(&code, pattern) {
                others.push(format!("src/util.rs:{line_no}: {pattern}"));
            }
        }
    }
    assert!(
        others.is_empty(),
        "src/util.rs picked up a HOME read outside get_home()'s single \
            var_os (docs/notes/home-handling-strategy.md calls get_home \
            the sole production environment read):\n  {}",
        others.join("\n  ")
    );
    assert_eq!(
        var_os_reads, 1,
        "src/util.rs must perform exactly one raw HOME read (the \
            var_os(\"HOME\") inside get_home); found {var_os_reads} — the \
            strategy doc's \"sole production environment read\" claim no \
            longer holds"
    );
}

#[test]
fn documented_call_sites_match_the_code() {
    let rows = strategy_rows();

    // Actual per-module production reference counts.
    let mut actual: BTreeMap<String, usize> = BTreeMap::new();
    for name in src_file_names() {
        if name == HOME_OWNER {
            continue;
        }
        let text = read_repo(&format!("src/{name}"));
        let count = production_code_lines(&text, &name)
            .iter()
            .filter(|(_, code)| {
                // A `use` item imports the name without calling it.
                !code.trim_start().starts_with("use ") && count_ident(code, "get_home") > 0
            })
            .count();
        if count > 0 {
            actual.insert(name, count);
        }
    }
    let expected: BTreeMap<String, usize> = EXPECTED_GET_HOME_REFERENCES
        .iter()
        .map(|(module, count)| ((*module).to_string(), *count))
        .collect();
    assert_eq!(
        actual, expected,
        "get_home() production call sites drifted from the pinned snapshot \
            — update the call-site table in {STRATEGY_DOC} and \
            EXPECTED_GET_HOME_REFERENCES in tests/home_env_guard.rs \
            together, in the same commit"
    );

    // Module sets: every table module except util.rs (whose call site is
    // the definition itself, exempt from the count above) has production
    // call sites, and every module with call sites has a row.
    let table_modules: BTreeSet<String> = rows.iter().map(|(m, _)| m.clone()).collect();
    assert!(
        table_modules.contains(HOME_OWNER),
        "{STRATEGY_DOC}: the call-site table lost its {HOME_OWNER} \
            get_home() row — that row anchors the whole table"
    );
    let mut allowed = actual.keys().cloned().collect::<BTreeSet<String>>();
    allowed.insert(HOME_OWNER.to_string());
    let code_modules: BTreeSet<String> = actual.keys().cloned().collect();
    let stale_rows: Vec<String> = table_modules.difference(&allowed).cloned().collect();
    assert!(
        stale_rows.is_empty(),
        "{STRATEGY_DOC} documents get_home() call sites in modules that \
            have none ({}): remove or update the stale rows",
        stale_rows.join(", ")
    );
    let undocumented: Vec<String> = code_modules.difference(&table_modules).cloned().collect();
    assert!(
        undocumented.is_empty(),
        "modules with get_home() call sites but no row in {STRATEGY_DOC}'s \
            call-site table ({}): document them there and extend \
            EXPECTED_GET_HOME_REFERENCES",
        undocumented.join(", ")
    );

    // Row validity: every documented function still exists as a production
    // `fn` definition in the module the row names.
    for (module, function) in &rows {
        let short = function
            .rsplit("::")
            .next()
            .unwrap_or(function)
            .trim_end_matches("()");
        let text = read_repo(&format!("src/{module}"));
        // `fn name(` or `fn name<…>` — the `<` arm covers generic fns like
        // `resolve_stop_info_with<F>`. A prefix match on a longer name
        // (`fn resolve_stop_info` inside `fn resolve_stop_info_with`) is
        // rejected because the byte after the name is `_`, not `(` or `<`.
        let defined = production_code_lines(&text, module)
            .iter()
            .any(|(_, code)| {
                let Some(pos) = code.find("fn ") else {
                    return false;
                };
                let rest = &code[pos + 3..];
                rest.starts_with(short)
                    && rest[short.len()..]
                        .strip_prefix(|c: char| c == '(' || c == '<')
                        .is_some()
            });
        assert!(
            defined,
            "{STRATEGY_DOC} documents `{function}` in src/{module}, but no \
                production `fn {short}` is defined there — the table has \
                drifted from the code; update both together with \
                EXPECTED_GET_HOME_REFERENCES"
        );
    }
}
