//! Documentation-drift guard for the transcript-slug algorithm.
//!
//! The plan glossary and the Transcript Reader section once described the slug
//! as "strip the leading `/`, then replace the remaining `/` with `-`" while
//! `docs/notes/hook-design.md` — and `src/poller.rs::cwd_to_slug`, verified
//! live against real `~/.claude/projects/` contents (bead claudepr-26e7a0b6) —
//! define folding **every** non-alphanumeric byte to `-`, leading `/`
//! included. The two stale passages produced slugs claude never creates
//! (`home-coding-myproject` vs the real `-home-coding-myproject`); this test
//! exists so that split cannot silently reappear (bead claudepr-3243f25c).
//!
//! Three layers, each catching a different half of the drift:
//!
//! 1. The fixture (`tests/fixtures/slug_vectors_v2.1.263.json`) pins the
//!    live-verified vectors against the implementation itself — if
//!    `cwd_to_slug` ever changes scheme, this fails first and forces a
//!    conscious decision before any doc is touched.
//! 2. Every `<path> → <slug>` mapping in the markdown docs is re-derived with
//!    `cwd_to_slug` — a doc example that stops matching the implementation
//!    fails here, wherever it lives.
//! 3. Banned phrasings (the strip-leading-slash recipe and its un-dashed
//!    outputs) and `projects/<slug>/` segments that match the stale scheme are
//!    rejected outright, so the wrong algorithm cannot be reintroduced in
//!    prose that has no paired mapping to re-derive.

use std::fs;
use std::path::Path;

use serde::Deserialize;

use claude_print::poller::cwd_to_slug;

const FIXTURE: &str = include_str!("fixtures/slug_vectors_v2.1.263.json");

#[derive(Debug, Deserialize)]
struct SlugFixture {
    vectors: Vec<SlugVector>,
    rejected: Vec<RejectedCwd>,
}

#[derive(Debug, Deserialize)]
struct SlugVector {
    cwd: String,
    slug: String,
}

#[derive(Debug, Deserialize)]
struct RejectedCwd {
    cwd: String,
}

/// The stale scheme's recipe, in spellings a literal match can catch. The
/// strip-the-leading-slash wording itself is banned structurally (any
/// inflection of "strip" followed by "the leading" on one line — see
/// `STRIP_VERB_FORMS`), because the recipe drifts by inflection: the plan
/// glossary said "stripping", the Transcript Reader section said "strip", and
/// "claude strips the leading slash" is the same recipe again. Historical
/// *mentions* of the superseded scheme (e.g. the "the earlier
/// strip-leading-slash scheme produced slugs claude never creates" note in
/// `hook-design.md`) deliberately avoid these shapes; if you need to mention
/// it, do the same.
const BANNED_PHRASES: &[&str] = &[
    "replace('/', '-')",
    "replace(\"/\", \"-\")",
    // The stale scheme's outputs for the documented example cwds — always
    // wrong because they lack the leading dash.
    "`home-coding-myproject`",
    "`home-user-myproject`",
];

/// Verb inflections the strip-the-leading-slash recipe appears in. Matched
/// case-insensitively at word boundaries; flagged when the recipe's object
/// ("the leading") follows anywhere later on the same line.
const STRIP_VERB_FORMS: &[&str] = &["stripped", "stripping", "strips", "strip"];

/// The object that turns "strip" (which also names the deliberate ANSI / env
/// stripping elsewhere in these docs) into the slug recipe.
const STRIP_RECIPE_OBJECT: &str = "the leading";

/// One line of the strip-the-leading-slash recipe: a standalone inflection of
/// "strip" with the recipe's object later on the same line. Line-granular on
/// purpose — the fold scheme's own "the leading `/` included" phrasing shares
/// lines with "folds"/"folding", never with a strip verb, and the historical
/// mention ("the earlier strip-leading-slash scheme") has no "the leading" on
/// its line — so legitimate prose passes and only the recipe pairs.
fn line_uses_strip_recipe(line: &str) -> bool {
    let lower = line.to_lowercase();
    for verb in STRIP_VERB_FORMS {
        let mut from = 0;
        while let Some(rel) = lower[from..].find(verb) {
            let start = from + rel;
            let end = start + verb.len();
            let word_starts = lower[..start]
                .chars()
                .next_back()
                .is_none_or(|c| !c.is_alphanumeric());
            let word_ends = lower[end..]
                .chars()
                .next()
                .is_none_or(|c| !c.is_alphanumeric());
            if word_starts && word_ends && lower[end..].contains(STRIP_RECIPE_OBJECT) {
                return true;
            }
            from = end;
        }
    }
    false
}

