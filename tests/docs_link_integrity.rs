//! Local link-and-anchor integrity guard for the documentation corpus
//! (bead claudepr-a7754a8e).
//!
//! The documentation cross-references itself constantly — README.md,
//! AGENTS.md, and the notes under `docs/` link each other's files and
//! sections, cite the scripts a reader is told to run, and name the
//! version-pinned fixtures the suites replay — but only selected contracts
//! were pinned: `tests/docs_build_layout.rs` owns the §"Test structure"
//! table's own mentions, `tests/docs_test_classification.rs` owns the
//! classification table, and the per-note contract suites own their one
//! note. Nothing resolved the corpus's links themselves, so a renamed note,
//! a moved script, a deleted fixture, or a reworded heading quietly left a
//! 404 or a dead citation behind — the drift this guard fails CI on:
//!
//! 1. **Local file links** — every markdown link (inline `[…](url)` and
//!    reference definition `[…]: url`) in every scanned surface whose URL
//!    is not an external `scheme:` URL must resolve to an existing file or
//!    directory through the linking document's own relative join, `./` and
//!    `../` segments normalized. This is exactly the shape the guard caught
//!    on landing: the `get_home` link in `docs/notes/config-file-contract.md`
//!    pointed at `../src/util.rs`, one directory short of `src/util.rs` —
//!    a link that renders as 404 on both forges while the file it meant
//!    sits two levels up.
//! 2. **Section anchors** — every `#fragment` (same-document and
//!    cross-file) must match a heading of the target markdown under the
//!    forges' slug rules: lowercase, punctuation dropped, underscores kept,
//!    whitespace to hyphens, duplicate headings disambiguated `-1`, `-2`
//!    (so `#warm-pty-pool-adr-005`, `#repository--contributions`, and
//!    `#hook-inheritance-inherit_hooks` all resolve). A fragment on a
//!    non-markdown target is drift too — no heading anchors exist there.
//! 3. **Referenced scripts** — every `scripts/` path cited in a code span
//!    or a fenced command block (the copy-paste surfaces) must exist, brace
//!    alternatives (`scripts/x.{sh,py}`) expanded and checked each.
//! 4. **Documented fixture paths** — the same for every `tests/fixtures/`
//!    citation.
//!
//! Citation placeholders that name a *future* file are skipped by shape, not
//! by exemption list: a basename with an uppercase letter (`vX.Y.Z`
//! version placeholders) or any of `<`, `>`, `$`, `*`, `?` in the token is
//! conventionally illustrative (`transcript_vX.Y.Z.jsonl`,
//! `stream_json_golden_v<new>.jsonl`), never a checkout path.
//!
//! Deliberately not pinned here: backtick citations outside the two anchored
//! prefixes (the `tests/*.rs` / `src/*.rs` mentions of the §"Test structure"
//! table are owned by `tests/docs_build_layout.rs`, the target
//! classification by `tests/docs_test_classification.rs`); site-absolute
//! `/`-leading links, which the forges resolve against the server root; and
//! CHANGELOG.md, a historical record whose links describe past states.
//! Links inside fenced blocks or inline code spans are not rendered links
//! and are not checked — but fenced lines are scanned for citations,
//! because a command block is exactly what a reader copies.
//!
//! Library-level like the rows it guards: reads the documentation corpus
//! and resolves link targets and citations against the checkout; spawns
//! nothing.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

/// The scanned surfaces: the two root documents plus every markdown file
/// under `docs/`, recursively (notes, plan, research).
const SURFACE_FILES: [&str; 2] = ["README.md", "AGENTS.md"];
const SURFACE_DIR: &str = "docs";

/// Citation anchors: the two repo-rooted prefixes whose mentions must
/// resolve — the scripts the docs tell a reader to run, and the fixtures
/// they name as the pinned evidence.
const CITATION_PREFIX_SCRIPTS: &str = "scripts/";
const CITATION_PREFIX_FIXTURES: &str = "tests/fixtures/";

/// One drift finding: the surface and 1-based line it was found on, a
/// stable kind the negative meta-tests match on, and a human-facing detail.
#[derive(Debug)]
struct Finding {
    file: String,
    line: usize,
    kind: &'static str,
    detail: String,
}

/// What the scan checked — so the always-on run can prove it held a real
/// corpus, not an accidentally-empty one.
#[derive(Default)]
struct Stats {
    surfaces: usize,
    links: usize,
    anchors: usize,
    scripts: usize,
    fixtures: usize,
}

