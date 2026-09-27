//! Consolidated `claude -p` compatibility contract (bead claudepr-10f4ffa5).
//!
//! The README calls claude-print a drop-in, wire-compatible replacement for
//! `claude -p`; `docs/notes/claude-p-compat-contract.md` is the single
//! normative definition of what that claim means across its six observable
//! axes. This suite holds that document against the implementation in both
//! directions so neither can move alone:
//!
//!   * **flag surface** — §1's table (and the serve table) is the closed world
//!     of the real `Cli` parser: every parser flag documented, every documented
//!     flag real, shorts and defaults equal on both sides;
//!   * **child argv** — §6's construction replayed through the real
//!     `Session::build_child_argv`, then observed end-to-end through
//!     `mock-claude`'s `MOCK_RECORD_ARGS` dump: a baseline run's child argv is
//!     byte-exactly the documented default shape, and a full-featured run
//!     carries every `verbatim`/`always`/`becomes` form from §1 while every
//!     `never`-class flag (plus `--print`/`-p`/`--output-format`) stays out;
//!   * **child environment** — §7's forced/scrubbed lists observed through
//!     `MOCK_RECORD_ENV`: an inherited decoy entrypoint and all five scrubbed
//!     markers go in, exactly one forced `cli` entrypoint and the persistence
//!     force come out, a control variable survives verbatim;
//!   * **prompt input** — §2's precedence observed with `MOCK_ECHO_PROMPT`:
//!     `--input-file` beats the positional beats stdin;
//!   * **exit codes** — §4's table equal to `ClaudePrintError`'s
//!     `exit_code()`/`subtype()` mapping, plus the pre-emitter shapes (exit 4
//!     empty prompt, exit 2 NUL byte, oversize stdin, missing binary, usage
//!     error) driven against the compiled binary;
//!   * **signals** — §5's session rows presence-pinned against the handler and
//!     teardown wiring in `src/session.rs`, the relay rows citing the two
//!     forwarding suites that pin them behaviorally.
//!
//! Hermetic: compiled `claude-print` + mock-claude; every run gets a throwaway
//! `HOME` and a unique mock session id, so transcripts and trust state land in
//! a temp dir that dies with the test. Child-env overrides only — this process
//! never mutates its own environment.

use std::ffi::{CString, OsString};
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use clap::CommandFactory;
use tempfile::TempDir;

use claude_print::cli::Cli;
use claude_print::config::Config;
use claude_print::error::ClaudePrintError;
use claude_print::hook::HookInstaller;
use claude_print::session::{LaunchOptions, Session};

// ── repo / doc plumbing ──────────────────────────────────────────────────────

/// Repo root, resolved the same way as `tests/docs_test_classification.rs`:
/// an explicit override first (harnesses that copy the tree), then the runtime
/// manifest dir cargo sets for this test build.
fn repo_root() -> PathBuf {
    if let Ok(dir) = std::env::var("CLAUDE_PRINT_TEST_REPO") {
        let dir = PathBuf::from(dir);
        if dir.join("Cargo.toml").exists() {
            return dir;
        }
    }
    PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"))
}

fn contract_doc() -> String {
    let path = repo_root().join("docs/notes/claude-p-compat-contract.md");
    fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("contract doc missing at {}: {e}", path.display()))
}

fn src_source(rel: &str) -> String {
    let path = repo_root().join(rel);
    fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("source missing at {}: {e}", path.display()))
}

/// The doc text between two headings (exclusive), or through end-of-file when
/// the end heading is absent.
fn doc_section<'a>(doc: &'a str, start: &str, end: &str) -> &'a str {
    let (_, rest) = doc
        .split_once(start)
        .unwrap_or_else(|| panic!("contract doc missing the {start:?} section"));
    match rest.split_once(end) {
        Some((body, _)) => body,
        None => rest,
    }
}

/// Markdown table rows as trimmed cell vectors; separator and non-table lines
/// dropped. Doc cells never contain literal pipes, so a naive split is exact.
fn table_rows(section: &str) -> Vec<Vec<String>> {
    section
        .lines()
        .filter(|l| l.trim_start().starts_with('|'))
        .filter(|l| !l.contains("|---"))
        .map(|l| {
            l.split('|')
                .skip(1) // leading pipe
                .map(|c| c.trim().to_string())
                .collect::<Vec<_>>()
        })
        .filter(|cells| !cells.is_empty())
        .collect()
}

/// A §1 flag-table row: `| \`--input-file <FILE>\` | \`-f\` | — | never |`.
#[derive(Debug, Clone)]
struct DocFlag {
    /// Long name without dashes, e.g. `input-file`.
    name: String,
    short: Option<char>,
    /// Default cell, backticks stripped.
    default: String,
    /// Child-argv cell, verbatim (the closed-vocabulary value + parenthetical).
    child_argv: String,
}

fn doc_flags(doc: &str) -> Vec<DocFlag> {
    let section = doc_section(doc, "## 1. Supported flags", "## 2. Prompt input");
    table_rows(section)
        .into_iter()
        .filter_map(|cells| {
            let first = cells.first()?.clone();
            if !first.contains("--") {
                return None; // the `[PROMPT]` positional row, headers, prose rows
            }
            if first.contains("`serve ") {
                return None; // the serve table's rows are parsed by doc_serve_flags
            }
            let flag = first
                .trim_matches('`')
                .split_whitespace()
                .find(|t| t.starts_with("--"))
                .unwrap_or_else(|| panic!("flag row without a -- token: {first:?}"))
                .to_string();
            let short = cells
                .get(1)
                .and_then(|c| c.trim_matches('`').strip_prefix('-'))
                .and_then(|s| s.chars().next());
            let default = cells
                .get(2)
                .map(|c| c.trim_matches('`').to_string())
                .unwrap_or_default();
            let child_argv = cells.get(3).cloned().unwrap_or_default();
            Some(DocFlag {
                name: flag.trim_start_matches('-').to_string(),
                short,
                default,
                child_argv,
            })
        })
        .collect()
}

