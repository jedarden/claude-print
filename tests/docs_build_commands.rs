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
//! Four surfaces, all pinned here:
//!
//! - **§"Build commands" presence** — the bolded prohibition sentence,
//!   the `cargo test --tests` alternative it prescribes in the same
//!   breath, the named single-target selector guidance, the exact
//!   all-targets line in the section's command fence, and the exact
//!   doctest line beside it (`--tests` skips doctests, so a fence that
//!   stops at the all-targets line documents a verification workflow
//!   that never runs the doc examples — claudepr-adaad914); and the
//!   fence itself may never prescribe a wildcard selector — the fence
//!   is the copy-paste surface.
//! - **Repo-wide reintroduction scan** — every file of AGENTS.md,
//!   README.md, `docs/`, `scripts/`, and the vendored CI
//!   `claude-print-ci-workflowtemplate.yml` is parsed with a selector
//!   grammar (`--test NAME`, `--test=NAME`, surrounding quotes stripped)
//!   and any selector carrying a glob metacharacter (`*`, `?`, `[`)
//!   fails the build. The single sanctioned occurrence is the AGENTS.md
//!   prohibition sentence itself, anchored by content (the exact bolded
//!   lead), not by file or line — so the exemption cannot hide a
//!   prescription: a negative meta-test re-flags the hazardous line the
//!   moment the prohibition lead leaves it.
//! - **CI WorkflowTemplate test commands** (claudepr-76a5274c) — the
//!   vendored template is the one prohibited-form surface that does not
//!   merely get copied but *executes*, so it carries its own pin on top
//!   of the scan: its suite invocation must use the documented
//!   `cargo test --tests` form, and a commented-out invocation satisfies
//!   nothing (a `#`-prefixed line does not run). Because `--tests`
//!   skips doctests, the invocation set must also carry an uncommented
//!   `--doc` leg (claudepr-adaad914) — otherwise CI could drop the
//!   doctest run and stay green, the silent omission a presence pin is
//!   for.
//! - **§"Verification execution modes"** (claudepr-39f53ae4) — the
//!   subsection that defines how a verification run's *mode* is
//!   identified and recorded. Workspace guidance already distinguishes
//!   remote iad-ci verification from the local fallback and requires
//!   evidence to report targeted vs complete coverage; the subsection is
//!   the repository's operational half of that requirement — the mode is
//!   identified from the wrapper's own `[cargo-remote]` output lines
//!   (never intent), and recorded in evidence *prose* (never annotated
//!   onto the shell-executed `verified:` block lines, where a
//!   parenthetical is a parse error that fails the gate). Complete is
//!   pinned as the same two-leg split the fence prescribes: `--tests`
//!   *and* `--doc`.
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
//! `docs/`, `scripts/`, and the CI WorkflowTemplate; spawns nothing.

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

/// The doctest form the section's command fence must carry beside it.
/// `--tests` selects unit + integration targets and skips doctests
/// entirely, so a fence that stops at [`ALL_TARGETS_COMMAND`] documents a
/// verification workflow that never runs the doc examples — the same
/// two-leg split the CI template executes (claudepr-adaad914).
const DOC_TEST_COMMAND: &str = "cargo test --doc";

/// The heading of the §"Build commands" subsection pinning the
/// verification execution-mode contract (bead claudepr-39f53ae4).
/// Workspace guidance already distinguishes remote iad-ci verification
/// from the local cgroup-limited fallback and requires evidence to report
/// whether a verification was targeted or complete; the subsection is
/// where the repository defines the operational halves of that
/// requirement — how to *identify* the mode a run took (the wrapper's own
/// `[cargo-remote]` output lines, never intent) and how to *record* it
/// (evidence prose, never the shell-executed `verified:` block lines).
const VERIFICATION_MODES_HEADING: &str = "### Verification execution modes";

/// The execution-axis lead: the bullet that names the where-did-it-run
/// axis. Without a named axis the remote/local reporting requirement has
/// no hook to hang identification guidance on.
const REMOTE_AXIS_LEAD: &str = "**Remote or local — where the run executed.**";

