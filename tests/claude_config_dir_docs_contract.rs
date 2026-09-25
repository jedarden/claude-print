//! Documentation-contract test for the Claude-state / `CLAUDE_CONFIG_DIR`
//! sections (bead claudepr-e46458c4).
//!
//! The behavioral invariant — claude-print never sets `CLAUDE_CONFIG_DIR`
//! and scrubs an inherited one, so the child's transcript lands under
//! `$HOME/.claude/projects/` — is enforced by
//! `tests/claude_config_dir_contract.rs` (bead claudepr-bfe97ce4) at the
//! PTY and binary level. But the *published* account of that invariant
//! lives in prose in two places, and prose drifts:
//!
//! - `docs/notes/config-file-contract.md` §"Claude Code state and
//!   `CLAUDE_CONFIG_DIR`" — the normative contract text, which draws the
//!   boundary this bead exists to document: the config file configures the
//!   **wrapper**; Claude Code's own state (credentials, settings, and the
//!   transcripts claude-print reads back) is not configurable through
//!   claude-print at all and stays HOME-rooted.
//! - `README.md` §Configuration / §"Claude Code state (`CLAUDE_CONFIG_DIR`)"
//!   — the user-facing summary of the same claims.
//!
//! Until this test the sections' claims were checked by nobody: a renamed
//! const (`SCRUBBED_ENV`), a relocated function
//! (`derive_transcript_path`, `projects_dir_for_cwd`), a moved billing
//! script, or a quietly added `--claude-config-dir` flag would leave both
//! documents stating invariants the binary no longer has. Every check here
//! reads the docs and judges them against the implementation, never the
//! reverse:
//!
//!   * each contract-bearing sentence of the two sections is present in
//!     the section (whitespace-normalized, scoped by heading so a string
//!     surviving elsewhere cannot satisfy a check vacuously), and the two
//!     documents name the same identifiers for the same claims;
//!   * every file the sections cite exists (`src/hook.rs`, `src/pty.rs`,
//!     `scripts/check-billing.sh`, `docs/notes/home-handling-strategy.md`,
//!     `tests/claude_config_dir_contract.rs`), and the source-level claims
//!     hold: `SCRUBBED_ENV` carries `CLAUDE_CONFIG_DIR`, `FORCED_ENV` —
//!     the only variables claude-print injects — does not, and `src/hook.rs`
//!     (the per-run temp dir) never mentions the variable;
//!   * the HOME-rooting claim is behavioral, not syntactic: under a guarded
//!     `HOME` *and* a decoy inherited `CLAUDE_CONFIG_DIR`,
//!     `derive_transcript_path` returns exactly
//!     `$HOME/.claude/projects/<cwd-slug>/<session-id>.jsonl` and
//!     `projects_dir_for_cwd` returns `$HOME/.claude/projects/<cwd-slug>/`
//!     — neither path touches the decoy, which is what "HOME-rooted by
//!     construction and cannot follow a redirect" means;
//!   * "No flag, config key, or environment variable redirects the config
//!     dir" is checked against all three surfaces: clap's own parser
//!     definitions (the only long flags containing `config` are the
//!     wrapper's `--config` and the child-forwarded `--mcp-config`), the
//!     closed-world `[defaults]` schema (a `claude_config_dir` key is an
//!     unknown-field parse error naming the four real keys), and `FORCED_ENV`.
//!
//! Library-level and hermetic: no PTY, no `claude` binary, no subprocess.
//! The one environment-mutating test takes the process-env guard this
//! crate's suites already use; it is the only test in this binary that
//! reads or writes `HOME`/`CLAUDE_CONFIG_DIR`, so it cannot race the
//! parallel tests here.

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use clap::CommandFactory;

use claude_print::cli::Cli;
use claude_print::config::Config;
use claude_print::poller::{cwd_to_slug, derive_transcript_path, projects_dir_for_cwd};

/// The normative section's heading in `docs/notes/config-file-contract.md`.
const DOC_HEADING: &str = "Claude Code state and `CLAUDE_CONFIG_DIR`";

