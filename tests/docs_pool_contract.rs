//! Documentation-contract test for the published serve / `--pool-socket` API
//! (bead claudepr-e4e94485).
//!
//! README.md §"Warm PTY pool (ADR-005)" and AGENTS.md §"Pool operations"
//! publish the operator contract for the pool: example command lines, the
//! `serve` flag table, the default socket path, the pool-size bounds, the
//! `0600` socket-node permission, the acquire budget, and the fallback-vs-
//! protocol-failure split. Those claims live in prose, and prose drifts: a
//! renamed flag, a moved default, or a changed cap would leave every
//! published example quietly wrong while the binary keeps working. Nothing
//! pinned that surface to the parser that implements it —
//! `tests/help_version_e2e.rs` only checks that the string "serve" appears
//! in top-level `--help`, and `tests/docs_slug_consistency.rs` guards a
//! different doc against a different implementation.
//!
//! This test is the drift guard. Every check reads the docs and judges them
//! against the implementation, never the reverse:
//!
//!   * every `claude-print ...` example line published in the two sections
//!     parses through the real `Cli` parser — the serve example lands in the
//!     `serve` subcommand with exactly the flag values the line shows, the
//!     client example sets `--pool-socket` and a prompt;
//!   * the documented default socket path, pool-size default, pool-size cap,
//!     and acquire budget equal the binary's own values (`DEFAULT_SOCKET_PATH`,
//!     clap's `default_value`, `MAX_POOL_SIZE` + `validate_pool_size`,
//!     `DEFAULT_ACQUIRE_TIMEOUT_SECS`);
//!   * the `serve` usage line and flag table list exactly the long flags the
//!     subcommand actually accepts — no undocumented flag, and no documented
//!     flag the parser would reject — and the rendered `serve` help carries
//!     every one of them;
//!   * the documented "`0600` regardless of umask" permission is re-derived
//!     by binding a real socket through `bind_socket` and stat-ing the node;
//!   * the documented fallback/protocol-failure split matches
//!     `is_stateless_fallback`, and the hard-error marker the README quotes
//!     is exactly what `AcquireFailure`'s `Display` produces.
//!
//! Library-level and hermetic: no daemon, no worker, no `claude` binary —
//! the only filesystem effects are reads of the markdown and one bound
//! socket in a tempdir.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use clap::{CommandFactory, Parser};

use claude_print::cli::{Cli, Command};
use claude_print::pool::{
    bind_socket, is_stateless_fallback, validate_pool_size, AcquireFailure,
    DEFAULT_ACQUIRE_TIMEOUT_SECS, DEFAULT_SOCKET_PATH, MAX_POOL_SIZE,
};

/// The README's operator section for the pool.
const README_HEADING: &str = "Warm PTY pool (ADR-005)";

/// The AGENTS.md quick reference for the same surface.
const AGENTS_HEADING: &str = "Pool operations (ADR-005)";

/// The repo root, resolved from the *runtime* `CARGO_MANIFEST_DIR` with the
/// compile-time value as fallback — the same dance as
/// `tests/docs_slug_consistency.rs`: the compile-time value alone bakes the
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

fn readme() -> String {
    read_doc("README.md")
}

fn agents_md() -> String {
    read_doc("AGENTS.md")
}

/// The body of a `## <heading>` section: from just after the heading line to
/// the next `## ` heading (or EOF). Subsections (`###`) stay inside it.
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

fn readme_pool_section(readme: &str) -> &str {
    section(readme, README_HEADING)
}

/// Every `claude-print ...` command line published inside a ```bash fence in
/// the given markdown, as whitespace-split argv vectors ready for the
/// parser. Non-claude-print lines in those fences (`pkill`, env-prefixed
/// script calls) are not claude-print invocations and are skipped. One
/// shell-ism is honored: a pair of double quotes wrapping a whole token is
/// the quoting the published examples use for arguments with spaces or
/// punctuation, so it is stripped — the parser must see the quoted *value*.
fn example_argv(md: &str) -> Vec<Vec<String>> {
    let mut out = Vec::new();
    let mut in_bash = false;
    for line in md.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("```") {
            in_bash = trimmed == "```bash";
            continue;
        }
        if in_bash && trimmed.starts_with("claude-print") {
            out.push(
                trimmed
                    .split_whitespace()
                    .map(|token| {
                        token
                            .strip_prefix('"')
                            .and_then(|t| t.strip_suffix('"'))
                            .map(str::to_string)
                            .unwrap_or_else(|| token.to_string())
                    })
                    .collect(),
            );
        }
    }
    out
}

