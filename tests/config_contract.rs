//! Config-file contract pin (bead claudepr-227efdb1).
//!
//! `docs/notes/config-file-contract.md` is the normative definition of the
//! optional TOML config file: path resolution (`--config` / `XDG_CONFIG_HOME` /
//! `HOME` precedence), the `[defaults]` schema and its four keys, built-in
//! defaults, CLI-over-config precedence, missing-file behavior, validation
//! rules, and the exact error shapes. This test keeps that document honest by
//! loading `tests/fixtures/config_contract_examples_v1.json` and checking
//! layers that each catch a different half of the drift:
//!
//! 1. **Loader alignment** — every fixture case is replayed through
//!    `Config::load_or_default` from a temp cwd (so the doc's bare relative
//!    filenames reproduce byte-for-byte in the error messages): TOML that
//!    must parse resolves to the fixture's expected values through the real
//!    `resolve_*` tiering, and TOML that must fail produces exactly the
//!    fixture's user-facing message (`ClaudePrintError::from(err).message()`).
//! 2. **Path alignment** — every `default_path` rule replays under env
//!    guards, pinning the XDG-over-HOME precedence including the two sharp
//!    edges the doc states: an empty-but-set `XDG_CONFIG_HOME` is used as-is
//!    (cwd-relative path), and a non-UTF-8 one falls back to `$HOME`.
//! 3. **Table alignment** — the fixture's key table is asserted against the
//!    resolvers' built-in defaults and against clap's actual parser defaults,
//!    which is the mechanism of the documented `max_turns`/`timeout_secs`
//!    limitation: a clap `default_value` makes an absent flag
//!    indistinguishable from an explicit one, so the config tier never fires.
//! 4. **Emitter alignment** — the documented text/json/stream-json error
//!    lines are replayed through `emit_error` and byte-compared, so the
//!    output shapes in the doc and README cannot drift from `src/emitter.rs`.
//! 5. **Doc alignment** — every `documented: true` example must appear
//!    verbatim in the contract doc (and, when `also_in_readme`, in the
//!    README's Configuration section) — an example edited on one side
//!    without the other fails here.
//!
//! A contract change therefore updates implementation, fixture, and document
//! together in one commit — which is the point.

use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::OsStringExt;
use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard};

use clap::CommandFactory;
use serde::Deserialize;

use claude_print::cli::{Cli, OutputFormat};
use claude_print::config::Config;
use claude_print::emitter::emit_error;
use claude_print::error::ClaudePrintError;

const FIXTURE: &str = include_str!("fixtures/config_contract_examples_v1.json");
const DOC: &str = include_str!("../docs/notes/config-file-contract.md");
const README: &str = include_str!("../README.md");

/// Env and cwd are process-global; every test that touches them takes this
/// lock so the panics of one replay cannot poison another's environment.
static PROCESS_LOCK: Mutex<()> = Mutex::new(());

fn process_lock() -> MutexGuard<'static, ()> {
    PROCESS_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

// ── fixture schema ───────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct Fixture {
    contract_version: String,
    document: String,
    pinned_by: String,
    readme: String,
    keys: Vec<KeyRow>,
    default_path: Vec<PathRule>,
    cases: Vec<Case>,
    emitted: Vec<Emitted>,
}

#[derive(Debug, Deserialize)]
struct KeyRow {
    name: String,
    toml_type: String,
    builtin_default: serde_json::Value,
    cli_flag: String,
}