/// The README summary's heading, a `###` subsection of `## Configuration`.
const README_TITLE: &str = "Claude Code state (`CLAUDE_CONFIG_DIR`)";

/// Env is process-global; the HOME-rooting replay takes this lock so its
/// panics cannot poison the environment for any other test in this binary
/// (same discipline as `tests/config_contract.rs`).
static PROCESS_LOCK: Mutex<()> = Mutex::new(());

fn process_lock() -> MutexGuard<'static, ()> {
    PROCESS_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

// ── doc access ───────────────────────────────────────────────────────────────

/// The repo root, resolved from the *runtime* `CARGO_MANIFEST_DIR` with the
/// compile-time value as fallback — the same dance as
/// `tests/docs_pool_contract.rs`: the compile-time value alone bakes the
/// building checkout's path into the test binary, and a reused shared-cache
/// binary then reads the docs of a tree that no longer exists. Runtime
/// resolution always reads the docs of the tree under test.
fn repo_root() -> PathBuf {
    PathBuf::from(
        std::env::var("CARGO_MANIFEST_DIR")
            .unwrap_or_else(|_| env!("CARGO_MANIFEST_DIR").to_string()),
    )
}

fn read_doc(rel: &str) -> String {
    let path = repo_root().join(rel);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {rel}: {e}"))
}

/// The body of a `## <heading>` section: from just after the heading line
/// to the next `## ` heading (or EOF). Subsections (`###`) stay inside it.
fn section<'a>(md: &'a str, heading: &str) -> &'a str {
    let marker = format!("## {heading}");
    // The heading must be a whole line, not a prefix of a longer one.
    let start = md
        .find(&format!("\n{marker}\n"))
        .map(|p| p + 1)
        .or_else(|| md.starts_with(&marker).then_some(0))
        .unwrap_or_else(|| panic!("heading '{marker}' not found"));
    let body = md[start + marker.len()..]
        .strip_prefix('\n')
        .unwrap_or(&md[start + marker.len()..]);
    let end = body.find("\n## ").unwrap_or(body.len());
    &body[..end]
}

/// A `### <title>` subsection of an extracted section, from its heading up
/// to the next `###` heading.
fn subsection<'a>(section: &'a str, title: &str) -> &'a str {
    let heading = format!("### {title}");
    let start = section
        .find(&heading)
        .unwrap_or_else(|| panic!("section must carry a `{heading}` subsection"));
    let rest = &section[start..];
    let end = rest.find("\n### ").unwrap_or(rest.len());
    &rest[..end]
}

/// Collapse every run of whitespace to a single space, so a phrase can be
/// matched across the markdown source's line wrapping without pinning where
/// the author happened to break the line.
fn normalized(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The named `const NAME: &[..] = &[...]` block of a source file, scoped to
/// its own text: a bare file-wide `contains` would be satisfied by mentions
/// in comments or unit-test fixtures elsewhere in the file, so removing the
/// real entry would not fail it (same scoping discipline as
/// `tests/claude_config_dir_contract.rs`).
fn const_block<'a>(source: &'a str, name: &str) -> &'a str {
    source
        .split(&format!("const {name}"))
        .nth(1)
        .unwrap_or_else(|| panic!("source must still define const {name}"))
        .split("];")
        .next()
        .unwrap_or_else(|| panic!("the {name} block must be terminated"))
}

// ── the two documented sections, extracted and normalized ───────────────────

fn contract_section() -> String {
    let doc = read_doc("docs/notes/config-file-contract.md");
    section(&doc, DOC_HEADING).to_string()
}

fn readme_summary() -> String {
    let readme = read_doc("README.md");
    let configuration = section(&readme, "Configuration");
    subsection(configuration, README_TITLE).to_string()
}

// ── 1. the normative section's claims ────────────────────────────────────────