/// Parse one published example through the real CLI and return it, panicking
/// with the offending line if clap rejects what the docs publish.
fn parse_example(argv: &[String]) -> Cli {
    Cli::try_parse_from(argv.iter()).unwrap_or_else(|e| {
        panic!(
            "published example no longer parses:\n  {}\nclap error: {e}",
            argv.join(" ")
        )
    })
}

// ── the published examples parse ─────────────────────────────────────────────

#[test]
fn every_published_readme_example_parses_through_the_real_cli() {
    let examples = example_argv(readme_pool_section(&readme()));
    assert!(
        !examples.is_empty(),
        "README §{README_HEADING} must publish claude-print examples for this guard to pin"
    );
    for argv in &examples {
        parse_example(argv);
    }
}

#[test]
fn every_published_agents_quick_reference_example_parses_through_the_real_cli() {
    let examples = example_argv(section(&agents_md(), AGENTS_HEADING));
    assert!(
        !examples.is_empty(),
        "AGENTS.md §{AGENTS_HEADING} must publish claude-print examples for this guard to pin"
    );
    for argv in &examples {
        parse_example(argv);
    }
}

#[test]
fn the_published_serve_example_parses_as_serve_with_the_documented_values() {
    let examples = example_argv(readme_pool_section(&readme()));
    let argv = examples
        .iter()
        .find(|argv| argv.get(1).map(String::as_str) == Some("serve"))
        .expect("README must publish a `claude-print serve ...` example");
    let cli = parse_example(argv);
    match cli.command {
        Some(Command::Serve {
            pool_size,
            socket,
            verbose,
        }) => {
            assert_eq!(pool_size, 2, "the README serve example pins --pool-size 2");
            assert_eq!(
                socket.as_deref(),
                Some(DEFAULT_SOCKET_PATH),
                "the README serve example pins the default socket path"
            );
            assert_eq!(
                verbose,
                argv.iter().any(|t| t == "--verbose"),
                "the parsed --verbose must match what the published line shows"
            );
        }
        other => panic!("the serve example must parse as the Serve subcommand, got {other:?}"),
    }
}

#[test]
fn the_published_client_example_sets_pool_socket_and_prompt() {
    let examples = example_argv(readme_pool_section(&readme()));
    let argv = examples
        .iter()
        .find(|argv| argv.iter().any(|t| t == "--pool-socket"))
        .expect("README must publish a `--pool-socket` client example");
    let cli = parse_example(argv);
    assert_eq!(
        cli.pool_socket.as_deref(),
        Some(Path::new(DEFAULT_SOCKET_PATH)),
        "the README client example pins the default socket path"
    );
    assert_eq!(
        cli.prompt.as_deref(),
        Some("prompt"),
        "the README client example pins the literal prompt argument"
    );
    assert!(
        cli.command.is_none(),
        "the client example is a session invocation, not a subcommand"
    );
    // The flag must also be documented in the top-level options table and
    // actually exist on the top-level command.
    let readme = readme();
    assert!(
        readme.contains("| `--pool-socket <PATH>` |"),
        "README must document --pool-socket in the options table"
    );
    assert!(
        Cli::command()
            .get_arguments()
            .any(|arg| arg.get_long() == Some("pool-socket")),
        "--pool-socket must exist as a long flag on the top-level command"
    );
}

// ── documented values equal the binary's ─────────────────────────────────────

#[test]
fn documented_default_socket_path_equals_the_binary_constant() {
    let readme = readme();
    assert!(
        readme.contains(&format!("| `--socket <PATH>` | `{DEFAULT_SOCKET_PATH}` |")),
        "the serve flag table's --socket default must be DEFAULT_SOCKET_PATH"
    );
    // The rendered `serve --help` states the same default in its --socket
    // doc comment — pin that hardcoded prose to the constant too.
    assert!(
        rendered_serve_help().contains(DEFAULT_SOCKET_PATH),
        "serve --help must state DEFAULT_SOCKET_PATH as the --socket default"
    );
}