/// Path and anchor resolution against a repository tree: the live checkout
/// for the always-on run, an in-memory map for the negative meta-tests.
trait RepoView {
    fn exists(&self, rel: &str) -> bool;
    /// The markdown source of `rel`, when it is a readable markdown file.
    fn markdown(&self, rel: &str) -> Option<String>;
}

/// The live checkout under test.
struct LiveRepo {
    root: PathBuf,
}

impl RepoView for LiveRepo {
    fn exists(&self, rel: &str) -> bool {
        self.root.join(rel).exists()
    }
    fn markdown(&self, rel: &str) -> Option<String> {
        if !rel.ends_with(".md") {
            return None;
        }
        fs::read_to_string(self.root.join(rel)).ok()
    }
}

/// A synthetic tree: exactly the files planted by a meta-test, nothing
/// else, so a resolution failure can never be rescued by the real checkout.
struct VirtualRepo {
    files: BTreeMap<String, String>,
}

impl RepoView for VirtualRepo {
    fn exists(&self, rel: &str) -> bool {
        self.files.contains_key(rel)
    }
    fn markdown(&self, rel: &str) -> Option<String> {
        if rel.ends_with(".md") {
            self.files.get(rel).cloned()
        } else {
            None
        }
    }
}

// ── The slug model ───────────────────────────────────────────────────────────

/// The forge slug of one heading: lowercase, keep alphanumerics, hyphens
/// and underscores, drop every other character (dots, parens, backticks,
/// `&`, em dashes, …), turn each whitespace character into a hyphen. This
/// is the rule that makes `Warm PTY pool (ADR-005)` →
/// `warm-pty-pool-adr-005`, `Repository & contributions` →
/// `repository--contributions` (the dropped `&` leaves both surrounding
/// spaces' hyphens), `SCM_RIGHTS fd transfer` → `scm_rights-fd-transfer`
/// (underscore kept), and a heading whose label sits in inline code,
/// `Hook inheritance (inherit_hooks)` → `hook-inheritance-inherit_hooks`.
fn slug_of(heading: &str) -> String {
    let mut out = String::with_capacity(heading.len());
    for c in heading.chars() {
        if c.is_whitespace() {
            out.push('-');
        } else if c.is_alphanumeric() || c == '-' || c == '_' {
            for lc in c.to_lowercase() {
                out.push(lc);
            }
        }
        // everything else is punctuation and dropped
    }
    out
}

/// Every anchor a markdown document exposes: its ATX heading slugs in
/// document order with forge duplicate-disambiguation — the first `## Dup`
/// is `dup`, the second `dup-1`, the third `dup-2`. Headings inside fenced
/// blocks are literal text and contribute nothing.
fn anchor_set(markdown: &str) -> BTreeSet<String> {
    let mut set = BTreeSet::new();
    let mut seen: BTreeMap<String, usize> = BTreeMap::new();
    let mut in_fence = false;
    for line in markdown.lines() {
        if line.trim_start().starts_with("```") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            continue;
        }
        let t = line.trim_start();
        let pounds = t.len() - t.trim_start_matches('#').len();
        if !(1..=6).contains(&pounds) {
            continue;
        }
        let rest = &t[pounds..];
        if !rest.starts_with([' ', '\t']) {
            continue; // `#tag`, not a heading
        }
        let slug = slug_of(rest.trim());
        let n = seen.entry(slug.clone()).or_insert(0);
        if *n == 0 {
            set.insert(slug);
        } else {
            set.insert(format!("{slug}-{n}"));
        }
        *n += 1;
    }
    set
}

// ── Link and citation extraction ─────────────────────────────────────────────

/// The contents of `line`'s inline code spans (`` `…` `` and ``` ``…`​` ```
/// runs), in order. Code spans are literal text — links inside them do not
/// render — but they are the citation surface.
fn code_spans(line: &str) -> Vec<String> {
    let chars: Vec<char> = line.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] != '`' {
            i += 1;
            continue;
        }
        let start = i;
        while i < chars.len() && chars[i] == '`' {
            i += 1;
        }
        let fences = i - start;
        // find the next run of exactly as many backticks
        let mut j = i;
        let mut close = None;
        while j < chars.len() {
            if chars[j] == '`' {
                let run_start = j;
                while j < chars.len() && chars[j] == '`' {
                    j += 1;
                }
                if j - run_start == fences {
                    close = Some(run_start);
                    break;
                }
            } else {
                j += 1;
            }
        }
        match close {
            Some(end) => {
                out.push(chars[i..end].iter().collect());
                i = end + fences;
            }
            None => break, // unmatched opener: no spans from here on
        }
    }
    out
}