/// The one line every local fallback prints — `~/.local/bin/cargo-remote`
/// routes *every* fallback reason (no remote, uncommitted changes, failed
/// push, failed submission) through a single `local_limited` that emits
/// this first. It is therefore the identification tell for the local half
/// of the mode axis: present in the output, the run executed locally under
/// the cgroup limits.
const LOCAL_FALLBACK_TELL: &str = "[cargo-remote] falling back to local";

/// The remote success line — the terminal `[cargo-remote] PASSED` (or
/// `FAILED`) only a run that actually submitted to the `rust-verify`
/// WorkflowTemplate on iad-ci and streamed its logs can print. The
/// identification tell for the remote half of the mode axis.
const REMOTE_PASSED_TELL: &str = "[cargo-remote] PASSED";

/// The coverage-axis lead: the bullet that names the what-did-it-cover
/// axis — targeted selectors vs the complete two-leg split.
const COVERAGE_AXIS_LEAD: &str = "**Targeted or complete — what the run covered.**";

/// The definition of complete verification, as one joined fragment: both
/// fence legs, conjunctively. Pinning the *join* (not just each command
/// separately, which the §"Build commands" pins above already cover) is
/// what makes "complete" unfakeable — degrade the conjunction to a single
/// leg and this fragment is gone even though both commands still appear
/// somewhere in the section.
const COMPLETE_SPLIT: &str = "`cargo test --tests` *and* `cargo test --doc`";

/// The recording requirement for targeted verification: the evidence must
/// name every selector it ran, so "targeted" is a verifiable claim, not a
/// euphemism for "partial".
const TARGETED_NAMING_RULE: &str = "must name every selector it ran";

/// The recording rule's anchor: mode and coverage travel in the prose
/// around a fenced `verified:` block, never inside its lines — those
/// lines are shell-executed verbatim by the close gate, so an annotation
/// like `exit=0 (remote, complete)` on a command line is a parse error
/// that fails the gate, not evidence.
const RECORDING_RULE: &str = "lines of a fenced `verified:` block";

/// Glob metacharacters that make a `--test` selector fragile: each crosses
/// the fleet's TEST_ARGS flatten as a literal and glob-expands in the
/// unquoted re-expansion. A named selector carries none, which is exactly
/// why it is the sanctioned single-target spelling.
const GLOB_METACHARS: [char; 3] = ['*', '?', '['];

/// The vendored CI WorkflowTemplate this guard also pins (bead
/// claudepr-76a5274c): the declarative-config copy is deployed from here,
/// and the embedded bash is the one surface the prohibition covers that
/// does not merely get copy-pasted but executes — so a hazardous or
/// undocumented suite spelling in it is the CI's own drift, not just a
/// reader's trap.
const CI_WORKFLOW_FILE: &str = "claude-print-ci-workflowtemplate.yml";

/// The documented all-targets selector token the CI suite invocation must
/// carry — the same `--tests` the AGENTS.md §"Build commands" fence
/// prescribes for every test target.
const ALL_TARGETS_SELECTOR: &str = "--tests";

/// The doctest selector token the CI template's invocation set must carry
/// on an uncommented `cargo test` line — the executing half of the same
/// split [`DOC_TEST_COMMAND`] documents. Without this pin the `--doc` leg
/// could disappear from CI and nothing would fail: `--tests` stays green
/// precisely because it never ran doctests (claudepr-adaad914).
const DOC_TEST_SELECTOR: &str = "--doc";

/// The repo-wide scan scope: AGENTS.md itself (whose single sanctioned
/// occurrence is the prohibition sentence) plus the operational surfaces
/// the beads name — README.md, every file under `docs/`, every file under
/// `scripts/` (any extension, recursively), and the vendored CI
/// WorkflowTemplate (claudepr-76a5274c), whose embedded bash is a command
/// surface like any script under `scripts/`.
const SCAN_FILES: [&str; 3] = ["AGENTS.md", "README.md", CI_WORKFLOW_FILE];
const SCAN_DIRS: [&str; 2] = ["docs", "scripts"];

/// Read a repo file from the checkout under test, resolving the root
/// through [`repo_root`]'s runtime-first candidate chain.
fn repo_file(relative: &str) -> String {
    fs::read_to_string(repo_root().join(relative))
        .unwrap_or_else(|e| panic!("read {relative} from the checkout under test: {e}"))
}

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
/// claudepr-270570be; the same chain as `tests/install_sh.rs`). Candidates, most
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