#[test]
fn documented_pool_size_default_and_cap_match_the_binary() {
    // The cap: both docs publish "1–<cap>" and the constant is the cap.
    let readme = readme();
    let agents = agents_md();
    let range = format!("1\u{2013}{MAX_POOL_SIZE}");
    assert!(
        readme_pool_section(&readme).contains(&range),
        "README must publish the pool-size range {range}"
    );
    assert!(
        section(&agents, AGENTS_HEADING).contains(&range),
        "AGENTS.md must publish the pool-size range {range}"
    );
    // The default: the serve table documents `1` and a bare `serve` parses
    // to exactly that.
    assert!(
        readme.contains("| `--pool-size <N>` | `1` |"),
        "the serve flag table must document the --pool-size default of 1"
    );
    let cli = Cli::try_parse_from(["claude-print", "serve"])
        .expect("a bare `serve` must parse (pinned in src/main.rs tests too)");
    match cli.command {
        Some(Command::Serve { pool_size, .. }) => assert_eq!(pool_size, 1),
        other => panic!("bare serve must parse as Serve, got {other:?}"),
    }
    // The bounds the docs describe are exactly what validate_pool_size
    // enforces: 0 and cap+1 are argv failures, 1 and the cap are fine.
    assert!(validate_pool_size(1).is_ok());
    assert!(validate_pool_size(MAX_POOL_SIZE).is_ok());
    let zero = validate_pool_size(0).unwrap_err();
    assert!(zero.contains("at least 1"), "0 must be rejected: {zero}");
    let over = validate_pool_size(MAX_POOL_SIZE + 1).unwrap_err();
    assert!(
        over.contains(&MAX_POOL_SIZE.to_string()) && over.contains("exceeds"),
        "cap+1 must be rejected naming the cap: {over}"
    );
}

#[test]
fn documented_acquire_budget_matches_the_binary() {
    assert_eq!(
        DEFAULT_ACQUIRE_TIMEOUT_SECS, 60,
        "the docs publish min(<this>, --timeout); moving it is a doc change too"
    );
    assert!(
        readme_pool_section(&readme())
            .contains(&format!("min({DEFAULT_ACQUIRE_TIMEOUT_SECS} s, --timeout)")),
        "README must publish the acquire budget as min(<cap> s, --timeout)"
    );
    assert!(
        section(&agents_md(), AGENTS_HEADING)
            .contains(&format!("min({DEFAULT_ACQUIRE_TIMEOUT_SECS}s, --timeout)")),
        "AGENTS.md must publish the acquire budget as min(<cap>s, --timeout)"
    );
}

// ── the serve flag surface is complete and real ──────────────────────────────

/// The long flags the `serve` subcommand actually accepts, straight from the
/// clap tree (the auto `help` flag excluded), each spelled with its `--`.
fn serve_long_flags() -> Vec<String> {
    let mut flags: Vec<String> = Cli::command()
        .find_subcommand("serve")
        .expect("serve subcommand in the clap tree")
        .get_arguments()
        .filter_map(|arg| arg.get_long().map(|long| format!("--{long}")))
        .filter(|long| long != "--help")
        .collect();
    flags.sort();
    flags
}

/// The long flags the README's serve section documents, from the two places
/// it publishes them: the usage line (`claude-print serve [--pool-size N]
/// ...`) and the flag-table rows whose first cell is a `--flag`.
fn documented_serve_flags(section: &str) -> Vec<String> {
    let mut flags: Vec<String> = Vec::new();
    let usage = section
        .lines()
        .find(|l| l.starts_with("claude-print serve ["))
        .expect("the serve section must publish a `claude-print serve [...]` usage line");
    for token in usage.split_whitespace() {
        let token = token.trim_start_matches('[').trim_end_matches(']');
        if let Some(flag) = token.strip_prefix("--") {
            assert!(
                !flag.is_empty() && !flag.contains('='),
                "usage line flag `{token}` is not a plain long flag"
            );
            flags.push(format!("--{flag}"));
        }
    }
    for line in section.lines() {
        if !line.starts_with("| `--") {
            continue;
        }
        let first_cell = &line["| `".len()..];
        let first_cell = first_cell.split('`').next().unwrap_or_default();
        if let Some(name) = first_cell.split_whitespace().next() {
            assert!(
                name.starts_with("--") && !name[2..].contains('<'),
                "flag-table first cell `{first_cell}` must be a bare flag name"
            );
            flags.push(name.to_string());
        }
    }
    flags.sort();
    flags.dedup();
    flags
}

