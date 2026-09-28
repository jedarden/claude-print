//! `--mcp-config` bound-MCP-init contract (bead claudepr-d000e48b).
//!
//! `docs/notes/mcp-config-contract.md` is the normative definition behind
//! the compat contract's one-line child-argv transformation and the
//! config-file contract's no-config-tier boundary section. This suite holds
//! that document against the implementation in both directions:
//!
//!   * **source pins** — the `Cli` field's `value_delimiter`, the
//!     `LaunchOptions` wiring, `build_child_argv`'s strict-once/pair-per-entry
//!     construction and the `mcp-config value invalid` NUL-guard literal, and
//!     the README / compat-contract / config-file-contract cross-links;
//!   * **parse level** — `Cli::try_parse_from` over the delimiting edges the
//!     forwarding legs never spelled out: empty values, empty comma segments,
//!     leading/trailing commas, whitespace preservation, duplicates, mixed
//!     occurrence append, `serve` accepting the top-level flag, and the
//!     missing-value usage error;
//!   * **end to end** — the compiled binary against mock-claude: the
//!     missing-value error is mode-independent (text and json byte-identical,
//!     clap answers before any mode logic); the edge entries forward verbatim
//!     with the empty entry arming strict mode; one invocation naming a
//!     nonexistent path, malformed inline JSON, and an empty entry completes
//!     successfully with all three forwarded (claude-print validated nothing —
//!     the child is the validator); a child that exits before the Stop payload
//!     with the flag present exits 2 with the per-mode shapes (text stderr
//!     line; json/stream-json one-line `internal_error` object on stdout),
//!     and `--show-child-stderr` adds the child's captured bytes on stderr;
//!     `--no-inherit-hooks` orders `--setting-sources=` before the strict
//!     segment.
//!
//! Hermetic: compiled `claude-print` + mock-claude; every run gets a
//! throwaway `HOME`, an explicit empty `--config`, and its own working
//! directory, and mock knobs travel only on the child's environment — this
//! process never mutates its own environment.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use clap::Parser;
use claude_print::cli::Cli;
use tempfile::TempDir;

// ── repo / doc plumbing (the `tests/show_child_stderr_contract.rs` pattern) ──

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

fn read_repo_file(rel: &str) -> String {
    let path = repo_root().join(rel);
    fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("repo file missing at {}: {e}", path.display()))
}

// ── source pins ──────────────────────────────────────────────────────────────

/// §2/§3: the flag is a repeatable comma-delimited `Cli` field wired into the
/// session's launch options — the three places a regression would detach the
/// command-line surface from the child-argv segment.
#[test]
fn the_flag_is_a_delimited_cli_field_wired_into_launch_options() {
    let cli = read_repo_file("src/cli.rs");
    assert!(
        cli.contains("#[arg(long = \"mcp-config\", value_delimiter = ',')]"),
        "--mcp-config must stay a repeatable comma-delimited CLI field"
    );

    let main_rs = read_repo_file("src/main.rs");
    assert!(
        main_rs.contains("mcp_configs: cli.mcp_config.clone()"),
        "main must wire cli.mcp_config into LaunchOptions"
    );

    let session = read_repo_file("src/session.rs");
    assert!(
        session.contains("pub mcp_configs: Vec<String>"),
        "LaunchOptions must carry the entry list"
    );
}

/// §3/§4: the child-argv segment is constructed exactly as documented —
/// `--strict-mcp-config` once behind the non-empty gate, one
/// `--mcp-config <entry>` pair per entry — and the one claude-print-side
/// content check is the argv NUL guard, whose literal the doc quotes.
#[test]
fn the_strict_segment_construction_and_nul_guard_match_the_source() {
    let session = read_repo_file("src/session.rs");
    assert!(
        session.contains("if !launch.mcp_configs.is_empty() {"),
        "the segment must stay gated on list emptiness (an empty ENTRY still arms it)"
    );
    assert!(
        session.contains("args.push(CString::new(\"--strict-mcp-config\").unwrap());"),
        "--strict-mcp-config must be pushed exactly once, leading the segment"
    );
    assert!(
        session.contains("\"mcp-config value invalid: {e}\""),
        "the NUL-guard error literal must stay as documented (§4)"
    );

    let doc = read_repo_file("docs/notes/mcp-config-contract.md");
    assert!(
        doc.contains("mcp-config value invalid"),
        "the doc must quote the NUL-guard literal"
    );
    assert!(
        doc.contains("--strict-mcp-config --mcp-config <entry> [--mcp-config <entry> …]"),
        "the doc must show the segment shape"
    );
    assert!(
        doc.contains("claude exited before Stop hook fired"),
        "the doc must carry the child-side rejection error message (§5)"
    );
}