/// Markdown docs the guard covers: everything under `docs/` plus the README.
fn doc_files() -> Vec<(String, String)> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    collect_md(&root.join("docs"), root, &mut files);
    let readme = root.join("README.md");
    if readme.exists() {
        files.push((
            "README.md".to_string(),
            fs::read_to_string(&readme).expect("read README.md"),
        ));
    }
    assert!(
        !files.is_empty(),
        "no markdown docs found — the guard lost its inputs"
    );
    files
}

fn collect_md(dir: &Path, root: &Path, out: &mut Vec<(String, String)>) {
    let mut entries: Vec<_> = fs::read_dir(dir)
        .expect("read docs dir")
        .map(|e| e.expect("dir entry"))
        .collect();
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let path = entry.path();
        if path.is_dir() {
            collect_md(&path, root, out);
        } else if path.extension().is_some_and(|ext| ext == "md") {
            let rel = path
                .strip_prefix(root)
                .expect("doc under root")
                .to_string_lossy()
                .into_owned();
            out.push((rel, fs::read_to_string(&path).expect("read doc")));
        }
    }
}

/// Extract `(cwd, slug)` pairs from arrow mappings in one line, in both the
/// backticked form (`` `/a/b` → `-a-b` ``) and the bare fenced form
/// (`/a/b → -a-b`). Only pairs whose left side looks like a filesystem path
/// and whose right side looks like a slug (no `/`, slug charset only) are
/// returned, so the docs' many non-slug arrows (state transitions, billing
/// routing, hook tables) never reach the assertion.
fn extract_mappings(line: &str) -> Vec<(String, String)> {
    // match_indices yields char-boundary-safe positions, which the docs need —
    // their ASCII trees are full of multi-byte box-drawing characters.
    let mut arrows: Vec<(usize, usize)> = Vec::new();
    for (pos, _) in line.match_indices('→') {
        arrows.push((pos, '→'.len_utf8()));
    }
    for (pos, _) in line.match_indices("->") {
        arrows.push((pos, 2));
    }
    arrows.sort_unstable_by_key(|(pos, _)| *pos);

    let mut pairs = Vec::new();
    for (pos, len) in arrows {
        if let (Some(cwd), Some(slug)) = (token_before(line, pos), token_after(line, pos + len)) {
            // A fold introduces a dash whenever the cwd had any non-slug
            // character (every absolute path does), so a dashless slug can
            // only be a verbatim pure-alphanumeric cwd. Requiring one of the
            // two keeps prose flows (`/dev/ttyS0` → `shell`) out.
            let looks_like_slug = !slug.is_empty()
                && !slug.contains('/')
                && slug.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
                && (slug.contains('-') || slug == cwd);
            if cwd.starts_with('/') && looks_like_slug {
                pairs.push((cwd, slug));
            }
        }
    }
    pairs
}

/// The path token ending immediately before `pos` (whitespace skipped), with
/// surrounding backticks stripped: `` `/a/b` `` and bare `/a/b` both give
/// `/a/b`.
fn token_before(line: &str, pos: usize) -> Option<String> {
    let raw: String = line[..pos]
        .chars()
        .rev()
        .skip_while(|c| c.is_whitespace())
        .take_while(|c| is_path_char(*c))
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    let piece = raw.trim_matches('`');
    // A stray interior backtick (token ran into another span) — keep only the
    // segment after it, which is the one adjacent to the arrow.
    let piece = piece.rsplit('`').next().unwrap_or(piece);
    let piece = piece.trim_end_matches(',');
    if piece.is_empty() {
        None
    } else {
        Some(piece.to_string())
    }
}

