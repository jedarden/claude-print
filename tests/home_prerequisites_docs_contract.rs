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

/// Read a repo file, resolving the root from the *runtime*
/// `CARGO_MANIFEST_DIR` (compile-time value as fallback). The compile-time
/// value alone bakes the building checkout's path into the test binary;
/// when the shared target cache reuses that binary from a different
/// extraction — exactly the clean-tree verification NEEDLE re-runs — the
/// read would hit a directory that no longer exists. See
/// `tests/platform_matrix_docs.rs::repo_file` for the full rationale.
fn repo_file(relative: &str) -> String {
    let root = PathBuf::from(
        std::env::var("CARGO_MANIFEST_DIR")
            .unwrap_or_else(|_| env!("CARGO_MANIFEST_DIR").to_string()),
    );
    fs::read_to_string(root.join(relative))
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
