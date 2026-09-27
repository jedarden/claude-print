//! Documentation-contract test for the README §Prerequisites `HOME` bullet
//! (bead claudepr-082ed0ae).
//!
//! The bullet is normative prose: it states what `HOME` must be (an
//! existing, writable directory for the running user), that every entry
//! point except `--help` validates it at startup, that unset/empty/missing/
//! read-only values are setup errors exiting status 2 with quoted message
//! text, that `json`/`stream-json` carry the same message in `error_message`,
//! and that `XDG_CONFIG_HOME` is the one partial exception (relocates the
//! config file, does not remove the requirement). The *behavior* those
//! sentences describe is pinned — but by tests that never read the
//! paragraph, so an edit to the bullet's wording or quoted strings would
//! leave every one of them green while the README stopped describing the
//! tool. Unlike the README's other normative summaries (§"Why this exists"
//! pinned by `tests/billing_entrypoint_contract.rs`, the Configuration
//! summary by `tests/config_contract.rs`), the bullet had no guard.
//!
//! Every check here reads the bullet and judges it against the compiled
//! CLI, never the reverse. The unset-HOME message is pinned without a
//! parallel hand-typed constant: the backtick quote is extracted from the
//! bullet and compared byte-for-byte against the binary's *observed*
//! stderr / `error_message`, so README and implementation are mechanically
//! locked with the CLI as the single source of truth (the same message
//! `tests/home_unset.rs` asserts from the other side).
//!
//! What this guard deliberately does **not** pin, because a sibling owns
//! it: the not-writable quote's byte-level template match against a real
//! read-only HOME and the Troubleshooting ```text block
//! (`tests/home_provisioning_recipes.rs`, which extracts both from the
//! README itself), the error contract across config/poller/session
//! callers and the /root-fallback regression (`tests/home_unset.rs`), the
//! call-site discipline (`tests/home_env_guard.rs`), the XDG-over-HOME
//! config-path precedence table (`tests/config_contract.rs`), and the
//! HOME-rooting of Claude Code state and transcripts — the caveat's
//! "still live under `$HOME`" half (`tests/claude_config_dir_docs_contract.rs`).
//!
//! Compiled-binaries, child-env overrides only: every leg drives
//! `CARGO_BIN_EXE_claude-print` with `HOME` (and `XDG_CONFIG_HOME` where
//! relevant) overridden in the child, so this process's environment is
//! never touched and no lock is needed.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

/// Setup errors exit 2 — the bullet: "the process exits with status 2".
const SETUP_EXIT: i32 = 2;

/// The text-mode stderr prefix every setup error carries
/// (`src/main.rs` routes the startup HOME failure through `emit_error`).
const TEXT_ERROR_PREFIX: &str = "error: invalid config: ";

// ── README extraction ───────────────────────────────────────────────────────

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

/// Read a repo file from the checkout under test, resolving the root
/// through [`repo_root`]'s runtime-first candidate chain.
fn repo_file(relative: &str) -> String {
    fs::read_to_string(repo_root().join(relative))
        .unwrap_or_else(|e| panic!("read {relative} from the checkout under test: {e}"))
}

/// Slice a markdown document from a heading line up to the next heading of
/// the same or higher level. Scoping pins to the slice means a string that
/// merely survives elsewhere in the document cannot satisfy them.
fn section_under_heading<'a>(document: &'a str, heading: &str, next_heading: &str) -> &'a str {
    let start = document
        .find(heading)
        .unwrap_or_else(|| panic!("README no longer has a {heading} heading"));
    let end = document[start + heading.len()..]
        .find(next_heading)
        .unwrap_or_else(|| panic!("README section {heading} is not followed by another heading"));
    &document[start..start + heading.len() + end]
}

/// The single §Prerequisites bullet this contract guards: the one starting
/// `- **`HOME` must name`. Slicing to the next bullet keeps a phrase that
/// survives in another Prerequisites bullet (e.g. the Claude Code one)
/// from satisfying a pin vacuously.
fn home_bullet() -> String {
    let readme = repo_file("README.md");
    let prerequisites = section_under_heading(&readme, "## Prerequisites", "\n## ");
    let start = prerequisites
        .find("- **`HOME` must name")
        .unwrap_or_else(|| {
            panic!("§Prerequisites no longer carries a `- **`HOME` must name` bullet")
        });
    let end = prerequisites[start + 1..]
        .find("\n- ")
        .unwrap_or(prerequisites.len() - start - 1);
    prerequisites[start..start + 1 + end].to_string()
}