/// The slug token starting immediately after `pos` (whitespace and an opening
/// backtick skipped): `` `-a-b` `` and bare `-a-b` both give `-a-b`. The scan
/// stops at the first character outside the slug charset, so trailing
/// backticks, periods and prose never attach to the token.
fn token_after(line: &str, pos: usize) -> Option<String> {
    let piece: String = line[pos..]
        .chars()
        .skip_while(|c| c.is_whitespace())
        .skip_while(|c| *c == '`')
        .take_while(|c| is_slug_char(*c))
        .collect();
    if piece.is_empty() {
        None
    } else {
        Some(piece)
    }
}

/// Characters a cwd path token may contain in doc prose.
fn is_path_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '/' | '-' | '_' | '.' | '`')
}

/// Characters a folded slug may contain.
fn is_slug_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '-'
}

/// Concrete `projects/<segment>/` directory names appearing in doc paths.
/// `<placeholder>` segments are skipped by construction (they fail the
/// charset scan).
fn extract_project_segments(content: &str) -> Vec<String> {
    let mut segments = Vec::new();
    let mut rest = content;
    while let Some(idx) = rest.find("projects/") {
        let after = &rest[idx + "projects/".len()..];
        let seg: String = after.chars().take_while(|c| is_slug_char(*c)).collect();
        // Only a segment followed by `/` names a transcript directory; prose
        // like "projects directory" is not a slug.
        if !seg.is_empty() && after[seg.len()..].starts_with('/') {
            segments.push(seg);
        }
        rest = after;
    }
    segments
}

/// Every `cwd` a document pins: JSON payload fields and mapping left sides.
fn extract_documented_cwds(content: &str) -> Vec<String> {
    let mut cwds: Vec<String> = Vec::new();
    for line in content.lines() {
        if let Some(json_cwd) = extract_json_cwd(line) {
            cwds.push(json_cwd);
        }
        for (cwd, _) in extract_mappings(line) {
            cwds.push(cwd);
        }
    }
    cwds
}