/// `line` with every inline code span's content blanked to spaces, so link
/// extraction sees only what renders as a link. The delimiting backticks
/// stay — they cannot look like link syntax.
fn mask_code_spans(line: &str) -> String {
    let mut masked: Vec<char> = line.chars().collect();
    let chars = masked.clone();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] != '`' {
            i += 1;
            continue;
        }
        let start = i;
        while i < chars.len() && chars[i] == '`' {
            i += 1;
        }
        let fences = i - start;
        let mut j = i;
        let mut close = None;
        while j < chars.len() {
            if chars[j] == '`' {
                let run_start = j;
                while j < chars.len() && chars[j] == '`' {
                    j += 1;
                }
                if j - run_start == fences {
                    close = Some(run_start);
                    break;
                }
            } else {
                j += 1;
            }
        }
        match close {
            Some(end) => {
                for slot in masked.iter_mut().take(end).skip(i) {
                    *slot = ' ';
                }
                i = end + fences;
            }
            None => break,
        }
    }
    masked.into_iter().collect()
}

/// Whether `url` is an external reference no checkout can resolve: a
/// leading `scheme:` (`https://…`, `mailto:…`) whose scheme segment is
/// alphabetic-first and carries only scheme characters. A colon later in a
/// relative path (`foo/bar:baz`) does not make one.
fn is_external(url: &str) -> bool {
    let Some(colon) = url.find(':') else {
        return false;
    };
    let scheme = &url[..colon];
    !scheme.is_empty()
        && scheme.starts_with(|c: char| c.is_ascii_alphabetic())
        && scheme
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '+' || c == '-' || c == '.')
}

/// The URL of every inline link in `masked` (a line whose code spans are
/// already blanked): the `…)` payload of each `](`, first whitespace token
/// only so an optional ` "title"` tail is dropped, surrounding `<>`
/// stripped. Images (`![…](url)`) are links for this purpose.
fn inline_link_urls(masked: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = masked;
    while let Some(open) = rest.find("](") {
        let after = &rest[open + 2..];
        let Some(close) = after.find(')') else {
            break;
        };
        let url = after[..close].split_whitespace().next().unwrap_or("");
        let url = url.trim_start_matches('<').trim_end_matches('>');
        if !url.is_empty() {
            out.push(url.to_string());
        }
        rest = &after[close..];
    }
    out
}

/// The URL of a reference-definition line `[…]: url` (code spans already
/// blanked), if the line is one.
fn reference_definition_url(masked: &str) -> Option<String> {
    let t = masked.trim_start();
    let t = t.strip_prefix('[')?;
    let label_end = t.find(']')?;
    if label_end == 0 {
        return None; // `[]:` is not a definition
    }
    let t = t[label_end + 1..].strip_prefix(':')?;
    let url = t.split_whitespace().next()?;
    let url = url.trim_start_matches('<').trim_end_matches('>');
    (!url.is_empty()).then(|| url.to_string())
}

/// Lexically normalize `path` relative to `base_dir` (a repo-relative
/// directory, `""` at the root): resolve `.` and `..` segments without
/// touching the filesystem. Climbing above the root stays at the root —
/// the lenient join the forges' renderers effectively perform.
fn normalize_rel(base_dir: &str, path: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for seg in base_dir.split('/').filter(|s| !s.is_empty()) {
        parts.push(seg);
    }
    for seg in path.split('/').filter(|s| !s.is_empty() && *s != ".") {
        if seg == ".." {
            parts.pop();
        } else {
            parts.push(seg);
        }
    }
    parts.join("/")
}

// ── Citation extraction ──────────────────────────────────────────────────────

/// `path` with its first `{a,b,…}` group expanded into one path per
/// alternative (`scripts/x.{sh,py}` → two paths). Unmatched or nested
/// braces are not expansion syntax and come back as the literal path.
fn expand_braces(path: &str) -> Vec<String> {
    let Some(start) = path.find('{') else {
        return vec![path.to_string()];
    };
    let Some(len) = path[start + 1..].find('}') else {
        return vec![path.to_string()];
    };
    let end = start + 1 + len;
    let inner = &path[start + 1..end];
    if inner.contains('{') || inner.contains('}') {
        return vec![path.to_string()];
    }
    inner
        .split(',')
        .map(|alt| format!("{}{}{}", &path[..start], alt, &path[end + 1..]))
        .collect()
}