/// The `--flag` spellings documented in the serve table.
fn doc_serve_flags(doc: &str) -> Vec<String> {
    let section = doc_section(doc, "## 1. Supported flags", "## 2. Prompt input");
    table_rows(section)
        .into_iter()
        .filter_map(|cells| {
            let first = cells.first()?.clone();
            if !first.contains("`serve --") {
                return None;
            }
            Some(
                first
                    .trim_matches('`')
                    .split_whitespace()
                    .find(|t| t.starts_with("--"))
                    .expect("serve row without a -- token")
                    .to_string(),
            )
        })
        .collect()
}

// ── binary-e2e plumbing (the `tests/binary_e2e.rs` harness pattern) ─────────

/// Locate a workspace bin built alongside this test binary (`current_exe()` →
/// `target/<profile>/deps/` → `target/<profile>/`), so the suite never names a
/// written-out build path.
fn workspace_bin(name: &str) -> PathBuf {
    let exe = std::env::current_exe().expect("current_exe");
    exe.parent()
        .and_then(|p| p.parent())
        .expect("test binary must live under target/<profile>/deps/")
        .join(name)
}

/// Per-session wall-clock budget: mock-claude answers within a couple of
/// seconds; 30 s is a generous ceiling that still fails fast on a wedge.
const BUDGET: Duration = Duration::from_secs(30);

/// A throwaway HOME (transcripts, trust state, config probing all land here)
/// plus an empty explicit `--config`, so runs are hermetic against both the
/// developer's real config and the real `$HOME/.claude` tree. The caller must
/// keep the returned TempDir alive for the run.
fn hermetic_home() -> (TempDir, PathBuf) {
    let dir = TempDir::new().expect("temp home");
    let home = dir.path().join("home");
    fs::create_dir_all(&home).expect("create temp home");
    let config = dir.path().join("config.toml");
    fs::write(&config, "").expect("write empty config");
    (dir, config)
}

/// Wire the hermetic environment plus this run's mock seams into the command.
fn wire(cmd: &mut Command, home: &Path, record_args: Option<&Path>, record_env: Option<&Path>) {
    cmd.env("HOME", home);
    cmd.env("MOCK_UNIQUE_SESSION_ID", "1");
    if let Some(p) = record_args {
        cmd.env("MOCK_RECORD_ARGS", p);
    }
    if let Some(p) = record_env {
        cmd.env("MOCK_RECORD_ENV", p);
    }
}

struct Outcome {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

/// Run to completion with an explicit stdin; kill (and fail) past `budget`.
fn run(cmd: &mut Command, budget: Duration, stdin: Stdio) -> Outcome {
    cmd.stdin(stdin)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let start = Instant::now();
    let mut child = cmd
        .spawn()
        .unwrap_or_else(|e| panic!("failed to spawn claude-print: {e}"));
    let deadline = start + budget;
    let code = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status.code(),
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    panic!("claude-print did not exit within {budget:?}");
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(_) => break None,
        }
    };
    let output = child.wait_with_output().expect("wait_with_output");
    Outcome {
        code: code.or(output.status.code()),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    }
}

/// Read mock-claude's `MOCK_RECORD_ARGS` dump (NUL-separated argv).
fn read_recorded_argv(path: &Path) -> Vec<String> {
    let bytes = fs::read(path).unwrap_or_else(|e| {
        panic!(
            "MOCK_RECORD_ARGS file was not written at {}: {e}",
            path.display()
        )
    });
    bytes
        .split(|b| *b == 0)
        .filter(|e| !e.is_empty())
        .map(|e| String::from_utf8_lossy(e).into_owned())
        .collect()
}

/// Read mock-claude's `MOCK_RECORD_ENV` dump (NUL-separated `KEY=VALUE`,
/// mirroring /proc/self/environ — exactly the environment claude-print exec'd
/// the child with).
fn read_recorded_env(path: &Path) -> Vec<(String, String)> {
    let bytes = fs::read(path).unwrap_or_else(|e| {
        panic!(
            "MOCK_RECORD_ENV file was not written at {}: {e}",
            path.display()
        )
    });
    bytes
        .split(|b| *b == 0)
        .filter(|e| !e.is_empty())
        .map(|e| {
            let entry = String::from_utf8_lossy(e).into_owned();
            match entry.split_once('=') {
                Some((k, v)) => (k.to_string(), v.to_string()),
                None => (entry, String::new()),
            }
        })
        .collect()
}

// ── §1: the flag surface is the closed world of the parser ──────────────────

/// The fully-built clap tree: `build()` is what injects the auto `help` arg,
/// so the closed world below sees the same surface `--help` advertises.
fn real_parser() -> clap::Command {
    let mut cmd = Cli::command();
    cmd.build();
    cmd
}