/// Every contract-bearing sentence of the contract doc's section is present
/// (scoped to the section, whitespace-normalized), the section's "Pinned
/// by" self-reference is honored, and every file it names exists. If a
/// sentence is reworded, this fails — that is the point: rewording the
/// contract means updating this test in the same commit, so the wording
/// cannot silently stop describing the implementation.
#[test]
fn contract_doc_section_carries_the_documented_claims() {
    let text = normalized(&contract_section());

    // The wrapper/state boundary this section exists to draw.
    for phrase in [
        "The file this contract describes configures the wrapper only.",
        "session transcripts under \
         `$HOME/.claude/projects/<cwd-slug>/<session-id>.jsonl`.",
        "claude-print never sets it.",
        "`CLAUDE_CONFIG_DIR` is an entry in `SCRUBBED_ENV` (`src/pty.rs`), \
         the one list the pre-fork child-env builder filters through.",
        "The per-run temp directory exists solely for the Stop-hook \
         settings injection (`src/hook.rs`) and never redirects the config dir.",
        "(`derive_transcript_path`, from `session_id` + `cwd`) and the \
         stream-json live reader's binding (`projects_dir_for_cwd`) both root \
         at `get_home()` and cannot follow a redirect.",
        "`scripts/check-billing.sh` inspects the newest transcript under \
         `~/.claude/projects/`",
        "Relocating Claude Code state therefore means setting `HOME`",
        "No flag, config key, or environment variable redirects the config \
         dir through claude-print.",
        "The behavioral invariant is enforced by \
         `tests/claude_config_dir_contract.rs`",
    ] {
        assert!(
            text.contains(phrase),
            "the contract doc's {DOC_HEADING:?} section must carry: {phrase:?}\n\
             section:\n{text}"
        );
    }

    // The doc's "Pinned by" row must keep naming this guard, so a reader of
    // the contract can find the test that enforces its wording.
    let doc = read_doc("docs/notes/config-file-contract.md");
    assert!(
        doc.contains("tests/claude_config_dir_docs_contract.rs"),
        "the contract doc's Pinned-by table must name this test"
    );

    // Every file the section cites must exist — a moved file turns the
    // contract's citations into dead references.
    for rel in [
        "src/hook.rs",
        "src/pty.rs",
        "src/poller.rs",
        "scripts/check-billing.sh",
        "docs/notes/home-handling-strategy.md",
        "tests/claude_config_dir_contract.rs",
    ] {
        assert!(
            repo_root().join(rel).exists(),
            "the contract doc cites {rel}, which must exist in the repo"
        );
    }
}

// ── 2. the README summary agrees ─────────────────────────────────────────────

/// The README's summary must live inside `## Configuration`, carry the same
/// claims as the contract doc (same identifiers for the same facts — the
/// contract doc declares itself normative, so a summary naming a different
/// const or script is a drift, not a paraphrase), and its one link target
/// must exist.
#[test]
fn readme_summary_agrees_with_the_contract_doc() {
    let readme = read_doc("README.md");
    let summary = normalized(&readme_summary());

    for phrase in [
        "The rules above configure the wrapper, not Claude Code itself.",
        "session transcripts under `$HOME/.claude/projects/`.",
        "`claude-print` never sets it.",
        "(`SCRUBBED_ENV` in `src/pty.rs`)",
        "(`derive_transcript_path` and `projects_dir_for_cwd` in `src/poller.rs`;",
        "`scripts/check-billing.sh` inspects the newest transcript under \
         `~/.claude/projects/`",
        "The invariant is enforced by `tests/claude_config_dir_contract.rs`.",
        "No flag, config key, or environment variable redirects the config dir.",
        "Full contract: [`docs/notes/config-file-contract.md`](docs/notes/config-file-contract.md),",
    ] {
        assert!(
            summary.contains(phrase),
            "the README's {README_TITLE:?} summary must carry: {phrase:?}\n\
             summary:\n{summary}"
        );
    }

    // Identifier-level agreement with the normative section: both documents
    // must name the same enforcement points for the same claims.
    let contract = normalized(&contract_section());
    for identifier in [
        "`SCRUBBED_ENV`",
        "`src/pty.rs`",
        "`derive_transcript_path`",
        "`projects_dir_for_cwd`",
        "`scripts/check-billing.sh`",
        "`tests/claude_config_dir_contract.rs`",
        "`$HOME/.claude/projects/",
    ] {
        assert!(
            contract.contains(identifier),
            "contract doc missing {identifier} (README names it — drift)"
        );
        assert!(
            summary.contains(identifier),
            "README summary missing {identifier} (contract doc names it — drift)"
        );
    }

    // The relocation pointer: the link anchor must resolve to a real README
    // heading (GitHub derives `#home-in-containers-and-chroots` from
    // "### HOME in containers and chroots").
    assert!(
        summary.contains("[HOME in containers and chroots](#home-in-containers-and-chroots)"),
        "the README summary must point relocation at the HOME section"
    );
    assert!(
        readme
            .lines()
            .any(|l| l.trim() == "### HOME in containers and chroots"),
        "the linked heading '### HOME in containers and chroots' must exist in README"
    );
}