/// §1 cross-links: the README row points at this contract, the compat
/// contract keeps its §1 dedicated-contract pointer, and the config-file
/// contract's boundary section references the note it coexists with.
#[test]
fn readme_compat_and_config_contract_stay_linked_to_the_contract() {
    let readme = read_repo_file("README.md");
    let row_start = readme
        .find("| `--mcp-config <MCP_CONFIG>` |")
        .expect("README's flag table must keep the --mcp-config row");
    let row_end = readme[row_start..]
        .find('\n')
        .expect("table rows are single lines");
    let row = &readme[row_start..row_start + row_end];
    assert!(
        row.contains("docs/notes/mcp-config-contract.md"),
        "the README row must link the contract: {row}"
    );

    let compat = read_repo_file("docs/notes/claude-p-compat-contract.md");
    assert!(
        compat.contains("`--mcp-config` has a dedicated contract"),
        "compat §1 must keep the dedicated-contract paragraph"
    );
    assert!(
        compat.contains("docs/notes/mcp-config-contract.md"),
        "compat §1 must point readers at the dedicated contract"
    );

    let config_contract = read_repo_file("docs/notes/config-file-contract.md");
    assert!(
        config_contract.contains("docs/notes/mcp-config-contract.md"),
        "the config-file contract's mcp boundary section must reference the dedicated note"
    );
}

// ── parse level (§2) ─────────────────────────────────────────────────────────

/// The three spellings of one occurrence, and accumulation across occurrences:
/// later occurrences append in command-line order.
#[test]
fn spellings_parse_and_occurrences_append_in_order() {
    let repeated = Cli::try_parse_from(["cp", "--mcp-config", "a", "--mcp-config", "b"])
        .expect("repeated spelling parses");
    assert_eq!(repeated.mcp_config, ["a", "b"]);

    let comma = Cli::try_parse_from(["cp", "--mcp-config", "a,b"]).expect("comma spelling parses");
    assert_eq!(comma.mcp_config, repeated.mcp_config);

    let equals = Cli::try_parse_from(["cp", "--mcp-config=a,b"]).expect("=-joined spelling parses");
    assert_eq!(equals.mcp_config, repeated.mcp_config);

    let mixed = Cli::try_parse_from(["cp", "--mcp-config", "a,b", "--mcp-config", "c"])
        .expect("mixed occurrences parse");
    assert_eq!(mixed.mcp_config, ["a", "b", "c"]);
}

/// The delimiting edges §2 pins: the split is purely lexical — empty entries
/// are legal and preserved, whitespace is never trimmed, duplicates stay.
#[test]
fn delimiting_edges_parse_verbatim() {
    let cases: &[(&[&str], Vec<&str>)] = &[
        // One empty value → one empty entry.
        (&["--mcp-config", ""], vec![""]),
        // Interior, lone, leading, and trailing commas each contribute an
        // empty segment.
        (&["--mcp-config", "a,,b"], vec!["a", "", "b"]),
        (&["--mcp-config", ","], vec!["", ""]),
        (&["--mcp-config", "a,"], vec!["a", ""]),
        (&["--mcp-config", ",a"], vec!["", "a"]),
        // No trimming: spaces are part of the entry.
        (&["--mcp-config", " a , b "], vec![" a ", " b "]),
        (&["--mcp-config", " "], vec![" "]),
        // No deduplication.
        (&["--mcp-config", "a", "--mcp-config", "a"], vec!["a", "a"]),
    ];
    for (args, expected) in cases {
        let cli = Cli::try_parse_from(std::iter::once("cp").chain(args.iter().copied()))
            .unwrap_or_else(|e| panic!("{args:?} must parse: {e}"));
        assert_eq!(
            cli.mcp_config, *expected,
            "the lexical split must be verbatim for {args:?}"
        );
    }

    // Absent flag → empty list (forwards nothing, §3).
    let bare = Cli::try_parse_from(["cp"]).expect("bare invocation parses");
    assert!(
        bare.mcp_config.is_empty(),
        "no --mcp-config must mean no entries"
    );
}