/// The backtick-quoted span beginning with `anchor` — used to pull the
/// bullet's quoted CLI strings out of the prose without pinning the prose
/// around them.
fn backtick_quote<'a>(text: &'a str, anchor: &str) -> &'a str {
    let start = text
        .find(&format!("`{anchor}"))
        .unwrap_or_else(|| panic!("text no longer quotes a span starting {anchor:?}"));
    let content_start = start + 1;
    let end = text[content_start..]
        .find('`')
        .unwrap_or_else(|| panic!("quoted span starting {anchor:?} is never closed"));
    &text[content_start..content_start + end]
}

/// Collapse all whitespace runs to single spaces so a rewrapped bullet
/// still matches its pinned claim.
fn normalized(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Assert the bullet states `claim` verbatim modulo line wrapping.
fn states(claim: &str) {
    let bullet = home_bullet();
    assert!(
        normalized(&bullet).contains(&normalized(claim)),
        "the §Prerequisites HOME bullet no longer states the pinned claim: {}",
        normalized(claim)
    );
}

/// The bullet's quoted unset/empty HOME message, extracted — never
/// hand-typed here — so the byte-level pins below compare README text
/// directly against what the CLI emits.
fn unset_quote() -> String {
    backtick_quote(&home_bullet(), "HOME environment variable not set or empty").to_string()
}

// ── CLI harness ─────────────────────────────────────────────────────────────

fn claude_print_binary() -> PathBuf {
    let binary = PathBuf::from(env!("CARGO_BIN_EXE_claude-print"));
    assert!(
        binary.is_file(),
        "claude-print binary missing at {}",
        binary.display()
    );
    binary
}

fn stdout_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// Run the CLI with `HOME` removed from the child environment — the
/// bullet's "unset" setup error, exactly `env -u HOME claude-print ...`.
/// `XDG_CONFIG_HOME` goes too unless the caller provides one, so the
/// invoking user's real home and config cannot leak into the result.
fn run_without_home(args: &[&str], xdg: Option<&Path>) -> Output {
    let mut command = Command::new(claude_print_binary());
    command.args(args).env_remove("HOME").stdin(Stdio::null());
    match xdg {
        Some(path) => {
            command.env("XDG_CONFIG_HOME", path);
        }
        None => {
            command.env_remove("XDG_CONFIG_HOME");
        }
    }
    command
        .output()
        .unwrap_or_else(|e| panic!("run claude-print {:?} without HOME: {e}", args))
}

/// Assert a text-mode setup failure: exit 2, empty stdout, and stderr
/// exactly the pinned line built from the bullet's own quote.
fn assert_pinned_setup_failure(output: &Output, case: &str) {
    let stdout = stdout_of(output);
    let stderr = stderr_of(output);
    assert_eq!(
        output.status.code(),
        Some(SETUP_EXIT),
        "{case}: stdout={stdout:?}, stderr={stderr:?}"
    );
    assert!(
        stdout.is_empty(),
        "{case}: a setup error must not print success output: {stdout:?}"
    );
    assert_eq!(
        stderr,
        format!("{TEXT_ERROR_PREFIX}{}\n", unset_quote()),
        "{case}: the CLI's stderr forked from the bullet's quoted message"
    );
}

// ── the pins ────────────────────────────────────────────────────────────────

/// The bullet's load-bearing claims must stay in the bullet. Behavioral
/// halves live in the tests below and in the sibling suites the module doc
/// names; this pin is what makes a prose edit that orphans them fail.
#[test]
fn prerequisites_bullet_states_the_home_contract_claims() {
    states(
        "`HOME` must name an existing, writable directory** for the user running \
            `claude-print` — a non-empty value alone is not enough",
    );
    states(
        "every entry point except `--help` (prompt runs, `--check`, `--version`, \
            `serve`) validates it at startup",
    );
    states(
        "the path must exist, be a directory, and pass a create-write-remove probe \
            of a temporary file inside it",
    );
    states(
        "A `HOME` that is unset, empty, missing, or read-only is a setup error, \
            never a `/root` fallback: the process exits with status 2 and an \
            actionable message",
    );
    states(
        "(`json` and `stream-json` output carries the same message in \
            `error_message`)",
    );
    states(
        "Setting `XDG_CONFIG_HOME` is the one partial exception: it relocates the \
            config file so that resolution no longer consults `HOME`, but it does \
            not remove the requirement",
    );
    states("Claude Code state and transcripts still live under `$HOME`");

    // The bullet hands operators to the recipes section; that anchor must
    // still resolve to a real heading.
    states(
        "See [HOME in containers and chroots](#home-in-containers-and-chroots) for \
            provisioning recipes",
    );
    let readme = repo_file("README.md");
    assert!(
        readme.contains("### HOME in containers and chroots"),
        "the bullet's provisioning-recipes anchor no longer resolves to a heading"
    );
}

/// The quoted unset/empty message is the CLI's text-mode stderr line,
/// byte-for-byte: extract the quote from the bullet, run the real binary
/// with HOME unset, and demand they agree. No third constant sits between
/// them — the observed stderr *is* the expected value's other half, so
/// drifting either the prose quote or `src/util.rs`'s message fails here
/// (`tests/home_unset.rs` pins the same line from the implementation side).
#[test]
fn unset_home_quote_is_byte_for_byte_the_cli_text_error_line() {
    let quote = unset_quote();
    // The prose promises "an actionable message", so the quote must carry a
    // remedy — but its exact wording is *not* frozen here: a coordinated
    // reword of `src/util.rs` updates this quote through the byte equality
    // below, never through a third hand-typed constant (the frozen-bytes
    // pin lives in `tests/home_unset.rs`).
    assert!(
        quote.contains("set HOME"),
        "the quoted unset-HOME message lost its remedy clause: {quote:?}"
    );
    assert_pinned_setup_failure(&run_without_home(&["--version"], None), "--version");
}

/// The bullet's parenthetical — "(`json` and `stream-json` output carries
/// the same message in `error_message`)" — is a structural claim, not just
/// prose: both structured modes must emit a JSON result object whose
/// `error_message` is exactly the text-mode message (the shared prefix
/// included) with `is_error` true, on stdout, with stderr silent.
#[test]
fn json_and_stream_json_error_message_carries_the_pinned_message() {
    let expected_message = format!("invalid config: {}", unset_quote());

    for format in ["json", "stream-json"] {
        let output = run_without_home(&["--output-format", format, "--version"], None);
        let stdout = stdout_of(&output);
        let stderr = stderr_of(&output);
        assert_eq!(
            output.status.code(),
            Some(SETUP_EXIT),
            "{format}: a missing HOME is a setup error in every output format: \
             stdout={stdout:?}, stderr={stderr:?}"
        );
        assert!(
            stderr.is_empty(),
            "{format}: the structured setup error belongs on stdout, got stderr {stderr:?}"
        );

        let event: serde_json::Value = serde_json::from_str(stdout.trim())
            .unwrap_or_else(|e| panic!("{format}: stdout is not one JSON object: {e}: {stdout:?}"));
        assert_eq!(
            event["is_error"], true,
            "{format}: the HOME setup error must set is_error: {event}"
        );
        assert_eq!(
            event["error_message"],
            serde_json::Value::String(expected_message.clone()),
            "{format}: error_message must carry the same message the bullet quotes \
             (text-mode prefix included): {event}"
        );
    }
}

/// The XDG caveat's two halves, replayed against the real CLI:
///
/// - *relocates the config file* — with a valid `HOME`, an unreadable
///   config at `$XDG_CONFIG_HOME/claude-print/config.toml` makes the CLI
///   fail naming that file, so config resolution demonstrably followed
///   XDG and never consulted the `$HOME/.config` location;
/// - *does not remove the requirement* — a complete, valid XDG config
///   alongside an unset `HOME` still dies as the pinned setup error with
///   the bullet's quoted message.
///
/// The third half — "Claude Code state and transcripts still live under
/// `$HOME`" — is owned by `tests/claude_config_dir_docs_contract.rs`.
#[test]
fn xdg_exception_caveat_relocates_config_yet_keeps_the_requirement() {
    let xdg = tempfile::tempdir().expect("create temporary XDG_CONFIG_HOME");
    let config_dir = xdg.path().join("claude-print");
    fs::create_dir(&config_dir).expect("create temporary config directory");

    // Half 1 — relocation: resolution reads the XDG file, not $HOME's.
    let home = tempfile::tempdir().expect("create a valid HOME the CLI may validate");
    fs::write(config_dir.join("config.toml"), "not [valid toml").expect("write invalid XDG config");

    let unused_backend = std::env::current_exe().expect("locate an existing backend path");
    let output = Command::new(claude_print_binary())
        .arg("--claude-binary")
        .arg(&unused_backend)
        .arg("test prompt")
        .env("HOME", home.path())
        .env("XDG_CONFIG_HOME", xdg.path())
        .stdin(Stdio::null())
        .output()
        .expect("run claude-print with a broken XDG config");
    let stdout = stdout_of(&output);
    let stderr = stderr_of(&output);
    assert_eq!(
        output.status.code(),
        Some(SETUP_EXIT),
        "an invalid config file is a setup error: stdout={stdout:?}, stderr={stderr:?}"
    );
    let xdg_config_path = config_dir.join("config.toml");
    assert!(
        stderr.starts_with(TEXT_ERROR_PREFIX)
            && stderr.contains(&xdg_config_path.display().to_string()),
        "config resolution must follow XDG_CONFIG_HOME and name the relocated file: {stderr:?}"
    );
    let home_config_path = home.path().join(".config/claude-print/config.toml");
    assert!(
        !stderr.contains(&home_config_path.display().to_string()),
        "resolution must not fall back to the HOME config location once XDG is set: {stderr:?}"
    );

    // Half 2 — the requirement stands: a *valid* XDG config does not buy a
    // session without HOME.
    fs::write(
        config_dir.join("config.toml"),
        "[defaults]\nmodel = \"claude-haiku-4-5\"\n",
    )
    .expect("write valid XDG config");
    assert_pinned_setup_failure(
        &run_without_home(&["--version"], Some(xdg.path())),
        "valid XDG config with HOME unset",
    );
}

/// "every entry point except `--help` (prompt runs, `--check`, `--version`,
/// `serve`) validates it at startup" — each named entry point, run with
/// HOME unset from the child env, must die as the pinned setup error; the
/// one documented exception must render help successfully. `serve` gets a
/// temp socket path so a drift that let it start would bind somewhere
/// disposable (and hang this test loudly) rather than touch a real socket.
#[test]
fn every_documented_entry_point_validates_home_except_help() {
    let unused_backend = std::env::current_exe().expect("locate an existing backend path");
    let backend_arg = unused_backend
        .to_str()
        .expect("backend path must be UTF-8 for argv");
    let socket = tempfile::tempdir().expect("create socket directory");
    let socket_arg = socket
        .path()
        .join("pool.sock")
        .to_str()
        .expect("socket path must be UTF-8 for argv")
        .to_string();

    let entry_points: [(&str, Vec<&str>); 4] = [
        ("prompt run", vec!["--claude-binary", backend_arg, "prompt"]),
        ("--check", vec!["--check"]),
        ("--version", vec!["--version"]),
        ("serve", vec!["serve", "--socket", &socket_arg]),
    ];
    for (case, args) in entry_points {
        assert_pinned_setup_failure(&run_without_home(&args, None), case);
    }

    let help = run_without_home(&["--help"], None);
    let stdout = stdout_of(&help);
    assert_eq!(
        help.status.code(),
        Some(0),
        "--help is the one entry point that must render without HOME: stderr={:?}",
        stderr_of(&help)
    );
    assert!(
        stdout.contains("Usage:"),
        "--help must actually render help, got {stdout:?}"
    );
    assert!(stderr_of(&help).is_empty(), "unexpected --help stderr");
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