/// `"cwd": "/abs/path"` → `/abs/path`, tolerating whitespace variation.
fn extract_json_cwd(line: &str) -> Option<String> {
    let idx = line.find("\"cwd\"")?;
    let rest = line[idx + "\"cwd\"".len()..].trim_start();
    let rest = rest.strip_prefix(':')?.trim_start();
    let rest = rest.strip_prefix('"')?;
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

/// The stale scheme applied to `cwd`: leading `/` stripped, remaining `/`
/// folded to `-`. A documented `projects/<segment>/` must never equal this.
fn stale_scheme_slug(cwd: &str) -> String {
    cwd.strip_prefix('/').unwrap_or(cwd).replace('/', "-")
}

#[test]
fn fixture_vectors_match_cwd_to_slug() {
    let fixture: SlugFixture =
        serde_json::from_str(FIXTURE).expect("parse slug_vectors_v2.1.263.json");
    assert!(
        fixture.vectors.len() >= 10,
        "fixture lost vectors — the live-verified minimum is 10"
    );
    for vector in &fixture.vectors {
        let derived =
            cwd_to_slug(&vector.cwd).unwrap_or_else(|e| panic!("cwd {:?}: {e}", vector.cwd));
        assert_eq!(
            derived, vector.slug,
            "cwd_to_slug diverged from the live-verified vector for {:?} — \
             if the scheme changed on purpose, update the fixture, the docs it \
             cites, and docs/plan/plan.md together",
            vector.cwd
        );
    }
    for rejected in &fixture.rejected {
        assert!(
            cwd_to_slug(&rejected.cwd).is_err(),
            "cwd {:?} was verified rejected but cwd_to_slug now accepts it",
            rejected.cwd
        );
    }
}

#[test]
fn doc_slug_mappings_match_cwd_to_slug() {
    let mut failures = Vec::new();
    for (rel, content) in doc_files() {
        for line in content.lines() {
            for (cwd, slug) in extract_mappings(line) {
                match cwd_to_slug(&cwd) {
                    Ok(derived) if derived == slug => {}
                    Ok(derived) => failures.push(format!(
                        "{rel}: mapping `{cwd}` → `{slug}` does not match \
                         cwd_to_slug (expected `{derived}`)"
                    )),
                    Err(e) => failures.push(format!(
                        "{rel}: mapping `{cwd}` → `{slug}` uses a cwd \
                         cwd_to_slug rejects: {e}"
                    )),
                }
            }
        }
    }
    assert!(
        failures.is_empty(),
        "doc slug examples drifted from the implementation:\n  {}",
        failures.join("\n  ")
    );
}

#[test]
fn docs_contain_no_stale_slug_scheme() {
    let mut failures = Vec::new();
    for (rel, content) in doc_files() {
        for phrase in BANNED_PHRASES {
            if content.contains(phrase) {
                failures.push(format!("{rel}: stale slug phrasing {phrase:?}"));
            }
        }
        for line in content.lines() {
            if line_uses_strip_recipe(line) {
                failures.push(format!(
                    "{rel}: stale strip-the-leading-slash recipe: {line:?}"
                ));
            }
        }
        // A `projects/<segment>/` path equal to the stale scheme's output for
        // a cwd documented in the same file is the hook-design.md-style drift
        // (payload example showing `home-user-myproject` next to a derivation
        // section that folds to `-home-user-myproject`).
        let documented = extract_documented_cwds(&content);
        for segment in extract_project_segments(&content) {
            for cwd in &documented {
                if segment == stale_scheme_slug(cwd) && segment != *cwd {
                    failures.push(format!(
                        "{rel}: projects/{segment}/ matches the stale \
                         strip-leading-slash scheme of cwd {cwd:?} (fold gives \
                         {:?})",
                        cwd_to_slug(cwd).unwrap_or_default()
                    ));
                }
            }
        }
    }
    assert!(
        failures.is_empty(),
        "stale slug scheme found in docs:\n  {}",
        failures.join("\n  ")
    );
}

#[test]
fn strip_recipe_detector_catches_historical_spellings() {
    // Guard the structural layer: the literal banned phrases were folded into
    // `line_uses_strip_recipe`, so if that detector ever stops matching, the
    // stale recipe becomes publishable again with every test green. Pin the
    // spellings the recipe actually appeared in (plan glossary, Transcript
    // Reader section, third-person drift) plus a couple of plausible
    // re-inflections.
    for line in [
        "stripping the leading `/`, then replace the remaining `/` with `-`",
        "strip the leading `/`, then replace the remaining `/` with `-`",
        "claude strips the leading slash from the cwd before folding",
        "the cwd is stripped of the leading `/` before the fold",
    ] {
        assert!(
            line_uses_strip_recipe(line),
            "detector missed a historical spelling of the stale recipe: {line:?}"
        );
    }
    // …and the legitimate shapes that share words with the recipe — the fold
    // scheme's own phrasing, the deliberate historical mention, ANSI and
    // environment stripping — must stay clean, or every doc edit trips here.
    for line in [
        "`<slug>` folds **every** non-alphanumeric byte of the `cwd` to `-` —",
        "including the leading `/` — the scheme claude 2.1.263 actually uses for",
        "`~/.claude/projects/` (verified live; the earlier strip-leading-slash scheme",
        "| EC-9 | `last_assistant_message` contains ANSI escape sequences | Strip ANSI",
        "update health checks that invoke `--version` in a stripped environment.",
        ".rstrip()",
    ] {
        assert!(
            !line_uses_strip_recipe(line),
            "detector flagged legitimate prose: {line:?}"
        );
    }
}

#[test]
fn docs_actually_contain_slug_documentation() {
    // Guard the guard: if the docs ever stop documenting the slug entirely,
    // the two checks above would pass vacuously on an empty corpus.
    let mapping_count: usize = doc_files()
        .iter()
        .map(|(_, content)| {
            content
                .lines()
                .map(extract_mappings)
                .map(|pairs| pairs.len())
                .sum::<usize>()
        })
        .sum();
    assert!(
        mapping_count >= 3,
        "expected at least 3 slug mappings across the docs, found \
         {mapping_count} — did the slug documentation move?"
    );
}