#[test]
fn serve_flag_table_and_usage_line_list_exactly_the_real_flags() {
    let documented = documented_serve_flags(readme_pool_section(&readme()));
    let actual = serve_long_flags();
    assert_eq!(
        documented, actual,
        "the README serve flag surface must match the parser exactly: \
         documented {documented:?} vs actual {actual:?}"
    );
}

/// The help text `claude-print serve --help` renders (via the clap tree, so
/// this is the same text the binary prints without spawning it).
fn rendered_serve_help() -> String {
    let mut cmd = Cli::command();
    let serve = cmd
        .find_subcommand_mut("serve")
        .expect("serve subcommand in the clap tree");
    serve.render_help().to_string()
}

#[test]
fn rendered_serve_help_carries_the_documented_surface() {
    let help = rendered_serve_help();
    for flag in serve_long_flags() {
        assert!(
            help.contains(&flag),
            "`serve --help` must list {flag} — the README documents it:\n{help}"
        );
    }
    assert!(
        help.contains("Run the pool daemon"),
        "`serve --help` must carry the subcommand about line (pinned at the \
         binary level in tests/help_version_e2e.rs too):\n{help}"
    );
}

// ── the permission and failure-classification claims ────────────────────────

#[test]
fn documented_socket_permissions_match_the_bind() {
    let dir = tempfile::tempdir().expect("tempdir for the bound socket");
    let path = dir.path().join("pool.sock");
    let _listener = bind_socket(&path).expect("bind_socket on a fresh path");
    let mode = fs::metadata(&path)
        .unwrap_or_else(|e| panic!("stat the bound node: {e}"))
        .permissions()
        .mode();
    assert_eq!(
        mode & 0o777,
        0o600,
        "the socket node the daemon binds must be owner-only"
    );
    for (doc, name) in [
        (readme_pool_section(&readme()), "README.md"),
        (section(&agents_md(), AGENTS_HEADING), "AGENTS.md"),
    ] {
        assert!(
            doc.contains("0600"),
            "{name} must publish the 0600 socket-node permission"
        );
    }
}

#[test]
fn documented_fallback_and_protocol_failure_split_matches_the_classifier() {
    // Every refusal code the README's failure table routes to the stateless
    // fallback classifies as a fallback in the implementation.
    for code in [
        "pool_full",
        "shutting_down",
        "internal_error",
        "acquire_timeout",
    ] {
        let failure = AcquireFailure::PoolUnavailable {
            code: code.to_string(),
            error: format!("{code} refused the acquire"),
        };
        assert!(
            is_stateless_fallback(&failure),
            "{code} must classify as stateless fallback"
        );
    }
    // ...and the unreachable shapes (absent/stale socket) do too.
    assert!(is_stateless_fallback(&AcquireFailure::Unreachable {
        socket: PathBuf::from(DEFAULT_SOCKET_PATH),
        reason: "nothing listening".to_string(),
    }));
    // While anything the table routes to the hard-error row classifies as a
    // protocol failure — never a fallback.
    assert!(!is_stateless_fallback(&AcquireFailure::Protocol(
        "malformed response".to_string()
    )));

    // The hard-error marker the README quotes is byte-for-byte the prefix
    // AcquireFailure's Display produces. (AGENTS.md describes the hard-error
    // row without quoting the marker, so only the README is held to it.)
    assert_eq!(
        AcquireFailure::Protocol("detail".to_string()).to_string(),
        "pool protocol failure: detail",
        "the README's `error: pool protocol failure: <detail>` promise rides this Display"
    );
    assert!(
        readme_pool_section(&readme()).contains("pool protocol failure"),
        "README must publish the hard-error marker"
    );
}