/// Whether a citation token names a *future* file by shape rather than a
/// checkout path: a basename carrying an uppercase letter (`vX.Y.Z`
/// version placeholders) or any of `<`, `>`, `$`, `*`, `?` anywhere. The
/// real trees under both anchored prefixes are all-lowercase.
fn is_placeholder(token: &str) -> bool {
    let basename = token.rsplit('/').next().unwrap_or(token);
    token.contains(['<', '>', '$', '*', '?']) || basename.chars().any(|c| c.is_ascii_uppercase())
}

/// Every anchored citation in `text` (a code span's content or a fenced
/// line), as `(prefix, expanded path)` pairs: whitespace tokens, shell
/// punctuation trimmed, `VAR=value` prefixes stripped, a leading `./`
/// dropped, brace alternatives expanded.
fn citations_in(text: &str) -> Vec<(&'static str, String)> {
    let mut out = Vec::new();
    for token in text.split_whitespace() {
        let mut tok = token.trim_start_matches(['"', '\'', '(']);
        tok = tok.trim_end_matches(['"', '\'', ')', ',', '.', ';', ':']);
        while let Some((name, rest)) = tok.split_once('=') {
            let env_shaped = !name.is_empty()
                && name
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
                && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
            if env_shaped {
                tok = rest;
            } else {
                break;
            }
        }
        let tok = tok.strip_prefix("./").unwrap_or(tok);
        let prefix = if tok.starts_with(CITATION_PREFIX_SCRIPTS) {
            CITATION_PREFIX_SCRIPTS
        } else if tok.starts_with(CITATION_PREFIX_FIXTURES) {
            CITATION_PREFIX_FIXTURES
        } else {
            continue;
        };
        if is_placeholder(tok) {
            continue;
        }
        for expanded in expand_braces(tok) {
            out.push((prefix, expanded));
        }
    }
    out
}

// ── The check itself ─────────────────────────────────────────────────────────

/// Check every scanned surface's links, anchors, and citations against
/// `repo`, returning every finding (sorted by surface, then line — the
/// deterministic order of the BTreeMap iteration) plus what was checked.
fn check_documentation(
    docs: &BTreeMap<String, String>,
    repo: &dyn RepoView,
) -> (Vec<Finding>, Stats) {
    let mut findings = Vec::new();
    let mut stats = Stats {
        surfaces: docs.len(),
        ..Default::default()
    };
    for (rel, content) in docs {
        let mut in_fence = false;
        for (i, line) in content.lines().enumerate() {
            let line_no = i + 1;
            if line.trim_start().starts_with("```") {
                in_fence = !in_fence;
                continue;
            }
            if in_fence {
                // A fenced line is not rendered markdown, but it is the
                // copy-paste surface — its citations must resolve.
                check_citations(rel, line_no, line, repo, &mut findings, &mut stats);
                continue;
            }
            for span in code_spans(line) {
                check_citations(rel, line_no, &span, repo, &mut findings, &mut stats);
            }
            let masked = mask_code_spans(line);
            let mut urls = inline_link_urls(&masked);
            if let Some(url) = reference_definition_url(&masked) {
                urls.push(url);
            }
            for url in &urls {
                check_url(rel, line_no, content, url, repo, &mut findings, &mut stats);
            }
        }
    }
    (findings, stats)
}

/// Resolve one citation-bearing text (code span or fenced line).
fn check_citations(
    rel: &str,
    line_no: usize,
    text: &str,
    repo: &dyn RepoView,
    findings: &mut Vec<Finding>,
    stats: &mut Stats,
) {
    for (prefix, path) in citations_in(text) {
        if prefix == CITATION_PREFIX_SCRIPTS {
            stats.scripts += 1;
        } else {
            stats.fixtures += 1;
        }
        if !repo.exists(&path) {
            findings.push(Finding {
                file: rel.to_string(),
                line: line_no,
                kind: "citation",
                detail: format!(
                    "documents {path:?} under {prefix}, which does not exist in the \
                     checkout — a renamed or removed file must ripple through the \
                     documentation in the same commit"
                ),
            });
        }
    }
}