#[test]
fn doc_flag_table_matches_the_parser_closed_world() {
    let doc = contract_doc();
    let mut documented: Vec<String> = doc_flags(&doc).into_iter().map(|f| f.name).collect();
    let documented_len = documented.len();
    documented.sort();
    documented.dedup();
    assert_eq!(
        documented.len(),
        documented_len,
        "§1 must list every flag exactly once"
    );

    let mut real: Vec<String> = real_parser()
        .get_arguments()
        .filter_map(|arg| arg.get_long().map(String::from))
        .collect();
    real.sort();

    assert_eq!(
        documented, real,
        "§1 must be the complete accepted option surface: every parser flag \
         documented exactly once, every documented flag real"
    );

    // The one positional (the prompt) is documented as the `[PROMPT]` row and
    // is the parser's only positional.
    let positional_rows = table_rows(doc_section(
        &doc,
        "## 1. Supported flags",
        "## 2. Prompt input",
    ))
    .into_iter()
    .filter(|cells| {
        cells
            .first()
            .map(|c| c.contains("[PROMPT]"))
            .unwrap_or(false)
    })
    .count();
    assert_eq!(
        positional_rows, 1,
        "§1 must document the positional exactly once"
    );
    let positional_count = real_parser()
        .get_arguments()
        .filter(|arg| arg.get_long().is_none() && arg.get_short().is_none())
        .count();
    assert_eq!(
        positional_count, 1,
        "the parser must have exactly one positional"
    );
}

#[test]
fn doc_flag_shorts_match_the_parser() {
    let doc = contract_doc();
    let cmd = real_parser();
    for flag in doc_flags(&doc) {
        let arg = cmd
            .get_arguments()
            .find(|a| a.get_long() == Some(flag.name.as_str()))
            .unwrap_or_else(|| panic!("--{} documented but not in the parser", flag.name));
        assert_eq!(
            arg.get_short(),
            flag.short,
            "--{} short spelling must match §1",
            flag.name
        );
    }
}