#[derive(Debug, Deserialize)]
struct PathRule {
    id: String,
    xdg: String,
    home: String,
    documented: Option<bool>,
    also_in_readme: Option<bool>,
    expect_path: Option<String>,
    expect_error: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Case {
    id: String,
    documented: Option<bool>,
    /// The error/emitted payload also appears in the README.
    also_in_readme: Option<bool>,
    /// The TOML block itself also appears in the README (the README shows
    /// some errors without the file that produced them, and vice versa).
    toml_in_readme: Option<bool>,
    file: String,
    toml: Option<String>,
    missing: Option<bool>,
    directory: Option<bool>,
    cli: Option<CliOverride>,
    expect: Option<Expect>,
    error: Option<String>,
}

#[derive(Debug, Deserialize, Clone, Default)]
struct CliOverride {
    model: Option<String>,
    inherit_hooks: Option<bool>,
    max_turns: Option<u32>,
    timeout_secs: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct Expect {
    model: String,
    inherit_hooks: bool,
    max_turns: u32,
    timeout_secs: u64,
}

#[derive(Debug, Deserialize)]
struct Emitted {
    id: String,
    documented: Option<bool>,
    also_in_readme: Option<bool>,
    format: String,
    claude_version: String,
    message: String,
    expected_stdout: String,
    expected_stderr: String,
}

fn fixture() -> Fixture {
    serde_json::from_str(FIXTURE).expect("config_contract_examples_v1.json must parse")
}

// ── process-state guard ──────────────────────────────────────────────────────

/// Captures cwd/`HOME`/`XDG_CONFIG_HOME` and restores them on drop — including
/// unwinding through a failed assertion — so replays leave the environment as
/// they found it for the rest of this test binary.
struct ProcessGuard {
    prev_cwd: PathBuf,
    prev_home: Option<OsString>,
    prev_xdg: Option<OsString>,
}

impl ProcessGuard {
    fn capture() -> Self {
        Self {
            prev_cwd: std::env::current_dir().expect("current dir must be readable"),
            prev_home: std::env::var_os("HOME"),
            prev_xdg: std::env::var_os("XDG_CONFIG_HOME"),
        }
    }
}

impl Drop for ProcessGuard {
    fn drop(&mut self) {
        let _ = std::env::set_current_dir(&self.prev_cwd);
        match self.prev_home.take() {
            Some(value) => std::env::set_var("HOME", value),
            None => std::env::remove_var("HOME"),
        }
        match self.prev_xdg.take() {
            Some(value) => std::env::set_var("XDG_CONFIG_HOME", value),
            None => std::env::remove_var("XDG_CONFIG_HOME"),
        }
    }
}

/// The message a user sees for a config failure: `main.rs` converts the
/// library error through `From<Error> for ClaudePrintError` before emitting,
/// and that conversion is where `invalid config at <path>` becomes
/// `invalid config: <path>` — replaying it keeps the doc's shapes honest
/// end to end.
fn user_facing(err: claude_print::error::Error) -> String {
    ClaudePrintError::from(err).message().to_string()
}

/// A message appears in a markdown doc either raw (text-mode `error:` line,
/// code block) or JSON-escaped (the json/stream-json example embeds the
/// parse error with `\n` escapes). Parse-tier messages carry a trailing LF
/// that a fenced block hides, so the raw check strips it.
fn contains_message(haystack: &str, message: &str) -> bool {
    let stripped = message.strip_suffix('\n').unwrap_or(message);
    haystack.contains(stripped) || haystack.contains(&serde_json::to_string(message).unwrap())
}

// ── layer 1: loader alignment ────────────────────────────────────────────────

/// Every fixture case replays through `Config::load_or_default` from a temp
/// cwd under the doc's bare filenames. Successful parses resolve through the
/// real precedence tiering (CLI override → config value → built-in default);
/// failures must produce exactly the documented user-facing message.
#[test]
fn fixture_cases_replay_against_load_or_default() {
    let _lock = process_lock();
    let fx = fixture();

    for case in &fx.cases {
        // Declared before the guard so cwd is restored before the tempdir
        // drops — removing a directory that is the process cwd fails on
        // some platforms.
        let dir = tempfile::tempdir().unwrap();
        let _guard = ProcessGuard::capture();
        std::env::set_current_dir(dir.path()).unwrap();

        let path = PathBuf::from(&case.file);
        if case.directory == Some(true) {
            std::fs::create_dir(&path).unwrap();
        } else if case.missing != Some(true) {
            std::fs::write(&path, case.toml.as_deref().unwrap_or("")).unwrap();
        }

        let result = Config::load_or_default(&path);
        match (&case.expect, &case.error) {
            (Some(expected), None) => {
                let config = result
                    .unwrap_or_else(|e| panic!("case {}: expected success, got: {e}", case.id));
                let cli = case.cli.clone().unwrap_or_default();
                assert_eq!(
                    config.resolve_model(cli.model.clone()),
                    expected.model,
                    "case {}: resolved model diverged",
                    case.id
                );
                assert_eq!(
                    config.resolve_inherit_hooks(cli.inherit_hooks),
                    expected.inherit_hooks,
                    "case {}: resolved inherit_hooks diverged",
                    case.id
                );
                assert_eq!(
                    config.resolve_max_turns(cli.max_turns),
                    expected.max_turns,
                    "case {}: resolved max_turns diverged",
                    case.id
                );
                assert_eq!(
                    config.resolve_timeout_secs(cli.timeout_secs),
                    expected.timeout_secs,
                    "case {}: resolved timeout_secs diverged",
                    case.id
                );
            }
            (None, Some(want)) => {
                let err = result.expect_err(&format!("case {}: expected a failure", case.id));
                assert_eq!(
                    user_facing(err),
                    *want,
                    "case {}: user-facing error diverged from the fixture",
                    case.id
                );
            }
            _ => panic!(
                "case {}: fixture must set exactly one of expect / error",
                case.id
            ),
        }
    }
}

// ── layer 2: path alignment ──────────────────────────────────────────────────

/// Every `default_path` rule replays under env guards, pinning the doc's
/// precedence including the empty and non-UTF-8 `XDG_CONFIG_HOME` edges and
/// the strict `HOME` failure.
#[test]
fn default_path_rules_replay() {
    let _lock = process_lock();
    let fx = fixture();

    for rule in &fx.default_path {
        let xdg_dir = tempfile::tempdir().unwrap();
        let home_dir = tempfile::tempdir().unwrap();
        let _guard = ProcessGuard::capture();

        match rule.xdg.as_str() {
            "set" => std::env::set_var("XDG_CONFIG_HOME", xdg_dir.path()),
            "empty" => std::env::set_var("XDG_CONFIG_HOME", ""),
            "non-utf8" => std::env::set_var(
                "XDG_CONFIG_HOME",
                OsString::from_vec(b"/non-utf8-\xff".to_vec()),
            ),
            "unset" => std::env::remove_var("XDG_CONFIG_HOME"),
            other => panic!("rule {}: unknown xdg mode {other:?}", rule.id),
        }
        match rule.home.as_str() {
            "set" => std::env::set_var("HOME", home_dir.path()),
            "unset" => std::env::remove_var("HOME"),
            other => panic!("rule {}: unknown home mode {other:?}", rule.id),
        }

        match (&rule.expect_path, &rule.expect_error) {
            (Some(template), None) => {
                let expected = template
                    .replace("{xdg}", &xdg_dir.path().to_string_lossy())
                    .replace("{home}", &home_dir.path().to_string_lossy());
                let got = Config::default_path()
                    .unwrap_or_else(|e| panic!("rule {}: expected a path, got: {e}", rule.id));
                assert_eq!(
                    got,
                    PathBuf::from(expected),
                    "rule {}: default_path diverged",
                    rule.id
                );
            }
            (None, Some(want)) => {
                let err = Config::default_path()
                    .err()
                    .unwrap_or_else(|| panic!("rule {}: expected an error", rule.id));
                assert_eq!(
                    user_facing(err),
                    *want,
                    "rule {}: user-facing HOME error diverged",
                    rule.id
                );
            }
            _ => panic!(
                "rule {}: fixture must set exactly one of expect_path / expect_error",
                rule.id
            ),
        }
    }
}

// ── layer 3: table alignment ─────────────────────────────────────────────────

/// The fixture's key table is the doc's defaults table in data form; asserting
/// it against the resolvers on an empty config pins `DEFAULT_MODEL` and the
/// hardcoded fallbacks (30, 3600, true) that the doc advertises.
#[test]
fn keys_table_matches_builtin_defaults() {
    let fx = fixture();
    let empty = Config::default();

    assert_eq!(fx.keys.len(), 4, "the contract admits exactly four keys");
    for key in &fx.keys {
        match key.name.as_str() {
            "model" => assert_eq!(
                empty.resolve_model(None),
                key.builtin_default
                    .as_str()
                    .unwrap_or_else(|| panic!("model builtin_default must be a string")),
                "key model: built-in default diverged"
            ),
            "inherit_hooks" => assert_eq!(
                empty.resolve_inherit_hooks(None),
                key.builtin_default
                    .as_bool()
                    .unwrap_or_else(|| panic!("inherit_hooks builtin_default must be a bool")),
                "key inherit_hooks: built-in default diverged"
            ),
            "max_turns" => assert_eq!(
                empty.resolve_max_turns(None),
                key.builtin_default
                    .as_u64()
                    .unwrap_or_else(|| panic!("max_turns builtin_default must be a number"))
                    as u32,
                "key max_turns: built-in default diverged"
            ),
            "timeout_secs" => assert_eq!(
                empty.resolve_timeout_secs(None),
                key.builtin_default
                    .as_u64()
                    .unwrap_or_else(|| panic!("timeout_secs builtin_default must be a number")),
                "key timeout_secs: built-in default diverged"
            ),
            other => panic!("fixture names unknown key {other:?}"),
        }
    }
}

/// The doc's CLI-counterpart column is pinned against clap's own parser
/// definitions — including the mechanism of the documented limitation: the
/// `max_turns`/`timeout_secs` flags carry a `default_value` equal to the
/// built-in default, so `main.rs` always passes `Some(...)` into the
/// resolvers and the config tier never fires, while `model` and
/// `--no-inherit-hooks` have no parser default and their absence is
/// detectable.
#[test]
fn documented_cli_counterparts_exist_with_pinned_defaults() {
    let fx = fixture();
    let cmd = Cli::command();

    for key in &fx.keys {
        let flag = key
            .cli_flag
            .split(',')
            .next()
            .expect("cli_flag must name a long flag")
            .trim();
        let long = flag
            .strip_prefix("--")
            .unwrap_or_else(|| panic!("cli_flag {flag:?} must be a long flag"));
        let arg = cmd
            .get_arguments()
            .find(|a| a.get_long() == Some(long))
            .unwrap_or_else(|| panic!("clap defines no --{long} for key {}", key.name));

        match key.name.as_str() {
            // No parser default: an absent flag stays `None`, so the config
            // tier of the resolver genuinely applies.
            "model" | "inherit_hooks" => assert!(
                arg.get_default_values().is_empty(),
                "--{long} must have no clap default_value — its absence is what lets \
                 defaults.{} apply; the documented precedence depends on it",
                key.name
            ),
            // Parser default equal to the built-in default: the documented
            // limitation — the config key parses and validates but cannot
            // override the flag default.
            "max_turns" | "timeout_secs" => {
                let want = key.builtin_default.to_string();
                assert!(
                    arg.get_default_values()
                        .iter()
                        .any(|v| v == &OsStr::new(&want)),
                    "--{long} must carry clap default_value {want} (the documented \
                     mechanism that neutralizes defaults.{}); found {:?}",
                    key.name,
                    arg.get_default_values()
                );
            }
            other => panic!("fixture names unknown key {other:?}"),
        }
    }

    // The --config flag exists, takes a value, and defaults to nothing —
    // path discovery is what the doc's location rules describe.
    let config_arg = cmd
        .get_arguments()
        .find(|a| a.get_long() == Some("config"))
        .expect("clap defines no --config flag");
    assert!(
        config_arg.get_default_values().is_empty(),
        "--config must have no default; the XDG/HOME discovery path is the default"
    );
}

/// The closed-world key set: the unknown-key parse error must name every
/// documented key, so a key added to `Defaults` without a fixture/doc update
/// fails here (the error stops listing exactly the four documented names).
#[test]
fn unknown_key_error_lists_every_documented_key() {
    let fx = fixture();
    let case = fx
        .cases
        .iter()
        .find(|c| c.id == "unknown-key-in-defaults")
        .expect("fixture must carry the unknown-key case");
    let message = case
        .error
        .as_deref()
        .expect("unknown-key case must carry an error");

    assert!(
        message.contains("expected one of"),
        "the unknown-key error must list the expected fields"
    );
    for key in &fx.keys {
        assert!(
            message.contains(&key.name),
            "unknown-key error must list documented key {}",
            key.name
        );
    }
}

// ── layer 4: emitter alignment ───────────────────────────────────────────────

/// The documented text/json/stream-json error lines replay through
/// `emit_error` byte-for-byte: config errors go to stderr in every mode,
/// stdout stays empty, and the json/stream-json shapes match the
/// output-format contract.
#[test]
fn emitted_cases_replay_byte_for_byte() {
    let fx = fixture();

    for case in &fx.emitted {
        let format = match case.format.as_str() {
            "text" => OutputFormat::Text,
            "json" => OutputFormat::Json,
            "stream-json" => OutputFormat::StreamJson,
            other => panic!("fixture names unknown output format {other:?}"),
        };
        let mut stdout: Vec<u8> = Vec::new();
        let mut stderr: Vec<u8> = Vec::new();

        emit_error(
            &mut stdout,
            &mut stderr,
            &ClaudePrintError::Config(case.message.clone()),
            &format,
            &case.claude_version,
            true, // config errors always fire before a session exists
        )
        .unwrap();

        assert_eq!(
            String::from_utf8_lossy(&stdout),
            case.expected_stdout,
            "case {}: stdout bytes diverged from the fixture",
            case.id
        );
        assert_eq!(
            String::from_utf8_lossy(&stderr),
            case.expected_stderr,
            "case {}: stderr bytes diverged from the fixture",
            case.id
        );
    }
}

// ── layer 5: doc alignment ───────────────────────────────────────────────────

/// Every documented example must appear in the contract doc: TOML blocks
/// verbatim, error messages raw or JSON-escaped, emitted lines verbatim
/// (trailing newline aside). Cases without a textual payload (the empty
/// file, the missing file) are pinned by the loader layer alone. The key
/// table's rows are pinned too — name and TOML type exactly as the fixture
/// carries them.
#[test]
fn documented_examples_appear_verbatim_in_the_doc() {
    let fx = fixture();

    for key in &fx.keys {
        assert!(
            DOC.contains(&format!("| `{}` | {} |", key.name, key.toml_type)),
            "key {}: the doc's key table must carry the fixture's row `| `{}` | {} |` — \
             update doc and fixture together",
            key.name,
            key.name,
            key.toml_type
        );
    }

    for case in &fx.cases {
        if case.documented != Some(true) {
            continue;
        }
        if let Some(toml) = case.toml.as_deref() {
            if !toml.is_empty() {
                assert!(
                    DOC.contains(toml),
                    "case {} is documented: true but its TOML does not appear verbatim in \
                     docs/notes/config-file-contract.md — update doc and fixture together",
                    case.id
                );
            }
        }
        if let Some(err) = case.error.as_deref() {
            assert!(
                contains_message(DOC, err),
                "case {} is documented: true but its error message appears in neither raw \
                 nor JSON-escaped form in docs/notes/config-file-contract.md",
                case.id
            );
        }
    }

    for rule in &fx.default_path {
        if rule.documented != Some(true) {
            continue;
        }
        if let Some(err) = rule.expect_error.as_deref() {
            assert!(
                contains_message(DOC, err),
                "rule {} is documented: true but its error appears in neither raw nor \
                 JSON-escaped form in docs/notes/config-file-contract.md",
                rule.id
            );
        }
    }

    for case in &fx.emitted {
        if case.documented != Some(true) {
            continue;
        }
        let payload = if !case.expected_stdout.is_empty() {
            &case.expected_stdout
        } else {
            &case.expected_stderr
        };
        let example = payload.strip_suffix('\n').unwrap_or(payload);
        if example.is_empty() {
            continue;
        }
        assert!(
            DOC.contains(example),
            "case {} is documented: true but its emitted line does not appear verbatim in \
             docs/notes/config-file-contract.md — update doc and fixture together",
            case.id
        );
    }
}

/// Examples the README's Configuration section also shows (`also_in_readme`)
/// are pinned there too — the summary cannot drift from the contract either.
/// TOML blocks are pinned only where `toml_in_readme` says the README shows
/// the file (the README displays some errors without the file that produced
/// them); error messages are pinned for every `also_in_readme` case.
#[test]
fn readme_examples_still_match() {
    let fx = fixture();

    for case in &fx.cases {
        if case.also_in_readme != Some(true) {
            continue;
        }
        if case.toml_in_readme == Some(true) {
            if let Some(toml) = case.toml.as_deref() {
                if !toml.is_empty() {
                    assert!(
                        README.contains(toml),
                        "case {} is toml_in_readme but its TOML does not appear verbatim \
                         in README.md — update README and fixture together",
                        case.id
                    );
                }
            }
        }
        if let Some(err) = case.error.as_deref() {
            assert!(
                contains_message(README, err),
                "case {} is also_in_readme but its error message appears in neither raw \
                 nor JSON-escaped form in README.md",
                case.id
            );
        }
    }

    for rule in &fx.default_path {
        if rule.also_in_readme != Some(true) {
            continue;
        }
        if let Some(err) = rule.expect_error.as_deref() {
            assert!(
                contains_message(README, err),
                "rule {} is also_in_readme but its error appears in neither raw nor \
                 JSON-escaped form in README.md",
                rule.id
            );
        }
    }

    for case in &fx.emitted {
        if case.also_in_readme != Some(true) {
            continue;
        }
        let payload = if !case.expected_stdout.is_empty() {
            &case.expected_stdout
        } else {
            &case.expected_stderr
        };
        let example = payload.strip_suffix('\n').unwrap_or(payload);
        assert!(
            !example.is_empty() && README.contains(example),
            "case {} is also_in_readme but its emitted line does not appear verbatim in \
             README.md — update README and fixture together",
            case.id
        );
    }
}

// ── self-integrity ───────────────────────────────────────────────────────────

/// The fixture's own header must still point at this test, this document, and
/// the contract version the doc's table row advertises — the pointers that
/// make the pin navigable. Ids must be unique across every fixture section.
#[test]
fn fixture_header_points_at_this_test_and_doc() {
    let fx = fixture();

    assert_eq!(
        fx.pinned_by, "tests/config_contract.rs",
        "fixture pinned_by must name this test file"
    );
    assert_eq!(
        fx.document, "docs/notes/config-file-contract.md",
        "fixture document must name the contract doc"
    );
    assert_eq!(
        fx.readme, "README.md",
        "fixture readme must name the README"
    );
    assert_eq!(fx.contract_version, "v1");
    assert!(
        DOC.contains(&format!(
            "| **Contract version** | {} |",
            fx.contract_version
        )),
        "the doc's Contract version row must match the fixture's contract_version"
    );

    let mut ids: Vec<&str> = fx
        .cases
        .iter()
        .map(|c| c.id.as_str())
        .chain(fx.default_path.iter().map(|r| r.id.as_str()))
        .chain(fx.emitted.iter().map(|e| e.id.as_str()))
        .collect();
    ids.sort_unstable();
    let dupes = ids.windows(2).filter(|w| w[0] == w[1]).count();
    assert_eq!(dupes, 0, "fixture ids must be unique across all sections");
}