/// Resolve one markdown link URL (inline link or reference definition).
fn check_url(
    rel: &str,
    line_no: usize,
    own_content: &str,
    url: &str,
    repo: &dyn RepoView,
    findings: &mut Vec<Finding>,
    stats: &mut Stats,
) {
    if is_external(url) {
        return;
    }
    let (path, fragment) = match url.split_once('#') {
        Some((p, f)) => (p, f),
        None => (url, ""),
    };
    if path.is_empty() {
        // same-document anchor
        stats.anchors += 1;
        if !anchor_set(own_content).contains(fragment) {
            findings.push(Finding {
                file: rel.to_string(),
                line: line_no,
                kind: "anchor",
                detail: format!(
                    "links to #{fragment}, which is not a heading of {rel} — a \
                     reworded heading must ripple through every link to it"
                ),
            });
        }
        return;
    }
    if path.starts_with('/') {
        return; // site-absolute: resolved against the forges' server root
    }
    let base = Path::new(rel).parent().unwrap_or_else(|| Path::new(""));
    let target = normalize_rel(&base.display().to_string(), path);
    stats.links += 1;
    if !repo.exists(&target) {
        findings.push(Finding {
            file: rel.to_string(),
            line: line_no,
            kind: "link-target",
            detail: format!(
                "links to {url:?}, which resolves to {target:?} from {rel} — not a \
                 file in the checkout (check the relative depth: each ../ climbs one \
                 directory)"
            ),
        });
        return;
    }
    if fragment.is_empty() {
        return;
    }
    stats.anchors += 1;
    if !target.ends_with(".md") {
        findings.push(Finding {
            file: rel.to_string(),
            line: line_no,
            kind: "anchor-target",
            detail: format!(
                "links to {url:?} — heading anchors exist only for markdown files, \
                 so the fragment cannot resolve"
            ),
        });
        return;
    }
    match repo.markdown(&target) {
        None => findings.push(Finding {
            file: rel.to_string(),
            line: line_no,
            kind: "anchor-target",
            detail: format!("links to {target:?}, which exists but cannot be read"),
        }),
        Some(target_md) => {
            if !anchor_set(&target_md).contains(fragment) {
                findings.push(Finding {
                    file: rel.to_string(),
                    line: line_no,
                    kind: "anchor",
                    detail: format!(
                        "links to {url:?}, but {target} has no heading slugging to \
                         #{fragment} — a reworded heading must ripple through every \
                         link to it"
                    ),
                });
            }
        }
    }
}

// ── The live corpus ──────────────────────────────────────────────────────────

/// Repo-root probes: a directory holding both is a claude-print checkout.
const ROOT_PROBES: [&str; 2] = ["AGENTS.md", "Cargo.toml"];

/// Where repo-relative files are read from, resolved at *runtime* — never
/// the bare compile-time `env!("CARGO_MANIFEST_DIR")`, which bakes the
/// building checkout's path into the test binary. The local cargo wrapper
/// maps `.git`-less extractions onto one shared target dir, so an
/// extraction of unchanged content instant-reuses a cached test binary
/// compiled in an extraction that has since been deleted; a baked-only
/// root then fails every later run of that binary with file-NotFound
/// panics that have nothing to do with drift (bead claudepr-23f81f16; the
/// same chain as `tests/docs_test_classification.rs`). Candidates, most
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
    .unwrap_or_else(|e| panic!("locating the repo root to read the documentation corpus from: {e}"))
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

/// The scanned corpus: the two root documents plus every markdown file
/// under `docs/`, recursively, as repo-relative path to content.
fn documentation_surfaces() -> BTreeMap<String, String> {
    let root = repo_root();
    let mut docs = BTreeMap::new();
    for rel in SURFACE_FILES {
        let content = fs::read_to_string(root.join(rel))
            .unwrap_or_else(|e| panic!("reading scanned surface {rel}: {e}"));
        docs.insert(rel.to_string(), content);
    }
    walk_markdown(&root.join(SURFACE_DIR), SURFACE_DIR, &mut docs);
    assert!(
        !docs.is_empty(),
        "the scanned documentation corpus came back empty — the guard cannot be \
         vacuously passing off an unread tree"
    );
    docs
}

fn walk_markdown(dir: &Path, rel_prefix: &str, out: &mut BTreeMap<String, String>) {
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
            walk_markdown(&path, &rel, out);
        } else if name.ends_with(".md") {
            let content = fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("reading scanned surface {rel}: {e}"));
            out.insert(rel, content);
        }
    }
}

// ── Always-on checks ─────────────────────────────────────────────────────────