// ── 3. the source-level claims ───────────────────────────────────────────────

/// The doc's two directional rules, checked against the one file that
/// builds the child environment: `CLAUDE_CONFIG_DIR` is scrubbed (present
/// in `SCRUBBED_ENV`) and never forced (absent from `FORCED_ENV`, the only
/// variables claude-print injects). The `FORCED_ENV` membership is pinned
/// exactly so adding a variable there requires updating this test — and the
/// doc sentence that says claude-print never sets the config dir.
#[test]
fn env_lists_scrub_and_never_force_claude_config_dir() {
    let pty_source = read_doc("src/pty.rs");

    let scrubbed = const_block(&pty_source, "SCRUBBED_ENV");
    assert!(
        scrubbed.contains("\"CLAUDE_CONFIG_DIR\","),
        "the doc claims CLAUDE_CONFIG_DIR is scrubbed via SCRUBBED_ENV \
         (src/pty.rs); the const block no longer carries it:\n{scrubbed}"
    );

    let forced = const_block(&pty_source, "FORCED_ENV");
    assert!(
        !forced.contains("CLAUDE_CONFIG_DIR"),
        "the doc claims claude-print never sets CLAUDE_CONFIG_DIR, but \
         FORCED_ENV mentions it:\n{forced}"
    );
    // The forced set is small and each entry is its own invariant (billing
    // entrypoint, session persistence) — pin the exact membership, so a
    // new forced variable requires updating this pin and the doc's
    // never-sets-it sentence together.
    let entries: Vec<&str> = forced.lines().filter(|l| l.contains("(\"")).collect();
    assert_eq!(
        entries,
        [
            "    (\"CLAUDE_CODE_ENTRYPOINT\", \"cli\"),",
            "    (\"CLAUDE_CODE_FORCE_SESSION_PERSISTENCE\", \"1\"),",
        ],
        "FORCED_ENV membership changed — update this pin, the pty.rs doc \
         comment, and the doc's never-sets-it sentence together"
    );

    // The per-run temp directory: the doc says it exists solely for the
    // Stop-hook settings injection and never redirects the config dir.
    let hook_source = read_doc("src/hook.rs");
    assert!(
        hook_source.contains("settings.json"),
        "src/hook.rs is cited as the Stop-hook settings injection — it must \
         still write settings.json"
    );
    assert!(
        hook_source.contains("tempdir"),
        "src/hook.rs is cited for the per-run temp directory — it must still \
         create one"
    );
    assert!(
        !hook_source.contains("CLAUDE_CONFIG_DIR"),
        "the doc claims the per-run temp directory never redirects the \
         config dir, but src/hook.rs mentions CLAUDE_CONFIG_DIR"
    );

    // The billing check the doc says watches the HOME-rooted tree.
    let billing = read_doc("scripts/check-billing.sh");
    assert!(
        billing.contains(".claude/projects"),
        "the doc claims scripts/check-billing.sh inspects transcripts under \
         ~/.claude/projects/ — the script no longer references that tree"
    );
}

// ── 4. the HOME-rooting claim, behaviorally ─────────────────────────────────

/// Captures `HOME` and `CLAUDE_CONFIG_DIR` and restores them on drop —
/// including unwinding through a failed assertion — so the replay leaves
/// the environment as it found it for the rest of this test binary.
struct EnvGuard {
    prev_home: Option<OsString>,
    prev_config_dir: Option<OsString>,
}