fn agents_md() -> String {
    repo_file("AGENTS.md")
}

/// The slice of `doc` from the `heading` line up to the next markdown
/// heading *outside any fenced code block*. The sections pinned here carry
/// bash fences full of `#`-comment lines, so a fence-blind heading cut
/// would end a section at the first fence comment; the fence state is
/// tracked for exactly that reason. Pure over `doc` so the negative
/// meta-tests can mutate it in memory. Panics when the heading is gone —
/// a missing section is drift, not a pass.
fn section_after_heading(doc: &str, heading: &str) -> String {
    let lines: Vec<&str> = doc.lines().collect();
    let start = lines
        .iter()
        .position(|l| l.trim() == heading)
        .unwrap_or_else(|| {
            panic!(
                "AGENTS.md must keep the {heading:?} heading — this guard \
                 scopes its pins to that section"
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

/// The §"Build commands" slice of `doc`, located by heading so the guard
/// stays immune to surrounding edits.
fn build_commands_section(doc: &str) -> String {
    section_after_heading(doc, BUILD_COMMANDS_HEADING)
}

/// The §"Verification execution modes" slice of `doc` — the subsection
/// this guard's mode pins (claudepr-39f53ae4) scope to.
fn verification_modes_section(doc: &str) -> String {
    section_after_heading(doc, VERIFICATION_MODES_HEADING)
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
    assert!(
        fence.iter().any(|l| l.trim() == DOC_TEST_COMMAND),
        "the section's command fence must carry the exact `{DOC_TEST_COMMAND}` line — \
         `{ALL_TARGETS_COMMAND}` skips doctests, so a fence that stops there documents \
         a verification workflow that never runs the doc examples; the doctest leg is \
         part of the documented workflow, not an optional extra (claudepr-adaad914)"
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

/// The §"Verification execution modes" contract, checkable against
/// caller-supplied section text (the live section for the always-on test,
/// mutated text for the negative meta-tests). Panics on drift. Every
/// fragment is load-bearing: the two axis leads name what must be
/// reported, the two `[cargo-remote]` tells are how the mode is
/// identified from output, the joined split defines "complete", and the
/// recording rules say where the report goes (evidence prose) and what a
/// targeted report must carry (every selector, by name).
fn check_verification_modes_section(section: &str) {
    assert!(
        section.contains(REMOTE_AXIS_LEAD),
        "the modes section must keep its execution-axis lead {REMOTE_AXIS_LEAD:?} — \
         without the axis named, the remote/local reporting requirement has no \
         hook to hang the identification tells on"
    );
    assert!(
        section.contains(LOCAL_FALLBACK_TELL),
        "the modes section must keep the local-fallback tell {LOCAL_FALLBACK_TELL:?} — \
         it is the one line every cgroup-limited fallback prints, so it is how a \
         reader identifies the local mode from the run's output instead of \
         assuming intent (a clean tree that merely *should* have gone remote can \
         still fall back, e.g. on a failed push)"
    );
    assert!(
        section.contains(REMOTE_PASSED_TELL),
        "the modes section must keep the remote success tell {REMOTE_PASSED_TELL:?} — \
         only a run that actually submitted to rust-verify on iad-ci and streamed \
         its logs can print it, so it is how a reader identifies the remote mode"
    );
    assert!(
        section.contains(COVERAGE_AXIS_LEAD),
        "the modes section must keep its coverage-axis lead {COVERAGE_AXIS_LEAD:?} — \
         without the axis named, the targeted-vs-complete reporting requirement \
         has no hook to hang the split definition on"
    );
    assert!(
        section.contains(COMPLETE_SPLIT),
        "the modes section must keep complete verification defined as \
         {COMPLETE_SPLIT:?}, joined — both legs, conjunctively. The §\"Build \
         commands\" pins establish that each command stays in the fence; this \
         fragment is what the mode definition itself cannot lose: degrade the \
         conjunction to a single leg and \"complete\" silently redefines as \
         `--tests` alone, which skips doctests and has not run the suite"
    );
    assert!(
        section.contains(TARGETED_NAMING_RULE),
        "the modes section must keep the targeted-recording rule \
         ({TARGETED_NAMING_RULE:?}) — without it a targeted report can claim \
         \"targeted\" while naming nothing, and the coverage axis becomes \
         unverifiable"
    );
    assert!(
        section.contains(RECORDING_RULE),
        "the modes section must keep the recording rule anchored on \
         {RECORDING_RULE:?} — the verified-block lines are shell-executed \
         verbatim, so the mode must travel in the surrounding prose, never as \
         an annotation on an executed command line (an annotation there is a \
         parse error that fails the gate, not evidence)"
    );
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

/// Whether `line` is a comment — `#` after indentation, which covers both
/// the YAML comments and the `#` lines of the bash embedded in the
/// WorkflowTemplate's args. A commented-out command does not execute, so
/// it must satisfy no presence pin.
fn is_comment_line(line: &str) -> bool {
    line.trim_start().starts_with('#')
}

/// Whether `line` invokes `cargo test`: a non-comment line whose
/// whitespace tokens carry `cargo` and `test` consecutively. Token
/// boundaries keep quote-glued mentions (`echo "cargo test"`) out, and
/// multi-command lines (`cargo build ... && cargo test ...`) in.
fn invokes_cargo_test(line: &str) -> bool {
    if is_comment_line(line) {
        return false;
    }
    let tokens: Vec<&str> = line.split_whitespace().collect();
    tokens.windows(2).any(|w| w[0] == "cargo" && w[1] == "test")
}

/// The CI WorkflowTemplate's test-invocation contract, checkable against
/// caller-supplied template text (the live file for the always-on test,
/// mutated text for the negative meta-tests). Panics on drift, in the
/// order that names the most specific defect first:
///
/// 1. the template must invoke `cargo test` at all — a CI template that
///    never runs the suite has nothing to pin the selector contract onto;
/// 2. no invocation may carry a wildcard `--test` selector — the template
///    executes, so the hazardous spelling here is not copy-paste risk but
///    the CI's own drift;
/// 3. at least one invocation must carry the documented all-targets
///    selector `--tests` — the same sanctioned spelling the AGENTS.md
///    fence prescribes, modeled by the one surface that runs it;
/// 4. the invocation set must also carry the doctest selector `--doc` on
///    an uncommented line — `--tests` skips doctests, so a template that
///    runs only the all-targets leg stays green while never running the
///    doc examples (claudepr-adaad914).
fn check_ci_workflow_test_commands(content: &str) {
    let invocations: Vec<&str> = content
        .lines()
        .filter(|line| invokes_cargo_test(line))
        .collect();
    assert!(
        !invocations.is_empty(),
        "{CI_WORKFLOW_FILE} must invoke `cargo test` — a CI template that never runs \
         the suite has nothing to pin the selector contract onto"
    );
    for line in &invocations {
        if let Some(selector) = hazardous_selector(line) {
            panic!(
                "{CI_WORKFLOW_FILE} runs the wildcard `--test` selector {selector:?} \
                 (line: {:?}) — the workflow command executes, and AGENTS.md \
                 §\"Build commands\" prohibits that form even where stock Cargo \
                 would glob-resolve it. Use `{ALL_TARGETS_COMMAND}` (every target) \
                 or a named `--test <name>` selector (one target)",
                line.trim()
            );
        }
    }
    assert!(
        invocations
            .iter()
            .any(|line| line.split_whitespace().any(|t| t == ALL_TARGETS_SELECTOR)),
        "the CI suite invocation in {CI_WORKFLOW_FILE} must use the documented \
         all-targets form `{ALL_TARGETS_COMMAND}` — the WorkflowTemplate is the \
         copy-paste surface that executes, and AGENTS.md §\"Build commands\" names \
         `{ALL_TARGETS_COMMAND}` as the sanctioned all-targets spelling; drifting \
         the CI command off it re-opens the exact gap this guard exists to close"
    );
    assert!(
        invocations
            .iter()
            .any(|line| line.split_whitespace().any(|t| t == DOC_TEST_SELECTOR)),
        "the CI invocation set in {CI_WORKFLOW_FILE} must include a `{DOC_TEST_COMMAND}` \
         leg beside `{ALL_TARGETS_COMMAND}` (claudepr-adaad914) — `--tests` skips \
         doctests, so a template that runs only the all-targets leg stays green while \
         the doc examples silently stop running, which is exactly the omission this \
         presence pin exists to make impossible; a commented-out `--doc` line does \
         not execute and satisfies nothing"
    );
}

/// The scan scope as `(repo-relative path, content)` pairs: the root files
/// (SCAN_FILES) plus every regular file under the scan directories,
/// recursively.
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
        "the scan scope (AGENTS.md, README.md, the CI WorkflowTemplate, docs/, \
         scripts/) came back empty — the guard cannot be vacuously passing off \
         an unread tree"
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

#[test]
fn verification_modes_section_defines_identification_and_recording() {
    check_verification_modes_section(&verification_modes_section(&agents_md()));
}

#[test]
fn ci_workflowtemplate_test_commands_follow_the_selector_contract() {
    check_ci_workflow_test_commands(&repo_file(CI_WORKFLOW_FILE));
    // The template is scan scope too, not just pinned in isolation: a
    // wildcard planted anywhere in it — command, comment, or prose — must
    // fail the repo-wide scan test above, so its membership is pinned
    // here rather than left to SCAN_FILES staying honest on its own.
    assert!(
        scanned_files()
            .iter()
            .any(|(rel, _)| rel == CI_WORKFLOW_FILE),
        "{CI_WORKFLOW_FILE} must be inside the repo-wide scan scope — its \
         embedded bash is a command surface like docs/ and scripts/"
    );
}

// ── Negative meta-tests: the guard must FAIL when its inputs rot ─────────────
//
// Every leg mutates the live document in memory — nothing is written to
// disk — and requires the owning check to panic naming the drift, the
// committed non-vacuity pattern of `tests/docs_build_layout.rs`
// (claudepr-4d967120).

/// Stripping any pinned fragment of the §"Verification execution modes"
/// contract fails the owning presence check — as does every drift shape
/// the mode definition specifically exists to prevent: the complete-split
/// conjunction degraded to a single leg (both commands still present, the
/// *definition* quietly redefined), the recording rule pointed into the
/// executed block lines, and the heading renamed away (a missing section
/// is drift, not a pass — the extractor itself must panic).
#[test]
fn negative_meta_stripped_mode_fragments_fail_the_modes_pin() {
    let section = verification_modes_section(&agents_md());

    assert_drift(
        || {
            check_verification_modes_section(&replaced_once(
                &section,
                REMOTE_AXIS_LEAD,
                "**Where the run executed.**",
            ))
        },
        &["execution-axis lead"],
    );
    assert_drift(
        || {
            check_verification_modes_section(&replaced_once(
                &section,
                LOCAL_FALLBACK_TELL,
                "[cargo-remote] running locally",
            ))
        },
        &["local-fallback tell"],
    );
    assert_drift(
        || {
            check_verification_modes_section(&replaced_once(
                &section,
                REMOTE_PASSED_TELL,
                "[cargo-remote] OK",
            ))
        },
        &["remote success tell"],
    );
    assert_drift(
        || {
            check_verification_modes_section(&replaced_once(
                &section,
                COVERAGE_AXIS_LEAD,
                "**What the run covered.**",
            ))
        },
        &["coverage-axis lead"],
    );
    assert_drift(
        || {
            check_verification_modes_section(&replaced_once(
                &section,
                TARGETED_NAMING_RULE,
                "may run any selectors it likes",
            ))
        },
        &["targeted-recording rule"],
    );
    assert_drift(
        || {
            check_verification_modes_section(&replaced_once(
                &section,
                RECORDING_RULE,
                "lines of the executed block",
            ))
        },
        &["recording rule"],
    );

    // The conjunction degraded to a single leg: both commands still appear
    // in the section (each is pinned elsewhere in the fence), yet the
    // *definition* of complete now stops at `--tests` — which skips
    // doctests. Exactly the drift the joined fragment exists to catch.
    assert_drift(
        || {
            check_verification_modes_section(&replaced_once(
                &section,
                COMPLETE_SPLIT,
                "`cargo test --tests`",
            ))
        },
        &["joined", "both legs"],
    );

    // The heading renamed away: the extractor must fail loudly rather than
    // let the pins silently pass vacuously off a section that no longer
    // exists. (replaced_once over the whole document, since the heading is
    // what the extractor searches for.)
    let doc = agents_md();
    assert_drift(
        || {
            verification_modes_section(&replaced_once(
                &doc,
                VERIFICATION_MODES_HEADING,
                "### Verification modes",
            ));
        },
        &["must keep", "Verification execution modes"],
    );
}

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
    // The doctest line demoted to a fence comment: the documented workflow
    // stops at `--tests` and never runs the doctests `--tests` skips —
    // verification omitting doctests is the drift this leg exists to catch.
    assert_drift(
        || {
            check_build_commands_section(&replaced_once(
                &section,
                "\ncargo test --doc\n",
                "\n# cargo test --doc\n",
            ))
        },
        &["command fence", "cargo test --doc"],
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

/// The CI WorkflowTemplate pin fails on every drift shape it exists to
/// catch, and the planted wildcard also fails the repo-wide scan — the
/// template is scan scope, not an island with its own private grammar.
#[test]
fn negative_meta_ci_workflow_drift_fails_the_workflow_pin() {
    let template = repo_file(CI_WORKFLOW_FILE);
    // Sanity for the live state: the committed template passes both legs.
    check_ci_workflow_test_commands(&template);
    check_no_hazardous_selectors(CI_WORKFLOW_FILE, &template);

    // The documented all-targets form demoted back to the bare invocation
    // the template carried before the pin existed.
    assert_drift(
        || {
            check_ci_workflow_test_commands(&replaced_once(
                &template,
                "cargo test --tests --verbose",
                "cargo test --verbose",
            ))
        },
        &["documented all-targets form", ALL_TARGETS_COMMAND],
    );

    // The all-targets selector demoted to a single named target: the
    // sanctioned one-target spelling, but not the documented all-targets
    // form — CI quietly stopping at one suite is the silent gap the
    // presence leg exists to close, so the grammar's sanctioned negative
    // space must not satisfy it.
    assert_drift(
        || {
            check_ci_workflow_test_commands(&replaced_once(
                &template,
                "cargo test --tests --verbose",
                "cargo test --test docs_build_commands --verbose",
            ))
        },
        &["documented all-targets form"],
    );

    // A wildcard selector planted into the executing suite command.
    let planted = replaced_once(
        &template,
        "cargo test --tests --verbose",
        "cargo test --test '*' --verbose",
    );
    assert_drift(
        || check_ci_workflow_test_commands(&planted),
        &[CI_WORKFLOW_FILE, "wildcard `--test` selector"],
    );
    assert_drift(
        || check_no_hazardous_selectors(CI_WORKFLOW_FILE, &planted),
        &[CI_WORKFLOW_FILE, "wildcard `--test` selector"],
    );

    // The doctest leg demoted to a bare invocation: `--tests` still runs,
    // so the all-targets pin holds, yet doctests silently stop executing in
    // CI — the all-targets leg stays green precisely because it never ran
    // them, which is the omission the `--doc` presence leg exists to catch.
    assert_drift(
        || {
            check_ci_workflow_test_commands(&replaced_once(
                &template,
                "cargo test --doc --verbose",
                "cargo test --verbose",
            ))
        },
        &["`cargo test --doc` leg"],
    );

    // The doctest leg commented out: the line still spells the sanctioned
    // form, but a `#`-prefixed line does not execute and satisfies nothing
    // — counting it would let CI "keep" doctests it no longer runs.
    assert_drift(
        || {
            check_ci_workflow_test_commands(&replaced_once(
                &template,
                "cargo test --doc --verbose",
                "# cargo test --doc --verbose",
            ))
        },
        &["`cargo test --doc` leg"],
    );

    // The invocation commented out: the line still spells the sanctioned
    // form, but a `#`-prefixed line does not execute and satisfies nothing
    // — counting it would let a commented-out test step pass the pin.
    assert_drift(
        || {
            check_ci_workflow_test_commands(&replaced_once(
                &template,
                "cargo test --tests --verbose",
                "# cargo test --tests --verbose",
            ))
        },
        &["documented all-targets form"],
    );

    // A template with no cargo test invocation at all, and one whose only
    // invocation-shaped line is a comment.
    assert_drift(
        || check_ci_workflow_test_commands("set -ex\ncargo build --release\n"),
        &["must invoke `cargo test`"],
    );
    assert_drift(
        || check_ci_workflow_test_commands("# cargo test --tests --verbose\n"),
        &["must invoke `cargo test`"],
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