#[test]
fn local_links_anchors_script_and_fixture_references_resolve() {
    let docs = documentation_surfaces();
    let (findings, _stats) = check_documentation(&docs, &LiveRepo { root: repo_root() });
    let rendered = findings
        .iter()
        .map(|f| format!("  {}:{} [{}] {}", f.file, f.line, f.kind, f.detail))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        findings.is_empty(),
        "the documentation corpus carries unresolved local references — renamed or \
         removed files and headings must ripple through the docs in the same commit:\n\
         {rendered}"
    );
}

#[test]
fn the_scan_holds_a_real_corpus() {
    let docs = documentation_surfaces();
    let (findings, stats) = check_documentation(&docs, &LiveRepo { root: repo_root() });
    assert!(
        findings.is_empty(),
        "the corpus must be clean before its stats can certify it"
    );
    // The corpus is pinned from below, never from above: these bounds only
    // prove the scan saw a real corpus, so a parsing regression that
    // silently extracts nothing cannot pass vacuously.
    assert!(
        stats.surfaces >= 20,
        "expected the two root documents plus every docs/ note ({})",
        stats.surfaces
    );
    // Most corpus links are bare same-document anchors, so file targets and
    // anchors are deliberately separate bounds.
    assert!(
        stats.links >= 20,
        "expected dozens of file-target links, saw {}",
        stats.links
    );
    assert!(
        stats.anchors >= 30,
        "expected dozens of anchors, saw {}",
        stats.anchors
    );
    assert!(
        stats.scripts >= 100,
        "expected the docs to cite scripts/ heavily, saw {}",
        stats.scripts
    );
    assert!(
        stats.fixtures >= 20,
        "expected the docs to name tests/fixtures/ heavily, saw {}",
        stats.fixtures
    );
    // The walk must recurse: the corpus lives in docs/ subdirectories.
    assert!(
        docs.keys()
            .any(|k| k.starts_with("docs/") && k.matches('/').count() >= 2),
        "expected scanned surfaces below docs/<subdir>/ — the recursive walk \
         degenerated to the docs/ root"
    );
}

// ── The slug model, pinned against the corpus's hard cases ───────────────────

#[test]
fn heading_slugs_follow_the_forges_rules() {
    // Four live shapes from the corpus, each exercising one slug rule.
    assert_eq!(slug_of("Warm PTY pool (ADR-005)"), "warm-pty-pool-adr-005");
    assert_eq!(
        slug_of("Repository & contributions"),
        "repository--contributions"
    );
    assert_eq!(
        slug_of("Hook inheritance (`inherit_hooks`)"),
        "hook-inheritance-inherit_hooks"
    );
    assert_eq!(slug_of("SCM_RIGHTS fd transfer"), "scm_rights-fd-transfer");
    assert_eq!(
        slug_of("Versioning and compatibility"),
        "versioning-and-compatibility"
    );
    assert_eq!(slug_of("Linger"), "linger");
    assert_eq!(slug_of("Self-check"), "self-check");
    // Duplicate headings are suffixed, not shadowed.
    let anchors = anchor_set("# Dup\n\ntext\n\n# Dup\n\n# Dup\n");
    assert!(anchors.contains("dup") && anchors.contains("dup-1") && anchors.contains("dup-2"));
    // Fenced `#` lines are literal text, not headings.
    assert!(anchor_set("```text\n# not a heading\n```\n\n# real\n").contains("real"));
    assert!(!anchor_set("```text\n# not a heading\n```\n\n# real\n").contains("not-a-heading"));
    // `#tag` is not a heading; `# heading` is.
    assert_eq!(anchor_set("#tag\n# heading\n").len(), 1);
}

// ── Negative meta-tests: the guard must REPORT planted drift ─────────────────
//
// Every leg plants drift in a synthetic tree (or mutates the live corpus
// in memory) and requires the owning check to report it by kind, file, and
// detail — the committed non-vacuity pattern of `tests/docs_build_layout.rs`
// (claudepr-4d967120): the guard is re-proven on every run, not asserted
// once in a discarded scratch run.

/// Check a synthetic tree and return its findings.
fn virtual_findings(files: &[(&str, &str)]) -> Vec<Finding> {
    let docs: BTreeMap<String, String> = files
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    check_documentation(
        &docs,
        &VirtualRepo {
            files: docs.clone(),
        },
    )
    .0
}