impl EnvGuard {
    fn capture() -> Self {
        Self {
            prev_home: std::env::var_os("HOME"),
            prev_config_dir: std::env::var_os("CLAUDE_CONFIG_DIR"),
        }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        match self.prev_home.take() {
            Some(value) => std::env::set_var("HOME", value),
            None => std::env::remove_var("HOME"),
        }
        match self.prev_config_dir.take() {
            Some(value) => std::env::set_var("CLAUDE_CONFIG_DIR", value),
            None => std::env::remove_var("CLAUDE_CONFIG_DIR"),
        }
    }
}

/// "HOME-rooted by construction and cannot follow a redirect" is a claim
/// about behavior, so it is replayed behaviorally: under a guarded throwaway
/// `HOME` and a decoy inherited `CLAUDE_CONFIG_DIR` pointing elsewhere,
/// both documented readers must derive their paths under
/// `$HOME/.claude/projects/` — in the exact documented shape — and never
/// under the decoy. A reader that ever consulted `CLAUDE_CONFIG_DIR` (or
/// grew any other root) fails here.
///
/// This is the only test in this binary that reads or writes either
/// variable, so it cannot race the parallel tests here; the process lock
/// keeps a panic from poisoning the environment mid-restore.
#[test]
fn transcript_readers_are_home_rooted_and_ignore_a_config_dir_redirect() {
    let _lock = process_lock();
    let _guard = EnvGuard::capture();

    let home = tempfile::TempDir::new().expect("throwaway HOME");
    let decoy = tempfile::TempDir::new().expect("decoy CLAUDE_CONFIG_DIR root");
    std::env::set_var("HOME", home.path());
    std::env::set_var("CLAUDE_CONFIG_DIR", decoy.path());

    // The doc's shape claim, character for character:
    // `$HOME/.claude/projects/<cwd-slug>/<session-id>.jsonl`. The slug is
    // derived through the real folder so this test pins the *layout*, not
    // a copy of the folding rules.
    const SESSION_ID: &str = "docs-contract-pin-session";
    let cwd = "/home/coding/claude-print";
    let derived = derive_transcript_path(SESSION_ID, cwd)
        .expect("derivation must succeed under a valid writable HOME");
    let expected = home
        .path()
        .join(".claude")
        .join("projects")
        .join(cwd_to_slug(cwd).expect("slug"))
        .join(format!("{SESSION_ID}.jsonl"));
    assert_eq!(
        derived, expected,
        "derive_transcript_path must produce the documented \
         $HOME/.claude/projects/<cwd-slug>/<session-id>.jsonl shape"
    );

    // The stream-json live reader's binding: the projects directory for the
    // *current* working directory, HOME-rooted.
    let projects =
        projects_dir_for_cwd().expect("projects dir must succeed under a valid writable HOME");
    let live_cwd = std::env::current_dir().expect("current dir");
    let expected_projects = home
        .path()
        .join(".claude")
        .join("projects")
        .join(cwd_to_slug(&live_cwd.to_string_lossy()).expect("slug"));
    assert_eq!(
        projects, expected_projects,
        "projects_dir_for_cwd must derive from HOME and the live cwd only"
    );

    // Neither path may touch the decoy — an inherited redirect cannot move
    // the tree these readers watch, which is why the wrapper must scrub it.
    for path in [&derived, &projects] {
        assert!(
            !path.starts_with(decoy.path()),
            "doc claim 'cannot follow a redirect' violated: {path:?} roots \
             under the decoy CLAUDE_CONFIG_DIR"
        );
    }
}

// ── 5. no redirection surface ────────────────────────────────────────────────

/// Gather every long flag of the parser, root command and subcommands
/// included, via clap's own definitions — the same surface `--help`
/// renders.
fn all_long_flags(cmd: &clap::Command, out: &mut Vec<String>) {
    for arg in cmd.get_arguments() {
        if let Some(long) = arg.get_long() {
            out.push(long.to_string());
        }
    }
    for sub in cmd.get_subcommands() {
        all_long_flags(sub, out);
    }
}