/// §2/§6: a value is required (the documented usage error), and `serve`
/// accepts the top-level flag — parser surface, daemon path never reads it.
#[test]
fn a_value_is_required_and_serve_accepts_the_top_level_flag() {
    let err = Cli::try_parse_from(["cp", "--mcp-config"])
        .expect_err("--mcp-config without a value must be a usage error");
    assert!(
        err.to_string()
            .contains("a value is required for '--mcp-config <MCP_CONFIG>'"),
        "the usage error must name the flag: {err}"
    );

    let serve = Cli::try_parse_from(["cp", "--mcp-config", "/x.json", "serve"])
        .expect("the top-level flag parses ahead of serve");
    assert_eq!(serve.mcp_config, ["/x.json"]);
    assert!(
        serve.command.is_some(),
        "the serve subcommand must still dispatch with the flag accepted"
    );
}

// ── binary-e2e plumbing (the `tests/show_child_stderr_contract.rs` pattern) ──

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

/// Per-run wall-clock budget: mock-claude responds within ~2 s; a dying child
/// exits faster; 60 s only absorbs a loaded box while still failing on a wedge.
const BUDGET: Duration = Duration::from_secs(60);

/// One test's hermetic world: a throwaway `HOME` (transcripts and the config
/// probe land here), an explicit empty `--config`, and a private working
/// directory.
struct World {
    home: TempDir,
    workdir: TempDir,
    config: PathBuf,
}

impl World {
    fn new() -> Self {
        let home = TempDir::new().expect("temp home");
        let workdir = TempDir::new().expect("temp workdir");
        let config = home.path().join("config.toml");
        fs::write(&config, "").expect("write empty config");
        Self {
            home,
            workdir,
            config,
        }
    }

    /// A `claude-print` command in this world: hermetic HOME + workdir, the
    /// mock backend and the explicit empty config; the caller adds mock knobs
    /// (child env) and flags.
    fn cmd(&self) -> Command {
        let mut cmd = Command::new(workspace_bin("claude-print"));
        cmd.current_dir(self.workdir.path());
        cmd.env("HOME", self.home.path());
        cmd.arg("--claude-binary").arg(workspace_bin("mock-claude"));
        cmd.arg("--config").arg(&self.config);
        cmd
    }
}