/// Require a finding of `kind` on `file` whose detail carries every
/// fragment, failing with the full finding list when absent.
fn assert_reported(findings: &[Finding], kind: &str, file: &str, fragments: &[&str]) {
    let rendered = || {
        findings
            .iter()
            .map(|f| format!("  {}:{} [{}] {}", f.file, f.line, f.kind, f.detail))
            .collect::<Vec<_>>()
            .join("\n")
    };
    assert!(
        findings.iter().any(|f| {
            f.kind == kind && f.file == file && fragments.iter().all(|x| f.detail.contains(x))
        }),
        "the planted drift was NOT reported as a {kind} finding on {file} naming \
         {fragments:?} — the guard is vacuous for this mutation. Findings:\n{}",
        rendered()
    );
}

/// Require that no finding at all was reported.
fn assert_clean(findings: &[Finding]) {
    assert!(
        findings.is_empty(),
        "nothing should have been reported:\n{}",
        findings
            .iter()
            .map(|f| format!("  {}:{} [{}] {}", f.file, f.line, f.kind, f.detail))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

#[test]
fn negative_meta_broken_link_targets_are_reported() {
    // The exact drift the guard caught live on landing: a relative link one
    // directory short of its target.
    let findings = virtual_findings(&[
        ("README.md", "# Root\n"),
        ("src/util.rs", "pub fn get_home() {}\n"),
        (
            "docs/notes/note.md",
            "# Note\n\n[`get_home`](../src/util.rs) — one ../ short from docs/notes/.\n\n\
             And the fixed form is clean: [`get_home`](../../src/util.rs).\n",
        ),
    ]);
    assert_reported(
        &findings,
        "link-target",
        "docs/notes/note.md",
        &["../src/util.rs"],
    );
    assert_eq!(
        findings.len(),
        1,
        "the corrected ../../ form must not be reported: {findings:?}"
    );

    // A plain dead file link, in a reference definition too.
    let findings = virtual_findings(&[
        ("README.md", "# Root\n\nSee [ghost](docs/notes/ghost.md).\n"),
        (
            "docs/notes/real.md",
            "# Real\n\n[home]: ../../README.md\n[ghost]: ghost2.md\n",
        ),
    ]);
    assert_reported(
        &findings,
        "link-target",
        "README.md",
        &["docs/notes/ghost.md"],
    );
    assert_reported(
        &findings,
        "link-target",
        "docs/notes/real.md",
        &["ghost2.md"],
    );
    assert_eq!(
        findings.len(),
        2,
        "the live [home] definition must not be reported"
    );
}

#[test]
fn negative_meta_broken_anchors_are_reported() {
    let findings = virtual_findings(&[
        (
            "README.md",
            "# Title\n\n## Warm PTY pool (ADR-005)\n\n## Dup\n\n## Dup\n\n\
             Self: [pool](#warm-pty-pool-adr-005), [dead](#warm-pty-pool), \
             [first](#dup), [second](#dup-1), [third](#dup-2).\n",
        ),
        (
            "docs/n.md",
            "# N\n\n[up](../README.md#warm-pty-pool-adr-005) ok, \
                       [bad](../README.md#repository--contributions) not.\n",
        ),
    ]);
    assert_reported(&findings, "anchor", "README.md", &["#warm-pty-pool"]);
    assert_reported(&findings, "anchor", "README.md", &["#dup-2"]);
    assert_reported(
        &findings,
        "anchor",
        "docs/n.md",
        &["#repository--contributions"],
    );
    assert_eq!(
        findings.len(),
        3,
        "the four live anchors (slug rules + duplicate suffixes) must not be reported"
    );

    // A fragment on a non-markdown target can never resolve.
    let findings = virtual_findings(&[
        ("install.sh", "#!/usr/bin/env sh\n"),
        ("README.md", "# Root\n\n[frag](install.sh#section)\n"),
    ]);
    assert_reported(
        &findings,
        "anchor-target",
        "README.md",
        &["install.sh#section"],
    );
    assert_eq!(findings.len(), 1);
}

#[test]
fn negative_meta_dead_script_and_fixture_citations_are_reported() {
    let findings = virtual_findings(&[
        ("README.md", "# Root\n"),
        ("scripts/real.sh", ":\n"),
        ("tests/fixtures/real.json", "{}\n"),
        (
            "AGENTS.md",
            "# Agents\n\nPlain: `scripts/ghost.sh` — dead.\n\n\
             Command: `bash scripts/real.sh` — live.\n\n\
             Env-glued: `CLAUDE_PRINT_POOL=1 ./scripts/ghost.sh` — dead.\n\n\
             Brace with one dead arm: `scripts/{real.sh,ghost.sh}`.\n\n\
             Fixture: `tests/fixtures/ghost.json` — dead.\n\n\
             Punctuated: (see `scripts/ghost.sh`.) — dead.\n",
        ),
        (
            "docs/fence.md",
            "# Fence\n\n```bash\nbash scripts/ghost.sh\n```\n\n\
             ```text\n[not a link](docs/ghost.md)\n```\n",
        ),
    ]);
    let agents_citations: Vec<_> = findings
        .iter()
        .filter(|f| f.file == "AGENTS.md" && f.kind == "citation")
        .collect();
    assert_eq!(
        agents_citations.len(),
        5,
        "exactly the five planted dead citations on AGENTS.md — the live real.sh and \
         real.json must not be reported: {agents_citations:?}"
    );
    for f in &agents_citations {
        assert!(
            f.detail.contains("ghost"),
            "each planted citation names its path: {}",
            f.detail
        );
    }
    assert_reported(
        &findings,
        "citation",
        "docs/fence.md",
        &["scripts/ghost.sh"],
    );
    assert_eq!(
        findings.len(),
        6,
        "exactly the six planted dead citations — the live real.sh/real.json and the \
         fenced link-syntax (not a rendered link) must not be reported: {findings:?}"
    );
}

#[test]
fn negative_meta_placeholders_and_external_forms_are_skipped() {
    let findings = virtual_findings(&[
        ("README.md", "# Root\n"),
        (
            "AGENTS.md",
            "# Agents\n\n\
             Placeholder version stem: `tests/fixtures/transcript_vX.Y.Z.jsonl`.\n\n\
             Parameterized: `tests/fixtures/stream_json_golden_v<new>.jsonl` and \
             `scripts/$CLAUDE_PRINT_BIN`.\n\n\
             Glob: `scripts/*` and `scripts/probe?.sh`.\n\n\
             External: [site](https://example.com/x), [mail](mailto:a@example.com), \
             and [ftp](ftp://example.com/f).\n\n\
             Site-absolute: [abs](/docs/notes/ghost.md).\n\n\
             Live: `scripts/real.sh`.\n",
        ),
        ("scripts/real.sh", ":\n"),
    ]);
    assert_clean(&findings);
}

#[test]
fn negative_meta_code_spans_are_not_links_and_fences_are_not_prose() {
    // A link shaped like syntax inside a code span does not render and is
    // not checked; the same shape outside the span is.
    let findings = virtual_findings(&[(
        "README.md",
        "# Root\n\n`[ghost](docs/ghost.md)` — literal.\n\n[real](README.md) — live.\n",
    )]);
    assert_clean(&findings);

    let findings = virtual_findings(&[(
        "README.md",
        "# Root\n\n[dead](docs/ghost.md) — a real link.\n",
    )]);
    assert_reported(&findings, "link-target", "README.md", &["docs/ghost.md"]);
    assert_eq!(findings.len(), 1);
}

#[test]
fn negative_meta_planted_drift_in_the_live_corpus_is_reported() {
    // The real corpus feeds the same checker: planting drift in the live
    // documents in memory must surface it against the real tree. The
    // planted lines go on top — a document whose tail sits inside a fenced
    // block would swallow anything appended after it.
    let mut docs = documentation_surfaces();
    for (surface, planted) in [
        (
            "AGENTS.md",
            "`scripts/ghost-claudepr-a7754a8e.sh`\n".to_string(),
        ),
        (
            "README.md",
            "[ghost](docs/notes/ghost-claudepr-a7754a8e.md)\n".to_string(),
        ),
    ] {
        let live = docs.get(surface).expect("a scanned surface").clone();
        docs.insert(surface.to_string(), format!("{planted}{live}"));
    }
    let (findings, _) = check_documentation(&docs, &LiveRepo { root: repo_root() });
    assert_reported(
        &findings,
        "citation",
        "AGENTS.md",
        &["ghost-claudepr-a7754a8e.sh"],
    );
    assert_reported(
        &findings,
        "link-target",
        "README.md",
        &["docs/notes/ghost-claudepr-a7754a8e.md"],
    );
    assert_eq!(
        findings.len(),
        2,
        "the unmutated corpus must be clean to begin with"
    );
}

// ── The repo-root resolution, pinned like its siblings ───────────────────────

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