/// "No flag, config key, or environment variable redirects the config dir
/// through claude-print", checked against all three surfaces the sentence
/// names:
///
/// - **flag** — the only long flags containing `config` are the wrapper's
///   own `--config` (the TOML file this contract describes) and
///   `--mcp-config` (forwarded to the child, never an env redirect of
///   claude's state). A future `--claude-config-dir` fails the set;
/// - **config key** — the closed-world `[defaults]` schema rejects a
///   `claude_config_dir` key at parse time, naming the four real keys;
/// - **environment variable** — `FORCED_ENV` carries no
///   `CLAUDE_CONFIG_DIR` (pinned by the env-lists test above; repeated
///   cheaply here so this test alone covers its doc sentence).
#[test]
fn no_flag_or_config_key_redirects_the_config_dir() {
    // Flag surface.
    let mut longs = Vec::new();
    all_long_flags(&Cli::command(), &mut longs);
    longs.sort();
    longs.dedup();
    let config_flags: Vec<&str> = longs
        .iter()
        .map(String::as_str)
        .filter(|l| l.contains("config"))
        .collect();
    assert_eq!(
        config_flags,
        ["config", "mcp-config"],
        "the doc claims no flag redirects the config dir; the set of long \
         flags containing 'config' changed — update the flag, this pin, and \
         the doc sentence together"
    );

    // Config-key surface: the loader's own closed-world rejection.
    let bad = tempfile::tempdir().expect("tempdir");
    let bad_path = bad.path().join("claude-config-dir-key.toml");
    fs::write(
        &bad_path,
        "[defaults]\nclaude_config_dir = \"/decoy/.claude\"\n",
    )
    .expect("write bad config");
    let err = Config::load_or_default(&bad_path)
        .expect_err("a claude_config_dir key must be rejected by the closed-world schema");
    let message = err.to_string();
    assert!(
        message.contains("unknown field"),
        "the rejection must be the closed-world unknown-field error, got: {message}"
    );
    assert!(
        message.contains("`claude_config_dir`"),
        "the error must name the rejected key, got: {message}"
    );
    for key in ["inherit_hooks", "model", "max_turns", "timeout_secs"] {
        assert!(
            message.contains(key),
            "the closed-world error must still list the real key {key}: {message}"
        );
    }

    // Environment-variable surface.
    let pty_source = read_doc("src/pty.rs");
    let forced = const_block(&pty_source, "FORCED_ENV");
    assert!(
        !forced.contains("CLAUDE_CONFIG_DIR"),
        "FORCED_ENV must not set CLAUDE_CONFIG_DIR:\n{forced}"
    );
}

// ── 6. the cited behavioral twin ─────────────────────────────────────────────

/// The doc and README both defer the invariant's *enforcement* to
/// `tests/claude_config_dir_contract.rs`; that citation must keep pointing
/// at a test that actually asserts the scrub — a renamed or hollowed-out
/// twin would leave both documents citing a guard that no longer guards.
#[test]
fn cited_behavioral_twin_enforces_the_scrub() {
    let twin = read_doc("tests/claude_config_dir_contract.rs");
    for marker in [
        "CLAUDE_CONFIG_DIR",
        "SCRUBBED_ENV",
        "binary_transcript_lands_in_real_config_dir_despite_inherited_decoy",
        "binary_child_env_drops_inherited_claude_config_dir",
    ] {
        assert!(
            twin.contains(marker),
            "tests/claude_config_dir_contract.rs is cited by both docs as \
             the invariant's enforcement; it no longer carries {marker:?}"
        );
    }
    // And it must stay cargo-discovered: `tests/*.rs` is cargo's
    // integration-test root, and this file's own execution alongside the
    // twin is what keeps both citations non-vacuous.
    let path = Path::new("tests/claude_config_dir_contract.rs");
    assert_eq!(
        path.extension().and_then(|e| e.to_str()),
        Some("rs"),
        "the behavioral twin must stay a cargo-discovered integration test"
    );
}