#[test]
fn doc_defaults_match_clap_and_the_resolvers() {
    let doc = contract_doc();
    let cmd = real_parser();
    let clap_default = |name: &str| -> Vec<String> {
        cmd.get_arguments()
            .find(|a| a.get_long() == Some(name))
            .map(|a| {
                a.get_default_values()
                    .iter()
                    .map(|v| v.to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default()
    };

    // The resolvers' compiled-in defaults, observed through a missing config
    // (NotFound → defaults) — the documented `--model`/`--max-turns` defaults
    // are applied at resolve time, not left to the child's fallback.
    let config = Config::load_or_default(&PathBuf::from("/compat-no-such-dir/config.toml"))
        .expect("missing config resolves to defaults");

    for flag in doc_flags(&doc) {
        let want = flag.default.as_str();
        match flag.name.as_str() {
            "model" => {
                assert_eq!(want, "claude-sonnet-4-6", "§1 --model default drifted");
                assert!(
                    clap_default("model").is_empty(),
                    "--model must carry no clap default_value — the compiled-in \
                     default is applied by the resolver"
                );
                assert_eq!(
                    config.resolve_model(None),
                    want,
                    "the resolver's compiled-in default must equal §1's --model default"
                );
            }
            "max-turns" => {
                assert_eq!(want, "30", "§1 --max-turns default drifted");
                assert_eq!(
                    clap_default("max-turns"),
                    vec!["30"],
                    "--max-turns clap default_value must equal §1"
                );
                assert_eq!(
                    config.resolve_max_turns(None),
                    30,
                    "the resolver's compiled-in default must equal §1's --max-turns default"
                );
            }
            "timeout" => assert_eq!(
                clap_default("timeout"),
                vec![want],
                "--timeout default must match §1"
            ),
            "output-format" => {
                assert_eq!(
                    clap_default("output-format"),
                    vec![want],
                    "--output-format default must match §1"
                )
            }
            "first-output-timeout" | "stream-json-timeout" | "stop-hook-timeout" => {
                assert_eq!(
                    clap_default(&flag.name),
                    vec![want],
                    "--{} default must match §1",
                    flag.name
                );
            }
            // Everything else documents "—" or "off". A value-taking flag must
            // carry no clap default at all (an implicit default would break the
            // config tiering); a bool flag's "off"/absent default is false, so
            // clap's implicit SetTrue "false" agrees with the doc.
            _ => {
                let arg = cmd
                    .get_arguments()
                    .find(|a| a.get_long() == Some(flag.name.as_str()))
                    .expect("flag exists (closed world already proven)");
                let takes_value = matches!(
                    arg.get_action(),
                    clap::ArgAction::Set | clap::ArgAction::Append
                );
                if takes_value {
                    assert!(
                        clap_default(&flag.name).is_empty(),
                        "--{} documents no default, so it must carry no clap default_value",
                        flag.name
                    );
                } else {
                    assert!(
                        clap_default(&flag.name).iter().all(|v| v == "false"),
                        "--{} documents off/no default; clap must not default it true ({:?})",
                        flag.name,
                        clap_default(&flag.name)
                    );
                }
            }
        }
    }
}

#[test]
fn serve_table_matches_the_serve_subcommand() {
    let mut documented = doc_serve_flags(&contract_doc());
    documented.sort();

    let mut real: Vec<String> = Cli::command()
        .find_subcommand("serve")
        .expect("serve subcommand in the clap tree")
        .get_arguments()
        .filter_map(|arg| arg.get_long().map(|long| format!("--{long}")))
        .filter(|long| long != "--help")
        .collect();
    real.sort();

    assert_eq!(
        documented, real,
        "the serve table must list exactly the flags the serve subcommand accepts"
    );
}

// ── §6: build_child_argv replays the documented construction ────────────────

/// §1's Child-argv column is a closed vocabulary; anything else in the cell
/// means the doc grew a forwarding class the tests do not enforce.
#[test]
fn doc_child_argv_vocabulary_is_closed() {
    for flag in doc_flags(&contract_doc()) {
        assert!(
            flag.child_argv.starts_with("never")
                || flag.child_argv.starts_with("verbatim")
                || flag.child_argv.starts_with("always:")
                || flag.child_argv.starts_with("becomes:")
                || flag.child_argv.starts_with("entry point"),
            "--{}: Child-argv cell {:?} is outside the closed vocabulary \
             (never / verbatim / always: / becomes: / entry point)",
            flag.name,
            flag.child_argv
        );
    }
}

fn built_child_argv(launch: &LaunchOptions, claude_args: &[&str]) -> Vec<String> {
    let installer = HookInstaller::new().expect("hook installer");
    let args: Vec<OsString> = claude_args.iter().map(OsString::from).collect();
    Session::build_child_argv(Path::new("/usr/bin/claude"), &installer, launch, &args)
        .expect("build_child_argv")
        .into_iter()
        .map(|a: CString| a.to_string_lossy().into_owned())
        .collect()
}

/// §6 in default mode: the derived prefix is exactly the relay settings flag —
/// no isolation flag, no MCP block. (The `--model`/`--max-turns` pairs of §6
/// items 4–5 arrive through `claude_args`, which main() always fills; the
/// observed legs below pin that assembly end to end.)
#[test]
fn build_child_argv_default_shape_matches_the_doc() {
    let argv = built_child_argv(&LaunchOptions::default(), &[]);
    assert_eq!(
        argv.len(),
        1,
        "the default-mode derived prefix is exactly the relay settings flag: {argv:?}"
    );
    assert!(
        argv[0].starts_with("--settings=") && argv[0].contains("settings.json"),
        "argv[0] must be the relay --settings= flag: {:?}",
        argv[0]
    );
}

/// §6 items 2–3: isolation mode adds `--setting-sources=`; named MCP configs
/// add `--strict-mcp-config` plus one `--mcp-config <entry>` per entry, in
/// order, between the settings flag and the model pair.
#[test]
fn build_child_argv_isolation_and_mcp_derivations_match_the_doc() {
    let isolation = LaunchOptions {
        no_inherit_hooks: true,
        ..LaunchOptions::default()
    };
    let argv = built_child_argv(&isolation, &[]);
    assert_eq!(
        argv,
        [argv[0].clone(), "--setting-sources=".to_string(),],
        "isolation mode forwards exactly --setting-sources= after --settings: {argv:?}"
    );

    let mcp = LaunchOptions {
        mcp_configs: vec!["/tmp/a.json".into(), "/tmp/b.json".into()],
        ..LaunchOptions::default()
    };
    let argv = built_child_argv(&mcp, &[]);
    assert_eq!(
        argv[1..],
        [
            "--strict-mcp-config",
            "--mcp-config",
            "/tmp/a.json",
            "--mcp-config",
            "/tmp/b.json",
        ][..],
        "the MCP block is --strict-mcp-config plus one pair per entry, in order: {argv:?}"
    );
    assert!(
        !built_child_argv(&LaunchOptions::default(), &[])
            .contains(&"--strict-mcp-config".to_string()),
        "default mode must not forward the MCP block"
    );
}

/// §6 items 4–8: caller-supplied claude_args land verbatim after the derived
/// prefix — the model/max-turns/tools/skip-permissions forwarding that
/// `main()` assembles reaches the child unreshaped.
#[test]
fn build_child_argv_forwards_claude_args_verbatim() {
    let claude_args = [
        "--model",
        "compat-model-x",
        "--max-turns",
        "7",
        "--dangerously-skip-permissions",
        "--allowedTools",
        "Bash",
        "--disallowedTools",
        "WebFetch",
    ];
    let argv = built_child_argv(&LaunchOptions::default(), &claude_args);
    assert_eq!(
        argv[1..],
        claude_args[..],
        "claude_args must land verbatim, in order, after --settings=: {argv:?}"
    );
}

// ── §6 observed end-to-end through MOCK_RECORD_ARGS ─────────────────────────

/// The baseline run's child argv is byte-exactly the documented default shape:
/// relay settings flag, the always-derived model/max-turns pairs with the
/// compiled-in defaults — and nothing else. The prompt, `--config`, and
/// `--claude-binary` are in the invocation but must not be in the child argv.
#[test]
fn observed_child_argv_baseline_is_exactly_the_documented_shape() {
    let mock = workspace_bin("mock-claude");
    let (_home, config) = hermetic_home();
    let record = _home.path().join("recorded-argv");
    let mut cmd = Command::new(workspace_bin("claude-print"));
    cmd.arg("--claude-binary").arg(&mock);
    cmd.arg("--config").arg(&config);
    cmd.arg("hello from the compatibility contract");
    wire(&mut cmd, _home.path(), Some(&record), None);

    let out = run(&mut cmd, BUDGET, Stdio::null());
    assert_eq!(
        out.code,
        Some(0),
        "baseline run must succeed; stderr: {}",
        out.stderr
    );

    let argv = read_recorded_argv(&record);
    assert_eq!(
        argv.len(),
        6,
        "the baseline child argv must be exactly the documented default shape: {argv:?}"
    );
    // argv[0] is the child binary itself; its exact spelling is the resolver's
    // business (canonicalization included), so pin the tail byte-exactly.
    assert!(
        argv[0].ends_with("mock-claude"),
        "argv[0] must be the claude binary itself: {argv:?}"
    );
    assert!(
        argv[1].starts_with("--settings="),
        "argv[1] must be the relay --settings= flag: {argv:?}"
    );
    assert_eq!(
        &argv[2..],
        &["--model", "claude-sonnet-4-6", "--max-turns", "30"][..],
        "the baseline tail must be the always-derived pairs with the compiled-in defaults"
    );
    // The prompt travels via the PTY, never the argv (§2).
    assert!(
        !argv
            .iter()
            .any(|a| a.contains("hello from the compatibility contract")),
        "the prompt must never appear on the child argv: {argv:?}"
    );
    // Every `never`-class flag from §1 stays out even when not passed, and the
    // forbidden print-mode flags (§8) stay out unconditionally.
    for flag in [
        "--print",
        "-p",
        "--output-format",
        "-o",
        "--timeout",
        "--input-file",
        "-f",
        "--verbose",
        "--check",
        "--config",
        "--pool-socket",
        "--claude-binary",
        "--pretrust-cwd",
        "--show-child-stderr",
        "--first-output-timeout",
        "--stream-json-timeout",
        "--stop-hook-timeout",
        "--no-inherit-hooks",
        "--strict-mcp-config",
        "--setting-sources=",
    ] {
        assert!(
            !argv.contains(&flag.to_string()),
            "{flag} must never reach the child argv: {argv:?}"
        );
    }
}

/// A full-featured run — one flag from every forwarding class in §1 — must
/// produce exactly the documented §6 order: settings, isolation, MCP block,
/// then the caller args (model, max-turns, skip-permissions, tools) verbatim,
/// with every claude-print-local flag staying home.
#[test]
fn observed_child_argv_full_feature_run_matches_the_doc_classification() {
    let mock = workspace_bin("mock-claude");
    let (_home, config) = hermetic_home();
    let mcp = _home.path().join("mcp.json");
    fs::write(&mcp, r#"{"mcpServers":{}}"#).expect("write mcp config");
    let record = _home.path().join("recorded-argv");

    let mut cmd = Command::new(workspace_bin("claude-print"));
    cmd.arg("--claude-binary").arg(&mock);
    cmd.arg("--config").arg(&config);
    cmd.arg("--model").arg("compat-model-x");
    cmd.arg("--max-turns").arg("7");
    cmd.arg("--allowedTools").arg("Bash");
    cmd.arg("--disallowedTools").arg("WebFetch");
    cmd.arg("--dangerously-skip-permissions");
    cmd.arg("--no-inherit-hooks");
    cmd.arg("--mcp-config").arg(&mcp);
    // claude-print-local flags: accepted here, never forwarded.
    cmd.arg("--timeout").arg("3600");
    cmd.arg("--first-output-timeout").arg("90");
    cmd.arg("--stream-json-timeout").arg("90");
    cmd.arg("--stop-hook-timeout").arg("120");
    cmd.arg("--output-format").arg("json");
    cmd.arg("--verbose");
    cmd.arg("--show-child-stderr");
    cmd.arg("the full-feature prompt");
    wire(&mut cmd, _home.path(), Some(&record), None);

    let out = run(&mut cmd, BUDGET, Stdio::null());
    assert_eq!(
        out.code,
        Some(0),
        "full-feature run must succeed; stderr: {}",
        out.stderr
    );

    let argv = read_recorded_argv(&record);
    let mcp_str = mcp.to_string_lossy().into_owned();
    let expected_tail = vec![
        "--setting-sources=".to_string(),
        "--strict-mcp-config".to_string(),
        "--mcp-config".to_string(),
        mcp_str.clone(),
        "--model".to_string(),
        "compat-model-x".to_string(),
        "--max-turns".to_string(),
        "7".to_string(),
        "--dangerously-skip-permissions".to_string(),
        "--allowedTools".to_string(),
        "Bash".to_string(),
        "--disallowedTools".to_string(),
        "WebFetch".to_string(),
    ];
    assert_eq!(
        argv.len(),
        expected_tail.len() + 2,
        "no flag may vanish from or sneak into the child argv: {argv:?}"
    );
    assert!(
        argv[1].starts_with("--settings="),
        "argv[1] must be the relay --settings= flag: {argv:?}"
    );
    assert_eq!(
        &argv[2..],
        &expected_tail[..],
        "the full-feature child argv must match §6's documented order exactly"
    );
}

// ── §7: the child environment ────────────────────────────────────────────────

/// The §7 tables are exactly the two forced pairs and the five scrubbed
/// markers — the doc may not grow a variable the implementation does not
/// force/scrub (the observed leg below proves the listed ones behave).
#[test]
fn doc_env_tables_exactly_the_pinned_lists() {
    let doc = contract_doc();
    let section = doc_section(&doc, "## 7. Child environment", "## 8. Divergences");
    let (forced_part, scrubbed_part) = section
        .split_once("**Scrubbed**")
        .expect("§7 must carry the **Scrubbed** table");

    let forced: Vec<(String, String)> = table_rows(forced_part)
        .into_iter()
        .filter(|cells| {
            cells
                .first()
                .map(|c| c.starts_with('`') && c.contains("CLAUDE"))
                .unwrap_or(false)
        })
        .map(|cells| {
            (
                cells[0].trim_matches('`').to_string(),
                cells[1].trim_matches('`').to_string(),
            )
        })
        .collect();
    assert_eq!(
        forced,
        vec![
            ("CLAUDE_CODE_ENTRYPOINT".to_string(), "cli".to_string()),
            (
                "CLAUDE_CODE_FORCE_SESSION_PERSISTENCE".to_string(),
                "1".to_string()
            ),
        ],
        "§7's forced table must be exactly the two documented pairs"
    );

    let scrubbed: Vec<String> = table_rows(scrubbed_part)
        .into_iter()
        .filter(|cells| cells.first().map(|c| c.contains("CLAUDE")).unwrap_or(false))
        .map(|cells| cells[0].trim_matches('`').to_string())
        .collect();
    assert_eq!(
        scrubbed,
        vec![
            "CLAUDE_CODE_SESSION_ID".to_string(),
            "CLAUDECODE".to_string(),
            "CLAUDE_CODE_CHILD_SESSION".to_string(),
            "CLAUDE_CODE_SKIP_PROMPT_HISTORY".to_string(),
            "CLAUDE_CONFIG_DIR".to_string(),
        ],
        "§7's scrubbed table must be exactly the five documented markers"
    );
}

/// Observed through a live run that inherits every scrubbed marker plus a
/// decoy entrypoint: the child's recorded environment carries exactly one
/// forced `cli` entrypoint (the decoy `sdk-cli` overridden), the persistence
/// force, none of the five scrubbed names, and a control variable verbatim.
#[test]
fn observed_child_env_forces_and_scrubs_match_the_doc() {
    let mock = workspace_bin("mock-claude");
    let (_home, config) = hermetic_home();
    let record = _home.path().join("recorded-env");
    let mut cmd = Command::new(workspace_bin("claude-print"));
    cmd.arg("--claude-binary").arg(&mock);
    cmd.arg("--config").arg(&config);
    cmd.arg("env probe");
    // Everything the §7 scrub list drops, plus the decoy the force list
    // overrides, plus one control variable that must survive untouched.
    cmd.env("CLAUDE_CODE_ENTRYPOINT", "sdk-cli");
    cmd.env("CLAUDE_CODE_SESSION_ID", "decoy-session-id");
    cmd.env("CLAUDECODE", "1");
    cmd.env("CLAUDE_CODE_CHILD_SESSION", "1");
    cmd.env("CLAUDE_CODE_SKIP_PROMPT_HISTORY", "1");
    cmd.env("CLAUDE_CONFIG_DIR", "/decoy/claude-config");
    cmd.env("COMPAT_PROBE_KEEPME", "compat-keeps-me");
    wire(&mut cmd, _home.path(), None, Some(&record));

    let out = run(&mut cmd, BUDGET, Stdio::null());
    assert_eq!(
        out.code,
        Some(0),
        "env probe run must succeed; stderr: {}",
        out.stderr
    );

    let env = read_recorded_env(&record);
    let value_of = |key: &str| -> Vec<&str> {
        env.iter()
            .filter(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
            .collect()
    };

    assert_eq!(
        value_of("CLAUDE_CODE_ENTRYPOINT"),
        vec!["cli"],
        "exactly one entrypoint, forced to cli over the inherited sdk-cli"
    );
    assert_eq!(
        value_of("CLAUDE_CODE_FORCE_SESSION_PERSISTENCE"),
        vec!["1"],
        "the persistence force must reach the child"
    );
    for key in [
        "CLAUDE_CODE_SESSION_ID",
        "CLAUDECODE",
        "CLAUDE_CODE_CHILD_SESSION",
        "CLAUDE_CODE_SKIP_PROMPT_HISTORY",
        "CLAUDE_CONFIG_DIR",
    ] {
        assert!(
            value_of(key).is_empty(),
            "{key} is on the §7 scrub list and must not reach the child"
        );
    }
    assert_eq!(
        value_of("COMPAT_PROBE_KEEPME"),
        vec!["compat-keeps-me"],
        "the environment passes through verbatim apart from the two §7 deltas"
    );
}

// ── §2: prompt-source precedence and validation shapes ──────────────────────

/// §2 precedence observed with the prompt echoed back as the response
/// (`MOCK_ECHO_PROMPT`): `--input-file` beats the positional beats stdin, and
/// each source alone is the one used. Text mode emits the response plus one LF.
#[test]
fn prompt_source_precedence_file_beats_argv_beats_stdin() {
    let mock = workspace_bin("mock-claude");
    let mk = |content: &[u8]| -> (TempDir, PathBuf) {
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join("prompt.txt");
        fs::write(&path, content).expect("write prompt file");
        (dir, path)
    };
    let run_case =
        |input_file: Option<&Path>, positional: Option<&str>, stdin_file: Option<&Path>| {
            let (_home, config) = hermetic_home();
            let mut cmd = Command::new(workspace_bin("claude-print"));
            cmd.arg("--claude-binary").arg(&mock);
            cmd.arg("--config").arg(&config);
            cmd.env("MOCK_ECHO_PROMPT", "1");
            wire(&mut cmd, _home.path(), None, None);
            if let Some(f) = input_file {
                cmd.arg("--input-file").arg(f);
            }
            if let Some(p) = positional {
                cmd.arg(p);
            }
            let stdin = match stdin_file {
                Some(f) => Stdio::from(fs::File::open(f).expect("open stdin file")),
                None => Stdio::null(),
            };
            let out = run(&mut cmd, BUDGET, stdin);
            assert_eq!(
                out.code,
                Some(0),
                "precedence run must succeed; stderr: {}",
                out.stderr
            );
            out.stdout
        };

    let (_fdir, file_prompt) = mk(b"FROM-FILE");
    let (_sdir, stdin_prompt) = mk(b"FROM-STDIN");

    assert_eq!(
        run_case(Some(&file_prompt), Some("FROM-ARGV"), None),
        "FROM-FILE\n",
        "--input-file must beat the positional"
    );
    assert_eq!(
        run_case(None, Some("FROM-ARGV"), Some(&stdin_prompt)),
        "FROM-ARGV\n",
        "the positional must beat stdin"
    );
    assert_eq!(
        run_case(None, None, Some(&stdin_prompt)),
        "FROM-STDIN\n",
        "stdin must be used when it is the only source"
    );
}

/// §2/§4: an empty stdin with no other source is input validation — exit 4,
/// the documented plain line on stderr, stdout empty in every mode.
#[test]
fn empty_stdin_is_exit_4_with_the_documented_line() {
    let mock = workspace_bin("mock-claude");
    let (_home, config) = hermetic_home();
    let mut cmd = Command::new(workspace_bin("claude-print"));
    cmd.arg("--claude-binary").arg(&mock);
    cmd.arg("--config").arg(&config);
    wire(&mut cmd, _home.path(), None, None);

    let out = run(&mut cmd, BUDGET, Stdio::null());
    assert_eq!(
        out.code,
        Some(4),
        "empty prompt must exit 4; stderr: {}",
        out.stderr
    );
    assert!(
        out.stdout.is_empty(),
        "pre-emitter failures keep stdout clean: {:?}",
        out.stdout
    );
    assert_eq!(
        out.stderr, "claude-print: no prompt provided (pass as argument, --input-file, or stdin)\n",
        "the exit-4 line must match §2 exactly"
    );
}

/// §2: a NUL byte in the prompt is a policy rejection — exit 2 with the
/// documented offset-bearing line.
#[test]
fn null_byte_stdin_is_exit_2_at_the_documented_offset() {
    let mock = workspace_bin("mock-claude");
    let (_home, config) = hermetic_home();
    let nul = _home.path().join("nul-prompt.bin");
    fs::write(&nul, b"ab\0cd").expect("write nul prompt");
    let mut cmd = Command::new(workspace_bin("claude-print"));
    cmd.arg("--claude-binary").arg(&mock);
    cmd.arg("--config").arg(&config);
    wire(&mut cmd, _home.path(), None, None);
    let stdin = Stdio::from(fs::File::open(&nul).expect("open nul prompt"));

    let out = run(&mut cmd, BUDGET, stdin);
    assert_eq!(
        out.code,
        Some(2),
        "a NUL byte must exit 2; stderr: {}",
        out.stderr
    );
    assert!(
        out.stderr
            .contains("prompt contains a null byte at offset 2"),
        "the NUL rejection must carry the documented offset: {:?}",
        out.stderr
    );
}

/// §2: stdin past the 10 MiB cap is a policy rejection — exit 2 with the
/// documented limit line.
#[test]
fn oversize_stdin_is_exit_2_with_the_documented_limit() {
    let mock = workspace_bin("mock-claude");
    let (_home, config) = hermetic_home();
    let big = _home.path().join("big.bin");
    let payload = vec![b'x'; 10 * 1024 * 1024 + 1];
    let mut f = fs::File::create(&big).expect("create oversize stdin");
    f.write_all(&payload).expect("write oversize stdin");
    drop(f);
    let mut cmd = Command::new(workspace_bin("claude-print"));
    cmd.arg("--claude-binary").arg(&mock);
    cmd.arg("--config").arg(&config);
    wire(&mut cmd, _home.path(), None, None);
    let stdin = Stdio::from(fs::File::open(&big).expect("open oversize stdin"));

    let out = run(&mut cmd, BUDGET, stdin);
    assert_eq!(
        out.code,
        Some(2),
        "oversize stdin must exit 2; stderr: {}",
        out.stderr
    );
    assert!(
        out.stderr
            .contains("claude-print: stdin is larger than the 10485760-byte limit"),
        "the oversize rejection must carry the documented limit: {:?}",
        out.stderr
    );
}

/// §2: a non-regular `--input-file` is input validation — exit 4 naming the
/// path (a character device is not a regular file).
#[test]
fn input_file_non_regular_is_exit_4() {
    let mock = workspace_bin("mock-claude");
    let (_home, config) = hermetic_home();
    let mut cmd = Command::new(workspace_bin("claude-print"));
    cmd.arg("--claude-binary").arg(&mock);
    cmd.arg("--config").arg(&config);
    cmd.arg("--input-file").arg("/dev/null");
    wire(&mut cmd, _home.path(), None, None);

    let out = run(&mut cmd, BUDGET, Stdio::null());
    assert_eq!(
        out.code,
        Some(4),
        "a non-regular --input-file must exit 4; stderr: {}",
        out.stderr
    );
    assert!(
        out.stderr.starts_with("claude-print: ") && out.stderr.contains("/dev/null"),
        "the exit-4 line must name the path behind the claude-print: prefix: {:?}",
        out.stderr
    );
}

/// §8 divergence 1: the print flag itself is rejected by claude-print's own
/// parser — an argv usage error, exit 2, before anything spawns.
#[test]
fn unknown_flag_is_exit_2_usage_error() {
    let mock = workspace_bin("mock-claude");
    let (_home, config) = hermetic_home();
    let mut cmd = Command::new(workspace_bin("claude-print"));
    cmd.arg("--claude-binary").arg(&mock);
    cmd.arg("--config").arg(&config);
    cmd.arg("--definitely-not-a-claude-print-flag");
    wire(&mut cmd, _home.path(), None, None);

    let out = run(&mut cmd, BUDGET, Stdio::null());
    assert_eq!(
        out.code,
        Some(2),
        "an unknown flag must exit 2; stderr: {}",
        out.stderr
    );
    assert!(
        !out.stderr.is_empty(),
        "a usage error must explain itself on stderr"
    );
}

/// §4: a missing claude binary is a setup failure — exit 2, the documented
/// not-found line naming the path.
#[test]
fn missing_binary_is_exit_2_naming_the_path() {
    let (_home, config) = hermetic_home();
    let missing = "/compat-no-such-dir/claude-missing";
    let mut cmd = Command::new(workspace_bin("claude-print"));
    cmd.arg("--claude-binary").arg(missing);
    cmd.arg("--config").arg(&config);
    cmd.arg("prompt");
    wire(&mut cmd, _home.path(), None, None);

    let out = run(&mut cmd, BUDGET, Stdio::null());
    assert_eq!(
        out.code,
        Some(2),
        "a missing binary must exit 2; stderr: {}",
        out.stderr
    );
    assert!(
        out.stderr.contains(missing) && out.stderr.contains("not found in PATH"),
        "the setup error must name the missing path: {:?}",
        out.stderr
    );
}

// ── §4: the exit-code table is the error type's mapping ─────────────────────

#[test]
fn exit_code_table_matches_the_error_type() {
    let doc = contract_doc();
    let section = doc_section(&doc, "## 4. Exit codes", "## 5. Signal mapping");
    let mut rows: Vec<(i32, String)> = table_rows(section)
        .into_iter()
        .filter_map(|cells| {
            let first = cells.first()?.trim_matches('`').to_string();
            first
                .parse::<i32>()
                .ok()
                .map(|exit| (exit, cells[1].clone()))
        })
        .collect();
    rows.sort_by_key(|(exit, _)| *exit);
    assert_eq!(
        rows.iter().map(|(e, _)| *e).collect::<Vec<_>>(),
        vec![0, 1, 2, 4, 124, 130],
        "§4 must document exactly the six exit codes"
    );

    // `subtype` cells: the success row names success, the exit-4 row names no
    // subtype, and the four error rows equal the error type's mapping.
    let subtype_of = |exit: i32| -> String {
        rows.iter()
            .find(|(e, _)| *e == exit)
            .map(|(_, s)| s.clone())
            .expect("row present")
    };
    assert_eq!(subtype_of(0).trim_matches('`'), "success");
    assert!(
        subtype_of(4).contains("none"),
        "exit 4 must be documented as carrying no JSON subtype: {:?}",
        subtype_of(4)
    );

    let cases: Vec<(ClaudePrintError, i32, &str)> = vec![
        (ClaudePrintError::Setup("x".into()), 2, "internal_error"),
        (ClaudePrintError::Config("x".into()), 2, "internal_error"),
        (
            ClaudePrintError::AssistantError("x".into()),
            1,
            "assistant_error",
        ),
        (ClaudePrintError::Timeout, 124, "timeout"),
        (ClaudePrintError::Interrupted, 130, "interrupted"),
    ];
    for (err, exit, subtype) in cases {
        assert_eq!(
            (err.exit_code(), err.subtype()),
            (exit, subtype),
            "the error type must map {subtype} to the §4 row for exit {exit}"
        );
        assert_eq!(
            subtype_of(exit).trim_matches('`'),
            subtype,
            "§4's subtype for exit {exit} must equal the error type's"
        );
    }
}

// ── §5: signals ──────────────────────────────────────────────────────────────

#[test]
fn signal_section_matches_the_wiring_and_cites_live_suites() {
    let doc = contract_doc();
    let section = doc_section(&doc, "## 5. Signal mapping", "## 6. Child argv");
    assert!(
        section.contains("130") && section.contains("interrupted"),
        "§5's session rows must name the exit code and subtype the error type returns"
    );

    // The session-path wiring §5 describes: intercepting handlers whose
    // teardown escalates to SIGKILL, in the source that arms them.
    let session = src_source("src/session.rs");
    for marker in ["sigint_handler", "sigterm_handler", "SIGTERM", "SIGKILL"] {
        assert!(
            session.contains(marker),
            "src/session.rs must carry the §5 teardown wiring ({marker})"
        );
    }

    // The relay rows §5 defers to the behavioral suites — the suites must
    // exist for the citation to mean anything.
    for suite in [
        "tests/sigint_forwarding_e2e.rs",
        "tests/sigwinch_forwarding_e2e.rs",
    ] {
        assert!(
            repo_root().join(suite).exists(),
            "§5 cites {suite}; the suite must exist"
        );
        assert!(
            section.contains(suite),
            "§5 must cite {suite} for the relay mapping"
        );
    }
}

// ── the doc names its own pin ────────────────────────────────────────────────

#[test]
fn doc_names_this_suite_as_its_pin() {
    let doc = contract_doc();
    assert!(
        doc.contains("tests/claude_p_compat_contract.rs"),
        "the contract doc's Pinned-by row must name this suite"
    );
}