struct Outcome {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

/// Run `cmd` to completion with a null stdin; kill (and fail) past `budget`.
fn run(cmd: &mut Command, budget: Duration) -> Outcome {
    cmd.stdin(Stdio::null())
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

/// Read mock-claude's MOCK_RECORD_ARGS dump (NUL-separated argv, one trailing
/// NUL) preserving **empty arguments** — unlike `tests/binary_e2e.rs`'s reader,
/// which filters them: an empty `--mcp-config` entry must survive the read to
/// be assertable. Only the terminator's trailing empty segment is dropped.
fn read_recorded_argv(path: &Path) -> Vec<String> {
    let bytes = fs::read(path).unwrap_or_else(|e| {
        panic!(
            "MOCK_RECORD_ARGS file was not written at {}: {e}",
            path.display()
        )
    });
    let mut parts: Vec<String> = bytes
        .split(|b| *b == 0)
        .map(|s| String::from_utf8_lossy(s).into_owned())
        .collect();
    if parts.last().is_some_and(|s| s.is_empty()) {
        parts.pop();
    }
    parts
}

/// One recorded run in a fresh world: `extra_envs` are additional child-env
/// mock knobs, `flags` precede the prompt. `MOCK_RECORD_ARGS` itself is wired
/// here, pointing at a file under the throwaway HOME. Returns the outcome and
/// the decoded child argv.
fn run_recorded(extra_envs: &[(&str, &str)], flags: &[&str]) -> (Outcome, Vec<String>) {
    let world = World::new();
    let record = world.home.path().join("child_argv");
    let mut cmd = world.cmd();
    cmd.env("MOCK_RECORD_ARGS", &record);
    for (k, v) in extra_envs {
        cmd.env(k, v);
    }
    for flag in flags {
        cmd.arg(flag);
    }
    cmd.arg("contract probe prompt");
    let out = run(&mut cmd, BUDGET);
    (out, read_recorded_argv(&record))
}

/// The `--mcp-config` forwarding segment of a recorded child argv: the values
/// following the (unique) `--strict-mcp-config` as `--mcp-config <value>`
/// pairs. Panics when the strict flag is missing — the flag-absent shape is
/// asserted by the existing suites, so here a missing flag is a recording or
/// forwarding defect worth failing on.
fn mcp_entry_values(args: &[String]) -> Vec<String> {
    let occurrences = args
        .iter()
        .filter(|a| a.as_str() == "--strict-mcp-config")
        .count();
    assert_eq!(
        occurrences, 1,
        "--strict-mcp-config must appear EXACTLY ONCE, got {occurrences} in {args:?}"
    );
    let strict = args
        .iter()
        .position(|a| a == "--strict-mcp-config")
        .expect("strict flag present (counted above)");
    let mut values = Vec::new();
    let mut i = strict + 1;
    while args.get(i).map(String::as_str) == Some("--mcp-config") {
        values.push(
            args.get(i + 1)
                .cloned()
                .unwrap_or_else(|| panic!("--mcp-config at index {i} without a value: {args:?}")),
        );
        i += 2;
    }
    values
}

// ── end to end: usage-error mode-independence (§5) ───────────────────────────

/// §5: the missing-value usage error is answered by clap before any mode
/// logic — text and json invocations exit 2 with byte-identical stderr, empty
/// stdout, and no prompt/config complaint (parse wins over everything).
#[test]
fn the_missing_value_usage_error_is_mode_independent() {
    let mut outcomes = Vec::new();
    for extra in [
        vec![],                                 // text (default)
        vec!["--output-format", "json"],        // json
        vec!["--output-format", "stream-json"], // stream-json
    ] {
        let world = World::new();
        let mut cmd = world.cmd();
        for flag in &extra {
            cmd.arg(flag);
        }
        cmd.arg("--mcp-config"); // no value, no prompt: clap must answer first
        outcomes.push(run(&mut cmd, BUDGET));
    }

    for out in &outcomes {
        assert_eq!(
            out.code,
            Some(2),
            "missing value: expected clap usage exit 2\nstdout:\n{}\nstderr:\n{}",
            out.stdout,
            out.stderr
        );
        assert!(
            out.stdout.is_empty(),
            "clap answers before any emitter: stdout must be empty — {}",
            out.stdout
        );
        assert!(
            out.stderr
                .contains("a value is required for '--mcp-config <MCP_CONFIG>'"),
            "the clap usage error must name the flag: {}",
            out.stderr
        );
        assert!(
            !out.stderr.contains("no prompt"),
            "the usage error must precede prompt handling: {}",
            out.stderr
        );
    }
    assert_eq!(
        outcomes[0].stderr, outcomes[1].stderr,
        "text and json must fail byte-identically (mode-independent)"
    );
    assert_eq!(
        outcomes[0].stderr, outcomes[2].stderr,
        "text and stream-json must fail byte-identically (mode-independent)"
    );
}

// ── end to end: forwarding of the edges (§3) ─────────────────────────────────

/// §3: the gate is list emptiness, not entry emptiness — a single empty entry
/// arms strict mode and forwards an empty value; whitespace entries cross
/// verbatim; duplicates stay; one `--mcp-config <entry>` pair per entry, in
/// order, contiguously behind the strict flag (the pair walk inside
/// `mcp_entry_values` proves the shape).
#[test]
fn empty_and_whitespace_entries_forward_verbatim_and_arm_strict_mode() {
    let (out, args) = run_recorded(
        &[],
        &[
            "--mcp-config",
            "",
            "--mcp-config",
            " a , b ",
            "--mcp-config",
            "x,x",
        ],
    );
    assert_eq!(
        out.code,
        Some(0),
        "the delimiting edges are legal input, not validation errors — the run \
         must complete\nstdout:\n{}\nstderr:\n{}",
        out.stdout,
        out.stderr
    );
    assert_eq!(
        mcp_entry_values(&args),
        ["", " a ", " b ", "x", "x"],
        "entries must forward verbatim: the empty entry preserved (and arming \
         strict mode, or mcp_entry_values would have panicked), whitespace \
         untrimmed, duplicates kept: {args:?}"
    );
}

// ── end to end: the validation boundary (§4) ─────────────────────────────────

/// §4: claude-print validates nothing about entry content. One invocation
/// naming a nonexistent path, a malformed inline JSON object, and an empty
/// entry still spawns, forwards all three verbatim, and completes
/// successfully whenever the child does not care — the mock child accepts
/// anything, so the successful round trip (response emitted, `MOCK_ECHO_PROMPT`
/// makes it the prompt) proves no claude-print-side existence or parse check
/// exists. A real `claude` would reject the value and take the §5 legs below.
#[test]
fn nonexistent_malformed_and_empty_entries_are_forwarded_not_validated() {
    const MISSING: &str = "/nonexistent-mcp-contract/does-not-exist.json";
    const MALFORMED: &str = "{\"mcpServers\":{\"fs\":{\"command\":";
    let (out, args) = run_recorded(
        &[("MOCK_ECHO_PROMPT", "1")],
        &[
            "--mcp-config",
            MISSING,
            "--mcp-config",
            MALFORMED,
            "--mcp-config",
            "",
        ],
    );
    assert_eq!(
        out.code,
        Some(0),
        "no claude-print-side content validation: against a child that accepts \
         anything the run must complete\nstdout:\n{}\nstderr:\n{}",
        out.stdout,
        out.stderr
    );
    assert!(
        out.stdout.contains("contract probe prompt"),
        "the response must be emitted — the session genuinely completed, which \
         is what makes the verbatim forwarding meaningful: {}",
        out.stdout
    );
    assert_eq!(
        mcp_entry_values(&args),
        [MISSING, MALFORMED, ""],
        "all three entries must cross to the child argv byte-identically: {args:?}"
    );
}

// ── end to end: child-side rejection per mode (§5) ───────────────────────────

/// The `--show-child-stderr` block's header/end markers and the pre-injection
/// death reason (that contract's literals, quoted here only as needles).
const DUMP_HEADER: &str = "claude-print: ----- child PTY output (";
const DUMP_END: &str = "claude-print: ----- end child output -----";
const PRE_INJECTION_REASON: &str = "child exited before prompt was injected";

/// The exact structured object §5 specifies, asserted field-for-field on one
/// stdout line (json and stream-json alike on this arm).
fn assert_internal_error_result(line: &str, format: &str) {
    let value: serde_json::Value = serde_json::from_str(line)
        .unwrap_or_else(|e| panic!("format={format}: stdout must parse — {e}\n{line}"));
    assert_eq!(value["type"], "result", "format={format}: {line}");
    assert_eq!(
        value["subtype"], "internal_error",
        "format={format}: {line}"
    );
    assert_eq!(value["is_error"], true, "format={format}: {line}");
    assert_eq!(
        value["error_message"], "claude exited before Stop hook fired",
        "format={format}: {line}"
    );
}

/// §5: the modeled Claude-side validation failure — the child rejects the
/// named config and exits before the Stop payload (`MOCK_EXIT_BEFORE_STOP`)
/// — surfaces through the generic session error path with the same exit 2 in
/// every mode: text keeps stdout empty with the `error:` line on stderr;
/// json and stream-json each put exactly the one-line `result`/`internal_error`
/// object on stdout. The stream-json case is exactly one line because the
/// session died before prompt injection — no transcript records were ever
/// streamed, only the synthesized object lands.
#[test]
fn child_side_rejection_is_reported_per_mode_with_the_same_exit() {
    for format in ["text", "json", "stream-json"] {
        let world = World::new();
        let mut cmd = world.cmd();
        cmd.env("MOCK_EXIT_BEFORE_STOP", "1");
        if format != "text" {
            cmd.args(["--output-format", format]);
        }
        cmd.args(["--mcp-config", "/nonexistent-mcp-contract/rejected.json"]);
        cmd.arg("a prompt the dying child never answers");
        let out = run(&mut cmd, BUDGET);

        assert_eq!(
            out.code,
            Some(2),
            "format={format}: the mode shapes the report, never the exit — \
             stderr:\n{}",
            out.stderr
        );
        if format == "text" {
            assert!(
                out.stdout.is_empty(),
                "format=text: stdout must stay empty — stdout:\n{}",
                out.stdout
            );
            assert!(
                out.stderr
                    .contains("error: claude exited before Stop hook fired"),
                "format=text: the error line must name the family — stderr:\n{}",
                out.stderr
            );
        } else {
            let lines: Vec<&str> = out
                .stdout
                .lines()
                .filter(|l| !l.trim().is_empty())
                .collect();
            assert_eq!(
                lines.len(),
                1,
                "format={format}: stdout must be exactly the one-line result \
                 object — stdout:\n{}",
                out.stdout
            );
            assert_internal_error_result(lines[0], format);
        }
    }
}

/// §5/§4: `--show-child-stderr` adds the child's captured bytes on stderr —
/// the only place the failing config's own complaint could ever appear, a
/// real child's error text naming it — while the stdout object stays intact.
#[test]
fn show_child_stderr_adds_the_capture_while_the_stdout_object_stays_intact() {
    let world = World::new();
    let mut cmd = world.cmd();
    cmd.env("MOCK_EXIT_BEFORE_STOP", "1");
    cmd.args(["--output-format", "json", "--show-child-stderr"]);
    cmd.args(["--mcp-config", "/nonexistent-mcp-contract/rejected.json"]);
    cmd.arg("a prompt the dying child never answers");
    let out = run(&mut cmd, BUDGET);

    assert_eq!(out.code, Some(2), "stderr:\n{}", out.stderr);
    assert_eq!(
        out.stderr.matches(DUMP_HEADER).count(),
        1,
        "exactly one dump block on stderr — stderr:\n{}",
        out.stderr
    );
    assert!(
        out.stderr.contains(DUMP_END),
        "the dump block must be closed — stderr:\n{}",
        out.stderr
    );
    assert!(
        out.stderr.contains(PRE_INJECTION_REASON),
        "the dump header must carry the pre-injection death reason — stderr:\n{}",
        out.stderr
    );

    let lines: Vec<&str> = out
        .stdout
        .lines()
        .filter(|l| !l.trim().is_empty())
        .collect();
    assert_eq!(
        lines.len(),
        1,
        "the stdout object must stay intact beside the stderr dump — stdout:\n{}",
        out.stdout
    );
    assert_internal_error_result(lines[0], "json+--show-child-stderr");
}

// ── end to end: isolation ordering (§3) ──────────────────────────────────────

/// §3: in isolation mode the `--setting-sources=` suppression precedes the
/// strict MCP segment — the recorded argv pins the documented order (relay
/// `--settings=`, then `--setting-sources=`, then the segment).
#[test]
fn isolation_mode_orders_setting_sources_ahead_of_the_strict_segment() {
    let (out, args) = run_recorded(
        &[],
        &["--no-inherit-hooks", "--mcp-config", "/ordered.json"],
    );
    assert_eq!(
        out.code,
        Some(0),
        "isolation plus the flag is an ordinary successful run\nstdout:\n{}\nstderr:\n{}",
        out.stdout,
        out.stderr
    );
    let settings = args
        .iter()
        .position(|a| a.starts_with("--settings="))
        .expect("the relay --settings= is always forwarded");
    let sources = args
        .iter()
        .position(|a| a == "--setting-sources=")
        .expect("isolation mode forwards --setting-sources=");
    let strict = args
        .iter()
        .position(|a| a == "--strict-mcp-config")
        .expect("the MCP segment is present (values asserted below)");
    assert!(
        settings < sources && sources < strict,
        "the documented order is relay --settings=, then --setting-sources=, \
         then --strict-mcp-config: {args:?}"
    );
    assert_eq!(
        mcp_entry_values(&args),
        ["/ordered.json"],
        "the entry must still forward behind the suppression: {args:?}"
    );
}
